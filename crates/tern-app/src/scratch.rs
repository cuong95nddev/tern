//! Scratch files (IntelliJ-style): throwaway buffers kept as real files in a
//! global scratches folder outside any project, saved as you type, and
//! reopenable later from the tab bar's + menu or the Scratches panel.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use gpui_kit::component::input::{Editor, EditorState, InputEvent};
use gpui_kit::*;

use crate::diff_pane::language_for;
use crate::platform::bind;
use crate::template::{self, Framed, Scope};

const AUTOSAVE_DELAY: Duration = Duration::from_millis(400);

/// File types offered by "New Scratch File": (menu label, extension).
pub const KINDS: &[(&str, &str)] = &[
    ("Plain Text", "txt"),
    ("Markdown", "md"),
    ("JSON", "json"),
    ("YAML", "yaml"),
    ("Shell Script", "sh"),
    ("Python", "py"),
    ("JavaScript", "js"),
    ("TypeScript", "ts"),
    ("Rust", "rs"),
    ("Go", "go"),
    ("SQL", "sql"),
    ("HTML", "html"),
    ("CSS", "css"),
];

/// The kinds as the `kinds` list of `menu.new_scratch`.
pub fn kind_entries() -> Vec<crate::template::Scope> {
    KINDS.iter().map(|(label, ext)| crate::template::Scope::default().var("label", *label).var("ext", *ext)).collect()
}

actions!(scratch, [SaveScratch]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([bind("cmd-s", SaveScratch, Some("Scratch"))]);
}

/// `scratches` in Tern's data folder (see `dirs::data`), shared by all projects.
pub fn dir() -> PathBuf {
    crate::dirs::data().join("scratches")
}

/// Create the next free `scratch.EXT`, `scratch_1.EXT`, ... in `dir`.
pub fn create(dir: &Path, ext: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    for n in 0.. {
        let name = if n == 0 { format!("scratch.{ext}") } else { format!("scratch_{n}.{ext}") };
        let path = dir.join(name);
        // `create_new` so two windows never pick the same name.
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

/// Scratch files in `dir`, most recently modified first.
pub fn list(dir: &Path) -> Vec<PathBuf> {
    list_with_times(dir).into_iter().map(|(p, _)| p).collect()
}

/// [`list`] with each file's modification time.
pub fn list_with_times(dir: &Path) -> Vec<(PathBuf, Option<SystemTime>)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return vec![] };
    let mut files: Vec<_> = entries
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .filter_map(|e| {
            let m = e.metadata().ok().filter(|m| m.is_file())?;
            Some((e.path(), m.modified().ok()))
        })
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.1));
    files
}

pub struct Scratch {
    path: PathBuf,
    name: SharedString,
    editor: Entity<EditorState>,
    dirty: bool,
    message: Option<SharedString>,
    _subs: Vec<Subscription>,
    _autosave: Option<Task<()>>,
}

impl Focusable for Scratch {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
    }
}

impl Scratch {
    pub fn open(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let lang = language_for(&path);
        let editor = cx.new(|cx| {
            let mut ed = EditorState::new(window, cx).line_number(true).soft_wrap(false).placeholder("Scratch…");
            ed.set_highlighter(lang, cx);
            ed.set_value(text, window, cx);
            ed
        });
        let sub = cx.subscribe(&editor, |this, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                this.changed(cx);
            }
        });
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default().into();
        Self { path, name, editor, dirty: false, message: None, _subs: vec![sub], _autosave: None }
    }

    pub fn title(&self) -> &SharedString {
        &self.name
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Save shortly after typing stops.
    fn changed(&mut self, cx: &mut Context<Self>) {
        self.dirty = true;
        self._autosave = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(AUTOSAVE_DELAY).await;
            let _ = this.update(cx, |this, cx| this.flush(cx));
        }));
        cx.notify();
    }

    /// Write pending edits now.
    pub fn flush(&mut self, cx: &mut Context<Self>) {
        self._autosave = None;
        if !self.dirty {
            return;
        }
        let text = self.editor.read(cx).value().to_string();
        match crate::window_state::write_atomic(&self.path, text.as_bytes()) {
            Ok(()) => {
                self.dirty = false;
                self.message = None;
            },
            Err(e) => self.message = Some(format!("Save failed: {e}").into()),
        }
        cx.notify();
    }

    /// The tab is closing: save, and drop the file if it is empty so
    /// abandoned scratches don't pile up.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.flush(cx);
        if self.editor.read(cx).value().trim().is_empty() {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    /// Rename the file within the scratches folder; the type follows the
    /// new extension.
    pub fn rename(&mut self, name: &str, cx: &mut Context<Self>) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() || name.starts_with('.') || name.contains('/') {
            return Err(format!("Invalid file name {name:?}"));
        }
        let new = self.path.with_file_name(name);
        if new == self.path {
            return Ok(());
        }
        if new.exists() {
            return Err(format!("{name} already exists"));
        }
        self.flush(cx);
        std::fs::rename(&self.path, &new).map_err(|e| format!("Rename failed: {e}"))?;
        let lang = language_for(&new);
        self.editor.update(cx, |ed, cx| ed.set_highlighter(lang, cx));
        self.name = name.to_owned().into();
        self.path = new;
        cx.notify();
        Ok(())
    }

    fn save(&mut self, _: &SaveScratch, _: &mut Window, cx: &mut Context<Self>) {
        self.flush(cx);
    }

    #[cfg(debug_assertions)]
    pub fn set_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |ed, cx| ed.set_value(text.to_owned(), window, cx));
        // `set_value` emits no Change, so schedule the save as typing would.
        self.changed(cx);
    }
}

impl Render for Scratch {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let scope = Scope::new("scratch")
            .var("message", self.message.clone().unwrap_or_default())
            .slot("editor", Editor::new(&self.editor).bordered(false).h_full());
        let mut frames = |name: &str, _, f| match (name, f) {
            ("scratch", Framed::Box(d)) => d.key_context("Scratch").on_action(cx.listener(Self::save)).into_any_element(),
            (_, f) => f.into_any_element(),
        };
        template::render("scratch", scope, &mut frames, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::{create, list};

    #[test]
    fn create_picks_next_free_name_and_list_finds_them() {
        let dir = std::env::temp_dir().join(format!("tern-scratch-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = create(&dir, "txt").unwrap();
        let b = create(&dir, "txt").unwrap();
        let c = create(&dir, "py").unwrap();
        assert_eq!(a.file_name().unwrap(), "scratch.txt");
        assert_eq!(b.file_name().unwrap(), "scratch_1.txt");
        assert_eq!(c.file_name().unwrap(), "scratch.py");
        let mut found = list(&dir);
        found.sort();
        let mut want = vec![a, b, c];
        want.sort();
        assert_eq!(found, want);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
