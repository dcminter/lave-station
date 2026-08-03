# Lave Station

A Gtk Based GUI for Docker, implemented in Rust, using native control of Docker.

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

View preferences — the sidebar width, the container tables' running-only/all toggle, the
image table's tagged-only/all toggle and the widths you have dragged table columns to —
are kept in GSettings under `com.paperstack.LaveStation`. Sort order is deliberately not
among them: it lasts for the session, and every launch opens newest-first.

The running-only choice is one setting shared by the environment page and the Containers
page, since it is one question; the two toggles cannot disagree about what is showing.

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
cargo test                                     # hermetic; no daemon required
cargo test --features live-docker              # adds tests against the local daemon
cargo test -p lave --features live-gtk         # adds tests that build widgets; needs a display
cargo clippy --all-targets -- -D warnings
```

Business logic lives in `crates/lave-core`, which has no GTK or D-Bus dependency, so
`cargo test -p lave-core` runs without GTK development packages or a display.

# AI Declaration

This tool is pretty much pure vibe-coded with Claude Code to scratch my own itch!
