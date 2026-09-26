//! Stale-while-revalidate answers for slow read-only endpoints (`cached_answer`), kept in memory
//! and on disk (`<data_dir>/cache/answers`).

use crate::{App, Shared, cache_store};
use chrono::{DateTime, Utc};
use serde_json::json;
use std::{
    collections::HashMap,
    path::PathBuf,
    time::{Duration, Instant},
};

/// Cached answers for slow read-only endpoints, by key: when each was computed, and the value.
/// With a disk layer (`<data_dir>/cache/answers`, see [`cache_store`]) answers outlive a restart:
/// a key missing from memory is looked up on disk once, served at the age it has there, and
/// refreshed behind the answer when that age is past the key's freshness — so a restarted
/// mothership answers from what it last knew instead of "scanning".
#[derive(Default)]
pub struct AnswerCache {
    entries: std::sync::Mutex<HashMap<String, CachedAnswer>>,
    refreshing: std::sync::Mutex<std::collections::HashSet<String>>,
    /// Keys already looked up on disk this run, found or not, so a miss costs one read.
    probed: std::sync::Mutex<std::collections::HashSet<String>>,
    /// When a scope was last invalidated, in ms since the epoch: `repo:<owner/name>`, `org:<org>`
    /// or `key:<key>` (see [`answer_scopes`]). An answer computed before its scope's mark is stale
    /// whatever its age.
    marks: std::sync::Mutex<HashMap<String, u64>>,
    disk: Option<cache_store::DiskCache>,
}

#[derive(Clone)]
struct CachedAnswer {
    /// When it was stored, on this run's clock; `None` for an answer read from disk that is older
    /// than this process's clock can say (it is stale).
    at: Option<Instant>,
    /// When its computation started, on the wall clock (ms): what the cockpit shows as "updated",
    /// and what invalidation marks are compared with.
    fetched_at: u64,
    value: serde_json::Value,
}

/// Keys that must not outlive the process: markers of work done this run and live measurements.
fn answer_persists(key: &str) -> bool {
    !(key.starts_with("code-fetch:") || key.starts_with("repo-meta-pending:") || key == "storage")
}

/// The org-wide aggregates, recomputed (from per-repository parts) when one repository changes.
const ORG_AGGREGATES: &[&str] = &["deps-published:", "deps-dependencies:", "deps-supply:"];

/// The invalidation scopes a key belongs to: itself, the repository its first segment names
/// (`packages:owner/name`, `code-loc:owner/name:sha`, `deps-scan:owner/name@sha`), the org of an
/// org aggregate, and `repos` for the repository list.
pub fn answer_scopes(key: &str) -> Vec<String> {
    let mut out = vec![format!("key:{key}")];
    if let Some((_, rest)) = key.split_once(':') {
        let first = rest.split([':', '@']).next().unwrap_or(rest);
        if first.contains('/') {
            out.push(format!("repo:{first}"));
        } else if ORG_AGGREGATES.iter().any(|p| key.starts_with(p)) {
            out.push(format!("org:{first}"));
        }
    }
    out
}

impl AnswerCache {
    /// A cache that keeps its answers under `dir` as well as in memory.
    pub fn persistent(dir: impl Into<PathBuf>) -> Self {
        AnswerCache {
            disk: Some(cache_store::DiskCache::new(dir, cache_store::ANSWERS_MAX_BYTES)),
            ..Default::default()
        }
    }

    /// The cached answer for `key`: from memory, else (once per run) from disk.
    fn lookup(&self, key: &str) -> Option<CachedAnswer> {
        if let Some(hit) = self.entries.lock().unwrap_or_else(|p| p.into_inner()).get(key) {
            return Some(hit.clone());
        }
        let disk = self.disk.as_ref()?;
        if !answer_persists(key) || !self.probed.lock().unwrap_or_else(|p| p.into_inner()).insert(key.to_string()) {
            return None;
        }
        let entry = disk.load(key)?;
        let age = Duration::from_millis(cache_store::now_ms().saturating_sub(entry.fetched_at));
        let hit = CachedAnswer {
            at: Instant::now().checked_sub(age),
            fetched_at: entry.fetched_at,
            value: entry.value,
        };
        // A computation that finished meanwhile is newer than the disk: keep it.
        Some(
            self.entries
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .entry(key.to_string())
                .or_insert(hit)
                .clone(),
        )
    }

    fn is_fresh(&self, key: &str, hit: &CachedAnswer, fresh: Duration) -> bool {
        if !hit.at.is_some_and(|at| at.elapsed() < fresh) {
            return false;
        }
        let marks = self.marks.lock().unwrap_or_else(|p| p.into_inner());
        !answer_scopes(key)
            .iter()
            .any(|s| marks.get(s).is_some_and(|m| *m >= hit.fetched_at))
    }

    /// Stores an answer whose computation started at `started` (ms), in memory and on disk.
    fn put(&self, key: &str, started: u64, value: serde_json::Value) {
        if let Some(disk) = self.disk.as_ref().filter(|_| answer_persists(key)) {
            let mut entry = cache_store::DiskEntry::new(key, value.clone());
            entry.fetched_at = started;
            if let Err(e) = disk.store(&entry) {
                eprintln!("{key}: could not keep the answer on disk: {e:#}");
            }
        }
        self.entries.lock().unwrap_or_else(|p| p.into_inner()).insert(
            key.to_string(),
            CachedAnswer {
                at: Some(Instant::now()),
                fetched_at: started,
                value,
            },
        );
    }

    /// Claims the one background refresh a key may have running; false when one already is.
    fn begin_refresh(&self, key: &str) -> bool {
        self.refreshing
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(key.to_string())
    }

    fn end_refresh(&self, key: &str) {
        self.refreshing.lock().unwrap_or_else(|p| p.into_inner()).remove(key);
    }

    /// Whether a refresh of `key` is running.
    pub fn is_refreshing(&self, key: &str) -> bool {
        self.refreshing.lock().unwrap_or_else(|p| p.into_inner()).contains(key)
    }

    /// When the cached answer for `key` was computed (ms since the epoch), if one is cached.
    pub fn fetched_at(&self, key: &str) -> Option<u64> {
        self.lookup(key).map(|hit| hit.fetched_at)
    }

    /// A cached answer still within `fresh` (and not invalidated since), without computing anything.
    pub fn peek(&self, key: &str, fresh: Duration) -> Option<serde_json::Value> {
        self.lookup(key)
            .filter(|hit| self.is_fresh(key, hit, fresh))
            .map(|hit| hit.value)
    }

    /// Stores a value computed outside [`cached_answer`] (one batch answering many keys).
    pub fn insert(&self, key: &str, started: u64, value: serde_json::Value) {
        self.put(key, started, value);
    }

    /// Marks every answer in `scope` (see [`answer_scopes`]) stale: the next read serves it and
    /// refreshes it behind the answer.
    pub fn invalidate(&self, scope: impl Into<String>) {
        self.marks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(scope.into(), cache_store::now_ms());
    }

    /// Drops a key from memory, so the next read waits for a new computation.
    fn forget(&self, key: &str) {
        self.entries.lock().unwrap_or_else(|p| p.into_inner()).remove(key);
    }

    /// [`Self::forget`] for every key starting with `prefix` (in-memory only keys, such as
    /// `code-fetch:<org>/`, are the ones this is for).
    pub fn forget_prefix(&self, prefix: &str) {
        self.entries
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|key, _| !key.starts_with(prefix));
    }
}

/// Stale-while-revalidate for a slow read-only answer. Within `fresh` the cached value is returned
/// as is; past it the cached value is still returned at once and one background refresh is started
/// (never two for the same key); with nothing cached the caller waits for the first computation. A
/// failed computation keeps the last good value and reports the error only when there is none.
impl App {
    /// Whether a boolean mark kept beside the answer cache is set (see [`Self::answer_cache_mark`]).
    pub fn answer_cache_has(&self, key: &str) -> bool {
        self.answer_cache
            .entries
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(key)
            .is_some_and(|hit| hit.value.as_bool() == Some(true))
    }

    /// Sets or clears a boolean mark next to the cached answers — e.g. "this answer was made while
    /// GitHub was still computing, refresh it soon". Marks live in memory only.
    pub fn answer_cache_mark(&self, key: &str, on: bool) {
        self.answer_cache.entries.lock().unwrap_or_else(|p| p.into_inner()).insert(
            key.to_string(),
            CachedAnswer {
                at: Some(Instant::now()),
                fetched_at: cache_store::now_ms(),
                value: serde_json::Value::Bool(on),
            },
        );
    }

    /// A colony pushed to, opened or merged a pull request on `repo`: its answers, its org's
    /// aggregates and the repository list go stale, and the next read of the clone fetches again.
    /// Nothing else is touched — other repositories keep their answers, and the org aggregates
    /// recompute from per-repository scans that are reused until a branch moves.
    pub fn invalidate_repo(&self, repo: &str) {
        self.answer_cache.forget(&format!("code-fetch:{repo}"));
        self.answer_cache.invalidate(format!("repo:{repo}"));
        if let Some((owner, _)) = repo.split_once('/') {
            self.answer_cache.invalidate(format!("org:{owner}"));
        }
        self.answer_cache.invalidate("key:repos");
    }
}

/// `value` with when it was computed and whether a refresh is running, for the cockpit's
/// "updated 5m ago · refreshing": `cached_at` (RFC 3339) and `refreshing` on an object answer.
pub fn with_cache_info(app: &App, key: &str, mut value: serde_json::Value) -> serde_json::Value {
    if let Some(obj) = value.as_object_mut() {
        if let Some(at) = app
            .answer_cache
            .fetched_at(key)
            .and_then(|ms| DateTime::<Utc>::from_timestamp_millis(ms as i64))
        {
            obj.insert("cached_at".into(), json!(at));
        }
        obj.insert("refreshing".into(), json!(app.answer_cache.is_refreshing(key)));
    }
    value
}

/// Starts one background computation of `key` unless one is running; its result is stored.
fn spawn_refresh<F, Fut>(app: &Shared, key: String, compute: F, log_errors: bool)
where
    F: Fn(Shared) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = anyhow::Result<serde_json::Value>> + Send + 'static,
{
    if !app.answer_cache.begin_refresh(&key) {
        return;
    }
    let app = app.clone();
    tokio::spawn(async move {
        let started = cache_store::now_ms();
        match compute(app.clone()).await {
            Ok(value) => app.answer_cache.put(&key, started, value),
            Err(e) if log_errors => eprintln!("{key}: {e:#}"),
            Err(_) => {}
        }
        app.answer_cache.end_refresh(&key);
    });
}

/// [`cached_answer`] for an answer too slow to wait for on a request (it may clone repositories
/// and call registries): a cached value is returned as `Some` (and refreshed behind the answer once
/// stale); a miss starts the first computation in the background and returns `None` at once, so the
/// caller can answer "scanning" and be asked again. A failed computation leaves nothing cached, so
/// the next ask starts it again.
pub fn cached_answer_nowait<F, Fut>(
    app: &Shared,
    key: impl Into<String>,
    fresh: Duration,
    compute: F,
) -> Option<serde_json::Value>
where
    F: Fn(Shared) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = anyhow::Result<serde_json::Value>> + Send + 'static,
{
    let key: String = key.into();
    let hit = app.answer_cache.lookup(&key);
    let stale = hit.as_ref().is_none_or(|h| !app.answer_cache.is_fresh(&key, h, fresh));
    if stale {
        spawn_refresh(app, key, compute, true);
    }
    hit.map(|h| h.value)
}

pub async fn cached_answer<F, Fut>(
    app: &Shared,
    key: impl Into<String>,
    fresh: Duration,
    compute: F,
) -> anyhow::Result<serde_json::Value>
where
    F: Fn(Shared) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = anyhow::Result<serde_json::Value>> + Send + 'static,
{
    let key: String = key.into();
    if let Some(hit) = app.answer_cache.lookup(&key) {
        if !app.answer_cache.is_fresh(&key, &hit, fresh) {
            spawn_refresh(app, key, compute, false);
        }
        return Ok(hit.value);
    }
    let started = cache_store::now_ms();
    let value = compute(app.clone()).await?;
    app.answer_cache.put(&key, started, value.clone());
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util;
    use std::sync::Arc;

    #[test]
    fn keys_belong_to_their_repository_and_org_aggregates_to_their_org() {
        assert_eq!(
            answer_scopes("packages:acme/web"),
            vec!["key:packages:acme/web", "repo:acme/web"]
        );
        assert_eq!(
            answer_scopes("code-loc:acme/web:abc123"),
            vec!["key:code-loc:acme/web:abc123", "repo:acme/web"]
        );
        assert_eq!(
            answer_scopes("deps-scan:acme/web@abc123:v1"),
            vec!["key:deps-scan:acme/web@abc123:v1", "repo:acme/web"]
        );
        assert_eq!(answer_scopes("deps-supply:acme"), vec!["key:deps-supply:acme", "org:acme"]);
        assert_eq!(
            answer_scopes("registry:npm:@acme/sdk:true"),
            vec!["key:registry:npm:@acme/sdk:true"]
        );
        assert_eq!(answer_scopes("repos"), vec!["key:repos"]);
    }

    #[test]
    fn answers_survive_a_restart_at_the_age_they_had() {
        let dir = std::env::temp_dir().join(format!("colonizer-answers-{}", util::short_id()));
        let two_hours_ago = cache_store::now_ms() - 2 * 3600 * 1000;
        let before = AnswerCache::persistent(&dir);
        before.put("deps-supply:acme", two_hours_ago, json!({"risks": []}));
        before.put("code-fetch:acme/web", two_hours_ago, json!(true));
        let after = AnswerCache::persistent(&dir);
        let hit = after.lookup("deps-supply:acme").expect("read back from disk");
        assert_eq!(hit.value, json!({"risks": []}));
        assert_eq!(hit.fetched_at, two_hours_ago);
        assert!(
            !after.is_fresh("deps-supply:acme", &hit, Duration::from_secs(3600)),
            "an hour's freshness has passed"
        );
        assert!(after.is_fresh("deps-supply:acme", &hit, Duration::from_secs(3 * 3600)));
        assert!(after.lookup("code-fetch:acme/web").is_none(), "run-only markers are not kept");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_restarted_mothership_serves_the_kept_answer_and_refreshes_it_behind() {
        let root = std::env::temp_dir().join(format!("colonizer-answers-restart-{}", util::short_id()));
        let app = crate::tests::test_app(&root);
        let disk = cache_store::DiskCache::new(root.join("data/cache/answers"), cache_store::ANSWERS_MAX_BYTES);
        let mut kept = cache_store::DiskEntry::new("deps-supply:acme", json!({"v": "old"}));
        kept.fetched_at = cache_store::now_ms() - 2 * 3600 * 1000;
        disk.store(&kept).unwrap();
        let compute = |_app: Shared| async move { Ok(json!({"v": "new"})) };
        assert_eq!(
            cached_answer_nowait(&app, "deps-supply:acme", Duration::from_secs(3600), compute),
            Some(json!({"v": "old"})),
            "no scanning: the kept answer comes back at once"
        );
        for _ in 0..100 {
            if cached_answer_nowait(&app, "deps-supply:acme", Duration::from_secs(3600), compute) == Some(json!({"v": "new"})) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(
            cached_answer_nowait(&app, "deps-supply:acme", Duration::from_secs(3600), compute),
            Some(json!({"v": "new"}))
        );
        assert_eq!(
            disk.load("deps-supply:acme").unwrap().value,
            json!({"v": "new"}),
            "and the refresh is kept too"
        );
        let annotated = with_cache_info(&app, "deps-supply:acme", json!({"v": "new"}));
        assert!(annotated["cached_at"].is_string());
        assert_eq!(annotated["refreshing"], json!(false));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_push_to_one_repository_stales_only_its_answers_and_its_orgs_aggregates() {
        let root = std::env::temp_dir().join(format!("colonizer-invalidate-{}", util::short_id()));
        let app = crate::tests::test_app(&root);
        let started = cache_store::now_ms() - 10;
        let keys = [
            "packages:acme/web",
            "deps-scan:acme/web@abc:v1",
            "deps-supply:acme",
            "repos",
            "packages:acme/api",
            "deps-supply:other",
            "registry:npm:left-pad:true",
        ];
        for key in keys {
            app.answer_cache.put(key, started, json!(key));
        }
        app.answer_cache_mark("code-fetch:acme/web", true);
        app.invalidate_repo("acme/web");
        let fresh = |key: &str| {
            let hit = app.answer_cache.lookup(key).unwrap();
            app.answer_cache.is_fresh(key, &hit, Duration::from_secs(3600))
        };
        for stale in &keys[..4] {
            assert!(!fresh(stale), "{stale} should be stale");
        }
        for kept in &keys[4..] {
            assert!(fresh(kept), "{kept} should be untouched");
        }
        assert!(
            !app.answer_cache_has("code-fetch:acme/web"),
            "the next read fetches the clone"
        );
        // An answer computed after the push is fresh again.
        tokio::time::sleep(Duration::from_millis(2)).await;
        app.answer_cache.put("packages:acme/web", cache_store::now_ms(), json!("new"));
        assert!(fresh("packages:acme/web"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_nowait_answer_starts_in_the_background_and_is_served_once_ready() {
        let root = std::env::temp_dir().join(format!("colonizer-nowait-{}", util::short_id()));
        let app = crate::tests::test_app(&root);
        let compute = |_app: Shared| async move { Ok(serde_json::json!("scanned")) };
        assert_eq!(
            cached_answer_nowait(&app, "scan", Duration::from_secs(60), compute),
            None,
            "a miss answers at once"
        );
        for _ in 0..100 {
            if cached_answer_nowait(&app, "scan", Duration::from_secs(60), compute).is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(
            cached_answer_nowait(&app, "scan", Duration::from_secs(60), compute),
            Some(serde_json::json!("scanned"))
        );
        let failing = |_app: Shared| async move { Err::<serde_json::Value, _>(anyhow::anyhow!("offline")) };
        assert_eq!(cached_answer_nowait(&app, "broken", Duration::from_secs(60), failing), None);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            cached_answer_nowait(&app, "broken", Duration::from_secs(60), failing),
            None,
            "a failure caches nothing"
        );
        let _ = std::fs::remove_dir_all(root);
    }
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn a_fresh_answer_is_served_from_the_cache_and_a_stale_one_refreshes_behind_it() {
        let root = std::env::temp_dir().join(format!("colonizer-answer-cache-{}", util::short_id()));
        let app = crate::tests::test_app(&root);
        let calls = Arc::new(AtomicUsize::new(0));
        let compute = {
            let calls = calls.clone();
            move |_app: Shared| {
                let calls = calls.clone();
                async move { Ok(serde_json::json!(calls.fetch_add(1, Ordering::SeqCst) + 1)) }
            }
        };
        // Nothing cached: the caller waits for the first answer.
        assert_eq!(
            cached_answer(&app, "k", Duration::from_secs(60), compute.clone())
                .await
                .unwrap(),
            1
        );
        // Fresh: no second computation.
        assert_eq!(
            cached_answer(&app, "k", Duration::from_secs(60), compute.clone())
                .await
                .unwrap(),
            1
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // Stale: the old value comes back at once and one refresh runs behind it.
        assert_eq!(cached_answer(&app, "k", Duration::ZERO, compute.clone()).await.unwrap(), 1);
        for _ in 0..50 {
            if calls.load(Ordering::SeqCst) == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(cached_answer(&app, "k", Duration::from_secs(60), compute).await.unwrap(), 2);
        let _ = std::fs::remove_dir_all(root);
    }
}
