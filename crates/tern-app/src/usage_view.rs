//! Usage bar, always under the window's panes (template `usage`): the
//! Claude account the shown tab runs as, the first one turned on when it
//! isn't a Claude tab (`usage.item`), with its limits as far as they fit
//! (`usage.item.limit`). Clicking it opens its whole report
//! (`usage.card`), which also lists every account (`usage.card.account`) to
//! run the shown tab as, open a tab as, rename, turn on or off, add or
//! remove. The numbers come from the app's one store ([`crate::usage`]),
//! the accounts from [`crate::accounts`].
//!
//! Beside it, when the shown tab is a Claude one, a ring of how full its
//! conversation's context window is (`usage.context`); clicking it opens
//! `usage.context.card`, which turns that tab's context limit on or off and
//! sets it (see [`crate::agents::ContextGuard`]).

use std::time::{Duration, SystemTime};

use gpui_kit::component::ActiveTheme;
use gpui_kit::component::input::{Input, InputEvent, InputState, MaskPattern};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::{Selectable, Sizable as _};
use gpui_kit::*;

use crate::accounts::{self, AccountStore};
use crate::agents::{Agent, ContextGuard, ContextUse};
use crate::template::{self, Framed};
use crate::usage::{self, AccountUsage, Limit, UsageStore};

/// What the bar asks of the workspace it is in.
#[derive(Debug, Clone)]
pub enum UsageEvent {
    /// Run the shown Claude tab as this account instead.
    UseHere(String),
    /// A new Claude tab running as this account.
    NewTab(String),
    /// Ask for a name and add an account.
    Add,
    /// Ask for this account's new name.
    Rename(String),
    /// Ask before removing this account.
    Remove(String),
    /// Ask which account the shown Claude tab switches to, then turn this
    /// one, which it runs as, off.
    TurnOff(String),
    /// The shown Claude tab keeps its context window in bounds so.
    Guard(ContextGuard),
}

/// The shown Claude tab's context window: how full it is (once its status
/// line said) and the limit it keeps.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContextShown {
    pub usage: Option<ContextUse>,
    pub guard: ContextGuard,
}

impl ContextShown {
    /// Its values, for `usage.context` and `usage.context.card`.
    fn scope(&self, scope: template::Scope) -> template::Scope {
        let (g, percent) = (self.guard, self.usage.map(|u| u.percent));
        let used = match self.usage {
            Some(u) => format!("{}% of {} used", u.percent.round(), crate::agents::tokens_label(u.size)),
            None => "Not known until Claude answers".into(),
        };
        scope
            .var("context", true)
            .var("context_ring", percent.map(|p| p.to_string()).unwrap_or_default())
            .var("context_text", percent.map(|p| format!("{}%", p.round())).unwrap_or_default())
            .var("context_used", used)
            .var("guarded", g.on)
            .var("over", g.on && percent.is_some_and(|p| p >= f32::from(g.limit)))
            .var("limit", format!("{}%", g.limit))
            .var("auto_switch", g.auto_switch)
    }
}

pub struct UsageView {
    store: Entity<UsageStore>,
    accounts: Entity<AccountStore>,
    card: Entity<UsageCard>,
    /// The account the shown Claude tab runs as; `None` when the shown tab
    /// isn't a Claude one.
    current: Option<String>,
    /// The account the bar shows (see [`shown`]).
    shown: Option<String>,
    /// The card is open.
    open: bool,
    /// The shown Claude tab's context window; `None` when the shown tab
    /// isn't a Claude one.
    context: Option<ContextShown>,
    context_card: Entity<ContextCard>,
    /// The context card is open.
    context_open: bool,
    _subs: Vec<Subscription>,
    _tick: Task<()>,
}

impl EventEmitter<UsageEvent> for UsageView {}

impl UsageView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let store = usage::store(cx);
        let accounts = accounts::store(cx);
        let subs = vec![cx.observe(&store, |_, _, cx| cx.notify()), cx.observe(&accounts, |_, _, cx| cx.notify())];
        let bar = cx.entity().downgrade();
        let card = cx.new(|cx| {
            cx.observe(&store, |_, _, cx| cx.notify()).detach();
            cx.observe(&accounts, |_, _, cx| cx.notify()).detach();
            UsageCard { store: store.clone(), accounts: accounts.clone(), bar, current: None }
        });
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
        let bar = cx.entity().downgrade();
        let context_card = cx.new(|_| ContextCard { bar, shown: None, input: None });
        Self {
            store,
            accounts,
            card,
            current: None,
            shown: None,
            open: false,
            context: None,
            context_card,
            context_open: false,
            _subs: subs,
            _tick: tick,
        }
    }

    pub fn context(&self) -> Option<&ContextShown> {
        self.context.as_ref()
    }

    /// The shown tab's context window (`None`: it isn't a Claude tab).
    pub fn set_context(&mut self, context: Option<ContextShown>, cx: &mut Context<Self>) {
        if self.context != context {
            self.context = context;
            self.context_open &= context.is_some();
            self.context_card.update(cx, |c, cx| {
                c.shown = context;
                cx.notify();
            });
            cx.notify();
        }
    }

    pub fn current(&self) -> Option<&str> {
        self.current.as_deref()
    }

    /// The shown tab runs as `current` (`None`: it isn't a Claude tab).
    pub fn set_current(&mut self, current: Option<String>, cx: &mut Context<Self>) {
        if self.current != current {
            self.current = current.clone();
            self.card.update(cx, |c, cx| {
                c.current = current;
                cx.notify();
            });
            cx.notify();
        }
    }

    /// Closes the card and asks the workspace for `ev`.
    fn act(&mut self, ev: UsageEvent, cx: &mut Context<Self>) {
        self.open = false;
        cx.emit(ev);
        cx.notify();
    }
}

/// The account the bar shows: the one the shown Claude tab runs as; else
/// the one it showed, while that is on; else the first one turned on. So
/// turning an account on never changes it.
fn shown(current: Option<&str>, before: Option<&str>, accounts: &AccountStore) -> String {
    if let Some(id) = current.filter(|id| accounts.get(id).is_some()) {
        return id.to_owned();
    }
    if let Some(id) = before.filter(|id| accounts.get(id).is_some_and(|a| a.active)) {
        return id.to_owned();
    }
    accounts.active().first().map_or(accounts::DEFAULT, |a| a.id.as_str()).to_owned()
}

/// A limit's values, for `usage.item.limit`, `usage.card.limit` and `usage.bar`.
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

/// Why an account shows no numbers, in a word or two; empty when it does.
fn note(u: &AccountUsage) -> &'static str {
    match (&u.report, &u.error) {
        (Some(_), _) => "",
        (None, Some(_)) => "Can't read usage",
        (None, None) if u.reading || !u.checked => "Reading…",
        (None, None) => "Not signed in",
    }
}

/// `5h 27% · W 8%`: an account's limits in a line.
pub fn summary(u: &AccountUsage) -> String {
    match &u.report {
        Some(r) => r.limits.iter().map(|l| format!("{} {}%", l.short, l.used.round())).collect::<Vec<_>>().join(" · "),
        None => note(u).to_owned(),
    }
}

impl Render for UsageView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Drawn: someone may be looking, so keep the numbers fresh.
        self.store.update(cx, |s, _| s.seen());
        let id = shown(self.current.as_deref(), self.shown.as_deref(), self.accounts.read(cx));
        if self.shown.as_deref() != Some(id.as_str()) {
            self.shown = Some(id.clone());
            // The card shows it too.
            self.card.update(cx, |_, cx| cx.notify());
        }
        let name = self.accounts.read(cx).name(&id);
        let now = SystemTime::now();
        let (limits, note) = self.store.update(cx, |s, _| {
            let u = s.get(&id);
            (u.report.as_ref().map(|r| r.limits.iter().map(|l| limit_scope(l, now)).collect()), note(u))
        });
        let mut scope = template::Scope::new("usage")
            .var("name", name)
            .var("note", note)
            .var("open", self.open)
            .var("context_open", self.context_open)
            .slot("icon", Agent::Claude.icon(cx.theme()))
            .list("limits", limits.unwrap_or_default());
        if let Some(c) = &self.context {
            scope = c.scope(scope);
        }
        let (context_card, context_open) = (self.context_card.clone(), self.context_open);
        let me = cx.entity().downgrade();
        let card = self.card.clone();
        let open = self.open;
        template::render(
            "usage",
            scope,
            &mut |name, _, f| match (name, f) {
                ("item", Framed::Box(d)) => {
                    let (me, card) = (me.clone(), card.clone());
                    Popover::new("usage-card")
                        .anchor(Anchor::BottomLeft)
                        .appearance(false)
                        .open(open)
                        .on_open_change(move |open, _, cx| {
                            let _ = me.update(cx, |this, cx| {
                                this.open = *open;
                                cx.notify();
                            });
                        })
                        .trigger(Trigger::new(d.id("usage-item")))
                        .content(move |_, _, _| card.clone())
                        .into_any_element()
                },
                ("context", Framed::Box(d)) => {
                    let (me, card) = (me.clone(), context_card.clone());
                    Popover::new("usage-context-card")
                        .anchor(Anchor::BottomLeft)
                        .appearance(false)
                        .open(context_open)
                        .on_open_change(move |open, _, cx| {
                            let _ = me.update(cx, |this, cx| {
                                this.context_open = *open;
                                cx.notify();
                            });
                        })
                        .trigger(Trigger::new(d.id("usage-context")))
                        .content(move |_, _, _| card.clone())
                        .into_any_element()
                },
                (_, f) => f.into_any_element(),
            },
            cx,
        )
    }
}

/// A box that opens a popover when clicked, as a button does.
#[derive(IntoElement)]
struct Trigger {
    base: Stateful<Div>,
    selected: bool,
}

impl Trigger {
    fn new(base: Stateful<Div>) -> Self {
        Self { base, selected: false }
    }
}

impl Styled for Trigger {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl InteractiveElement for Trigger {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.base.interactivity()
    }
}

impl Selectable for Trigger {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl RenderOnce for Trigger {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        self.base
    }
}

/// The shown account's whole report and every account, open while the
/// bar's item is clicked.
pub struct UsageCard {
    store: Entity<UsageStore>,
    accounts: Entity<AccountStore>,
    bar: WeakEntity<UsageView>,
    /// As the bar's.
    current: Option<String>,
}

impl Render for UsageCard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // As the bar decided; worked out alike before it drew.
        let before = self.bar.upgrade().and_then(|b| b.read(cx).shown.clone());
        let account = shown(self.current.as_deref(), before.as_deref(), self.accounts.read(cx));
        let store = self.store.downgrade();
        let title = self.accounts.read(cx).name(&account);
        let list = self.accounts.read(cx).list.clone();
        let names: Vec<String> = list.iter().map(|a| self.accounts.read(cx).name(&a.id)).collect();
        let offered = list.iter().filter(|a| a.active).count();
        let now = SystemTime::now();
        let scope = self.store.update(cx, |s, _| {
            let entries = list
                .iter()
                .zip(&names)
                .map(|(a, name)| {
                    // The email, unless it is the name already, and the usage.
                    let email = accounts::email(&a.id).filter(|e| e != name).unwrap_or_default();
                    let detail = [email, summary(s.get(&a.id))].into_iter().filter(|t| !t.is_empty()).collect::<Vec<_>>().join(" · ");
                    template::Scope::default()
                        .var("name", name.as_str())
                        .var("detail", detail)
                        .var("shown", a.id == account)
                        .var("active", a.active)
                        .var("last", a.active && offered <= 1)
                        .var("removable", a.id != accounts::DEFAULT)
                        .var("switch_tip", if a.active { "Turn Off" } else { "Turn On" })
                })
                .collect();
            let u = s.get(&account);
            let report = u.report.as_ref();
            let limits = report.map(|r| r.limits.iter().map(|l| limit_scope(l, now)).collect()).unwrap_or_default();
            let updated = match u.updated {
                Some(t) => format!("Updated {}", usage::ago(t, now)),
                None if u.reading => "Reading…".into(),
                None => "Not read yet".into(),
            };
            let error = match (&u.error, report) {
                (Some(e), _) => e.clone(),
                (None, None) if !u.reading && u.checked => "No sign-in yet: run /login in a Claude tab of this account".into(),
                _ => String::new(),
            };
            template::Scope::new("usage.card")
                .var("title", title.as_str())
                .var("updated", updated)
                .var("plan", report.and_then(|r| r.plan.clone()).unwrap_or_default())
                .var(
                    "email",
                    // Not when it is the title already.
                    report.and_then(|r| r.email.clone()).or_else(|| accounts::email(&account)).filter(|e| *e != title).unwrap_or_default(),
                )
                .var("error", error)
                .var("extra", report.and_then(|r| r.extra.clone()).unwrap_or_default())
                .var("reading", u.reading)
                .list("limits", limits)
                .list("accounts", entries)
        });
        let scope = scope.slot("icon", Agent::Claude.icon(cx.theme()));
        let (bar, accounts, tab) = (self.bar.clone(), self.accounts.downgrade(), self.current.clone());
        // Closes the card and asks the workspace for an event.
        let act = move |ev: UsageEvent| {
            let bar = bar.clone();
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                cx.stop_propagation();
                let _ = bar.update(cx, |b, cx| b.act(ev.clone(), cx));
            }
        };
        template::render(
            "usage.card",
            scope,
            &mut |name, ix, f| {
                let a = ix.and_then(|ix| list.get(ix));
                match (name, f, a) {
                    ("refresh", Framed::Button(b), _) => {
                        let (store, account) = (store.clone(), account.clone());
                        b.on_click(move |_, _, cx| {
                            let _ = store.update(cx, |s, cx| s.refresh(&account, cx));
                        })
                        .into_any_element()
                    },
                    ("dashboard", Framed::Button(b), _) => b.on_click(|_, _, cx| cx.open_url(usage::DASHBOARD)).into_any_element(),
                    ("status", Framed::Button(b), _) => b.on_click(|_, _, cx| cx.open_url(usage::STATUS_PAGE)).into_any_element(),
                    ("add", Framed::Button(b), _) => b.on_click(act(UsageEvent::Add)).into_any_element(),
                    // In a Claude tab: run it as this account; else open one.
                    // One turned off can't be run as.
                    ("account", Framed::Box(d), Some(a)) => {
                        let ev = match &tab {
                            _ if !a.active => None,
                            Some(t) if *t == a.id => None,
                            Some(_) => Some(UsageEvent::UseHere(a.id.clone())),
                            None => Some(UsageEvent::NewTab(a.id.clone())),
                        };
                        let d = d.id(("usage-account", ix.unwrap_or_default()));
                        match ev {
                            Some(ev) => d.cursor_pointer().on_click(act(ev)).into_any_element(),
                            None => d.into_any_element(),
                        }
                    },
                    ("new_tab", Framed::Button(b), Some(a)) => b.on_click(act(UsageEvent::NewTab(a.id.clone()))).into_any_element(),
                    ("rename", Framed::Button(b), Some(a)) => b.on_click(act(UsageEvent::Rename(a.id.clone()))).into_any_element(),
                    ("remove", Framed::Button(b), Some(a)) => b.on_click(act(UsageEvent::Remove(a.id.clone()))).into_any_element(),
                    // Turning off the account the shown tab runs as asks
                    // which one it switches to first.
                    ("enable", Framed::Switch(sw), Some(a)) => {
                        let (accounts, id) = (accounts.clone(), a.id.clone());
                        let in_use = tab.as_deref() == Some(a.id.as_str());
                        let bar = self.bar.clone();
                        sw.on_click(move |on, _, cx| {
                            cx.stop_propagation();
                            if !*on && in_use {
                                let _ = bar.update(cx, |b, cx| b.act(UsageEvent::TurnOff(id.clone()), cx));
                            } else {
                                let _ = accounts.update(cx, |s, cx| s.set_active(&id, *on, cx));
                            }
                        })
                        .into_any_element()
                    },
                    (_, f, _) => f.into_any_element(),
                }
            },
            cx,
        )
    }
}

/// What clicking the context ring opens: how full the shown Claude tab's
/// context window is, and its limit to turn on or off and type.
pub struct ContextCard {
    bar: WeakEntity<UsageView>,
    /// As the bar's.
    shown: Option<ContextShown>,
    /// Where the limit is typed (made at the first draw: it needs the window).
    input: Option<(Entity<InputState>, Subscription)>,
}

impl ContextCard {
    /// Ask the workspace for the shown tab's guard changed so; the card stays.
    fn set(&self, guard: ContextGuard, cx: &mut App) {
        let _ = self.bar.update(cx, |_, cx| cx.emit(UsageEvent::Guard(guard)));
    }

    /// The limit field, made once, showing the limit while it isn't typed in.
    fn input(&mut self, limit: u8, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        let input = match &self.input {
            Some((input, _)) => input.clone(),
            None => {
                let input = cx.new(|cx| {
                    InputState::new(window, cx)
                        .mask_pattern(MaskPattern::Number { separator: None, fraction: None })
                        .default_value(limit.to_string())
                });
                let sub = cx.subscribe_in(&input, window, |this, input, ev: &InputEvent, window, cx| {
                    let Some(g) = this.shown.map(|s| s.guard) else { return };
                    let text = input.read(cx).value();
                    let limit = match ev {
                        // Taken as typed once it is a limit it can be.
                        InputEvent::Change => ContextGuard::typed(&text),
                        // Done typing: the nearest one, or back to what it was.
                        InputEvent::PressEnter { .. } | InputEvent::Blur => {
                            let limit = ContextGuard::nearest(&text).unwrap_or(g.limit);
                            if text.trim() != limit.to_string() {
                                input.update(cx, |i, cx| i.set_value(limit.to_string(), window, cx));
                            }
                            Some(limit)
                        },
                        InputEvent::Focus => None,
                    };
                    if let Some(limit) = limit.filter(|l| *l != g.limit) {
                        this.set(ContextGuard { limit, ..g }, cx);
                    }
                });
                self.input = Some((input.clone(), sub));
                input
            },
        };
        let typing = input.read(cx).focus_handle(cx).is_focused(window);
        if !typing && input.read(cx).value() != limit.to_string() {
            input.update(cx, |i, cx| i.set_value(limit.to_string(), window, cx));
        }
        input
    }
}

impl Render for ContextCard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(shown) = self.shown else { return div().into_any_element() };
        let g = shown.guard;
        let input = self.input(g.limit, window, cx);
        let me = cx.entity().downgrade();
        let scope = shown.scope(template::Scope::new("usage.context.card")).slot("input", Input::new(&input).small());
        template::render(
            "usage.context.card",
            scope,
            &mut |name, _, f| match (name, f) {
                (switch @ ("guard" | "auto_switch"), Framed::Switch(sw)) => {
                    let me = me.clone();
                    let guarding = switch == "guard";
                    sw.on_click(move |on, _, cx| {
                        cx.stop_propagation();
                        let guard = if guarding { ContextGuard { on: *on, ..g } } else { ContextGuard { auto_switch: *on, ..g } };
                        let _ = me.update(cx, |c, cx| c.set(guard, cx));
                    })
                    .into_any_element()
                },
                (_, f) => f.into_any_element(),
            },
            cx,
        )
        .into_any_element()
    }
}
