//! How much of the Claude plan's limits is used, read with Claude Code's own
//! sign-in (the login keychain's `Claude Code-credentials`, else
//! `~/.claude/.credentials.json`) from the endpoint behind its `/usage`.
//! The endpoint isn't documented: an answer it doesn't recognize shows as
//! an error rather than as numbers.
//!
//! One store for the app ([`Usage`]); the usage panels only draw it. It reads
//! while a panel was drawn lately, every [`REFRESH`], and backs off when the
//! service says no. The last answer and when to ask next are kept in the
//! cache folder, shared by every Tern running, so a restart or a second
//! Tern doesn't ask again before it is due.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui_kit::*;
use serde::{Deserialize, Serialize};

const URL: &str = "https://api.anthropic.com/api/oauth/usage";
/// The endpoint serves Claude Code's OAuth clients.
const BETA: &str = "oauth-2025-04-20";
const AGENT: &str = "claude-code/2.1.0";
const KEYCHAIN: &str = "Claude Code-credentials";

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

/// What a read found, from the service or from the cache.
struct Outcome {
    /// `None`: signed out, or nothing read yet.
    report: Option<Report>,
    error: Option<String>,
    read_at: Option<SystemTime>,
    /// When anyone may ask the service again.
    next_at: SystemTime,
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
}

impl Cached {
    fn outcome(&self) -> Outcome {
        let report = self.body.as_deref().and_then(|b| parse(b).ok()).map(|r| Report { plan: self.plan.clone(), email: self.email.clone(), ..r });
        Outcome {
            read_at: report.as_ref().map(|_| UNIX_EPOCH + Duration::from_secs(self.read_at)),
            report,
            error: self.error.clone(),
            next_at: UNIX_EPOCH + Duration::from_secs(self.next_at),
        }
    }
}

fn cache_path() -> PathBuf {
    crate::dirs::cache().join("claude-usage.json")
}

fn load() -> Option<Cached> {
    serde_json::from_slice(&std::fs::read(cache_path()).ok()?).ok()
}

fn save(c: &Cached) {
    let path = cache_path();
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
    pub report: Option<Report>,
    /// Why the last read failed; the report, if any, is older.
    pub error: Option<String>,
    pub updated: Option<SystemTime>,
    pub reading: bool,
    due: Instant,
    /// A click asked for this read.
    forced: bool,
    manual: Option<Instant>,
    seen: Option<Instant>,
    _task: Task<()>,
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
        let task = cx.spawn(async move |this, cx| {
            loop {
                let go = this.update(cx, |this, cx| {
                    let go = this.should_read(Instant::now()).then_some(this.forced);
                    if go.is_some() {
                        this.reading = true;
                        this.forced = false;
                        cx.notify();
                    }
                    go
                });
                match go {
                    Ok(Some(forced)) => {
                        let outcome = cx.background_executor().spawn(async move { read(forced) }).await;
                        if this.update(cx, |this, cx| this.take(outcome, cx)).is_err() {
                            break;
                        }
                    },
                    Ok(None) => {},
                    Err(_) => break,
                }
                cx.background_executor().timer(Duration::from_secs(2)).await;
            }
        });
        // The last answer at once, from the cache; the first read asks the
        // service only if it is due.
        let cached = load().map(|c| c.outcome());
        Self {
            report: cached.as_ref().and_then(|c| c.report.clone()),
            error: cached.as_ref().and_then(|c| c.error.clone()),
            updated: cached.and_then(|c| c.read_at),
            reading: false,
            due: Instant::now(),
            forced: false,
            manual: None,
            seen: None,
            _task: task,
        }
    }

    fn should_read(&self, now: Instant) -> bool {
        !self.reading && now >= self.due && self.seen.is_some_and(|s| now.duration_since(s) < UNSEEN)
    }

    /// A panel shows the numbers: keep them fresh.
    pub fn seen(&mut self) {
        self.seen = Some(Instant::now());
    }

    /// Read now, unless a click just did (the service's own limits and
    /// backoff still hold: see [`read`]).
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        if self.manual.is_some_and(|m| now.duration_since(m) < MANUAL_SPACING) {
            return;
        }
        self.manual = Some(now);
        self.forced = true;
        self.due = now;
        self.seen = Some(now);
        cx.notify();
    }

    fn take(&mut self, o: Outcome, cx: &mut Context<Self>) {
        self.reading = false;
        self.report = o.report;
        self.error = o.error;
        self.updated = o.read_at;
        let wait = o.next_at.duration_since(SystemTime::now()).unwrap_or_default();
        // Not before the next tick: a cache another Tern keeps stale must
        // not make this one spin.
        self.due = Instant::now() + wait.max(MANUAL_SPACING);
        cx.notify();
    }
}

/// Reads the usage once, blocking. The service is asked only when the
/// cache (shared with other Terns) says it is due; a click (`forced`) asks
/// sooner, but not within [`FORCED_FRESH`] of the last answer nor while
/// the service said to wait.
fn read(forced: bool) -> Outcome {
    let now = SystemTime::now();
    let cached = load().unwrap_or_default();
    let next_at = UNIX_EPOCH + Duration::from_secs(cached.next_at);
    let fresh = UNIX_EPOCH + Duration::from_secs(cached.read_at) + FORCED_FRESH > now;
    let waiting = cached.error.is_some() && next_at > now;
    if if forced { fresh || waiting } else { next_at > now } {
        return cached.outcome();
    }
    let mut c = cached;
    let result = ask();
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
        Ask::Rejected => {
            c.rejections += 1;
            c.error = Some("Claude rejected the sign-in; run Claude Code to renew it".into());
            (REFRESH * 2u32.saturating_pow(c.rejections - 1)).min(REJECTED_LIMIT)
        },
        Ask::Failed(e) => {
            c.error = Some(e);
            ERROR_BACKOFF
        },
    };
    c.next_at = secs(now + wait);
    save(&c);
    c.outcome()
}

/// What the service answered.
enum Ask {
    Done { body: String, plan: Option<String>, email: Option<String> },
    /// No Claude Code sign-in on this machine (or an API-key one).
    SignedOut,
    RateLimited,
    /// The service refused the sign-in (it expired; Claude Code renews it
    /// when it runs).
    Rejected,
    Failed(String),
}

/// Asks the service, blocking: the sign-in, then the endpoint.
fn ask() -> Ask {
    let Some(credentials) = credentials() else { return Ask::SignedOut };
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
                email: email(),
            },
            Err(e) => Ask::Failed(e),
        },
        401 | 403 => Ask::Rejected,
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

fn claude_dir() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).or_else(|| dirs_home().map(|h| h.join(".claude")))
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Claude Code's sign-in: the keychain on a Mac, else its credentials file.
fn credentials() -> Option<Credentials> {
    let from_file = || std::fs::read(claude_dir()?.join(".credentials.json")).ok().and_then(|b| serde_json::from_slice(&b).ok());
    keychain().or_else(from_file)
}

/// Claude Code keeps its sign-in under the user's account name. Asked by
/// service alone, `security` may answer with another account's item first
/// (one `sudo claude` left under `root`), so the user's own is asked first.
#[cfg(target_os = "macos")]
fn keychain() -> Option<Credentials> {
    let user = std::env::var("USER").ok();
    [user.as_deref(), None].into_iter().find_map(|account| {
        let mut c = Command::new("/usr/bin/security");
        c.args(["find-generic-password", "-s", KEYCHAIN]);
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
fn keychain() -> Option<Credentials> {
    None
}

/// The account's email, from Claude Code's settings.
fn email() -> Option<String> {
    let mut paths: Vec<PathBuf> = claude_dir().map(|d| d.join(".claude.json")).into_iter().collect();
    paths.extend(dirs_home().map(|h| h.join(".claude.json")));
    paths.iter().find_map(|p| {
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(p).ok()?).ok()?;
        v["oauthAccount"]["emailAddress"].as_str().map(str::to_owned)
    })
}

/// GETs the endpoint with the system's curl. The token goes in on stdin, so
/// it never shows in the process list.
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
    use std::time::{Duration, UNIX_EPOCH};

    use super::{Cached, Limit, WEEK, countdown, parse, plan, timestamp};

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
