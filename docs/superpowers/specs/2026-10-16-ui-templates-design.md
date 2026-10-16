# UI templates (2026-10-16)

## Why

The user wants to rearrange Tern's UI (positions, order, layering, what sits inside what) from the theme or
`settings.json`, without code changes. Example: the pane title was overdrawn by the body's border, and the only fix
was in code. Scope, in the user's words: the whole UI except the text editor and the terminal content.

## Decided

- **Templates are JSON trees of boxes**, under `templates` in a theme file or `settings.json` (settings over theme over
  built-in, whole template by name). The built-in ones live in `crates/tern-app/src/templates.json`, are written to
  `reference/templates.json`, and reproduce the default look exactly.
- **Code owns behavior, templates own structure and look.** Behavior (clicks, drags, focus, scrolling, resizing state,
  actions) stays in code and attaches to named boxes (`frame`). Pieces that are behavior through and through (terminal
  grid, editor, gpui-component widgets that hold state) are `slot`s the template places.
- **Styling reuses elements.** A node takes an element's properties (`element`) plus its own (`style`), with the same
  value syntax (`$token`, `$element.prop`, theme colors). `states` switch properties by condition. A `$element.prop`
  with no value leaves the property unset (no error), so templates can point at optional element props.
- **Node kinds:** box (children), `slot`, `text` (`{var}` interpolation), `each` + `item` (lists the code provides),
  `use` (another template; the use site's placement wins).
- **Layout:** `layout` (column/row/wrap), `align`, `justify`, `fill`, `grow`, `shrink`, `position` (absolute edges),
  `z` (sibling paint order), `overlay` (gpui `deferred`), `clip`, `truncate`, `border_under` (box border painted under
  the children, so a child can sit on it), `split` (resizable panels with `size`/`min`/`max`/`fixed`).
- **Conditions:** `a`, `!a`, `a > 1` (==, !=, <, <=, >, >=), `&&`, `||` (no parentheses).
- **Templates are renderer-neutral.** A TUI front end (a client of `tern-mux` drawing in a terminal) is planned
  for later, and it must be able to read the same templates. So the format describes structure, layout and look,
  never gpui: no gpui names or gpui-only concepts in node kinds or properties, and lengths stay plain numbers that
  a renderer maps to its own unit (pixels in the GUI, cells in a TUI). Properties only a GUI can honor (radius,
  shadow, opacity, background images, floating in a separate OS window) are marked "GUI only" in
  `settings.schema.json` and the README; other renderers ignore them silently, without reporting a problem. The engine keeps compiling
  (`compile`, conditions, `{var}` text, `use`, the part/dock arrangement) apart from drawing (`render`), so a second
  renderer reuses everything but the drawing. The TUI itself is not part of this spec.
- Problems (unknown slot/element/template/property, bad condition, a template that uses itself) go to the status bar,
  like other settings problems.

## Phases

1. **Done:** engine (`template.rs`), window, workspace (+ `workspace.left`, `status_bar`), pane, pane body + title.
2. Pane tab bar and tabs: a `tab` item template (icon, badge, label, close) drawn in a box the code frames with the
   tab's click/drag/drop/menu behavior; the bar's scroll/overflow stays code. Group chips, vertical tabs, empty pane,
   new-tab and pane buttons as nodes. Needs an `icon` node (icon by name, size, color) and a `button` node that the
   code frames.
3. Workspace tab bar and welcome screen (recent list as `each`).
4. Side panels: headers (`panel_header`, `header_button`) and rows (`list_row`) as templates; the lists stay
   `uniform_list` slots that draw each row from a row template.
5. File header of the viewer; terminal padding box.
6. Dialog bodies and footers; menus as data (items, separators, submenus) from templates.
7. Screenshot tool (`shot/`): needs elements first.

## User-arranged layout (parts, docks, floating)

### Why

The user wants the UI as dynamic as possible: a template may look nothing like the default, and the template also
says what the user may do with each piece inside the app: drag it somewhere else, attach it to another area, resize
it, float it. Today only tabs move (between panes, or to a pane's edge to split it), and the side panels sit where
the template puts them.

### Decided

- **Parts.** A view that can move is a `part` node (instead of `slot`) with a name the code provides: `explorer`,
  `scratches`, `changes`, and the center's panes. A part has a `kind` (`panel`, `pane`) that docks match on. The
  template only gives each part's **default** place; where it is now comes from the user's arrangement (below).
- **Docks.** A box with `dock: "<name>"` is an area that holds parts. It declares:
  - `accepts`: part names or kinds it takes (`["panel"]`, `["explorer", "changes"]`). Omitted: nothing can be
    dropped in (its default parts still show).
  - `zones`: where a dropped part goes. `before` / `after` (next to the parts already there, along the dock's
    `split` axis), `edges` (split the part under the pointer on that side, like panes today), `stack` (join the
    part under the pointer as tabs in one area).
  - `empty`: `hide` (default: an empty dock takes no space and appears as a drop target only while a part it
    accepts is dragged) or `keep` (stays, sized by its own style).
  - A dock's children are laid out as a `split` along its axis, so parts in it resize against each other with the
    existing `size` / `min` / `max` / `fixed` (read from the part's default node, or the dock's defaults when the
    part arrives from elsewhere).
- **Dragging.** A part names the box that grabs it with `handle` (a box inside it marked `handle: true`, e.g. the
  panel header; for panes, the tab). `drag: false` (or a condition) pins a part where it is. While a part is dragged,
  every dock that accepts it shows its zones, styled by a `drop_zone` element (replaces today's hard-coded
  accent overlay). The drag chip stays `style::drag_chip`.
- **Resizing.** Any `split` edge resizes as now; `resize: false` (or a condition) on a panel locks the edge after it.
  Floating parts resize from their corners and edges (below).
- **Floating.** A part may leave every dock and float over the window: drag it out of all drop zones and release,
  or "Float" in its menu. `float: false` forbids it. A floating part is drawn in an `overlay` layer at a position
  and size the user sets (drag its handle to move, its edges to resize; `min`/`max` still apply), above the
  docks, clipped to the window. "Dock" in its menu (or dragging it back onto a zone) docks it again. Floating
  parts are styled by a `floating` element (background, border, radius, shadow).
- **Separate OS window (later phase).** "Move to New Window" pops a floating part out into its own OS window
  (`float: "window"` allows it). This needs the view moved to another gpui window, the way workspace tabs move
  between windows today; it comes after in-window floating works.
- **Panes are a dock.** The center is `{ "dock": "center", "accepts": ["pane"], "zones": ["edges", "stack"] }` and
  today's drop-zone code (`drop_overlay`, `Zone::at`, `drop_in_pane`) becomes the generic dock code. Tabs dropped on
  a pane still join it or split it; there is one drag-and-drop system for the whole app.
- **Remembered.** The user's arrangement is stored per workspace in the session (`window_state.rs`, next to the
  pane layout), never written into the template: for each dock, its parts in order with their sizes and how
  they are split or stacked; for each floating part, its rect relative to the window. Templates stay the default.
  "Reset Layout" (View menu and command palette) drops the stored arrangement.
- **Template changes.** On load, a stored part whose dock no longer exists, or no longer accepts it, goes back to
  its default place; a part the template no longer has is dropped; problems go to the status bar like other
  settings problems. A part appears exactly once: if a template places a part twice, that is a problem and the
  first one wins.
- **Locking.** `layout.locked` in `settings.json` turns off dragging, attaching, floating and resizing everywhere
  (the stored arrangement still applies). Per part, `drag`, `float` and `resize` take conditions like `when`.
- **Showing and hiding** a part (the strip buttons) keeps its place: a hidden part leaves its dock and comes back
  to the same spot.

### Calls made while writing this (Claude's, for the user to check)

- **A dock's arrangement is a tree**, like the pane layout today: a dock holds one node that is a part, a stack of
  parts, or a split (row or column) of nodes. `before`/`after` add to the dock's top split; `edges` split the node
  under the pointer on that side (so a perpendicular sub-split can appear inside a dock). The built-in
  `workspace.left` (explorer over scratches) is that dock's default tree, and its three `when` variants go away:
  hiding a part removes it from the drawn tree, and a split or stack left with one child draws as that child.
- **The center dock's tree is the pane layout.** `panes.rs`'s `Layout` becomes the center dock's tree (its leaves
  are panes) and the session keeps storing it as now. Tabs still move one by one, as today. A whole pane moves by
  dragging its tab bar's empty space (its `handle`). The built-in center accepts only `pane`, and side docks
  accept only `panel`. A template may allow mixing, since `accepts` decides.
- **`when` on a part is its shown/hidden state** (the strip buttons and View menu toggle it). Hidden parts keep
  their place in the stored tree and come back there.
- **Strip buttons follow the part.** A part's toggle button sits in the strip on the side of the dock it is in
  now (`left_strip` / `right_strip` are filled from the arrangement); a floating part's button stays where its
  last dock was.
- **A stack has an active part**, shown with a small tab row (`dock_tabs`). Hiding the active one shows the next.
  A stack of one draws as that part, with no tab row.
- **Sizes:** a part that is `fixed` stores pixels; others store their fraction of the split. A dock may set
  `part_size` (the default size for a part that arrives from elsewhere); without it, the part keeps the size from
  its own default node, and a `fixed` part going into a split on the other axis uses that dock's `part_size`, or
  takes an even share.
- **`drag`, `float`, `resize` conditions** read the same variables as the part's `when`, evaluated per part.
- **Stored per template name:** the arrangement is keyed by the name of the template that holds the docks
  (`workspace`), plus a hash of its dock names. Switching to a theme whose `workspace` template is different starts
  from that template's defaults, and switching back restores the old arrangement.

### Phases

These fit after templates phase 4 (side panels as templates), since parts need the panels drawn by templates:

A. **Done (2026-10-16).** `part`, `dock` (`accepts`, `before`/`after`, `empty`), `handle`, drop-zone element; side panels movable between
   left and right docks; stored per workspace; Reset Layout; `layout.locked`.
B. Center as a dock: move the pane drop-zone code to the dock code; `edges` and `stack` for panels too.
C. Floating parts in the window (move, resize, dock again, stored).
D. Pop out to a separate OS window.

### Left to the builder

- The exact stored format (it joins the session file's existing per-workspace lines).
- How a stacked dock area draws its tabs (reuse the pane tab bar or a small `dock_tabs` template).
- Whether gpui's `on_drag`/`drag_over` carries a generic `DraggedPart`, or panes keep `DraggedTab` and the dock
  code takes both.

## Left to the builder (templates)

- Node names for icons and buttons, how a `button` node maps onto gpui-component's `Button`.
- Whether gpui-component `Tab`/`TabBar` stay (with the template drawing their insides) or are replaced by plain boxes.
