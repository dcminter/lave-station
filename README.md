# Lave Station

A Gtk Based GUI for Docker, implemented in Rust, using native control of Docker.

![Screen capture of the UI](./docs/screenshot.png)

Provides:
  * Visual list of running and/or stopped containers
  * Visual list of images
  * Control over all containers and images
  * Log viewers
  * Container and image metadata

# AI Declaration

This tool is pretty much pure vibe-coded with Claude Code to scratch my own itch!

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

View preferences - the sidebar width, view toggles on lists, and table column widths
are stored in GSettings under `com.paperstack.LaveStation`. Sort order is not stored
beyond the session lifetime.

The schema is also compiled with `cargo build` into the build directory, so an 
uninstalled run finds it without anything being installed system-wide. If it 
exists then a copy under `/usr/share/glib-2.0/schemas` takes precedence.

To read or reset the settings by hand, point `gsettings` at the compiled copy:

```
SCHEMAS=$(find target/debug/build -type d -name schemas | head -1)
gsettings --schemadir "$SCHEMAS" list-recursively com.paperstack.LaveStation
gsettings --schemadir "$SCHEMAS" reset-recursively com.paperstack.LaveStation
```

## The panel indicator

The activity monitor is published as a StatusNotifierItem. KDE, Xfce, Cinnamon and MATE
show these natively. **GNOME does not**, so it needs an extension:

```
sudo apt install gnome-shell-extension-appindicator
```

Enable it and log out and back in. Without a StatusNotifier host the tool will
close the session entirely on window close.

## Browsing a container in your file manager

Selecting a container and choosing **Open in Files** mounts its filesystem read-only
under `$XDG_RUNTIME_DIR/lave-station/` and then hands the directory to the desktop
directory browser via XDG Desktop Portal. This needs `fusermount3`, which is in 
the `fuse3` package:

```
sudo apt install fuse3
```

The mount point is lazy; nothing will be transferred until a read is attempted. The
mount is read-only and the mount only exists while the application is running.

Image (rather than container) browsing is managed entirely within the app because it needs
a stand-in container for any filesystem to exist.

## Installing for a test run

To get the icon and launcher association working:

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
cargo test                                     # No docker daemon required
cargo test --features live-docker              # adds tests against the local docker daemon
cargo test -p lave --features live-gtk         # adds tests that build widgets; needs a display
cargo clippy --all-targets -- -D warnings      # Aggressive clippy review
```

Business logic lives in `crates/lave-core`, which has no GTK or D-Bus dependency, so
`cargo test -p lave-core` runs without GTK development packages or a display.

