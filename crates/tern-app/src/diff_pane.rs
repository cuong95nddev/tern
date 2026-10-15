//! Diff of one file vs HEAD. Shared by the Changes preview popup, diff tabs
//! and the viewer's diff mode.

use std::path::{Path, PathBuf};

use gpui_kit::component::ActiveTheme;
use gpui_kit::component::diff::{Diff, DiffFile, DiffMode, DiffState};
use gpui_kit::component::highlighter::Language;
use gpui_kit::*;

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
}

impl Focusable for DiffPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl DiffPane {
    pub fn new(path: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut this = Self { path, diff: None, message: Some("Loading…".into()), focus: cx.focus_handle(), _load: None };
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
                        None => this.diff = Some(cx.new(|cx| DiffState::new(files, cx).with_mode(DiffMode::Unified))),
                    }
                }
                cx.notify();
            });
        }));
    }

    /// Paths changed on disk: reload if this file or git metadata (commit,
    /// stage, checkout) is among them.
    pub fn files_changed(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        if paths.iter().any(|p| p == &self.path || p.components().any(|c| c.as_os_str() == ".git")) {
            self.reload(cx);
        }
    }
}

impl Render for DiffPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body: AnyElement = match (&self.message, &self.diff) {
            (None, Some(diff)) => Diff::new(diff).line_number(true).syntax_highlight(true).into_any_element(),
            (msg, _) => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(cx.theme().muted_foreground)
                .child(msg.clone().unwrap_or_default())
                .into_any_element(),
        };
        div().track_focus(&self.focus).size_full().min_h_0().bg(cx.theme().background).child(body)
    }
}
