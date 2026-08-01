# GTK 4 Applications in Rust — Best Practices

Guidance for implementing a native GTK 4 desktop application in Rust. Written for an
implementing agent; assumes no prior context.

**Version snapshot: August 2026.** Sections 1 and 2 pin specific versions. Verify
against crates.io before starting — the ecosystem moves roughly twice a year, in step
with GNOME releases. Everything else in this document is stable guidance.

---

## 1. Versions and the dependency set

The gtk-rs ecosystem releases as a **coordinated set**. Mixing versions across crates
produces type mismatches that surface as confusing trait errors, because each crate
re-exports its own `glib`.

### Current aligned set

```toml
[dependencies]
gtk = { package = "gtk4", version = "0.11", features = ["v4_18"] }
adw = { package = "libadwaita", version = "0.9", features = ["v1_7"] }
```

| Crate | Version | Binds |
|---|---|---|
| `gtk4` | 0.11.x | GTK 4.22 |
| `libadwaita` | 0.9.x | libadwaita 1.9 |
| `glib` / `gio` | 0.22.x | pulled in transitively — do not pin separately |
| `gdk4`, `gsk4`, `pango`, `cairo-rs` | matched | re-exported by `gtk4`; prefer `gtk::gdk::…` over a direct dependency |
| `relm4` (optional) | 0.11.x | requires `gtk4` 0.11.2+, `libadwaita` 0.9.1+ |

Rules:

- **Never mix a versioned crate with a git dependency** on another crate in the set.
  It will not compile.
- **Do not add `glib` or `gio` as direct dependencies** unless you need something not
  re-exported. If you must, match the version exactly to what `gtk4` depends on.
- Rename `gtk4` → `gtk` and `libadwaita` → `adw` in `Cargo.toml`, as above. All
  ecosystem documentation and examples assume these names.
- MSRV tracks recent Rust closely (1.92+ for this set). State it in `Cargo.toml` via
  `rust-version`.

### Feature flags select the *minimum runtime GTK version*

This is the single most consequential decision in the manifest and it is frequently
got wrong.

The `gtk4` crate exposes `v4_2`, `v4_4` … `v4_18`, `v4_20` features. Enabling `v4_18`
compiles in APIs added up to GTK 4.18 **and requires GTK ≥ 4.18 at runtime**. Users on
older systems get a hard failure, not a graceful degradation.

Choose deliberately:

- **Flatpak / Snap only, or GNOME-nightly target** — target the newest, since you ship
  the runtime.
- **Distribution packages (Debian, Fedora, Ubuntu LTS)** — target the oldest GTK you
  intend to support and accept the smaller API surface.
- **Reasonable default for a new app in 2026** — GTK 4.18 / libadwaita 1.7. This
  covers current stable releases of the major distributions without giving up much.

Never enable the newest feature flag reflexively. Decide, then record the decision and
its reason in the repository.

### If using Relm4

Relm4 provides `gnome_47` … `gnome_50` features that set the correct `gtk4` and
`libadwaita` version flags together. Use these instead of setting version features
by hand — mismatched pairs (e.g. `adw/v1_9` with a GNOME 49 target) are a known
source of breakage.

---

## 2. GTK 4 vs GTK 3

Use **GTK 4**. GTK 3 is in maintenance only; `gtk3-rs` is effectively unmaintained
and its next upstream release is not expected before 2027. There is no scenario for a
new application that justifies GTK 3.

Assume **Wayland-first**. GNOME 50 dropped its X11 session. X11 still works via the
GDK backend, but do not design around X11-specific behaviour: no global window
positioning, no assumptions about being able to query absolute pointer coordinates,
no client-side window placement.

---

## 3. Use libadwaita, and decide early whether you mean it

`libadwaita` is not optional polish. It provides adaptive layout, the GNOME visual
language, dark/light style management, and most of the widgets users expect from a
modern GNOME app. Building GNOME-targeted apps on bare GTK 4 means reimplementing it
worse.

The trade-off is real and should be an explicit project decision: an Adwaita app looks
correct on GNOME and looks foreign on KDE, Cinnamon, or Xfce. If the target is
cross-desktop, either accept that or use plain GTK 4 with system styling — do not try
to make Adwaita blend in.

Key pieces:

| Need | Widget |
|---|---|
| Application window | `adw::ApplicationWindow` (not `gtk::ApplicationWindow`) |
| Adaptive multi-pane layout | `adw::NavigationSplitView`, `adw::OverlaySplitView` |
| Navigation stack | `adw::NavigationView`, `adw::NavigationPage` |
| Sidebars | `adw::Sidebar`, `adw::ViewSwitcherSidebar` (libadwaita 1.9+) |
| Preferences | `adw::PreferencesDialog`, `PreferencesPage`, `PreferencesGroup` |
| List rows | `adw::ActionRow`, `EntryRow`, `SwitchRow`, `ComboRow`, `SpinRow` |
| Dialogs | `adw::AlertDialog`, `adw::Dialog` — **not** `gtk::MessageDialog`, deprecated |
| About | `adw::AboutDialog` |
| Toasts | `adw::ToastOverlay`, `adw::Toast` — prefer over notifications for in-app feedback |
| Empty / error states | `adw::StatusPage` |
| Breakpoint-driven responsive layout | `adw::Breakpoint` on the window |

Initialise with `adw::Application` rather than `gtk::Application`; it calls
`adw::init()` for you.

### Styling

- Prefer Adwaita's named style classes (`.suggested-action`, `.destructive-action`,
  `.boxed-list`, `.title-1` … `.title-4`, `.dim-label`) over custom CSS.
- Custom CSS goes in a single `style.css` loaded from GResource, using **standard CSS
  media queries** for dark and high-contrast variants. The autoloaded `style-dark.css`,
  `style-hc.css`, and `style-hc-dark.css` files are **deprecated as of libadwaita 1.9**.
- Never hardcode colours. Use Adwaita named colours (`@window_bg_color`,
  `@accent_bg_color`, …) so the app follows the user's theme and accent colour.
- Query and follow the system colour scheme via `adw::StyleManager`. Do not implement
  your own dark mode toggle without also honouring `Default` (follow system).

---

## 4. Application structure

### Use GApplication properly

```rust
let app = adw::Application::builder()
    .application_id("org.example.MyApp")   // reverse-DNS, must match .desktop basename
    .flags(gio::ApplicationFlags::HANDLES_OPEN)  // only if you handle file opens
    .build();
```

The application ID is load-bearing. It determines:

- D-Bus single-instance behaviour (free with `GApplication` — do not roll your own)
- GSettings schema path
- The `.desktop` file name, and therefore the icon (§7)
- The Wayland `app_id`, and therefore window→launcher association
- The Flatpak app ID and AppStream component ID

Pick it once, use it everywhere, never change it after release.

Wire `startup` (one-time setup, resource registration), `activate` (show a window),
and `open` (handle files) rather than doing work in `main`.

### Actions, not signal spaghetti

Use `GAction` / `GActionMap` for anything a user can invoke:

- `app.quit`, `app.preferences`, `app.about` on the application
- `win.*` on the window
- Bind accelerators with `app.set_accels_for_action()`
- Build menus as `GMenu` in `.ui` files or Blueprint, referencing action names

This gets you keyboard shortcuts, menu items, and D-Bus activation from one
definition, and it is what the platform expects.

### Subclassing

Real applications need custom `GObject` subclasses — for the window, for list item
data, for custom widgets. Use `glib::wrapper!` plus the `subclass` module, and
`#[derive(glib::Properties)]` with `#[property(get, set)]` for properties. Use
`gtk::CompositeTemplate` to bind `.ui`/Blueprint children to struct fields.

Boilerplate is significant but mechanical. Do not avoid subclassing by threading
`Rc<RefCell<…>>` through closures — that pattern does not survive contact with a
medium-sized app.

### Reactivity: `glib::Object::bind_property`

Prefer property bindings over manual signal handlers for keeping UI in sync with
state. Bidirectional bindings with `SYNC_CREATE | BIDIRECTIONAL` remove a large class
of update bugs.

---

## 5. Defining the UI

Three options, in order of preference for a new project:

### 5a. Blueprint (recommended)

`.blp` files compiled to GtkBuilder XML at build time. Far less verbose than XML,
has a language server, IDE support in Builder / Kate / Workbench, and a porting tool
from existing `.ui` files. It is in the GNOME SDK and is the direction the platform
is moving.

Set it up as a Meson subproject via `blueprint-compiler.wrap` so contributors do not
need it installed. Compile to XML, bundle the XML into GResource, gitignore the
generated files.

### 5b. GtkBuilder XML (`.ui`)

Universally supported, works with Cambalache for visual editing. Verbose. Choose this
if the build system cannot accommodate Blueprint or the team already knows XML well.

### 5c. Relm4 (declarative, in Rust)

An Elm-inspired layer over gtk4-rs with a `view!` macro. Keeps the UI in Rust, gives
a clear message-passing architecture, and removes most subclassing boilerplate.

Choose Relm4 when the application is state-heavy and you want a prescribed
architecture. Choose Blueprint + plain gtk4-rs when you want to stay close to the
platform, use GNOME tooling, and match how the rest of the ecosystem is written.
**Do not mix approaches** within one application.

### Layout

- Compose with `gtk::Box`, `gtk::Grid`, `adw::Bin`, and `adw::Clamp` (which constrains
  content width for readability — use it for any centred content column).
- Use `halign`/`valign`/`hexpand`/`vexpand` rather than fixed sizes. Never call
  `set_size_request` to achieve layout.
- Use `adw::Breakpoint` for responsive behaviour, not manual width watching.
- `GtkConstraintLayout` exists but is rarely the right tool; reach for it only for
  genuinely non-hierarchical layouts.
- For scrolling lists, **never** build one widget per item. Use `gtk::ListView`,
  `GridView`, or `ColumnView` with `GListModel` + `SignalListItemFactory` and recycle
  item widgets. `gtk::ListBox` is acceptable only for short, fixed lists (settings
  rows). Relm4's `TypedListView` / `TypedColumnView` wrap this more ergonomically.

---

## 6. Accessibility

**There is no separate accessibility crate and none is needed.** GTK 4 has
accessibility built in and exposes it over AT-SPI automatically. The work is in using
widgets correctly, not in adding a library.

Requirements:

- **Label everything.** Every icon-only button needs a tooltip *and* an accessible
  label. In Blueprint/`.ui`, use the `accessibility` block with `label`; in code,
  `widget.update_property(&[gtk::accessible::Property::Label("…")])`.
- **Use `mnemonic_widget`** to associate `GtkLabel`s with their controls, or use
  `adw::EntryRow` etc. which handle it.
- **Set roles only when overriding the default.** Standard widgets already carry
  correct `AccessibleRole` values. Setting the wrong role is worse than setting none.
- **Announce dynamic changes.** Use `update_state` for `Busy`, `Checked`, `Expanded`,
  and `AccessibleProperty::Description` for status text that changes.
- **Never convey information by colour alone.** Pair colour with an icon or text.
- **Keyboard: everything reachable, nothing trapped.** Test the full app with Tab,
  Shift+Tab, arrows, Escape, and Enter without touching the mouse. Custom widgets need
  explicit focus handling (`set_focusable`, `set_can_focus`).
- **Respect user preferences.** As of libadwaita 1.9, most widgets honour the
  system-wide reduced-motion preference automatically — but any custom animation you
  write must check it. Support high contrast via CSS media queries. Never disable
  focus rings.
- **Do not hardcode font sizes.** Users change text scaling.

Verification is non-negotiable and must be done at least once before release:

- Run **Orca** (the GNOME screen reader) against the app and navigate it end to end.
- Use **Accerciser** to inspect the exposed AT-SPI tree and confirm names and roles.
- `GTK_DEBUG=interactive` opens the GTK Inspector, which has an accessibility tab.

---

## 7. Icons, `.desktop` files, and appearing correctly at runtime

This is the area most likely to look "broken" to users while the code is perfectly
correct.

### The rule: three names must match exactly

For the desktop to associate a running window with its launcher and icon:

```
application_id      = org.example.MyApp
desktop file        = org.example.MyApp.desktop
icon file           = org.example.MyApp.svg
```

If the window shows a generic icon in the dash or Alt-Tab, this mismatch is
almost always the cause.

### Application icon

Install into the **hicolor** theme, using the app ID as the filename:

```
$PREFIX/share/icons/hicolor/scalable/apps/org.example.MyApp.svg
$PREFIX/share/icons/hicolor/symbolic/apps/org.example.MyApp-symbolic.svg
```

- Scalable SVG is the primary asset. Ship a symbolic variant too.
- Run `gtk-update-icon-cache` on install (Meson's `gnome.post_install()` does this).
- Do **not** call `gtk::Window::set_icon_name` as a substitute for installing the
  icon properly; on Wayland the compositor resolves the icon from the desktop file
  via `app_id`.

### `.desktop` file

Minimum viable entry:

```ini
[Desktop Entry]
Name=My App
Exec=my-app %U
Icon=org.example.MyApp
Terminal=false
Type=Application
Categories=GTK;Utility;
StartupNotify=true
# X11 fallback only — Wayland uses app_id
StartupWMClass=org.example.MyApp
```

Validate with `desktop-file-validate`.

### In-app icons

- Use **symbolic icons from the Adwaita icon theme** by name
  (`document-open-symbolic`, etc.). Do not bundle copies of stock icons.
- For app-specific icons, ship symbolic SVGs in your GResource and register the
  resource path with the default `IconTheme`. Follow the GNOME symbolic icon
  conventions (16px grid, single colour, `-symbolic` suffix) or they will not
  recolour with the theme.
- `relm4-icons` packages a large symbolic icon set if you need more than Adwaita ships.
- GTK 4.22 introduces **GtkSvg**, an in-process SVG renderer supporting animation.
  It is intended for *trusted* assets — your own resources and system icons.
- **For user-supplied or third-party images, use Glycin**, which decodes in a sandbox.
  Never feed untrusted SVG to the in-process renderer.

### AppStream metainfo

Ship `org.example.MyApp.metainfo.xml`. Required for Flathub and for the app to appear
properly in GNOME Software. Validate with `appstreamcli validate`. Include screenshots,
a summary, `<content_rating>`, and release entries.

---

## 8. Desktop integration

| Need | Use |
|---|---|
| Settings persistence | **GSettings** (`gio::Settings`) with a schema installed under the app ID. Not a config file. Bind directly to widget properties with `settings.bind()`. |
| Notifications | `gio::Notification` + `app.send_notification()`. Do not talk to D-Bus directly. |
| File chooser | `gtk::FileDialog` (async, portal-aware). **Not** the deprecated `FileChooserDialog`. |
| Sandboxed capabilities | **`ashpd`** (currently 0.13.x) — XDG Desktop Portals in Rust: screenshot, camera, location, background, autostart, secret, file transfer, inhibit. |
| Arbitrary D-Bus | **`zbus`** for services with no portal. Prefer the portal where one exists. |
| Passwords / tokens | Secret portal via `ashpd`, or `oo7` for libsecret-style access. Never a plaintext file. |
| Colour scheme | `adw::StyleManager` |
| Global shortcuts, inhibit sleep, autostart | Portals via `ashpd` — not desktop-specific APIs |
| Search provider integration | Implement `org.gnome.Shell.SearchProvider2` via `zbus`, plus a search-provider `.ini` file |
| Localisation | `gettext-rs` + `gettext` in Meson; mark Blueprint strings with `_()` and pass the extra `xgettext` flags Blueprint requires, or strings will be silently missed |
| System tray | Not supported by GNOME. Do not design around it. Use notifications and the background portal instead. |

**Write for the sandbox even if not shipping Flatpak initially.** Assuming
unrestricted filesystem access, then retrofitting portals, is a substantial rewrite.
Going the other way costs nothing.

---

## 9. Threading and async

GTK is **not thread-safe**. All widget access must happen on the main thread. GTK
types are `!Send` and `!Sync`, and the compiler enforces this — do not attempt to work
around it.

Correct patterns:

- **Short async work**: `glib::spawn_future_local` — runs on the GLib main context,
  can touch widgets, does not require `Send`.
- **CPU-bound or blocking work**: move it to a thread or a `tokio`/`async-std`
  runtime, then send results back with `async_channel` (or `glib::MainContext::channel`
  in older code) and update widgets in the receiving task on the main thread.
- **glib async I/O**: `gio` file, socket, and subprocess APIs have async variants that
  integrate with the main loop natively — prefer these over `std::fs` on the main
  thread.
- Relm4 has `Command` / worker components for exactly this; use them rather than
  hand-rolling channels.

Never block the main thread. A blocked main loop freezes rendering and input, and on
Wayland the compositor will mark the window unresponsive.

---

## 10. Build and packaging

**Use Meson** as the top-level build system, invoking Cargo for the Rust build. This
is unusual for Rust projects but it is what the GNOME toolchain expects, and it is
what gets you GResource compilation, Blueprint compilation, gettext, GSettings schema
compilation and validation, icon cache updates, and `.desktop`/metainfo installation
without hand-writing any of it.

A pure-Cargo build is viable for a small app, but you will end up reimplementing the
above in `build.rs`.

Ship as **Flatpak** for primary distribution. It solves the runtime version problem
that §1's feature flags create, and Flathub is where GNOME users look.

Suggested layout:

```
data/
  org.example.MyApp.desktop.in
  org.example.MyApp.metainfo.xml.in
  org.example.MyApp.gschema.xml
  icons/hicolor/scalable/apps/org.example.MyApp.svg
  ui/*.blp
  org.example.MyApp.gresource.xml
po/
src/
  main.rs
  application.rs
  window.rs
build-aux/org.example.MyApp.json      # Flatpak manifest
meson.build
Cargo.toml
```

Bundle UI files, CSS, and icons into a **GResource** compiled into the binary. Do not
load them from the filesystem at runtime.

---

## 11. Debugging

| Tool | Purpose |
|---|---|
| `GTK_DEBUG=interactive` | GTK Inspector — live widget tree, CSS, accessibility, layout borders |
| `GTK_DEBUG=builder` | Diagnostics for `.ui` files; catches deprecated patterns (all standard widgets as of GNOME 50) |
| `G_MESSAGES_DEBUG=all` | GLib logging |
| `GTK_DEBUG=layout` / `=size-request` | Layout troubleshooting |
| `GSK_RENDERER=cairo` | Isolate GPU/Vulkan rendering issues |
| Accerciser | Inspect the AT-SPI tree |
| Orca | Actually hear what a screen reader user gets |

Treat `Gtk-CRITICAL` and `Gtk-WARNING` on stderr as build failures, not noise. They
almost always indicate real API misuse.

---

## 12. Common mistakes

1. Mismatched `gtk4` / `libadwaita` / `relm4` versions
2. Enabling the newest `v4_*` feature flag without deciding on a minimum runtime
3. `gtk::ApplicationWindow` instead of `adw::ApplicationWindow` in an Adwaita app
4. Using deprecated GTK 3-era widgets: `MessageDialog`, `FileChooserDialog`,
   `GtkStackSidebar`, anything named `*Combo*` from GTK 3
5. Application ID not matching the `.desktop` basename → generic icon
6. Icon installed outside `hicolor/…/apps/`, or icon cache not updated
7. Building a widget per row instead of using `ListView` with a factory
8. Blocking the main thread on I/O or computation
9. Hardcoded colours and font sizes instead of theme variables
10. Icon-only buttons with no accessible label
11. Config in a hand-rolled file instead of GSettings
12. Assuming direct filesystem access instead of using portals
13. Loading UI and CSS from disk instead of GResource
14. `Rc<RefCell<…>>` closure webs instead of GObject subclasses and properties

---

## Quick reference: rules

1. GTK 4 only; assume Wayland
2. Keep `gtk4` / `libadwaita` / `relm4` versions aligned as a set
3. Choose the `v4_*` feature flag deliberately and document why
4. Use libadwaita widgets, not their GTK 3-era equivalents
5. Application ID = `.desktop` basename = icon filename, exactly
6. Install icons into `hicolor`, ship SVG plus symbolic
7. Define UI in Blueprint; bundle everything in GResource
8. Use `GAction` for every user-invocable operation
9. Use `ListView`/`ColumnView` with factories for any list that can grow
10. Label every control; verify with Orca and Accerciser before release
11. Never block or touch widgets off the main thread
12. GSettings for settings, portals for capabilities, GResource for assets
13. Build with Meson, ship as Flatpak
14. Treat GTK criticals as errors
