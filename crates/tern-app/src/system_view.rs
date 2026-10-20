//! System panel: how busy the machine is, as a status bar (template
//! `system`): the CPU's load as a sparkline of its last samples and the
//! memory in use as a bar. The numbers come from the app's one store
//! ([`crate::system`]).

use gpui_kit::*;

use crate::system::{self, SystemStore};
use crate::template::{self, Framed};

pub struct SystemView {
    store: Entity<SystemStore>,
    focus: FocusHandle,
    _sub: Subscription,
}

impl Focusable for SystemView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl SystemView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let store = system::store(cx);
        let sub = cx.observe(&store, |_, _, cx| cx.notify());
        Self { store, focus: cx.focus_handle(), _sub: sub }
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
        template::render(
            "system",
            scope,
            &mut |name, _, f| match (name, f) {
                ("panel", Framed::Box(d)) => d.track_focus(focus).into_any_element(),
                (_, f) => f.into_any_element(),
            },
            cx,
        )
    }
}
