//! Usage panel: the plans' usage as a status bar (template `usage`, a
//! `usage.item` per service, for now Claude's only, with a
//! `usage.item.limit` per limit as far as they fit). Hovering an item shows
//! the whole report
//! (`usage.card`), which stays open under the pointer for its links. The
//! numbers come from the app's one store ([`crate::usage`]).

use std::time::{Duration, SystemTime};

use gpui_kit::component::ActiveTheme;
use gpui_kit::*;

use crate::agents::Agent;
use crate::template::{self, Framed, on_event};
use crate::usage::{self, Limit, UsageStore};

#[derive(Debug, Clone)]
pub enum UsageEvent {
    /// The header's close button: hide the panel.
    Hide,
}

pub struct UsageView {
    store: Entity<UsageStore>,
    card: Entity<UsageCard>,
    focus: FocusHandle,
    _sub: Subscription,
    _tick: Task<()>,
}

impl EventEmitter<UsageEvent> for UsageView {}

impl Focusable for UsageView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl UsageView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let store = usage::store(cx);
        let card = cx.new(|cx| {
            cx.observe(&store, |_, _, cx| cx.notify()).detach();
            UsageCard { store: store.clone() }
        });
        let sub = cx.observe(&store, |_, _, cx| cx.notify());
        // The times count down by the minute.
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(30)).await;
                let Ok(card) = this.update(cx, |this, cx| {
                    cx.notify();
                    this.card.clone()
                }) else {
                    break;
                };
                card.update(cx, |_, cx| cx.notify());
            }
        });
        Self { store, card, focus: cx.focus_handle(), _sub: sub, _tick: tick }
    }
}

/// A limit's values, for `usage.item`, `usage.card.limit` and `usage.bar`.
fn limit_scope(l: &Limit, now: SystemTime) -> template::Scope {
    let used = f64::from(l.used.clamp(0., 100.));
    let pace = l.pace(now);
    let expected = pace.map_or(0., |p| f64::from(p.expected.clamp(0., 100.)));
    template::Scope::new(format!("usage-{}", l.name))
        .var("name", l.name.as_str())
        .var("short", l.short.as_str())
        .var("used", used)
        .var("rest", 100. - used)
        .var("used_text", format!("{}", used.round()))
        .var("left", format!("{}", (100. - used).round()))
        .var("resets", l.resets_in(now).unwrap_or_default())
        .var("warning", (80.0..95.).contains(&used))
        .var("critical", used >= 95.)
        .var("has_pace", pace.is_some())
        .var("pace", expected)
        .var("pace_rest", 100. - expected)
        .var("pace_text", pace.map(|p| p.describe(l.used)).unwrap_or_default())
}

impl Render for UsageView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Drawn: someone may be looking, so keep the numbers fresh.
        self.store.update(cx, |s, _| s.seen());
        let me = cx.entity().downgrade();
        let store = self.store.downgrade();
        let s = self.store.read(cx);
        let reading = s.reading;
        let header = crate::dock::panel_header(
            "usage",
            "usage.header",
            "Usage",
            None,
            template::Scope::new("usage.header.buttons").var("reading", reading),
            &mut |name, _, f| match (name, f) {
                ("refresh", Framed::Button(b)) => {
                    let store = store.clone();
                    b.on_click(move |_, _, cx| {
                        let _ = store.update(cx, |s, cx| s.refresh(cx));
                    })
                    .into_any_element()
                },
                ("hide", Framed::Button(b)) => {
                    b.on_click(on_event(&me, |_, _: &ClickEvent, _, cx| cx.emit(UsageEvent::Hide))).into_any_element()
                },
                (_, f) => f.into_any_element(),
            },
            cx,
        );
        let now = SystemTime::now();
        // One item per service, with its limits as far as they fit.
        let items: Vec<template::Scope> = s
            .report
            .as_ref()
            .map(|r| {
                template::Scope::new("usage-claude")
                    .slot("icon", Agent::Claude.icon(cx.theme()))
                    .list("limits", r.limits.iter().map(|l| limit_scope(l, now)).collect())
            })
            .into_iter()
            .collect();
        let message = match (&s.report, &s.error) {
            (Some(_), _) => String::new(),
            (None, Some(e)) => e.clone(),
            (None, None) if reading || s.updated.is_none() => "Reading Claude usage…".into(),
            (None, None) => "Sign in to Claude Code to see its usage".into(),
        };
        let scope = template::Scope::new("usage")
            .var("ready", !items.is_empty())
            .var("stale", s.report.is_some() && s.error.is_some())
            .var("message", message)
            .slot("header", header)
            .list("items", items);
        let (focus, card) = (&self.focus, self.card.clone());
        template::render(
            "usage",
            scope,
            &mut |name, ix, f| match (name, f) {
                ("panel", Framed::Box(d)) => d.track_focus(focus).into_any_element(),
                ("item", Framed::Box(d)) => {
                    let card = card.clone();
                    d.id(("usage-item", ix.unwrap_or_default())).hoverable_tooltip(move |_, _| card.clone().into()).into_any_element()
                },
                (_, f) => f.into_any_element(),
            },
            cx,
        )
    }
}

/// The whole report, shown while its item is hovered.
pub struct UsageCard {
    store: Entity<UsageStore>,
}

impl Render for UsageCard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let store = self.store.downgrade();
        let s = self.store.read(cx);
        let now = SystemTime::now();
        let report = s.report.as_ref();
        let limits = report.map(|r| r.limits.iter().map(|l| limit_scope(l, now)).collect()).unwrap_or_default();
        let updated = match s.updated {
            Some(t) => format!("Updated {}", usage::ago(t, now)),
            None if s.reading => "Reading…".into(),
            None => "Not read yet".into(),
        };
        let scope = template::Scope::new("usage.card")
            .var("title", "Claude")
            .var("updated", updated)
            .var("plan", report.and_then(|r| r.plan.clone()).unwrap_or_default())
            .var("email", report.and_then(|r| r.email.clone()).unwrap_or_default())
            .var("error", s.error.clone().unwrap_or_default())
            .var("extra", report.and_then(|r| r.extra.clone()).unwrap_or_default())
            .var("reading", s.reading)
            .slot("icon", Agent::Claude.icon(cx.theme()))
            .list("limits", limits);
        template::render(
            "usage.card",
            scope,
            &mut |name, _, f| match (name, f) {
                ("refresh", Framed::Button(b)) => {
                    let store = store.clone();
                    b.on_click(move |_, _, cx| {
                        let _ = store.update(cx, |s, cx| s.refresh(cx));
                    })
                    .into_any_element()
                },
                ("dashboard", Framed::Button(b)) => b.on_click(|_, _, cx| cx.open_url(usage::DASHBOARD)).into_any_element(),
                ("status", Framed::Button(b)) => b.on_click(|_, _, cx| cx.open_url(usage::STATUS_PAGE)).into_any_element(),
                (_, f) => f.into_any_element(),
            },
            cx,
        )
    }
}
