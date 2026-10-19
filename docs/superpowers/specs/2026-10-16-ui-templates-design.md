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
- **Node kinds:** box (children), `slot`, `text` (`{var}` interpolation), `each` + `item` (lists the code provides;
  `children` come after the entries, an entry sees `index`), `use` (another template; the use site's placement
  wins), `icon` (a bundled Lucide name, or `{var}`), `button` (named like a `frame`: the code attaches what it does;
  takes `icon`, `label`, `tooltip`, `variant`, `size`, `selected`, `disabled`, `loading`).
- **Values in styles:** a style value `"{name}"` takes a value the code gives (a tab group's color). A state's
  properties may name an `element` whose properties they take (`{"active": {"element": "pane_tab_bar.tab_active"}}`).
  A state key with a `hover` term holds under the pointer; `hover:<group>` while the box named `group` is.
- **All new UI goes through templates.** Any UI added or reworked from now on is a template (plus `frame`/`slot`
  behavior in code), never a hand-built gpui tree (decided 2026-10-18).
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
2. **Done (2026-10-18):** `pane_tab_bar` (+ `.item`, `.tab`, `.chip`, `.end`), `vertical_tabs` (+ `.tab`, `.group`),
   `pane.buttons`, `pane.new_tab`, `pane.empty`. gpui-component's `TabBar`/`Tab` are gone from the pane: tabs are
   plain boxes (the `tabs` frame scrolls them, `more` lists them), styled by `pane_tab_bar.tab` /
   `pane_tab_bar.tab_active`, whose defaults reproduce the old look (pixel-compared on Windows, scroll, wrap and
   vertical layouts). The tab's icon is still a code `slot` (file and agent icons carry their own colors).
   Differences: an empty pane's tab bar keeps its 32px height (it used to shrink); the Maximize/Restore Pane icons
   (`maximize-2`, `minimize-2`) drew nothing, as before: they weren't among the bundled icons, and the check
   read gpui-kit's list of Lucide names, most of which aren't bundled. Fixed 2026-10-19: they are bundled, and
   an `icon` is checked against the app's assets, so a name Tern doesn't bundle is a problem.
3. **Done (2026-10-19):** `workspace_tabs` (+ `.tab`) and `welcome` (+ `.recent`). gpui-component's
   `TabBar`/`Tab` are gone from the app except the screenshot tool's pages. Workspace tabs share the pane tabs'
   element properties (`height`, `text`, `hover_text`, `border`, ...); the defaults reproduce the old look
   (pixel-compared on Windows: tabs, overflow, welcome with and without a background image, hover). Any template
   box takes `background_image*` in its `style` (the welcome screen's image fills the window while
   `welcome.width` sizes its column). Buttons take `loading`. One difference: the bar scrolls the active
   workspace into view, as pane tab bars do (it stayed put before).
   The Settings window (2026-10-19, `settings_window.rs`) was built on templates from the start: `settings`
   (side list + page), `settings.nav_item`, `settings.row` (a setting with a switch, a stepper, a button or a
   value, picked by values the code gives) and `settings.remote` (Connections' link and QR code, a `qr` slot).
4. **Done (2026-10-19):** side panel headers' buttons (`explorer.header.buttons`, `changes.header.buttons`,
   `scratches.header.buttons`, in `panel_header`'s `buttons` slot) and rows (`explorer.row`, `changes.repo`,
   `changes.file`, `scratches.row`). The lists stay `uniform_list`s that draw each row from its template;
   `style::header_button`, `list_row`, `chevron` and `chevron_space` are gone. The file icon is still a code
   `slot` (it carries its own color), and so is the explorer's name field (`field`). New node property
   `nowrap`: one line never cut, at least as wide as its text even when it grows (the explorer's names, so its
   list still scrolls sideways). Pixel-compared on Windows with the old build: rows at several depths,
   selection, cursor, cut, rename and new-entry fields, hover, a collapsed repo, the header buttons and the
   scratches menu. The panels' empty states (`changes.empty`, `scratches.empty`) are still code.
5. **Done (2026-10-19):** `file_header` (the viewer's path, unsaved dot, and Preview / Diff / VS Code buttons:
   their labels are in the template, picked by `when` on `preview`, `diff`, `markdown`, `image`) and `terminal`
   (the box around the grid: `terminal`'s padding and background image; its background is a value the code
   gives, the palette's with `background_opacity`; the grid is the `grid` slot). The VS Code button now takes
   `file_header.button` too. Pixel-identical on Windows with the old build (code, diff, Markdown, preview,
   image, unsaved, terminal).
6. **Done (2026-10-19):** dialog bodies and footers; menus as data (items, separators, submenus) from templates.
   Calls made:
   - **Dialogs.** gpui-component's `Dialog` stays the frame (overlay, title, close button, Escape, focus): that
     is behavior. Its body is template `dialog.<name>` and its footer `dialog.<name>.footer`; the title stays a
     string in code. Built-in: `dialog.trash`, `dialog.restart`, `dialog.delete_scratch`, `dialog.diff_preview`,
     `dialog.name`, `dialog.claude_args`, `dialog.theme`. Elements `dialog.note` (the small muted line) and
     `dialog.footer` (the buttons' row). New node property `wrap`: text wraps at the box's width even where a box
     around it keeps it on one line (a dialog's message).
   - **Menus.** A menu is a template `menu.<name>` whose root's children are its entries; the code passes the
     popup in (where it is anchored, whether it scrolls, the focus its actions go to stay code) and gets it back
     filled. Entries: `{"separator": true}`; `{"action": "file_tree::NewFile", "label": ...}` dispatches that
     action and shows its key (an unknown action is a problem); `{"button": "name", "label": ...}` is an item the
     code attaches what it does to by name; both take `icon`, `selected` (checked) and `disabled`; a button with
     `children` draws them as the item (a color dot before the label); `{"text": ...}` is a label that can't be
     clicked; a box with a `label` and `children` is a submenu; `each` + `item` gives an entry per list entry;
     `use` puts another menu's entries in place (`menu.tab_layout` in several menus). `when` hides an entry; the
     menu drops separators left at the ends or doubled. The entries are worked out apart from drawing (a plain
     list a TUI could draw too). A `button` with `action` anywhere else dispatches the action when clicked.
   - Built-in: `menu.explorer`, `menu.scratch`, `menu.new_scratch`, `menu.workspace_tab`, `menu.new_tab`,
     `menu.pane_layout`, `menu.tab_layout`, `menu.tab`, `menu.tab_group`, `menu.tab_bar`, `menu.tabs`. The
     application menu bar (`menus.rs`, native on macOS) stays code for now: its items must all be actions, some
     carry data (Open Recent), and macOS adds its own (Services).
   - Dialog bodies take element `dialog.body` (`gap` 16, as gpui-component's dialog content puts between its
     children). Action names are checked after compiling (`Templates::check_actions`, against the registered
     actions); `reference/actions.md` lists them. Pixel-identical on Windows with the old build: every menu
     (explorer, scratch row, workspace tab, new tab with its submenus, pane layout, tab, tab group with color dots
     and check, tab bar, the overflow list) and every dialog (trash, restart, delete scratch, diff preview, name,
     Claude arguments, theme); action items and button items do what they did.
7. **Done (2026-10-19):** the screenshot tool (`shot/`).
   - `shot_overlay` (the frozen screen and its canvas are the `picture` and `scene` slots; the hint, the region's
     size, `shot_overlay.toolbar`, `shot_overlay.style_bar` and the text box), `shot_editor` (+ `.toolbar`,
     `.tabs`, `.tab`; the page canvas is the `scene` slot, the text field `input`), `shot_tray` (+ `.shot`; the
     prompt is the `prompt` slot, a thumbnail `thumb`). The stroke sizes and colors are `shot.size` and
     `shot.color`, shared by the overlay and the editor. New elements (`shot_overlay.*`, `shot.*`,
     `shot_editor.*`, `shot_tray.*`) whose defaults are the old hard-coded look.
   - gpui-component's `TabBar`/`Tab` are gone from the app: the editor's tabs are boxes styled like the pane tabs.
   - The tool's own icons are named like bundled ones (`shot-rect`, `shot-undo`, ...) and served as assets on
     every platform (`shot_icons.rs`), so the built-in templates check on Linux too.
   - New: a `position` edge takes a number the code gives (`"top": "{bar_top}"`): the overlay's bars and text
     box sit where the region leaves room.
   - Pixel-identical on Windows with the previous build (through `shot-fake`, `@shot`, `tray-add`, `editor`):
     the overlay before and after a region is picked, with the rect, text and mosaic tools and drawn shapes; the
     tray; the editor with one and two tabs, hovered tabs and tools. Differences: the editor's tab bar shows its
     New Page button (gpui-component's `TabBar` never drew it, having no suffix), and keeps its height with no
     tab open; a tray thumbnail of a tall capture is contained and centered (it filled the box, cut off).
8. **Done (2026-10-19):** what the phases above didn't name.
   - `viewer` (the file header, then the body: the `diff`, `image`, `preview` or `editor` slot, or `{message}`),
     `diff_pane` (`diff` slot or `{message}`), `scratch` (a save error over the `editor` slot).
   - The side panels' frames: `explorer`, `changes`, `scratches` (`header` and `list` slots; the panel's
     element, background image included, is the root's). Their empty states `changes.empty` and
     `scratches.empty` are templates now.
   - `tool_strip.left` / `tool_strip.right` (both `use` `tool_strip`): list `parts` with `icon`, `on`, `tooltip`;
     the `part` button toggles its panel.
   - `agent_monitor` (+ `.workspace`, `.agent`), with elements `agent_monitor`, `.header`, `.workspace`,
     `.agent`, `.status`, `.empty` whose defaults are the old hard-coded look. The pulsing dot is a value the
     code gives (`dot_color`); the agent's logo stays a slot (it carries its own colors); the canvas that fits
     the window to the panel is the `fit` slot.
   - Drag ghosts: `drag_chip` (`label`, `icon`) for tabs, files, panels and workspaces; `drag_group` for a tab
     group's chip. `style::drag_chip` now renders the template; `group_pill` is gone.
   - Pixel-identical on Windows with the previous build: layout, strips and their tooltip, viewer (code, diff,
     Markdown, image), scratch, empty panels, agent monitor with no agents, and the file, tab and group drag
     ghosts. Not compared: the monitor's agent rows (no agent was running) and the Markdown preview body.

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
- **Separate OS window (later phase; built in phase D, with `window` on the part in place of
  `float: "window"`).** "Move to New Window" pops a floating part out into its own OS window
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
B. **Done (2026-10-19).** Center as a dock: move the pane drop-zone code to the dock code; `edges` and `stack` for
   panels too. As built (plan `2026-10-19-layout-phase-b.md`):
   - `Edge`, `Zone` and `Zone::at` live in `arrange.rs`; `Zone::pick` keeps a zone to what the area takes. The
     overlay is `dock::zone_overlay` + `dock::on_zones` (generic over the drag payload), with the hint under the
     pointer in one global (`dock::DropHint`). Panes and docks use the same code.
   - `edges` splits the area showing the target part (across the dock's axis too, as a sub-split); `stack` makes a
     `Tree::Stack` (stored `[s a *b]`, `*` the shown part). A stack draws with `dock_tabs` (+ `.tab`, frame `tab`):
     a tab shows its part and drags it. A part dropped on a stacked part's edge splits around the whole stack;
     `before`/`after` a stacked part goes next to its stack. With `edges` or `stack`, a dock's areas show the zone
     under the pointer; else the `before`/`after` halves. The built-in side docks take `edges` and `stack`.
   - The dock named `center` is the pane dock (`{ "dock": "center", "accepts": ["pane"], "zones": ["edges",
     "stack"] }`): it draws the code's `center` slot and is left out of the arrangement (stored arrangements keep
     their key). Its `zones` decide whether a tab may split (`edges`) or join (`stack`) a pane. With `accepts`
     `pane`, a whole pane drags by its tab bar's empty space (a `DraggedPart` of kind `pane`): onto a pane its tabs
     join it, on a side it moves there. `layout.locked` stops pane drags; tab drags stay unlocked.
     Not done: mixing panels into the center or panes into side docks (the center's tree holds only panes).
   - Control commands for checks: `part-move PART DOCK stack|left|right|up|down TARGET`, `pane-move FROM TO ZONE`.
C. **Done (2026-10-19).** Floating parts in the window. As built (plan `2026-10-19-layout-phase-c.md`):
   - A part let go outside every drop zone floats where it was let go (its top-left where its handle was
     grabbed); the right-click menu of its handle (`menu.part`, buttons `float` / `dock`) floats it under the
     pointer or docks it again. Dropped on a zone, it docks there. Let go on its own place (alone in its area),
     nothing happens, so a short drag of a header doesn't float it.
   - Floating parts are drawn over the root of the template that holds the docks (no new node property), each
     in a `floating` box (element `floating`: look, `width` / `height` to start with, `min_width` /
     `min_height`, `grip`). It moves by its handle (it follows the pointer, with no drag chip, and lets the zones
     under it take the drop) and resizes from its sides and corners; still, it keeps clicks from what is under it.
   - Stored after the docks as `f:<part>=<dock>@<x>,<y>,<w>,<h>`: the dock it came from (its strip button stays
     there, Dock sends it back there, or to its default dock when that one is gone), its top-left as fractions of
     the area (a smaller window keeps it in view), its size in pixels. Hiding it (strip button) keeps it floating.
   - `float: false` (or a condition) on a part keeps it in the docks; `layout.locked` stops floating, moving and
     resizing (floating parts stay where they are).
   - Control commands for checks: `part-float PART X Y W H`, `part-dock PART`.
D. **Done (2026-10-19).** Parts in OS windows of their own. As built (plan `2026-10-19-layout-phase-d.md`):
   - "Move to New Window" in a part's handle menu (`menu.part`, button `window`) opens it in a normal OS window
     (system title bar, `Explorer — <folder>`), drawn with template `part_window` (element `part_window`, the part
     in slot `part`) and the workspace's values. In that window the part's menu has Float (back over the main
     window's workspace, centered) and Dock; its handle doesn't drag (gpui drags stay in their window). Closing the
     window docks the part again.
   - Letting a part's handle go outside the main window (a drag out of it) does the same, the window's top-left
     near where it was let go (added 2026-10-19; the mouse is captured during a drag, so the main window hears
     the release).
   - `window: false` (or a condition) on a part keeps it in the main window, in place of the spec's
     `float: "window"` (which would read `"window"` as a condition, and hide the feature from the built-in
     templates). Default `true`. GUI only.
   - Stored among the floating parts as `p:<part>=<dock>@<display>@<x>,<y>,<w>,<h>`: the display's uuid (`-`
     unknown) and the window's content bounds there, as `window_state` keeps main windows. A display that is gone,
     or bounds off it, open it centered on the main display. The window's moves and resizes are kept 400 ms after
     they stop. Its strip button stays with the dock it came from.
   - The workspace opens and closes the windows to match its arrangement as it draws: hiding the part (strip button,
     its key) closes the window and showing it opens it at the same place; the windows come back with the session
     (also for a workspace whose tab isn't shown). Closing the workspace or its main window closes them and keeps
     them stored. Part windows (like the agent monitor) don't keep Tern running.
   - Keys the workspace handles (toggle panels, new tabs, ...) work from its main window only; the part's own keys
     work in its window.
   - Control commands for checks: `part-window PART X Y W H` (pixels on the main window's display; a part already
     in a window is moved by its title bar only), `part-dock PART`.
   - Checked on Windows: pop out by command and by menu (explorer, changes), move the window and restart (same
     place), hide and show, Float and Dock from its menu, close docks it (kept after restart); the default look is
     pixel-identical with the previous build.

### Left to the builder

- The exact stored format (it joins the session file's existing per-workspace lines).
- How a stacked dock area draws its tabs (reuse the pane tab bar or a small `dock_tabs` template).
- Whether gpui's `on_drag`/`drag_over` carries a generic `DraggedPart`, or panes keep `DraggedTab` and the dock
  code takes both.

## Left to the builder (templates)

- Node names for icons and buttons, how a `button` node maps onto gpui-component's `Button`: **done** (`icon`,
  `button`; see Decided).
- Whether gpui-component `Tab`/`TabBar` stay or are replaced by plain boxes: **replaced** in panes (phase 2) and
  the workspace tab bar (phase 3) and the screenshot editor (phase 7).
