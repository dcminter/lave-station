//! Turning a container's log stream into lines fit to display.
//!
//! The 8-byte frame header of `docs/container_daemon_integration.md` §5 is unpicked by
//! `bollard` before it reaches us, so what arrives here is already
//! [`LogChunk`]s tagged with their stream. What is *not* done for us:
//!
//! * chunk boundaries fall wherever the socket happened to split, so a line can arrive
//!   in pieces and must be reassembled;
//! * output is arbitrary bytes, not necessarily UTF-8;
//! * containers emit ANSI escapes, which a text view renders as visible rubbish;
//! * a chatty container will happily produce more lines than any window can hold.

use std::collections::VecDeque;

use crate::engine::{LogChunk, LogStream};

/// How many lines the viewer keeps. Beyond this the oldest are dropped: a container
/// that has logged for a week is not something to hold in memory in its entirety.
pub const MAX_RETAINED_LINES: usize = 5000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    pub stream: LogStream,
    pub text: String,
}

/// What one push changed, so the widget layer can append rather than redraw.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Appended {
    pub lines: Vec<LogLine>,
    /// How many lines fell off the top to make room. The viewer deletes this many from
    /// its own start, so the two stay in step without either recounting.
    pub dropped: usize,
}

/// Accumulates chunks into lines, holding at most [`MAX_RETAINED_LINES`].
#[derive(Debug, Clone)]
pub struct Transcript {
    lines: VecDeque<LogLine>,
    /// A line split across chunks, kept per stream: stdout and stderr interleave, and
    /// joining a fragment of one to a fragment of the other would corrupt both.
    partial_stdout: Vec<u8>,
    partial_stderr: Vec<u8>,
    capacity: usize,
}

impl Default for Transcript {
    fn default() -> Self {
        Self::new(MAX_RETAINED_LINES)
    }
}

impl Transcript {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            partial_stdout: Vec::new(),
            partial_stderr: Vec::new(),
            // A zero capacity would make every push drop everything, which is a
            // configuration mistake rather than a thing to honour.
            capacity: capacity.max(1),
        }
    }

    #[must_use]
    pub fn lines(&self) -> &VecDeque<LogLine> {
        &self.lines
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty() && self.partial_stdout.is_empty() && self.partial_stderr.is_empty()
    }

    /// Absorb a chunk, returning the lines it completed.
    pub fn push(&mut self, chunk: &LogChunk) -> Appended {
        let partial = match chunk.stream {
            LogStream::Stdout => &mut self.partial_stdout,
            LogStream::Stderr => &mut self.partial_stderr,
        };
        partial.extend_from_slice(&chunk.bytes);

        let mut completed = Vec::new();
        // Only whole lines are emitted; a trailing fragment waits for the next chunk.
        while let Some(position) = partial.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = partial.drain(..=position).collect();
            completed.push(LogLine {
                stream: chunk.stream,
                text: clean(&line[..line.len() - 1]),
            });
        }

        self.append(completed)
    }

    /// Emit whatever is left over as a final line.
    ///
    /// A container that exits without a trailing newline still had something to say,
    /// and dropping it would silently lose the last thing it printed.
    pub fn finish(&mut self) -> Appended {
        let mut completed = Vec::new();

        for (partial, stream) in [
            (&mut self.partial_stdout, LogStream::Stdout),
            (&mut self.partial_stderr, LogStream::Stderr),
        ] {
            if !partial.is_empty() {
                let text = clean(partial);
                partial.clear();
                if !text.is_empty() {
                    completed.push(LogLine { stream, text });
                }
            }
        }

        self.append(completed)
    }

    fn append(&mut self, lines: Vec<LogLine>) -> Appended {
        self.lines.extend(lines.iter().cloned());

        let dropped = self.lines.len().saturating_sub(self.capacity);
        for _ in 0..dropped {
            self.lines.pop_front();
        }

        Appended { lines, dropped }
    }
}

/// Make one line's bytes fit to display: decode leniently, drop ANSI escapes and the
/// carriage return of a CRLF ending.
fn clean(bytes: &[u8]) -> String {
    // Lossy rather than an error: a container writing something that is not UTF-8 is
    // misbehaving, but the rest of its output is still worth reading.
    let text = String::from_utf8_lossy(bytes);
    let text = text.strip_suffix('\r').unwrap_or(&text);
    strip_ansi(text)
}

/// Remove ANSI escape sequences.
///
/// Colour is the common case, but the same syntax carries cursor movement and screen
/// clearing, which a scrolling text view cannot honour anyway.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut characters = text.chars();

    while let Some(character) = characters.next() {
        if character != '\u{1b}' {
            out.push(character);
            continue;
        }

        match characters.next() {
            // CSI: parameters and intermediates, then a final byte in @-~.
            Some('[') => {
                for following in characters.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&following) {
                        break;
                    }
                }
            }
            // OSC: runs until BEL or a string terminator.
            Some(']') => {
                while let Some(following) = characters.next() {
                    if following == '\u{7}' {
                        break;
                    }
                    if following == '\u{1b}' {
                        characters.next();
                        break;
                    }
                }
            }
            // A two-character escape consumed whole, or a lone ESC at the very end of
            // the line — nothing further to drop either way.
            Some(_) | None => {}
        }
    }

    out
}

#[cfg(test)]
mod tests {
    // expect is fine in tests; a failed assumption should abort the test.
    #![allow(clippy::expect_used)]

    use super::*;

    fn chunk(stream: LogStream, text: &str) -> LogChunk {
        LogChunk {
            stream,
            bytes: text.as_bytes().to_vec(),
        }
    }

    fn out(text: &str) -> LogChunk {
        chunk(LogStream::Stdout, text)
    }

    fn err(text: &str) -> LogChunk {
        chunk(LogStream::Stderr, text)
    }

    fn texts(appended: &Appended) -> Vec<&str> {
        appended
            .lines
            .iter()
            .map(|line| line.text.as_str())
            .collect()
    }

    #[test]
    fn whole_lines_come_out_one_at_a_time() {
        let mut transcript = Transcript::default();

        let appended = transcript.push(&out("first\nsecond\n"));

        assert_eq!(texts(&appended), vec!["first", "second"]);
    }

    #[test]
    fn a_line_split_across_chunks_is_rejoined() {
        let mut transcript = Transcript::default();

        assert!(transcript.push(&out("half")).lines.is_empty());
        let appended = transcript.push(&out(" of a line\n"));

        assert_eq!(texts(&appended), vec!["half of a line"]);
    }

    #[test]
    fn the_two_streams_keep_their_fragments_apart() {
        let mut transcript = Transcript::default();

        // Interleaved partials: joining them would corrupt both lines.
        transcript.push(&out("stdout "));
        transcript.push(&err("stderr "));
        let from_out = transcript.push(&out("finished\n"));
        let from_err = transcript.push(&err("finished\n"));

        assert_eq!(texts(&from_out), vec!["stdout finished"]);
        assert_eq!(texts(&from_err), vec!["stderr finished"]);
    }

    #[test]
    fn each_line_remembers_which_stream_it_came_from() {
        let mut transcript = Transcript::default();

        transcript.push(&out("normal\n"));
        transcript.push(&err("a problem\n"));

        let streams: Vec<LogStream> = transcript.lines().iter().map(|line| line.stream).collect();
        assert_eq!(streams, vec![LogStream::Stdout, LogStream::Stderr]);
    }

    #[test]
    fn a_trailing_fragment_survives_the_stream_ending() {
        let mut transcript = Transcript::default();
        transcript.push(&out("no newline at the end"));

        let appended = transcript.finish();

        assert_eq!(texts(&appended), vec!["no newline at the end"]);
    }

    #[test]
    fn finishing_an_exhausted_transcript_adds_nothing() {
        let mut transcript = Transcript::default();
        transcript.push(&out("complete\n"));

        assert!(transcript.finish().lines.is_empty());
    }

    #[test]
    fn crlf_endings_do_not_leave_a_stray_carriage_return() {
        let mut transcript = Transcript::default();

        let appended = transcript.push(&out("windows\r\nunix\n"));

        assert_eq!(texts(&appended), vec!["windows", "unix"]);
    }

    #[test]
    fn colour_codes_are_stripped_rather_than_shown_as_rubbish() {
        let mut transcript = Transcript::default();

        let appended = transcript.push(&out("\u{1b}[31merror\u{1b}[0m: it broke\n"));

        assert_eq!(texts(&appended), vec!["error: it broke"]);
    }

    #[test]
    fn a_window_title_escape_is_stripped_whole() {
        let mut transcript = Transcript::default();

        let appended = transcript.push(&out("\u{1b}]0;a title\u{7}after\n"));

        assert_eq!(texts(&appended), vec!["after"]);
    }

    #[test]
    fn output_that_is_not_utf8_still_yields_a_line() {
        let mut transcript = Transcript::default();
        let chunk = LogChunk {
            stream: LogStream::Stdout,
            bytes: vec![b'o', b'k', 0xff, 0xfe, b'\n'],
        };

        let appended = transcript.push(&chunk);

        assert_eq!(appended.lines.len(), 1, "a bad byte must not lose the line");
        assert!(appended.lines[0].text.starts_with("ok"));
    }

    #[test]
    fn the_oldest_lines_are_dropped_once_the_cap_is_reached() {
        let mut transcript = Transcript::new(3);

        transcript.push(&out("one\ntwo\nthree\n"));
        let appended = transcript.push(&out("four\n"));

        assert_eq!(appended.dropped, 1);
        let kept: Vec<&str> = transcript
            .lines()
            .iter()
            .map(|line| line.text.as_str())
            .collect();
        assert_eq!(kept, vec!["two", "three", "four"]);
    }

    #[test]
    fn nothing_is_dropped_while_there_is_room() {
        let mut transcript = Transcript::new(10);

        let appended = transcript.push(&out("one\ntwo\n"));

        assert_eq!(appended.dropped, 0);
    }

    #[test]
    fn a_single_push_larger_than_the_cap_still_leaves_a_full_transcript() {
        let mut transcript = Transcript::new(2);

        let appended = transcript.push(&out("one\ntwo\nthree\nfour\n"));

        assert_eq!(appended.dropped, 2);
        assert_eq!(transcript.lines().len(), 2);
    }

    #[test]
    fn a_zero_capacity_is_treated_as_one_rather_than_swallowing_everything() {
        let mut transcript = Transcript::new(0);

        transcript.push(&out("kept\n"));

        assert_eq!(transcript.lines().len(), 1);
    }

    fn tokens(line: &str) -> Vec<(Token, String)> {
        let characters: Vec<char> = line.chars().collect();
        highlight(line)
            .into_iter()
            .map(|span| {
                (
                    span.token,
                    characters[span.start..span.end].iter().collect::<String>(),
                )
            })
            .collect()
    }

    #[test]
    fn a_structured_line_is_broken_into_keys_and_values() {
        let coloured = tokens(r#"{"level":"warn","tries":3,"ok":false}"#);

        assert!(coloured.contains(&(Token::Key, "\"level\"".to_owned())));
        assert!(coloured.contains(&(Token::Text, "\"warn\"".to_owned())));
        assert!(coloured.contains(&(Token::Key, "\"tries\"".to_owned())));
        assert!(coloured.contains(&(Token::Number, "3".to_owned())));
        assert!(coloured.contains(&(Token::Literal, "false".to_owned())));
    }

    #[test]
    fn a_string_is_a_key_only_when_a_colon_follows_it() {
        let coloured = tokens(r#"{"a":"b"}"#);

        // Same text either side of the colon; only position tells them apart.
        assert_eq!(
            coloured
                .iter()
                .filter(|(token, _)| *token == Token::Key)
                .count(),
            1
        );
        assert_eq!(
            coloured
                .iter()
                .filter(|(token, _)| *token == Token::Text)
                .count(),
            1
        );
    }

    #[test]
    fn an_escaped_quote_does_not_end_a_string_early() {
        let coloured = tokens(r#"{"msg":"she said \"hello\" loudly"}"#);

        assert!(
            coloured
                .iter()
                .any(|(token, text)| *token == Token::Text && text.contains("loudly")),
            "{coloured:?}"
        );
    }

    #[test]
    fn plain_output_is_left_uncoloured() {
        assert!(highlight("starting server on port 8080").is_empty());
        // Mentioning a brace is not the same as being JSON.
        assert!(highlight("config {broken").is_empty());
    }

    #[test]
    fn a_half_written_line_is_left_alone_rather_than_half_coloured() {
        // A container flushing mid-write produces exactly this.
        assert!(highlight(r#"{"level":"info","msg":"partial"#).is_empty());
    }

    #[test]
    fn a_json_array_is_not_treated_as_a_structured_line() {
        // Structured logging is objects; a bare array is far more likely to be prose.
        assert!(highlight("[1, 2, 3]").is_empty());
    }

    #[test]
    fn leading_whitespace_does_not_stop_a_line_being_recognised() {
        assert!(!highlight(r#"  {"level":"info"}"#).is_empty());
    }

    #[test]
    fn spans_are_character_offsets_so_multibyte_text_lines_up() {
        let line = r#"{"msg":"café","n":1}"#;
        let characters: Vec<char> = line.chars().collect();

        for span in highlight(line) {
            assert!(span.end <= characters.len(), "{span:?} runs past the line");
        }
        assert!(tokens(line).contains(&(Token::Text, "\"café\"".to_owned())));
    }

    #[test]
    fn an_empty_transcript_says_so_until_something_partial_arrives() {
        let mut transcript = Transcript::default();
        assert!(transcript.is_empty());

        transcript.push(&out("partial"));

        assert!(
            !transcript.is_empty(),
            "a fragment is still output that has been received"
        );
    }
}

/// What a token in a structured log line is, so the viewer can colour it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    /// An object member's name.
    Key,
    /// A string value.
    Text,
    Number,
    /// `true`, `false` or `null`.
    Literal,
    /// Braces, brackets, colons and commas.
    Punctuation,
}

/// One coloured run, in **character** offsets so a text view can use them directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub token: Token,
}

/// Colour a line if it is a JSON object, otherwise leave it alone.
///
/// Structured logging is common enough that a wall of JSON is worth reading, but a line
/// that merely *contains* a brace is not. The line is validated as a whole first, so a
/// half-written line — a container flushing mid-write — is left plain rather than
/// half-coloured.
///
/// Returns an empty list for anything that is not a complete JSON object.
#[must_use]
pub fn highlight(line: &str) -> Vec<Span> {
    let trimmed = line.trim();
    if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
        return Vec::new();
    }
    if serde_json::from_str::<serde_json::Value>(trimmed).is_err() {
        return Vec::new();
    }

    scan(line)
}

/// Walk the line, emitting a span per token. Only reached once the line is known to be
/// valid JSON, so this does not have to reject bad input — only describe good input.
fn scan(line: &str) -> Vec<Span> {
    let characters: Vec<char> = line.chars().collect();
    let mut spans = Vec::new();
    let mut index = 0;

    while index < characters.len() {
        let character = characters[index];

        match character {
            '"' => {
                let start = index;
                index += 1;
                while index < characters.len() {
                    if characters[index] == '\\' {
                        // An escape consumes the character after it, so a \" does not
                        // end the string.
                        index += 2;
                        continue;
                    }
                    if characters[index] == '"' {
                        break;
                    }
                    index += 1;
                }
                index = (index + 1).min(characters.len());

                // A string is a key exactly when a colon follows it.
                let followed_by_colon = characters[index..]
                    .iter()
                    .find(|candidate| !candidate.is_whitespace())
                    == Some(&':');

                spans.push(Span {
                    start,
                    end: index,
                    token: if followed_by_colon {
                        Token::Key
                    } else {
                        Token::Text
                    },
                });
            }
            '{' | '}' | '[' | ']' | ':' | ',' => {
                spans.push(Span {
                    start: index,
                    end: index + 1,
                    token: Token::Punctuation,
                });
                index += 1;
            }
            '-' | '0'..='9' => {
                let start = index;
                while index < characters.len()
                    && matches!(characters[index], '-' | '+' | '.' | 'e' | 'E' | '0'..='9')
                {
                    index += 1;
                }
                spans.push(Span {
                    start,
                    end: index,
                    token: Token::Number,
                });
            }
            't' | 'f' | 'n' => {
                let start = index;
                while index < characters.len() && characters[index].is_ascii_alphabetic() {
                    index += 1;
                }
                spans.push(Span {
                    start,
                    end: index,
                    token: Token::Literal,
                });
            }
            _ => index += 1,
        }
    }

    spans
}
