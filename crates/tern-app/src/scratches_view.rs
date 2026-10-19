//! Scratches panel: every file in the global scratches folder, newest first,
//! live-updated by an fs watcher. Clicking a row opens it in a tab.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use gpui_kit::component::ActiveTheme;
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tern_fs::{ChangeBatch, FsWatcher};

use crate::file_icons;
use crate::template::{self, Framed, on_event};
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
        let indent = style::row_indent(ROW, 0, cx);
        let now = SystemTime::now();
        range
            .filter_map(|ix| {
                let (path, modified) = self.files.get(ix)?;
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let (icon, icon_color) = file_icons::file_icon(&name, theme);
                let age = modified.map(|m| ago(now.duration_since(m).unwrap_or_default()));
                let scope = template::Scope::new(ROW)
                    .var("indent", indent)
                    .var("selected", self.selected.as_deref() == Some(path.as_path()))
                    .var("name", name)
                    .var("age", age.unwrap_or_default())
                    .slot("icon", style::row_icon(ROW, icon, icon_color, cx));
                Some(template::render(
                    ROW,
                    scope,
                    &mut |name, _, f| match (name, f) {
                        ("row", Framed::Box(d)) => d
                            .id(ix)
                            .on_click(cx.listener(move |this, _, _, cx| this.click(ix, cx)))
                            .on_mouse_down(MouseButton::Right, cx.listener(move |this, _, _, _| this.menu_ix = Some(ix)))
                            .into_any_element(),
                        (_, f) => f.into_any_element(),
                    },
                    cx,
                ))
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
        let buttons = template::Scope::new("scratches.header.buttons").var("hide_key", crate::platform::shortcut("⌘⇧S"));
        let header = crate::dock::panel_header(
            "scratches",
            "scratches.header",
            "Scratches",
            Some(self.files.len()),
            buttons,
            &mut |name, _, f| match (name, f) {
                ("new", Framed::Button(b)) => {
                    let me = me.clone();
                    b.dropdown_menu(move |menu, window, cx| {
                        let me = me.clone();
                        let items: template::MenuItems = Rc::new(move |name, ix| {
                            let (_, ext) = *scratch::KINDS.get(ix?).filter(|_| name == "new")?;
                            let me = me.clone();
                            Some(Rc::new(move |_, _, cx| {
                                let _ = me.update(cx, |_, cx| cx.emit(ScratchesEvent::New(ext)));
                            }))
                        });
                        let scope = template::Scope::new("new-scratch").list("kinds", scratch::kind_entries());
                        template::menu("menu.new_scratch", scope, menu, items, window, cx)
                    })
                    .into_any_element()
                },
                ("open_folder", Framed::Button(b)) => {
                    b.on_click(on_event(&me, |this, _: &ClickEvent, _, cx| cx.open_with_system(&this.dir))).into_any_element()
                },
                ("hide", Framed::Button(b)) => {
                    b.on_click(on_event(&me, |_, _: &ClickEvent, _, cx| cx.emit(ScratchesEvent::Hide))).into_any_element()
                },
                (_, f) => f.into_any_element(),
            },
            cx,
        );
        let empty = self.files.is_empty();
        let list = (!empty).then(|| {
            div()
                .id("scratch-list")
                .flex_1()
                .min_h_0()
                .child(uniform_list("scratches", self.files.len(), cx.processor(Self::render_rows)).track_scroll(&self.scroll).size_full())
                .map(|d| style::el("scratches.list", cx).apply(d))
                .context_menu(move |menu, window, cx| {
                    let Some(this) = me.upgrade() else { return menu };
                    let Some(path) = this.read(cx).menu_ix.and_then(|ix| this.read(cx).files.get(ix)).map(|f| f.0.clone()) else {
                        return menu;
                    };
                    let me = me.clone();
                    let items: template::MenuItems = Rc::new(move |name, _| {
                        let f: fn(&Path, &mut App) -> Option<ScratchesEvent> = match name {
                            "open" => |p, _| Some(ScratchesEvent::Open(p.to_owned())),
                            "copy_path" => |p, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(p.display().to_string()));
                                None
                            },
                            "reveal" => |p, cx| {
                                cx.reveal_path(p);
                                None
                            },
                            "delete" => |p, _| Some(ScratchesEvent::Delete(p.to_owned())),
                            _ => return None,
                        };
                        let (me, path) = (me.clone(), path.clone());
                        Some(Rc::new(move |_, _, cx| {
                            if let Some(ev) = f(&path, cx) {
                                let _ = me.update(cx, |_, cx| cx.emit(ev));
                            }
                        }))
                    });
                    let scope = template::Scope::new("scratch-menu").var("reveal", crate::platform::REVEAL);
                    template::menu("menu.scratch", scope, menu, items, window, cx)
                })
        });
        let scope = template::Scope::new("scratches")
            .var("empty", empty)
            .var("new_key", crate::platform::shortcut("⌘N"))
            .slot("header", header)
            .slot_opt("list", list);
        let focus = &self.focus;
        template::render(
            "scratches",
            scope,
            &mut |name, _, f| match (name, f) {
                ("panel", Framed::Box(d)) => d.track_focus(focus).into_any_element(),
                (_, f) => f.into_any_element(),
            },
            cx,
        )
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
