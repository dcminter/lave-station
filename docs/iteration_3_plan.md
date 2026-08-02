# Iteration 3 — Interaction

Version 3 is where Lave Station stops being a viewer. It gains the ability to change
the daemon's state, to stream logs, to reconstruct an image's Dockerfile, and to expose
a container's filesystem as a real directory the desktop's own file manager can browse.

The scope, as asked for:

1. Start and stop existing containers.
2. View the logs of a container in either state.
3. Navigate a container's filesystem, and open it in the system file manager in a way
   that is not tied to GNOME.
4. View the Dockerfile an image was built from, if that is possible.
5. View an image's filesystem, read-only.

Three decisions widened it, all taken deliberately:

6. The mutation surface includes **removal and pruning**, not just start and stop.
7. Image filesystems are reached by creating a **throwaway container**.
8. The file manager gets a **live FUSE mount**, not an extracted copy.

Nothing here weakens the rules in
[the daemon integration notes](./container_daemon_integration.md): the API is spoken
directly, never the CLI; the application never runs as root and never invokes `sudo`.
What does change is §"read-only, socket-only" from
[the iteration 2 plan](./iteration_2_plan.md) — that posture is now explicitly retired,
and §4 below is what replaces it.

## 1. What the daemon will and will not give us

Probed against Docker 29.6.2 on the development host — rootful, `overlayfs`,
`DockerRootDir=/var/lib/docker`. Every claim below was checked rather than assumed.

### 1.1 The filesystem API is recursive-only

`GET /containers/{id}/archive?path=P` returns a tar of `P` **and its entire subtree**.
There is no non-recursive mode and no metadata-only mode.

```
GET /containers/{hello-world}/archive?path=/   →  14 members, whole tree
HEAD  same  →  X-Docker-Container-Path-Stat: base64 JSON, one path only
```

So *listing a directory* means streaming its whole subtree over the socket and throwing
the content bytes away. Measured on the development host:

```
/etc of google/cloud-sdk:emulators   1,532,416 bytes   54ms
```

Cheap for `/etc`. `/usr` on the same image is over a gigabyte. This is the single
biggest constraint on the design, and §5 is how it is handled.

Two mitigations do **not** work and were rejected:

* **Reading the host's storage directly.** `/var/lib/docker` is `root`-owned and
  unreadable to the invoking user, and `/proc/{pid}/root` is likewise unreachable for a
  container running as root. Both would require `sudo`, which is forbidden.
* **Asking the daemon for less.** There is no parameter for it.

### 1.2 It works on stopped containers

This was the good surprise. The archive endpoint served a complete tar of `/` from an
**exited** `hello-world`. Filesystem browsing therefore needs neither a running process
nor a shell in the image, which rules `exec`-based listing *out* as the primary
mechanism — it would fail on every stopped container and on every distroless or
`scratch` image.

`exec ls` remains available as a cheap optimisation for running containers that happen
to have a shell. It is an optimisation only; the archive path must work alone.

### 1.3 A single file comes back on its own

```
GET /containers/{id}/archive?path=/etc/hosts   →  tar with one member, 148 bytes
```

Which is what makes a FUSE `read()` viable. There is no range support, so a partial read
of a large file still transfers the whole thing — hence the file cache in §6.

A missing path is a clean `404`, so "no such file" is distinguishable from a transport
failure.

### 1.4 The Dockerfile is reconstructable, not recoverable

The original Dockerfile is not stored. What *is* stored is the history, and on this host
it is unusually legible:

```
CMD ["pub-sub-monitor"]
ENTRYPOINT []
COPY /app/target/release/pub-sub-tui /usr/local/bin/pub-sub-tui # buildkit
RUN /bin/sh -c apt-get update && apt-get install -y --no-install-recommends ca-certificates ...
# debian.sh --arch 'amd64' out/ 'bookworm' '@1781049600'
```

The last line is the *base image's* own history, which is where the `FROM` boundary
falls — and Version 2's `relations::base_of` already tells us which local image that is.
That combination gives a genuinely useful reconstruction, including the one line
`docker history` cannot produce.

**It is a reconstruction and will be labelled as one.** Known losses:

* `COPY --from=builder` keeps the source path but loses the stage, as visible above:
  `/app/target/release/pub-sub-tui` is a path in a build stage that no longer exists.
* `ARG` values are baked into the recorded command; the `ARG` declarations are gone.
* Squashed and `--no-cache`-flattened images lose the instruction boundaries entirely.
* An image with no local base resolves `FROM` to a bare digest.

Presenting this as *the* Dockerfile would be a lie. Presenting it as a reconstruction,
with the losses stated in the pane rather than buried in a doc, is honest and still
worth having.

## 2. Sequencing

Each step stands alone and is useful on its own, so the version can be judged as it
lands rather than only at the end.

| # | Step | Depends on |
|---|---|---|
| 1 | Engine trait: mutation, streaming, archive | — |
| 2 | `model::action` — what is offered, and what it warns | 1 |
| 3 | Action buttons and confirmation dialogs | 2 |
| 4 | Log viewer | 1 |
| 5 | Dockerfile reconstruction | 1 |
| 6 | Filesystem index from tar headers | 1 |
| 7 | Image browsing via throwaway container | 6 |
| 8 | FUSE mount and file-manager hand-off | 6 |

## 3. The engine trait stops being read-only

`ContainerEngine`'s doc comment currently reads "Everything iteration 1 asks of a
container daemon. Read-only by design." That guarantee is being given up, so it is
replaced rather than quietly deleted.

Added: `start`, `stop`, `restart`, `pause`, `unpause`, `kill`, `remove_container`,
`remove_image`, `prune_containers`, `prune_images`, `logs`, `archive`, `stat_path`,
`image_history`, `create_scratch_container`.

`FakeEngine` grows a recording of what it was asked to do, so the widget layer's
behaviour under failure is testable without a daemon — which matters far more now that
calls can destroy things.

## 4. The safety design for destructive actions

Removal and pruning were added at the user's explicit request. They are the only
operations in the application that can lose data, and they are treated accordingly.

**Reversibility is the classification, not severity.** Start, stop, restart, pause,
unpause and kill all leave the container recoverable, so they act immediately with no
dialog — a confirmation on every stop trains the user to dismiss dialogs unread, which
is precisely what makes the removal dialog dangerous.

**Removal and pruning always confirm**, with:

* the object named as the user knows it — the container's name, the image's tag — and
  never only by ID;
* what else goes with it, stated concretely: a container's writable layer, the
  containers that would block an image removal, the reclaimable bytes;
* a destructively-styled confirm button, and Cancel as the default.

**Prune previews exactly what it will remove.** The daemon has no dry-run, so the
preview is computed here from the listings already held — exited containers, untagged
images — and the dialog lists them by name. A preview that says "and 14 others" is not a
preview. If nothing matches, the action reports that instead of opening an empty dialog.

**Force is never implicit.** Removing a running container is a separate, differently
worded confirmation that says it will be killed first.

Everything is decided in `model::action` and tested there: which actions a state offers,
what each dialog says, and what the preview contains. The widget layer renders the
decision and cannot invent one.

## 5. Filesystem indexing under a recursive API

Given §1.1, a directory listing costs its whole subtree. The design accepts that and
makes the cost visible and bounded rather than pretending it is not there.

* **One fetch indexes a whole subtree.** The tar is read for headers only — name, size,
  mode, mtime, link target — and content bytes are discarded as they stream past. The
  resulting tree is cached, so the first visit to a directory pays for everything
  beneath it and every subsequent visit within that subtree is free.
* **A byte budget bounds it.** Indexing stops at the budget and the tree is marked
  truncated. The pane says so, names the directory, and offers to index a chosen
  subdirectory instead. A truncated tree is never drawn as though it were complete.
* **It is cancellable**, with progress, because a gigabyte over a socket is long enough
  for the user to change their mind.
* **`/` is not indexed eagerly.** Opening a container's filesystem indexes `/` to depth
  one by fetching each top-level entry's stat, not by fetching `/` whole.

The parsing, the budget arithmetic and the truncation reporting all live in
`lave-core::model::fs_tree` and are tested against synthetic tar streams. No daemon is
involved in the tests.

## 6. FUSE

The live mount was chosen over extracting a copy. The advantages are real — it is
lazy, so nothing is transferred until read; it is live, so a running container's changes
are visible; and any file manager on any desktop can open it, because it is just a
directory.

Design:

* **`fuser` 0.18 with `default-features = false`.** The default feature set links
  `libfuse`, which would add a `libfuse3-dev` build dependency. Without it, `fuser`
  speaks the FUSE protocol over `/dev/fuse` directly and shells out only to
  `fusermount3` for the mount itself. Verified present on the target platform:
  `fusermount3` in `/usr/bin`, `/dev/fuse` mode `0666`.
* **Read-only, always.** Version 3 adds no write path. The archive API can write
  (`PUT`), and deliberately goes unused: a file manager that appears to support editing
  but silently discards changes is worse than one that does not offer it.
* **Mounted under `$XDG_RUNTIME_DIR`**, one directory per mounted object, so mounts are
  per-user, on tmpfs, and cleaned up by the system on logout even if we fail to.
* **Unmounted on exit**, with a startup sweep for stale mounts left by a crash.
* **`readdir` reuses §5's index**, including its budget. A directory whose index was
  truncated reports what it has; it does not block forever trying to complete.
* **`read` fetches the whole file once and caches it**, since the endpoint has no range
  support (§1.3). Files above a size threshold are fetched to a temp file rather than
  held in memory.

The hand-off to the file manager is `gtk::FileLauncher` (GTK 4.10+, present in the
pinned 0.11.4), which routes through the XDG Desktop Portal where one exists and falls
back to the session's default handler otherwise. That is what makes it work on KDE,
Xfce and Cinnamon rather than only GNOME — the request that prompted this design.

**The risk, stated plainly:** this is the largest and least certain piece of work in the
version. A FUSE filesystem is a kernel-facing interface with its own failure modes —
stale handles, unmount races, blocking reads on a slow socket — and none of it is
exercisable by the hermetic test suite. The mitigation is that the interesting logic
(path resolution, the index, cache policy) lives in `lave-core` and is tested there; the
FUSE adapter itself is kept as thin as the GTK layer is.

### What `fuser` 0.18 actually looks like

The API differs from the one this plan assumed, in ways that changed the design:

* **`Filesystem` methods take `&self`, not `&mut self`.** All mutable state — the inode
  map, the caches — therefore sits behind a `Mutex`. Requests serialise on it, which
  costs nothing here because the session is single-threaded anyway.
* **Inodes, file handles, errnos and generations are newtypes** (`INodeNo`,
  `FileHandle`, `Errno`, `Generation`) rather than integers.
* **Mounting is `spawn_mount` with a `Config`**, not `spawn_mount2` with a slice of
  options, and `Config` is `#[non_exhaustive]`.

### Verified against a real container

Mounted a never-started `hello-world` and exercised it from the shell:

```
lave-probe /run/user/1000/lave-station/probe-… fuse.lave-station ro,nosuid,nodev,noexec
```

* `ls -la` lists the image's own contents with correct sizes.
* `/etc/mtab` reports its real target, `/proc/mounts`.
* `md5sum` of `/hello` through the mount matches the same file fetched from the archive
  endpoint directly, and reads at an offset return the right bytes.
* `touch` fails with "Read-only file system".
* The mount disappears from `/proc/mounts` when the process exits.

One thing that looked like a bug and was not: `/etc/hostname` reads as empty. It is
genuinely zero bytes until a container is started, which is when Docker writes it.

## 7. Throwaway containers for image filesystems

An image has no filesystem the daemon will serve — the archive endpoint is a container
endpoint. So browsing one means creating a container from it and never starting it.

* Created with our own label, `com.paperstack.lave-station.scratch=1`, and a recognisable
  name.
* Never started. No entrypoint runs; nothing in the image executes.
* Removed as soon as browsing ends.
* **Swept at startup.** Anything carrying that label from a previous run is removed
  before the window opens, so a crash cannot leak containers into the user's `docker ps`.

This is a real side effect and is disclosed in the UI at the point of use, not buried
here.

## 7a. The interaction rework

The first cut put actions on a button bar and opened everything else in dialogs. Both
were wrong in the same way: a dialog is a dead end, and several of them at once are
impossible to keep track of. What replaced them:

* **Actions are on a context menu**, on the sidebar rows *and* the tables, built from
  `model::action` as the buttons were.

  **Opening the menu does not change the selection.** Acting on a container must not move
  the panel away from whatever is being looked at — that was the whole complaint the
  rework answers, and an early version got it backwards by selecting the right-clicked
  row "so the menu and the Details tab agree". They do not need to agree: the menu
  carries its own target. `menu_target` holds the right-clicked object alongside the
  offers, and every step from the menu item to the daemon call takes that object
  explicitly rather than reading the selection.

  The gesture is attached per row rather than to the list, so the row itself identifies
  its object and no coordinate has to be mapped back to a list position. The two views
  reach the object differently, because their widget hierarchies differ:

  | View | Route |
  | --- | --- |
  | Sidebar | `GtkTreeExpander::list_row` → item, attached once in `setup` |
  | Tables | the cell's `GtkListItem` item, attached in `bind`, removed in `unbind` |

  The table needs the `bind`/`unbind` pair because the row is not known until `bind`, and
  a recycled cell would otherwise accumulate a gesture per binding.
  `GtkColumnViewRow` would be the natural home for it but is not a widget, so it cannot
  carry a controller at all.

  **The menu is not modal**, so moving from one row's menu to another's costs one click
  rather than two. A modal popover takes a grab, and the click that dismisses it is
  swallowed by that grab instead of reaching the row underneath — so with modality the
  second row needs dismiss-then-open.

  Giving up modality means dismissal becomes ours. The menu closes when an item is
  chosen, on `Escape`, when another menu opens, and on a click outside it — the last
  detected by comparing the click against `compute_bounds`. That comparison is only valid
  once a layout pass has run: measured in the same tick as `popup()` the bounds are
  `0x0`, and only afterwards do they become real (`170x220` in testing). Since a user's
  click on an item necessarily follows layout, the test holds; but it is why the outside
  check must not be moved any earlier. Focus is asked for explicitly after `popup()`,
  because without the grab a modal popover would have taken, the menu could not otherwise
  be driven from the keyboard.

  **The popover is anchored to the toast overlay, not to the widget that was clicked.**
  A table cell is not a durable anchor: the detail pane is rebuilt whenever a refresh
  arrives, and a popover whose parent has left the widget tree cannot be realized. This
  first showed up when opening the menu still re-selected — the re-render destroyed the
  cell immediately — producing

  ```
  Gtk-WARNING  Calling gtk_widget_realize() on a widget that isn't inside a toplevel window
  Gtk-CRITICAL gtk_native_get_surface: assertion 'GTK_IS_NATIVE (self)' failed
  Gdk-CRITICAL gdk_surface_new_popup: assertion 'GDK_IS_SURFACE (parent)' failed
  ```

  and a segmentation fault under the broadway backend. The sidebar never showed it,
  because re-rendering the detail pane leaves the sidebar alone. The click point is now
  translated into the overlay's coordinates with `compute_point` **before** selecting,
  and the overlay — the outermost content, never rebuilt — is what the popover hangs
  from.
* **The button bar is gone**, which gives every page back the vertical space it took.
* **Output opens as tabs** in an `AdwTabView`. Tab one is Details, pinned so it has no
  close button: it follows the selection and there is nothing sensible to close it to.
* **One tab per object per kind**, so two containers' logs can sit side by side. Asking
  for something already open focuses it rather than stacking a duplicate. Closing a logs
  tab stops its stream; closing a files tab releases the scratch container.
* **Prune moved to the primary menu**, where it belongs — it acts on the daemon, not on
  whatever happens to be selected. Each item is insensitive when it would remove nothing,
  recomputed from `for_environment` on every snapshot.

`Open in Files` was left exactly as it was: it launches the desktop's own file manager,
which was the point of it.

### Structured logs

A log line that is a whole JSON object has its keys and values coloured;
anything else is left plain. The decision lives in `model::logs::highlight` and is
tested there, returning spans in **character** offsets because that is what a
`GtkTextBuffer` indexes by — byte offsets would misplace every tag after the first
non-ASCII character.

The line is validated with `serde_json` before being tokenised, which is what keeps a
half-written line — a container flushing mid-write — plain rather than half-coloured.
Only objects qualify: a bare array is far more likely to be prose that happens to start
with a bracket. A string is a key exactly when the next non-space character is a colon,
which is the only thing distinguishing the two sides of `{"a":"b"}`.

### A re-entrancy trap worth remembering

`apply_listing` held a `RefCell` borrow across `ListStore::splice`. Splicing emits
`items-changed` synchronously, which sends GTK back into the `GtkTreeListModel` create
function to ask whether the new rows expand — and that wants the same cell mutably. The
result was not a caught panic but an **abort**, because it happens inside a callback
declared `nounwind`.

The rule this leaves: never hold a borrow of window state across a call into GTK that can
emit a signal. The borrow is now taken, used and released before the splice, and the
create function uses `try_borrow_mut` so a future mistake is reported rather than fatal.

### The file tree

Drilling into a list was replaced by a `GtkTreeListModel` that expands in place. This
turned out to suit the daemon better as well: because the archive endpoint is recursive,
indexing `/` already builds the whole subtree, so **expanding a directory costs a round
trip only the first time and nothing thereafter**. The runtime holds that index for as
long as the tab is open.

`Index::covers` decides whether the held index can answer without going back to the
daemon, and is careful about the case that matters: a *complete* index answers for paths
it does not hold, because their absence is the answer, but a *truncated* one cannot —
a directory missing from it may simply lie past where indexing stopped.

`GtkTreeListModel` needs a child model returned synchronously, so a directory's model is
created empty and filled when its listing arrives.

## 8. Test strategy

Unchanged in principle, and more important than before: everything that decides
something lives in `lave-core` and is tested without a display or a daemon.

New in this version, because mutation makes it necessary:

* `FakeEngine` records the calls made to it, so "the confirm button actually removed the
  right container" and "a failed stop surfaces an error" are assertions rather than
  manual checks.
* The prune preview is tested against listings, not against a daemon.
* `fs_tree` is tested against synthetic tar streams, including truncation at the budget
  and pathological entries — absolute paths, `..` components, symlink loops.

## 9. Risks

* **FUSE is the big one.** See §6.
* **Destructive actions are irreversible by definition.** The mitigation is §4, and the
  fact that every dialog's text is decided by a tested function rather than written
  inline at the call site.
* **The recursive archive API makes some directories expensive.** Bounded, reported, and
  cancellable (§5), but not solvable.
* **A reconstructed Dockerfile can mislead** if the reconstruction is mistaken for the
  original. Mitigated by labelling in the pane itself (§1.4).
