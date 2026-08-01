# Lave Station

A Gtk Based GUI for Docker, implemented in Rust, using native control of Docker.

# Iterations

Version 1 (implemented — see [the plan](./docs/iteration_1_plan.md)) - the essential application; window, persistent activity monitor indicator in home screen menu, tree menu on left hand side populated with a node containing Images available on the local device and another listing Containers (stopped and running) available on the local device. Selecting any item in the left hand side tree menu causes the metadata for that item to be rendered in the main part of the window on the right hand side. The root node of the tree, when selected (as it is by default at application start), causes the main part of the window to display various metadata for the local docker environment.


# Building and running

Build prerequisites (Debian 13):

```
sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev
```

Then:

```
cargo build
cargo run -p lave
```

Options: `--docker-host <URL>` overrides `DOCKER_HOST` and any active Docker context,
`--log-level <level>` sets verbosity, and `--no-indicator` suppresses the panel
indicator.

## The panel indicator

The activity monitor is published as a StatusNotifierItem. KDE, Xfce, Cinnamon and MATE
show these natively. **GNOME does not**, so it needs an extension:

```
sudo apt install gnome-shell-extension-appindicator
```

Enable it and log out and back in. Without a StatusNotifier host, Lave Station says so
and closes on window close rather than leaving itself running with no way to reach it.

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
