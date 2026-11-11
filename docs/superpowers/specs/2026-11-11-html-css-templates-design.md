# Templates in HTML and CSS

Templates can be written in HTML with a CSS stylesheet instead of JSON. They
compile to the same template tree (`template::Node`), so docks, slots, frames,
`{var}` holes, conditions, elements, themes and the motion layer
(`2026-11-11-actors-design.md`) work the same. It is a syntax, not a browser:
only what maps onto the template engine is accepted; the rest is a problem
shown in the status bar, never silently dropped.

## Where

- A theme or `settings.json` names files: `"html": ["ui/agents.html"]`,
  `"css": ["ui/theme.css"]` (relative to the config folder). Saving one
  reloads like any config file.
- Each top-level `<template name="agents.tab">…</template>` in an HTML file
  defines (or replaces) that template, exactly like `templates.agents.tab`.
- Built-in templates stay JSON; `reference/` gains an HTML rendering of each
  built-in, generated from the JSON, as a starting point to copy.

## HTML → nodes

| HTML | Template node |
|---|---|
| `<div>`, `<span>`, `<section>`, any container | box; `class`/`id` pick CSS rules |
| text (with `{name}` holes) | `text` |
| `<img src="images/x.png">` | `image` |
| `<icon name="folder-open"/>` | `icon` |
| `<button name="close" icon="x" variant="ghost" size="xsmall" tooltip="…" action="workspace::X">label</button>` | `button` |
| `<slot name="icon"/>` | `slot` |
| `<use template="pane.body" element="…"/>` | `use` |
| `<actor name="rabbit"/>` | `actor` |
| `<each list="items"> … </each>` | `each` + `item` (children after the item go in `<after>`) |
| `<separator/>`, `<menu-item action="…">` | menu entries |

Attributes for template behavior: `data-when`, `data-frame`, `data-element`,
`data-part`, `data-dock` (+ `data-accepts`, `data-zones`, `data-strip`),
`data-split`, `data-size`, `data-min`, `data-max`, `data-fixed`,
`data-handle`, `data-group`, `data-on-click` (an animation), `data-states`
is not needed: use CSS state selectors.

## CSS → styles

- Selectors: type, `.class`, `#id`, descendant and child combinators,
  `[data-when]`-free state pseudo-classes mapped to template conditions:
  `:hover`, `:active`, `:focus` (`focused`), `:checked`/`.selected`
  (`selected`), and `:is([data-x])` style conditions via
  `:state(name)` → a template `states` entry with condition `name`.
  Specificity and source order as in CSS; `!important` is a problem.
- Properties: `display: flex|none` (`none` → hidden), `flex-direction`,
  `flex-wrap`, `gap`, `align-items`, `justify-content`, `flex-grow`,
  `flex-shrink`, `width`/`height`/`min-*`/`max-*`, `padding*`, `margin*`,
  `position: absolute|relative` + `top/right/bottom/left`, `z-index`,
  `overflow: hidden`, `background`(-color, -image url), `color`, `border*`,
  `border-radius*`, `box-shadow` (mapped to the shadow sizes), `opacity`,
  `font-family/-size/-weight/-style`, `line-height`, `text-transform`,
  `text-overflow: ellipsis` (+ `white-space: nowrap`), `-webkit-line-clamp`,
  `cursor`, `transition`, `animation`, `transform: translate/scale/rotate`.
- Units: `px`, `rem`, `%` (of the parent, where gpui supports relative
  lengths), `s`/`ms`, `deg`.
- `var(--name)` reads a token (`--row-height` ↔ `$row_height`); `--name: …`
  on `:root` defines tokens; `var(--muted-foreground)` reads theme colors;
  `color-mix()` is not supported, use `$name/NN` via `rgb(from var(--x) r g b / 50%)` → problem; keep it simple: `var(--primary-15)` style tokens.
- `@keyframes` → `animations`; `@media (prefers-reduced-motion)` →
  reduce-motion rules; other at-rules are problems.

## Implementation notes

- Parse with existing crates: `html5ever` (or `tl` for speed) for HTML,
  `cssparser`/`lightningcss` for CSS. Compile in `template/html.rs` and
  `template/css.rs` into the same JSON-shaped `Source` the JSON loader makes,
  so one compiler checks everything and problems keep their paths
  (`ui/agents.html:12:5: no property "float"`).
- Tests: every row of the tables above round-trips to the JSON form; each
  unsupported feature gives a problem with file:line:col; the HTML rendering
  of every built-in template compiles back to the same tree as its JSON.

## Not in scope

JavaScript, grid, floats, tables, forms beyond `button`, the full cascade
(inheritance only for text properties), pseudo-elements.
