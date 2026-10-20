//! Diff of one file vs HEAD. Shared by the Changes preview popup, diff tabs
//! and the viewer's diff mode.

use std::path::{Path, PathBuf};

use gpui_kit::component::diff::{Diff, DiffFile, DiffMode, DiffState};
use gpui_kit::component::highlighter::Language;
use gpui_kit::*;
use tern_fs::ChangeBatch;

use crate::template::{self, Framed, Scope, on_event};

/// Whether diffs show side by side (`editor.diff_split`).
pub struct DiffSplit(pub bool);

impl Global for DiffSplit {}

fn split_mode(cx: &App) -> DiffMode {
    if cx.try_global::<DiffSplit>().is_some_and(|s| s.0) { DiffMode::Split } else { DiffMode::Unified }
}

pub fn language_for(path: &Path) -> SharedString {
    let ext = path.extension().and_then(|e| e.to_str()).or_else(|| path.file_name().and_then(|n| n.to_str())).unwrap_or("");
    Language::from_str(ext).name().into()
}

pub struct DiffPane {
    path: PathBuf,
    diff: Option<Entity<DiffState>>,
    message: Option<SharedString>,
    focus: FocusHandle,
    _load: Option<Task<()>>,
    _settings: Subscription,
}

impl Focusable for DiffPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl DiffPane {
    pub fn new(path: PathBuf, cx: &mut Context<Self>) -> Self {
        // Every diff follows the setting, whichever one changed it.
        let settings = cx.observe_global::<DiffSplit>(|this, cx| {
            let mode = split_mode(cx);
            if let Some(diff) = &this.diff {
                diff.update(cx, |d, cx| d.set_mode(mode, cx));
            }
        });
        let mut this =
            Self { path, diff: None, message: Some("Loading…".into()), focus: cx.focus_handle(), _load: None, _settings: settings };
        this.reload(cx);
        this
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let path = self.path.clone();
        let lang = language_for(&path);
        self._load = Some(cx.spawn(async move |this, cx| {
            let patch = cx.background_spawn(async move { tern_fs::git::diff_vs_head(&path) }).await;
            let _ = this.update(cx, |this, cx| {
                let files = patch
                    .as_deref()
                    .and_then(|p| DiffFile::parse(p).ok())
                    .unwrap_or_default()
                    .into_iter()
                    .map(|f| f.with_language(lang.clone()))
                    .collect::<Vec<_>>();
                if files.is_empty() {
                    this.diff = None;
                    this.message = Some("No changes vs HEAD".into());
                } else {
                    this.message = None;
                    match &this.diff {
                        Some(d) => d.update(cx, |d, cx| d.set_files(files, cx)),
                        None => {
                            let mode = split_mode(cx);
                            this.diff = Some(cx.new(|cx| DiffState::new(files, cx).with_mode(mode)));
                        },
                    }
                }
                cx.notify();
            });
        }));
    }

    fn toggle_split(&mut self, cx: &mut Context<Self>) {
        let split = split_mode(cx) == DiffMode::Unified;
        crate::settings::set_value(&["editor", "diff_split"], split.then_some(true.into()), cx);
    }

    /// Paths changed on disk: reload if this file is among them, or git
    /// metadata (commit, stage, checkout) of a repo holding it changed.
    pub fn files_changed(&mut self, batch: &ChangeBatch, cx: &mut Context<Self>) {
        if batch.paths.contains(&self.path) || batch.git_repos.iter().any(|r| self.path.starts_with(r)) {
            self.reload(cx);
        }
    }
}

impl Render for DiffPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let scope = Scope::new("diff_pane");
        let scope = match (&self.message, &self.diff) {
            (None, Some(diff)) => {
                let me = cx.entity().downgrade();
                let split = diff.read(cx).mode() == DiffMode::Split;
                scope.slot(
                    "diff",
                    // Side by side, long lines wrap so each side keeps half the
                    // width, as on GitHub; one shared horizontal scroll would
                    // push the right side out of view.
                    Diff::new(diff).line_number(true).syntax_highlight(true).soft_wrap(split).header_title_suffix(move |_, _, cx| {
                        template::render(
                            "diff_pane.mode",
                            Scope::new("diff_pane.mode").var("split", split),
                            &mut |name, _, f| match (name, f) {
                                ("mode", Framed::Button(b)) => {
                                    b.on_click(on_event(&me, |this: &mut Self, _: &ClickEvent, _, cx| this.toggle_split(cx))).into_any_element()
                                },
                                (_, f) => f.into_any_element(),
                            },
                            cx,
                        )
                    }),
                )
            },
            (msg, _) => scope.var("message", msg.clone().unwrap_or_default()),
        };
        let focus = &self.focus;
        template::render(
            "diff_pane",
            scope,
            &mut |name, _, f| match (name, f) {
                ("diff_pane", Framed::Box(d)) => d.track_focus(focus).into_any_element(),
                (_, f) => f.into_any_element(),
            },
            cx,
        )
    }
}
