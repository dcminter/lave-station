# Iteration 5 — Fixing what version 4 got wrong

Nothing here is new capability. Two of the five items are version 4 features that did not
work, one is a layout that was wrong from version 2, one is a control that was missing,
and one is text that should never have been drawn.

The scope, as reported:

1. The context menu on the container list sometimes fails to appear, and often has a
   scrollbar down its side. *Are we using the wrong components — is this a local minimum?*
2. The prune items in the application menu are permanently greyed out, with prunable
   objects present.
3. A select-all / unselect-all control beside the cog.
4. Wildly varying vertical space in the detail panels, even for one kind of object.
5. The grey text to the right of the sidebar nodes: inconsistently updated, not useful —
   delete it.
6. The Images and Containers pages should lead with their table, or else spend far less
   vertical space on the summary.
7. A running-only filter on the Containers page, and a tagged-only one on Images.
8. Containers before Images in the sidebar.

Everything in [the daemon integration notes](./container_daemon_integration.md) still
holds: the API is spoken directly, never the CLI; the application never runs as root and
never invokes `sudo`.

## 1. The menu — yes, a local minimum

Three separate faults, all downstream of one decision: building the menu from a `GMenu`
and showing it in a `GtkPopoverMenu`.

### 1.1 The icons were never drawn

Version 4 checked that `GtkPopoverMenu` honours the `icon` attribute by looking for a
`GtkImage` child, found one, and concluded the icons worked. They did not. Dumping the
built widget tree says why:

```
GtkModelButton
  GtkBox
  GtkImage 0x0 visible=false      <- an item with an icon
  GtkLabel "Start"
GtkModelButton
  GtkBox
  GtkLabel "Plain One"            <- an item without one: no image at all
```

`GtkModelButton` sets its image visible only when the item is iconic or has no text. That
is the GNOME guideline — menus name their actions in words — and it is not configurable.
So the version 4 test was measuring the wrong thing: the presence of the child says the
attribute was read, not that anything is on screen.

### 1.2 The scrollbar

`GtkPopoverMenu` wraps its items in a `GtkScrolledWindow`, for menus taller than the
screen. Both its scrollbars exist from the moment the menu is built:

```
GtkPopoverContent
  GtkScrolledWindow
    GtkViewport / GtkStack / GtkMenuSectionBox / GtkBox / GtkModelButton ...
  GtkScrollbar
  GtkScrollbar
```

Whenever the popover is constrained — near an edge, or with the surface sized against the
parent rather than the monitor — that scrolled window is what the user sees.

### 1.3 Sometimes it did not open at all

**Row activation.** The row's own click gesture takes any button, and the table has
`single-click-activate`. A right-click therefore selected and activated the row, which
navigates, which re-renders the detail pane — destroying the cell whose gesture was about
to open the menu. `compute_point` then returns `None` on an unparented widget and
`show_context_menu` returns without doing anything.

The secondary gesture now **claims its sequence**, which cancels the row's own gesture, so
the press never reaches it: no selection, no activation, no rebuild underneath the menu.
The gesture also moved from `bind`/`unbind` to `setup`, reading the row from the list item
when it fires — the pattern the checkbox column already used — so a recycled cell cannot
end up with no gesture or with two.

### 1.4 What is there now

A `GtkPopover` containing a `GtkBox` of `GtkButton`s, one per offer, each an icon beside
a label.

* **Icons appear**, because these are our widgets and nothing hides them.
* **Colour is set where the widget is made** — `icon.add_css_class(".tone-bad")` — instead
  of walking the built tree matching `GtkModelButton` by name, which was necessary only
  because the type is private to GTK. The count-check that guarded that walk goes too.
* **No scrolled window**, so no scrollbar.

Keyboard navigation was checked rather than assumed: `child_focus(Down/Up)` moves between
the buttons and reports it moved, so arrow keys work as they would in a menu.

Because the buttons carry their offer by capture, the `win.offer` and `win.bulk` actions
and the `menu_offers` / `menu_target` / `bulk_offers` fields they indexed into are gone.
The menu no longer refers to anything by position.

### 1.5 The grab, and why a row's menu must not take one

The first attempt at the above turned `autohide` back on, on the grounds that a grab gives
Escape, click-away and keyboard focus for nothing. It does — and it also **swallows** the
click that dismisses the menu instead of letting it reach what it landed on. So
right-clicking a second row spent its click closing the first row's menu, and opening the
second cost another. That is the wrong trade for a menu attached to rows in a list, where
moving from one to the next is the common case.

So a row's menu does not autohide, and the two things that made version 4's hand-rolled
dismissal misbehave are addressed rather than avoided:

* **The phase.** The gesture is on the toast overlay in the *capture* phase, which runs
  root-first, so it fires before the row that was clicked. Right-clicking a second row
  therefore closes the first menu and then opens the second, in that order, on one press.
  In the bubble phase the order would reverse and the new menu would shut the instant it
  opened. The gesture never claims its sequence, so it cannot swallow that press.
* **Telling a press on the menu from a press elsewhere.** Version 4 compared coordinates —
  `compute_bounds` of the popover within the overlay. A popover is a `GtkNative` with a
  surface of its own, so that is a comparison across surfaces, and it only has to be wrong
  once to dismiss a menu the user was choosing from. The menu is asked instead, via a
  `GtkEventControllerMotion` that says whether the pointer is over it. No coordinates.

The choice is named — `Dismissal::Watched` against `Dismissal::Grab` — because it is not a
detail: the cog's menu hangs off a button, where there is no second button to move to and
the dismissing click was aimed at nothing else, so that one keeps the grab.

`the_context_menu_behaves_as_a_context_menu` pins it. It builds both menus and asserts a
row's does not autohide and a button's does, alongside the icon visibility and the rule
before the destructive section. It lives behind a `live-gtk` feature because GTK will not
construct a widget without a display, and it fails loudly rather than skipping when there
is none:

```
cargo test -p lave --features live-gtk
```

It was checked against the regression it exists for: with `autohide` forced back on it
fails with "a row's menu must not grab, or moving to another row's menu costs two clicks".

## 2. The prunes were asking the wrong listing

`update_prune_actions` read `self.imp().snapshot` — and ran as part of applying a new
snapshot, *before* that snapshot was stored. So it always asked the **previous** listing
what was prunable, which on the first listing is nothing at all. With a quiet daemon there
is no second listing, so both items stayed greyed out for the whole session.

The fix is not to move the call. It is to pass the snapshot in, so there is no field to
read at the wrong moment and the mistake cannot come back.

Verified against the daemon: with five stopped containers and no dangling images, the
window reports `prune-containers enabled=true`, `prune-images enabled=false`.

## 3. Select-all

A three-state `GtkCheckButton` at the head of the strip above the table, lined up over the
column of row checkboxes so what it governs needs no label. Checked, mixed and unchecked
mean what they always mean; clicking it checks everything on the page, clicking again
clears it.

The states are `action::Tally` in core, with tests, because "insensitive when empty,
mixed when partial, complete when all" is a rule rather than a widget detail — including
the case where more objects are checked than the page holds, which happens for a moment
between an action landing and the listing that follows it.

What select-all covers comes from the page's own rows, not from the widgets: a
`GtkColumnView` only builds widgets for the rows scrolled into view, so asking the widgets
would silently select the visible ones.

Bringing the control into line with the ticks sets its `active` property, which emits
`toggled`, which would read as the user having clicked it — hence the `syncing` flag. The
rebuild that redraws the row checkboxes is deferred to an idle, because it destroys the
control whose signal handler asked for it.

## 4. Vertical space: the flow box was the wrong container

`GtkFlowBox` with `homogeneous` allocates **every** child the size of the largest. Measured
with four groups of 2, 12, 3 and 7 rows:

```
homogeneous=true    every child 503x705      <- including the 149-tall one
homogeneous=false   line 1 both 503x705      <- still the tallest in the line
                    line 2 both 503x430
```

So a group of two rows beside a group of fifteen was given the height of the fifteen, and
with `valign=start` it drew at its own height and left the rest blank. That is the varying
vertical space, and turning `homogeneous` off only narrows it to one line at a time.
`AdwWrapBox` is line-based in the same way, and arrived in libadwaita 1.7 besides.

What is there now is a small `GtkLayoutManager` subclass that packs each group under the
last one in whichever column is currently shortest. The arithmetic — how many columns fit,
how wide each is, which column each group goes in and where — is `model::layout` in core,
with tests; the widget does nothing but measure and allocate.

Measured on the same window, with real container data:

```
one column   (687 wide)   total 1642, each group at its own natural height
two columns (1126 wide)   total  923, groups at (0,0) (572,0) (0,387) (572,387) (0,719)
```

The fold to one column happens below `2 x 380 + 18`, as it did before.

## 5. The sidebar's grey text

Deleted from the row. It was the daemon version on the root, a count on Images and
Containers, and the state on each container.

It also had a real fault behind the user's "inconsistently updated": a `TreeNodeObject`
is updated in place so that expansion and selection survive a refresh, but a `GtkListView`
factory's `bind` only runs when a row is *bound*. Nothing re-bound the three standing
nodes, so their counts were whatever they had been when the row was first realised. The
labels beside them are constant, which is why only the counts appeared to go stale.

The field survives as `TreeNode::description`, renamed and documented as the row's
accessible label and nothing else. Removing it entirely would have taken a container's
state out of what a screen reader is told, leaving icon shape and colour to carry it
alone — and the shape is not spoken.

## 6. The listing pages lead with their tables

The Images and Containers pages opened with a five- or six-row summary group and put the
table below it. At roughly 50 pixels an `AdwActionRow`, that is 300 pixels of counting
before the first object appears — on the pages whose entire purpose is the objects.

Both now set `table_first`, as the environment page already did, so the table leads and
the summary is reference material beneath it. The summary is unchanged: it describes the
collection, not the view of it, so hiding rows with the filters below does not change what
it counts. That is a test, because a count that fell to zero when the rows were hidden
would just be reporting the filter back at the user.

## 7. Filters on both listing pages

The Containers page gets the same running-only toggle the environment page has, and the
Images page gets a tagged-only one.

`ContainerFilter` becomes `TableFilter` and carries a `FilterKind`, since a toggle is now
attached to two different questions and the widget layer has to know which preference it
drives. `running_label` becomes `narrow_label` for the same reason.

The container toggle reads and writes the **same** preference as the environment page's.
It is one question — "do I care about stopped containers right now" — and two answers
would mean two toggles that could sit there disagreeing about what is showing. There is a
test that the two pages report the same kind and the same state.

Untagged images are a new preference, `show-untagged-images`, defaulting to true: hiding
things by default would leave someone wondering where they went. Both are stored, as the
running-only choice already was.

The counts in the labels are of the whole collection, not of the filtered view, so
"Tagged (16) / All (17)" says what each button would show rather than what is showing.

Checked against the daemon, with an untagged image committed for the purpose and removed
afterwards: Images went 17 rows to 16 and back, Containers 5 to 0 and back, and narrowing
on the Containers page left the environment page's toggle reading the same way.

## 8. Containers before Images in the sidebar

`tree::build` lists them in the new order, and that is the only place it is decided. The
widget layer used to reach into `children[0]` and `children[1]` by position in four
places, which is exactly the coupling that makes a reorder risky, so:

* `TreeNode::child(&NodeId)` looks a standing node up by identity;
* the sidebar's list store is filled by walking `children` in order;
* `apply_snapshot` matches each node object to its node by key rather than by index;
* the tests do the same, and one test — `containers_are_listed_before_images` — asserts
  the order itself.

Nothing outside `tree` now knows which comes first.
