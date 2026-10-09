//! Model discovery (issue #1167): what each provider's `GET /v1/models` publishes, kept for the
//! cockpit's provider form. The lists live in a sidecar to the operator's `providers.json` —
//! `<config_dir>/provider-models.json` — on purpose: a discovery is an observation, so it never
//! rewrites the configuration and never races a save of it.

use super::*;

/// How old a discovered list may get before the background sweep re-probes its provider — and how
/// long a failed attempt holds its place in the sweep's retry map, so a provider that will not
/// answer is asked at most once a day either way.
const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);
/// How often the sweep wakes to look for stale lists.
const SWEEP_EVERY: Duration = Duration::from_secs(60 * 60);
/// A hard cap on models kept per provider, so a huge catalogue cannot bloat the sidecar.
pub(crate) const MAX_MODELS: usize = 500;

/// One provider's last discovered list: the endpoint it was read from, the models it published
/// (deduped, sorted, [`MAX_MODELS`] at most), what the discovery before it did not have, and when.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Discovered {
    pub base_url: String,
    pub models: Vec<String>,
    pub new_models: Vec<String>,
    pub discovered_at: DateTime<Utc>,
}

/// Every provider's discovered list, persisted to `<config_dir>/provider-models.json`.
pub(crate) struct Discover {
    file: PathBuf,
    inner: Mutex<BTreeMap<String, Discovered>>,
}

impl Discover {
    /// Reads the sidecar; a missing or corrupt file starts discovery over, and the next probe
    /// refills it.
    pub(crate) fn load(config_dir: &std::path::Path) -> Self {
        let file = config_dir.join("provider-models.json");
        let saved = std::fs::read(&file)
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default();
        Self {
            file,
            inner: Mutex::new(saved),
        }
    }

    /// Records a real answer: a parsed `data` list, or an endpoint that answered 404 with no list
    /// (recorded as empty). `new_models` is what the previous entry for the same base URL did not
    /// have — empty on a first discovery or after a base URL change, and recomputed on every
    /// record, so a model the endpoint drops stops counting as new. Anything else — an unreachable
    /// probe, a refused, throttled or failed status, a 200 with no list in the body — must not be
    /// recorded: a blip must not wipe the list.
    pub(crate) fn record(&self, provider: &Provider, models: &[String]) {
        let mut list = models.to_vec();
        list.sort();
        list.dedup();
        list.truncate(MAX_MODELS);
        let mut map = self.inner.lock().unwrap();
        let new_models = match map.get(&provider.id).filter(|d| d.base_url == provider.base_url) {
            Some(previous) => list
                .iter()
                .filter(|m| !previous.models.iter().any(|known| known == *m))
                .cloned()
                .collect(),
            None => Vec::new(),
        };
        map.insert(
            provider.id.clone(),
            Discovered {
                base_url: provider.base_url.clone(),
                models: list,
                new_models,
                discovered_at: Utc::now(),
            },
        );
        self.flush(&map);
    }

    /// The provider's discovered list — but only while it names the provider's current base URL, so
    /// a provider repointed elsewhere is not shown its old endpoint's models.
    pub(crate) fn entry(&self, id: &str, base_url: &str) -> Option<Discovered> {
        self.inner.lock().unwrap().get(id).filter(|d| d.base_url == base_url).cloned()
    }

    /// Forgets a deleted provider's list.
    pub(crate) fn forget(&self, id: &str) {
        let mut map = self.inner.lock().unwrap();
        if map.remove(id).is_some() {
            self.flush(&map);
        }
    }

    /// Writes the whole map atomically ([`write_json_atomic`]). A failed write loses only the
    /// discovery until the next probe refills it.
    fn flush(&self, map: &BTreeMap<String, Discovered>) {
        if let Some(dir) = self.file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        write_json_atomic(&self.file, map);
    }
}

/// The daily sweep: wakes every [`SWEEP_EVERY`], and probes each provider whose discovered list is
/// missing or older than [`STALE_AFTER`]. Failed attempts are stamped in a map kept in memory, so
/// an unreachable provider costs one probe a day rather than one a sweep; the map dies with the
/// process on purpose — a restart's first sweep simply tries again.
pub(crate) async fn refresh_loop(app: crate::Shared) {
    let mut attempted: BTreeMap<String, Instant> = BTreeMap::new();
    loop {
        tokio::time::sleep(SWEEP_EVERY).await;
        let now = Utc::now();
        for provider in app.providers() {
            let fresh = app
                .provider_models
                .entry(&provider.id, &provider.base_url)
                .is_some_and(|d| (now - d.discovered_at).num_seconds() < STALE_AFTER.as_secs() as i64);
            let tried = attempted.get(&provider.id).is_some_and(|at| at.elapsed() < STALE_AFTER);
            if fresh || tried {
                continue;
            }
            attempted.insert(provider.id.clone(), Instant::now());
            probe(&app, &provider).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (Discover, PathBuf) {
        let dir = std::env::temp_dir().join(format!("colonizer-provider-models-{}", crate::util::short_id()));
        (Discover::load(&dir), dir)
    }

    fn provider(base_url: &str) -> Provider {
        Provider {
            base_url: base_url.into(),
            ..crate::gateway::tests::provider("x", None)
        }
    }

    #[test]
    fn record_dedupes_sorts_and_flags_what_is_new() {
        let (d, dir) = store();
        let p = provider("http://x");
        d.record(&p, &["m2".to_string(), "m1".to_string(), "m2".to_string()]);
        let found = d.entry("x", "http://x").unwrap();
        assert_eq!(found.models, vec!["m1", "m2"]);
        assert!(found.new_models.is_empty(), "a first discovery flags nothing");
        d.record(&p, &["m3".to_string(), "m1".to_string()]);
        let found = d.entry("x", "http://x").unwrap();
        assert_eq!(found.models, vec!["m1", "m3"]);
        assert_eq!(found.new_models, vec!["m3"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_repointed_provider_hides_the_old_list_and_starts_over() {
        let (d, dir) = store();
        let p = provider("http://x");
        d.record(&p, &["m1".to_string()]);
        assert!(d.entry("x", "http://y").is_none(), "another endpoint's list is not served");
        d.record(
            &Provider {
                base_url: "http://y".into(),
                ..p
            },
            &["m9".to_string()],
        );
        assert!(d.entry("x", "http://x").is_none(), "the old endpoint's list is hidden");
        let found = d.entry("x", "http://y").unwrap();
        assert_eq!(found.models, vec!["m9"]);
        assert!(found.new_models.is_empty(), "a moved provider starts over");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_sidecar_persists_and_forget_writes_through() {
        let (d, dir) = store();
        d.record(&provider("http://x"), &["m1".to_string()]);
        assert_eq!(
            Discover::load(&dir).entry("x", "http://x").unwrap().models,
            vec!["m1"],
            "a fresh Discover reads the sidecar back"
        );
        d.forget("x");
        assert!(
            Discover::load(&dir).entry("x", "http://x").is_none(),
            "the forget is persisted"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_huge_catalogue_is_capped() {
        let (d, dir) = store();
        let models: Vec<String> = (0..MAX_MODELS + 100).map(|i| format!("m{i:04}")).collect();
        d.record(&provider("http://x"), &models);
        assert_eq!(d.entry("x", "http://x").unwrap().models.len(), MAX_MODELS);
        let _ = std::fs::remove_dir_all(dir);
    }
}
