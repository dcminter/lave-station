# Iteration 2 — Spit and Polish

Version 2 refines what Version 1 built. Nothing here changes the read-only,
socket-only posture set out in [the iteration 1 plan](./iteration_1_plan.md) or in
[the daemon integration notes](./container_daemon_integration.md).

The scope, as asked for:

1. A resizeable sidebar.
2. Colour in the state icons — green for running, red for stopped.
3. Images titled by tag, falling back to ID; sorted by tag, then untagged by age.
4. Containers titled and sorted by name.
5. Better use of horizontal space in the detail pane.
6. The relationship between containers and images, made visible and navigable.
7. A `docker ps` table leading the root node's panel — see §7.

## 1. Decisions taken

| Question | Decision | Why |
| --- | --- | --- |
| Draggable divider | `GtkPaned` | `AdwNavigationSplitView` sets its sidebar width programmatically and offers no drag handle. Only `GtkPaned` gives the user one. |
| View preference storage | `$XDG_CONFIG_HOME/lave-station/settings.json` | `GSettings` needs a compiled schema installed system-wide before the app will start, which would break `cargo run` until packaging exists. A plain file is also unit-testable, which `GSettings` is not. |
| Overview layout | `GtkColumnView` tables | The Images and Containers pages were a stack of five summary rows in a 600px clamp on a 1200px window. A sortable table is the thing that width is for. |
| Object page layout | Two-column `GtkFlowBox`, widened clamp | `AdwWrapBox` would be the natural fit but arrived in libadwaita 1.7, above this application's floor of 1.5. |
| Colour source | Adwaita named colours | `@success_color`, `@warning_color`, `@error_color` follow the user's theme and accent rather than hard-coding hex. |
| Relationship model | Shared layer prefixes | See §3. `Parent` is empty on anything BuildKit produced, so derivation has to be reconstructed. |

## 2. What an image actually is, and why it matters here

The request asked to surface the case where "a container relates to more than one
image", on the understanding that an image is a set of layered images. That was true
of Docker before 1.10 (February 2016), when every layer *was* an image with its own ID
and a `Parent` pointer. Content-addressable storage replaced it.

Today:

* A **layer** is a filesystem diff, identified by the SHA-256 of its uncompressed
  contents — its *diffID*.
* An **image** is a JSON config document listing an ordered array of diffIDs plus the
  metadata (`Env`, `Cmd`, `Entrypoint`, labels, history). The image ID is the SHA-256 of
  that config. An image *references* a layer stack; it is not made of other images.

Probed on the development host, this is what that looks like:

```
node:22-alpine           Parent: null   4 layers
pub-sub-gui-web-gui      Parent: null   9 layers
```

`web-gui` is built `FROM node:22-alpine`, and yet `Parent` is null on both. The
derivation is still perfectly visible, just represented differently:

```
web-gui's first 4 layers are EXACTLY node:22-alpine's 4 layers
```

**Derivation is a shared prefix of diffIDs, not a pointer.** That is the whole basis of
`model::relations`.

`docker history` still shows the old shape, but reports `<missing>` for every row but
the top: those are history records, not images. Under BuildKit — the default since
Docker 23 — intermediate build stages are never materialised as images at all.

So a container relates to exactly one image by ID. Three genuine "more than one"
relationships do exist, and Version 2 shows all three.

## 3. The three relationships

### 3.1 One image, many containers

Matched on `ImageID`. Shown on the image page as a **Used by** group, one navigable row
per container; and on the container page as **Others from the same image**, so the
relation is traversable from either end.

Counted from the container listing rather than from the image listing's `Containers`
field, which the daemon reports as `-1` unless explicitly asked to compute it.

### 3.2 A tag that has moved

A container records both `Image` (the reference typed at create time) and `ImageID`
(the digest it actually runs). Pull the same tag again and the tag moves to the new
image while the container keeps the old one — which is where untagged `<none>` images
come from.

The container page's **Image** group then shows two rows: the image it is running, and
"what `nginx:1.27` refers to now — this container predates it". Both navigable. When the
two agree, only one row appears.

Reference matching normalises `nginx` to `nginx:latest`, matches by digest and by bare
ID, and does not mistake the port in `registry:5000/thing` for a tag.

### 3.3 One image derived from another

`base_of` finds the local image whose *entire* layer stack is a proper prefix of this
one's, longest match winning, so `web-gui` is reported against `node:22-alpine` rather
than against `alpine`. `derived_from` is its exact dual and lists only immediate
descendants, so the relation forms a tree rather than a transitive mess. Images with
identical stacks are siblings, not ancestors, and are reported separately.

A property test asserts the two functions stay exact duals on whatever the daemon
actually holds.

**Cost.** Layer digests are not in `GET /images/json`, so this needs one inspect per
image. They are fetched eight at a time after each listing and cached by image ID, so a
refresh only inspects images not seen before, and a reconnect inspects none. Layer
stacks are immutable, so the cache never goes stale; entries for deleted images are
dropped each refresh. A failure to read layers costs the relationships for that image
and nothing else.

## 4. Naming and ordering

* An image is titled by its **alphabetically first** real tag — the daemon's own
  ordering is not dependable — and by its short ID when it has none. `<none>:<none>`
  is no longer displayed. Image rows carry **no secondary text at all**: the tag names
  the image, and where there is no tag the label is already the ID, so showing the ID
  beside either was noise.
* Images sort tagged-first alphabetically, then untagged newest-first. The ID breaks
  ties so the order is total.
* Containers sort by name, case-insensitively.

**Container names are unique per daemon.** `POST /containers/create` rejects a duplicate
with `409 Conflict`, so no ID disambiguation is needed. `Names` is an array because a
single container may have several names — a legacy of container links — but the mapping
is many-names-to-one-container, never the reverse. The only fallback case is a container
with no name at all, which falls back to its short ID.

## 5. Colour

`Tone` is decided in `lave-core` and tested there; the widget layer only maps a tone to
a CSS class. Running is green, exited and dead are red, restarting/paused/stopping are
amber, and created is neutral — a container created but never started is not a failure.
Untagged images are amber, since they are usually residue left by a tag moving.

State remains conveyed by icon *shape* and by *text* as well, so colour reinforces the
signal rather than being the signal.

### Category colours

The three standing nodes are coloured for identity rather than for state: Docker's own
blue (`#2496ED`) for the daemon, a soft violet (`#9A86D4`) for Images, a soft teal
(`#5FB6B0`) for Containers.

These are literals, which the rest of the stylesheet deliberately avoids. Docker's blue
is a brand and cannot be derived from the user's theme; and the GNOME palette offers no
pastel far enough from the green/amber/red already carrying state, which is the one
thing these must not be confused with. A test asserts no state ever returns a category
tone. All three are mid-tone, so they hold up against a light and a dark sidebar alike.

`Tone::ALL` drives the class-stripping in the widget layer, so adding a tone can no
longer leave a stale class on a recycled row.

### The panel indicator is not coloured

It cannot be, as things stand: the indicator asks for `-symbolic` icon names, and
symbolic is a contract that the icon is monochrome and painted in the panel's foreground
colour. A host honouring that will discard any colour we ask for.

Colour there would mean `StatusNotifierItem`'s `IconPixmap` — ARGB32 bitmaps sent over
D-Bus, rendered on the GTK thread at startup because GSK is main-thread-only while the
tray lives on a D-Bus task. That is a real piece of work, and it buys glanceability
rather than information, against a convention that wants panel icons monochrome.

What it does instead is `Status`: `NeedsAttention` when the activity monitor has given
up, `Active` otherwise, so the panel emphasises the item in whatever way that desktop
emphasises things. `Reconnecting` deliberately stays `Active` — the application is
already handling it, and an indicator that shouts about every dropped socket is one the
user learns to ignore.

`AttentionIconName` is set alongside it, because hosts show that *instead of* `IconName`
while attention is wanted; leaving it unset would blank the indicator at the one moment
it matters.

## 6. Test strategy

Unchanged in principle: every decision lives in `lave-core` and is tested without a
display. Version 2 added 87 tests, taking `lave-core` from 128 to 215.

The live suite (`--features live-docker`) gained
`relationships_hold_together_on_real_data`, which reconstructs derivation from the real
daemon and asserts the dual property holds. On the development host it finds:

```
pub-sub-gui-web-gui:latest <- FROM node:22-alpine (4 of 9 layers)
```

matching the manual `curl` probe exactly.

## 7. The root panel's container table

The environment page is led by a full-width table with exactly the columns `docker ps`
reports — Container ID, Image, Command, Created, Status, Ports, Names — with the state
carried as a coloured icon in the first cell. Activating a row selects that container in
the sidebar.

**Filtering.** A linked pair of toggle buttons switches between running-only and
everything, labelled with both counts (`Running (2)` / `All (5)`). "Running" means
`state.is_active()` — running, restarting or paused — which is what plain `docker ps`
shows; a container that was created but never started is not running, and appears only
under All. The default is All, matching the rest of the application, which the README
has shown stopped containers in since Version 1.

**Sorting.** Every column sorts, by the cell's key rather than by its rendered text, so
Created orders by timestamp and not by the string "3 months ago". The default is Created
descending — newest first, as `docker ps` itself lists them.

**Persistence.** The toggle and the sort order join the sidebar width in
`settings.json`. Restoring a stored sort is applied *before* the change handler is
connected, so reopening the window does not immediately write back what it just read. A
stored column title that no longer exists is ignored at render time rather than rejected
at parse time, so renaming a column cannot make a stored file invalid, and a downgrade
does not silently lose the user's choice.

**Layout.** Tables are now rendered outside the clamp — width is the point of them —
while groups clamp themselves to a readable measure.

**Height.** The detail area is a vertical `GtkPaned`: the leading table takes the upper
half, the metadata scrolls in the lower one, and the divider between them can be
dragged. Pages without a leading table hide the upper half entirely, so the paned costs
them nothing.

The table opens sized to its *running* containers, `visible_rows` clamping that to
between 3 and 20. Three so a quiet machine still gets something table-shaped rather than
a stripe; twenty so a busy host does not push the daemon metadata off the screen. Past
that the user drags for more — which is also how the stopped containers are reached when
the toggle is set to All, since the sizing counts only the running ones.

The row count is decided in `lave-core` and tested there; the widget layer only turns it
into pixels, using an estimated row height because GTK will not report the real one
before the first layout. A drag overrides the estimate, and is remembered for the rest
of the session so a refresh does not silently undo it. It is deliberately *not*
persisted to disk: each launch should size itself to what is running now.

The Containers node keeps its own table with a State column, which is a different and
more compact view of the same objects. Only the root panel uses the `docker ps` columns,
because that is what was asked for; if the duplication grates, they could converge.

## 8. Delivered

Verified here:

* `cargo fmt && cargo clippy --all-targets --all-features -- -W clippy::pedantic -D warnings` — clean.
* 215 hermetic tests in `lave-core` and 2 in `lave` pass; no daemon required.
* Both live tests pass against Docker 29.6.2.
* Headless run under `gtk4-broadwayd` + `GDK_BACKEND=broadway`: window builds, connects,
  reads all 16 images' layer stacks, and emits **zero** GTK or GLib diagnostics.

### Divergences from the plan

* **The sidebar loses adaptive collapse.** `GtkPaned` has no equivalent of
  `AdwNavigationSplitView`'s narrow-window behaviour. For a desktop developer tool this
  was judged a fair trade for a divider that can actually be dragged; an `AdwBreakpoint`
  could restore it later.
* **One header bar instead of two.** Dropping the split view meant dropping its paired
  `AdwNavigationPage` headers. A single `AdwWindowTitle` now carries the selected
  object's title *and* subtitle, which V1 computed but never displayed.
* **Table rows activate on single click.** Chosen over double-click because the tables
  are a navigation surface, not an editing one.

### Not done

* **`docker history` per image.** The layer *stack* is what derivation needs; the
  per-instruction history is a richer view that belongs with a dedicated layers panel.
  Deferred.
* **Nesting containers under images in the tree.** Cross-links were chosen instead, so
  no object appears twice. Worth revisiting in Version 3 once the cross-links have been
  used in anger.
* **Persisting anything but the sidebar width.** Window size and the selected node could
  join it; the file format already tolerates unknown fields in both directions.

## 9. Risks

* **Inspect cost on a large image collection.** A machine with several hundred images
  pays several hundred inspects on first refresh, eight at a time. On a local socket
  this is milliseconds each and happens off the main thread, but it is the one place
  Version 2 does materially more work than Version 1.
* **`GtkFlowBox` raggedness.** Two columns of unequal-height groups leave an uneven
  bottom edge. Acceptable; true masonry would need `AdwWrapBox` and a 1.7 floor.
