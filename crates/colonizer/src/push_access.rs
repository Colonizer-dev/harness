//! Whether this mothership's GitHub identity may push to the repository a colony is being launched
//! on, and what to do when it may not (issue #1134).
//!
//! A mothership signed in to an account with no write access to a repository could launch a colony
//! happily: the agent cloned, edited, committed, reviewed and merged its own work, hours and a
//! microVM later `git push` answered 403 and the colony sat in the terminal `Failed` state with
//! every commit stranded on the machine that made them. Nothing in the launch had been wrong but
//! the very first thing it needed, and the operator had no way to learn that before paying for it.
//!
//! So the answer is asked once, up front, in [`check`], and the launch is refused while nothing
//! exists yet — no worktree, no microVM, no claim. `sessions::launch::create` is the one
//! in-process entry point for `POST /api/sessions`, the queue, the loops, the decisions-inbox redo,
//! burn-down and the merge loop, so one check there covers every way a colony starts.
//!
//! Three answers, not two. A GitHub call that fails — the breaker open ([`github_breaker`]),
//! network down, rate limited, no `gh` installed — is [`Verdict::Unknown`], deliberately *not* a
//! refusal: a launch must not be blocked by a blip, and the colony's own push is still the real
//! test. That is also why only the two decided answers are cached: a blip must be retried on the
//! next launch, not remembered for ten minutes.
//!
//! The same question is asked a second time, in the other direction, when a publish fails:
//! [`refuses_push`] recognises a push refused for want of write access so the colony is *parked* —
//! resumable, publishable again — instead of failed, which is terminal.

use crate::App;
use chrono::{DateTime, Utc};
use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex as StdMutex},
};

/// How long a decided push-access answer is remembered: long enough that a queue of launches on the
/// same repository does not ask GitHub once per colony (the check is on the launch path, before a
/// microVM boots), short enough that granting the account write access takes effect in minutes
/// rather than after a harness restart. Ten minutes is about the length of one colony's worth of
/// queued launches.
pub(crate) const PUSH_ACCESS_TTL_SECS: i64 = 10 * 60;

/// What GitHub says about this identity's right to write to a repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The identity can push to this repo.
    CanPush,
    /// It cannot: read-only, or no access at all. `login` names who this mothership is.
    CannotPush { login: String },
    /// GitHub could not be asked — breaker open, network down, rate limited, no `gh`.
    /// Deliberately *not* a refusal: a launch must not be blocked by a blip.
    Unknown,
}

/// The sentence a refused launch carries (issue #1134). Pure, so it is testable apart from the
/// network — the answer GitHub gave and the answer the operator reads are one string.
pub(crate) fn refusal(login: &str, repo: &str) -> String {
    format!(
        "this mothership signs in to GitHub as {login}, which can't push to {repo}; \
         launch it on a mothership that can, or give {login} write access"
    )
}

/// Whether a failed publish is GitHub refusing this identity's right to write to the repo
/// (issue #1134) — a 403 on the push, git's own "Permission to OWNER/REPO.git denied to LOGIN",
/// or a refused/expired credential.
pub(crate) fn refuses_push(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    // A credential that no longer authenticates is the same dead end as a missing permission, and
    // the account breaker already calls it that: one classification, not a second spelling list.
    let dead_credential = matches!(
        crate::github_breaker::classify(&lower),
        crate::github_breaker::Failure::TokenRevoked
    );
    // git's own spelling: `remote: Permission to OWNER/REPO.git denied to LOGIN`.
    let denied_to = lower.contains("permission to") && lower.contains(" denied to ");
    // The transport's: `The requested URL returned error: 403`, or a body carrying `"status":"403"`.
    // Read as the literal it is, on the lowercased text `classify` sees.
    let forbidden = lower.contains("403");
    // Deliberately explicit: a non-fast-forward rejection, a GH013 secret rejection and a missing
    // branch must NOT match here. All three are `push_guard::PublishHold`s, handled and resumed a
    // few lines above this classifier is consulted from, and calling them a permission problem
    // would park a colony that simply has to rebase or drop a secret.
    dead_credential || denied_to || forbidden
}

// ---------------------------------------------------------------------------------------------
// The cache.
// ---------------------------------------------------------------------------------------------

/// One decided answer and when it was given. [`Verdict::Unknown`] is never stored, so only the
/// two decided variants appear here.
type Answer = (DateTime<Utc>, Verdict);
/// A cached canonical name and when it was fetched, for the same TTL as push-access answers.
type CanonicalEntry = (DateTime<Utc>, String);

/// A short-lived memory of push-access answers, per `(GitHub identity, repository)`.
pub(crate) struct AccessCache {
    ttl: chrono::Duration,
    seen: StdMutex<HashMap<(String, String), Answer>>,
    /// Cache of GitHub `full_name` by lowercased repo, for canonicalizing repository names (issue #1284).
    /// Shared across identities, same TTL as push-access answers.
    canonical: StdMutex<HashMap<String, CanonicalEntry>>,
}

impl AccessCache {
    /// `new` takes the TTL so a test can build one that expires in seconds.
    pub(crate) fn new(ttl_secs: i64) -> Self {
        Self {
            ttl: chrono::Duration::seconds(ttl_secs),
            seen: StdMutex::new(HashMap::new()),
            canonical: StdMutex::new(HashMap::new()),
        }
    }

    /// A fresh answer for `(identity, repo)`, if there is one. Keys are lowercased, so `Acme/App`
    /// and `acme/app` are one question.
    fn get(&self, identity: &str, repo: &str, now: DateTime<Utc>) -> Option<Verdict> {
        let seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        let (at, verdict) = seen.get(&(identity.to_ascii_lowercase(), repo.to_ascii_lowercase()))?;
        (now - *at < self.ttl).then(|| verdict.clone())
    }

    /// Remembers a decided answer, sweeping what has expired on the way in so the map stays as big
    /// as the identities and repositories active in the last ten minutes.
    ///
    /// `Unknown` is refused here rather than at the call site: a blip must be asked again on the
    /// next launch, not remembered for the TTL, and one place that says so cannot be forgotten by
    /// a later caller.
    fn put(&self, identity: &str, repo: &str, now: DateTime<Utc>, verdict: &Verdict) {
        if *verdict == Verdict::Unknown {
            return;
        }
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        seen.retain(|_, (at, _)| now - *at < self.ttl);
        seen.insert(
            (identity.to_ascii_lowercase(), repo.to_ascii_lowercase()),
            (now, verdict.clone()),
        );
    }

    /// A cached canonical name for `repo`, if there is one and it has not expired.
    fn get_canonical(&self, repo: &str, now: DateTime<Utc>) -> Option<String> {
        let canonical_map = self.canonical.lock().unwrap_or_else(|e| e.into_inner());
        let (at, name) = canonical_map.get(&repo.to_ascii_lowercase())?;
        (now - *at < self.ttl).then(|| name.clone())
    }

    /// Remember a canonical name for `repo`, sweeping what has expired.
    fn put_canonical(&self, repo: &str, now: DateTime<Utc>, canonical: String) {
        let mut canonical_map = self.canonical.lock().unwrap_or_else(|e| e.into_inner());
        canonical_map.retain(|_, (at, _)| now - *at < self.ttl);
        canonical_map.insert(repo.to_ascii_lowercase(), (now, canonical));
    }
}

/// Every mothership's answers, in this process. Keyed on the identity fingerprint, not on the
/// credential, so reconnecting as another account misses the cache instead of being told the
/// previous account's answer.
static CACHE: LazyLock<AccessCache> = LazyLock::new(|| AccessCache::new(PUSH_ACCESS_TTL_SECS));

// ---------------------------------------------------------------------------------------------
// The check.
// ---------------------------------------------------------------------------------------------

/// Whether the mothership's GitHub identity may push to `repo`, asking GitHub unless a fresh
/// answer is cached and never blocking a launch on a GitHub that could not be asked.
pub(crate) async fn check(app: &App, repo: &str) -> Verdict {
    // The breaker already holds the queue and the publish path for a refused account; asking again
    // would only spend a call on a question whose answer is already "not now".
    if crate::github_breaker::paused(app).is_some() {
        return Verdict::Unknown;
    }
    let identity = crate::github_breaker::identity(app);
    let now = Utc::now();
    if let Some(hit) = CACHE.get(&identity, repo, now) {
        return hit;
    }
    // Who this mothership is (cached 5 minutes by `github::viewer`) and what that account may do to
    // the repository. Either call failing leaves the question unasked, not answered no.
    let path = format!("repos/{repo}");
    let (viewer, repo_json) = tokio::join!(crate::github::viewer(app), crate::github::gh_get_json(app, &path));
    let (Ok(viewer), Ok(repo_json)) = (viewer, repo_json) else {
        return Verdict::Unknown;
    };
    let (Some(login), Some(push)) = (viewer["login"].as_str(), repo_json["permissions"]["push"].as_bool()) else {
        return Verdict::Unknown;
    };
    let verdict = if push {
        Verdict::CanPush
    } else {
        // GitHub answers `push: false` both for a read-only collaborator and for an account with no
        // access to the repository at all, and it does not distinguish them here. The message says
        // "can't push", which is true either way, and both are fixed by the same grant.
        Verdict::CannotPush {
            login: login.to_string(),
        }
    };
    CACHE.put(&identity, repo, now, &verdict);
    // Issue #1284: cache the canonical repo name from GitHub's response, so `canonical_repo_name`
    // normally gets a cache hit instead of making a second request.
    if let Some(full_name) = repo_json.get("full_name").and_then(|v| v.as_str())
        && crate::util::valid_repo(full_name)
        && full_name.eq_ignore_ascii_case(repo)
    {
        CACHE.put_canonical(repo, now, full_name.to_string());
    }
    verdict
}

/// The canonical `full_name` for a repository (issue #1284), or `None` if the breaker is open,
/// no token is available, or GitHub could not be reached. Caches the result for the push-access TTL.
/// Accepts `full_name` only if it passes `valid_repo` and matches the input case-insensitively
/// (a redirect keeps the input, e.g. a transfer).
pub(crate) async fn canonical_repo_name(app: &App, repo: &str) -> Option<String> {
    let now = Utc::now();
    if let Some(cached) = CACHE.get_canonical(repo, now) {
        return Some(cached);
    }
    if crate::github_breaker::paused(app).is_some() {
        return None;
    }
    let path = format!("repos/{repo}");
    let repo_json = crate::github::gh_get_json(app, &path).await.ok()?;
    let full_name = repo_json.get("full_name")?.as_str()?;
    if !crate::util::valid_repo(full_name) || !full_name.eq_ignore_ascii_case(repo) {
        return None;
    }
    let canonical = full_name.to_string();
    CACHE.put_canonical(repo, now, canonical.clone());
    Some(canonical)
}

#[cfg(test)]
mod tests;
