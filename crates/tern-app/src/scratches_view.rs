//! Scratches panel: every file in the global scratches folder, newest first,
//! live-updated by an fs watcher. Clicking a row opens it in a tab.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Icon, IconName};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tern_fs::{ChangeBatch, FsWatcher};

use crate::file_icons;
use crate::{scratch, style};

/// The element a row is drawn as ([`style`]).
const ROW: &str = "scratches.row";

#[derive(Debug, Clone)]
pub enum ScratchesEvent {
    Open(PathBuf),
    /// Create a scratch file with this extension.
    New(&'static str),
    /// Delete the file (after closing any tab showing it).
    Delete(PathBuf),
    /// The header's close button: hide the panel.
    Hide,
}

pub struct ScratchesView {
    dir: PathBuf,
    files: Vec<(PathBuf, Option<SystemTime>)>,
    selected: Option<PathBuf>,
    /// The row last right-clicked, for the context menu.
    menu_ix: Option<usize>,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    _watcher: Option<FsWatcher>,
    _tasks: Vec<Task<()>>,
}

impl EventEmitter<ScratchesEvent> for ScratchesView {}

impl Focusable for ScratchesView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ScratchesView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let dir = scratch::dir();
        let _ = std::fs::create_dir_all(&dir);
        let (tx, rx) = flume::unbounded::<ChangeBatch>();
        let watcher = FsWatcher::new(&dir, Duration::from_millis(100), move |b| {
            let _ = tx.send(b);
        })
        .map_err(|e| log::warn!("scratches watcher failed: {e}"))
        .ok();
        let pump = cx.spawn(async move |this, cx| {
            while rx.recv_async().await.is_ok() {
                rx.try_iter().for_each(drop);
                if this.update(cx, |this, cx| this.refresh(cx)).is_err() {
                    break;
                }
            }
        });
        // "3m ago" labels go stale without file events.
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(60)).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        Self {
            files: scratch::list_with_times(&dir),
            dir,
            selected: None,
            menu_ix: None,
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            _watcher: watcher,
            _tasks: vec![pump, tick],
        }
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.files = scratch::list_with_times(&self.dir);
        cx.notify();
    }

    #[cfg(debug_assertions)] // the debug control socket
    pub fn len(&self) -> usize {
        self.files.len()
    }

    fn click(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some((path, _)) = self.files.get(ix) else { return };
        self.selected = Some(path.clone());
        cx.emit(ScratchesEvent::Open(path.clone()));
        cx.notify();
    }

    fn render_rows(&mut self, range: Range<usize>, _: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let look = style::el(ROW, cx);
        let (fg, muted, hover, active) =
            (look.color("text"), look.color("detail_text"), look.color("hover_background"), look.color("selected_background"));
        let detail_size = look.len("detail_font_size");
        let selected_text = look.is_set("selected_text").then(|| look.color("selected_text"));
        let now = SystemTime::now();
        range
            .filter_map(|ix| {
                let (path, modified) = self.files.get(ix)?;
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let (icon, icon_color) = file_icons::file_icon(&name, theme);
                let selected = self.selected.as_deref() == Some(path.as_path());
                let age = modified.map(|m| ago(now.duration_since(m).unwrap_or_default()));
                Some(
                    style::list_row(ROW, ix, 0, cx)
                        .when(selected, |d| d.bg(active))
                        .when_some(selected_text.filter(|_| selected), |d, c| d.text_color(c))
                        .hover(|d| d.bg(hover))
                        .on_click(cx.listener(move |this, _, _, cx| this.click(ix, cx)))
                        .on_mouse_down(MouseButton::Right, cx.listener(move |this, _, _, _| this.menu_ix = Some(ix)))
                        .child(style::chevron_space(ROW, cx))
                        .child(style::row_icon(ROW, icon, icon_color, cx))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .when(!selected || selected_text.is_none(), |d| d.text_color(fg))
                                .child(name),
                        )
                        .when_some(age, |d, a| d.child(div().flex_shrink_0().text_size(detail_size).text_color(muted).child(a)))
                        .into_any_element(),
                )
            })
            .collect()
    }
}

/// "now", "5m", "3h", "2d", "6w".
fn ago(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => "now".into(),
        60..3600 => format!("{}m", s / 60),
        3600..86400 => format!("{}h", s / 3600),
        86400..604800 => format!("{}d", s / 86400),
        _ => format!("{}w", s / 604800),
    }
}

impl Render for ScratchesView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let me = cx.entity().downgrade();
        let buttons = [
            {
                let me = me.clone();
                style::header_button("scratches.header.button", "new-scratch", IconName::Plus, cx)
                    .tooltip("New Scratch File")
                    .dropdown_menu(move |mut menu, _, _| {
                        for (label, ext) in scratch::KINDS {
                            let me = me.clone();
                            menu = menu.item(PopupMenuItem::new(format!("{label}  .{ext}")).on_click(move |_, _, cx| {
                                let _ = me.update(cx, |_, cx| cx.emit(ScratchesEvent::New(ext)));
                            }));
                        }
                        menu
                    })
                    .into_any_element()
            },
            style::header_button("scratches.header.button", "open-scratches-folder", Icon::new(gpui_kit::assets::IconName::FolderOpen), cx)
                .tooltip("Open Scratches Folder")
                .on_click(cx.listener(|this, _, _, cx| cx.open_with_system(&this.dir)))
                .into_any_element(),
            style::header_button("scratches.header.button", "hide-scratches", IconName::Close, cx)
                .tooltip(format!("Hide Scratches ({})", crate::platform::shortcut("⌘⇧S")))
                .on_click(cx.listener(|_, _, _, cx| cx.emit(ScratchesEvent::Hide)))
                .into_any_element(),
        ];
        let header = crate::dock::panel_header("scratches", "scratches.header", "Scratches", Some(self.files.len()), buttons, cx);
        let body: AnyElement = if self.files.is_empty() {
            style::el("scratches.empty", cx)
                .apply(div().flex_1().flex().flex_col().items_center().justify_center())
                .child("No scratch files")
                .child(div().text_xs().child(format!("{} to create one", crate::platform::shortcut("⌘N"))))
                .into_any_element()
        } else {
            div()
                .id("scratch-list")
                .flex_1()
                .min_h_0()
                .child(uniform_list("scratches", self.files.len(), cx.processor(Self::render_rows)).track_scroll(&self.scroll).size_full())
                .map(|d| style::el("scratches.list", cx).apply(d))
                .context_menu(move |menu, _, cx| {
                    let Some(this) = me.upgrade() else { return menu };
                    let Some(path) = this.read(cx).menu_ix.and_then(|ix| this.read(cx).files.get(ix)).map(|f| f.0.clone()) else {
                        return menu;
                    };
                    let item = |label: &'static str, f: fn(&Path, &mut App) -> Option<ScratchesEvent>| {
                        let (me, path) = (me.clone(), path.clone());
                        PopupMenuItem::new(label).on_click(move |_, _, cx| {
                            if let Some(ev) = f(&path, cx) {
                                let _ = me.update(cx, |_, cx| cx.emit(ev));
                            }
                        })
                    };
                    menu.item(item("Open", |p, _| Some(ScratchesEvent::Open(p.to_owned()))))
                        .separator()
                        .item(item("Copy Path", |p, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(p.display().to_string()));
                            None
                        }))
                        .item(item(crate::platform::REVEAL, |p, cx| {
                            cx.reveal_path(p);
                            None
                        }))
                        .separator()
                        .item(item("Delete…", |p, _| Some(ScratchesEvent::Delete(p.to_owned()))))
                })
                .into_any_element()
        };
        let panel = style::el("scratches", cx);
        panel.container(div().track_focus(&self.focus).size_full().flex().flex_col()).child(header).child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::ago;
    use std::time::Duration;

    #[test]
    fn ago_buckets() {
        assert_eq!(ago(Duration::from_secs(5)), "now");
        assert_eq!(ago(Duration::from_secs(5 * 60)), "5m");
        assert_eq!(ago(Duration::from_secs(3 * 3600)), "3h");
        assert_eq!(ago(Duration::from_secs(2 * 86400)), "2d");
        assert_eq!(ago(Duration::from_secs(21 * 86400)), "3w");
    }
}
