//! Gate snapshots recorded at dispatcher decision points, published atomically at tick end.

use serde_json::json;
use std::collections::BTreeMap;
use std::sync::{OnceLock, RwLock};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GateKind {
    RepoCap,
    OrgCap,
    Chain,
    Held,
    Superseded,
    Host,
}

impl GateKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RepoCap => "repo_cap",
            Self::OrgCap => "org_cap",
            Self::Chain => "chain",
            Self::Held => "held",
            Self::Superseded => "superseded",
            Self::Host => "host",
        }
    }
}

#[derive(Clone, Debug)]
pub struct GateSnapshot {
    pub kind: GateKind,
    pub text: String,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct GateSummary {
    pub admissible: usize,
    pub blocked_by: BTreeMap<String, serde_json::Value>,
}

#[derive(Default)]
pub struct GateStore {
    scan: BTreeMap<String, GateSnapshot>,
    published: BTreeMap<String, GateSnapshot>,
}

impl GateStore {
    pub fn record(&mut self, colony_id: &str, kind: GateKind, text: impl Into<String>) {
        self.scan
            .insert(colony_id.to_string(), GateSnapshot { kind, text: text.into() });
    }

    pub fn begin_scan(&mut self) {
        self.scan.clear();
    }

    pub fn publish(&mut self) {
        self.published = self.scan.clone();
    }

    pub fn summary(&self, queued_ids: &[String]) -> GateSummary {
        let mut counts: BTreeMap<GateKind, usize> = BTreeMap::new();
        let mut repo_counts: BTreeMap<String, usize> = BTreeMap::new();
        let mut org_counts: BTreeMap<String, usize> = BTreeMap::new();
        let mut admissible = 0;

        for id in queued_ids {
            if let Some(snapshot) = self.published.get(id) {
                *counts.entry(snapshot.kind).or_insert(0) += 1;
                if snapshot.kind == GateKind::RepoCap
                    && let Some(repo) = snapshot.text.split_whitespace().next()
                {
                    *repo_counts.entry(repo.to_string()).or_insert(0) += 1;
                } else if snapshot.kind == GateKind::OrgCap
                    && let Some(org) = snapshot.text.split_whitespace().next()
                {
                    *org_counts.entry(org.to_string()).or_insert(0) += 1;
                }
            } else {
                admissible += 1;
            }
        }

        let mut blocked_by = BTreeMap::new();
        for (kind, count) in counts {
            let key = kind.as_str().to_string();
            match kind {
                GateKind::RepoCap => {
                    let mut breakdown = BTreeMap::new();
                    for (repo, repo_count) in &repo_counts {
                        breakdown.insert(repo.clone(), json!(*repo_count));
                    }
                    blocked_by.insert(key, json!(breakdown));
                }
                GateKind::OrgCap => {
                    let mut breakdown = BTreeMap::new();
                    for (org, org_count) in &org_counts {
                        breakdown.insert(org.clone(), json!(*org_count));
                    }
                    blocked_by.insert(key, json!(breakdown));
                }
                _ => {
                    blocked_by.insert(key, json!(count));
                }
            }
        }

        GateSummary { admissible, blocked_by }
    }

    pub fn get_published(&self, colony_id: &str) -> Option<GateSnapshot> {
        self.published.get(colony_id).cloned()
    }

    pub fn dominant_gate_kind(&self, queued_ids: &[String]) -> Option<&'static str> {
        let mut counts: BTreeMap<GateKind, usize> = BTreeMap::new();
        for id in queued_ids {
            if let Some(snapshot) = self.published.get(id)
                && snapshot.kind != GateKind::Host
            {
                *counts.entry(snapshot.kind).or_insert(0) += 1;
            }
        }
        counts
            .into_iter()
            .max_by_key(|(_, count)| *count)
            .map(|(kind, _)| kind.as_str())
    }
}

static GATES: OnceLock<RwLock<GateStore>> = OnceLock::new();

pub(crate) fn record(colony_id: &str, kind: GateKind, text: impl Into<String>) {
    if let Ok(mut store) = GATES.get_or_init(|| RwLock::new(GateStore::default())).write() {
        store.record(colony_id, kind, text);
    }
}

pub(crate) fn record_held(colony_id: &str, text: impl Into<String>) {
    record(colony_id, GateKind::Held, text);
}

pub(crate) fn record_superseded(colony_id: &str) {
    record(
        colony_id,
        GateKind::Superseded,
        "superseded by a newer colony; starts only if kept",
    );
}

pub(crate) fn record_chain(colony_id: &str, parent_id: &str) {
    record(
        colony_id,
        GateKind::Chain,
        format!("queued behind colony {parent_id}: starts once that colony's pull request merges"),
    );
}

pub(crate) fn record_if_no_room(
    colony_id: &str,
    sessions: &[crate::sessions::Session],
    org: &str,
    repo: &str,
    max_parallel: usize,
    org_limit: Option<u64>,
    repo_limit: u64,
) {
    let busy = || sessions.iter().filter(|s| s.holds_slot());
    let busy_count = busy().count();
    let org_busy = busy().filter(|s| s.org.eq_ignore_ascii_case(org)).count();
    let repo_busy = busy().filter(|s| s.repo.eq_ignore_ascii_case(repo)).count() as u64;

    if busy_count >= max_parallel {
        record(
            colony_id,
            GateKind::Host,
            format!("global slot capacity exhausted ({}/{})", busy_count, max_parallel),
        );
    } else if let Some(limit) = org_limit
        && org_busy as u64 >= limit
    {
        record(colony_id, GateKind::OrgCap, format!("{org} org cap ({}/{})", org_busy, limit));
    } else if repo_busy >= repo_limit {
        record(
            colony_id,
            GateKind::RepoCap,
            format!("{repo} repo cap ({}/{})", repo_busy, repo_limit),
        );
    }
}

pub(crate) fn begin_scan() {
    if let Ok(mut store) = GATES.get_or_init(|| RwLock::new(GateStore::default())).write() {
        store.begin_scan();
    }
}

pub(crate) fn publish() {
    if let Ok(mut store) = GATES.get_or_init(|| RwLock::new(GateStore::default())).write() {
        store.publish();
    }
}

pub fn summary(queued_ids: &[String]) -> GateSummary {
    let gates = GATES.get_or_init(|| RwLock::new(GateStore::default()));
    if let Ok(store) = gates.read() {
        store.summary(queued_ids)
    } else {
        GateSummary {
            admissible: queued_ids.len(),
            blocked_by: BTreeMap::new(),
        }
    }
}

pub(crate) fn get_published(colony_id: &str) -> Option<GateSnapshot> {
    let gates = GATES.get_or_init(|| RwLock::new(GateStore::default()));
    gates.read().ok().and_then(|store| store.get_published(colony_id))
}

pub fn dominant_gate_kind(queued_ids: &[String]) -> Option<&'static str> {
    let gates = GATES.get_or_init(|| RwLock::new(GateStore::default()));
    if let Ok(store) = gates.read() {
        store.dominant_gate_kind(queued_ids)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_issue_scenario() {
        let mut store = GateStore::default();
        for i in 0..56 {
            store.record(&format!("r{i}"), GateKind::RepoCap, "Colonizer-dev/harness (3/3)");
        }
        for i in 0..23 {
            store.record(&format!("c{i}"), GateKind::Chain, "parent merge");
        }
        store.record("s1", GateKind::Superseded, "superseded");
        store.publish();

        let ids: Vec<String> = (0..56)
            .map(|i| format!("r{i}"))
            .chain((0..23).map(|i| format!("c{i}")))
            .chain(std::iter::once("s1".to_string()))
            .collect();

        assert_eq!(store.summary(&ids).admissible, 0);
        assert_eq!(store.dominant_gate_kind(&ids), Some("repo_cap"));
    }

    #[test]
    fn test_mixed_with_admissible() {
        let mut store = GateStore::default();
        store.record("held1", GateKind::Held, "retry");
        store.publish();
        assert_eq!(
            store.summary(&["held1".to_string(), "admissible1".to_string()]).admissible,
            1
        );
    }

    #[test]
    fn test_stale_entries() {
        let mut store = GateStore::default();
        store.record("c1", GateKind::Held, "old");
        store.publish();
        assert!(store.summary(&["c1".to_string()]).admissible == 0);

        store.begin_scan();
        store.publish();
        assert_eq!(store.summary(&["c1".to_string()]).admissible, 1);
    }

    #[test]
    fn test_get_published() {
        let mut store = GateStore::default();
        store.record("test", GateKind::Held, "test gate");
        store.publish();
        let gate = store.get_published("test");
        assert!(gate.is_some());
        assert_eq!(gate.unwrap().kind, GateKind::Held);
    }
}
