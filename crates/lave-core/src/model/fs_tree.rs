//! Indexing a container's filesystem from the archive endpoint.
//!
//! The endpoint has no directory listing and no metadata-only mode: asking for a path
//! returns a tar of that path **and its entire subtree**, contents included. See
//! `docs/iteration_3_plan.md` §1.1 for the measurements. So indexing means streaming the
//! tar past, keeping the headers and discarding the content, under a byte budget —
//! `/etc` on a real image cost 1.5MB, `/usr` on the same image would cost over a
//! gigabyte.
//!
//! Nothing here talks to a daemon. The caller feeds it bytes; it produces a tree.
//!
//! Member names are relative to the *parent* of the requested path — asking for `/etc`
//! yields `etc/`, `etc/hosts` — with `/` as the one case where they are already
//! absolute. Both were checked against a real daemon rather than assumed.

use std::collections::BTreeMap;

/// One tar header block.
const BLOCK: usize = 512;

/// How much of a subtree to stream before giving up. Generous enough for a
/// configuration directory, small enough that `/usr` on a large image fails fast
/// instead of pulling a gigabyte through the socket.
pub const DEFAULT_BUDGET: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    HardLink,
    /// Devices, fifos, sockets: listed, but nothing can be done with them here.
    Other,
}

impl EntryKind {
    #[must_use]
    pub fn is_directory(self) -> bool {
        self == EntryKind::Directory
    }

    fn from_typeflag(flag: u8) -> Self {
        match flag {
            b'0' | b'\0' | b'7' => EntryKind::File,
            b'1' => EntryKind::HardLink,
            b'2' => EntryKind::Symlink,
            b'5' => EntryKind::Directory,
            _ => EntryKind::Other,
        }
    }
}

/// One member of the archive, as read from its header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// As named in the tar, before being made absolute.
    pub name: String,
    pub size: u64,
    pub mode: u32,
    /// Seconds since the Unix epoch.
    pub mtime: i64,
    pub kind: EntryKind,
    /// Empty unless this is a link.
    pub link_target: String,
}

/// A node in the indexed tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// Absolute path within the container.
    pub path: String,
    /// The last component, which is what a browser displays.
    pub name: String,
    pub size: u64,
    pub mode: u32,
    pub mtime: i64,
    pub kind: EntryKind,
    pub link_target: String,
}

/// The indexed filesystem, keyed by absolute path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FsTree {
    nodes: BTreeMap<String, Node>,
}

impl FsTree {
    #[must_use]
    pub fn get(&self, path: &str) -> Option<&Node> {
        self.nodes.get(&normalise(path))
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The immediate children of a directory, in name order.
    ///
    /// Ordered by the `BTreeMap`, so a listing is stable between calls rather than
    /// reshuffling as the index fills in.
    #[must_use]
    pub fn children(&self, path: &str) -> Vec<&Node> {
        let parent = normalise(path);
        self.nodes
            .values()
            .filter(|node| is_child_of(&parent, &node.path))
            .collect()
    }

    fn insert(&mut self, node: Node) {
        self.nodes.insert(node.path.clone(), node);
    }
}

/// The result of indexing one subtree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Index {
    pub tree: FsTree,
    /// True when the budget ran out. The tree is then a prefix of the truth, and must
    /// never be presented as complete.
    pub truncated: bool,
    pub bytes_read: u64,
    /// The path that was asked for.
    pub root: String,
}

impl Index {
    /// Whether this index can answer for `path` without going back to the daemon.
    ///
    /// A complete index answers for everything, including paths it does not mention —
    /// their absence is the answer. A **truncated** one cannot: a directory missing from
    /// it may simply be past where indexing stopped, so only paths it actually holds can
    /// be trusted.
    #[must_use]
    pub fn covers(&self, path: &str) -> bool {
        self.tree.get(path).is_some() || !self.truncated
    }

    /// What to tell the user when the index stopped early.
    #[must_use]
    pub fn truncation_notice(&self) -> Option<String> {
        if !self.truncated {
            return None;
        }

        Some(format!(
            "{} is too large to list in full: {} were read before stopping. Open a \
             subdirectory to index that part instead.",
            self.root,
            crate::model::format::bytes(i64::try_from(self.bytes_read).unwrap_or(i64::MAX))
        ))
    }
}

/// Feeds bytes in, produces an [`Index`].
#[derive(Debug)]
pub struct Indexer {
    scanner: TarScanner,
    tree: FsTree,
    root: String,
    budget: u64,
    bytes_read: u64,
    truncated: bool,
}

impl Indexer {
    #[must_use]
    pub fn new(requested_path: &str, budget: u64) -> Self {
        Self {
            scanner: TarScanner::default(),
            tree: FsTree::default(),
            root: normalise(requested_path),
            budget,
            bytes_read: 0,
            truncated: false,
        }
    }

    /// Absorb a chunk. Returns false once the budget is spent, at which point the
    /// caller should stop reading and let the stream drop.
    pub fn push(&mut self, bytes: &[u8]) -> bool {
        if self.truncated {
            return false;
        }

        self.bytes_read += bytes.len() as u64;

        for entry in self.scanner.push(bytes) {
            let path = absolute_path(&self.root, &entry.name);
            self.tree.insert(Node {
                name: base_name(&path),
                path,
                size: entry.size,
                mode: entry.mode,
                mtime: entry.mtime,
                kind: entry.kind,
                link_target: entry.link_target,
            });
        }

        if self.bytes_read >= self.budget {
            self.truncated = true;
            return false;
        }

        true
    }

    #[must_use]
    pub fn finish(self) -> Index {
        Index {
            tree: self.tree,
            truncated: self.truncated,
            bytes_read: self.bytes_read,
            root: self.root,
        }
    }
}

/// Pull one regular file's content out of a tar.
///
/// Asking the archive endpoint for a single file returns a tar with that one member, so
/// this is what a `read` becomes. Extended headers are skipped; the first regular file
/// wins, because that is the only thing a single-file request can contain.
///
/// Returns `None` when the archive holds no regular file — a request for a directory,
/// or for a symlink, which carries its target in the header rather than as content.
#[must_use]
pub fn extract_file(tar: &[u8]) -> Option<Vec<u8>> {
    let mut offset = 0;

    while offset + BLOCK <= tar.len() {
        let block = &tar[offset..offset + BLOCK];
        offset += BLOCK;

        if block.iter().all(|byte| *byte == 0) {
            return None;
        }

        let size = numeric(&block[124..136]);
        let flag = block[156];
        let content_blocks = usize::try_from(size + padding(size)).unwrap_or(0);

        if EntryKind::from_typeflag(flag) == EntryKind::File && size > 0 {
            let end = offset.saturating_add(usize::try_from(size).unwrap_or(0));
            return tar.get(offset..end.min(tar.len())).map(<[u8]>::to_vec);
        }

        offset = offset.saturating_add(content_blocks);
    }

    None
}

/// An incremental tar reader that keeps headers and discards content.
///
/// Written rather than taken from a crate because the shape of the problem is unusual:
/// the content must be thrown away as it streams past, and the read abandoned partway
/// through. A `Read`-based archive reader would want to own the whole stream.
#[derive(Debug, Default)]
struct TarScanner {
    /// Header bytes accumulated so far; never grows beyond one block.
    header: Vec<u8>,
    /// Content and padding still to discard.
    skipping: u64,
    /// A GNU long name or PAX path read from a preceding extended header.
    pending_name: Option<String>,
    /// Bytes of an extended header still being collected.
    extended: Option<ExtendedHeader>,
}

#[derive(Debug)]
struct ExtendedHeader {
    kind: ExtendedKind,
    remaining: u64,
    data: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExtendedKind {
    /// GNU: the content is the next entry's name.
    LongName,
    /// PAX: the content is `length key=value\n` records.
    Pax,
}

impl TarScanner {
    fn push(&mut self, bytes: &[u8]) -> Vec<Entry> {
        let mut entries = Vec::new();
        let mut rest = bytes;

        while !rest.is_empty() {
            // Collecting the body of an extended header, which names the next entry.
            if let Some(extended) = self.extended.as_mut() {
                let take = usize::try_from(extended.remaining.min(rest.len() as u64))
                    .unwrap_or(rest.len());
                extended.data.extend_from_slice(&rest[..take]);
                extended.remaining -= take as u64;
                rest = &rest[take..];

                if extended.remaining == 0 {
                    let extended = self.extended.take().unwrap_or_else(|| ExtendedHeader {
                        kind: ExtendedKind::Pax,
                        remaining: 0,
                        data: Vec::new(),
                    });
                    self.pending_name = extended_name(&extended);
                    // Its content is padded to a block like any other.
                    self.skipping = padding(extended.data.len() as u64);
                }
                continue;
            }

            // Discarding a member's content.
            if self.skipping > 0 {
                let take =
                    usize::try_from(self.skipping.min(rest.len() as u64)).unwrap_or(rest.len());
                self.skipping -= take as u64;
                rest = &rest[take..];
                continue;
            }

            // Filling a header block.
            let want = BLOCK - self.header.len();
            let take = want.min(rest.len());
            self.header.extend_from_slice(&rest[..take]);
            rest = &rest[take..];

            if self.header.len() < BLOCK {
                break;
            }

            let block = std::mem::take(&mut self.header);
            if let Some(entry) = self.interpret(&block) {
                entries.push(entry);
            }
        }

        entries
    }

    /// Turn one header block into an entry, or arrange to collect what follows.
    fn interpret(&mut self, block: &[u8]) -> Option<Entry> {
        // Two zero blocks end the archive; a single one is enough to know this is not a
        // header.
        if block.iter().all(|byte| *byte == 0) {
            return None;
        }

        let size = numeric(&block[124..136]);
        let flag = block[156];

        match flag {
            b'L' => {
                self.extended = Some(ExtendedHeader {
                    kind: ExtendedKind::LongName,
                    remaining: size,
                    data: Vec::new(),
                });
                return None;
            }
            b'x' | b'X' => {
                self.extended = Some(ExtendedHeader {
                    kind: ExtendedKind::Pax,
                    remaining: size,
                    data: Vec::new(),
                });
                return None;
            }
            // A global PAX header or a GNU long link target: skip it rather than
            // misapply it to the next entry.
            b'g' | b'K' => {
                self.skipping = size + padding(size);
                return None;
            }
            _ => {}
        }

        let name = self
            .pending_name
            .take()
            .unwrap_or_else(|| ustar_name(block));

        // Directories declare a size but carry no content; everything else pads to a
        // block boundary.
        self.skipping = size + padding(size);

        Some(Entry {
            name,
            size,
            mode: u32::try_from(numeric(&block[100..108])).unwrap_or(0),
            mtime: i64::try_from(numeric(&block[136..148])).unwrap_or(0),
            kind: EntryKind::from_typeflag(flag),
            link_target: text(&block[157..257]),
        })
    }
}

fn extended_name(extended: &ExtendedHeader) -> Option<String> {
    match extended.kind {
        ExtendedKind::LongName => {
            let name = text(&extended.data);
            (!name.is_empty()).then_some(name)
        }
        ExtendedKind::Pax => pax_path(&extended.data),
    }
}

/// Pull `path=` out of PAX records, each `<length> <key>=<value>\n`.
fn pax_path(data: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(data);
    let mut rest = text.as_ref();

    while !rest.is_empty() {
        let space = rest.find(' ')?;
        let length: usize = rest[..space].parse().ok()?;
        if length == 0 || length > rest.len() {
            return None;
        }
        let record = &rest[space + 1..length];
        if let Some(value) = record.strip_prefix("path=") {
            return Some(value.trim_end_matches('\n').to_owned());
        }
        rest = &rest[length..];
    }

    None
}

/// `ustar` splits a long name into a prefix and a name field.
fn ustar_name(block: &[u8]) -> String {
    let name = text(&block[0..100]);
    let prefix = text(&block[345..500]);

    if prefix.is_empty() {
        name
    } else {
        format!("{prefix}/{name}")
    }
}

/// Header fields are NUL-padded ASCII.
fn text(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_owned()
}

/// Numeric fields are octal ASCII, except that GNU uses base 256 with the top bit set
/// for values too large to fit.
fn numeric(bytes: &[u8]) -> u64 {
    if bytes.first().is_some_and(|byte| byte & 0x80 != 0) {
        return bytes
            .iter()
            .skip(1)
            .fold(u64::from(bytes[0] & 0x7f), |total, byte| {
                total.wrapping_shl(8) | u64::from(*byte)
            });
    }

    let digits = text(bytes);
    u64::from_str_radix(digits.trim(), 8).unwrap_or(0)
}

/// Bytes of padding after `size` bytes of content.
fn padding(size: u64) -> u64 {
    let remainder = size % BLOCK as u64;
    if remainder == 0 {
        0
    } else {
        BLOCK as u64 - remainder
    }
}

/// Make a member name absolute.
///
/// Member names are relative to the parent of the requested path, so `/etc` yields
/// `etc/hosts`. Asking for `/` is the exception: those names are already absolute.
#[must_use]
pub fn absolute_path(root: &str, member: &str) -> String {
    let member = member.trim_end_matches('/');

    if member.starts_with('/') {
        return normalise(member);
    }

    let parent = parent_of(root);
    if parent == "/" {
        normalise(&format!("/{member}"))
    } else {
        normalise(&format!("{parent}/{member}"))
    }
}

/// Strip trailing slashes and collapse doubled ones, so a path is its own key.
#[must_use]
pub fn normalise(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut last_was_slash = false;

    for character in path.chars() {
        if character == '/' {
            if !last_was_slash {
                out.push('/');
            }
            last_was_slash = true;
        } else {
            out.push(character);
            last_was_slash = false;
        }
    }

    while out.len() > 1 && out.ends_with('/') {
        out.pop();
    }

    if out.is_empty() { "/".to_owned() } else { out }
}

#[must_use]
pub fn parent_of(path: &str) -> String {
    let path = normalise(path);
    match path.rfind('/') {
        Some(0) | None => "/".to_owned(),
        Some(position) => path[..position].to_owned(),
    }
}

#[must_use]
pub fn base_name(path: &str) -> String {
    let path = normalise(path);
    match path.rfind('/') {
        Some(position) if path.len() > 1 => path[position + 1..].to_owned(),
        _ => path,
    }
}

/// True when `candidate` sits directly inside `parent`.
fn is_child_of(parent: &str, candidate: &str) -> bool {
    if candidate == parent {
        return false;
    }

    let Some(rest) = candidate.strip_prefix(parent) else {
        return false;
    };
    // "/etcetera" must not count as a child of "/etc".
    let rest = match rest.strip_prefix('/') {
        Some(rest) => rest,
        None if parent == "/" => rest,
        None => return false,
    };

    !rest.is_empty() && !rest.contains('/')
}

#[cfg(test)]
mod tests {
    // expect is fine in tests; a failed assumption should abort the test.
    #![allow(clippy::expect_used)]

    use super::*;

    /// Build one ustar header block.
    fn header(name: &str, size: u64, flag: u8, mode: u32, mtime: i64) -> Vec<u8> {
        let mut block = vec![0u8; BLOCK];
        block[..name.len()].copy_from_slice(name.as_bytes());
        write_octal(&mut block[100..108], u64::from(mode));
        write_octal(&mut block[124..136], size);
        write_octal(&mut block[136..148], u64::try_from(mtime).unwrap_or(0));
        block[156] = flag;
        block[257..262].copy_from_slice(b"ustar");
        block
    }

    fn write_octal(field: &mut [u8], value: u64) {
        let text = format!("{:0width$o}", value, width = field.len() - 1);
        field[..text.len()].copy_from_slice(text.as_bytes());
    }

    fn content(bytes: &[u8]) -> Vec<u8> {
        let mut out = bytes.to_vec();
        out.resize(
            out.len() + usize::try_from(padding(bytes.len() as u64)).unwrap_or(0),
            0,
        );
        out
    }

    /// A tar of `/etc` as the daemon would produce it.
    fn etc_archive() -> Vec<u8> {
        let mut tar = Vec::new();
        tar.extend(header("etc/", 0, b'5', 0o755, 1_700_000_000));
        tar.extend(header("etc/hosts", 12, b'0', 0o644, 1_700_000_001));
        tar.extend(content(b"127.0.0.1 x"));
        tar.extend(header("etc/mtab", 0, b'2', 0o777, 1_700_000_002));
        tar.extend(header("etc/ssl/", 0, b'5', 0o755, 1_700_000_003));
        tar.extend(header("etc/ssl/cert.pem", 4, b'0', 0o644, 1_700_000_004));
        tar.extend(content(b"abcd"));
        tar.extend(vec![0u8; BLOCK * 2]);
        tar
    }

    fn index_of(root: &str, tar: &[u8]) -> Index {
        let mut indexer = Indexer::new(root, DEFAULT_BUDGET);
        indexer.push(tar);
        indexer.finish()
    }

    #[test]
    fn a_directory_tar_becomes_a_tree_of_absolute_paths() {
        let index = index_of("/etc", &etc_archive());

        let mut paths: Vec<&str> = index
            .tree
            .children("/etc")
            .iter()
            .map(|n| n.path.as_str())
            .collect();
        paths.sort_unstable();

        assert_eq!(paths, vec!["/etc/hosts", "/etc/mtab", "/etc/ssl"]);
    }

    #[test]
    fn children_are_the_immediate_ones_only() {
        let index = index_of("/etc", &etc_archive());

        // cert.pem is under /etc/ssl, not directly under /etc.
        assert!(
            !index
                .tree
                .children("/etc")
                .iter()
                .any(|node| node.name == "cert.pem")
        );
        assert_eq!(index.tree.children("/etc/ssl").len(), 1);
    }

    #[test]
    fn a_sibling_with_a_shared_prefix_is_not_mistaken_for_a_child() {
        let mut tar = Vec::new();
        tar.extend(header("etc/", 0, b'5', 0o755, 0));
        tar.extend(header("etc/hosts", 0, b'0', 0o644, 0));
        let mut indexer = Indexer::new("/etc", DEFAULT_BUDGET);
        indexer.push(&tar);
        let mut index = indexer.finish();
        // Force the awkward case directly.
        index.tree.insert(Node {
            path: "/etcetera".to_owned(),
            name: "etcetera".to_owned(),
            size: 0,
            mode: 0,
            mtime: 0,
            kind: EntryKind::Directory,
            link_target: String::new(),
        });

        assert!(
            !index
                .tree
                .children("/etc")
                .iter()
                .any(|node| node.name == "etcetera")
        );
    }

    #[test]
    fn kinds_and_modes_survive_the_header() {
        let index = index_of("/etc", &etc_archive());

        let hosts = index.tree.get("/etc/hosts").expect("hosts is indexed");
        assert_eq!(hosts.kind, EntryKind::File);
        assert_eq!(hosts.mode, 0o644);
        assert_eq!(hosts.size, 12);
        assert_eq!(hosts.mtime, 1_700_000_001);

        assert_eq!(
            index.tree.get("/etc/mtab").expect("mtab").kind,
            EntryKind::Symlink
        );
        assert!(index.tree.get("/etc/ssl").expect("ssl").kind.is_directory());
    }

    #[test]
    fn content_is_discarded_rather_than_mistaken_for_headers() {
        // The file's content is 512 bytes of plausible-looking rubbish.
        let mut tar = Vec::new();
        tar.extend(header("etc/", 0, b'5', 0o755, 0));
        tar.extend(header("etc/trap", BLOCK as u64, b'0', 0o644, 0));
        tar.extend(header("etc/not-real", 0, b'0', 0o644, 0));
        tar.extend(header("etc/after", 0, b'0', 0o644, 0));

        let index = index_of("/etc", &tar);

        assert!(index.tree.get("/etc/after").is_some());
        assert!(
            index.tree.get("/etc/not-real").is_none(),
            "a header-shaped payload must be skipped as content"
        );
    }

    #[test]
    fn a_member_split_across_chunks_is_read_correctly() {
        let tar = etc_archive();
        let mut indexer = Indexer::new("/etc", DEFAULT_BUDGET);

        // Deliberately unaligned pieces, as a socket would deliver them.
        for piece in tar.chunks(37) {
            indexer.push(piece);
        }
        let index = indexer.finish();

        assert_eq!(
            index.tree.get("/etc/ssl/cert.pem").expect("indexed").size,
            4
        );
    }

    #[test]
    fn indexing_stops_and_says_so_once_the_budget_is_spent() {
        let tar = etc_archive();
        let mut indexer = Indexer::new("/etc", 100);

        let keep_going = indexer.push(&tar);
        let index = indexer.finish();

        assert!(!keep_going, "the caller must be told to stop");
        assert!(index.truncated);
        assert!(
            index
                .truncation_notice()
                .is_some_and(|notice| notice.contains("/etc"))
        );
    }

    #[test]
    fn a_complete_index_answers_for_paths_it_does_not_hold() {
        let index = index_of("/etc", &etc_archive());

        assert!(index.covers("/etc/hosts"));
        // Absence is itself the answer when nothing was missed.
        assert!(index.covers("/etc/nothing-here"));
    }

    #[test]
    fn a_truncated_index_only_answers_for_what_it_actually_reached() {
        let mut indexer = Indexer::new("/etc", 100);
        indexer.push(&etc_archive());
        let index = indexer.finish();

        assert!(
            !index.covers("/etc/never-reached"),
            "a path past the cut-off may still exist, so this must be refetched"
        );
    }

    #[test]
    fn a_complete_index_carries_no_notice() {
        let index = index_of("/etc", &etc_archive());

        assert!(!index.truncated);
        assert_eq!(index.truncation_notice(), None);
    }

    #[test]
    fn pushing_after_the_budget_is_spent_changes_nothing() {
        let mut indexer = Indexer::new("/etc", 1);
        indexer.push(&etc_archive());
        let before = indexer.tree.len();

        assert!(!indexer.push(&etc_archive()));
        assert_eq!(indexer.tree.len(), before);
    }

    #[test]
    fn a_gnu_long_name_is_applied_to_the_entry_that_follows() {
        let long = format!("etc/{}", "a".repeat(150));
        let mut tar = Vec::new();
        tar.extend(header("././@LongLink", long.len() as u64, b'L', 0, 0));
        tar.extend(content(long.as_bytes()));
        tar.extend(header("etc/truncated-name", 0, b'0', 0o644, 0));

        let index = index_of("/etc", &tar);

        assert!(
            index.tree.get(&format!("/{long}")).is_some(),
            "indexed: {:?}",
            index
                .tree
                .children("/etc")
                .iter()
                .map(|n| &n.path)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_pax_path_record_is_applied_to_the_entry_that_follows() {
        let long = format!("etc/{}", "b".repeat(150));
        let record = format!("{} path={long}\n", 0);
        // Length is the whole record including the length field itself.
        let body = {
            let mut length = record.len();
            loop {
                let candidate = format!("{length} path={long}\n");
                if candidate.len() == length {
                    break candidate;
                }
                length = candidate.len();
            }
        };

        let mut tar = Vec::new();
        tar.extend(header("PaxHeaders/0", body.len() as u64, b'x', 0, 0));
        tar.extend(content(body.as_bytes()));
        tar.extend(header("etc/short", 0, b'0', 0o644, 0));

        let index = index_of("/etc", &tar);

        assert!(index.tree.get(&format!("/{long}")).is_some());
        assert!(
            index.tree.get("/etc/short").is_none(),
            "the PAX name replaces the header's own"
        );
    }

    #[test]
    fn a_ustar_prefix_is_joined_to_the_name() {
        let mut block = header("hosts", 0, b'0', 0o644, 0);
        let prefix = b"etc/deeply/nested";
        block[345..345 + prefix.len()].copy_from_slice(prefix);

        let index = index_of("/etc", &block);

        assert!(index.tree.get("/etc/deeply/nested/hosts").is_some());
    }

    #[test]
    fn a_root_archive_keeps_its_absolute_names() {
        let mut tar = Vec::new();
        tar.extend(header("/", 0, b'5', 0o755, 0));
        tar.extend(header("/.dockerenv", 0, b'0', 0o644, 0));
        tar.extend(header("/etc/", 0, b'5', 0o755, 0));

        let index = index_of("/", &tar);

        assert!(index.tree.get("/.dockerenv").is_some());
        assert!(index.tree.get("/etc").is_some());
        assert_eq!(index.tree.children("/").len(), 2);
    }

    #[test]
    fn a_large_size_in_base_256_is_read_rather_than_overflowing() {
        let mut block = header("etc/huge", 0, b'0', 0o644, 0);
        // GNU base-256: high bit set, then big-endian bytes.
        block[124] = 0x80;
        block[125..136].copy_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0x40, 0]);

        let mut indexer = Indexer::new("/etc", DEFAULT_BUDGET);
        indexer.push(&block);
        let index = indexer.finish();

        assert_eq!(index.tree.get("/etc/huge").expect("indexed").size, 0x40_00);
    }

    #[test]
    fn the_trailing_zero_blocks_do_not_become_an_entry() {
        let index = index_of("/etc", &etc_archive());

        assert!(
            !index
                .tree
                .children("/etc")
                .iter()
                .any(|node| node.name.is_empty())
        );
    }

    #[test]
    fn paths_are_normalised_so_a_lookup_does_not_depend_on_trailing_slashes() {
        assert_eq!(normalise("/etc/"), "/etc");
        assert_eq!(normalise("/etc//ssl/"), "/etc/ssl");
        assert_eq!(normalise("/"), "/");
        assert_eq!(normalise(""), "/");

        let index = index_of("/etc", &etc_archive());
        assert!(index.tree.get("/etc/ssl/").is_some());
    }

    #[test]
    fn parents_and_basenames_behave_at_the_root() {
        assert_eq!(parent_of("/etc/ssl"), "/etc");
        assert_eq!(parent_of("/etc"), "/");
        assert_eq!(parent_of("/"), "/");

        assert_eq!(base_name("/etc/ssl"), "ssl");
        assert_eq!(base_name("/etc"), "etc");
        assert_eq!(base_name("/"), "/");
    }

    #[test]
    fn a_single_file_archive_yields_its_content() {
        let mut tar = Vec::new();
        tar.extend(header("hosts", 11, b'0', 0o644, 0));
        tar.extend(content(b"127.0.0.1 x"));
        tar.extend(vec![0u8; BLOCK * 2]);

        assert_eq!(extract_file(&tar).as_deref(), Some(&b"127.0.0.1 x"[..]));
    }

    #[test]
    fn extraction_stops_at_the_declared_size_rather_than_the_block_boundary() {
        let mut tar = Vec::new();
        tar.extend(header("short", 3, b'0', 0o644, 0));
        tar.extend(content(b"abc"));

        assert_eq!(extract_file(&tar).as_deref(), Some(&b"abc"[..]));
    }

    #[test]
    fn extraction_skips_an_extended_header_to_reach_the_file() {
        let name = "x".repeat(120);
        let mut tar = Vec::new();
        tar.extend(header("././@LongLink", name.len() as u64, b'L', 0, 0));
        tar.extend(content(name.as_bytes()));
        tar.extend(header("placeholder", 5, b'0', 0o644, 0));
        tar.extend(content(b"found"));

        assert_eq!(extract_file(&tar).as_deref(), Some(&b"found"[..]));
    }

    #[test]
    fn a_directory_or_symlink_archive_has_no_content_to_extract() {
        let directory = header("etc/", 0, b'5', 0o755, 0);
        assert_eq!(extract_file(&directory), None);

        let symlink = header("etc/mtab", 0, b'2', 0o777, 0);
        assert_eq!(extract_file(&symlink), None);
    }

    #[test]
    fn a_truncated_archive_does_not_read_past_its_end() {
        let mut tar = Vec::new();
        tar.extend(header("cut", 4096, b'0', 0o644, 0));
        tar.extend(b"only a few bytes".to_vec());

        // Must return what is there rather than panicking on the missing remainder.
        let extracted = extract_file(&tar).expect("some content");
        assert!(extracted.len() <= 16);
    }

    #[test]
    fn member_names_are_made_absolute_against_the_requested_paths_parent() {
        assert_eq!(absolute_path("/etc", "etc/hosts"), "/etc/hosts");
        assert_eq!(
            absolute_path("/etc/ssl", "ssl/cert.pem"),
            "/etc/ssl/cert.pem"
        );
        assert_eq!(absolute_path("/", "/.dockerenv"), "/.dockerenv");
        assert_eq!(absolute_path("/etc", "etc/"), "/etc");
    }
}
