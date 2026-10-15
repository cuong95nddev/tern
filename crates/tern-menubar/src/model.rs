//! What the menu shows, from the daemon's session list.

use std::path::Path;

use tern_mux::proto::SessionInfo;
use tern_term::status::ActivityStatus;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub id: u64,
    pub glyph: &'static str,
    pub label: String,
    /// The working directory, `~`-shortened.
    pub cwd: Option<String>,
    /// Some Tern tab shows it.
    pub open: bool,
    pub exited: bool,
}

impl Row {
    pub fn menu_title(&self) -> String {
        let mut t = format!("{}  {}", self.glyph, self.label);
        if let Some(cwd) = &self.cwd {
            t.push_str(" — ");
            t.push_str(cwd);
        }
        if !self.open {
            t.push_str("  (not open)");
        }
        t
    }
}

/// Needs input, working, done, idle, exited; then by label.
fn rank(s: &SessionInfo) -> u8 {
    match (s.exited, s.status) {
        (true, _) => 4,
        (false, ActivityStatus::Blocked) => 0,
        (false, ActivityStatus::Working) => 1,
        (false, ActivityStatus::Done) => 2,
        (false, ActivityStatus::None) => 3,
    }
}

fn glyph(s: &SessionInfo) -> &'static str {
    match (s.exited, s.status) {
        (true, _) => "×",
        (false, ActivityStatus::Blocked) => "●",
        (false, ActivityStatus::Working) => "◐",
        (false, ActivityStatus::Done) => "✓",
        (false, ActivityStatus::None) => "○",
    }
}

fn label(s: &SessionInfo) -> String {
    s.title.clone().filter(|t| !t.trim().is_empty()).or_else(|| s.program.clone()).unwrap_or_else(|| "shell".into())
}

fn short(cwd: &Path, home: Option<&Path>) -> String {
    match home.and_then(|h| cwd.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => cwd.display().to_string(),
    }
}

/// The menu's rows, the ones waiting for the user first.
pub fn rows(list: &[SessionInfo], home: Option<&Path>) -> Vec<Row> {
    let mut rows: Vec<_> = list
        .iter()
        .map(|s| {
            let row = Row {
                id: s.id,
                glyph: glyph(s),
                label: label(s),
                cwd: s.cwd.as_deref().map(|c| short(c, home)),
                open: s.attached,
                exited: s.exited,
            };
            (rank(s), row)
        })
        .collect();
    rows.sort_by(|(ra, a), (rb, b)| ra.cmp(rb).then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase())).then(a.id.cmp(&b.id)));
    rows.into_iter().map(|(_, r)| r).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IconState {
    /// Running sessions.
    pub count: usize,
    /// One waits for the user.
    pub attention: bool,
}

pub fn icon(list: &[SessionInfo]) -> IconState {
    let running = list.iter().filter(|s| !s.exited);
    IconState { count: running.clone().count(), attention: running.into_iter().any(|s| s.status == ActivityStatus::Blocked) }
}

/// The text beside the icon: the count (none at 0), and a dot when
/// something waits for the user.
pub fn icon_title(s: IconState) -> String {
    match (s.count, s.attention) {
        (0, false) => String::new(),
        (0, true) => "•".into(),
        (n, false) => n.to_string(),
        (n, true) => format!("{n} •"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(id: u64, status: ActivityStatus, title: Option<&str>) -> SessionInfo {
        SessionInfo { id, status, title: title.map(Into::into), attached: true, ..Default::default() }
    }

    #[test]
    fn rows_put_waiting_first() {
        let list = vec![
            s(1, ActivityStatus::None, Some("b idle")),
            s(2, ActivityStatus::Done, Some("done")),
            SessionInfo { exited: true, ..s(3, ActivityStatus::None, Some("a exited")) },
            s(4, ActivityStatus::Working, Some("working")),
            s(5, ActivityStatus::Blocked, Some("needs you")),
            s(6, ActivityStatus::None, Some("a idle")),
        ];
        let ids: Vec<_> = rows(&list, None).iter().map(|r| r.id).collect();
        assert_eq!(ids, [5, 4, 2, 6, 1, 3]);
    }

    #[test]
    fn row_labels() {
        let home = Path::new("/Users/me");
        let list = vec![
            SessionInfo { cwd: Some("/Users/me/src/tern".into()), attached: false, ..s(1, ActivityStatus::None, Some("✳ Fix bug")) },
            SessionInfo { program: Some("vim".into()), cwd: Some("/tmp".into()), ..s(2, ActivityStatus::None, None) },
            SessionInfo { title: Some(String::new()), ..s(3, ActivityStatus::None, None) },
        ];
        let r = rows(&list, Some(home));
        let by = |id| r.iter().find(|r| r.id == id).unwrap();
        assert_eq!(by(1).label, "✳ Fix bug");
        assert_eq!(by(1).cwd.as_deref(), Some("~/src/tern"));
        assert!(!by(1).open);
        assert_eq!(by(1).menu_title(), "○  ✳ Fix bug — ~/src/tern  (not open)");
        assert_eq!(by(2).label, "vim");
        assert_eq!(by(2).menu_title(), "○  vim — /tmp");
        assert_eq!(by(3).label, "shell");
    }

    #[test]
    fn icon_counts_running_and_flags_waiting() {
        assert_eq!(icon(&[]), IconState { count: 0, attention: false });
        let list = vec![s(1, ActivityStatus::Working, None), SessionInfo { exited: true, ..s(2, ActivityStatus::None, None) }];
        assert_eq!(icon(&list), IconState { count: 1, attention: false });
        let list = vec![s(1, ActivityStatus::Blocked, None), s(2, ActivityStatus::None, None)];
        assert_eq!(icon(&list), IconState { count: 2, attention: true });
        assert_eq!(icon_title(IconState { count: 0, attention: false }), "");
        assert_eq!(icon_title(IconState { count: 3, attention: false }), "3");
        assert_eq!(icon_title(IconState { count: 2, attention: true }), "2 •");
    }
}
