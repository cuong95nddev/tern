//! The Review Changes dialog's body: the Changes panel's list and commit box
//! on the left, the selected file's diff on the right. The list, its checks
//! and the message are the panel's ([`ChangesView`]), shown here instead of
//! there while the dialog is open.

use std::path::Path;

use gpui_kit::component::Sizable as _;
use gpui_kit::component::input::Textarea;
use gpui_kit::*;
use tern_fs::ChangeBatch;

use crate::changes_view::ChangesView;
use crate::diff_pane::DiffPane;
use crate::style;
use crate::template::{self, Framed, Scope, on_event};

pub struct ChangesReview {
    changes: Entity<ChangesView>,
    /// The selected file's diff.
    pane: Option<Entity<DiffPane>>,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    _observe: Subscription,
    _release: Subscription,
}

impl Focusable for ChangesReview {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ChangesReview {
    pub fn new(changes: Entity<ChangesView>, cx: &mut Context<Self>) -> Self {
        changes.update(cx, |c, cx| {
            c.set_reviewing(true, cx);
            // Something to review from the start.
            if c.selected().is_none() {
                c.step(1, cx);
            }
        });
        let observe = cx.observe(&changes, |this, _, cx| this.sync(cx));
        let release = cx.on_release(|this, cx| this.changes.update(cx, |c, cx| c.set_reviewing(false, cx)));
        let mut this = Self {
            changes,
            pane: None,
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            _observe: observe,
            _release: release,
        };
        this.sync(cx);
        this
    }

    /// Show the selected file's diff, if it isn't shown already. A file
    /// gone from the list (committed, reverted) passes to the first left.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let selected = self.changes.read(cx).selected().map(Path::to_path_buf);
        if selected.is_none() && self.pane.is_some() && self.changes.update(cx, |c, cx| c.step(1, cx)).is_some() {
            // Its notification syncs again.
            return;
        }
        if self.pane.as_ref().map(|p| p.read(cx).path().to_owned()) != selected {
            self.pane = selected.map(|p| cx.new(|cx| DiffPane::new(p, cx)));
        }
        cx.notify();
    }

    /// Paths changed on disk: reload the diff if they touch it.
    pub fn files_changed(&mut self, batch: &ChangeBatch, cx: &mut Context<Self>) {
        if let Some(p) = &self.pane {
            p.update(cx, |p, cx| p.files_changed(batch, cx));
        }
    }

    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        if let Some(ix) = self.changes.update(cx, |c, cx| c.step(delta, cx)) {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
        }
    }

    fn key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // Only with the list in focus: the message field and the diff take
        // their own arrows.
        if !self.focus.is_focused(window) || ev.keystroke.modifiers.modified() {
            return;
        }
        match ev.keystroke.key.as_str() {
            "down" | "j" => self.step(1, cx),
            "up" | "k" => self.step(-1, cx),
            _ => return,
        }
        cx.stop_propagation();
    }
}

impl Render for ChangesReview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let changes = self.changes.read(cx);
        let rows = changes.row_count();
        let empty = rows == 0;
        let mut scope = changes
            .commit_scope(Scope::new("changes-review"), cx)
            .var("empty", empty)
            .var("refreshing", changes.refreshing())
            .var("selected", self.pane.is_some())
            .slot("input", Textarea::new(changes.message()).small())
            .slot_opt("diff", self.pane.clone());
        if !empty {
            let list = self.changes.clone();
            let focus = self.focus.clone();
            let list =
                uniform_list("changes-review", rows, move |range, window, cx| list.update(cx, |c, cx| c.rows_for(range, true, window, cx)))
                    .track_scroll(&self.scroll)
                    // The arrows step through the list once it is clicked.
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| window.focus(&focus, cx))
                    .flex_1();
            scope = scope.slot("list", style::el("changes.list", cx).apply(list));
        }
        let me = cx.entity().downgrade();
        let changes = self.changes.downgrade();
        let focus = &self.focus;
        template::render(
            "dialog.changes_review",
            scope,
            &mut |name, _, f| match (name, f) {
                ("review", Framed::Box(d)) => d.track_focus(focus).on_key_down(on_event(&me, Self::key_down)).into_any_element(),
                ("amend", Framed::Button(b)) => {
                    let changes = changes.clone();
                    b.on_click(move |_, window, cx| {
                        let _ = changes.update(cx, |c, cx| c.toggle_amend(window, cx));
                    })
                    .into_any_element()
                },
                ("commit", Framed::Button(b)) => {
                    let changes = changes.clone();
                    b.on_click(move |_, window, cx| {
                        let _ = changes.update(cx, |c, cx| c.commit(window, cx));
                    })
                    .into_any_element()
                },
                (_, f) => f.into_any_element(),
            },
            cx,
        )
    }
}
