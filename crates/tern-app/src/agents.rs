//! Coding-agent CLIs a terminal may be running, told apart by their
//! foreground programs, and their logos (from t3code, MIT: see
//! `assets/agents/LICENSE-t3code`).

use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::Theme;
use gpui_kit::*;
use tern_agent::{AgentId, ScreenState};
use tern_term::status::ActivityStatus;

/// What an agent's row says of its state, as T3 Code's sidebar does: its
/// label, icon (empty for none) and color. `since` is how long it has been
/// in `status`; `rang` is a bell rung while not shown.
pub fn status_look(status: ActivityStatus, rang: bool, since: std::time::Duration, theme: &Theme) -> (String, &'static str, Hsla) {
    match (status, rang) {
        (ActivityStatus::Working, _) => (format!("Working {}", working_label(since)), "circle-dashed", theme.info),
        (ActivityStatus::Blocked, _) => ("Needs input".to_owned(), "message-circle-question-mark", theme.warning),
        (ActivityStatus::Done, _) => ("Done".to_owned(), "circle-check", theme.success),
        (ActivityStatus::None, true) => ("Bell".to_owned(), "bell", theme.warning),
        (ActivityStatus::None, false) => (ago_label(since), "", theme.muted_foreground),
    }
}

/// How full a Claude conversation's context window is: what its status
/// line's input says (see `tern_bridge::statusline`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContextUse {
    /// Percent of the window used, 0 to 100.
    pub percent: f32,
    /// The window's size, in tokens.
    pub size: u64,
}

impl ContextUse {
    /// From a status line's input. Before the first answer the window is
    /// empty: Claude Code then gives no percentage.
    pub fn read(line: &serde_json::Value) -> Option<Self> {
        let w = line.get("context_window")?;
        let size = w.get("context_window_size")?.as_u64().filter(|s| *s > 0)?;
        let percent = w.get("used_percentage").and_then(serde_json::Value::as_f64).unwrap_or(0.);
        Some(Self { percent: percent.clamp(0., 100.) as f32, size })
    }
}

impl From<tern_bridge::context::Reading> for ContextUse {
    fn from(r: tern_bridge::context::Reading) -> Self {
        Self { percent: r.percent, size: r.size }
    }
}

/// A Claude tab's handoff, which its conversation writes when the user
/// asks (to step away) or past its context limit, and which it can go on
/// from in a fresh conversation later, also after a restart.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Handoff {
    #[default]
    None,
    /// The user asked for one; it isn't written yet.
    Asked,
    /// Written at this path.
    Written(String),
}

/// How a Claude tab keeps its conversation's context window in bounds (see
/// `tern_bridge::context`): when `on`, past `limit` percent Claude is asked
/// for a handoff, and with `auto_switch` the tab then goes on in a fresh
/// conversation. Set from the usage bar's context ring.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContextGuard {
    pub on: bool,
    pub limit: u8,
    pub auto_switch: bool,
}

impl Default for ContextGuard {
    fn default() -> Self {
        Self { on: false, limit: 50, auto_switch: true }
    }
}

impl ContextGuard {
    /// What the limit can be set to.
    pub const MIN: u8 = 5;
    pub const MAX: u8 = 100;

    /// From a saved tab's (limit, auto switch), on when there is one.
    pub fn from_saved(saved: Option<(u8, bool)>) -> Self {
        saved.map_or_else(Self::default, |(limit, auto_switch)| Self { on: true, limit: limit.clamp(Self::MIN, Self::MAX), auto_switch })
    }

    /// What a saved tab keeps of it.
    pub fn saved(&self) -> Option<(u8, bool)> {
        self.on.then_some((self.limit, self.auto_switch))
    }

    /// The limit the daemon is told: 0 when off.
    pub fn sent_limit(&self) -> u8 {
        if self.on { self.limit } else { 0 }
    }

    /// The limit typed as `text`, when it is one it can be.
    pub fn typed(text: &str) -> Option<u8> {
        text.trim().parse().ok().filter(|l| (Self::MIN..=Self::MAX).contains(l))
    }

    /// The limit nearest to what was typed as `text` (none for no number).
    pub fn nearest(text: &str) -> Option<u8> {
        text.trim().parse::<u64>().ok().map(|l| l.clamp(Self::MIN.into(), Self::MAX.into()) as u8)
    }
}

/// The size of a context window, short: `200k`, `1M`.
pub fn tokens_label(n: u64) -> String {
    match n {
        n if n >= 1_000_000 && n % 1_000_000 == 0 => format!("{}M", n / 1_000_000),
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1e6),
        n => format!("{}k", n / 1000),
    }
}

/// An agent row's state icon (template `agents.tab`), as Harp's sidebar
/// draws it: an arc turning on its track while it works (`working`), an amber
/// ring around a dot while it waits for you (`needs`), a filled disc with the
/// tick cut out and the title in bold once its turn is done (`done`), a quiet
/// tick once its tab has been seen (`seen`), an empty ring before its first
/// turn (`pending`), an amber dot after a bell rang in it unseen (`rang`), a
/// pulse once its turn is done while only watch loops it left (background
/// shells, Monitors, session crons) keep it working (`monitoring`, as Orca
/// has it); a dot on its corner once its turn is done while it left shells
/// running in the background (`shell_dot`). `time`, how long it has been working or
/// since its turn ended; `tip`, all of it in words, with how full its
/// context window is.
pub fn ring_vars(
    scope: crate::template::Scope,
    view: &crate::terminal_view::TerminalView,
    rang: bool,
    context: Option<ContextUse>,
    theme: &Theme,
) -> crate::template::Scope {
    let (status, shells) = (view.status(), view.shells().len());
    let seen = view.seen_done() && status == ActivityStatus::None;
    let since = view.status_since().elapsed();
    let monitoring = view.monitoring();
    let (word, _, _) = status_look(status, rang, since, theme);
    let word = match view.hook_status() {
        _ if seen && !rang => "Done".to_owned(),
        Some(h) if status == ActivityStatus::Working => {
            let word = if monitoring { format!("Monitoring background tasks {}", working_label(since)) } else { word };
            hook_detail(word, h)
        },
        _ => word,
    };
    let time = match status {
        ActivityStatus::Working => working_label(since),
        _ => ago_label(since),
    };
    // Only a finished turn: while it works its shells are its work.
    let shell_dot = shells > 0 && (status == ActivityStatus::Done || seen);
    let mut tip = match context {
        Some(c) => format!("{word} · context {:.0}% of {}", c.percent, tokens_label(c.size)),
        None => word,
    };
    if shell_dot {
        tip += &format!(" · {shells} background shell{}", if shells == 1 { "" } else { "s" });
    }
    let idle = status == ActivityStatus::None;
    scope
        .var("time", time)
        .var("tip", tip)
        .var("working", status == ActivityStatus::Working && !monitoring)
        .var("monitoring", monitoring)
        .var("needs", status == ActivityStatus::Blocked)
        .var("done", status == ActivityStatus::Done)
        .var("rang", idle && rang)
        .var("seen", idle && !rang && seen)
        .var("pending", idle && !rang && !seen)
        .var("shell_dot", shell_dot)
}

/// [`ring_vars`] for a chat tab: its turn working, waiting for the user,
/// done and not yet looked at, or idle.
pub fn chat_ring_vars(scope: crate::template::Scope, view: &crate::chat_view::ChatView, theme: &Theme) -> crate::template::Scope {
    let status = view.status();
    let since = view.status_since().elapsed();
    let seen = view.seen_done() && status == ActivityStatus::None;
    let (word, _, _) = status_look(status, false, since, theme);
    let tip = if seen { "Done".to_owned() } else { word };
    let time = match status {
        ActivityStatus::Working => working_label(since),
        _ => ago_label(since),
    };
    let idle = status == ActivityStatus::None;
    scope
        .var("time", time)
        .var("tip", tip)
        .var("working", status == ActivityStatus::Working)
        .var("monitoring", false)
        .var("needs", status == ActivityStatus::Blocked)
        .var("done", status == ActivityStatus::Done)
        .var("rang", false)
        .var("seen", idle && seen)
        .var("pending", idle && !seen)
        .var("shell_dot", false)
}

/// `word` with what keeps a Claude tab working besides its own turn.
fn hook_detail(mut word: String, h: &tern_term::claude_status::ClaudeStatus) -> String {
    use tern_term::claude_status::{TurnState, Wakeup};
    let plural = |n: usize, one: &str| if n == 1 { format!("1 {one}") } else { format!("{n} {one}s") };
    if h.main_agent.is_some_and(|m| m.state == TurnState::Done) {
        word += " · turn ended";
    }
    if h.held.agents > 0 {
        word += &format!(" · {} at work", plural(h.held.agents, "subagent"));
    }
    if h.held.shells {
        word += " · background task running";
    }
    if h.held.cron {
        word += " · session cron scheduled";
    }
    match h.wakeup {
        Some(Wakeup::Notification) => word += " · a task ended; Claude is about to be told",
        Some(Wakeup::FinishingTurn) => word += " · answering a background task",
        None => {},
    }
    word
}

/// An agent row's lines under its name, as the settings ask: Claude's
/// latest words (`say`, at most `say_lines` lines) and its tool call after
/// them (`doing`); `recent` when either is shown.
pub fn recent_vars(mut scope: crate::template::Scope, recent: &tern_chat::Recent, cx: &App) -> crate::template::Scope {
    let config = cx.try_global::<crate::settings::AgentsConfig>().copied().unwrap_or_default();
    let say = recent.text.as_ref().filter(|_| config.reply_lines > 0);
    let doing = recent.tool.as_ref().filter(|_| config.show_tool);
    scope = scope.var("recent", say.is_some() || doing.is_some());
    if let Some(t) = say {
        scope = scope.var("say", t.clone()).var("say_lines", config.reply_lines as f64);
    }
    if let Some(t) = doing {
        scope = scope.var("doing", t.clone());
    }
    scope
}

/// What a workspace's line says of its agents that wait for you, in place
/// of its dot: "1 needs you".
pub fn needs_label(n: usize) -> String {
    format!("{n} need{} you", if n == 1 { "s" } else { "" })
}

/// The glyphs of an agent's state (Harp's sidebar icons, 16×16, drawn in
/// the text color).
const GLYPHS: &[(&str, &str)] = &[
    // A disc with the tick cut out of it; `-cut` also cuts room for the
    // background-shell dot on its corner.
    (
        "icons/agent-done.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><mask id="m"><rect width="16" height="16" fill="white"/><path d="M5.1 8.2l2 2 3.8-4.1" fill="none" stroke="black" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/></mask><circle cx="8" cy="8" r="7.2" fill="black" mask="url(#m)"/></svg>"#,
    ),
    (
        "icons/agent-done-cut.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><mask id="m"><rect width="16" height="16" fill="white"/><path d="M5.1 8.2l2 2 3.8-4.1" fill="none" stroke="black" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/><circle cx="15.5" cy="0.5" r="4" fill="black"/></mask><circle cx="8" cy="8" r="7.2" fill="black" mask="url(#m)"/></svg>"#,
    ),
    // A pulse line: watching what it left running.
    (
        "icons/agent-activity.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none"><path d="M1.5 8h3l2-5 3 10 2-5h3" stroke="black" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/></svg>"#,
    ),
    (
        "icons/agent-tick.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none"><path d="M4.2 8.4l2.6 2.6 5-5.4" stroke="black" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/></svg>"#,
    ),
    (
        "icons/agent-ring.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none"><circle cx="8" cy="8" r="6.6" stroke="black" stroke-width="1.3"/></svg>"#,
    ),
    (
        "icons/agent-needs.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none"><circle cx="8" cy="8" r="6.2" stroke="black" stroke-width="1.8"/><circle cx="8" cy="8" r="3.2" fill="black"/></svg>"#,
    ),
    (
        "icons/agent-dot.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><circle cx="8" cy="8" r="3.2" fill="black"/></svg>"#,
    ),
];

/// A state's color: blue while working, amber while waiting for you,
/// green once done (as t3code's sidebar has them).
pub fn status_color(status: ActivityStatus, theme: &Theme) -> Hsla {
    match status {
        ActivityStatus::Working => theme.info,
        ActivityStatus::Blocked => theme.warning,
        ActivityStatus::Done => theme.success,
        ActivityStatus::None => theme.muted_foreground,
    }
}

/// Which state a workspace shows for its agents: the one that most wants you.
pub fn priority(status: ActivityStatus) -> u8 {
    match status {
        ActivityStatus::Blocked => 3,
        ActivityStatus::Working => 2,
        ActivityStatus::Done => 1,
        ActivityStatus::None => 0,
    }
}

/// What a row calls an agent: its tab's title without the status glyph
/// some agents put first (Claude Code's "✳"), else the agent's name.
pub fn display_name(title: &str, agent: &AgentId) -> String {
    let bare = title.trim_start_matches(|c: char| !c.is_alphanumeric()).trim();
    if bare.is_empty() { agent_name(agent) } else { bare.to_owned() }
}

/// How long a turn has run, as T3 Code's sidebar says it: `12s`, `3m`, `1h 5m`.
pub fn working_label(d: std::time::Duration) -> String {
    let s = d.as_secs();
    match s / 60 {
        0 => format!("{s}s"),
        m if m < 60 => format!("{m}m"),
        m => format!("{}h {}m", m / 60, m % 60),
    }
}

/// How long ago something happened, compact: `now`, `5m`, `3h`, `2d`.
pub fn ago_label(d: std::time::Duration) -> String {
    let m = d.as_secs() / 60;
    match m {
        0 => "now".to_owned(),
        m if m < 60 => format!("{m}m"),
        m if m < 24 * 60 => format!("{}h", m / 60),
        m => format!("{}d", m / (24 * 60)),
    }
}

macro_rules! logos {
    ($($name:literal),* $(,)?) => {
        &[$((concat!("agents/", $name, ".svg"), include_bytes!(concat!("../assets/agents/", $name, ".svg")))),*]
    };
}

const LOGOS: &[(&str, &[u8])] = logos![
    "claude",
    "gemini",
    "openai-light",
    "openai-dark",
    "opencode-light",
    "opencode-dark",
    "cursor-light",
    "cursor-dark",
    "grok-light",
    "grok-dark",
    "pi-light",
    "pi-dark",
];

/// Agent `id`'s product name (`Claude Code`), or its id when Tern doesn't
/// know it.
pub fn agent_name(id: &AgentId) -> String {
    tern_agent::registry().get(id.as_str()).map_or_else(|| id.to_string(), |d| d.name.clone())
}

/// What agent `id` is doing, read off its terminal title and the bottom
/// lines of its screen (lowercased): for when it doesn't report it itself.
/// `None` when its screen can't tell.
pub fn read_screen(id: &AgentId, title: &str, bottom: &str) -> Option<ActivityStatus> {
    let registry = tern_agent::registry();
    let state = registry.get(id.as_str())?.screen.read(title, bottom)?;
    Some(match state {
        ScreenState::Working => ActivityStatus::Working,
        ScreenState::Blocked => ActivityStatus::Blocked,
        ScreenState::Idle => ActivityStatus::None,
    })
}

/// Agent `id`'s logo in full color, icon-sized (an image rather than an
/// `Icon`, which tints every path one color); a terminal icon for an
/// agent with no logo Tern has.
pub fn agent_icon(id: &AgentId, theme: &Theme) -> AnyElement {
    let registry = tern_agent::registry();
    let logo = registry.get(id.as_str()).and_then(|d| d.logo.as_ref()).map(|l| if theme.is_dark() { &l.dark } else { &l.light });
    let path = logo.map(|stem| format!("agents/{stem}.svg")).filter(|p| asset(p).is_some());
    match path {
        Some(path) => img(path).size_3p5().flex_shrink_0().into_any_element(),
        None => Icon::new(IconName::SquareTerminal).size_3p5().text_color(theme.muted_foreground).into_any_element(),
    }
}

/// The logo or ring glyph at `path`, for the app's asset source.
pub fn asset(path: &str) -> Option<&'static [u8]> {
    LOGOS.iter().find(|(p, _)| *p == path).map(|(_, b)| *b).or_else(|| GLYPHS.iter().find(|(p, _)| *p == path).map(|(_, s)| s.as_bytes()))
}

pub fn asset_paths(prefix: &str) -> impl Iterator<Item = SharedString> + '_ {
    let glyphs = GLYPHS.iter().map(|(p, _)| *p);
    LOGOS.iter().map(|(p, _)| *p).chain(glyphs).filter(move |p| p.starts_with(prefix)).map(SharedString::from)
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's `test` attribute would shadow the std one.

    #[test]
    fn context_from_the_status_line() {
        use super::ContextUse;
        let line = serde_json::json!({"context_window": {"context_window_size": 1000000, "used_percentage": 42.5}});
        assert_eq!(ContextUse::read(&line), Some(ContextUse { percent: 42.5, size: 1_000_000 }));
        let fresh = serde_json::json!({"context_window": {"context_window_size": 200000, "used_percentage": null}});
        assert_eq!(ContextUse::read(&fresh), Some(ContextUse { percent: 0., size: 200_000 }));
        assert_eq!(ContextUse::read(&serde_json::json!({"model": {}})), Option::None);
        assert_eq!((super::tokens_label(200_000), super::tokens_label(1_000_000)), ("200k".to_owned(), "1M".to_owned()));
    }

    #[test]
    fn context_guard_reads_and_saves() {
        use super::ContextGuard;
        let g = ContextGuard::default();
        assert_eq!((g.sent_limit(), g.saved()), (0, Option::None));
        let on = ContextGuard { on: true, ..g };
        assert_eq!((on.sent_limit(), on.saved()), (50, Some((50, true))));
        assert_eq!(ContextGuard::from_saved(on.saved()), on);
        assert_eq!(ContextGuard::from_saved(Option::None), g);
        assert_eq!(
            (ContextGuard::typed(" 60 "), ContextGuard::typed("3"), ContextGuard::typed("101")),
            (Some(60), Option::None, Option::None)
        );
        assert_eq!(
            (ContextGuard::nearest("3"), ContextGuard::nearest("250"), ContextGuard::nearest("")),
            (Some(5), Some(100), Option::None)
        );
    }
}
