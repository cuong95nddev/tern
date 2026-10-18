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
let fit = localStorage.getItem("tern.fit") === "1";
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

function label(s) {
  if (s.title) return s.title;
  if (s.program) return s.program;
  return s.cwd ? s.cwd.split(/[\\/]/).filter(Boolean).pop() || s.cwd : `Terminal ${s.id}`;
}

const STATUS = { Working: "Working", Blocked: "Needs input", Done: "Done" };
const ORDER = { Blocked: 0, Done: 1, Working: 2 };

function renderList() {
  const ul = $("sessions");
  ul.replaceChildren();
  const list = [...sessions].sort(
    (a, b) => (a.exited - b.exited) || ((ORDER[a.status] ?? 3) - (ORDER[b.status] ?? 3)) || label(a).localeCompare(label(b)),
  );
  for (const s of list) {
    const li = document.createElement("li");
    li.className = s.exited ? "exited" : "";
    const info = document.createElement("div");
    info.className = "info";
    const name = document.createElement("div");
    name.className = "name";
    name.textContent = label(s);
    const where = document.createElement("div");
    where.className = "where";
    // Shown right to left so the end of a long path stays visible.
    where.textContent = "‎" + [s.cwd, s.program].filter(Boolean).join(" · ");
    info.append(name, where);
    li.append(info);
    const status = s.exited ? "Ended" : STATUS[s.status];
    if (status) {
      const badge = document.createElement("span");
      badge.className = `badge ${s.exited ? "" : s.status}`;
      badge.textContent = status;
      li.append(badge);
    }
    li.onclick = () => openSession(s.id);
    ul.append(li);
  }
  $("empty").hidden = sessions.length > 0;
  const waiting = sessions.filter((s) => s.status === "Blocked" && !s.exited).length;
  document.title = waiting ? `(${waiting}) Tern` : "Tern";
}

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

// The width of one cell per pixel of font size: as xterm.js drew it, else
// as measured.
let cellRatio = null;
function charRatio() {
  const drawn = term && term._core && term._core._renderService && term._core._renderService.dimensions.css.cell.width;
  if (drawn) return drawn / term.options.fontSize;
  if (cellRatio) return cellRatio;
  const c = document.createElement("canvas").getContext("2d");
  c.font = `100px ${FONT}`;
  cellRatio = c.measureText("W").width / 100;
  return cellRatio;
}

function room() {
  const wrap = $("term-wrap");
  const cs = getComputedStyle(wrap);
  const w = wrap.clientWidth - parseFloat(cs.paddingLeft) - parseFloat(cs.paddingRight);
  const h = wrap.clientHeight - parseFloat(cs.paddingTop) - parseFloat(cs.paddingBottom);
  return { w, h };
}

// Draw the session's grid as large as fits the width.
function layout() {
  if (!term || $("term-view").hidden) return;
  const { w } = room();
  const fs = Math.max(MIN_FONT, Math.min(MAX_FONT, Math.floor((w / (size.cols * charRatio())) * 4) / 4));
  if (term.options.fontSize !== fs) term.options.fontSize = fs;
}

function sendFit() {
  const { w, h } = room();
  const cols = Math.floor(w / (FIT_FONT * charRatio()));
  const rows = Math.floor(h / Math.ceil(FIT_FONT * LINE_HEIGHT));
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
