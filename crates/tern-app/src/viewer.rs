//! File tab: preview / light editor (tree-sitter) and diff vs HEAD.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use gpui_kit::component::ActiveTheme;
use gpui_kit::component::input::{Editor, EditorState, InputEvent, Position};
use gpui_kit::component::text::{TextView, TextViewState};
use gpui_kit::*;

use crate::diff_pane::{DiffPane, language_for};
use crate::platform::bind;
use crate::template::{Framed, on_event};

const MAX_FILE_BYTES: u64 = 20 * 1024 * 1024;

actions!(viewer, [Save, ToggleDiff, TogglePreview, OpenExternal, ToggleWordWrap]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        bind("cmd-s", Save, Some("Viewer")),
        bind("cmd-shift-d", ToggleDiff, None),
        bind("cmd-shift-v", TogglePreview, None),
        bind("cmd-shift-o", OpenExternal, None),
        // Only where an editor has focus: in a terminal, Option-Z is typed.
        bind("alt-z", ToggleWordWrap, Some("Viewer")),
        bind("alt-z", ToggleWordWrap, Some("Scratch")),
    ]);
}

/// Flip word wrap for every file and scratch editor. Each one picks it up
/// with [`sync_word_wrap`] when next drawn.
pub fn toggle_word_wrap() {
    crate::window_state::set_word_wrap(!crate::window_state::word_wrap());
}

/// Bring `editor` to the shared word wrap choice; `wrap` is what it was last set to.
pub fn sync_word_wrap(editor: &Entity<EditorState>, wrap: &mut bool, window: &mut Window, cx: &mut App) {
    let want = crate::window_state::word_wrap();
    if *wrap != want {
        *wrap = want;
        editor.update(cx, |ed, cx| ed.set_soft_wrap(want, window, cx));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Edit,
    Diff,
    /// Rendered Markdown of the editor's current text (unsaved edits included).
    Preview,
}

fn is_markdown(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| ["md", "markdown", "mdx"].iter().any(|m| e.eq_ignore_ascii_case(m)))
}

fn image_format(path: &Path) -> Option<ImageFormat> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => ImageFormat::Png,
        "jpg" | "jpeg" => ImageFormat::Jpeg,
        "webp" => ImageFormat::Webp,
        "gif" => ImageFormat::Gif,
        "svg" => ImageFormat::Svg,
        "bmp" => ImageFormat::Bmp,
        "tif" | "tiff" => ImageFormat::Tiff,
        "ico" => ImageFormat::Ico,
        "pbm" | "pgm" | "ppm" | "pnm" => ImageFormat::Pnm,
        _ => return None,
    })
}

pub struct ViewerState {
    path: Option<PathBuf>,
    mode: Mode,
    unsaved: Option<String>,
}

#[derive(Debug, Clone)]
pub enum ViewerEvent {
    /// A relative link in the Markdown preview points at this file.
    OpenFile(PathBuf),
}

pub struct Viewer {
    root: PathBuf,
    path: Option<PathBuf>,
    editor: Entity<EditorState>,
    /// The editor's soft wrap, kept in step with [`crate::window_state::word_wrap`].
    wrap: bool,
    diff: Option<Entity<DiffPane>>,
    preview: Entity<TextViewState>,
    /// Set when the open file is an image; the body shows it instead of the editor.
    image: Option<Arc<Image>>,
    mode: Mode,
    dirty: bool,
    /// Ignore the Change event caused by our own `set_value`.
    loading: bool,
    /// The editor's highlighter language: setting it again rebuilds the
    /// highlighter, so a reload of the same file keeps it.
    language: Option<SharedString>,
    message: Option<SharedString>,
    /// Where to put the cursor once the file being loaded is in.
    go_to: Option<Position>,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
    _load: Option<Task<()>>,
}

impl EventEmitter<ViewerEvent> for Viewer {}

impl Focusable for Viewer {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

enum Loaded {
    Text(String),
    Image(Arc<Image>),
    Message(String),
}

fn load_file(path: &Path) -> Loaded {
    use std::io::Read;
    // Open first, then check the handle: avoids size TOCTOU and refuses FIFOs/
    // devices (a read on those could block or never end).
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => return Loaded::Message(format!("Cannot open: {e}")),
    };
    match file.metadata() {
        Ok(m) if !m.is_file() => return Loaded::Message("Not a regular file".into()),
        Ok(m) if m.len() > MAX_FILE_BYTES => {
            return Loaded::Message(format!("File too large to preview ({} MB)", m.len() / 1_048_576));
        },
        Err(e) => return Loaded::Message(format!("Cannot open: {e}")),
        _ => {},
    }
    let mut bytes = Vec::new();
    if let Err(e) = file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes) {
        return Loaded::Message(format!("Cannot open: {e}"));
    }
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Loaded::Message("File too large to preview".into());
    }
    if let Some(format) = image_format(path) {
        return Loaded::Image(Arc::new(Image::from_bytes(format, bytes)));
    }
    if bytes.iter().take(8192).any(|&b| b == 0) {
        return Loaded::Message("Binary file".into());
    }
    Loaded::Text(String::from_utf8_lossy(&bytes).into_owned())
}

/// Resolve `.` and `..` lexically, so a linked path compares equal to the
/// same file opened from the tree.
pub fn normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {},
            Component::ParentDir => {
                out.pop();
            },
            c => out.push(c),
        }
    }
    out
}

/// The file a relative Markdown link in `dir` points to (`/x` is relative to
/// `root`), if it is a file inside `root`. Containment is checked on the
/// resolved path, so a symlink in the workspace can't lead outside it; the
/// returned path stays lexical, to match the same file opened from the tree.
fn link_target(root: &Path, dir: &Path, target: &str) -> Option<PathBuf> {
    let path = normalize(&match target.strip_prefix('/') {
        Some(rest) => root.join(rest),
        None => dir.join(target),
    });
    let real = path.canonicalize().ok()?;
    (path.starts_with(root) && real.starts_with(root.canonicalize().ok()?) && real.is_file()).then_some(path)
}

impl Viewer {
    pub fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let wrap = crate::window_state::word_wrap();
        let editor = cx.new(|cx| EditorState::new(window, cx).line_number(true).soft_wrap(wrap));
        let sub = cx.subscribe_in(&editor, window, |this, _, ev: &InputEvent, _, cx| {
            if matches!(ev, InputEvent::Change) && !this.loading && !this.dirty {
                this.dirty = true;
                cx.notify();
            }
        });
        let preview = cx.new(|cx| TextViewState::markdown("", cx).scrollable(true));
        Self {
            root,
            path: None,
            editor,
            wrap,
            diff: None,
            preview,
            image: None,
            mode: Mode::Edit,
            dirty: false,
            loading: false,
            language: None,
            go_to: None,
            message: None,
            focus: cx.focus_handle(),
            // The decoded image stays in the app's asset cache until removed.
            _subs: vec![sub, cx.on_release(|this: &mut Self, cx| this.set_image(None, cx))],
            _load: None,
        }
    }

    pub fn open(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        // Opening always shows the file itself, even if this file was left in Diff.
        let mode = if is_markdown(&path) { Mode::Preview } else { Mode::Edit };
        if self.path.as_ref() == Some(&path) && !self.dirty {
            self.mode = mode;
            self.message = None;
            return self.reload(window, cx);
        }
        self.mode = mode;
        self.path = Some(path);
        self.dirty = false;
        self.diff = None;
        self.set_image(None, cx);
        self.reload(window, cx);
    }

    /// Show `image` (or none), dropping the one shown before from the app's
    /// asset cache, which keeps every image decoded for good otherwise.
    fn set_image(&mut self, image: Option<Arc<Image>>, cx: &mut App) {
        if let Some(old) = self.image.take()
            && image.as_ref().is_none_or(|new| new.id != old.id)
        {
            old.remove_asset(cx);
        }
        self.image = image;
    }

    /// Put the cursor on 1-based `line` (and `col`) once the file loads, or
    /// now if it has unsaved edits (it isn't reloaded then).
    pub fn go_to(&mut self, line: u32, col: Option<u32>, window: &mut Window, cx: &mut Context<Self>) {
        let pos = Position::new(line.saturating_sub(1), col.unwrap_or(1).saturating_sub(1));
        if !self.dirty {
            self.go_to = Some(pos);
            return;
        }
        self.mode = Mode::Edit;
        self.editor.update(cx, |ed, cx| ed.set_cursor_position(pos, window, cx));
        cx.notify();
    }

    /// Reload from disk keeping the cursor; skipped when there are unsaved edits.
    pub fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else { return };
        if self.dirty {
            return;
        }
        let lang = language_for(&path);
        self._load = Some(cx.spawn_in(window, async move |this, cx| {
            let p = path.clone();
            let loaded = cx.background_spawn(async move { load_file(&p) }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.path.as_ref() != Some(&path) {
                    return;
                }
                match loaded {
                    Loaded::Image(image) => {
                        this.message = None;
                        this.set_image(Some(image), cx);
                    },
                    Loaded::Text(text) => {
                        this.message = None;
                        this.set_image(None, cx);
                        this.loading = true;
                        // Restoring the cursor focuses the editor; a reload
                        // must not take focus from where it is.
                        let focused = window.focused(cx);
                        let lang = (this.language.as_ref() != Some(&lang)).then(|| {
                            this.language = Some(lang.clone());
                            lang
                        });
                        this.editor.update(cx, |ed, cx| {
                            if let Some(lang) = lang {
                                ed.set_highlighter(lang, cx);
                            }
                            if ed.value().as_ref() != text {
                                let cursor = ed.cursor_position();
                                ed.set_value(text, window, cx);
                                ed.set_cursor_position(cursor, window, cx);
                            }
                        });
                        if let Some(f) = focused {
                            window.focus(&f, cx);
                        }
                        if let Some(pos) = this.go_to.take() {
                            this.mode = Mode::Edit;
                            this.editor.update(cx, |ed, cx| ed.set_cursor_position(pos, window, cx));
                            // The editor scrolls to the cursor only once it has a
                            // layout of the new text: a tab just opened gets it
                            // when drawn, a frame or two from now.
                            cx.on_next_frame(window, move |_, window, cx| {
                                cx.on_next_frame(window, move |this, window, cx| {
                                    this.editor.update(cx, |ed, cx| ed.set_cursor_position(pos, window, cx));
                                });
                            });
                        }
                        this.loading = false;
                    },
                    Loaded::Message(m) => this.message = Some(m.into()),
                }
                match this.mode {
                    Mode::Diff => this.show_diff(cx),
                    Mode::Preview => this.sync_preview(cx),
                    Mode::Edit => {},
                }
                cx.notify();
            });
        }));
    }

    /// Paths changed on disk: reload if the open file is among them.
    pub fn files_changed(&mut self, paths: &[PathBuf], window: &mut Window, cx: &mut Context<Self>) {
        if let Some(p) = &self.path
            && paths.iter().any(|c| c == p)
        {
            self.reload(window, cx);
        }
    }

    pub fn show_diff(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else { return };
        self.mode = Mode::Diff;
        match &self.diff {
            Some(d) if d.read(cx).path() == path => d.update(cx, |d, cx| d.reload(cx)),
            _ => self.diff = Some(cx.new(|cx| DiffPane::new(path, cx))),
        }
        cx.notify();
    }

    pub fn toggle_diff(&mut self, _: &ToggleDiff, window: &mut Window, cx: &mut Context<Self>) {
        match self.mode {
            Mode::Edit => self.show_diff(cx),
            Mode::Preview => self.show_diff(cx),
            Mode::Diff => {
                self.mode = if self.path.as_deref().is_some_and(is_markdown) { Mode::Preview } else { Mode::Edit };
                self.message = None;
                self.reload(window, cx);
                cx.notify();
            },
        }
    }

    fn toggle_word_wrap(&mut self, _: &ToggleWordWrap, _: &mut Window, cx: &mut Context<Self>) {
        toggle_word_wrap();
        cx.notify();
    }

    fn sync_preview(&mut self, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).value();
        self.preview.update(cx, |p, cx| p.set_text(&text, cx));
    }

    pub fn toggle_preview(&mut self, _: &TogglePreview, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = &self.path else { return };
        if self.mode == Mode::Preview {
            self.mode = Mode::Edit;
        } else if is_markdown(path) {
            self.mode = Mode::Preview;
            self.message = None;
            // Leaving Diff: the editor may hold stale text, reload first.
            self.reload(window, cx);
            self.sync_preview(cx);
        }
        cx.notify();
    }

    /// Web links go to the browser; relative links open the target file here.
    fn open_link(&mut self, url: &str, cx: &mut Context<Self>) {
        let lower = url.to_ascii_lowercase();
        if ["http://", "https://", "mailto:"].iter().any(|s| lower.starts_with(s)) {
            cx.open_url(url);
            return;
        }
        // Other schemes (file:, javascript:, ...) are ignored; so are pure anchors.
        let target = url.split(['#', '?']).next().unwrap_or("");
        if target.is_empty() || target.contains(':') {
            return;
        }
        let Some(dir) = self.path.as_ref().and_then(|p| p.parent()) else { return };
        if let Some(path) = link_target(&self.root, dir, target) {
            cx.emit(ViewerEvent::OpenFile(path));
        }
    }

    fn save(&mut self, _: &Save, _: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else { return };
        let text = self.editor.read(cx).value().to_string();
        match std::fs::write(&path, text) {
            Ok(()) => self.dirty = false,
            Err(e) => self.message = Some(format!("Save failed: {e}").into()),
        }
        cx.notify();
    }

    pub fn open_external(&mut self, _: &OpenExternal, _: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else { return };
        let line = self.editor.read(cx).cursor_position().line + 1;
        let target = format!("{}:{line}", path.display());
        // Prefer the `code` CLI (jumps to line); fall back to Cursor, then the
        // app bundle (macOS) or the file's default app. On Windows the CLIs
        // are batch files, which need their extension.
        let clis: &[&str] = if cfg!(windows) { &["code.cmd", "cursor.cmd"] } else { &["code", "cursor"] };
        let spawned = clis.iter().any(|bin| {
            let mut c = Command::new(bin);
            #[cfg(windows)]
            std::os::windows::process::CommandExt::creation_flags(&mut c, 0x0800_0000); // CREATE_NO_WINDOW
            c.arg("-g").arg(&target).spawn().is_ok()
        });
        if !spawned {
            if cfg!(target_os = "macos") {
                let _ = Command::new("open").args(["-a", "Visual Studio Code"]).arg(&path).spawn();
            } else {
                cx.open_with_system(&path);
            }
        }
    }

    /// The open file was renamed or moved to `path`: follow it, keeping
    /// unsaved edits.
    pub fn set_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let lang = language_for(&path);
        self.language = Some(lang.clone());
        self.editor.update(cx, |ed, cx| ed.set_highlighter(lang, cx));
        self.path = Some(path);
        self.reload(window, cx);
        cx.notify();
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// What a rebuilt viewer needs to show the same thing.
    pub fn state(&self, cx: &App) -> ViewerState {
        ViewerState { path: self.path.clone(), mode: self.mode, unsaved: self.dirty.then(|| self.editor.read(cx).value().to_string()) }
    }

    /// Show `state` (from another viewer), unsaved edits included.
    pub fn restore(&mut self, state: ViewerState, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = state.path else { return };
        self.mode = state.mode;
        self.path = Some(path.clone());
        let Some(text) = state.unsaved else { return self.reload(window, cx) };
        self.loading = true;
        let lang = language_for(&path);
        self.language = Some(lang.clone());
        self.editor.update(cx, |ed, cx| {
            ed.set_highlighter(lang, cx);
            ed.set_value(text, window, cx);
        });
        self.loading = false;
        self.dirty = true;
        match self.mode {
            Mode::Diff => self.show_diff(cx),
            Mode::Preview => self.sync_preview(cx),
            Mode::Edit => {},
        }
        cx.notify();
    }

    #[cfg(debug_assertions)]
    pub fn debug_state(&self) -> String {
        format!("{:?}:{}", self.mode, self.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default())
    }
}

impl Render for Viewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        sync_word_wrap(&self.editor, &mut self.wrap, window, cx);
        let name = self.path.as_ref().map(|p| p.strip_prefix(&self.root).unwrap_or(p).display().to_string()).unwrap_or_default();
        let diff_mode = self.mode == Mode::Diff;
        let markdown = self.path.as_deref().is_some_and(is_markdown);
        let is_image = self.image.is_some();
        let me = cx.entity().downgrade();
        let scope = crate::template::Scope::new("viewer")
            .var("name", name)
            .var("dirty", self.dirty)
            .var("markdown", markdown)
            .var("image", is_image)
            .var("preview", self.mode == Mode::Preview)
            .var("diff", diff_mode)
            .var("wrap", self.wrap)
            .var("wrap_key", crate::platform::shortcut("⌥Z"))
            .var("preview_key", crate::platform::shortcut("⌘⇧V"))
            .var("diff_key", crate::platform::shortcut("⌘⇧D"))
            .var("open_external_key", crate::platform::shortcut("⌘⇧O"));
        // What the body shows: one slot (or the message) of the template's.
        let scope = if diff_mode && let Some(diff) = &self.diff {
            scope.slot("diff", diff.clone())
        } else if let Some(msg) = &self.message {
            scope.var("message", msg.clone())
        } else if let Some(image) = &self.image {
            let muted = cx.theme().muted_foreground;
            scope.slot(
                "image",
                img(image.clone())
                    .size_full()
                    .object_fit(ObjectFit::ScaleDown)
                    .with_fallback(move || div().text_color(muted).child("Cannot decode image").into_any_element()),
            )
        } else if self.mode == Mode::Preview {
            let viewer = me.clone();
            scope.slot(
                "preview",
                TextView::new(&self.preview).scrollable(true).size_full().px_4().py_2().on_link_click(move |url, _, _, cx| {
                    let url = url.clone();
                    let _ = viewer.update(cx, |v, cx| v.open_link(&url, cx));
                }),
            )
        } else {
            scope.slot("editor", Editor::new(&self.editor).bordered(false).h_full())
        };
        let focus = self.focus.clone();
        crate::template::render(
            "viewer",
            scope,
            &mut |name, _, f| match (name, f) {
                ("viewer", Framed::Box(d)) => d
                    .key_context("Viewer")
                    .track_focus(&focus)
                    .on_action(cx.listener(Self::save))
                    .on_action(cx.listener(Self::toggle_diff))
                    .on_action(cx.listener(Self::toggle_preview))
                    .on_action(cx.listener(Self::open_external))
                    .on_action(cx.listener(Self::toggle_word_wrap))
                    .into_any_element(),
                ("preview", Framed::Button(b)) => {
                    b.on_click(on_event(&me, |this, _: &ClickEvent, w, cx| this.toggle_preview(&TogglePreview, w, cx))).into_any_element()
                },
                ("diff", Framed::Button(b)) => {
                    b.on_click(on_event(&me, |this, _: &ClickEvent, w, cx| this.toggle_diff(&ToggleDiff, w, cx))).into_any_element()
                },
                ("wrap", Framed::Button(b)) => {
                    b.on_click(on_event(&me, |this, _: &ClickEvent, w, cx| this.toggle_word_wrap(&ToggleWordWrap, w, cx))).into_any_element()
                },
                ("open_external", Framed::Button(b)) => {
                    b.on_click(on_event(&me, |this, _: &ClickEvent, w, cx| this.open_external(&OpenExternal, w, cx))).into_any_element()
                },
                (_, f) => f.into_any_element(),
            },
            cx,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{Path, PathBuf, image_format, is_markdown, normalize};

    #[test]
    fn normalize_resolves_dots() {
        assert_eq!(normalize(Path::new("/r/docs/../README.md")), PathBuf::from("/r/README.md"));
        assert_eq!(normalize(Path::new("/r/./a/./b.md")), PathBuf::from("/r/a/b.md"));
        assert_eq!(normalize(Path::new("/r/../../x")), PathBuf::from("/x"));
    }

    #[test]
    #[cfg(unix)]
    fn links_stay_inside_the_root() {
        use super::link_target;
        let base = std::env::temp_dir().join(format!("tern-viewer-links-{}", std::process::id()));
        let (root, outside) = (base.join("root"), base.join("outside"));
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(root.join("README.md"), "").unwrap();
        std::fs::write(outside.join("secret.txt"), "").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("out")).unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("secret.txt")).unwrap();
        let docs = root.join("docs");

        assert_eq!(link_target(&root, &docs, "../README.md"), Some(root.join("README.md")));
        assert_eq!(link_target(&root, &docs, "/README.md"), Some(root.join("README.md")));
        assert_eq!(link_target(&root, &docs, "../../outside/secret.txt"), None);
        assert_eq!(link_target(&root, &docs, "../out/secret.txt"), None);
        assert_eq!(link_target(&root, &docs, "/secret.txt"), None);
        assert_eq!(link_target(&root, &docs, "missing.md"), None);
        assert_eq!(link_target(&root, &docs, ".."), None);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn markdown_extensions() {
        assert!(is_markdown(Path::new("a/README.md")));
        assert!(is_markdown(Path::new("x.MDX")));
        assert!(!is_markdown(Path::new("main.rs")));
        assert!(!is_markdown(Path::new("md")));
    }

    #[test]
    fn image_extensions() {
        assert!(image_format(Path::new("icon.PNG")).is_some());
        assert!(image_format(Path::new("a/b.svg")).is_some());
        assert!(image_format(Path::new("photo.jpeg")).is_some());
        assert!(image_format(Path::new("main.rs")).is_none());
        assert!(image_format(Path::new("png")).is_none());
    }
}
