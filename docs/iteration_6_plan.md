# Iteration 6 — Small tweaks to a working application

Four adjustments, none of them new capability. Two are layout, one is navigation, and
one changes what the tables open onto.

The scope, as reported:

1. Opening a page for a container or image should leave the root page there as the first
   tab — and, once that was in, should not replace the *previous* object's page either:
   detail pages should accumulate as the log and file tabs do. With a lot of tabs open,
   a menu on them to close all of them, or those to the left or right.
2. That page should lead with buttons for the same operations the table's context menu
   offers.
3. The summary on the Containers and Images pages should be the full width of the page,
   since nothing is shown beside it.
4. The container table should open onto the running ones, and the image table onto the
   tagged ones.

Everything in [the daemon integration notes](./container_daemon_integration.md) still
holds: the API is spoken directly, never the CLI; the application never runs as root and
never invokes `sudo`.

## 1. A detail tab per object, not one that follows the selection

Since version 3 the tab bar has opened with one pinned tab, Details, showing whatever the
sidebar had selected. Selecting a container therefore *replaced* the daemon's page, and
getting it back meant selecting the root node again — losing the container's page in
turn. The first cut of this iteration gave the environment a tab of its own and left
every object sharing a second one, which fixed half of it: two containers still could not
be open at once.

So a detail tab now belongs to the object it shows, and they accumulate exactly as the
log, file and Dockerfile tabs do:

* **The environment's tab is pinned first.** It shows the root page and nothing else,
  whatever the sidebar is on. A pinned tab is drawn as its icon alone, so it carries a
  tooltip.
* **Every other node opens its own tab**, and keeps it. Selecting something already open
  brings its tab forward rather than stacking a duplicate, and the tabs are closable.
* **The sidebar and the tabs stay in step in both directions.** Selecting a node brings
  its tab forward; bringing a tab forward moves the sidebar to what it shows.

The consequence is one set of detail widgets per tab. A `GtkPaned` and its two boxes came
from the window template; that pair is now `detail_pane::Surface`, built per tab. They
cannot be shared: a widget has one parent, and every page is open at once.

Only the surface on screen is drawn. Rendering them all would mean several tables with
checkboxes alive at the same time, and the checked set, the select-all control and the cog
are one each — the page in view owns them. So the tab that is brought forward renders
itself, through `selected-page` on the tab view, and the rest wait until they are.

That also gives the ticks a clearer rule than "the sidebar selection changed": what
`imp.viewing` holds is the page actually drawn, and moving to another detail tab discards
the ticks exactly as moving between two sidebar nodes does.

Two things had to be told apart from the user choosing something, because both move the
sidebar's selection on their own and both would otherwise open tabs nobody asked for:

* `imp.following`, set while a tab that has come forward is moving the sidebar to match.
* `imp.settling`, set while a new listing is being spliced into the tree. `GtkSingleSelection`
  picks a neighbour when the row it was on goes away, and before this guard a refresh that
  removed the container being viewed opened a tab for whichever container happened to be
  next in the list.

An object that leaves the daemon takes its tab with it, since the page has nothing left to
show and its buttons nothing left to act on. That happens after the selection has been
restored, so closing those tabs can never be closing the one on screen.

Checked against the daemon, driving the window from a temporary probe — a container
started for the purpose, opened, and then removed through the application's own Remove:

```
at rest                     ["hal"]
containers node             ["hal", "Containers"]
first container             ["hal", "Containers", "lave-probe"]
second container            ["hal", "Containers", "lave-probe", "pub-sub-tui-loadgen-1"]
back to the first           unchanged, showing "lave-probe"
back at the root            unchanged, showing "hal"
first container's tab closed["hal", "Containers", "pub-sub-tui-loadgen-1"]
after removing the container the tab was showing
                            ["hal", "Containers", "pub-sub-tui-loadgen-1"]
```

`detail_pages_get_a_tab_each_and_keep_it` pins the structure: the environment's tab is
pinned and stays at index 0 as two containers' tabs appear beside it, asking for one
already open yields the same tab rather than a second, and closing one leaves the others
alone.

### 1.1 Closing a lot of them at once

Tabs that accumulate need a way of clearing them, so right-clicking one offers **Close All
Tabs to the Left**, **to the Right**, and **Close All Tabs**.

Which tabs each of those closes is arithmetic, not a widget detail, so it is
`model::tabs` in core with tests:

```rust
pub fn closing(scope: Scope, tabs: usize, subject: usize, pinned: usize) -> Vec<usize>
pub fn is_offered(scope: Scope, tabs: usize, subject: usize, pinned: usize) -> bool
```

Pinned tabs are never among them — the environment's tab is pinned precisely because
there is nothing sensible to close it to — and a tab view keeps its pinned tabs at the
front, so they are simply the first `pinned` positions. A command that would close nothing
is greyed out rather than left to do nothing when chosen, which is the user's own example:
with only the environment's tab to the left of the first closable one, *to the Left* is
insensitive.

This one is a `GMenu` in the tab view's `menu-model`, not the hand-built popover version 5
went to the trouble of writing. The objections there do not apply: these items have no
icons to be hidden and no colour to carry, and being a menu model is what lets `GAction`
carry the sensitivity. `setup-menu` fires as the menu opens, carrying the tab it was
opened on, and that is the one moment the three commands can be measured.

The tab the menu was opened on is remembered rather than read back when an item is
chosen: the menu has closed by then. It is found again by walking the bar rather than by
asking `page_position`, which requires the page to still be in the view — and a
remembered tab need not be.

Measured against the daemon with the environment's tab, a Containers tab and an Images
tab open:

```
menu on "hal"         left=false right=true  all=true
menu on "Containers"  left=false right=true  all=true
menu on "Images"      left=true  right=false all=true
```

## 2. An object's page leads with its actions

The offers were already on the page — `DetailPage::actions`, decided in `model::action`
and tested there — and the widget layer had never drawn them. They are the same offers
the row's context menu makes, so the strip is built from the same data with the same
icons and the same tinting of the destructive ones.

Which pages get one is a rule rather than a widget detail, so it is in core, with tests:

```rust
pub fn shows_action_bar(&self) -> bool {
    self.table.is_none() && !self.actions.is_empty()
}
```

Only the pages describing a single object, in other words. A listing page acts through
its table — per row from the context menu, or on the checked rows from the cog — and the
environment page's actions are the two prunes, which are whole-machine operations and
live on the primary menu where the rest of the application's are.

The strip is a `GtkFlowBox`, which wraps when the pane is too narrow to hold every button
on one line. That is the widget version 5 removed from the *groups* for allocating every
child in a line the height of the tallest; here every child is one line tall, so the
objection does not apply. A container's eight offers measured 807x86 in a 687-wide pane —
two rows of buttons, each row its own height.

## 3. A lone group takes the whole width

`layout::column_count` answers "how many columns fit this width", which is the wrong
question when there is one group to put in them: the Containers and Images pages showed a
single summary in the left half of the pane with the right half empty.

```rust
pub fn columns_for(width: i32, groups: usize) -> usize {
    column_count(width).min(groups.max(1))
}
```

Measured on the same window: the Containers page's summary at 687x369 rather than half of
that, and the environment page — five groups — still folding into two columns of 394 in a
807-wide pane.

## 4. The tables open onto the working set

`show-stopped-containers` and `show-untagged-images` both defaulted to true, on the
reasoning that hiding things by default leaves a new user wondering where they went. In
practice that means a table of five stopped containers with the one that is running lost
among them, and a list of images padded out with residue.

Both now default to false. Nothing is hidden without saying so: the toggle above each
table counts both views — "Running (1) / All (6)", "Tagged (16) / All (17)" — so what is
being left out is on screen, and switching is one click. The container default applies to
the environment page too, since that is the same preference and always has been.

Verified against the daemon with an untagged image committed for the purpose and removed
afterwards: 6 containers listed as 1 row, 17 images as 16.

A stored preference still wins over the default, which is the point of storing it. Anyone
who set either toggle before this version will not see the change until they reset it:

```
SCHEMAS=$(find target/debug/build -type d -name schemas | head -1)
gsettings --schemadir "$SCHEMAS" reset com.paperstack.LaveStation show-untagged-images
```
