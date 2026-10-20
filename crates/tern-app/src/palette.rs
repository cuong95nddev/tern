//! The command palette (⌘⇧P), as in T3 Code: Tern's commands, filtered as
//! you type, and Search in Files (⌘⇧F), which searches the workspace's
//! files and opens a match at its line. Escape in Search in Files goes
//! back to the commands (with what was typed there); Escape there closes.
//! A workspace keeps its palette, so a search is still there when it opens
//! again.

use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui_kit::component::input::{Escape, Input, InputEvent, InputState, MoveDown, MoveUp};
use gpui_kit::component::{ActiveTheme, WindowExt as _};
use gpui_kit::*;
use tern_fs::search::{Match, Query, Results};

use crate::platform::shortcut;
use crate::template::{self, Framed, on_event};
use crate::{OpenFolder, agent_monitor, file_icons, main_window, restart, style, terminal_view, theme_picker, update, workspace};

/// Wait this long after a keystroke before searching.
const DEBOUNCE: Duration = Duration::from_millis(120);
/// At most this many matching lines.
const LIMIT: usize = 500;
/// A match further into its line than this many bytes is shown with the
/// start of the line cut.
const LEAD: usize = 48;
/// What is kept before the first match when the start of its line is cut.
const KEEP: usize = 16;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Commands,
    Search,
}

pub enum PaletteEvent {
    /// Run a command (the palette has closed).
    Run(Box<dyn Action>),
    /// Open the file at `path` with the cursor at `line`, `col` (1-based).
    Open { path: PathBuf, line: u32, col: u32 },
    /// Escape with the commands showing.
    Close,
}

enum Do {
    Search,
    Action(fn() -> Box<dyn Action>),
}

struct Command {
    title: &'static str,
    key: &'static str,
    run: Do,
}

macro_rules! cmd {
    ($title:literal, $key:literal, $action:expr) => {
        Command { title: $title, key: $key, run: Do::Action(|| Box::new($action)) }
    };
}

/// The commands, in the order shown before anything is typed.
const COMMANDS: &[Command] = &[
    Command { title: "Search in Files", key: "⌘⇧F", run: Do::Search },
    cmd!("New Claude Tab", "⌘T", workspace::NewClaudeTab),
    cmd!("New Shell Tab", "⌘⇧T", workspace::NewShellTab),
    cmd!("New Scratch File", "⌘N", workspace::NewScratch),
    cmd!("Toggle Explorer", "⌘B", workspace::ToggleExplorer),
    cmd!("Toggle Changes", "⌘⇧G", workspace::ToggleChanges),
    cmd!("Toggle Scratches", "⌘⇧S", workspace::ToggleScratches),
    cmd!("Focus Terminal", "⌘J", workspace::FocusTerminal),
    cmd!("Split Right", "⌘\\", workspace::SplitRight),
    cmd!("Split Down", "⌘⇧\\", workspace::SplitDown),
    cmd!("Maximize Pane", "⌘⇧↩", workspace::ToggleZoomPane),
    cmd!("Next Tab", "⌘⇧]", workspace::NextTab),
    cmd!("Previous Tab", "⌘⇧[", workspace::PrevTab),
    cmd!("Close Tab", "⌘W", workspace::CloseTab),
    cmd!("Reset Layout", "", workspace::ResetLayout),
    cmd!("Clear Terminal", "⌘K", terminal_view::Clear),
    cmd!("Agent Monitor", "", agent_monitor::ToggleAgentMonitor),
    cmd!("Next Workspace", "", main_window::NextWorkspace),
    cmd!("Previous Workspace", "", main_window::PrevWorkspace),
    cmd!("Close Workspace", "", main_window::CloseWorkspace),
    cmd!("New Window", "", main_window::NewWindow),
    cmd!("Open Folder…", "⌘O", OpenFolder),
    cmd!("Settings…", "⌘,", theme_picker::OpenSettings),
    cmd!("Open Settings File", "", theme_picker::OpenSettingsFile),
    cmd!("Theme…", "⌘K ⌘T", theme_picker::SelectTheme),
    cmd!("Check for Updates…", "", update::CheckForUpdates),
    cmd!("Restart Background Terminals…", "", restart::RestartBackground),
];

/// The indices of the commands `query` matches, best first: every word of
/// it is in the title; titles starting with it come first.
fn filter_commands(query: &str) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    let words: Vec<&str> = q.split_whitespace().collect();
    let mut found: Vec<(bool, usize)> = COMMANDS
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            let title = c.title.to_lowercase();
            words.iter().all(|w| title.contains(w)).then(|| (!title.starts_with(&q), i))
        })
        .collect();
    found.sort();
    found.into_iter().map(|(_, i)| i).collect()
}

/// A row of the search results: a file (its first match, its count) or a match.
#[derive(Clone, Copy)]
enum Row {
    File(usize, usize),
    Match(usize),
}

enum Status {
    Idle,
    Searching,
    Done,
    Error(String),
}

pub struct Palette {
    root: PathBuf,
    mode: Mode,
    commands_input: Entity<InputState>,
    search_input: Entity<InputState>,
    /// Indices into [`COMMANDS`], as filtered.
    commands: Vec<usize>,
    options: Query,
    results: Results,
    rows: Vec<Row>,
    /// Per match, its row.
    match_row: Vec<usize>,
    status: Status,
    /// The selected command or match.
    selected: usize,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    /// Set to stop the running search.
    cancel: Arc<AtomicBool>,
    _search: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<PaletteEvent> for Palette {}

impl Focusable for Palette {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Palette {
    pub fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let commands_input = cx.new(|cx| InputState::new(window, cx).placeholder("Type a command"));
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Search in files"));
        let subs = vec![
            cx.subscribe_in(&commands_input, window, |this, input, ev: &InputEvent, window, cx| match ev {
                InputEvent::Change => {
                    this.commands = filter_commands(&input.read(cx).value());
                    this.select(0, cx);
                },
                InputEvent::PressEnter { .. } => this.confirm(window, cx),
                _ => {},
            }),
            cx.subscribe_in(&search_input, window, |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::Change => this.search_soon(cx),
                InputEvent::PressEnter { .. } => this.confirm(window, cx),
                _ => {},
            }),
        ];
        Self {
            root,
            mode: Mode::Commands,
            commands_input,
            search_input,
            commands: filter_commands(""),
            options: Query::default(),
            results: Results::default(),
            rows: vec![],
            match_row: vec![],
            status: Status::Idle,
            selected: 0,
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            cancel: Arc::new(AtomicBool::new(false)),
            _search: None,
            _subs: subs,
        }
    }

    /// Show `mode`, its field focused with what it holds selected.
    pub fn show(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        self.mode = mode;
        let input = self.input().clone();
        input.update(cx, |i, cx| {
            i.focus(window, cx);
            i.select_all(window, cx);
        });
        if mode == Mode::Commands {
            self.commands = filter_commands(&input.read(cx).value());
        }
        self.select(0, cx);
    }

    fn input(&self) -> &Entity<InputState> {
        match self.mode {
            Mode::Commands => &self.commands_input,
            Mode::Search => &self.search_input,
        }
    }

    /// How many entries the selection moves over.
    fn len(&self) -> usize {
        match self.mode {
            Mode::Commands => self.commands.len(),
            Mode::Search => self.results.matches.len(),
        }
    }

    fn row_of(&self, entry: usize) -> usize {
        match self.mode {
            Mode::Commands => entry,
            Mode::Search => self.match_row.get(entry).copied().unwrap_or(0),
        }
    }

    fn select(&mut self, entry: usize, cx: &mut Context<Self>) {
        self.selected = entry;
        if self.len() > 0 {
            // A match's file sits above it: show it too at the top.
            let row = self.row_of(entry);
            let file_above = self.mode == Mode::Search && entry > 0 && row > 0 && matches!(self.rows[row - 1], Row::File(..));
            self.scroll.scroll_to_item(if file_above { row - 1 } else { row }, ScrollStrategy::Nearest);
        }
        cx.notify();
    }

    fn step(&mut self, by: isize, cx: &mut Context<Self>) {
        let n = self.len() as isize;
        if n > 0 {
            self.select((self.selected as isize + by).rem_euclid(n) as usize, cx);
        }
    }

    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.mode {
            Mode::Search => self.show(Mode::Commands, window, cx),
            Mode::Commands => cx.emit(PaletteEvent::Close),
        }
    }

    /// Enter, or a click: run the selected command, open the selected match.
    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.mode {
            Mode::Commands => {
                let Some(c) = self.commands.get(self.selected).map(|&i| &COMMANDS[i]) else { return };
                match c.run {
                    Do::Search => self.show(Mode::Search, window, cx),
                    Do::Action(make) => cx.emit(PaletteEvent::Run(make())),
                }
            },
            Mode::Search => {
                let Some(m) = self.results.matches.get(self.selected) else { return };
                let col = m.ranges.first().map_or(1, |r| m.text[..r.start].chars().count() as u32 + 1);
                cx.emit(PaletteEvent::Open { path: self.root.join(&m.path), line: m.line, col });
            },
        }
    }

    fn toggle_option(&mut self, set: fn(&mut Query), cx: &mut Context<Self>) {
        set(&mut self.options);
        self.search_soon(cx);
    }

    /// Search again once typing pauses; a search still running stops.
    fn search_soon(&mut self, cx: &mut Context<Self>) {
        self.cancel.store(true, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = cancel.clone();
        let query = Query { text: self.search_input.read(cx).value().to_string(), ..self.options.clone() };
        if query.text.is_empty() {
            self._search = None;
            self.set_results(Results::default(), Status::Idle, cx);
            return;
        }
        self.status = Status::Searching;
        cx.notify();
        let root = self.root.clone();
        self._search = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DEBOUNCE).await;
            let found =
                cx.background_spawn(async move { tern_fs::search::search(&root, &query, LIMIT, &cancel).map(|r| (r, cancel)) }).await;
            let _ = this.update(cx, |this, cx| match found {
                Ok((_, cancel)) if cancel.load(Ordering::Relaxed) => {},
                Ok((results, _)) => this.set_results(results, Status::Done, cx),
                Err(e) => this.set_results(Results::default(), Status::Error(e), cx),
            });
        }));
    }

    fn set_results(&mut self, results: Results, status: Status, cx: &mut Context<Self>) {
        self.rows.clear();
        self.match_row.clear();
        let mut i = 0;
        while i < results.matches.len() {
            let path = &results.matches[i].path;
            let n = results.matches[i..].iter().take_while(|m| &m.path == path).count();
            self.rows.push(Row::File(i, n));
            for j in i..i + n {
                self.match_row.push(self.rows.len());
                self.rows.push(Row::Match(j));
            }
            i += n;
        }
        self.results = results;
        self.status = status;
        self.select(0, cx);
    }

    /// What the line under the field says: searching, an error, the count.
    fn status_text(&self) -> Option<(String, bool)> {
        match &self.status {
            Status::Idle => None,
            Status::Searching => Some(("Searching…".into(), false)),
            Status::Error(e) => Some((e.clone(), true)),
            Status::Done => {
                let files = self.rows.iter().filter(|r| matches!(r, Row::File(..))).count();
                let more = if self.results.truncated { "+" } else { "" };
                let n = self.results.matches.len();
                Some((format!("{n}{more} {} in {files} {}", plural(n, "result"), plural(files, "file")), false))
            },
        }
    }

    fn empty_text(&self) -> Option<&'static str> {
        match self.mode {
            Mode::Commands if self.commands.is_empty() => Some("No matching commands"),
            Mode::Search if self.results.matches.is_empty() => match self.status {
                Status::Done => Some("No results found"),
                Status::Idle => Some("Type to search the workspace's files"),
                _ => None,
            },
            _ => None,
        }
    }

    fn render_rows(&mut self, range: Range<usize>, _: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let me = cx.entity().downgrade();
        range
            .filter_map(|ix| {
                let (name, scope, entry) = match self.mode {
                    Mode::Commands => {
                        let c = &COMMANDS[*self.commands.get(ix)?];
                        let scope = template::Scope::new("palette.command")
                            .var("selected", ix == self.selected)
                            .var("title", c.title)
                            .var("key", if c.key.is_empty() { String::new() } else { shortcut(c.key) });
                        ("palette.command", scope, Some(ix))
                    },
                    Mode::Search => match *self.rows.get(ix)? {
                        Row::File(first, count) => {
                            let path = &self.results.matches[first].path;
                            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                            let dir = path.parent().map(|p| p.display().to_string()).unwrap_or_default();
                            let (icon, color) = file_icons::file_icon(&name, cx.theme());
                            let scope = template::Scope::new("palette.file")
                                .var("name", name)
                                .var("dir", dir)
                                .var("count", count.to_string())
                                .slot("icon", style::row_icon("palette.file", icon, color, cx));
                            ("palette.file", scope, None)
                        },
                        Row::Match(m) => {
                            let found = &self.results.matches[m];
                            let parts = segments(found)
                                .into_iter()
                                .map(|(text, hit)| template::Scope::new("part").var("text", text).var("hit", hit))
                                .collect();
                            let scope = template::Scope::new("palette.match")
                                .var("selected", m == self.selected)
                                .var("line", found.line.to_string())
                                .var("mono", cx.theme().mono_font_family.clone())
                                .list("parts", parts);
                            ("palette.match", scope, Some(m))
                        },
                    },
                };
                Some(template::render(
                    name,
                    scope,
                    &mut |frame, _, f| match (frame, f, entry) {
                        ("row", Framed::Box(d), Some(entry)) => d
                            .id(ix)
                            .on_click(on_event(&me, move |this, _: &ClickEvent, window, cx| {
                                this.selected = entry;
                                this.confirm(window, cx);
                            }))
                            .into_any_element(),
                        (_, f, _) => f.into_any_element(),
                    },
                    cx,
                ))
            })
            .collect()
    }
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 { word.to_owned() } else { format!("{word}s") }
}

/// `m`'s line as runs of text, each matched or not: without its indent,
/// and with its start cut (`…`) when the first match is far into it.
fn segments(m: &Match) -> Vec<(String, bool)> {
    let text = &m.text;
    let mut start = text.len() - text.trim_start().len();
    let first = m.ranges.first().map_or(start, |r| r.start);
    let mut out = vec![];
    if first > start + LEAD {
        start = (0..=first - KEEP).rev().find(|&i| text.is_char_boundary(i)).unwrap_or(0);
        out.push(("…".to_owned(), false));
    }
    let mut at = start;
    for r in &m.ranges {
        if r.end <= at {
            continue;
        }
        let from = r.start.max(at);
        if from > at {
            out.push((text[at..from].to_owned(), false));
        }
        out.push((text[from..r.end].to_owned(), true));
        at = r.end;
    }
    if at < text.len() {
        out.push((text[at..].to_owned(), false));
    }
    out
}

impl Render for Palette {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let search = self.mode == Mode::Search;
        let list = uniform_list("palette", if search { self.rows.len() } else { self.commands.len() }, cx.processor(Self::render_rows))
            .track_scroll(&self.scroll)
            .flex_1();
        let (status, error) = self.status_text().filter(|_| search).unwrap_or_default();
        let field = Input::new(self.input()).appearance(false);
        let scope = template::Scope::new("palette")
            .var("search", search)
            .var("case", self.options.case_sensitive)
            .var("word", self.options.whole_word)
            .var("regex", self.options.regex)
            .var("status", status)
            .var("error", error)
            .var("empty", self.empty_text().unwrap_or_default())
            .var("enter_label", if search { "Open" } else { "Run" })
            .var("escape_label", if search { "Back" } else { "Close" })
            .slot("input", field)
            .slot("list", style::el("palette.list", cx).apply(list));
        let me = cx.entity().downgrade();
        let focus = &self.focus;
        template::render(
            "palette",
            scope,
            &mut |name, _, f| match (name, f) {
                ("palette", Framed::Box(d)) => d
                    .track_focus(focus)
                    .key_context("Palette")
                    .capture_action(on_event(&me, |this, _: &MoveUp, _, cx| {
                        this.step(-1, cx);
                        cx.stop_propagation();
                    }))
                    .capture_action(on_event(&me, |this, _: &MoveDown, _, cx| {
                        this.step(1, cx);
                        cx.stop_propagation();
                    }))
                    // The field lets Escape through when it has nothing to undo (a selection, a popup).
                    .on_action(on_event(&me, |this, _: &Escape, window, cx| this.escape(window, cx)))
                    .into_any_element(),
                ("case", Framed::Button(b)) => b
                    .on_click(on_event(&me, |this, _: &ClickEvent, _, cx| this.toggle_option(|q| q.case_sensitive ^= true, cx)))
                    .into_any_element(),
                ("word", Framed::Button(b)) => b
                    .on_click(on_event(&me, |this, _: &ClickEvent, _, cx| this.toggle_option(|q| q.whole_word ^= true, cx)))
                    .into_any_element(),
                ("regex", Framed::Button(b)) => {
                    b.on_click(on_event(&me, |this, _: &ClickEvent, _, cx| this.toggle_option(|q| q.regex ^= true, cx))).into_any_element()
                },
                (_, f) => f.into_any_element(),
            },
            cx,
        )
    }
}

/// Show `palette` in `mode` in a dialog over the window (or switch the open one to `mode`).
pub fn open(palette: &Entity<Palette>, mode: Mode, window: &mut Window, cx: &mut App) {
    if !window.has_active_dialog(cx) {
        let palette = palette.clone();
        window.open_dialog(cx, move |dialog, _, cx| {
            let palette = palette.clone();
            dialog
                .w(style::el("palette", cx).px("dialog_width"))
                .close_button(false)
                // Escape is the palette's: back to the commands first.
                .keyboard(false)
                .content(move |c, _, _| c.child(palette.clone()))
        });
    }
    palette.update(cx, |p, cx| p.show(mode, window, cx));
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tern_fs::search::Match;

    use super::{COMMANDS, KEEP, filter_commands, segments};

    fn titles(q: &str) -> Vec<&'static str> {
        filter_commands(q).into_iter().map(|i| COMMANDS[i].title).collect()
    }

    #[test]
    fn commands_filter_by_words_prefix_first() {
        assert_eq!(titles("").len(), COMMANDS.len());
        assert_eq!(titles("split"), ["Split Right", "Split Down"]);
        assert_eq!(titles("tab new")[..2], ["New Claude Tab", "New Shell Tab"]);
        // A title starting with the query comes before one only containing it.
        assert_eq!(titles("next")[..2], ["Next Tab", "Next Workspace"]);
        assert_eq!(titles("toggle")[0], "Toggle Explorer");
        assert!(titles("zzz").is_empty());
    }

    fn m(text: &str, ranges: &[std::ops::Range<usize>]) -> Match {
        Match { path: PathBuf::from("a"), line: 1, text: text.into(), ranges: ranges.to_vec() }
    }

    #[test]
    fn segments_drop_the_indent_and_mark_matches() {
        let parts = segments(&m("    let foo = foo;", &[8..11, 14..17]));
        let want = [("let ", false), ("foo", true), (" = ", false), ("foo", true), (";", false)];
        assert_eq!(parts, want.map(|(t, h)| (t.to_owned(), h)));
    }

    #[test]
    fn segments_cut_a_long_lead() {
        let line = format!("{}needle end", "x".repeat(100));
        let parts = segments(&m(&line, &[std::ops::Range { start: 100, end: 106 }]));
        assert_eq!(parts[0], ("…".to_owned(), false));
        assert_eq!(parts[1].0.len(), KEEP);
        assert_eq!(parts[2], ("needle".to_owned(), true));
    }
}
