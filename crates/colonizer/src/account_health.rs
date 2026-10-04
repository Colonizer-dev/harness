//! Claude-account health (issue #984): when a subscription/OAuth account — the credential injected
//! at the sandbox's TLS edge, not a gateway provider — stops working, every colony routed to it
//! fails its turn. A 401/403 means the sign-in expired or was revoked and a person must sign in
//! again; a usage limit (429) is left to the existing retry and quota paths, which already park
//! account-wide with a reset time. The account is marked here, its colonies wait in a distinct
//! parked state instead of burning autopilot retries, and the owner is told once per state change.
//! Nothing secret is stored: the account id, the failure's class and status, when it started, and a
//! stamp of the credential file's mtime, so a re-sign-in is noticed without reading the secret.

use crate::App;
use crate::Shared;
use crate::sessions::Session;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use tokio::sync::Mutex;

/// The park reason — and the attention reason on the same park record — a colony waits under while
/// its Claude account is unusable (issue #984). Not a [`SessionStatus`] variant: a waiting colony is
/// `Parked` like any other, and this reason is what [`crate::queue::resume_waiting_for_account`]
/// keys on to bring it back and what notify suppresses per colony.
pub const WAITING_FOR_ACCOUNT_REASON: &str = "waiting_for_account";

/// How an account is broken. Snake-case names are the `/api/status` and webhook spelling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// A 401/403: the sign-in expired or was revoked; a person has to sign in again.
    NeedsSignIn,
}

impl State {
    pub fn as_str(self) -> &'static str {
        "needs_sign_in"
    }

    /// The phrase the mothership log and the notification use.
    fn phrase(self) -> &'static str {
        "needs sign-in again"
    }
}

/// One account's trouble. Never any credential or request/response body: the id (the map key), the
/// failure class, the upstream status, when it started, and a stamp of the credential file.
#[derive(Clone, Debug)]
pub struct Trouble {
    pub state: State,
    pub class: String,
    pub status: u16,
    pub since: DateTime<Utc>,
    /// The credential file's mtime when the account was marked, so a re-sign-in that rewrites it
    /// clears the mark ([`sweep_credentials`]). `None` when no file could be stamped (the secret
    /// lives in the keychain, say) — then only [`record_ok`] clears it.
    pub cred_stamp: Option<SystemTime>,
}

/// The account-health map on [`App`]: one entry per account currently in trouble, keyed by the
/// account id the owner sees on the Accounts page.
#[derive(Default)]
pub struct AccountHealth(Mutex<BTreeMap<String, Trouble>>);

/// The account a colony is routed to: its recorded choice, else the install default the resolver
/// names when it chose none. Mirrors `events::model_router_line`'s `account.unwrap_or("default")`.
pub fn account_of(s: &Session) -> String {
    s.claude_account
        .clone()
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| "default".into())
}

/// Whether this account is in trouble, and what — the read side of the mark.
pub async fn troubled(app: &App, account: &str) -> Option<Trouble> {
    app.account_health.0.lock().await.get(account).cloned()
}

/// Every account in trouble, sorted by id — what `/api/status` and the notify loop read.
pub async fn snapshot(app: &App) -> Vec<(String, Trouble)> {
    app.account_health
        .0
        .lock()
        .await
        .iter()
        .map(|(a, t)| (a.clone(), t.clone()))
        .collect()
}

/// How many colonies are parked waiting on this account (issue #984), from a session list the caller
/// already holds. The park shape itself is [`crate::queue::waiting_for_account`]'s.
pub fn waiting_on(sessions: &[Session], account: &str) -> usize {
    sessions
        .iter()
        .filter(|s| crate::queue::waiting_for_account(s) && account_of(s) == account)
        .count()
}

/// The one line a notification carries for an account in trouble.
pub fn trouble_text(account: &str, waiting: usize) -> String {
    let line = format!("Claude account `{account}` needs you to sign in again.");
    match waiting {
        0 => line,
        1 => format!("{line} 1 colony is waiting on it."),
        n => format!("{line} {n} colonies are waiting on it."),
    }
}

/// Records an upstream sign-in failure for `account` (issue #984): a `401`/`403` marks the account
/// so its colonies wait for a re-sign-in. Returns whether the state changed — the once-per-state-change
/// signal the notify loop reads. A repeat refreshes only the status, so ten colonies failing one
/// account mark it once. A `429` is deliberately not here: a usage limit keeps the existing retry
/// and quota paths, which park account-wide with a reset time.
pub async fn record_failure(app: &App, account: &str, status: u16) -> bool {
    let changed = {
        let mut health = app.account_health.0.lock().await;
        match health.get_mut(account) {
            Some(existing) => {
                existing.status = status;
                false
            }
            None => {
                health.insert(
                    account.to_string(),
                    Trouble {
                        state: State::NeedsSignIn,
                        class: "auth".into(),
                        status,
                        since: Utc::now(),
                        cred_stamp: cred_stamp(&app.cfg.config_dir, account),
                    },
                );
                true
            }
        }
    };
    if changed {
        eprintln!(
            "claude account `{account}` {} ({status} auth); colonies routed to it will wait",
            State::NeedsSignIn.phrase()
        );
    }
    changed
}

/// Clears the account's mark — it works again — returning whether there was one, and says so once in
/// the mothership log. [`sweep_credentials`] calls it on a re-sign-in.
pub async fn record_ok(app: &App, account: &str) -> bool {
    let cleared = app.account_health.0.lock().await.remove(account).is_some();
    if cleared {
        eprintln!("claude account `{account}` works again; its colonies are resuming");
    }
    cleared
}

/// The newest mtime of the account's credential file and its encrypted sibling, if either exists.
/// The secret's contents are never read — only the timestamp that says it was rewritten.
fn cred_stamp(config_dir: &Path, account: &str) -> Option<SystemTime> {
    let path = crate::claude_accounts::account_file(config_dir, account);
    let mut encrypted: PathBuf = path.clone().into_os_string().into();
    encrypted.push(".enc");
    [path, encrypted]
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok()?.modified().ok())
        .max()
}

/// Clears any marked account whose credential file was rewritten since it was marked: a re-sign-in
/// writes a new credential, so the account works again. Called from the queue tick before
/// [`crate::queue::resume_waiting_for_account`] reads the marks.
pub async fn sweep_credentials(app: &App) {
    let stamps: Vec<(String, Option<SystemTime>)> = app
        .account_health
        .0
        .lock()
        .await
        .iter()
        .map(|(a, t)| (a.clone(), t.cred_stamp))
        .collect();
    for (account, stamp) in stamps {
        if let Some(stamp) = stamp
            && cred_stamp(&app.cfg.config_dir, &account).is_some_and(|now| now != stamp)
        {
            record_ok(app, &account).await;
        }
    }
}

/// The sign-in failure a model-router `log` event carries, if any (issue #984). The router names
/// `provider=anthropic` on the account's own path — a gateway-routed model names its provider
/// instead, and is not this account's traffic — with the failure's `class=` and `status=`. Only a
/// sign-in failure (class `auth`, 401/403) counts. A rate limit (429, or a 403 the router classes
/// `rate_limit`) is left to the existing retry and quota paths, so it never parks here.
pub fn router_failure(event: &Value) -> Option<(u16, State)> {
    if event["type"] != "log" || event["source"] != "model_router" {
        return None;
    }
    let message = event["message"].as_str()?;
    let field = |key: &str| message.split_whitespace().find_map(|t| t.strip_prefix(key));
    if field("provider=")? != "anthropic" {
        return None;
    }
    let class = field("class=")?;
    let status: u16 = field("status=")?.parse().ok()?;
    match (class, status) {
        ("auth", 401 | 403) => Some((status, State::NeedsSignIn)),
        _ => None,
    }
}

/// The sign-in failure a turn's free-text result names, if any (issue #984). Claude Code reports an
/// expired or refused credential as `API Error: 401 …`, and the words around it name authentication.
/// Deliberately narrow — a code in an error position plus one auth word, never a bare number — so an
/// unrelated mention cannot park a colony.
pub fn auth_in(text: &str) -> Option<u16> {
    let lower = text.to_lowercase();
    let words = [
        "authentication",
        "unauthorized",
        "forbidden",
        "credential",
        "oauth",
        "api key",
        "api-key",
    ];
    for code in [401u16, 403] {
        let positioned = lower.contains(&format!("api error: {code}"))
            || lower.contains(&format!("http {code}"))
            || lower.contains(&format!("status {code}"));
        if positioned && words.iter().any(|w| lower.contains(w)) {
            return Some(code);
        }
    }
    None
}

/// Whether this colony's turn result may mark its account on its own (issue #984). Only a colony that
/// routes through no gateway provider: one that does has the router's own `provider=` log for every
/// upstream failure, and there a 401/403 in the result text is a *provider's*, not the account's — the
/// router already marks the account when the failure is the account's own path.
pub fn text_may_mark(s: &Session) -> bool {
    s.allowed_providers.as_ref().is_none_or(|providers| providers.is_empty())
}

/// The account a colony waits on, if the turn that just ended shows it is unusable (issue #984): the
/// colony's own account is already marked, or the turn's result names a 401/403 sign-in failure and
/// the colony reaches Anthropic directly, so a missed router log still marks the account.
pub async fn unusable_account(app: &App, s: &Session, result: Option<&str>) -> Option<String> {
    let account = account_of(s);
    if text_may_mark(s)
        && let Some(status) = result.and_then(auth_in)
    {
        record_failure(app, &account, status).await;
    }
    troubled(app, &account).await.map(|_| account)
}

/// Parks a colony whose account is unusable (issue #984): the slot is released, the worktree kept,
/// the park and attention reason is [`WAITING_FOR_ACCOUNT_REASON`] so the queue resumes it when the
/// account works. No `provider_retries` are spent — this is not the transient-error backoff.
pub async fn park_waiting(app: &Shared, s: &Session, account: &str) {
    let error = format!("Claude account `{account}` needs sign-in again");
    let warn = format!(
        "waiting for Claude account `{account}`; parked, the worktree kept — the colony resumes when the account works again"
    );
    crate::lifecycle::park_colony(app, s, WAITING_FOR_ACCOUNT_REASON, None, error, warn).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::SessionStatus;

    #[test]
    fn a_router_line_names_the_account_failure_and_its_provider() {
        let event = serde_json::json!({
            "type": "log",
            "source": "model_router",
            "level": "error",
            "message": "upstream failure: provider=anthropic class=auth status=401 elapsed=0.3s",
        });
        assert_eq!(router_failure(&event), Some((401, State::NeedsSignIn)));
        // A rate limit is the existing retry and quota paths' — it must not park as a sign-in wait.
        let limited = serde_json::json!({
            "type": "log",
            "source": "model_router",
            "message": "upstream failure: provider=anthropic class=rate_limit status=429 elapsed=0.3s",
        });
        assert_eq!(router_failure(&limited), None);
        // A gateway-routed provider is not the account's traffic, and neither is a plain 5xx.
        let routed = serde_json::json!({
            "type": "log",
            "source": "model_router",
            "message": "upstream failure: provider=bailian class=auth status=401 elapsed=0.3s",
        });
        assert_eq!(router_failure(&routed), None);
        let overload = serde_json::json!({
            "type": "log",
            "source": "model_router",
            "message": "upstream failure: provider=anthropic class=upstream_5xx status=502 elapsed=0.3s",
        });
        assert_eq!(router_failure(&overload), None);
    }

    #[test]
    fn the_result_text_matcher_is_narrow() {
        assert_eq!(auth_in("API Error: 401 authentication_error"), Some(401));
        assert_eq!(auth_in("API Error: 403 forbidden"), Some(403));
        assert_eq!(auth_in("API Error: 502 model router: Anthropic is unreachable"), None);
        // A code without an auth word, or an auth word without a positioned code, is not a match.
        assert_eq!(auth_in("processed 401 items"), None);
        assert_eq!(auth_in("authentication is a nice word"), None);
    }

    /// A gateway-routed colony's result text is a provider's failure, not the Claude account's, and the
    /// router log has already spoken for it — so only a colony that routes nowhere marks from text.
    #[test]
    fn only_a_colony_that_routes_nowhere_marks_from_its_result_text() {
        let mut s = crate::sessions::tests::colony("acme", SessionStatus::Running);
        s.allowed_providers = Some(vec!["bailian".into()]);
        assert!(!text_may_mark(&s), "gateway-routed: the router log is the mark's source");
        s.allowed_providers = Some(vec![]);
        assert!(text_may_mark(&s));
        s.allowed_providers = None;
        assert!(text_may_mark(&s));
    }
}
