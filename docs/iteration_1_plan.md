# Iteration 1 — Implementation Plan

Scope taken verbatim from `README.md`:

> Version 1 — the essential application; window, persistent activity monitor indicator
> in home screen menu, tree menu on left hand side populated with a node containing
> Images available on the local device and another listing Containers (stopped and
> running) available on the local device. Selecting any item in the left hand side tree
> menu causes the metadata for that item to be rendered in the main part of the window
> on the right hand side. The root node of the tree, when selected (as it is by default
> at application start), causes the main part of the window to display various metadata
> for the local docker environment.

Iteration 1 is **read-only**. Nothing in it mutates daemon state.

---

## 1. Decisions taken

Settled before implementation; recorded here because several are irreversible or
expensive to change later.

| Decision | Choice | Note |
|---|---|---|
| Application ID | `com.paperstack.LaveStation` | Fixed forever after release; `.desktop` basename and icon filename match it exactly |
| Display name / binary | Lave Station / `lave` | |
| UI definition | `gtk4-rs` + GtkBuilder `.ui` XML, GResource via `build.rs`, pure Cargo | No Meson or Blueprint toolchain needed; `data/` laid out Meson-ready |
| GTK feature flags | `v4_14` / `v1_5` | **Revised** — see below |
| "Activity monitor indicator" | Desktop panel indicator via StatusNotifierItem (`ksni`) | Not an in-window widget |
| Window close | Hides the window; app keeps running behind the indicator | Requires the background portal; **no** autostart in this iteration |
| Detail pane | Curated named rows plus a collapsed raw-inspect JSON expander | |
| Verification target | GNOME on a **separate machine** | This box has no desktop environment at all |

---

## 2. Environment findings (probed 2026-08-01)

| Fact | Value |
|---|---|
| Host | Debian 13 (trixie), kernel 6.12 |
| Rust | 1.97.1 |
| Docker | Engine 29.6.2, API 1.55, rootful `/var/run/docker.sock` |
| Socket access | user `dcminter` is in group `docker` (gid 989) — no sudo needed |
| `DOCKER_HOST` | unset |
| GTK 4 dev packages, `pkg-config` | **not installed** |
| GNOME Shell / any desktop session | **not installed** — no `/usr/share/wayland-sessions` at all |
| Debian candidates | `libgtk-4-dev` 4.18.6, `libadwaita-1-dev` 1.7.6, `gnome-shell-extension-appindicator` 59-4 |
| Current session | `XDG_SESSION_TYPE=tty` |

This machine is a build-and-test host, not a run host. Three consequences shape the
plan:

1. **A prerequisite install is needed here before the GUI crate compiles.** This needs
   your `sudo` — step 0 of the work, and I cannot do it unattended:
   ```
   sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev
   ```
   Compiling needs no display, so this is sufficient for `cargo build` and `cargo test`.
2. **On the GNOME run machine** you will additionally need the runtime libraries and,
   because GNOME has no native tray, the AppIndicator extension:
   ```
   sudo apt install gnome-shell-extension-appindicator   # then enable it, then log out/in
   ```
3. **I cannot run the manual verification checklist myself** (§8) — there is no display
   here and the run machine is a different host. I will do everything up to and
   including a clean `cargo test` / `cargo clippy` and the automated asset validators;
   the on-screen checks are yours.

### The feature-flag decision, and why it was revised

Originally set to `v4_18` / `v1_7` on the reasoning that this build host is Debian 13,
which ships exactly GTK 4.18.6 and libadwaita 1.7.6.

**That was the wrong host to reason from.** The feature flags declare the *minimum
runtime* version, so what matters is the oldest machine the application must run on —
and the run host is Ubuntu 24.04 LTS with GTK 4.14.5 and libadwaita 1.5. Building there
failed outright at `gdk4-sys`, which is the flag doing its job.

Retargeted to `v4_14` / `v1_5`, which is what `docs/gtk4_applications_in_rust.md` §1
actually prescribes for this case: *"Distribution packages (Debian, Fedora, Ubuntu LTS)
— target the oldest GTK you intend to support and accept the smaller API surface."*

The cost was nothing. No crate version changed — `gtk4` 0.11 has a GTK 4.0 baseline and
selects its minimum by feature — and **no source change was needed**, so the smaller API
surface gave up nothing this iteration uses. Building here against 4.18 headers with the
`v4_14` feature set is itself the check: the flag turns any post-4.14 call into a compile
error on this machine, before it can reach the Ubuntu box.

`libadwaita 1.5` is a hard floor, set by `adw::AboutDialog`. Supporting 1.4 would mean
swapping it for the deprecated `adw::AboutWindow`.

---

## 3. Dependencies (versions verified against crates.io, 2026-08-01)

```toml
gtk    = { package = "gtk4",       version = "0.11", features = ["v4_14"] }
adw    = { package = "libadwaita", version = "0.9",  features = ["v1_5"] }
bollard = "0.21"
ksni    = "0.3"                      # StatusNotifierItem panel indicator
ashpd   = "0.13"                     # background portal (tokio backend, not the default)
tokio   = { version = "1", features = ["rt-multi-thread", "sync", "time"] }
futures-util  = "0.3"
async-channel = "2.5"
async-trait   = "0.1"
clap    = { version = "4", features = ["derive"] }
serde       = { version = "1", features = ["derive"] }
serde_json  = "1"
thiserror   = "2"
tracing     = "0.1"
tracing-subscriber = "0.3"

[build-dependencies]
glib-build-tools = "0.22"            # GResource compilation
```

`bollard` is the library the container-daemon doc names for Rust, and it satisfies that
doc's §2 go/no-go criteria: incremental response bodies (`/events`, stats and pull
progress arrive as `Stream`s) and connection hijacking for attach/exec. Hijacking is
not used in iteration 1 but is needed from iteration 2, and changing HTTP client later
would be expensive.

`ashpd` defaults to the async-std backend; it is configured explicitly for tokio so
there is only one runtime in the process.

`rust-version = "1.92"` goes in the manifest, per the GTK doc's MSRV note.

---

## 4. Crate layout

A two-crate Cargo workspace:

```
Cargo.toml                     # workspace
crates/lave-core/              # no GTK, no D-Bus, no I/O of its own
  src/cli.rs                   # clap definitions
  src/endpoint.rs              # endpoint resolution (DOCKER_HOST, contexts, sockets)
  src/engine/mod.rs            # ContainerEngine trait + domain types
  src/engine/bollard_engine.rs # real implementation
  src/engine/fake.rs           # test double
  src/model/tree.rs            # tree structure builder
  src/model/detail.rs          # detail-page builder
  src/model/format.rs          # bytes / ages / short IDs / port maps
  src/activity.rs              # activity state machine + reconnect backoff
  src/indicator.rs             # indicator icon/tooltip/menu model (no D-Bus)
  tests/                       # fixture-driven integration tests
crates/lave/                   # the binary: widgets, D-Bus, portal, runtime
  src/main.rs  application.rs  window.rs  sidebar.rs  detail_pane.rs
  src/indicator_tray.rs        # ksni adapter over lave-core's indicator model
  src/background.rs            # ashpd portal request + lifecycle
  src/runtime.rs               # tokio thread <-> glib main context bridge
  build.rs
  data/  ui/*.ui, style.css, *.gresource.xml, *.desktop, metainfo, icons/
```

**Why a workspace rather than one crate:** it makes "all business logic must have
corresponding tests" structurally enforceable rather than aspirational — code in
`lave-core` cannot touch a widget or a bus, so it is testable by construction. It also
means `cargo test -p lave-core` runs green with no GTK development packages and no
display, which is exactly this machine's situation and keeps the inner development loop
fast.

The split is load-bearing: **`lave-core` holds every decision; `lave` holds only
wiring.** Any formatting rule, fallback, or branch that appears in the binary crate is
in the wrong crate.

---

## 5. Test strategy

| Layer | How it is tested |
|---|---|
| CLI parsing | `clap` parsed from argument-vector fixtures: defaults, valid, invalid |
| Endpoint resolution | pure function over injected `EnvSource` + `PathProbe`; no real env or filesystem touched |
| Docker response → domain conversion | JSON fixtures captured from *this* daemon with `curl --unix-socket`, deserialised via `bollard`'s types, then converted |
| Tree building | pure function, table-driven |
| Detail pages | pure function returning `DetailPage` data, asserted as data — never by inspecting widgets |
| Formatting helpers | table-driven, boundaries and `None` cases |
| Activity state machine | pure reducer `(state, signal) -> (state, effects)`; backoff sequence asserted deterministically |
| Indicator model | pure function `(ActivityState, counts) -> IndicatorModel`; icon, tooltip and menu asserted as data |
| GTK layer, `ksni` adapter, portal | not unit-tested; kept thin enough that there is nothing to test. Manual checklist, §8 |
| Live daemon | optional tests behind `--features live-docker`, skipped by default so `cargo test` never needs a running daemon |

Fixtures are captured once and committed, so the default test run is hermetic and
offline. Every phase in §7 is written test-first, per `CLAUDE.md`.

---

## 6. Architecture

### 6.1 Threading

GTK is single-threaded; `bollard`, `ksni` and `ashpd` are all async. So:

- one `tokio` multi-thread runtime on a **background thread** owns the engine, the
  `/events` subscription, the SNI tray, and the portal;
- the GTK main thread sends `Command`s over an `async_channel::Sender`;
- the runtime sends `Update`s back over another channel, consumed on the main thread in
  `glib::spawn_future_local`, where touching widgets is legal.

`ksni` menu activations arrive on a D-Bus task, **not** the GTK main thread — they are
routed back through the same `Update` channel rather than touching widgets directly.
This is the single most likely place to introduce a threading bug, so it gets exactly
one code path.

### 6.2 Data flow

```
/events stream ──┐                                    ┌─► glib main ctx ──► widgets
                 ├─► runtime thread ──► Update ───────┤
tray activations ┘        │                           └─► ksni tray (panel)
commands from UI ─────────┘
```

Per the container-daemon doc's §6 rule "subscribe to events; do not poll", the tree and
the indicator are both driven by `/events`, seeded by an initial listing. Reconnection
uses exponential backoff and replays with `since=` so events in the gap are not lost.
The backoff policy is a tested pure function, not a sleep loop scattered through the
code.

### 6.3 Startup capability probe

One probe at startup, cached, surfaced to the user rather than left to fail opaquely
later — the container-daemon doc's §8. Its results *are* the root-node content:
resolved endpoint and how it was chosen, `/_ping` API version negotiation, rootful vs
rootless, engine flavour, storage driver (with an explicit warning if `native`), cgroup
version and delegated controllers, kernel version, and image/container counts.

Version negotiation is left to `bollard`, which reads `API-Version` from `/_ping`. No
version prefix is hardcoded anywhere.

### 6.4 Endpoint resolution

Exactly the order the container-daemon doc's §3 specifies:

1. `--docker-host` CLI option
2. `DOCKER_HOST` environment variable
3. active Docker context from `~/.docker/config.json` + `~/.docker/contexts/meta/*/meta.json`
4. `$XDG_RUNTIME_DIR/docker.sock` (rootless)
5. `/var/run/docker.sock` (rootful)

The Podman socket (`$XDG_RUNTIME_DIR/podman/podman.sock`) is included as a sixth
candidate. It costs one extra probe path and the Docker-compatible surface is identical
for everything iteration 1 does. It is not advertised as supported, but the resolver is
not written in a way that excludes it.

`ssh://` and `tcp://` resolve to a clear "not supported in this version" error rather
than a confusing connection failure.

### 6.5 The panel indicator

`lave-core::indicator` produces a pure `IndicatorModel` from the activity state and the
current counts:

```
IndicatorModel { icon, tooltip, items: [ Open, Status, Images(n), Containers(n/m), —, Quit ] }
```

`crates/lave/src/indicator_tray.rs` is a thin `ksni::Tray` adapter that renders that
model and forwards activations. Icons are supplied as **embedded pixmaps** from our
GResource rather than by icon name, so the indicator renders correctly even when the
app has not been installed into the system icon theme.

Per the GTK doc's accessibility rules, state is never conveyed by colour alone: each
indicator state pairs a distinct icon shape with explicit tooltip text.

**The failure mode that matters:** on a desktop with no StatusNotifier host (GNOME
without the AppIndicator extension, which is the default), the indicator silently never
appears. Combined with hide-on-close, that would leave the app running with no window
and no way to reach it. So at startup the app queries
`org.kde.StatusNotifierWatcher.IsStatusNotifierHostRegistered`; if no host is present it
**falls back to quit-on-close** and shows an in-window explanation naming the extension
to install. This is treated as a first-class supported configuration, not an error.

### 6.6 Lifecycle and the background portal

- Closing the window hides it; the application holds itself alive with a
  `gio::ApplicationHoldGuard`.
- `ashpd`'s Background portal is requested once, with a human-readable reason. If the
  request is **denied**, the app degrades to quit-on-close rather than fighting the
  portal, and says so.
- Reopening from the indicator rebuilds the window. Application state therefore lives in
  the runtime layer, never in the window — the window is disposable and may be
  constructed more than once per process.
- Quit is a `GAction` reachable from both the indicator menu and the in-window primary
  menu.
- No autostart in this iteration.

### 6.7 Errors

`thiserror` enums per module; no `unwrap`, no `unsafe` (already `forbid`-ed in the
manifest). `expect` appears only in `main`'s bootstrap — GResource registration and
runtime construction — which is what `CLAUDE.md` permits.

Daemon-unreachable is a normal state, not a crash: the content pane shows an
`adw::StatusPage` naming the endpoint tried, how it was chosen, and a Retry button; the
indicator shows its disconnected icon. Following the container-daemon doc's §4.6,
`EPERM`-class failures are mapped to human explanations ("the socket exists but your
user cannot read it — you are not in the `docker` group") rather than surfaced raw. That
mapping lives in `lave-core` and is tested.

### 6.8 UI structure

```
adw::ApplicationWindow
└── adw::NavigationSplitView
    ├── sidebar:  adw::ToolbarView
    │              ├── adw::HeaderBar  [ ☰ primary menu ]
    │              └── gtk::ScrolledWindow ▸ gtk::ListView
    │                     gtk::TreeListModel over gio::ListStore<TreeNode>
    │                     SignalListItemFactory: TreeExpander + icon + label
    └── content:  adw::ToolbarView
                   ├── adw::HeaderBar  (title = current selection)
                   └── gtk::ScrolledWindow ▸ adw::PreferencesPage
                          groups and rows rebuilt from a DetailPage value,
                          final group = collapsed raw inspect JSON
```

`ListView` + `TreeListModel` with a factory, never one widget per row — the GTK doc's
§5 rule, and it matters here because image and container lists grow without bound.

The root node is expanded and selected at startup, so environment metadata is what you
see on launch, as the README requires.

Accessibility is built in rather than retrofitted: accessible label plus tooltip on
every icon-only button, dynamic changes announced with `update_state`, full keyboard
reachability, no hardcoded colours or font sizes.

### 6.9 Assets and identity

Application ID, `.desktop` basename and icon filename are identical
(`com.paperstack.LaveStation`), per the GTK doc's §7 rule — a mismatch here is the usual
cause of a generic icon in the dash. `.ui` files, `style.css` and icons are compiled
into a GResource by `build.rs` and loaded from the binary, never from disk.

Meson and Flatpak packaging are **deliberately deferred**. For a single-window app with
two `.ui` files, `glib-build-tools` covers the only part of the GNOME toolchain
iteration 1 needs, and pure Cargo keeps the build working with no extra system tooling.
`data/` is laid out in the shape Meson expects, so that later move is additive.

---

## 7. Work breakdown (TDD; each phase is tests-first)

| # | Phase | Tests written first | Done when |
|---|---|---|---|
| 0 | Prerequisites, workspace skeleton, manifests | — | `sudo apt install …` done; workspace builds; `cargo test` green |
| 1 | CLI (`--docker-host`, `--log-level`, `--no-indicator`) | defaults, valid, invalid | tests pass |
| 2 | Endpoint resolution + Docker context parsing | precedence, each fallback, missing files, malformed JSON, unsupported schemes | tests pass |
| 3 | `ContainerEngine` trait, domain types, fake engine | conversion over recorded fixtures | tests pass |
| 4 | `bollard` implementation of the trait | `live-docker`-gated tests against the real daemon | gated tests pass locally |
| 5 | Formatting helpers (bytes, ages, short IDs, port maps) | table-driven incl. boundaries and `None` | tests pass |
| 6 | Tree builder | ordering, labels, counts, `<none>:<none>` images, unnamed containers | tests pass |
| 7 | Detail pages, all five selection kinds, incl. raw-JSON group | expected `DetailPage` data, incl. absent fields | tests pass |
| 8 | Activity state machine + reconnect backoff | transition table, backoff sequence, event-log ring bounds | tests pass |
| 9 | Indicator model | icon/tooltip/menu per state, count pluralisation | tests pass |
| 10 | Error mapping (`EPERM` and friends → human text) | each mapped case plus the unmapped fallback | tests pass |
| 11 | GTK shell: application, window, split view, sidebar tree | — (thin layer; §8) | window opens, tree populated, root selected |
| 12 | Detail pane renderer (`DetailPage` → widgets) | — | selection updates the right pane |
| 13 | Runtime bridge + `/events` subscription | reducer covered by phase 8 | tree stays live as containers start and stop |
| 14 | `ksni` tray adapter + StatusNotifierHost detection | — | indicator appears; absent-host fallback verified |
| 15 | Background portal, hide-on-close, reopen, quit action | — | window closes to indicator; reopens; quits cleanly |
| 16 | Error and empty states in-window | mapping already tested in phase 10 | `StatusPage` shown with actionable text |
| 17 | Assets: icon, `.desktop`, metainfo, GResource, CSS | `desktop-file-validate`, `appstreamcli validate` | validators clean |
| 18 | Final verification | — | §8 complete |

Phases 1–10 are all in `lave-core`, all fully tested, and are the bulk of the thinking.
Phases 11–17 are wiring.

---

## 8. Definition of done

Automated, run by me on this machine:

- `cargo test` green across the workspace, with no daemon required
- `cargo clippy --all-targets -- -D warnings` clean, including the `pedantic` group
- `cargo fmt --check` clean
- no `unwrap`, no `unsafe`, `expect` only in `main`'s bootstrap
- `desktop-file-validate` and `appstreamcli validate` pass

Manual, run by you on the GNOME machine (I have no display here):

- window opens; root node selected; environment metadata shown
- Images and Containers nodes populate and agree with `docker images` / `docker ps -a`
- each node kind renders sensible metadata; raw JSON expander works
- starting or stopping a container elsewhere updates the tree with no manual refresh
- indicator appears in the panel and tracks connection state and activity
- closing the window leaves the app running; the indicator reopens it; Quit exits
- with the AppIndicator extension disabled, the app explains itself and quits on close
- stopping the daemon shows the error state; restarting it recovers automatically
- full keyboard navigation — Tab, Shift+Tab, arrows, Enter, Escape; nothing trapped
- a `G_MESSAGES_DEBUG=all` run produces no `Gtk-CRITICAL` or `Gtk-WARNING`

---

## 9. Explicitly out of scope for iteration 1

Any mutation (start, stop, restart, remove, prune, pull, build); logs, exec, attach,
stats; volumes and networks as tree nodes; search, filtering and sorting; preferences
and GSettings; autostart; a native GNOME Shell extension; Meson build and Flatpak
packaging; localisation; remote (`tcp://`, `ssh://`) endpoints; containerd as a direct
target.

---

## 10. Delivered

Implemented on 2026-08-01. What is in the tree, and where it diverges from the plan
above — the plan was written first and is left intact so the differences are visible.

### Verified here

- `cargo test`: **128 tests**, hermetic, no daemon required
- `cargo test --features live-docker`: **132**, the extra four against the real daemon
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`: clean
- `cargo fmt --check`: clean
- No `unwrap`, no `unsafe`; `expect` appears only in `main`'s bootstrap and in tests
- The application was run headlessly under the GDK **broadway** backend: it built its
  window, resolved `unix:///var/run/docker.sock`, listed 16 images and 5 containers, and
  produced **zero** `Gtk-CRITICAL` or `Gtk-WARNING` messages under `G_MESSAGES_DEBUG=all`
- `crates/lave-core/tests/live_render.rs` renders every detail page from real daemon
  data and asserts no field comes out blank

### Divergences from the plan, and why

| Plan said | Built instead | Why |
|---|---|---|
| Indicator icons as embedded ARGB pixmaps | Stock Adwaita themed icon names | The indicator only ever shows stock icons (play, refresh, warning), which every panel host already resolves. Pixmaps would have added a rendering path for no gain. |
| Window is disposable and rebuilt on reopen | Window is hidden and reused | Simpler, and it preserves selection and scroll position across a hide/reopen cycle. State still lives in the runtime, so the original constraint holds. |
| Endpoint resolved once at startup | Resolved on every connection attempt | A daemon that starts *after* the app now gets picked up automatically, and a resolution failure becomes a retryable status rather than a startup abort. |
| Daemon-unreachable always shows the status page | Status page only before any data arrives; afterwards a toast, with the data left on screen | Blanking a populated window on a transient blip loses the user's place for no reason. |
| `Shared size` always shown for an image | Omitted when the daemon has not computed it | Docker reports `-1` unless asked to compute it, and asking is expensive on every refresh. Better to omit the row than to print "unknown". |

### Not done, and why

- **`desktop-file-validate` and `appstreamcli` were not run.** Neither is installed here
  (`desktop-file-utils` and `appstream`), and installing them needs your `sudo`. The
  `.desktop` and metainfo files are written but **unvalidated** — worth running once on
  the GNOME machine.
- **The raw inspect expander shows `bollard`'s typed view re-serialised**, not the
  daemon's bytes verbatim. Any field `bollard` does not model is absent. Faithful raw
  output needs a request path the crate does not expose publicly.
- **The §8 manual checklist is still yours to run.** Broadway proves the app starts,
  connects and renders without criticals; it cannot tell you whether the layout reads
  well, whether the indicator appears in a real panel, or whether keyboard navigation is
  free of traps.

---

## 11. Risks

| Risk | Mitigation |
|---|---|
| No display or desktop here; verification happens on another machine | Core crate fully testable offline; manual checklist written out explicitly for you to run |
| GNOME has no native tray; indicator invisible without the extension | Host detection at startup, documented fallback to quit-on-close, install instructions in README |
| Hide-on-close plus a missing indicator could strand the app with no UI | Exactly the case the host detection above prevents; treated as a first-class configuration |
| `ksni` activations arrive off the GTK main thread | Single routing path through the `Update` channel; no widget access from D-Bus tasks |
| Background portal denied by the user or absent on the desktop | Degrade to quit-on-close and say so; never fail startup |
| `TreeListModel` + factory boilerplate is fiddly | Isolated to one module; tree *content* is decided in tested core code |
| `bollard` 0.21 API differs from documentation examples | Phase 4 is thin and behind our own trait; a swap touches one file |
| Docker 29 / API 1.55 fields differ from `bollard`'s expectations | Fixtures captured from *this* daemon, so drift surfaces as a failing test |
| libadwaita styling looks foreign off GNOME | Accepted: GNOME is the stated target |
