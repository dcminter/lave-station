//! A container's filesystem, mounted read-only so any file manager can browse it.
//!
//! This is the piece that makes "open in the system file manager" work beyond GNOME:
//! once it is a real directory, the desktop's own handler takes it from there.
//!
//! Three things shape the design:
//!
//! * **FUSE is synchronous, the daemon client is not.** Rather than marshalling every
//!   request onto the application's runtime — which would couple mount latency to
//!   whatever the session loop happens to be doing — the mount opens its **own**
//!   connection and runs its own single-threaded runtime. A second Unix socket
//!   connection is cheap, and the isolation means a slow read cannot stall the window.
//! * **The archive endpoint is recursive-only**, so `readdir` reuses
//!   [`lave_core::model::fs_tree`]'s index and its budget.
//! * **There is no range support**, so `read` fetches a whole file once and caches it.
//!
//! Read-only throughout. The archive endpoint can write, and deliberately goes unused:
//! a file manager that appears to support editing but silently discards changes would be
//! worse than one that does not offer it.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use std::sync::Mutex;

use fuser::{
    Config, Errno, FileAttr, FileHandle, FileType, Filesystem, Generation, INodeNo, LockOwner,
    MountOption, OpenFlags, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry, Request,
};
use lave_core::engine::{ContainerEngine, bollard_engine::BollardEngine};
use lave_core::model::fs_tree::{self, DEFAULT_BUDGET, EntryKind, Indexer, Node};

/// How long the kernel may trust what we told it. Short, because a running container's
/// filesystem changes underneath us and a stale answer is worse than another round trip.
const TTL: Duration = Duration::from_secs(2);

/// The inode the kernel always starts from.
const ROOT_INODE: u64 = 1;

/// Files larger than this are not cached after being read. Enough to hold configuration
/// and source files; a multi-gigabyte layer blob is fetched again rather than kept.
const MAX_CACHED_FILE: usize = 8 * 1024 * 1024;

/// Everything mounted by this application lives under here, so a sweep can find strays.
const MOUNT_DIR: &str = "lave-station";

/// A live mount. Unmounts when dropped.
pub struct Mount {
    path: PathBuf,
    /// Kept only for its `Drop`, which unmounts.
    _session: fuser::BackgroundSession,
}

impl Mount {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Mount `container_id`'s filesystem and return the directory it appears at.
///
/// # Errors
///
/// If the runtime directory is unusable, the daemon cannot be reached on a fresh
/// connection, or the kernel refuses the mount.
pub fn mount(endpoint: &Path, container_id: &str, label: &str) -> Result<Mount, String> {
    let path = mount_point(label, container_id)?;
    std::fs::create_dir_all(&path).map_err(|error| format!("{}: {error}", path.display()))?;

    let filesystem = ContainerFs::connect(endpoint, container_id)?;

    let mut config = Config::default();
    // Owner only, which is Config's default ACL: the mount exposes an image's contents,
    // and nothing about it is other users' business.
    config.mount_options = vec![
        MountOption::RO,
        MountOption::NoDev,
        MountOption::NoSuid,
        MountOption::NoExec,
        // Named so it is identifiable in `mount` output rather than appearing as an
        // anonymous fuse mount.
        MountOption::FSName(format!("lave-{label}")),
        MountOption::Subtype("lave-station".to_owned()),
    ];

    let session = fuser::spawn_mount(filesystem, &path, &config)
        .map_err(|error| format!("could not mount at {}: {error}", path.display()))?;

    Ok(Mount {
        path,
        _session: session,
    })
}

/// Remove empty mount directories left by a previous run.
///
/// Only empty ones: a directory with anything in it is either still mounted or is not
/// ours to touch, and either way removing it would be wrong.
pub fn sweep_stale_mounts() {
    let Some(root) = mount_root() else {
        return;
    };

    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };

    for entry in entries.flatten() {
        // remove_dir refuses a non-empty directory, which is exactly the guard wanted.
        if std::fs::remove_dir(entry.path()).is_ok() {
            tracing::debug!("removed a stale mount point at {}", entry.path().display());
        }
    }
}

fn mount_root() -> Option<PathBuf> {
    // Per-user, on tmpfs, and cleared by the system on logout even if we fail to.
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    Some(PathBuf::from(runtime).join(MOUNT_DIR))
}

fn mount_point(label: &str, container_id: &str) -> Result<PathBuf, String> {
    let root = mount_root().ok_or_else(|| {
        "XDG_RUNTIME_DIR is not set, so there is nowhere private to mount".to_owned()
    })?;

    // The label is a container or image name and may contain anything; the short ID
    // keeps it unique. Both are reduced to characters that cannot escape the directory.
    let safe: String = label
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .collect();
    let short: String = container_id.chars().take(12).collect();

    Ok(root.join(format!("{safe}-{short}")))
}

/// The mutable half, behind a lock because `Filesystem` hands us `&self`.
#[derive(Default)]
struct State {
    /// Inode to path, and back. Assigned as paths are discovered: FUSE hands us inodes,
    /// the daemon wants paths.
    paths: HashMap<u64, String>,
    inodes: HashMap<String, u64>,
    next_inode: u64,
    /// Indexed directories, keyed by the directory's path.
    listings: HashMap<String, Vec<Node>>,
    /// Attributes for every path seen, so `getattr` need not re-index.
    attributes: HashMap<String, Node>,
    contents: HashMap<String, Vec<u8>>,
}

impl State {
    fn inode_for(&mut self, path: &str) -> u64 {
        if let Some(inode) = self.inodes.get(path) {
            return *inode;
        }

        let inode = self.next_inode;
        self.next_inode += 1;
        self.paths.insert(inode, path.to_owned());
        self.inodes.insert(path.to_owned(), inode);
        inode
    }
}

struct ContainerFs {
    runtime: tokio::runtime::Runtime,
    engine: BollardEngine,
    container_id: String,
    state: Mutex<State>,
}

impl ContainerFs {
    fn connect(endpoint: &Path, container_id: &str) -> Result<Self, String> {
        // Single-threaded: requests serialise on the lock anyway, and nothing here
        // fans out.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("could not start a runtime for the mount: {error}"))?;

        let engine = runtime
            .block_on(BollardEngine::connect(endpoint))
            .map_err(|error| format!("could not connect for the mount: {error}"))?;

        let mut state = State {
            next_inode: ROOT_INODE + 1,
            ..State::default()
        };
        state.paths.insert(ROOT_INODE, "/".to_owned());
        state.inodes.insert("/".to_owned(), ROOT_INODE);

        Ok(Self {
            runtime,
            engine,
            container_id: container_id.to_owned(),
            state: Mutex::new(state),
        })
    }

    /// Take the lock, recovering rather than propagating a poisoning: a panic in one
    /// request must not make the whole mount unusable.
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn path_for(&self, inode: INodeNo) -> Option<String> {
        self.state().paths.get(&inode.0).cloned()
    }

    /// Fetch and index a directory unless it is already known.
    fn ensure_listing(&self, path: &str) {
        if self.state().listings.contains_key(path) {
            return;
        }

        let index = {
            let mut indexer = Indexer::new(path, DEFAULT_BUDGET);
            let mut stream = self.engine.archive(&self.container_id, path);
            self.runtime.block_on(async {
                use futures_util::StreamExt;
                while let Some(Ok(chunk)) = stream.next().await {
                    if !indexer.push(&chunk) {
                        break;
                    }
                }
            });
            indexer.finish()
        };

        let children: Vec<Node> = index.tree.children(path).into_iter().cloned().collect();

        let mut state = self.state();
        // Everything the index saw is worth keeping: a later getattr on a child then
        // costs nothing, and the expensive part has already been paid for.
        for node in &children {
            state.attributes.insert(node.path.clone(), node.clone());
        }
        if let Some(node) = index.tree.get(path) {
            state.attributes.insert(path.to_owned(), node.clone());
        }
        state.listings.insert(path.to_owned(), children);
    }

    fn attribute(&self, path: &str) -> Option<Node> {
        if let Some(node) = self.state().attributes.get(path) {
            return Some(node.clone());
        }

        // Not seen yet: index its parent, which is what a lookup would have done.
        self.ensure_listing(&fs_tree::parent_of(path));
        self.state().attributes.get(path).cloned()
    }

    fn content(&self, path: &str) -> Option<Vec<u8>> {
        if let Some(cached) = self.state().contents.get(path) {
            return Some(cached.clone());
        }

        let bytes = {
            let mut collected = Vec::new();
            let mut stream = self.engine.archive(&self.container_id, path);
            self.runtime.block_on(async {
                use futures_util::StreamExt;
                while let Some(Ok(chunk)) = stream.next().await {
                    collected.extend_from_slice(&chunk);
                }
            });
            collected
        };

        let content = fs_tree::extract_file(&bytes)?;

        if content.len() <= MAX_CACHED_FILE {
            self.state()
                .contents
                .insert(path.to_owned(), content.clone());
        }

        Some(content)
    }

    fn attr(&self, path: &str) -> Option<FileAttr> {
        let node = self.attribute(path)?;
        let inode = self.state().inode_for(path);
        Some(to_attr(inode, &node))
    }
}

fn to_attr(inode: u64, node: &Node) -> FileAttr {
    let mtime = UNIX_EPOCH
        .checked_add(Duration::from_secs(u64::try_from(node.mtime).unwrap_or(0)))
        .unwrap_or(UNIX_EPOCH);

    FileAttr {
        ino: INodeNo(inode),
        size: node.size,
        blocks: node.size.div_ceil(512),
        atime: mtime,
        mtime,
        ctime: mtime,
        crtime: mtime,
        kind: to_kind(node.kind),
        // Write bits are cleared regardless of what the image says: the mount is
        // read-only, and advertising otherwise invites a file manager to try.
        perm: u16::try_from(node.mode & 0o555).unwrap_or(0o444),
        nlink: 1,
        // Owned by whoever mounted it. The container's own uids mean nothing here, and
        // showing them would make everything look unreadable.
        uid: own_uid(),
        gid: own_gid(),
        rdev: 0,
        blksize: 512,
        flags: 0,
    }
}

/// `getuid` without `unsafe`: the process's own directory reports its owner.
fn own_uid() -> u32 {
    std::fs::metadata("/proc/self")
        .map_or(0, |metadata| std::os::unix::fs::MetadataExt::uid(&metadata))
}

fn own_gid() -> u32 {
    std::fs::metadata("/proc/self")
        .map_or(0, |metadata| std::os::unix::fs::MetadataExt::gid(&metadata))
}

fn to_kind(kind: EntryKind) -> FileType {
    match kind {
        EntryKind::Directory => FileType::Directory,
        EntryKind::Symlink => FileType::Symlink,
        // Hard links and devices are presented as ordinary files: the content is what a
        // browser wants, and a device node here would be a lie anyway.
        _ => FileType::RegularFile,
    }
}

impl Filesystem for ContainerFs {
    fn lookup(&self, _request: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let Some(parent_path) = self.path_for(parent) else {
            reply.error(Errno::ENOENT);
            return;
        };
        let Some(name) = name.to_str() else {
            reply.error(Errno::ENOENT);
            return;
        };

        let path = fs_tree::normalise(&format!("{parent_path}/{name}"));
        match self.attr(&path) {
            Some(attr) => reply.entry(&TTL, &attr, Generation(0)),
            None => reply.error(Errno::ENOENT),
        }
    }

    fn getattr(
        &self,
        _request: &Request,
        inode: INodeNo,
        _fh: Option<FileHandle>,
        reply: ReplyAttr,
    ) {
        // The root has no parent to index, so it is described rather than looked up.
        if inode.0 == ROOT_INODE {
            reply.attr(&TTL, &root_attr());
            return;
        }

        let Some(path) = self.path_for(inode) else {
            reply.error(Errno::ENOENT);
            return;
        };

        match self.attr(&path) {
            Some(attr) => reply.attr(&TTL, &attr),
            None => reply.error(Errno::ENOENT),
        }
    }

    fn readdir(
        &self,
        _request: &Request,
        inode: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let Some(path) = self.path_for(inode) else {
            reply.error(Errno::ENOENT);
            return;
        };

        self.ensure_listing(&path);

        let mut state = self.state();
        let children = state.listings.get(&path).cloned().unwrap_or_default();
        let parent_inode = state.inode_for(&fs_tree::parent_of(&path));

        let mut entries: Vec<(u64, FileType, String)> = vec![
            (inode.0, FileType::Directory, ".".to_owned()),
            (parent_inode, FileType::Directory, "..".to_owned()),
        ];
        for node in &children {
            let child_inode = state.inode_for(&node.path);
            entries.push((child_inode, to_kind(node.kind), node.name.clone()));
        }
        drop(state);

        // The kernel resumes from an offset, so anything already sent is skipped.
        let start = usize::try_from(offset).unwrap_or(0);
        for (index, (child_inode, kind, name)) in entries.into_iter().enumerate().skip(start) {
            // The offset handed back is where to resume, so it is this entry plus one.
            if reply.add(INodeNo(child_inode), index as u64 + 1, kind, &name) {
                // The buffer is full; the kernel will ask again from here.
                break;
            }
        }

        reply.ok();
    }

    fn read(
        &self,
        _request: &Request,
        inode: INodeNo,
        _fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock: Option<LockOwner>,
        reply: ReplyData,
    ) {
        let Some(path) = self.path_for(inode) else {
            reply.error(Errno::ENOENT);
            return;
        };

        let Some(content) = self.content(&path) else {
            reply.error(Errno::EIO);
            return;
        };

        let start = usize::try_from(offset).unwrap_or(0).min(content.len());
        let end = start
            .saturating_add(usize::try_from(size).unwrap_or(0))
            .min(content.len());
        reply.data(&content[start..end]);
    }

    fn readlink(&self, _request: &Request, inode: INodeNo, reply: ReplyData) {
        let Some(path) = self.path_for(inode) else {
            reply.error(Errno::ENOENT);
            return;
        };

        match self.attribute(&path) {
            Some(node) if node.kind == EntryKind::Symlink => {
                reply.data(node.link_target.as_bytes());
            }
            _ => reply.error(Errno::ENOENT),
        }
    }
}

fn root_attr() -> FileAttr {
    let now = SystemTime::now();
    FileAttr {
        ino: INodeNo(ROOT_INODE),
        size: 0,
        blocks: 0,
        atime: now,
        mtime: now,
        ctime: now,
        crtime: now,
        kind: FileType::Directory,
        perm: 0o555,
        nlink: 2,
        uid: own_uid(),
        gid: own_gid(),
        rdev: 0,
        blksize: 512,
        flags: 0,
    }
}

#[cfg(test)]
mod tests {
    // expect is fine in tests; a failed assumption should abort the test.
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn a_mount_point_cannot_escape_the_runtime_directory() {
        // A container may be named anything; the path must stay a single component.
        let point = mount_point("../../etc/passwd", "abcdef123456").expect("a path");

        let name = point
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        assert!(!name.contains('/'), "got {name}");
        assert!(!name.contains(".."), "got {name}");
        assert!(name.ends_with("-abcdef123456"));
    }

    #[test]
    fn the_mount_point_is_unique_per_container() {
        let first = mount_point("web", "aaaaaaaaaaaa").expect("a path");
        let second = mount_point("web", "bbbbbbbbbbbb").expect("a path");

        assert_ne!(first, second, "two containers must not share a mount point");
    }

    #[test]
    fn write_permissions_are_never_advertised() {
        let node = Node {
            path: "/bin/sh".to_owned(),
            name: "sh".to_owned(),
            size: 10,
            // Writable by owner in the image, which the mount must not honour.
            mode: 0o755,
            mtime: 0,
            kind: EntryKind::File,
            link_target: String::new(),
        };

        let attr = to_attr(2, &node);

        assert_eq!(attr.perm & 0o222, 0, "no write bit may survive");
        assert_eq!(attr.perm, 0o555);
    }

    #[test]
    fn entry_kinds_map_to_something_a_file_manager_understands() {
        assert_eq!(to_kind(EntryKind::Directory), FileType::Directory);
        assert_eq!(to_kind(EntryKind::Symlink), FileType::Symlink);
        assert_eq!(to_kind(EntryKind::File), FileType::RegularFile);
        // A device node in a mounted image would be a lie; a plain file is honest.
        assert_eq!(to_kind(EntryKind::Other), FileType::RegularFile);
    }
}
