// Tern in a browser: the list of Tern's terminals, and one of them in
// xterm.js. The server sends the session's bytes as they come; this page
// only draws them and sends back what is typed. See bridge.rs for the
// messages.
"use strict";

const $ = (id) => document.getElementById(id);

const FONT = 'Menlo, Monaco, Consolas, "DejaVu Sans Mono", "Liberation Mono", monospace';
const LINE_HEIGHT = 1.15;
const MIN_FONT = 7;
const MAX_FONT = 15;
// The size a fitted terminal is drawn at.
const FIT_FONT = 13;

let socket = null;
let retry = 0;
let theme = null;
let sessions = [];
// The session shown, and the one asked for whose snapshot hasn't come yet
// (output before it belongs to the session left).
let current = null;
let pending = null;
let size = { cols: 80, rows: 24 };
// A phone fits the terminal to its screen unless told otherwise: the
// desktop's grid can't be read at a size that fits there.
let fit = (localStorage.getItem("tern.fit") ?? (matchMedia("(pointer: coarse)").matches ? "1" : "0")) === "1";
let term = null;
let snapshotNext = false;

// ---- The connection ---------------------------------------------------------

async function start() {
  const m = location.hash.match(/[#&]t=([0-9a-f]+)/);
  if (m) {
    // From the link: trade it for a cookie and take it out of the address.
    history.replaceState(null, "", location.pathname);
    const r = await fetch("/login", { method: "POST", body: m[1] }).catch(() => null);
    if (!r || !r.ok) return showMessage("This link isn't valid any more. Run tern --web on the computer and open the link it prints.");
  }
  const s = await fetch("/session").then((r) => r.json()).catch(() => false);
  if (!s) return showMessage("Open the link that tern --web prints on the computer running Tern.");
  // Ids are strings: they don't fit a JavaScript number.
  connect(new URLSearchParams(location.search).get("s"));
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
  socket.onclose = () => {
    $("conn").className = "dot down";
    socket = null;
    // Back off to 10 s; at once when the page comes back to the front.
    const wait = Math.min(10000, 500 * 2 ** retry++);
    setTimeout(() => socket || connect(null), wait);
  };
}

document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "visible" && !socket) connect(null);
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
      // Give the PTY this screen's size once the attach's repaint (a one
      // row resize and back) is over, so the size given back later is the
      // real one.
      if (fit) {
        const id = m.id;
        setTimeout(() => fit && current === id && sendFit(), 300);
      }
      break;
    case "resize":
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
  }
}

function onBytes(data) {
  if (pending != null || current == null || !term) return;
  const bytes = new Uint8Array(data);
  if (snapshotNext) {
    snapshotNext = false;
    // The snapshot paints the whole screen and scrollback: start at the end.
    // A terminal taller than the screen shows its bottom, where prompts are.
    term.write(bytes, () => {
      term.scrollToBottom();
      $("term-wrap").scrollTop = $("term-wrap").scrollHeight;
    });
  } else {
    term.write(bytes);
  }
}

// ---- The list -------------------------------------------------------------
// Laid out as Tern's agents panel and agent monitor (templates `agents.folder`
// and `agents.tab`): each folder, the state of its terminals that most wants
// you, and its terminals under it with their state.

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
  for (const s of sessions) {
    const key = s.cwd || "";
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key).push(s);
  }
  for (const [cwd, list] of groups) {
    list.sort((a, b) => (a.exited - b.exited) || (b.claude - a.claude) || label(a).localeCompare(label(b)));
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
      li.className = `tab ${s.exited ? "exited" : ""} ${s.status === "Working" ? "working" : ""}`;
      li.append(icon(s.claude ? "claude" : "terminal", s.claude ? "agent" : "muted"));
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
  $("empty").hidden = sessions.length > 0;
  const waiting = sessions.filter((s) => s.status === "Blocked" && !s.exited).length;
  document.title = waiting ? `(${waiting}) Tern` : "Tern";
}

// Working times count seconds.
setInterval(() => {
  if (!$("list-view").hidden && sessions.some((s) => s.status === "Working" && !s.exited)) renderList();
}, 1000);

function renderHeading() {
  const s = sessions.find((x) => x.id === (pending ?? current));
  if ($("term-view").hidden || !s) {
    $("title").textContent = "Tern";
    $("subtitle").textContent = `${sessions.length} terminal${sessions.length === 1 ? "" : "s"}`;
    return;
  }
  $("title").textContent = label(s);
  const status = s.exited ? "Ended" : STATUS[s.status];
  $("subtitle").textContent = [status, s.cwd].filter(Boolean).join(" · ");
}

function show(which) {
  for (const id of ["list-view", "term-view", "message-view"]) $(id).hidden = id !== which;
  $("back").hidden = which !== "term-view";
  $("fit").hidden = which !== "term-view";
  renderHeading();
}

function showList() {
  if (current != null && fit) send({ t: "unfit" });
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
  showList();
};

// ---- The terminal ---------------------------------------------------------

function ensureTerm() {
  if (term) return;
  term = new Terminal({
    fontFamily: FONT,
    fontSize: FIT_FONT,
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
  return { w, h };
}

// Draw the session's grid as large as fits the width (and the height, when
// it was fitted to this screen).
function layout() {
  if (!term || $("term-view").hidden) return;
  const { w, h } = room();
  let fs = MIN_FONT;
  for (let f = MAX_FONT; f > MIN_FONT; f -= 0.5) {
    const c = cellAt(f);
    if (size.cols * c.w <= w && (!fit || size.rows * c.h <= h)) {
      fs = f;
      break;
    }
  }
  if (term.options.fontSize !== fs) term.options.fontSize = fs;
}

function sendFit() {
  const { w, h } = room();
  const c = cellAt(FIT_FONT);
  const cols = Math.floor(w / c.w);
  const rows = Math.floor(h / c.h);
  if (cols > 0 && rows > 0 && (cols !== size.cols || rows !== size.rows)) send({ t: "resize", cols, rows });
}

function setFit(on) {
  fit = on;
  localStorage.setItem("tern.fit", on ? "1" : "0");
  $("fit").classList.toggle("on", on);
  if (current == null) return;
  if (on) sendFit();
  else send({ t: "unfit" });
}

$("fit").classList.toggle("on", fit);
$("fit").onclick = () => setFit(!fit);

let resizeTimer = 0;
window.addEventListener("resize", () => {
  clearTimeout(resizeTimer);
  resizeTimer = setTimeout(() => {
    layout();
    if (fit && current != null) sendFit();
  }, 150);
});

// Leaving the page gives the session its size back.
window.addEventListener("pagehide", () => {
  if (fit && current != null) send({ t: "unfit" });
});

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

start();
