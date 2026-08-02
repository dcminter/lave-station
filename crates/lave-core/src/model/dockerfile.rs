//! Reconstructing a Dockerfile from an image's build history.
//!
//! **This is a reconstruction, not a recovery.** The original Dockerfile is not stored
//! in the image; what is stored is a list of the instructions as the builder recorded
//! them. Enough survives to be genuinely useful, and enough is lost that presenting the
//! result as the original would be a lie — so every reconstruction carries its own
//! caveats, rendered with it rather than buried in documentation.
//!
//! Two recorded forms exist in the wild, both verified against the development host:
//!
//! ```text
//! legacy   /bin/sh -c #(nop)  CMD ["/etc/confluent/docker/run"]
//! legacy   |6 ARTIFACT_ID=cp-kafka BUILD_NUMBER=... /bin/sh -c echo "===> Installing"
//! buildkit COPY /app/target/release/pub-sub-tui /usr/local/bin/pub-sub-tui # buildkit
//! ```
//!
//! The `FROM` line is the one thing `docker history` cannot give us: it stops at the
//! base image's own records without saying where the boundary is. Version 2's
//! `relations::base_of` identifies the base by shared layer prefix, and its history
//! length gives the boundary. See `docs/iteration_3_plan.md` §1.4.

use crate::engine::HistoryEntry;
use crate::model::format;

/// One reconstructed line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instruction {
    /// `FROM`, `RUN`, `COPY`, and so on. Empty for a comment carried through verbatim.
    pub keyword: String,
    pub argument: String,
}

impl Instruction {
    #[must_use]
    pub fn is_comment(&self) -> bool {
        self.keyword.is_empty()
    }

    #[must_use]
    pub fn render(&self) -> String {
        if self.is_comment() {
            self.argument.clone()
        } else if self.argument.is_empty() {
            self.keyword.clone()
        } else {
            format!("{} {}", self.keyword, self.argument)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Reconstruction {
    pub instructions: Vec<Instruction>,
    /// What the `FROM` line resolved to, when a local base was identified.
    pub base: Option<String>,
    /// Everything the reader needs to distrust, in the order of how much it matters.
    pub caveats: Vec<String>,
}

impl Reconstruction {
    /// The whole thing as Dockerfile text, caveats included as leading comments.
    #[must_use]
    pub fn render(&self) -> String {
        let mut lines: Vec<String> = self
            .caveats
            .iter()
            .map(|caveat| format!("# {caveat}"))
            .collect();

        if !lines.is_empty() {
            lines.push(String::new());
        }

        lines.extend(self.instructions.iter().map(Instruction::render));
        lines.join("\n")
    }
}

/// Rebuild a Dockerfile.
///
/// `history` and `base_history` arrive newest-first, as the daemon reports them.
/// `base` names the image the layer-prefix analysis identified, if any.
#[must_use]
pub fn reconstruct(
    history: &[HistoryEntry],
    base: Option<&str>,
    base_history: &[HistoryEntry],
) -> Reconstruction {
    let mut instructions = Vec::new();
    let mut caveats = vec![
        "Reconstructed from this image's build history. It is not the original \
         Dockerfile, and will not necessarily rebuild an identical image."
            .to_owned(),
    ];

    // The base image's records sit at the bottom of ours, so its history length is the
    // boundary. A base with more history than us means the analysis disagrees with the
    // daemon; trust the daemon and treat the whole thing as ours.
    let own = match history.len().checked_sub(base_history.len()) {
        Some(own) if base.is_some() && own > 0 => own,
        // No base, or a base claiming more history than we have — which means the layer
        // analysis disagrees with the daemon. Trust the daemon and treat it all as ours.
        _ => history.len(),
    };

    match base {
        Some(base) => instructions.push(Instruction {
            keyword: "FROM".to_owned(),
            argument: base.to_owned(),
        }),
        None => caveats.push(
            "No local image matches this one's lower layers, so the FROM line could not be \
             recovered. The instructions below may include the base image's own."
                .to_owned(),
        ),
    }

    // Oldest first, which is Dockerfile order.
    let mut baked_args = false;
    let mut copied = false;

    for entry in history.iter().take(own).rev() {
        let parsed = parse(&entry.created_by);
        baked_args |= parsed.baked_args;
        copied |= parsed.instruction.keyword == "COPY" || parsed.instruction.keyword == "ADD";

        if !parsed.instruction.render().trim().is_empty() {
            instructions.push(parsed.instruction);
        }
    }

    if copied {
        caveats.push(
            "A COPY or ADD in a multi-stage build records the path inside the stage it \
             copied from, not the stage's name. Those paths will not exist in your build \
             context."
                .to_owned(),
        );
    }

    if baked_args {
        caveats.push(
            "Build arguments are recorded with their values substituted in. The ARG \
             declarations themselves are gone."
                .to_owned(),
        );
    }

    if history.len() == 1 {
        caveats.push(
            "This image has a single history entry, which usually means it was squashed or \
             imported. Its individual instructions no longer exist."
                .to_owned(),
        );
    }

    Reconstruction {
        instructions,
        base: base.map(str::to_owned),
        caveats,
    }
}

struct Parsed {
    instruction: Instruction,
    baked_args: bool,
}

/// Turn one recorded command back into an instruction.
fn parse(created_by: &str) -> Parsed {
    // BuildKit tags what it wrote; the marker is noise to a reader.
    let text = created_by
        .strip_suffix(" # buildkit")
        .unwrap_or(created_by)
        .trim();

    // `|3 FOO=bar BAZ=qux /bin/sh -c ...` — the legacy builder's way of recording that
    // three build arguments were in scope, with their values already substituted.
    if let Some(rest) = strip_build_args(text) {
        return Parsed {
            instruction: run(rest),
            baked_args: true,
        };
    }

    // `#(nop)` marks an instruction that added no layer; what follows is already in
    // Dockerfile form.
    if let Some(rest) = text
        .strip_prefix("/bin/sh -c #(nop) ")
        .or_else(|| text.strip_prefix("/bin/sh -c #(nop)"))
    {
        return Parsed {
            instruction: keyword_form(rest.trim()),
            baked_args: false,
        };
    }

    if let Some(rest) = text.strip_prefix("/bin/sh -c ") {
        return Parsed {
            instruction: run(rest),
            baked_args: false,
        };
    }

    // A bare comment is the base image's own build machinery, e.g. debian's
    // `# debian.sh --arch 'amd64' out/ 'bookworm'`. Carried through as a comment
    // because inventing an instruction for it would be worse.
    if text.starts_with('#') {
        return Parsed {
            instruction: Instruction {
                keyword: String::new(),
                argument: text.to_owned(),
            },
            baked_args: false,
        };
    }

    Parsed {
        instruction: keyword_form(text),
        baked_args: false,
    }
}

/// Strip a `|N ARG=value ...` prefix, returning what the shell actually ran.
fn strip_build_args(text: &str) -> Option<&str> {
    let rest = text.strip_prefix('|')?;
    // The count, then the assignments, then the command itself.
    let rest = rest.trim_start_matches(|character: char| character.is_ascii_digit());
    let position = rest.find("/bin/sh -c ")?;
    Some(&rest[position + "/bin/sh -c ".len()..])
}

fn run(command: &str) -> Instruction {
    Instruction {
        keyword: "RUN".to_owned(),
        argument: command.trim().to_owned(),
    }
}

/// Split `CMD ["x"]` into its keyword and argument. Anything that does not look like an
/// instruction becomes a comment rather than being guessed at.
fn keyword_form(text: &str) -> Instruction {
    let mut parts = text.splitn(2, char::is_whitespace);
    let keyword = parts.next().unwrap_or_default();

    if keyword.is_empty() {
        return Instruction {
            keyword: String::new(),
            argument: String::new(),
        };
    }

    if !is_instruction(keyword) {
        return Instruction {
            keyword: String::new(),
            argument: format!("# {text}"),
        };
    }

    let argument = parts.next().unwrap_or_default().trim();

    Instruction {
        keyword: keyword.to_owned(),
        argument: normalise(keyword, argument),
    }
}

/// Undo the daemon's rendering of an instruction's argument.
///
/// The history records what the builder held in memory, printed with Go's default
/// formatting. That is not Dockerfile syntax: string slices lose their commas, `EXPOSE`
/// and `VOLUME` arrive as bracketed lists, and `HEALTHCHECK` as a struct. All four were
/// found on real images rather than imagined.
fn normalise(keyword: &str, argument: &str) -> String {
    match keyword {
        // BuildKit records the shell form with its wrapper intact, so the argument
        // still reads `/bin/sh -c apt-get ...`.
        "RUN" => argument
            .strip_prefix("/bin/sh -c ")
            .unwrap_or(argument)
            .trim()
            .to_owned(),
        "CMD" | "ENTRYPOINT" => restore_commas(argument),
        "EXPOSE" | "VOLUME" => unbracket(argument).to_owned(),
        "HEALTHCHECK" => healthcheck(argument),
        _ => argument.to_owned(),
    }
}

/// `["node" "index.js"]` is Go's rendering of a string slice; the JSON form a Dockerfile
/// needs has commas.
fn restore_commas(argument: &str) -> String {
    if argument.starts_with('[') && argument.ends_with(']') {
        return argument.replace("\" \"", "\", \"");
    }
    argument.to_owned()
}

fn unbracket(argument: &str) -> &str {
    argument
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(argument)
        .trim()
}

/// Turn Go's struct rendering back into flags and a command.
///
/// `{Test:[CMD-SHELL curl -f localhost] Interval:15s Timeout:3s Retries:3}` becomes
/// `--interval=15s --timeout=3s --retries=3 CMD curl -f localhost`. A shape we do not
/// recognise is returned untouched rather than mangled.
fn healthcheck(argument: &str) -> String {
    let Some(body) = argument
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
    else {
        return argument.to_owned();
    };

    let Some(test_start) = body.find("Test:[") else {
        return argument.to_owned();
    };
    let after_test = &body[test_start + "Test:[".len()..];
    let Some(test_end) = after_test.find(']') else {
        return argument.to_owned();
    };
    let test = &after_test[..test_end];

    let mut flags = Vec::new();
    for (field, flag) in [
        ("Interval:", "--interval"),
        ("Timeout:", "--timeout"),
        ("StartPeriod:", "--start-period"),
        ("Retries:", "--retries"),
    ] {
        if let Some(value) = field_value(&after_test[test_end..], field)
            // Zero means "unset" in every one of these fields, and emitting it would
            // change the meaning rather than describe it.
            && value != "0s"
            && value != "0"
        {
            flags.push(format!("{flag}={value}"));
        }
    }

    // The test itself is `CMD-SHELL <shell command>` or `CMD <argv...>`.
    // Both forms render as `CMD`: the shell/exec distinction is carried by whether the
    // rest is a bare command line or a JSON array, which it already is either way.
    let command = match test.split_once(' ') {
        Some(("CMD-SHELL" | "CMD", rest)) => format!("CMD {rest}"),
        _ => format!("CMD {test}"),
    };

    if flags.is_empty() {
        command
    } else {
        format!("{} {command}", flags.join(" "))
    }
}

fn field_value<'a>(text: &'a str, field: &str) -> Option<&'a str> {
    let start = text.find(field)? + field.len();
    let rest = &text[start..];
    let end = rest
        .find(|character: char| character.is_whitespace() || character == '}')
        .unwrap_or(rest.len());
    Some(&rest[..end])
}

/// The instruction set, so an unrecognised leading word is reported rather than
/// presented as though it were a Dockerfile keyword.
fn is_instruction(word: &str) -> bool {
    const KEYWORDS: [&str; 18] = [
        "ADD",
        "ARG",
        "CMD",
        "COPY",
        "ENTRYPOINT",
        "ENV",
        "EXPOSE",
        "FROM",
        "HEALTHCHECK",
        "LABEL",
        "MAINTAINER",
        "ONBUILD",
        "RUN",
        "SHELL",
        "STOPSIGNAL",
        "USER",
        "VOLUME",
        "WORKDIR",
    ];

    KEYWORDS.contains(&word)
}

/// A one-line summary for the detail pane's group heading.
#[must_use]
pub fn summary(reconstruction: &Reconstruction, history: &[HistoryEntry]) -> String {
    let size: i64 = history.iter().map(|entry| entry.size).sum();
    format!(
        "{} instructions, {}",
        reconstruction
            .instructions
            .iter()
            .filter(|instruction| !instruction.is_comment())
            .count(),
        format::bytes(size)
    )
}

#[cfg(test)]
mod tests {
    // expect is fine in tests; a failed assumption should abort the test.
    #![allow(clippy::expect_used)]

    use super::*;

    fn entry(created_by: &str) -> HistoryEntry {
        HistoryEntry {
            created_by: created_by.to_owned(),
            ..HistoryEntry::default()
        }
    }

    /// Newest first, as the daemon reports it.
    fn pub_sub_history() -> Vec<HistoryEntry> {
        vec![
            entry(r#"CMD ["pub-sub-monitor"]"#),
            entry("ENTRYPOINT []"),
            entry("COPY /app/target/release/pub-sub-tui /usr/local/bin/pub-sub-tui # buildkit"),
            entry("RUN /bin/sh -c apt-get update && apt-get install -y ca-certificates # buildkit"),
            entry("# debian.sh --arch 'amd64' out/ 'bookworm' '@1781049600'"),
        ]
    }

    fn rendered(reconstruction: &Reconstruction) -> Vec<String> {
        reconstruction
            .instructions
            .iter()
            .map(Instruction::render)
            .collect()
    }

    #[test]
    fn the_base_image_supplies_the_from_line_history_cannot() {
        let base = vec![entry("# debian.sh --arch 'amd64' out/ 'bookworm'")];

        let result = reconstruct(&pub_sub_history(), Some("node:22-alpine"), &base);

        assert_eq!(
            rendered(&result).first().map(String::as_str),
            Some("FROM node:22-alpine")
        );
    }

    #[test]
    fn the_bases_own_records_are_not_repeated_as_ours() {
        let base = vec![entry("# debian.sh --arch 'amd64' out/ 'bookworm'")];

        let result = reconstruct(&pub_sub_history(), Some("debian:bookworm"), &base);

        assert!(
            !rendered(&result)
                .iter()
                .any(|line| line.contains("debian.sh")),
            "the base's build machinery belongs to the base: {:?}",
            rendered(&result)
        );
    }

    #[test]
    fn instructions_come_out_in_dockerfile_order_not_daemon_order() {
        let result = reconstruct(&pub_sub_history(), Some("debian:bookworm"), &[entry("#x")]);
        let lines = rendered(&result);

        let run = lines.iter().position(|line| line.starts_with("RUN"));
        let cmd = lines.iter().position(|line| line.starts_with("CMD"));

        assert!(run < cmd, "RUN should precede CMD: {lines:?}");
    }

    #[test]
    fn a_legacy_nop_instruction_loses_its_shell_wrapper() {
        let history = vec![entry(
            r#"/bin/sh -c #(nop)  CMD ["/etc/confluent/docker/run"]"#,
        )];

        let result = reconstruct(&history, None, &[]);

        assert_eq!(
            rendered(&result),
            vec![r#"CMD ["/etc/confluent/docker/run"]"#.to_owned()]
        );
    }

    #[test]
    fn a_legacy_shell_command_becomes_a_run() {
        let history = vec![entry(
            "/bin/sh -c apt-get update && apt-get install -y curl",
        )];

        let result = reconstruct(&history, None, &[]);

        assert_eq!(
            rendered(&result),
            vec!["RUN apt-get update && apt-get install -y curl".to_owned()]
        );
    }

    #[test]
    fn baked_build_arguments_are_stripped_and_confessed() {
        let history = vec![entry(
            "|6 ARTIFACT_ID=cp-kafka BUILD_NUMBER=349ed81f /bin/sh -c echo installing",
        )];

        let result = reconstruct(&history, None, &[]);

        assert_eq!(rendered(&result), vec!["RUN echo installing".to_owned()]);
        assert!(
            result.caveats.iter().any(|caveat| caveat.contains("ARG")),
            "{:?}",
            result.caveats
        );
    }

    #[test]
    fn a_buildkit_marker_is_not_rendered() {
        let history = vec![entry("WORKDIR /app # buildkit")];

        let result = reconstruct(&history, None, &[]);

        assert_eq!(rendered(&result), vec!["WORKDIR /app".to_owned()]);
    }

    #[test]
    fn every_reconstruction_says_it_is_one() {
        let result = reconstruct(&pub_sub_history(), Some("debian:bookworm"), &[entry("#x")]);

        assert!(
            result.caveats[0].contains("not the original"),
            "the first thing a reader sees must be the disclaimer: {:?}",
            result.caveats
        );
        assert!(result.render().starts_with("# Reconstructed"));
    }

    #[test]
    fn a_copy_warns_that_its_source_stage_is_gone() {
        let result = reconstruct(&pub_sub_history(), Some("debian:bookworm"), &[entry("#x")]);

        assert!(
            result.caveats.iter().any(|caveat| caveat.contains("COPY")),
            "{:?}",
            result.caveats
        );
    }

    #[test]
    fn an_unresolved_base_is_admitted_rather_than_guessed() {
        let result = reconstruct(&pub_sub_history(), None, &[]);

        assert_eq!(result.base, None);
        assert!(
            !rendered(&result)
                .iter()
                .any(|line| line.starts_with("FROM"))
        );
        assert!(
            result.caveats.iter().any(|caveat| caveat.contains("FROM")),
            "{:?}",
            result.caveats
        );
    }

    #[test]
    fn a_squashed_image_is_reported_as_such() {
        let history = vec![entry("/bin/sh -c #(nop)  CMD [\"/bin/sh\"]")];

        let result = reconstruct(&history, None, &[]);

        assert!(
            result
                .caveats
                .iter()
                .any(|caveat| caveat.contains("squashed")),
            "{:?}",
            result.caveats
        );
    }

    // The four below were all found on real images; fixtures alone missed every one.

    #[test]
    fn a_buildkit_run_loses_its_shell_wrapper() {
        let history = vec![entry(
            "RUN /bin/sh -c apt-get update && apt-get install -y ca-certificates # buildkit",
        )];

        let result = reconstruct(&history, None, &[]);

        assert_eq!(
            rendered(&result),
            vec!["RUN apt-get update && apt-get install -y ca-certificates".to_owned()]
        );
    }

    #[test]
    fn an_argument_vector_regains_the_commas_that_make_it_json() {
        let history = vec![entry(r#"CMD ["node" "dist/server/index.js"]"#)];

        let result = reconstruct(&history, None, &[]);

        assert_eq!(
            rendered(&result),
            vec![r#"CMD ["node", "dist/server/index.js"]"#.to_owned()]
        );
    }

    #[test]
    fn a_single_element_vector_is_left_alone() {
        let history = vec![entry(r#"CMD ["pub-sub-monitor"]"#)];

        let result = reconstruct(&history, None, &[]);

        assert_eq!(
            rendered(&result),
            vec![r#"CMD ["pub-sub-monitor"]"#.to_owned()]
        );
    }

    #[test]
    fn ports_and_volumes_shed_their_go_brackets() {
        let history = vec![
            entry("VOLUME [/var/lib/kafka/data /etc/kafka/secrets]"),
            entry("EXPOSE [8080/tcp]"),
        ];

        let result = reconstruct(&history, None, &[]);

        assert_eq!(
            rendered(&result),
            vec![
                "EXPOSE 8080/tcp".to_owned(),
                "VOLUME /var/lib/kafka/data /etc/kafka/secrets".to_owned(),
            ]
        );
    }

    #[test]
    fn a_healthcheck_struct_becomes_flags_and_a_command() {
        let history = vec![entry(
            "HEALTHCHECK {Test:[CMD-SHELL curl -f localhost/healthz] Interval:15s \
             Timeout:3s StartPeriod:5s StartInterval:0s Retries:3}",
        )];

        let result = reconstruct(&history, None, &[]);

        assert_eq!(
            rendered(&result),
            vec![
                "HEALTHCHECK --interval=15s --timeout=3s --start-period=5s --retries=3 \
                 CMD curl -f localhost/healthz"
                    .to_owned()
            ]
        );
    }

    #[test]
    fn a_healthcheck_shape_we_do_not_recognise_is_left_untouched_rather_than_mangled() {
        let history = vec![entry("HEALTHCHECK something-else-entirely")];

        let result = reconstruct(&history, None, &[]);

        assert_eq!(
            rendered(&result),
            vec!["HEALTHCHECK something-else-entirely".to_owned()]
        );
    }

    #[test]
    fn an_unrecognised_leading_word_becomes_a_comment_rather_than_a_fake_keyword() {
        let history = vec![entry("some-tool --did-something")];

        let result = reconstruct(&history, None, &[]);

        assert_eq!(
            rendered(&result),
            vec!["# some-tool --did-something".to_owned()]
        );
    }

    #[test]
    fn a_base_with_more_history_than_the_image_does_not_underflow() {
        let base = vec![
            entry("a"),
            entry("b"),
            entry("c"),
            entry("d"),
            entry("e"),
            entry("f"),
        ];

        // Must not panic, and must not silently drop everything.
        let result = reconstruct(&pub_sub_history(), Some("weird:base"), &base);

        assert!(result.instructions.len() > 1);
    }
}
