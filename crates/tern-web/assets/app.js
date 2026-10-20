// Tern in a browser: the list of Tern's terminals, and one of them in
// xterm.js. The server sends the session's bytes as they come; this page
// only draws them and sends back what is typed. See bridge.rs for the
// messages.
"use strict";

const $ = (id) => document.getElementById(id);

const FONT = 'Menlo, Monaco, Consolas, "DejaVu Sans Mono", "Liberation Mono", monospace';
const LINE_HEIGHT = 1.15;
// The page asks for the grid that fits its screen at this font size; the
// session takes it while the page is the one used last (see bridge.rs).
// Until it does, or once the desktop takes it back, the page draws the grid
// it has at this size or smaller until it fits.
const FONT_SIZE = 13;
const MIN_FONT = 7;

let socket = null;
let retry = 0;
let theme = null;
let sessions = [];
// The session shown, and the one asked for whose snapshot hasn't come yet
// (output before it belongs to the session left).
let current = null;
let pending = null;
let size = { cols: 80, rows: 24 };
// A screen this wide shows the agents beside the terminal.
const wideScreen = matchMedia("(min-width: 900px)");
// The view asked for: "list-view", "term-view" or "message-view".
let view = "list-view";
let term = null;
let snapshotNext = false;
// This device isn't paired (any more): don't reconnect.
let signedOut = false;

const PAIR_HINT = "On the computer running Tern, open Settings ▸ Connections, choose Create Link, and open that link here.";

// ---- The connection ---------------------------------------------------------

async function start() {
  const m = location.hash.match(/[#&]p=([0-9A-Za-z]+)/);
  const old = /[#&]t=/.test(location.hash);
  if (m || old) history.replaceState(null, "", location.pathname + location.search);
  // A pairing link: trade its code for this device's cookie (it works once).
  const r = m && (await fetch("/pair", { method: "POST", body: m[1] }).catch(() => null));
  // A device paired already gets in whatever the link says.
  const s = await fetch("/session").then((r) => r.json()).catch(() => false);
  if (!s && m && (!r || !r.ok)) {
    const why = r && r.status === 429 ? "Too many wrong pairing links: wait a few minutes." : "This pairing link was used already or has expired.";
    return signOut(`${why} ${PAIR_HINT}`);
  }
  if (!s) return signOut(old ? `Links like this one don't work any more: devices now pair once. ${PAIR_HINT}` : `This device isn't paired with Tern. ${PAIR_HINT}`);
  // Ids are strings: they don't fit a JavaScript number.
  connect(new URLSearchParams(location.search).get("s"));
}

function signOut(text) {
  signedOut = true;
  showMessage(text);
}

function connect(view) {
  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  socket = new WebSocket(`${proto}//${location.host}/ws`);
  socket.binaryType = "arraybuffer";
  socket.onopen = () => {
    retry = 0;
    $("conn").className = "dot up";
    const id = view ?? current;
    if (id != null) openSession(id);
  };
  socket.onmessage = (e) => (typeof e.data === "string" ? onText(JSON.parse(e.data)) : onBytes(e.data));
  socket.onclose = async () => {
    $("conn").className = "dot down";
    socket = null;
    if (signedOut) return;
    // Refused, or the server is down: ask which (it's back if it answers).
    if (retry >= 2 && (await fetch("/session").then((r) => r.json()).catch(() => true)) === false) {
      return signOut(`This device's access to Tern was revoked. ${PAIR_HINT}`);
    }
    // Back off to 10 s; at once when the page comes back to the front.
    const wait = Math.min(10000, 500 * 2 ** retry++);
    setTimeout(() => socket || signedOut || connect(null), wait);
  };
}

document.addEventListener("visibilitychange", () => {
  if (document.visibilityState !== "visible") return;
  if (!socket && !signedOut) connect(null);
  // Back to the page: it's the one used now.
  else fit(true);
});

function send(msg) {
  if (socket && socket.readyState === WebSocket.OPEN) socket.send(JSON.stringify(msg));
}

function onText(m) {
  switch (m.t) {
    case "hello":
      theme = m.theme;
      document.documentElement.style.setProperty("--bg", theme.background);
      document.documentElement.style.setProperty("--fg", theme.foreground);
      if (term) term.options.theme = theme;
      break;
    case "sessions":
      sessions = m.list;
      noteStatuses();
      renderList();
      renderTabs();
      renderHeading();
      break;
    case "view":
      if (m.id !== pending) break;
      pending = null;
      current = m.id;
      size = { cols: m.cols, rows: m.rows };
      ensureTerm();
      term.reset();
      term.resize(m.cols, m.rows);
      snapshotNext = true;
      setBanner(null);
      layout();
      fit(true);
      break;
    case "resize":
      // The desktop's window changed: drawn to fit as it is.
      size = { cols: m.cols, rows: m.rows };
      if (term) term.resize(m.cols, m.rows);
      layout();
      break;
    case "exited":
      setBanner(m.code == null ? "The program ended." : `The program ended (exit code ${m.code}).`);
      break;
    case "gone":
      if (m.id === current || m.id === pending) {
        current = pending = null;
        showList();
        history.replaceState(null, "", location.pathname);
      }
      break;
    case "error":
      showMessage(m.message);
      break;
    case "revoked":
      signOut(`This device's access to Tern was revoked. ${PAIR_HINT}`);
      break;
  }
}

function onBytes(data) {
  if (pending != null || current == null || !term) return;
  const bytes = new Uint8Array(data);
  if (snapshotNext) {
    snapshotNext = false;
    // The snapshot paints the whole screen and scrollback: start at the end.
    term.write(bytes, () => term.scrollToBottom());
  } else {
    term.write(bytes);
  }
}

// ---- The list -------------------------------------------------------------
// Laid out as Tern's agents panel and agent monitor (templates `agents.folder`
// and `agents.tab`): each folder, the state of its agents that most wants
// you, and its agents under it with their state. Plain terminals aren't
// listed.

function agents() {
  return sessions.filter((s) => s.claude);
}

function byState(a, b) {
  return (a.exited - b.exited) || label(a).localeCompare(label(b));
}

function label(s) {
  // Without the status glyph some agents put first (Claude Code's "✳").
  const title = (s.title || "").replace(/^[^\p{L}\p{N}]+/u, "").trim();
  if (title) return title;
  if (s.program) return s.program;
  return s.cwd ? baseName(s.cwd) : `Terminal ${s.id}`;
}

function baseName(path) {
  return path.split(/[\\/]/).filter(Boolean).pop() || path;
}

const STATUS = { Working: "Working", Blocked: "Needs input", Done: "Done" };
const PRIORITY = { Blocked: 3, Working: 2, Done: 1 };
// Lucide icons, as the app draws them.
const ICON = {
  Working: '<circle cx="12" cy="12" r="10" stroke-dasharray="3.5 2.8"/>',
  Blocked: '<path d="M7.9 20A9 9 0 1 0 4 16.1L2 22Z"/><path d="M9.09 9a3 3 0 0 1 5.83 1c0 2-3 3-3 3"/><path d="M12 17h.01"/>',
  Done: '<circle cx="12" cy="12" r="10"/><path d="m9 12 2 2 4-4"/>',
  folder: '<path d="m6 14 1.5-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.54 6a2 2 0 0 1-1.95 1.5H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.9a2 2 0 0 1 1.69.9l.81 1.2a2 2 0 0 0 1.67.9H18a2 2 0 0 1 2 2v2"/>',
  open: '<path d="m6 9 6 6 6-6"/>',
  closed: '<path d="m9 18 6-6-6-6"/>',
  terminal: '<path d="m7 11 2-2-2-2"/><path d="M11 13h4"/><rect width="18" height="18" x="3" y="3" rx="2"/>',
  claude: '<path d="M12 3v18M3 12h18M5.6 5.6l12.8 12.8M18.4 5.6 5.6 18.4"/>',
};

function icon(name, cls) {
  const span = document.createElement("span");
  span.className = `ic ${cls || ""}`;
  span.innerHTML = `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">${ICON[name]}</svg>`;
  return span;
}

// When each session's state was last seen to change: its working time, or
// how long ago its turn ended (from the page's first look at it).
const since = new Map();
// The folders whose terminals are hidden.
const collapsed = new Set();

function workingLabel(ms) {
  const s = Math.max(0, Math.floor(ms / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  return m < 60 ? `${m}m` : `${Math.floor(m / 60)}h ${m % 60}m`;
}

function agoLabel(ms) {
  const m = Math.floor(ms / 60000);
  if (m < 1) return "now";
  if (m < 60) return `${m}m`;
  if (m < 1440) return `${Math.floor(m / 60)}h`;
  return `${Math.floor(m / 1440)}d`;
}

function noteStatuses() {
  const now = Date.now();
  const seen = new Set();
  for (const s of sessions) {
    seen.add(s.id);
    const key = s.exited ? "exited" : s.status;
    const was = since.get(s.id);
    if (!was || was.status !== key) since.set(s.id, { status: key, at: now });
  }
  for (const id of since.keys()) if (!seen.has(id)) since.delete(id);
}

// A row's state: label, icon and class (the color), as `agents::status_look`.
function statusLook(s) {
  const at = since.get(s.id)?.at ?? Date.now();
  if (s.exited) return { text: "Ended", cls: "" };
  if (s.status === "Working") return { text: `Working ${workingLabel(Date.now() - at)}`, icon: "Working", cls: "Working" };
  if (STATUS[s.status]) return { text: STATUS[s.status], icon: s.status, cls: s.status };
  return { text: agoLabel(Date.now() - at), cls: "" };
}

function renderList() {
  const ul = $("sessions");
  ul.replaceChildren();
  // By folder, in the order they come; the agents first in each.
  const groups = new Map();
  const shown = agents();
  for (const s of shown) {
    const key = s.cwd || "";
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key).push(s);
  }
  for (const [cwd, list] of groups) {
    list.sort(byState);
    const open = !collapsed.has(cwd);
    const folder = document.createElement("li");
    folder.className = "folder";
    folder.append(icon("folder", "muted"));
    const name = document.createElement("span");
    name.className = "name";
    name.textContent = cwd ? baseName(cwd) : "Other";
    name.title = cwd;
    // As the app: the chevron after the name, shown on hover.
    const fill = document.createElement("span");
    fill.className = "fill";
    folder.append(name, icon(open ? "open" : "closed", "chevron"), fill);
    const top = list.filter((s) => !s.exited).map((s) => s.status).sort((a, b) => (PRIORITY[b] ?? 0) - (PRIORITY[a] ?? 0))[0];
    if (PRIORITY[top]) {
      const dot = document.createElement("span");
      dot.className = `dot ${top}`;
      folder.append(dot);
    }
    folder.onclick = () => {
      if (!collapsed.delete(cwd)) collapsed.add(cwd);
      renderList();
    };
    ul.append(folder);
    if (!open) continue;
    for (const s of list) {
      const li = document.createElement("li");
      li.className = `tab ${s.exited ? "exited" : ""} ${s.status === "Working" ? "working" : ""} ${s.id === (pending ?? current) ? "on" : ""}`;
      li.append(icon("claude", "agent"));
      const title = document.createElement("span");
      title.className = "name";
      title.textContent = label(s);
      li.append(title);
      const look = statusLook(s);
      const status = document.createElement("span");
      status.className = `status ${look.cls}`;
      if (look.icon) status.append(icon(look.icon));
      status.append(look.text);
      li.append(status);
      li.onclick = () => openSession(s.id);
      ul.append(li);
    }
  }
  $("empty").hidden = shown.length > 0;
  const waiting = shown.filter((s) => s.status === "Blocked" && !s.exited).length;
  document.title = waiting ? `(${waiting}) Tern` : "Tern";
}

// Working times count seconds.
setInterval(() => {
  if (sessions.some((s) => s.claude && s.status === "Working" && !s.exited)) {
    if (!$("list-view").hidden) renderList();
    renderTabs();
  }
}, 1000);

function renderHeading() {
  const s = sessions.find((x) => x.id === (pending ?? current));
  if ($("term-view").hidden || !s) {
    $("title").textContent = "Tern";
    const n = agents().length;
    $("subtitle").textContent = `${n} agent${n === 1 ? "" : "s"}`;
    return;
  }
  $("title").textContent = label(s);
  const status = s.exited ? "Ended" : STATUS[s.status];
  $("subtitle").textContent = [status, s.cwd].filter(Boolean).join(" · ");
}

// The agents of the shown session's folder, as Tern's tab bar (wide screens).
function renderTabs() {
  const nav = $("tabs");
  const id = pending ?? current;
  const s = sessions.find((x) => x.id === id);
  nav.hidden = !wideScreen.matches || !s;
  if (nav.hidden) return;
  const list = agents().filter((x) => (x.cwd || "") === (s.cwd || ""));
  if (!list.includes(s)) list.push(s);
  list.sort(byState);
  nav.replaceChildren(
    ...list.map((x) => {
      const b = document.createElement("button");
      b.className = `tab ${x.id === id ? "on" : ""}`;
      b.append(icon(x.claude ? "claude" : "terminal", x.claude ? "agent" : "muted"));
      const name = document.createElement("span");
      name.className = "name";
      name.textContent = label(x);
      b.append(name);
      if (!x.exited && PRIORITY[x.status]) {
        const dot = document.createElement("span");
        dot.className = `dot ${x.status}`;
        b.append(dot);
      }
      b.title = statusLook(x).text;
      b.onclick = () => x.id !== id && openSession(x.id);
      return b;
    }),
  );
}

function show(which) {
  view = which;
  const wide = wideScreen.matches;
  document.body.classList.toggle("wide", wide);
  // Wide: the list stays, and the terminal (or a note) beside it.
  const shown = which === "message-view" || !wide ? [which] : ["list-view", which === "term-view" ? "term-view" : "pick-view"];
  for (const id of ["list-view", "pick-view", "term-view", "message-view"]) $(id).hidden = !shown.includes(id);
  $("back").hidden = wide || which !== "term-view";
  renderHeading();
  renderList();
  renderTabs();
  layout();
  fit();
}

wideScreen.addEventListener("change", () => show(view));

function showList() {
  show("list-view");
}

function showMessage(text) {
  $("message").textContent = text;
  show("message-view");
}

function openSession(id) {
  pending = id;
  send({ t: "view", id });
  ensureTerm();
  show("term-view");
  history.replaceState(null, "", `${location.pathname}?s=${id}`);
}

$("back").onclick = () => {
  history.replaceState(null, "", location.pathname);
  // The session goes back to the desktop's size.
  send({ t: "leave" });
  current = pending = null;
  asked = null;
  showList();
};

// ---- The terminal ---------------------------------------------------------

function ensureTerm() {
  if (term) return;
  term = new Terminal({
    fontFamily: FONT,
    fontSize: FONT_SIZE,
    lineHeight: LINE_HEIGHT,
    scrollback: 10000,
    allowProposedApi: true,
    cursorBlink: false,
    macOptionIsMeta: true,
    theme: theme || undefined,
    cols: size.cols,
    rows: size.rows,
  });
  term.loadAddon(new Unicode11Addon.Unicode11Addon());
  term.unicode.activeVersion = "11";
  term.open($("term"));
  try {
    const webgl = new WebglAddon.WebglAddon();
    webgl.onContextLoss(() => webgl.dispose());
    term.loadAddon(webgl);
  } catch (e) {
    // The DOM renderer is fine.
  }
  quietReplies(term);
  term.onData((d) => {
    // Focus reports are the desktop's to send.
    if (d === "\x1b[I" || d === "\x1b[O") return;
    input(d);
  });
  term.onBinary((d) => input(d));
}

// Tern's background server already answers the program's queries (device
// attributes, cursor position, colors, modes); a second answer from here
// would reach the program as typed text.
function quietReplies(t) {
  const p = t.parser;
  const swallow = () => true;
  for (const id of [
    { final: "c" },
    { prefix: ">", final: "c" },
    { prefix: "=", final: "c" },
    { final: "n" },
    { prefix: "?", final: "n" },
    { intermediates: "$", final: "p" },
    { prefix: "?", intermediates: "$", final: "p" },
    { prefix: ">", final: "q" },
    { final: "t" },
  ]) {
    p.registerCsiHandler(id, swallow);
  }
  p.registerDcsHandler({ intermediates: "$", final: "q" }, swallow);
  for (const osc of [4, 10, 11, 12]) {
    // Queries only; colors the program sets still apply.
    p.registerOscHandler(osc, (data) => data.includes("?"));
  }
}

function input(d) {
  if (current != null && pending == null) send({ t: "input", d });
}

// The size of one cell at font size `fs`, as xterm.js draws it. It rounds
// cells to device pixels, so they don't scale with the font size: measured
// at that size, not worked out from another.
function cellAt(fs) {
  const dims = () => term._core && term._core._renderService && term._core._renderService.dimensions.css.cell;
  const was = term.options.fontSize;
  if (was !== fs) term.options.fontSize = fs;
  const c = dims();
  const cell = c && c.width ? { w: c.width, h: c.height } : { w: fs * 0.6, h: Math.ceil(fs * 1.2 * LINE_HEIGHT) };
  if (was !== fs) term.options.fontSize = was;
  return cell;
}

function room() {
  const wrap = $("term-wrap");
  const cs = getComputedStyle(wrap);
  const w = wrap.clientWidth - parseFloat(cs.paddingLeft) - parseFloat(cs.paddingRight);
  const h = wrap.clientHeight - parseFloat(cs.paddingTop) - parseFloat(cs.paddingBottom);
  // The on-screen keyboard takes most of a phone's height, for as long as
  // one types: the grid is sized for the height without it (the most seen
  // at this width), so the program isn't squeezed to a few rows each time.
  if (w !== tall.w) tall = { w, h };
  else tall.h = Math.max(tall.h, h);
  return { w, h, tallH: tall.h };
}

// The terminal's height without the on-screen keyboard, at this width.
let tall = { w: 0, h: 0 };

// Draw the session's grid as large as fits the screen, at most FONT_SIZE;
// scaled down past MIN_FONT. With the on-screen keyboard up, its bottom
// rows (the prompt) show, above the keyboard.
function layout() {
  if (!term || $("term-view").hidden) return;
  const { w, h, tallH } = room();
  let fs = 0;
  for (let f = FONT_SIZE; f >= MIN_FONT; f -= 0.5) {
    const c = cellAt(f);
    if (size.cols * c.w <= w && size.rows * c.h <= tallH) {
      fs = f;
      break;
    }
  }
  let scale = 1;
  if (!fs) {
    fs = MIN_FONT;
    const c = cellAt(fs);
    scale = Math.min(1, w / (size.cols * c.w), tallH / (size.rows * c.h));
  }
  if (term.options.fontSize !== fs) term.options.fontSize = fs;
  const drawn = size.rows * cellAt(fs).h * scale;
  const up = Math.max(0, drawn - h);
  const t = [up ? `translateY(${-up}px)` : "", scale < 1 ? `scale(${scale})` : ""].join(" ").trim();
  $("term").style.transform = t;
}

// The grid that fits the screen at FONT_SIZE, and the size of its cell in
// device pixels.
function fitting() {
  const { w, tallH: h } = room();
  const c = cellAt(FONT_SIZE);
  const dpr = window.devicePixelRatio || 1;
  return {
    cols: Math.max(10, Math.floor(w / c.w)),
    rows: Math.max(3, Math.floor(h / c.h)),
    cell_width: Math.round(c.w * dpr),
    cell_height: Math.round(c.h * dpr),
  };
}

// The size last asked for the shown session ("id cols rows").
let asked = null;
let fitTimer = null;

// Ask for the grid that fits the screen: at once with `now` (a view began,
// or the page came back: asked again, the desktop may have it since), else
// once the screen stops changing (rotation and the on-screen keyboard
// animate), when it changed.
function fit(now) {
  clearTimeout(fitTimer);
  if (now) ask(true);
  else fitTimer = setTimeout(() => ask(false), 150);
}

function ask(again) {
  if (!term || current == null || pending != null || $("term-view").hidden) return;
  const want = fitting();
  const key = `${current} ${want.cols} ${want.rows}`;
  if (again || key !== asked) {
    asked = key;
    send({ t: "resize", ...want });
  }
}

// The screen changed (window, rotation, the on-screen keyboard).
window.addEventListener("resize", () => {
  layout();
  fit();
});

// The on-screen keyboard: iOS ignores `interactive-widget` and lays the page
// out behind it, so the page is made the height of what is left above it.
// Times the zoom, so pinching in doesn't shrink the page.
if (window.visualViewport) {
  const vv = window.visualViewport;
  const fitKeyboard = () => {
    document.body.style.height = `${Math.round(vv.height * vv.scale)}px`;
    // iOS scrolls the page to show the focused box: it fits now.
    if (vv.scale <= 1.01) window.scrollTo(0, 0);
    layout();
    fit();
  };
  vv.addEventListener("resize", fitKeyboard);
  vv.addEventListener("scroll", fitKeyboard);
  fitKeyboard();
}

// A finger drag scrolls as a mouse wheel does. xterm.js only scrolls its
// scrollback by touch, and not at all while the program reads the mouse
// (Claude Code does): the wheel goes to the program then, as on the desktop.
// Two fingers are left to the browser (pinch to zoom).
{
  const wrap = $("term-wrap");
  let lastY = null;
  wrap.addEventListener("touchstart", (e) => {
    lastY = e.touches.length === 1 ? e.touches[0].clientY : null;
  }, { capture: true, passive: true });
  wrap.addEventListener("touchmove", (e) => {
    if (lastY == null || e.touches.length !== 1 || !term) return;
    e.preventDefault();
    e.stopPropagation();
    const t = e.touches[0];
    const dy = lastY - t.clientY;
    lastY = t.clientY;
    if (!dy) return;
    term.element.querySelector(".xterm-screen").dispatchEvent(new WheelEvent("wheel", {
      deltaY: dy,
      deltaMode: WheelEvent.DOM_DELTA_PIXEL,
      clientX: t.clientX,
      clientY: t.clientY,
      bubbles: true,
      cancelable: true,
    }));
  }, { capture: true, passive: false });
  wrap.addEventListener("touchend", () => (lastY = null), { capture: true, passive: true });
}

// ---- Keys and the message box ---------------------------------------------

const KEYS = {
  esc: () => "\x1b",
  tab: () => "\t",
  stab: () => "\x1b[Z",
  up: () => arrow("A"),
  down: () => arrow("B"),
  right: () => arrow("C"),
  left: () => arrow("D"),
  ctrlc: () => "\x03",
  enter: () => "\r",
};

function arrow(c) {
  return term && term.modes.applicationCursorKeysMode ? `\x1bO${c}` : `\x1b[${c}`;
}

for (const b of $("keys").querySelectorAll("button")) {
  // Keep the keyboard up: don't take focus from the message box.
  b.addEventListener("pointerdown", (e) => e.preventDefault());
  b.onclick = () => input(KEYS[b.dataset.key]());
}

const text = $("text");
text.addEventListener("input", () => {
  text.style.height = "auto";
  text.style.height = `${text.scrollHeight}px`;
});
text.addEventListener("keydown", (e) => {
  if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
    e.preventDefault();
    $("compose").requestSubmit();
  }
});
$("compose").onsubmit = (e) => {
  e.preventDefault();
  const value = text.value;
  if (!term || current == null) return;
  if (value) {
    // As a paste (bracketed when the program asked), so new lines don't
    // send it early; then Enter.
    term.paste(value);
  }
  input("\r");
  text.value = "";
  text.style.height = "auto";
};

function setBanner(textValue) {
  $("banner").hidden = !textValue;
  $("banner").textContent = textValue || "";
}

show("list-view");
start();
