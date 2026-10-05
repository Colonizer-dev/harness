//! The GitHub account circuit breaker (issue #1074).
//!
//! When GitHub refuses the account itself — a suspension, a revoked token, or secondary rate
//! limits that keep coming — every further call is wasted and, against a suspended account, adds
//! to the problem: during one suspension a mothership failed 36 queued launches one after another,
//! each with another call. The breaker turns that into one pause per GitHub identity:
//!
//! - [`classify`] sorts a failed `gh`/`git` call's output into one [`Failure`];
//! - a [`Failure::Suspended`] or [`Failure::TokenRevoked`], or [`SECONDARY_THRESHOLD`] secondary
//!   rate limits within [`SECONDARY_WINDOW_MINUTES`], opens the breaker for the identity the
//!   mothership is using;
//! - while it is open, [`App::gh`](crate::App::gh) and [`App::git_remote`](crate::App::git_remote)
//!   — the only two ways the mothership builds a command that reaches GitHub — hand back a command
//!   that fails at once with the cause instead of calling GitHub ([`refusal`]). The queue holds its
//!   launches, the autopilot holds its publishes, and the merge loop, the merge train, the claim
//!   updater and the decisions inbox skip their ticks, so nothing is retried in a loop;
//! - one slow probe ([`probe_tick`]): a single `GET /user` every [`PROBE_EVERY_MINUTES`] minutes,
//!   or every [`SECONDARY_PROBE_FIRST_MINUTES`] minutes doubling for a secondary limit. Its first
//!   success closes the breaker: the queue moves on its next tick, and the held publishes go out
//!   one at a time. Reconnecting another token changes the identity and closes it too.
//!
//! The state is per mothership (its config directory, so tests with their own App never share
//! one) and in memory: a restart forgets it, and the first refused call opens it again.

use crate::{ApiResult, App, Shared, util::truncate};
use axum::{Json, extract::State};
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
};
use tokio::process::Command;

/// The environment variable [`mark`] sets on every command that reaches GitHub: the identity and
/// the mothership it belongs to, so `util::exec` can feed a failure back without an [`App`]. Both
/// halves are opaque — a token fingerprint and a directory — never a credential.
pub(crate) const IDENTITY_ENV: &str = "COLONIZER_GITHUB_IDENTITY";
/// Set on a [`refusal`], so `util::describe` names it instead of the shell that stands in for `gh`.
pub(crate) const REFUSAL_ENV: &str = "COLONIZER_GITHUB_PAUSED";
/// Secondary rate limits within the window that open the breaker.
pub(crate) const SECONDARY_THRESHOLD: usize = 3;
pub(crate) const SECONDARY_WINDOW_MINUTES: i64 = 10;
/// How often a suspended or revoked account is probed.
pub(crate) const PROBE_EVERY_MINUTES: i64 = 30;
/// The first probe after secondary limits opened the breaker; each failed probe doubles it, up to
/// [`PROBE_EVERY_MINUTES`].
pub(crate) const SECONDARY_PROBE_FIRST_MINUTES: i64 = 5;
/// How long the probe's one `gh api user` may take.
const PROBE_LIMIT: std::time::Duration = std::time::Duration::from_secs(20);
/// How often the probe task looks at the clock; the probe itself runs only when due.
const TICK: std::time::Duration = std::time::Duration::from_secs(60);
/// The cache key when no token is set and `gh` uses its own CLI login (as `github::viewer` keys it).
const CLI_LOGIN: &str = "gh cli login";

// ---------------------------------------------------------------------------------------------
// The classifier.
// ---------------------------------------------------------------------------------------------

/// What a failed GitHub call says about the account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// A 403 whose body says the account is suspended. Nothing on this side can fix it.
    Suspended,
    /// A 401, "Bad credentials", or git's refused token: the credential is dead.
    TokenRevoked,
    /// The refusal names a scope the token lacks. Per action, so it never opens the breaker.
    MissingScope,
    /// A 403/429 naming a secondary rate limit (or abuse detection), or carrying `Retry-After`.
    SecondaryRateLimit,
    /// Worth riding out: a 5xx, a 429, the primary rate limit, a network failure.
    Transient,
    /// Anything else: a 404, a validation error, a permission on one repository.
    Other,
}

/// The HTTP statuses a failure's text names, in the spellings `gh` and git use: `gh`'s
/// `(HTTP 403)`, `gh api -i`'s `HTTP/2.0 403` head, git's `returned error: 403`, and a JSON body's
/// `"status":"401"`.
fn statuses(lower: &str) -> Vec<u16> {
    let mut out = Vec::new();
    let mut push = |rest: &str| {
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if digits.len() == 3
            && let Ok(code) = digits.parse()
        {
            out.push(code);
        }
    };
    for (at, _) in lower.match_indices("http") {
        let rest = &lower[at + 4..];
        // `http 403`, or `http/2.0 403` / `http/1.1 403` / `http/2 403`.
        let rest = match rest.strip_prefix('/') {
            Some(version) => version.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.'),
            None => rest,
        };
        if let Some(rest) = rest.strip_prefix(' ') {
            push(rest);
        }
    }
    for marker in ["returned error: ", "\"status\":\"", "\"status\": \""] {
        for (at, _) in lower.match_indices(marker) {
            push(&lower[at + marker.len()..]);
        }
    }
    out
}

/// Whether the text names a scope the token is missing: `gh`'s "missing required scopes", GitHub's
/// GraphQL "not been granted the required scopes", a push refused "without `workflow` scope", or a
/// REST "needs the \"admin:org\" scope".
fn names_scope(lower: &str) -> bool {
    lower.contains("scope")
        && [
            "required scope",
            "following scopes",
            "` scope",
            "\" scope",
            "' scope",
            "oauth scope",
        ]
        .iter()
        .any(|m| lower.contains(m))
}

/// Sorts a failed GitHub call's output — `gh`'s stderr, `gh api -i`'s head and body, or git's
/// stderr — into one [`Failure`]. The order matters: a suspension is a 403 that would otherwise
/// read as anything else, and a secondary limit is a 403 that is not a refused credential.
pub fn classify(text: &str) -> Failure {
    let lower = text.to_ascii_lowercase();
    let codes = statuses(&lower);
    let has = |code: u16| codes.contains(&code);
    if lower.contains("suspended") && (has(403) || lower.contains("account")) {
        Failure::Suspended
    } else if lower.contains("secondary rate limit")
        || lower.contains("abuse detection")
        || ((has(403) || has(429)) && lower.contains("retry-after"))
    {
        Failure::SecondaryRateLimit
    } else if names_scope(&lower) {
        Failure::MissingScope
    } else if has(401)
        || lower.contains("bad credentials")
        || lower.contains("invalid username or token")
        || lower.contains("authentication failed for")
    {
        Failure::TokenRevoked
    } else if has(429)
        || codes.iter().any(|c| (500..600).contains(c))
        || lower.contains("api rate limit exceeded")
        || crate::github::TRANSIENT_MARKERS.iter().any(|m| lower.contains(m))
    {
        Failure::Transient
    } else {
        Failure::Other
    }
}

// ---------------------------------------------------------------------------------------------
// The breaker.
// ---------------------------------------------------------------------------------------------

/// Why the breaker is open. Snake-case names are the `/api/github/status` spelling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    Suspended,
    TokenRevoked,
    SecondaryRateLimit,
}

impl Cause {
    /// The banner's first half.
    pub fn headline(self) -> &'static str {
        match self {
            Cause::Suspended => "GitHub account suspended",
            Cause::TokenRevoked => "Token revoked",
            Cause::SecondaryRateLimit => "GitHub secondary rate limit",
        }
    }

    /// The banner's second half: what a person does next.
    pub fn next_step(self) -> &'static str {
        match self {
            Cause::Suspended => "contact GitHub support",
            Cause::TokenRevoked => "reconnect GitHub in Settings → Connections",
            Cause::SecondaryRateLimit => "nothing to do, GitHub is checked again with a growing wait",
        }
    }

    /// The wait before probe number `probes + 1`.
    fn probe_wait(self, probes: u32) -> Duration {
        match self {
            Cause::Suspended | Cause::TokenRevoked => Duration::minutes(PROBE_EVERY_MINUTES),
            Cause::SecondaryRateLimit => {
                let minutes = SECONDARY_PROBE_FIRST_MINUTES.saturating_mul(1i64 << probes.min(16));
                Duration::minutes(minutes.min(PROBE_EVERY_MINUTES))
            }
        }
    }

    fn of(failure: Failure) -> Option<Cause> {
        match failure {
            Failure::Suspended => Some(Cause::Suspended),
            Failure::TokenRevoked => Some(Cause::TokenRevoked),
            _ => None,
        }
    }
}

/// An open breaker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Open {
    pub cause: Cause,
    pub since: DateTime<Utc>,
    pub next_probe: DateTime<Utc>,
    /// Probes that failed since it opened.
    pub probes: u32,
    /// What GitHub last said, truncated.
    pub detail: String,
}

/// One mothership's breaker. Pure: the clock is passed in.
#[derive(Debug, Default)]
pub(crate) struct Breaker {
    /// The identity the open state and the secondary-limit count belong to.
    identity: String,
    open: Option<Open>,
    secondary: VecDeque<DateTime<Utc>>,
    /// Colonies whose autopilot publish waits for the breaker to close, oldest first.
    held: VecDeque<String>,
    /// Commands refused while open: each one is a call GitHub never saw.
    refused: u64,
}

impl Breaker {
    /// Switches to `identity`, dropping what belonged to another one.
    fn adopt(&mut self, identity: &str) {
        if self.identity != identity {
            self.identity = identity.to_string();
            self.open = None;
            self.secondary.clear();
        }
    }

    pub(crate) fn open_for(&self, identity: &str) -> Option<&Open> {
        self.open.as_ref().filter(|_| self.identity == identity)
    }

    /// Records a failed call. Returns whether it opened the breaker (or turned a secondary-limit
    /// pause into a suspension or a revoked token).
    pub(crate) fn observe(&mut self, identity: &str, failure: Failure, detail: &str, now: DateTime<Utc>) -> bool {
        self.adopt(identity);
        let detail = truncate(detail.trim(), 300);
        if let Some(cause) = Cause::of(failure) {
            if self.open.as_ref().is_some_and(|o| o.cause == cause) {
                return false;
            }
            let since = self.open.as_ref().map_or(now, |o| o.since);
            self.open = Some(Open {
                cause,
                since,
                next_probe: now + cause.probe_wait(0),
                probes: 0,
                detail,
            });
            return true;
        }
        if failure != Failure::SecondaryRateLimit {
            return false;
        }
        self.secondary.push_back(now);
        self.prune(now);
        if self.open.is_some() || self.secondary.len() < SECONDARY_THRESHOLD {
            return false;
        }
        let cause = Cause::SecondaryRateLimit;
        self.open = Some(Open {
            cause,
            since: now,
            next_probe: now + cause.probe_wait(0),
            probes: 0,
            detail,
        });
        true
    }

    fn prune(&mut self, now: DateTime<Utc>) {
        let window = Duration::minutes(SECONDARY_WINDOW_MINUTES);
        while self.secondary.front().is_some_and(|at| now - *at > window) {
            self.secondary.pop_front();
        }
    }

    /// A probe that failed: the cause may sharpen (a secondary limit found to be a suspension), and
    /// the next probe waits its turn.
    pub(crate) fn probe_failed(&mut self, failure: Failure, detail: &str, now: DateTime<Utc>) {
        let Some(open) = self.open.as_mut() else { return };
        if let Some(cause) = Cause::of(failure)
            && cause != open.cause
        {
            open.cause = cause;
            open.probes = 0;
        } else {
            open.probes += 1;
        }
        if matches!(
            failure,
            Failure::Suspended | Failure::TokenRevoked | Failure::SecondaryRateLimit
        ) {
            open.detail = truncate(detail.trim(), 300);
        }
        open.next_probe = now + open.cause.probe_wait(open.probes);
    }

    /// Closes the breaker. Returns whether it was open.
    pub(crate) fn close(&mut self) -> bool {
        self.secondary.clear();
        self.open.take().is_some()
    }
}

/// Every mothership's breaker, keyed by its config directory.
static BREAKERS: LazyLock<Mutex<HashMap<PathBuf, Breaker>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn with<R>(config_dir: &Path, f: impl FnOnce(&mut Breaker) -> R) -> R {
    let mut all = BREAKERS.lock().unwrap_or_else(|e| e.into_inner());
    f(all.entry(config_dir.to_path_buf()).or_default())
}

/// The identity a token (or `gh`'s own login, for `None`) stands for: a fingerprint, never the token.
pub(crate) fn identity_of(token: Option<&str>) -> String {
    crate::util::fingerprint(token.unwrap_or(CLI_LOGIN))
}

/// The identity the mothership uses right now.
pub(crate) fn identity(app: &App) -> String {
    identity_of(app.github_token().as_deref())
}

/// Whether GitHub is paused for the identity the mothership uses, and why. The one check every
/// GitHub path makes, directly or through [`App::gh`](crate::App::gh).
pub(crate) fn paused(app: &App) -> Option<Open> {
    paused_for(app, &identity(app))
}

fn paused_for(app: &App, identity: &str) -> Option<Open> {
    with(&app.cfg.config_dir, |b| b.open_for(identity).cloned())
}

/// The sentence a refused call, a held publish and a skipped tick carry.
pub(crate) fn pause_message(open: &Open) -> String {
    format!(
        "GitHub is paused for this account — {}: {}. Launches, publishes, merges and GitHub writes wait; \
         a check finds out when it works again",
        open.cause.headline(),
        open.cause.next_step()
    )
}

/// The guard inside [`App::gh`](crate::App::gh) and [`App::git_remote`](crate::App::git_remote):
/// `Some` command that fails at once with the cause while the breaker is open for `identity`,
/// `None` to go ahead. Counts each refusal.
pub(crate) fn guard(app: &App, identity: &str) -> Option<Command> {
    let open = with(&app.cfg.config_dir, |b| {
        let open = b.open_for(identity).cloned();
        if open.is_some() {
            b.refused += 1;
        }
        open
    })?;
    Some(refusal(&open))
}

/// A command that writes the pause to stderr and exits 1 without touching the network. Arguments a
/// caller appends land as unused positional parameters.
pub(crate) fn refusal(open: &Open) -> Command {
    let mut c = Command::new("/bin/sh");
    c.args(["-c", "printf '%s\\n' \"$1\" >&2; exit 1", "colonizer-github-paused"])
        .arg(pause_message(open))
        .env(REFUSAL_ENV, "1");
    c
}

/// Tags a command that reaches GitHub, so its failure finds its way back here ([`observe_command`]).
pub(crate) fn mark(app: &App, identity: &str, cmd: &mut Command) {
    cmd.env(IDENTITY_ENV, format!("{identity}@{}", app.cfg.config_dir.display()));
}

/// Feeds a failed command's output back, if [`mark`] tagged it. Called by `util::exec` and
/// `util::exec_capture`, the helpers every `gh` and network `git` call goes through.
pub(crate) fn observe_command(cmd: &Command, output: &str) {
    let tag = cmd
        .as_std()
        .get_envs()
        .find(|(k, _)| *k == IDENTITY_ENV)
        .and_then(|(_, v)| v)
        .map(|v| v.to_string_lossy().into_owned());
    let Some((identity, dir)) = tag.as_deref().and_then(|t| t.split_once('@')) else {
        return;
    };
    observe(Path::new(dir), identity, output, Utc::now());
}

/// Classifies `output` and records it against `identity`. Returns the classification.
pub(crate) fn observe(config_dir: &Path, identity: &str, output: &str, now: DateTime<Utc>) -> Failure {
    let failure = classify(output);
    if matches!(
        failure,
        Failure::Suspended | Failure::TokenRevoked | Failure::SecondaryRateLimit
    ) {
        let opened = with(config_dir, |b| {
            b.observe(identity, failure, output, now).then(|| b.open.clone())
        });
        if let Some(Some(open)) = opened {
            eprintln!(
                "github: {} — pausing launches, publishes, merges and GitHub writes; next check at {}",
                open.cause.headline(),
                open.next_probe.format("%H:%M UTC")
            );
        }
    }
    failure
}

/// Holds a colony's autopilot publish while the breaker is open: the colony keeps working locally,
/// and the publish goes out when GitHub works again. Returns the open breaker when it held.
pub(crate) fn hold_publish(app: &App, id: &str) -> Option<Open> {
    let identity = identity(app);
    with(&app.cfg.config_dir, |b| {
        let open = b.open_for(&identity).cloned()?;
        if !b.held.iter().any(|h| h == id) {
            b.held.push_back(id.to_string());
        }
        Some(open)
    })
}

// ---------------------------------------------------------------------------------------------
// The probe and the recovery.
// ---------------------------------------------------------------------------------------------

/// The one call an open breaker makes: `GET /user`, around the guard. `Err` carries GitHub's answer.
async fn probe_github(app: &App) -> Result<(), String> {
    let mut cmd = app.gh_unguarded(["api", "-i", "user"]);
    let (out, err) = crate::util::exec_capture(PROBE_LIMIT, &mut cmd)
        .await
        .map_err(|e| format!("{e:#}"))?;
    match crate::cache_store::parse_gh_include(&out) {
        Some(res) if (200..300).contains(&res.status) => Ok(()),
        _ => Err(format!("{out}\n{err}")),
    }
}

/// One look at the clock: while the breaker is open and its probe is due, probe once. Returns
/// whether the breaker closed. A changed identity (a reconnected token) closes it without a probe.
pub(crate) async fn probe_tick_with<P, Fut>(app: &App, now: DateTime<Utc>, probe: P) -> bool
where
    P: FnOnce() -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    let identity = identity(app);
    let (open, same) = with(&app.cfg.config_dir, |b| (b.open.clone(), b.identity == identity));
    let Some(open) = open else { return false };
    if same && now < open.next_probe {
        return false;
    }
    let result = if same { probe().await } else { Ok(()) };
    match result {
        Ok(()) => {
            let closed = with(&app.cfg.config_dir, Breaker::close);
            if closed {
                eprintln!("github: the account works again; the queue resumes and held publishes go out one at a time");
                *app.github_viewer.lock().await = None;
            }
            closed
        }
        Err(text) => {
            let failure = classify(&text);
            with(&app.cfg.config_dir, |b| b.probe_failed(failure, &text, now));
            false
        }
    }
}

/// Publishes what the breaker held, one at a time, oldest first, stopping if it opens again.
pub(crate) async fn release_held_with<F, Fut>(app: &Shared, mut publish: F)
where
    F: FnMut(Shared, String) -> Fut,
    Fut: Future<Output = ()>,
{
    loop {
        if paused(app).is_some() {
            return;
        }
        let Some(id) = with(&app.cfg.config_dir, |b| b.held.pop_front()) else {
            return;
        };
        app.session_log(&id, "info", "autopilot: GitHub works again; publishing the held work".into())
            .await;
        publish(app.clone(), id).await;
    }
}

async fn tick(app: &Shared) {
    let closed = probe_tick_with(app, Utc::now(), || probe_github(app)).await;
    if closed {
        release_held_with(app, |app, id| crate::verify::after_turn(app, id, true)).await;
    }
}

fn start_tasks(app: &Shared) {
    let app = app.clone();
    tokio::spawn(async move {
        let mut every = tokio::time::interval(TICK);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            every.tick().await;
            tick(&app).await;
        }
    });
}

// ---------------------------------------------------------------------------------------------
// The status.
// ---------------------------------------------------------------------------------------------

/// The breaker's state for `GET /api/github/status` and `/api/status`'s `github_pause`.
pub(crate) async fn status_json(app: &App, now: DateTime<Utc>) -> Value {
    let identity = identity(app);
    let (open, held, refused, recent) = with(&app.cfg.config_dir, |b| {
        b.prune(now);
        let recent = if b.identity == identity { b.secondary.len() } else { 0 };
        (b.open_for(&identity).cloned(), b.held.len(), b.refused, recent)
    });
    let Some(open) = open else {
        return json!({"paused": false, "secondary_limits_recent": recent});
    };
    let queued = app
        .sessions
        .read()
        .await
        .iter()
        .filter(|s| s.status == crate::sessions::SessionStatus::Queued)
        .count();
    json!({
        "paused": true,
        "cause": open.cause,
        "message": open.cause.headline(),
        "next_step": open.cause.next_step(),
        "since": open.since,
        "next_probe_at": open.next_probe,
        "probes": open.probes,
        "detail": open.detail,
        "queued": queued,
        "held_publishes": held,
        "refused_calls": refused,
    })
}

async fn get_status(State(app): State<Shared>) -> ApiResult<Value> {
    Ok(Json(status_json(&app, Utc::now()).await))
}

fn routes() -> axum::Router<Shared> {
    axum::Router::new().route("/api/github/status", axum::routing::get(get_status))
}

/// This module's feature descriptor (`features.rs`). The status is the owner's: it names the cause
/// GitHub gave for the whole account.
pub(crate) const FEATURE: crate::features::Feature = crate::features::Feature {
    name: "github_breaker",
    routes,
    token_scope: None,
    activity: &[],
    kinds: &[],
    start_tasks: Some(start_tasks),
};

// ---------------------------------------------------------------------------------------------
// Test seams.
// ---------------------------------------------------------------------------------------------

#[cfg(test)]
thread_local! {
    static FAKE_GH: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// The program [`App::gh_unguarded`](crate::App::gh_unguarded) runs: `gh`, or a test's fake.
pub(crate) fn gh_program() -> PathBuf {
    #[cfg(test)]
    if let Some(fake) = FAKE_GH.with(|f| f.borrow().clone()) {
        return fake;
    }
    PathBuf::from("gh")
}

/// Points this test thread's `gh` at a fake until the guard drops. Thread-local, like
/// `authority::test_block_external_writes`, because tests run in parallel.
#[cfg(test)]
pub(crate) fn test_fake_gh(program: PathBuf) -> impl Drop {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            FAKE_GH.with(|f| *f.borrow_mut() = None);
        }
    }
    FAKE_GH.with(|f| *f.borrow_mut() = Some(program));
    Restore
}

#[cfg(test)]
mod tests;
