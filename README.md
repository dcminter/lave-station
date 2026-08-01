# Lave Station

A Gtk Based GUI for Docker, implemented in Rust, using native control of Docker.

# Iterations

Version 1 (implemented — see [the plan](./docs/iteration_1_plan.md)) - the essential application; window, persistent activity monitor indicator in home screen menu, tree menu on left hand side populated with a node containing Images available on the local device and another listing Containers (stopped and running) available on the local device. Selecting any item in the left hand side tree menu causes the metadata for that item to be rendered in the main part of the window on the right hand side. The root node of the tree, when selected (as it is by default at application start), causes the main part of the window to display various metadata for the local docker environment.

Version 2 (implemented — see [the plan](./docs/iteration_2_plan.md)) - spit and polish; the left hand tree panel is resizeable by dragging its divider, and the width is remembered between runs. The state icons carry colour as well as shape and text - green for running, red for stopped, amber for anything in between - and the three standing nodes are coloured for identity: Docker blue for the daemon, soft violet for Images, soft teal for Containers. Images are titled by tag alone, falling back to the short ID only when there is no tag, and are sorted by tag with the untagged ones following in descending order of age; containers are titled and sorted by name. The root node's panel is led by a full-width table of containers with the columns `docker ps` reports, toggleable between running-only and everything, sortable by any column and sorted by creation date by default - with the toggle and the sort order both remembered between runs. That table opens sized to fit the running containers, up to twenty of them, and has a draggable divider beneath it for reaching the rest. The Images and Containers nodes likewise render as sortable tables, and the single-object pages lay their groups out in two columns when the window is wide enough. Finally, the relationship between containers and images is made visible and navigable in all three of the senses in which it exists - one image to many containers, a tag that has moved out from under a running container, and one image derived from another.

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
its sort order — are remembered in `$XDG_CONFIG_HOME/lave-station/settings.json`
(`~/.config/lave-station/settings.json` by default). Deleting the file restores the
defaults; a corrupt one is ignored rather than being an error.

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
