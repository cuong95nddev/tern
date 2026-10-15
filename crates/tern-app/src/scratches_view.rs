//! Scratches panel: every file in the global scratches folder, newest first,
//! live-updated by an fs watcher. Clicking a row opens it in a tab.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tern_fs::{ChangeBatch, FsWatcher};

use crate::file_icons;
use crate::scratch;

const ROW_HEIGHT: f32 = 22.;

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
        let (fg, muted, hover, active) = (theme.foreground, theme.muted_foreground, theme.list_hover, theme.list_active);
        let now = SystemTime::now();
        range
            .filter_map(|ix| {
                let (path, modified) = self.files.get(ix)?;
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let (icon, icon_color) = file_icons::file_icon(&name, theme);
                let selected = self.selected.as_deref() == Some(path.as_path());
                let age = modified.map(|m| ago(now.duration_since(m).unwrap_or_default()));
                Some(
                    div()
                        .id(ix)
                        .h(px(ROW_HEIGHT))
                        .w_full()
                        .flex()
                        .items_center()
                        .gap_1()
                        .pl(px(10.))
                        .pr_2()
                        .text_sm()
                        .when(selected, |d| d.bg(active))
                        .hover(|d| d.bg(hover))
                        .on_click(cx.listener(move |this, _, _, cx| this.click(ix, cx)))
                        .on_mouse_down(MouseButton::Right, cx.listener(move |this, _, _, _| this.menu_ix = Some(ix)))
                        .child(Icon::new(icon).size_4().text_color(icon_color))
                        .child(div().flex_1().min_w_0().truncate().text_color(fg).child(name))
                        .when_some(age, |d, a| d.child(div().flex_shrink_0().text_xs().text_color(muted).child(a)))
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
        let theme = cx.theme();
        let me = cx.entity().downgrade();
        let header = div()
            .h(px(30.))
            .pl_3()
            .pr_1()
            .flex()
            .items_center()
            .gap_2()
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme.muted_foreground)
            .child("SCRATCHES")
            .when(!self.files.is_empty(), |d| d.child(self.files.len().to_string()))
            .child(div().flex_1())
            .child({
                let me = me.clone();
                Button::new("new-scratch").xsmall().ghost().icon(IconName::Plus).tooltip("New Scratch File").dropdown_menu(
                    move |mut menu, _, _| {
                        for (label, ext) in scratch::KINDS {
                            let me = me.clone();
                            menu = menu.item(PopupMenuItem::new(format!("{label}  .{ext}")).on_click(move |_, _, cx| {
                                let _ = me.update(cx, |_, cx| cx.emit(ScratchesEvent::New(ext)));
                            }));
                        }
                        menu
                    },
                )
            })
            .child(
                Button::new("open-scratches-folder")
                    .xsmall()
                    .ghost()
                    .icon(Icon::new(gpui_kit::assets::IconName::FolderOpen))
                    .tooltip("Open Scratches Folder")
                    .on_click(cx.listener(|this, _, _, cx| cx.open_with_system(&this.dir))),
            )
            .child(
                Button::new("hide-scratches")
                    .xsmall()
                    .ghost()
                    .icon(IconName::Close)
                    .tooltip(format!("Hide Scratches ({})", crate::platform::shortcut("⌘⇧S")))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ScratchesEvent::Hide))),
            );
        let body: AnyElement = if self.files.is_empty() {
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_1()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("No scratch files")
                .child(div().text_xs().child(format!("{} to create one", crate::platform::shortcut("⌘N"))))
                .into_any_element()
        } else {
            div()
                .id("scratch-list")
                .flex_1()
                .min_h_0()
                .child(uniform_list("scratches", self.files.len(), cx.processor(Self::render_rows)).track_scroll(&self.scroll).size_full())
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
        div().track_focus(&self.focus).size_full().flex().flex_col().bg(theme.sidebar).child(header).child(body)
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
