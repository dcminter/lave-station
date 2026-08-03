# Lave Station

A Gtk Based GUI for Docker, implemented in Rust, using native control of Docker.

# Iterations

Version 1 (implemented — see [the plan](./docs/iteration_1_plan.md)) - the essential application; window, persistent activity monitor indicator in home screen menu, tree menu on left hand side populated with a node containing Images available on the local device and another listing Containers (stopped and running) available on the local device. Selecting any item in the left hand side tree menu causes the metadata for that item to be rendered in the main part of the window on the right hand side. The root node of the tree, when selected (as it is by default at application start), causes the main part of the window to display various metadata for the local docker environment.

Version 2 (implemented — see [the plan](./docs/iteration_2_plan.md)) - spit and polish; the left hand tree panel is resizeable by dragging its divider, and the width is remembered between runs. The state icons carry colour as well as shape and text - green for running, red for stopped, amber for anything in between - and the three standing nodes are coloured for identity: Docker blue for the daemon, soft violet for Images, soft teal for Containers. Images are titled by tag alone, falling back to the short ID only when there is no tag, and are sorted by tag with the untagged ones following in descending order of age; containers are titled and sorted by name. The root node's panel is led by a full-width table of containers with the columns `docker ps` reports, toggleable between running-only and everything, sortable by any column and sorted by creation date by default - with the toggle and the sort order both remembered between runs. That table opens sized to fit the running containers, up to twenty of them, and has a draggable divider beneath it for reaching the rest. The Images and Containers nodes likewise render as sortable tables, and the single-object pages lay their groups out in two columns when the window is wide enough. Finally, the relationship between containers and images is made visible and navigable in all three of the senses in which it exists - one image to many containers, a tag that has moved out from under a running container, and one image derived from another.

Version 4 (implemented — see [the plan](./docs/iteration_4_plan.md)) - another round of polish; preferences move to GSettings, with the schema compiled by the Cargo build so an uninstalled run still finds it. The list views gain a checkbox column and a cog above them, insensitive until something is checked: an action is offered when at least one checked object can take it, applies only to those objects, and says in its own label how many it will touch, so starting a mixed selection starts the stopped ones and leaves the rest alone. Removing a selection forces per container rather than across the whole of it. The context menus carry icons, with anything that removes or halts marked out in red. Column widths are remembered between runs; sort order is deliberately not, though it does survive a refresh. Logs open on the tail, at the bottom rather than the top, and keep up as lines arrive until the user scrolls back to read something; the whole log is there on request. Several containers can now be followed at once, which the single stream of version 3 could not do.

Version 3 (implemented — see [the plan](./docs/iteration_3_plan.md)) - interaction; right-clicking anything in the tree offers what can be done to it. Containers can be started, stopped, restarted, paused and killed, and containers and images can be removed; pruning lives in the application menu, since it acts on the daemon rather than on a selection. Reversible actions act immediately; anything that loses data confirms first, naming what goes and listing every object a prune would remove. Output opens as tabs beside the metadata rather than in dialogs, one per object, so several can be kept in view at once: a container's logs stream in with stderr distinguished, its filesystem appears as a tree that expands in place, and an image's Dockerfile is reconstructed from the build history - `FROM` line included, resolved through Version 2's layer analysis. A container's filesystem can also be mounted read-only over FUSE and handed to the desktop's own file manager. An image's filesystem is reached through a labelled container created from it and never started.

## The image relationships

An image is not a set of layered images. Since Docker 1.10 it is a config document
naming an ordered stack of layers, and `Parent` is empty on anything BuildKit produced.
So derivation is reconstructed from *shared layer prefixes*: if every layer of A is also
the bottom of B, in order, then B was built `FROM` A. See
[the plan](./docs/iteration_2_plan.md) §2 for the evidence.

That costs one image inspect per image, done eight at a time after each listing and
cached by image ID, so a refresh only inspects images it has not seen before.

# Building and running

Build prerequisites (Debian 13):

```
sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev
```

`glib-compile-schemas`, which the build runs, comes with `libglib2.0-dev` — a dependency
of `libgtk-4-dev`, so installing the above is enough.

Lave Station targets **GTK 4.14 or newer** and **libadwaita 1.5 or newer**, which means
Ubuntu 24.04 LTS, Debian 13, or anything more recent.

Then:

```
cargo build
cargo run -p lave
```

Options: `--docker-host <URL>` overrides `DOCKER_HOST` and any active Docker context,
`--log-level <level>` sets verbosity, and `--no-indicator` suppresses the panel
indicator.

View preferences — the sidebar width, the container table's running-only/all toggle and
the widths you have dragged table columns to — are kept in GSettings under
`com.paperstack.LaveStation`. Sort order is deliberately not among them: it lasts for
the session, and every launch opens newest-first.

The schema is compiled by `cargo build` into the build directory, so an uninstalled run
finds it without anything being installed system-wide. An installed copy under
`/usr/share/glib-2.0/schemas` takes precedence. To read or reset the settings by hand,
point `gsettings` at the compiled copy:

```
SCHEMAS=$(find target/debug/build -type d -name schemas | head -1)
gsettings --schemadir "$SCHEMAS" list-recursively com.paperstack.LaveStation
gsettings --schemadir "$SCHEMAS" reset-recursively com.paperstack.LaveStation
```

Versions up to 3 used `~/.config/lave-station/settings.json`. That file is no longer
read or written, and can be deleted.

## The panel indicator

The activity monitor is published as a StatusNotifierItem. KDE, Xfce, Cinnamon and MATE
show these natively. **GNOME does not**, so it needs an extension:

```
sudo apt install gnome-shell-extension-appindicator
```

Enable it and log out and back in. Without a StatusNotifier host, Lave Station says so
and closes on window close rather than leaving itself running with no way to reach it.

The indicator's icon is monochrome, because panels paint symbolic icons in their own
foreground colour. When the connection is lost in a way retrying will not fix, the item
is marked `NeedsAttention` and the panel emphasises it however that desktop does — a
transient reconnect does not, since the application is already handling it.

## Browsing a container in your file manager

Selecting a container and choosing **Open in Files** mounts its filesystem read-only
under `$XDG_RUNTIME_DIR/lave-station/` and hands the directory to whatever the desktop
uses to browse directories, via the XDG Desktop Portal where there is one. This needs
`fusermount3`, which is in the `fuse3` package:

```
sudo apt install fuse3
```

No `libfuse3-dev` is required: the FUSE protocol is spoken directly.

The mount is lazy — nothing is transferred until something is read — and read-only
throughout. Mounts last until the application exits. An image is browsed in the window
rather than mounted, since it needs a stand-in container first.

## Installing for a test run

Packaging is a later iteration. To get the icon and launcher association working now:

```
install -Dm644 crates/lave/data/com.paperstack.LaveStation.desktop \
  ~/.local/share/applications/com.paperstack.LaveStation.desktop
install -Dm644 crates/lave/data/icons/hicolor/scalable/apps/com.paperstack.LaveStation.svg \
  ~/.local/share/icons/hicolor/scalable/apps/com.paperstack.LaveStation.svg
gtk4-update-icon-cache -f -t ~/.local/share/icons/hicolor
```

The `Exec=lave` line assumes the binary is on your `PATH`.

# Testing

```
cargo test                                    # hermetic; no daemon required
cargo test --features live-docker              # adds tests against the local daemon
cargo clippy --all-targets -- -D warnings
```

Business logic lives in `crates/lave-core`, which has no GTK or D-Bus dependency, so
`cargo test -p lave-core` runs without GTK development packages or a display.

# AI Declaration

This tool is pretty much pure vibe-coded with Claude Code to scratch my own itch!
