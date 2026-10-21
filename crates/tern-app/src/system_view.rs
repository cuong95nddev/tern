//! System panel: how busy the machine is, as a status bar (template
//! `system`): the CPU's load as a sparkline of its last samples and the
//! memory in use as a bar. The numbers come from the app's one store
//! ([`crate::system`]). A click opens the Resources card over it: what
//! each terminal uses ([`crate::resources_view`]).

use gpui_kit::component::popover::Popover;
use gpui_kit::*;

use crate::resources_view::{self, ResourcesView};
use crate::system::{self, SystemStore};
use crate::template::{self, Framed};
use crate::usage_view::Trigger;

pub struct SystemView {
    store: Entity<SystemStore>,
    card: Entity<ResourcesView>,
    /// The card is open.
    open: bool,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl Focusable for SystemView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl SystemView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let store = system::store(cx);
        let card = cx.new(ResourcesView::new);
        let subs = vec![
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.subscribe(&card, |this, _, _: &resources_view::Done, cx| {
                this.open = false;
                cx.notify();
            }),
        ];
        Self { store, card, open: false, focus: cx.focus_handle(), _subs: subs }
    }

    /// Open the Resources card (Show Resources).
    pub fn open_card(&mut self, cx: &mut Context<Self>) {
        self.open = true;
        cx.notify();
    }
}

/// A percent's values for a bar: `used` and `rest` share its room.
fn percent(scope: template::Scope, used: f32) -> template::Scope {
    let used = f64::from(used.clamp(0., 100.));
    scope
        .var("used", used)
        .var("rest", 100. - used)
        .var("text", format!("{}", used.round()))
        .var("warning", (80.0..95.).contains(&used))
        .var("critical", used >= 95.)
}

impl Render for SystemView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Drawn: someone may be looking, so keep sampling.
        self.store.update(cx, |s, _| s.seen());
        let s = self.store.read(cx);
        // The sparkline always has every bar, empty until sampled.
        let missing = system::HISTORY - s.cpu.len();
        let samples = std::iter::repeat_n(0., missing)
            .chain(s.cpu.iter().copied())
            .enumerate()
            .map(|(i, load)| percent(template::Scope::new(format!("system-cpu-{i}")), load))
            .collect();
        let cpu = s.cpu.back().copied();
        let mut scope = template::Scope::new("system")
            .var("ready", cpu.is_some() || s.mem.is_some())
            .var("cpu_text", cpu.map(|c| format!("{}", c.round())).unwrap_or_else(|| "–".into()))
            .var(
                "mem_detail",
                if s.mem_total > 0 {
                    format!("{} of {}", system::gigabytes(s.mem_used), system::gigabytes(s.mem_total))
                } else {
                    String::new()
                },
            )
            .list("cpu", samples);
        scope = percent(scope, s.mem.unwrap_or(0.));
        let focus = &self.focus;
        let (me, card, open) = (cx.entity().downgrade(), self.card.clone(), self.open);
        template::render(
            "system",
            scope,
            &mut |name, _, f| match (name, f) {
                ("panel", Framed::Box(d)) => d.track_focus(focus).into_any_element(),
                ("item", Framed::Box(d)) => {
                    let (me, card) = (me.clone(), card.clone());
                    Popover::new("resources-card")
                        .anchor(Anchor::BottomLeft)
                        .appearance(false)
                        .open(open)
                        .on_open_change(move |open, _, cx| {
                            let _ = me.update(cx, |this, cx| {
                                this.open = *open;
                                cx.notify();
                            });
                        })
                        .trigger(Trigger::new(d.id("system-item").cursor_pointer()))
                        .content(move |_, _, _| card.clone())
                        .into_any_element()
                },
                (_, f) => f.into_any_element(),
            },
            cx,
        )
    }
}
