# Iteration 4 — Another round of polish

Version 4 changes nothing about what Lave Station can do to the daemon. It changes how
much work it is to do it to several things at once, where preferences are kept, and
whether a log that is still being written is readable while it is being written.

The scope, as asked for:

1. Move state storage to **GSettings**, with the Cargo build driving the tooling. No
   migration of the old file.
2. **Bulk actions**: a checkbox in the list views and a cog above them, insensitive
   until something is checked. Selections need not survive a restart.
3. **Icons in the context menu**, with the stop icon red.
4. **Column widths survive a restart. Sort order deliberately does not.**
5. **Tail the logs** — tailing as the default view, the whole log only on request, and
   the window on to them starting at the bottom rather than the top.

Everything in [the daemon integration notes](./container_daemon_integration.md) still
holds: the API is spoken directly, never the CLI; the application never runs as root and
never invokes `sudo`.

## 1. GSettings without an installation step

The obvious objection to GSettings, and the reason
[the iteration 2 plan](./iteration_2_plan.md) avoided it, is that a schema normally has
to be installed under `/usr/share/glib-2.0/schemas` before the application will start at
all — and an uninstalled `cargo run` has installed nothing.

That is solved by compiling the schema during the build and looking in both places:

* `crates/lave/build.rs` stages `com.paperstack.LaveStation.gschema.xml` into
  `$OUT_DIR/schemas` and runs `glib-compile-schemas` over it. The compiler validates as
  it goes, so a malformed schema fails the build rather than the first run.
* The directory is baked into the binary with `cargo:rustc-env`, so nothing has to be
  set in the environment at run time.
* `prefs::lookup_schema` tries `SettingsSchemaSource::default()` first — an installed
  copy wins, which is what a packaged build wants — and falls back to the compiled
  directory otherwise.

The environment variable route (`GSETTINGS_SCHEMA_DIR`) was rejected rather than
overlooked: setting an environment variable at run time is `unsafe` in Rust 2024, and
CLAUDE.md forbids `unsafe` outright. Baking the path in at compile time needs no such
thing.

A store that cannot be found at all is not fatal. `Prefs` then holds `None`, reads
return defaults and writes are dropped with a warning. Losing a sidebar width is not
worth refusing to open the window over.

### 1.1 Where the decisions live

`lave-core` still owns the *shape* of the settings: the fields, their ranges and the
clamping. It has no GTK dependency and must not acquire one, so it knows nothing about
GSettings. `lave::prefs` is the binding, and is the only thing that names a key.

The schema's defaults and `Settings::default()` have to agree, or the first run would
differ from every run after it. That is a test, not a convention:
`an_untouched_store_reads_back_as_the_models_own_defaults`.

### 1.2 Column widths as a GVariant

Widths are keyed by table and then by column title — `a{sa{si}}`. glib maps that to a
nested `HashMap`, and the model uses `BTreeMap` so tests have a determinate order; the
conversion happens at the boundary.

Storing them by *title* rather than by position means a column that moves keeps its
width, and a column that is removed leaves a harmless orphan behind rather than
shifting every other width along by one.

### 1.3 Testing without touching the user's settings

`gio::functions::memory_settings_backend_new()` gives a store that exists only for the
test. Every test in `prefs` uses it. A test that used the default backend would write
into the developer's real dconf, which is both a poor test and rude.

### 1.4 The old file

`~/.config/lave-station/settings.json` is no longer read or written. No migration was
asked for and none is done; the file can simply be deleted.

## 2. Bulk actions

### 2.1 What a mixed selection offers

An action appears when **at least one** checked object would have been offered it on its
own, and applies only to those objects. Checking a running container and a stopped one
offers both "Start 1 Container" and "Stop 1 Container", each acting on the one it
applies to. The count is in the label so it is read before it is chosen, never after.

The alternative — offering only what applies to *everything* checked — makes a mixed
selection offer almost nothing, and the user has no way to see why.

`action::for_selection` derives all of this from `for_container`, so the bulk rules
cannot drift from the single-object ones: an action is offered in bulk exactly when the
single-object code would have offered it.

### 2.2 Flags are per object, not per selection

`Action::RemoveContainer { force }` is not a property of the action; it is a property of
the object. Removing a running container forces, removing a stopped one does not, and a
selection may hold both. So each `BulkTarget` carries its own `Action` and the offer's
own action is only a summary used to identify it. There is a test for precisely the
mistake this avoids: a stopped container must not be force-removed because a different
container in the selection happened to be running.

### 2.3 Where the ticks live

In the window, not in the widgets. The detail pane is rebuilt on every refresh, and
ticks that vanished whenever the daemon emitted an event would be unusable.

They are cleared when the page changes, and pruned when an object stops existing. The
page-change test is against the *previous selection*, not against the selection-changed
signal: a refresh re-selects the same object — the row's item is a new object even when
it names the same container — so reacting to the signal alone would clear the ticks a
few times a second on a busy host.

### 2.4 One report, not a stack of toasts

`Command::ActMany` runs the selection sequentially and reports once. Sequential rather
than concurrent because the objects are related often enough — a container and the image
it holds open — that racing them produces failures the user then has to reason about.

The wording is `action::bulk_outcome`, in core, with tests. A partial failure names what
failed rather than rounding to "done"; a total failure does not say "Removed 0 images",
which reads as success.

## 3. Icons in the menus

> **This section was wrong, and version 5 replaced what it describes.** The check below
> confirmed that an item with an icon gets a `GtkImage` child and one without does not —
> which is true, and does not mean the image is ever drawn. `GtkModelButton` hides its
> image whenever the item also has a label, so none of these icons appeared. See
> [the iteration 5 plan](./iteration_5_plan.md) §2.

`GtkPopoverMenu` does honour the `GMenu` `icon` attribute — checked with a throwaway
GTK program before any of this was written, because the documentation is not explicit
and the horizontal-buttons section hint suggests otherwise. An item with an icon gets a
`GtkImage` child; one without does not.

Colour is a different matter. A `GMenu` is a model and cannot carry a CSS class, so the
red is applied by walking what `GtkPopoverMenu` built and adding `.tone-bad` to the
images of the items that want it. Two things keep that honest:

* The buttons come out in the order the items were appended, and the count is compared
  before anything is coloured. A GTK that laid the menu out differently leaves the menu
  plain rather than painting the wrong row red.
* Which items are red is decided in core (`Offer::tone`), not here: anything that
  removes something, plus Stop and Kill, which halt something that is running.

Colour is reinforcement and never the signal — the label says what the item does, and
destructive items are in their own section besides.

The cog's menu is built when it opens rather than when the pane is rendered, so it
describes what is checked at that moment. Its popover does not exist until GTK has built
it from the model, so the tinting waits for an idle.

## 4. Widths persist, sort does not — but sort still survives a refresh

"Sorting orders should not survive restarts" is not the same as "sorting should be
forgotten constantly". A user's sort is held for the session, keyed by table id, and a
table opens on `Table::default_sort` — newest first — when the session has nothing to
say. Without the session state a re-sort would be undone by the next daemon event, which
on a busy host is a second or two later.

Column widths take the opposite route and are written to the store. Restoring one makes
GTK notify, which reports back the width just set, so `Settings::set_column_width`
returns whether anything actually changed and the store is only written when it did.

## 5. Tailing a log that is still being written

This took four attempts, and the failures are worth recording because they all look
correct.

The requirement: the viewer opens on the newest line, keeps up as lines arrive, and
stops keeping up when the user scrolls back to read something.

**Attempt 1 — scroll after each batch, to a mark created and deleted on the spot.**
Nothing moved. `gtk_text_view_scroll_to_mark` does not scroll immediately: a text view
lays its lines out lazily, so at the moment a line is inserted there is no position to
scroll to yet, and the call records a *pending* scroll to be completed once there is.
Deleting the mark straight afterwards throws that away.

**Attempt 2 — drive the scrollbar instead, from the adjustment's `changed` signal.**
The theory: `changed` fires when the layout has caught up and the content has a new
height, so that is the moment to move, and `set_value` takes effect at once. Measured,
the viewer stopped following after about three seconds. The trace says why:

```
changed        following=true  upper=2040 value=0        → set_value(1413)
value_changed  remaining=48    upper=2088 value=1413
changed        following=false upper=2088 value=1413
```

Validating lines *during* `set_value` grew the height again, so the position just set was
already 48 pixels short of the bottom — and the rule "we are following if we are at the
bottom" read that as the user having scrolled up three lines.

**Attempt 3 — guard the rule while we are the ones moving.** Better, and it followed
correctly for thousands of lines, but it still ended up stranded:

```
value_changed  jumping=false  placed=7413  value=1413  upper=8040
```

Something inside the text view moved the position back, on its own, to where it had been
at the very first jump. Whatever that is, no rule that infers intent from position can
survive it.

**What is there now.** The mark comes back — permanent, right gravity, so lines are
inserted before it — and every batch re-issues `scroll_to_mark`. That is the one call
that knows how to finish the job after the lines in between have been laid out, and
because it is re-issued per batch, a view that something else has scrolled is brought
back rather than left stranded. Whether to follow at all is taken from what the *user*
does — a wheel or touch scroll upwards, Page Up, Up or Home — and resumed by
`edge-reached` at the bottom, by whatever means they got there.

Measured, following a container writing ten lines a second: the view trails the end by a
few hundred pixels while output is arriving, because the layout is genuinely behind, and
lands exactly on the last line within a frame of the output pausing.

```
tail     lines=68  following=true  value=339  page=627  upper=1112
whole    lines=91  following=true  value=701  page=627  upper=1480
settled  lines=91  following=true  value=841  page=627  upper=1480   at_bottom=true
```

### 5.1 Tail and whole log

The tail asks for the last `engine::TAIL_LINES` (500) and follows. The whole log asks
for everything and follows. Switching between them restarts the stream and clears the
buffer: the daemon has no way to send the earlier lines of a stream already in progress.

### 5.2 One stream per viewer

Version 3 held a single log stream in the session loop, so opening a second container's
logs silently replaced the first, and closing either tab stopped whatever was running.
With one tab per object that is plainly wrong.

The streams are now a `SelectAll` of tagged streams, each wrapped in `abortable` so a
closing tab can stop its own and only its own. Each is given an explicit terminator —
once merged there is no other way to tell which stream has ended — and aborting cuts the
stream *before* its terminator, so a deliberate stop reports nothing, which is right:
there is nothing to report.

An empty `SelectAll` completes immediately with `None`, which would spin the select
loop, so the branch parks on `pending()` when nothing is open.

## 6. What was verified, and how

A temporary probe drove the new surfaces headlessly under `gtk4-broadwayd` and was
removed afterwards. It confirmed, on a real daemon:

* six checkboxes for six containers; the cog insensitive until one is ticked;
* a mixed selection offering `Kill 1`, `Stop 1`, `Pause 1`, `Restart 1`, `Start 1`,
  `Remove 2`, with Kill, Stop and Remove tinted and the rest not;
* the same for a row's own menu, with an icon on every item;
* a column width restored on the next run and applied to the right column;
* a session sort surviving a re-render;
* a bulk Start actually starting two containers through `ActMany`;
* the tail figures above.

The containers it acted on were created for the purpose and removed afterwards.
