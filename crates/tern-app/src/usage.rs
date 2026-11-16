//! How much of each active Claude account's plan limits is used, read with
//! that account's Claude Code sign-in (its login keychain item, else its
//! folder's `.credentials.json`; see [`crate::accounts`]) from the endpoint
//! behind Claude Code's `/usage`. The endpoint isn't documented: an answer
//! it doesn't recognize shows as an error rather than as numbers.
//!
//! One store for the app ([`Usage`]); the usage bars only draw it. It reads
//! while a bar was drawn lately, every [`REFRESH`] per account, and backs
//! off when the service says no. Each account's last answer and when to
//! ask next are kept in the cache folder, shared by every Tern running, so
//! a restart or a second Tern doesn't ask again before it is due.

use std::collections::HashMap;
#[cfg(not(windows))]
use std::io::Write as _;
use std::path::PathBuf;
#[cfg(not(windows))]
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui_kit::*;
use serde::{Deserialize, Serialize};

use crate::accounts;

const URL: &str = "https://api.anthropic.com/api/oauth/usage";
/// The endpoint serves Claude Code's OAuth clients.
const BETA: &str = "oauth-2025-04-20";
const AGENT: &str = "claude-code/2.1.0";

pub const DASHBOARD: &str = "https://claude.ai/settings/usage";
pub const STATUS_PAGE: &str = "https://status.anthropic.com";

/// The service rate limits the endpoint, and the numbers move slowly.
const REFRESH: Duration = Duration::from_secs(10 * 60);
const RATE_LIMITED: Duration = Duration::from_secs(30 * 60);
/// A read that failed (offline, the service down) is tried again sooner.
const ERROR_BACKOFF: Duration = Duration::from_secs(5 * 60);
/// A sign-in the service rejected is asked again half as often after each
/// rejection, up to this, so a dead token isn't sent every refresh.
const REJECTED_LIMIT: Duration = Duration::from_secs(6 * 60 * 60);
/// A click can't queue reads back to back.
const MANUAL_SPACING: Duration = Duration::from_secs(10);
/// A click doesn't ask again within this of the last answer (from any Tern).
const FORCED_FRESH: Duration = Duration::from_secs(60);
/// No panel drawn for this long: nobody looks, so nothing is read.
const UNSEEN: Duration = Duration::from_secs(15 * 60);

/// How often an agent's quota is read off its own files.
const AGENT_READ_EVERY: Duration = Duration::from_secs(60);

const SESSION: Duration = Duration::from_secs(5 * 60 * 60);
const WEEK: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// A limit: how much of it is used and when it starts over.
#[derive(Debug, Clone, PartialEq)]
pub struct Limit {
    /// `Session`, `Weekly`, or the model a weekly limit is for.
    pub name: String,
    /// What the status bar calls it: `5h`, `W`, the model.
    pub short: String,
    /// Percent, 0 to 100.
    pub used: f32,
    pub resets_at: Option<SystemTime>,
    pub length: Duration,
}

/// Where usage stands against an even pace through the window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pace {
    /// Percent that would be used by now at an even pace.
    pub expected: f32,
    /// When the limit runs out at the rate so far, if before it resets.
    pub runs_out: Option<Duration>,
}

impl Limit {
    /// Nothing until a hundredth of the window has passed: an early
    /// estimate swings too far to be useful.
    pub fn pace(&self, now: SystemTime) -> Option<Pace> {
        let left = self.resets_at?.duration_since(now).ok()?;
        let elapsed = self.length.checked_sub(left)?;
        if elapsed < self.length / 100 {
            return None;
        }
        let expected = (elapsed.as_secs_f64() / self.length.as_secs_f64() * 100.) as f32;
        let runs_out = (self.used > 0.)
            .then(|| Duration::from_secs_f64(f64::from(100. - self.used.min(100.)) * elapsed.as_secs_f64() / f64::from(self.used)))
            .filter(|t| *t < left);
        Some(Pace { expected, runs_out })
    }

    /// Time until it resets, as `4d 6h`.
    pub fn resets_in(&self, now: SystemTime) -> Option<String> {
        Some(countdown(self.resets_at?.duration_since(now).unwrap_or_default()))
    }
}

impl Pace {
    /// `11% in reserve · Lasts until reset`.
    pub fn describe(&self, used: f32) -> String {
        if used >= 100. {
            return "Ran out".into();
        }
        let margin = self.expected - used;
        let standing = if margin >= 0.5 {
            format!("{}% in reserve", margin.round())
        } else if margin <= -0.5 {
            format!("{}% in deficit", (-margin).round())
        } else {
            "On pace".into()
        };
        let outlook = self.runs_out.map_or_else(|| "Lasts until reset".into(), |t| format!("Runs out in {}", countdown(t)));
        format!("{standing} · {outlook}")
    }
}

/// What the service says about the account.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Report {
    /// `Max 5x`, from the sign-in.
    pub plan: Option<String>,
    pub email: Option<String>,
    pub limits: Vec<Limit>,
    /// Usage credits past the plan's limits: `Off`, `$1.20`, `$1.20 of $50.00`.
    pub extra: Option<String>,
}

/// Where an agent's account quota comes from, for the usage monitor: one
/// per agent that has `Capability::Usage` ([`usage_source`] is the table).
/// Accounts are Claude's (see [`crate::accounts`]) until another agent has them.
pub(crate) trait UsageSource: Send + Sync {
    /// The agent it reads for.
    fn agent(&self) -> &'static str;
    /// Read account `account`'s usage once, blocking; `forced` when the
    /// user asked for it now.
    fn read(&self, account: &str, forced: bool) -> Outcome;
}

/// Claude's quota: the endpoint behind Claude Code's `/usage`.
struct ClaudeUsage;

impl UsageSource for ClaudeUsage {
    fn agent(&self) -> &'static str {
        tern_agent::CLAUDE
    }

    fn read(&self, account: &str, forced: bool) -> Outcome {
        read(account, forced)
    }
}

/// Codex's quota: what its latest conversation was told, no network call.
struct CodexUsage;

impl UsageSource for CodexUsage {
    fn agent(&self) -> &'static str {
        tern_agent::CODEX
    }

    fn read(&self, _: &str, _: bool) -> Outcome {
        let now = SystemTime::now();
        let report = tern_chat::codex::rollout::latest_rate_limits(&tern_chat::codex::rollout::codex_home()).and_then(|l| codex_report(&l));
        Outcome { read_at: report.as_ref().map(|_| now), report, error: None, next_at: now + AGENT_READ_EVERY, rejected: false }
    }
}

/// One of Codex's limits, from `primary` or `secondary` in its `rate_limits`.
fn codex_limit(window: &serde_json::Value) -> Option<Limit> {
    let minutes = window.get("window_minutes")?.as_u64()?;
    let (name, short) = match minutes {
        300 => ("Session".to_owned(), "5h".to_owned()),
        10_080 => ("Weekly".to_owned(), "W".to_owned()),
        m if m % 1440 == 0 => (format!("{} days", m / 1440), format!("{}d", m / 1440)),
        m => (format!("{} hours", m / 60), format!("{}h", m / 60)),
    };
    Some(Limit {
        name,
        short,
        used: window.get("used_percent")?.as_f64()? as f32,
        resets_at: window.get("resets_at").and_then(serde_json::Value::as_u64).map(|s| UNIX_EPOCH + Duration::from_secs(s)),
        length: Duration::from_secs(minutes * 60),
    })
}

/// What Codex's `rate_limits` say; nothing without a limit.
fn codex_report(limits: &serde_json::Value) -> Option<Report> {
    let found: Vec<_> = ["primary", "secondary"].iter().filter_map(|k| codex_limit(limits.get(*k)?)).collect();
    if found.is_empty() {
        return None;
    }
    let plan = limits.get("plan_type").and_then(serde_json::Value::as_str).map(|p| {
        let mut c = p.chars();
        c.next().map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
    });
    Some(Report { plan, limits: found, ..Report::default() })
}

/// The agent whose accounts the usage monitor shows: accounts are Claude's
/// (see [`crate::accounts`]).
pub(crate) const ACCOUNT_AGENT: &str = tern_agent::CLAUDE;

/// The usage source of agent `agent`, if Tern can read its quota.
pub(crate) fn usage_source(agent: &str) -> Option<Box<dyn UsageSource>> {
    match agent {
        tern_agent::CLAUDE => Some(Box::new(ClaudeUsage)),
        tern_agent::CODEX => Some(Box::new(CodexUsage)),
        _ => None,
    }
}

/// What a read found, from the service or from the cache.
pub(crate) struct Outcome {
    /// `None`: signed out, or nothing read yet.
    report: Option<Report>,
    error: Option<String>,
    read_at: Option<SystemTime>,
    /// When anyone may ask the service again.
    next_at: SystemTime,
    /// The service rejected the sign-in: signing in again fixes it.
    rejected: bool,
}

/// The last answer, kept on disk so neither a restart nor another Tern
/// running (the installed one and a dev build) asks the rate-limited
/// endpoint again before it is due.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Cached {
    /// Unix seconds of the last good answer.
    read_at: u64,
    /// Unix seconds before which nobody asks again.
    next_at: u64,
    /// The last good answer, as the service gave it.
    body: Option<String>,
    plan: Option<String>,
    email: Option<String>,
    /// Why the last ask failed, if it did.
    error: Option<String>,
    /// Sign-in rejections in a row.
    rejections: u32,
    /// The rejected sign-in (see [`fingerprint`]): another one, from signing
    /// in again, is asked at once.
    #[serde(default)]
    rejected_token: Option<String>,
}

impl Cached {
    /// Whether this answer is of the account now signed in as `email`
    /// (either unknown: it may be).
    fn is_of(&self, email: Option<&str>) -> bool {
        match (self.email.as_deref(), email) {
            (Some(saved), Some(now)) => saved.eq_ignore_ascii_case(now),
            _ => true,
        }
    }

    fn outcome(&self) -> Outcome {
        let report =
            self.body.as_deref().and_then(|b| parse(b).ok()).map(|r| Report { plan: self.plan.clone(), email: self.email.clone(), ..r });
        Outcome {
            read_at: report.as_ref().map(|_| UNIX_EPOCH + Duration::from_secs(self.read_at)),
            report,
            error: self.error.clone(),
            next_at: UNIX_EPOCH + Duration::from_secs(self.next_at),
            rejected: self.rejections > 0,
        }
    }
}

fn cache_path(account: &str) -> PathBuf {
    match account {
        accounts::DEFAULT => crate::dirs::cache().join("claude-usage.json"),
        id => crate::dirs::cache().join(format!("claude-usage-{id}.json")),
    }
}

/// Deletes removed account `account`'s numbers.
pub fn forget_cached(account: &str) {
    if account != accounts::DEFAULT {
        let _ = std::fs::remove_file(cache_path(account));
    }
}

/// Account `account`'s last answer, unless it was another person's: the
/// account signed in again as someone else, or a Tern that took another
/// account's folder for this one saved it.
fn load(account: &str) -> Option<Cached> {
    let c: Cached = serde_json::from_slice(&std::fs::read(cache_path(account)).ok()?).ok()?;
    c.is_of(accounts::email(account).as_deref()).then_some(c)
}

fn save(account: &str, c: &Cached) {
    let path = cache_path(account);
    let Ok(text) = serde_json::to_vec(c) else { return };
    // Whole or not at all: another Tern may read it meanwhile.
    let tmp = path.with_extension(format!("json.{}", std::process::id()));
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
    if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

fn secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

/// The app's one usage store.
pub struct Usage(pub Entity<UsageStore>);

impl Global for Usage {}

pub struct UsageStore {
    /// The agent its accounts' quota is read for (see [`ACCOUNT_AGENT`]).
    agent: &'static str,
    accounts: HashMap<String, AccountUsage>,
    /// The quota of agents with no accounts, by agent.
    agents: HashMap<String, AccountUsage>,
    seen: Option<Instant>,
    _task: Task<()>,
    _agents_task: Task<()>,
}

/// One account's numbers.
pub struct AccountUsage {
    pub report: Option<Report>,
    /// Why the last read failed; the report, if any, is older.
    pub error: Option<String>,
    pub updated: Option<SystemTime>,
    pub reading: bool,
    /// The service (or the cache) answered once: without numbers or an
    /// error, the account isn't signed in.
    pub checked: bool,
    /// The service rejected its sign-in.
    pub rejected: bool,
    due: Instant,
    /// A click asked for this read.
    forced: bool,
    manual: Option<Instant>,
}

impl AccountUsage {
    /// An agent's, nothing read yet.
    fn blank() -> Self {
        Self {
            report: None,
            error: None,
            updated: None,
            reading: false,
            checked: false,
            rejected: false,
            due: Instant::now(),
            forced: false,
            manual: None,
        }
    }

    /// The last answer at once, from the cache; the first read asks the
    /// service only if it is due.
    fn new(account: &str) -> Self {
        let cached = load(account).map(|c| c.outcome());
        Self {
            report: cached.as_ref().and_then(|c| c.report.clone()),
            error: cached.as_ref().and_then(|c| c.error.clone()),
            checked: cached.is_some(),
            rejected: cached.as_ref().is_some_and(|c| c.rejected),
            updated: cached.and_then(|c| c.read_at),
            reading: false,
            due: Instant::now(),
            forced: false,
            manual: None,
        }
    }
}

/// The usage store, made on first use.
pub fn store(cx: &mut App) -> Entity<UsageStore> {
    if let Some(u) = cx.try_global::<Usage>() {
        return u.0.clone();
    }
    let store = cx.new(UsageStore::new);
    cx.set_global(Usage(store.clone()));
    store
}

impl UsageStore {
    fn new(cx: &mut Context<Self>) -> Self {
        let source: Option<std::sync::Arc<dyn UsageSource>> = usage_source(ACCOUNT_AGENT).map(Into::into);
        let agent = source.as_ref().map_or(ACCOUNT_AGENT, |s| s.agent());
        let task = cx.spawn(async move |this, cx| {
            let Some(source) = source else { return };
            loop {
                let go = this.update(cx, |this, cx| {
                    let active: Vec<String> = accounts::store(cx).read(cx).active().iter().map(|a| a.id.clone()).collect();
                    let now = Instant::now();
                    let looked = this.seen.is_some_and(|s| now.duration_since(s) < UNSEEN);
                    // One account a tick: the service is asked one at a time.
                    // One a click asked for first, then the longest due.
                    if !looked {
                        return None;
                    }
                    let id = active
                        .into_iter()
                        .filter_map(|id| {
                            let a = this.account(&id);
                            a.should_read(now).then_some(((a.forced, std::cmp::Reverse(a.due)), id))
                        })
                        .max_by(|a, b| a.0.cmp(&b.0))?
                        .1;
                    let a = this.account(&id);
                    a.reading = true;
                    let forced = std::mem::take(&mut a.forced);
                    cx.notify();
                    Some((id, forced))
                });
                match go {
                    Ok(Some((id, forced))) => {
                        let account = id.clone();
                        let source = source.clone();
                        let outcome = cx.background_executor().spawn(async move { source.read(&account, forced) }).await;
                        if this.update(cx, |this, cx| this.take(&id, outcome, cx)).is_err() {
                            break;
                        }
                    },
                    Ok(None) => {},
                    Err(_) => break,
                }
                cx.background_executor().timer(Duration::from_secs(2)).await;
            }
        });
        let sources: Vec<Box<dyn UsageSource>> =
            tern_agent::registry().all().filter(|d| d.id.as_str() != ACCOUNT_AGENT).filter_map(|d| usage_source(d.id.as_str())).collect();
        let sources: Vec<std::sync::Arc<dyn UsageSource>> = sources.into_iter().map(Into::into).collect();
        let agents_task = cx.spawn(async move |this, cx| {
            loop {
                for source in &sources {
                    let due = this.update(cx, |this, _| {
                        let now = Instant::now();
                        let looked = this.seen.is_some_and(|s| now.duration_since(s) < UNSEEN);
                        looked && this.agents.entry(source.agent().to_owned()).or_insert_with(AccountUsage::blank).should_read(now)
                    });
                    match due {
                        Ok(true) => {
                            let read = source.clone();
                            let outcome = cx.background_executor().spawn(async move { read.read("", false) }).await;
                            if this.update(cx, |this, cx| this.take_agent(source.agent(), outcome, cx)).is_err() {
                                return;
                            }
                        },
                        Ok(false) => {},
                        Err(_) => return,
                    }
                }
                cx.background_executor().timer(Duration::from_secs(2)).await;
            }
        });
        Self { agent, accounts: HashMap::new(), agents: HashMap::new(), seen: None, _task: task, _agents_task: agents_task }
    }

    /// Agent `agent`'s numbers, when it has no accounts.
    pub fn agent_usage(&mut self, agent: &str) -> &AccountUsage {
        self.agents.entry(agent.to_owned()).or_insert_with(AccountUsage::blank)
    }

    /// Read agent `agent`'s numbers at the next tick.
    pub fn refresh_agent(&mut self, agent: &str) {
        self.seen = Some(Instant::now());
        self.agents.entry(agent.to_owned()).or_insert_with(AccountUsage::blank).due = Instant::now();
    }

    fn take_agent(&mut self, agent: &str, o: Outcome, cx: &mut Context<Self>) {
        let a = self.agents.entry(agent.to_owned()).or_insert_with(AccountUsage::blank);
        a.checked = true;
        a.report = o.report;
        a.error = o.error;
        a.updated = o.read_at;
        a.due = Instant::now() + AGENT_READ_EVERY;
        cx.notify();
    }

    /// The agent whose quota it shows: its cards carry that agent's logo.
    pub fn agent(&self) -> tern_agent::AgentId {
        tern_agent::AgentId::new(self.agent)
    }

    fn account(&mut self, id: &str) -> &mut AccountUsage {
        self.accounts.entry(id.to_owned()).or_insert_with(|| AccountUsage::new(id))
    }

    /// Drops removed account `id`'s numbers.
    pub fn forget(&mut self, id: &str) {
        self.accounts.remove(id);
    }

    /// Account `id`'s numbers, read from the cache the first time.
    pub fn get(&mut self, id: &str) -> &AccountUsage {
        self.account(id)
    }

    /// A bar shows the numbers: keep them fresh.
    pub fn seen(&mut self) {
        self.seen = Some(Instant::now());
    }

    /// Read account `id` now, unless a click just did (the service's own
    /// limits and backoff still hold: see [`read`]).
    pub fn refresh(&mut self, id: &str, cx: &mut Context<Self>) {
        let now = Instant::now();
        self.seen = Some(now);
        let a = self.account(id);
        if a.manual.is_some_and(|m| now.duration_since(m) < MANUAL_SPACING) {
            return;
        }
        a.manual = Some(now);
        a.forced = true;
        a.due = now;
        cx.notify();
    }

    fn take(&mut self, id: &str, o: Outcome, cx: &mut Context<Self>) {
        // Removed meanwhile.
        let Some(a) = self.accounts.get_mut(id) else { return };
        a.reading = false;
        a.checked = true;
        a.report = o.report;
        a.error = o.error;
        a.updated = o.read_at;
        a.rejected = o.rejected;
        let mut wait = o.next_at.duration_since(SystemTime::now()).unwrap_or_default();
        // A rejected sign-in waits long to ask the service again, but
        // signing in again (here or in another Tab) is seen sooner: `read`
        // asks at once when the sign-in changed.
        if o.rejected {
            wait = wait.min(REFRESH);
        }
        // Not before the next tick: a cache another Tern keeps stale must
        // not make this one spin.
        a.due = Instant::now() + wait.max(MANUAL_SPACING);
        cx.notify();
    }
}

impl AccountUsage {
    /// Signing in (again) is what it needs: it never did, or the service
    /// rejected its sign-in.
    pub fn needs_sign_in(&self) -> bool {
        self.rejected || (self.checked && !self.reading && self.report.is_none() && self.error.is_none())
    }

    fn should_read(&self, now: Instant) -> bool {
        !self.reading && now >= self.due
    }
}

/// Reads account `account`'s usage once, blocking. The service is asked
/// only when the cache (shared with other Terns) says it is due; a click
/// (`forced`) asks sooner, but not within [`FORCED_FRESH`] of the last
/// answer nor while the service said to wait.
fn read(account: &str, forced: bool) -> Outcome {
    let now = SystemTime::now();
    let cached = load(account).unwrap_or_default();
    let next_at = UNIX_EPOCH + Duration::from_secs(cached.next_at);
    let fresh = UNIX_EPOCH + Duration::from_secs(cached.read_at) + FORCED_FRESH > now;
    let waiting = cached.error.is_some() && next_at > now;
    // Signed in again since the service rejected the sign-in: ask now.
    let renewed = cached.rejections > 0 && token_fingerprint(account) != cached.rejected_token;
    if !renewed && if forced { fresh || waiting } else { next_at > now } {
        return cached.outcome();
    }
    let mut c = cached;
    let result = ask(account);
    let now = SystemTime::now();
    let wait = match result {
        Ask::Done { body, plan, email } => {
            c = Cached { read_at: secs(now), body: Some(body), plan, email, ..Cached::default() };
            REFRESH
        },
        Ask::SignedOut => {
            c = Cached::default();
            REFRESH
        },
        Ask::RateLimited => {
            c.error = Some("Rate limited by Claude; trying again later".into());
            RATE_LIMITED
        },
        Ask::Rejected { token } => {
            c.rejections += 1;
            c.rejected_token = Some(token);
            c.error = Some("Claude rejected the sign-in; run Claude Code to renew it".into());
            (REFRESH * 2u32.saturating_pow(c.rejections - 1)).min(REJECTED_LIMIT)
        },
        Ask::Failed(e) => {
            c.error = Some(e);
            ERROR_BACKOFF
        },
    };
    c.next_at = secs(now + wait);
    save(account, &c);
    c.outcome()
}

/// What the service answered.
enum Ask {
    Done {
        body: String,
        plan: Option<String>,
        email: Option<String>,
    },
    /// The account has no Claude Code sign-in (or an API-key one).
    SignedOut,
    RateLimited,
    /// The service refused the sign-in (it expired; Claude Code renews it
    /// when it runs).
    /// `token`: the sign-in's [`fingerprint`].
    Rejected {
        token: String,
    },
    Failed(String),
}

/// Asks the service, blocking: the account's sign-in, then the endpoint.
fn ask(account: &str) -> Ask {
    let Some(credentials) = credentials(account) else { return Ask::SignedOut };
    let Some(oauth) = credentials.claude_ai_oauth else { return Ask::SignedOut };
    let Some(token) = oauth.access_token.filter(|t| !t.is_empty()) else { return Ask::SignedOut };
    let (status, body) = match get(&token) {
        Ok(r) => r,
        Err(e) => return Ask::Failed(e),
    };
    match status {
        // Checked now, so an answer it can't read isn't kept.
        200 => match parse(&body) {
            Ok(_) => Ask::Done {
                body,
                plan: plan(oauth.subscription_type.as_deref(), oauth.rate_limit_tier.as_deref()),
                email: accounts::email(account),
            },
            Err(e) => Ask::Failed(e),
        },
        401 | 403 => Ask::Rejected { token: fingerprint(&token) },
        429 => Ask::RateLimited,
        s => Ask::Failed(format!("Claude answered {s}")),
    }
}

#[derive(Deserialize)]
struct Credentials {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: Option<OAuth>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OAuth {
    access_token: Option<String>,
    subscription_type: Option<String>,
    rate_limit_tier: Option<String>,
}

/// A sign-in's token, told apart without keeping it.
fn fingerprint(token: &str) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(token.as_bytes()).iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// The [`fingerprint`] of account `account`'s sign-in now, if it has one.
fn token_fingerprint(account: &str) -> Option<String> {
    let token = credentials(account)?.claude_ai_oauth?.access_token.filter(|t| !t.is_empty())?;
    Some(fingerprint(&token))
}

/// Account `account`'s Claude Code sign-in: its keychain item on a Mac,
/// else its credentials file.
fn credentials(account: &str) -> Option<Credentials> {
    let from_file =
        || std::fs::read(accounts::claude_dir(account)?.join(".credentials.json")).ok().and_then(|b| serde_json::from_slice(&b).ok());
    keychain(&accounts::keychain_service(account)).or_else(from_file)
}

/// Claude Code keeps its sign-in under the user's account name. Asked by
/// service alone, `security` may answer with another account's item first
/// (one `sudo claude` left under `root`), so the user's own is asked first.
#[cfg(target_os = "macos")]
fn keychain(service: &str) -> Option<Credentials> {
    let user = std::env::var("USER").ok();
    [user.as_deref(), None].into_iter().find_map(|account| {
        let mut c = Command::new("/usr/bin/security");
        c.args(["find-generic-password", "-s", service]);
        if let Some(a) = account {
            c.args(["-a", a]);
        }
        let out = c.arg("-w").stderr(Stdio::null()).output().ok().filter(|o| o.status.success())?;
        let creds: Credentials = serde_json::from_slice(&out.stdout).ok()?;
        creds.claude_ai_oauth.as_ref()?.access_token.as_ref()?;
        Some(creds)
    })
}

#[cfg(not(target_os = "macos"))]
fn keychain(_service: &str) -> Option<Credentials> {
    None
}

/// GETs the endpoint with WinHTTP: a hidden curl.exe handed a sign-in is
/// what antivirus software flags.
#[cfg(windows)]
fn get(token: &str) -> Result<(u16, String), String> {
    let mut body = vec![];
    let auth = format!("Bearer {token}");
    let req = crate::winhttp::Request {
        method: "GET",
        url: URL,
        agent: AGENT,
        headers: &[("Authorization", &auth), ("anthropic-beta", BETA)],
        follow_redirects: false,
        max_time: Duration::from_secs(30),
    };
    let answer = crate::winhttp::send(&req, &mut body)?;
    Ok((answer.status, String::from_utf8_lossy(&body).into_owned()))
}

/// GETs the endpoint with the system's curl. The token goes in on stdin, so
/// it never shows in the process list.
#[cfg(not(windows))]
fn get(token: &str) -> Result<(u16, String), String> {
    let curl = if cfg!(target_os = "macos") { "/usr/bin/curl" } else { "curl" };
    let mut child = Command::new(curl)
        .args(["-sS", "--connect-timeout", "15", "--max-time", "30", "--proto", "=https", "-A", AGENT])
        .args(["-H", "@-", "-w", "\n%{http_code}", URL])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = write!(stdin, "Authorization: Bearer {token}\nanthropic-beta: {BETA}\n");
    }
    let out = child.wait_with_output().map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(err.trim().trim_start_matches("curl: ").to_owned());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let (body, status) = text.rsplit_once('\n').ok_or("curl gave no status")?;
    Ok((status.trim().parse().map_err(|_| "curl gave no status")?, body.to_owned()))
}

#[derive(Deserialize)]
struct Answer {
    limits: Option<Vec<LimitAnswer>>,
    five_hour: Option<Fixed>,
    seven_day: Option<Fixed>,
    spend: Option<Spend>,
}

#[derive(Deserialize)]
struct LimitAnswer {
    kind: String,
    percent: Option<f64>,
    resets_at: Option<String>,
    scope: Option<Scope>,
}

#[derive(Deserialize)]
struct Scope {
    model: Option<Model>,
}

#[derive(Deserialize)]
struct Model {
    display_name: Option<String>,
}

#[derive(Deserialize)]
struct Fixed {
    utilization: Option<f64>,
    resets_at: Option<String>,
}

#[derive(Deserialize)]
struct Spend {
    enabled: Option<bool>,
    used: Option<Money>,
    limit: Option<Money>,
}

#[derive(Deserialize)]
struct Money {
    amount_minor: i64,
    currency: String,
    exponent: u32,
}

impl Money {
    /// `$12.34`, or `12.34 EUR`.
    fn text(&self) -> String {
        let exp = self.exponent.min(6);
        let scale = 10u64.pow(exp);
        let sign = if self.amount_minor < 0 { "-" } else { "" };
        let n = self.amount_minor.unsigned_abs();
        let number = if exp == 0 { n.to_string() } else { format!("{}.{:0w$}", n / scale, n % scale, w = exp as usize) };
        match self.currency.trim() {
            "USD" => format!("{sign}${number}"),
            c => format!("{sign}{number} {c}"),
        }
    }
}

fn parse(body: &str) -> Result<Report, String> {
    let a: Answer = serde_json::from_str(body).map_err(|e| format!("Unexpected answer from Claude: {e}"))?;
    let mut limits: Vec<Limit> = a
        .limits
        .unwrap_or_default()
        .into_iter()
        .filter_map(|l| {
            let (name, short, length) = match l.kind.as_str() {
                "session" => ("Session".to_owned(), "5h".to_owned(), SESSION),
                "weekly_all" => ("Weekly".to_owned(), "W".to_owned(), WEEK),
                "weekly_scoped" => {
                    let model = l.scope?.model?.display_name.filter(|n| !n.trim().is_empty())?;
                    (format!("{model} weekly"), model, WEEK)
                },
                _ => return None,
            };
            Some(Limit { name, short, used: l.percent? as f32, resets_at: l.resets_at.as_deref().and_then(timestamp), length })
        })
        .collect();
    if limits.is_empty() {
        let fixed = [("Session", "5h", a.five_hour, SESSION), ("Weekly", "W", a.seven_day, WEEK)];
        limits = fixed
            .into_iter()
            .filter_map(|(name, short, w, length)| {
                let w = w?;
                Some(Limit {
                    name: name.into(),
                    short: short.into(),
                    used: w.utilization? as f32,
                    resets_at: w.resets_at.as_deref().and_then(timestamp),
                    length,
                })
            })
            .collect();
    }
    if limits.is_empty() {
        return Err("Claude reported no limits".into());
    }
    let extra = a.spend.map(|s| match (s.enabled.unwrap_or(false), s.used, s.limit) {
        (false, _, _) => "Off".to_owned(),
        (true, Some(used), Some(limit)) => format!("{} of {}", used.text(), limit.text()),
        (true, Some(used), None) => used.text(),
        (true, None, _) => "On".to_owned(),
    });
    Ok(Report { plan: None, email: None, limits, extra })
}

/// `max` on tier `default_claude_max_20x` is `Max 20x`.
fn plan(subscription: Option<&str>, tier: Option<&str>) -> Option<String> {
    let s = subscription?.trim();
    let mut name: String = s.chars().take(1).flat_map(char::to_uppercase).chain(s.chars().skip(1)).collect();
    if name.is_empty() {
        return None;
    }
    let multiple = tier
        .and_then(|t| t.rsplit('_').next())
        .filter(|last| last.strip_suffix('x').is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())));
    if let Some(m) = multiple {
        name = format!("{name} {m}");
    }
    Some(name)
}

/// An RFC 3339 time (`2026-10-20T07:59:59.879107+00:00`).
fn timestamp(s: &str) -> Option<SystemTime> {
    let (date, rest) = s.split_once(['T', ' '])?;
    let mut d = date.split('-').map(str::parse::<i64>);
    let (y, m, day) = (d.next()?.ok()?, d.next()?.ok()?, d.next()?.ok()?);
    let zone_at = rest.find(['Z', 'z', '+', '-']).unwrap_or(rest.len());
    let (time, zone) = rest.split_at(zone_at);
    let mut t = time.split(':');
    let (h, min) = (t.next()?.parse::<i64>().ok()?, t.next()?.parse::<i64>().ok()?);
    let sec: f64 = t.next().unwrap_or("0").parse().ok()?;
    let offset = match zone.chars().next() {
        Some(sign @ ('+' | '-')) => {
            let (oh, om) = zone[1..].split_once(':').unwrap_or((&zone[1..3.min(zone.len())], "0"));
            let o = oh.parse::<i64>().ok()? * 3600 + om.parse::<i64>().ok()? * 60;
            if sign == '-' { -o } else { o }
        },
        _ => 0,
    };
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + h * 3600 + min * 60 - offset;
    let at = Duration::from_secs_f64(secs as f64 + sec);
    (secs >= 0).then(|| UNIX_EPOCH + at)
}

/// `4d 6h`, `2h 10m`, `5m`.
pub fn countdown(t: Duration) -> String {
    let m = t.as_secs().div_ceil(60);
    let (d, h, m) = (m / (24 * 60), m / 60 % 24, m % 60);
    match (d, h) {
        (0, 0) => format!("{m}m"),
        (0, _) => format!("{h}h {m}m"),
        _ => format!("{d}d {h}h"),
    }
}

/// `2m ago`, for when the numbers were read.
pub fn ago(t: SystemTime, now: SystemTime) -> String {
    let s = now.duration_since(t).unwrap_or_default().as_secs();
    match s {
        0..60 => "just now".into(),
        60..3600 => format!("{}m ago", s / 60),
        _ => format!("{}h ago", s / 3600),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn codex_report_names_its_windows() {
        let limits = serde_json::json!({
            "primary": {"used_percent": 12.5, "window_minutes": 300, "resets_at": 1_795_000_565},
            "secondary": {"used_percent": 40.0, "window_minutes": 10080, "resets_at": 1_795_100_000},
            "plan_type": "plus",
        });
        let r = super::codex_report(&limits).unwrap();
        let names: Vec<_> = r.limits.iter().map(|l| (l.name.as_str(), l.short.as_str(), l.used)).collect();
        assert_eq!(names, [("Session", "5h", 12.5), ("Weekly", "W", 40.)]);
        assert_eq!(r.plan.as_deref(), Some("Plus"));
        assert_eq!(r.limits[0].resets_at, Some(UNIX_EPOCH + Duration::from_secs(1_795_000_565)));
        assert_eq!(r.limits[1].length, Duration::from_secs(7 * 24 * 60 * 60));
    }

    #[test]
    fn a_codex_window_of_another_length_says_its_length() {
        let limits = serde_json::json!({"primary": {"used_percent": 1.0, "window_minutes": 43200, "resets_at": 1}, "secondary": null, "plan_type": "free"});
        let r = super::codex_report(&limits).unwrap();
        assert_eq!((r.limits[0].name.as_str(), r.limits[0].short.as_str()), ("30 days", "30d"));
    }

    #[test]
    fn codex_report_without_limits_is_none() {
        assert_eq!(super::codex_report(&serde_json::json!({"primary": null, "secondary": null})), None);
    }

    use std::time::{Duration, UNIX_EPOCH};

    use super::{Cached, Limit, WEEK, countdown, parse, plan, timestamp, usage_source};

    #[test]
    fn claude_and_codex_have_a_usage_source() {
        assert_eq!(usage_source(tern_agent::CLAUDE).map(|s| s.agent()), Some(tern_agent::CLAUDE));
        assert_eq!(usage_source("codex").map(|s| s.agent()), Some(tern_agent::CODEX));
        assert!(usage_source("opencode").is_none());
    }

    const ANSWER: &str = r#"{
        "five_hour": {"utilization": 27.0, "resets_at": "2026-10-20T07:59:59.879107+00:00"},
        "seven_day": {"utilization": 8.0, "resets_at": "2026-10-26T07:59:59.879125+00:00"},
        "spend": {"used": {"amount_minor": 120, "currency": "USD", "exponent": 2}, "limit": null, "enabled": true},
        "limits": [
            {"kind": "session", "percent": 27, "resets_at": "2026-10-20T07:59:59.879107+00:00", "scope": null},
            {"kind": "weekly_all", "percent": 8, "resets_at": "2026-10-26T07:59:59.879125+00:00", "scope": null},
            {"kind": "weekly_scoped", "percent": 0, "resets_at": "2026-10-26T08:00:00+00:00",
             "scope": {"model": {"id": null, "display_name": "Fable"}, "surface": null}},
            {"kind": "something_new", "percent": 3}
        ]
    }"#;

    #[test]
    fn parses_limits() {
        let r = parse(ANSWER).unwrap();
        let names: Vec<_> = r.limits.iter().map(|l| (l.name.as_str(), l.short.as_str(), l.used)).collect();
        assert_eq!(names, [("Session", "5h", 27.), ("Weekly", "W", 8.), ("Fable weekly", "Fable", 0.)]);
        assert_eq!(r.extra.as_deref(), Some("$1.20"));
    }

    #[test]
    fn cached_answers() {
        let c = Cached { read_at: 100, next_at: 700, body: Some(ANSWER.into()), plan: Some("Max 5x".into()), ..Cached::default() };
        let back: Cached = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back, c);
        let o = c.outcome();
        let r = o.report.unwrap();
        assert_eq!((r.limits.len(), r.plan.as_deref()), (3, Some("Max 5x")));
        assert_eq!(o.read_at, Some(UNIX_EPOCH + Duration::from_secs(100)));
        assert_eq!(o.next_at, UNIX_EPOCH + Duration::from_secs(700));
        // Signed out, or an answer it can't read: no numbers.
        assert!(Cached { body: Some("<html>".into()), ..c }.outcome().report.is_none());
        assert!(Cached::default().outcome().read_at.is_none());
    }

    #[test]
    fn a_rejected_sign_in_is_kept_with_its_token() {
        // A cache from before the token was kept still reads.
        let old: Cached =
            serde_json::from_str(r#"{"read_at":1,"next_at":2,"body":null,"plan":null,"email":null,"error":"x","rejections":3}"#).unwrap();
        assert_eq!(old.rejected_token, None);
        assert!(old.outcome().rejected);
        assert!(!Cached::default().outcome().rejected);
        use super::fingerprint;
        assert_eq!(fingerprint("abc"), fingerprint("abc"));
        assert_ne!(fingerprint("abc"), fingerprint("abd"));
        assert_eq!(fingerprint("abc").len(), 16);
    }

    #[test]
    fn cached_answer_of_another_person_is_not_kept() {
        let c = Cached { email: Some("a@x.com".into()), ..Cached::default() };
        assert!(c.is_of(Some("A@x.com")));
        assert!(!c.is_of(Some("b@x.com")));
        assert!(c.is_of(None));
        assert!(Cached::default().is_of(Some("b@x.com")));
    }

    #[test]
    fn falls_back_to_fixed_windows() {
        let r = parse(r#"{"five_hour": {"utilization": 50, "resets_at": null}, "seven_day": null}"#).unwrap();
        assert_eq!(r.limits.len(), 1);
        assert_eq!(r.limits[0].used, 50.);
        assert!(r.extra.is_none());
        assert!(parse(r#"{"limits": []}"#).is_err());
        assert!(parse("<html>").is_err());
    }

    #[test]
    fn timestamps() {
        let t = timestamp("2026-10-20T07:59:59.5+00:00").unwrap();
        assert_eq!(t.duration_since(UNIX_EPOCH).unwrap().as_millis(), 1_792_483_199_500);
        assert_eq!(timestamp("2026-10-20T09:59:59.5+02:00"), Some(t));
        assert_eq!(timestamp("2026-10-20T07:59:59.5Z"), Some(t));
        assert_eq!(timestamp("nope"), None);
    }

    #[test]
    fn plans() {
        assert_eq!(plan(Some("max"), Some("default_claude_max_20x")).as_deref(), Some("Max 20x"));
        assert_eq!(plan(Some("pro"), None).as_deref(), Some("Pro"));
        assert_eq!(plan(Some("team"), Some("default_claude_max_5x")).as_deref(), Some("Team 5x"));
        assert_eq!(plan(None, Some("x_5x")), None);
    }

    #[test]
    fn pace() {
        let now = UNIX_EPOCH + Duration::from_secs(1_000_000);
        // Half the week gone, a quarter used: a quarter in reserve, lasts.
        let l = Limit { name: "W".into(), short: "W".into(), used: 25., resets_at: Some(now + WEEK / 2), length: WEEK };
        let p = l.pace(now).unwrap();
        assert_eq!(p.describe(l.used), "25% in reserve · Lasts until reset");
        // Half gone, 80% used: runs out after another eighth of the week.
        let l = Limit { used: 80., ..l };
        let p = l.pace(now).unwrap();
        assert_eq!(p.describe(l.used), "30% in deficit · Runs out in 21h 0m");
        assert_eq!(countdown(Duration::from_secs(4 * 86_400 + 6 * 3600)), "4d 6h");
        assert_eq!(countdown(Duration::from_secs(59)), "1m");
    }
}
