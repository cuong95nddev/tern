# Actors: animated decorations in templates

A theme (or `settings.json`) can put small animated figures over the window: a
rabbit that sits on a tuft of grass at the bottom of the sidebar, hops across
the status bar now and then, wanders over the panes, and runs off when clicked.
They are decorations: they never take a click meant for the UI under them,
except a click on the figure itself.

## Where they are described

`actors` (in a theme or `settings.json`, merged like `templates`): named actors.

```json
"actors": {
  "rabbit": {
    "size": [40, 40],
    "looks": {
      "sit":  { "image": "images/rabbit-sit.gif" },
      "run":  { "sprite": "images/rabbit-run.png", "frames": 6, "fps": 12 },
      "hop":  { "sprite": "images/rabbit-hop.png", "frames": 8, "fps": 16 },
      "sniff": { "image": "images/rabbit-sniff.png" }
    },
    "faces": "right",
    "moves": [
      { "do": "rest", "look": "sit", "at": [0.1, 1], "for": [20, 60] },
      { "do": "wander", "look": "run", "rest_look": "sniff", "speed": 120, "pause": [2, 6], "times": [2, 5] },
      { "do": "visit", "look": "hop", "path": "across", "speed": 220 },
      { "do": "patrol", "look": "run", "from": [0, 1], "to": [1, 1], "speed": 90, "times": 2 }
    ],
    "order": "random",
    "click": { "do": "flee", "look": "run", "speed": 400, "then": "rest" }
  }
}
```

- `size`: the figure's box in px.
- `looks`: what it shows in each state. `image`: a PNG, SVG or GIF (an animated
  GIF plays by itself) relative to Tern's config folder (or absolute / `~`).
  `sprite`: a PNG sheet of `frames` equal frames side by side, played at `fps`
  (`loop`: true by default). Every look is drawn mirrored when the figure moves
  against the way it `faces` (`left` or `right`).
- `moves`: what it does, one after another (`order`: `list`, the default, or
  `random`), forever:
  - `rest`: stays at `at` for a number of seconds (`for`: a number or a
    `[min, max]` range, picked at random), showing `look`.
  - `wander`: runs to a random point of its stage, pauses (`pause`), `times`
    times.
  - `patrol`: runs from `from` to `to` and back, `times` times.
  - `visit`: leaves its place, runs `across` the stage (out one side, in the
    other) or along a `path` of points, and comes back.
  - `hop`: jumps on an arc to `to` (`height` px high).
  Points are `[x, y]` in the stage: numbers 0..1 are fractions of its size,
  larger numbers (or `"40px"`) pixels; `y` is where the figure's feet are.
  `speed` is px per second. Moving shows `look`, standing shows `rest_look`
  (or the actor's first look).
- `click`: what a click on the figure does: any move above (`flee` is
  `wander` once at `speed`, away from the pointer), then `then`: `rest` (go
  back to its place) or `continue` (go on with `moves`). A click while it
  already reacts starts the reaction again.

## Where they are drawn

A template places an actor with an `actor` leaf in a positioned box, its
**stage**: the actor moves inside the box's bounds.

```json
{ "position": { "top": 0, "right": 0, "bottom": 0, "left": 0 }, "z": 10,
  "children": [{ "actor": "rabbit" }] }
```

So the rabbit can be confined to the status bar, the bottom of the sidebar
(beside a static `image` of grass), or the whole window (a stage over the
`window` template). The `image` leaf (`{"image": "images/grass.png", "style":
{"width": 64, "height": 24}}`) draws a still or GIF picture anywhere in a
template; the same path rules as looks.

## Behavior

- Mouse: the stage takes no mouse events; the figure's own box takes a click
  (and the pointer cursor), nothing else. Hover and clicks elsewhere go through.
- Each actor's state (where it is, what move, how far along) lives in a global
  per window and stage, keyed by the template path, so it survives redraws and
  theme reloads that keep the actor; a changed actor starts over.
- Animation: a moving actor requests the next frame; one resting with a still
  look requests none (a GIF animates on its own, a sprite asks for frames at
  its `fps` only). No timers run while the window is hidden or minimized; a
  long gap (sleep) skips ahead instead of replaying the moves missed.
- `"decorations": false` in `settings.json` (`ui.decorations`) hides all actors.
- Problems (a missing file, an unknown look, a bad point) show in the status bar
  like other template problems; the rest still draws.
- Documented in the config README (`settings.rs`), `settings.schema.json` and
  `reference/`, as other template features are.

## General motion, as in CSS (built first; actors use it)

Any box of any template can move and change like an HTML element with CSS
animations and transitions.

### Keyframe animations (`@keyframes` + `animation`)

`animations` (theme or `settings.json`, merged like `templates`) names keyframe
sets; a box runs one with `animation`:

```json
"animations": {
  "pulse": { "0%": { "opacity": 1, "scale": 1 }, "50%": { "opacity": 0.6, "scale": 1.08 }, "100%": { "opacity": 1, "scale": 1 } },
  "slide-in": { "from": { "translate_x": -20, "opacity": 0 }, "to": { "translate_x": 0, "opacity": 1 } }
}
```
```json
{ "when": "working", "icon": "bot", "animation": { "name": "pulse", "duration": 1.2, "iteration": "infinite", "easing": "ease-in-out" } }
```

- `animation`: `name`, `duration` (s), `delay` (s), `iteration` (a count or
  `infinite`), `direction` (`normal`, `reverse`, `alternate`,
  `alternate-reverse`), `easing` (`linear`, `ease`, `ease-in`, `ease-out`,
  `ease-in-out`, `steps(N)`, `cubic-bezier(a,b,c,d)`), `fill` (`none`,
  `forwards`, `backwards`, `both`), `play` (a condition: plays while it holds,
  e.g. `"hover"`, else paused), or a list of these to run several.
  A string is a shorthand: `"pulse 1.2s infinite ease-in-out"`.
- It starts when the box first shows (or when its `when` starts to hold, or
  `play` starts to hold), as an element entering the DOM does.
- Animatable: `opacity`, `translate_x`, `translate_y` (px), `scale`, `rotate`
  (deg), every color property (`background`, `text`, `border`, …) and every
  length (`width`, `height`, `padding*`, `margin*`, `radius`, `gap`,
  `border_width`, …). What gpui cannot transform on a plain box (scale and
  rotate on a div) is drawn on icons, images and text by their own transforms,
  and on a box by scaling its size around its center; list in the README what
  each kind supports.

### Transitions (`transition`)

When a property's value changes because a `states` entry starts or stops to
hold (hover, active, focused, selected, any condition), or a `"{var}"` value
changes, it eases to the new value instead of jumping:

```json
{ "element": "agents.tab", "transition": "background 0.15s ease-out, opacity 0.2s",
  "states": { "hover": { "background": "$row_hover" } } }
```

`transition`: `all` or property names, `duration`, `delay`, `easing`, as a
CSS-like string or a list of objects.

### Events

`"on": { "click": "animation-name", "hover": "…", "appear": "…" }` runs a
keyframe set once on that event (like adding a class from a click handler).
Actors' `click` reactions are built on this machinery.

### Engine rules

- One clock per window; a box that animates requests frames only while it
  runs; nothing ticks when nothing moves (a test checks that an idle window
  requests no frames).
- `ui.reduce_motion: true` (or the system's Reduce Motion setting on macOS)
  jumps to end states and stops infinite animations; `ui.decorations: false`
  hides actors.
- Animation state lives per window, keyed by the box's template path and list
  index, so redraws keep it.

## Not in scope

Actors that follow the pointer, sounds, actors talking to each other, physics.
