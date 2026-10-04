//! Fleet network policy (issue #690): the fleet owner's one egress floor for every member's
//! colonies, and the member-side clamp that makes it a floor rather than a suggestion.
//!
//! An owner sets it once (a fleet-wide `egress`, per-org and per-repo overrides, and a `reach` map)
//! and serves it at `GET /api/fleet/policy`; a member fetches it with its fleet token on the
//! history-push cadence (`fleet_sync.rs`), caches it, and clamps its resolved policy to it at boot
//! (`boot.rs`) — tighten, never loosen. A refusal is logged and recorded, so
//! `GET /api/sessions/{id}/egress` shows what was refused and why. The floor composes as an org's
//! overrides do (`egress::resolve`, #303); `reach` is keyed by member id (`mem_…`, or `"owner"`).

use crate::egress::{self, EgressMode, EgressPolicy};
use crate::orgs::EgressOverrides;
use crate::{ApiResult, Shared, client_error};
use axum::{Json, extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

/// Where the policy lives: the owner's own copy, and the member's cache of it (fleet_members.rs).
pub(crate) const POLICY_FILE: &str = "fleet-policy.json";
/// A wedged owner is skipped after this, never a hang; and a ceiling on what its answer may weigh.
const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const MAX_POLICY_BYTES: usize = 512 * 1024;
/// Validation bounds, so a hand-written or hostile policy cannot name an unbounded file.
const MAX_LEVELS: usize = 512;
const MAX_ENTRIES: usize = 256;
const MAX_ID: usize = 256;

/// The fleet owner's policy: the egress floor every member's colonies boot under, its overrides, and
/// who may reach whom. All optional — an empty policy is no floor at all.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FleetPolicy {
    /// The fleet-wide floor: the only level that covers every colony, whatever its org or repo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub egress: Option<EgressOverrides>,
    /// Per-org overrides, keyed by the GitHub org (repository owner).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub orgs: BTreeMap<String, EgressOverrides>,
    /// Per-repo overrides, keyed `owner/name` (the same string a session stores as its `repo`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub repos: BTreeMap<String, EgressOverrides>,
    /// Who may reach whom: a key present may reach only the ids it lists, an absent key every member.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reach: BTreeMap<String, Vec<String>>,
    /// Provenance: set on a member's cached copy, absent (not shipped) on the owner's own.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub from_owner: bool,
}

impl FleetPolicy {
    /// The floor a colony in `org` / `repo` boots under: `None` when no level names anything — a
    /// policy fencing only some orgs or repos is deliberately no floor for the rest.
    pub(crate) fn floor(&self, org: &str, repo: &str) -> Option<EgressPolicy> {
        let repo_level = self.repos.get(repo).or_else(|| self.repos.get(&format!("{org}/{repo}")));
        let levels = [self.egress.as_ref(), self.orgs.get(org), repo_level];
        if levels.iter().all(Option::is_none) {
            return None;
        }
        // The mode is the most specific level naming one (repo, org, fleet); the lists are the union.
        let mode = levels
            .iter()
            .rev()
            .filter_map(|level| level.as_ref().and_then(|o| o.mode.as_deref()))
            .map(str::trim)
            .filter(|mode| !mode.is_empty())
            .find_map(|mode| EgressMode::parse(mode).ok())
            .unwrap_or_default();
        let (mut allow, mut block) = (Vec::new(), Vec::new());
        for level in levels.iter().flatten() {
            for (entries, into) in [(level.allow.as_deref(), &mut allow), (level.block.as_deref(), &mut block)] {
                for entry in entries.unwrap_or_default() {
                    if egress::validate_entry(entry).is_ok() && !into.contains(entry) {
                        into.push(entry.clone());
                    }
                }
            }
        }
        Some(EgressPolicy { mode, allow, block })
    }

    /// Whether a colony on host `from` may reach host `to`. An absent `from` key may reach every
    /// member; a present one may reach only the ids it lists.
    pub(crate) fn may_reach(&self, from: &str, to: &str) -> bool {
        match self.reach.get(from) {
            None => true,
            Some(allowed) => allowed.iter().any(|host| host == to),
        }
    }
}

/// The member-side clamp: `local` (the colony's own resolved policy) may tighten `floor`, never
/// loosen it. Answers what the colony boots under and a note for each loosening refused:
///
/// - An `Allowlist` floor forces `Allowlist`; a local `open` is refused and takes the floor's
///   allowlist whole (it had no fence). A local `Allowlist` tightens: only entries the floor covers
///   survive, and an empty local list is a deny-all that stays empty. A local `Allowlist` under an
///   `Open` floor is kept whole. The block list is the union, so a local can never drop a floor block.
pub(crate) fn enforce(floor: &EgressPolicy, local: EgressPolicy) -> (EgressPolicy, Vec<String>) {
    let mut refused = Vec::new();
    let mut block = floor.block.clone();
    for entry in &local.block {
        if !block.contains(entry) {
            block.push(entry.clone());
        }
    }
    let (mode, allow) = if floor.mode == EgressMode::Allowlist {
        if local.mode != EgressMode::Allowlist {
            refused.push("egress mode `open` was refused: the fleet requires an allowlist".into());
            (EgressMode::Allowlist, floor.allow.clone())
        } else {
            let mut kept = Vec::new();
            for entry in &local.allow {
                if floor.allow.iter().any(|f| covered(entry, f)) {
                    kept.push(entry.clone());
                } else {
                    refused.push(format!("allow `{entry}` was refused: the fleet allowlist does not cover it"));
                }
            }
            (EgressMode::Allowlist, kept)
        }
    } else {
        (local.mode, local.allow.clone())
    };
    (EgressPolicy { mode, allow, block }, refused)
}

/// Whether floor entry `outer` reaches everywhere `inner` does, so `inner` adds nothing. Exact
/// entries cover themselves, a wider port covers a narrower, a `*.host` wildcard covers subdomains
/// but never the apex; IPs and CIDRs cover only themselves (an under-match merely narrows reach).
fn covered(inner: &str, outer: &str) -> bool {
    let (inner_target, inner_port) = target_of(inner);
    let (outer_target, outer_port) = target_of(outer);
    if let Some(port) = outer_port
        && inner_port != Some(port)
    {
        return false;
    }
    if inner_target == outer_target {
        return true;
    }
    let Some(suffix) = outer_target.strip_prefix("suffix=") else {
        return false;
    };
    match inner_target.strip_prefix("suffix=") {
        // A wildcard inner: covered when its own suffix is `suffix` or a subdomain of it.
        Some(inner_suffix) => inner_suffix == suffix || inner_suffix.ends_with(&format!(".{suffix}")),
        // An exact inner: covered only as a strict subdomain, never as the apex itself.
        None => inner_target.ends_with(&format!(".{suffix}")),
    }
}

/// An entry split the way msb's token spells it: the target, and the one port it names (if any).
fn target_of(entry: &str) -> (String, Option<u16>) {
    let token = egress::entry_target(entry);
    match token.rsplit_once(":tcp:") {
        Some((target, port)) => (target.to_string(), port.parse().ok()),
        None => (token, None),
    }
}

/// Loads the policy, never failing: a missing or damaged file reads as no floor. Ignoring a damaged
/// file could only loosen a member against a fleet it no longer hears from; the owner's next push
/// repairs the cache.
pub(crate) fn load(config_dir: &Path) -> Option<FleetPolicy> {
    let path = config_dir.join(POLICY_FILE);
    let bytes = std::fs::read(&path).ok()?;
    match serde_json::from_slice::<FleetPolicy>(&bytes) {
        Ok(policy) => Some(policy),
        Err(e) => {
            eprintln!("fleet policy: {} does not parse ({e}); ignoring it", path.display());
            None
        }
    }
}

async fn save(config_dir: &Path, policy: &FleetPolicy) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec_pretty(policy)?;
    crate::util::write_atomic(&config_dir.join(POLICY_FILE), &bytes).await
}

/// Validates a policy before it is stored or believed: bounded counts and ids, and every mode and
/// egress entry through the same parsers an operator's entry goes through (`egress.rs`).
fn validate(policy: &FleetPolicy) -> Result<(), String> {
    let levels = policy.egress.iter().chain(policy.orgs.values()).chain(policy.repos.values());
    if levels.clone().count() > MAX_LEVELS {
        return Err(format!("at most {MAX_LEVELS} org and repo overrides"));
    }
    for level in levels {
        if let Some(mode) = level.mode.as_deref().map(str::trim).filter(|m| !m.is_empty()) {
            EgressMode::parse(mode)?;
        }
        for entries in [level.allow.as_deref(), level.block.as_deref()].into_iter().flatten() {
            if entries.len() > MAX_ENTRIES {
                return Err(format!("at most {MAX_ENTRIES} entries per policy list"));
            }
            entries
                .iter()
                .try_for_each(|e| egress::validate_entry(e).map_err(|w| format!("invalid egress entry: {w}")))?;
        }
    }
    if policy.reach.len() > MAX_LEVELS || policy.reach.values().any(|to| to.len() > MAX_ENTRIES) {
        return Err(format!("at most {MAX_LEVELS} reach entries of {MAX_ENTRIES} hosts each"));
    }
    for (from, to) in &policy.reach {
        if from.is_empty() || from.len() > MAX_ID {
            return Err(format!("a reach key must be 1 to {MAX_ID} characters"));
        }
        if to.iter().any(|host| host.is_empty() || host.len() > MAX_ID) {
            return Err(format!("a reach host must be 1 to {MAX_ID} characters"));
        }
    }
    Ok(())
}

/// Fetches the owner's policy over this member's fleet token and caches it. A member no longer in a
/// fleet drops a policy cached from a former owner — it is not bound by it any more. A failed fetch
/// changes nothing: the last cached policy stands, so staleness can only keep a fence up, never
/// take one down.
pub(crate) async fn refresh(app: &Shared) -> anyhow::Result<()> {
    let config_dir = app.cfg.config_dir.clone();
    let Some(target) = app.fleet_members.membership().await else {
        if load(&config_dir).is_some_and(|p| p.from_owner) {
            let _ = std::fs::remove_file(config_dir.join(POLICY_FILE));
        }
        return Ok(());
    };
    let url = format!("{}/api/fleet/policy", target.owner_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .user_agent(concat!("colonizer/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut res = client
        .get(&url)
        .bearer_auth(&target.token)
        .timeout(FETCH_TIMEOUT)
        .send()
        .await?;
    if !res.status().is_success() {
        anyhow::bail!("the owner answered {} to the fleet policy", res.status());
    }
    let mut body = Vec::new();
    while let Some(chunk) = res.chunk().await? {
        if body.len() + chunk.len() > MAX_POLICY_BYTES {
            anyhow::bail!("the owner's fleet policy is too large");
        }
        body.extend_from_slice(&chunk);
    }
    let mut policy: FleetPolicy = serde_json::from_slice(&body)?;
    validate(&policy).map_err(|why| anyhow::anyhow!("the owner's fleet policy is invalid: {why}"))?;
    policy.from_owner = true;
    save(&config_dir, &policy).await?;
    Ok(())
}

/// `GET /api/fleet/policy`: the policy in force here — the owner's own, or a member's cached copy.
/// Readable by the owner and by a fleet token (`api_tokens::classify`).
pub async fn show(State(app): State<Shared>) -> Json<Value> {
    let policy = load(&app.cfg.config_dir).unwrap_or_default();
    Json(serde_json::to_value(policy).unwrap_or(Value::Null))
}

/// `PUT /api/fleet/policy`: the owner sets the fleet's floor. Owner-only (unlisted in `classify`),
/// and **409** on a mothership that is itself a member: the policy is the fleet owner's word.
pub async fn put(State(app): State<Shared>, Json(mut policy): Json<FleetPolicy>) -> ApiResult<Value> {
    if app.fleet_members.membership().await.is_some() {
        return Err(client_error(
            StatusCode::CONFLICT,
            "this mothership is a member of a fleet; the fleet policy is set by the fleet owner and cannot be replaced here",
        ));
    }
    validate(&policy).map_err(|why| client_error(StatusCode::BAD_REQUEST, &why))?;
    policy.from_owner = false;
    save(&app.cfg.config_dir, &policy)
        .await
        .map_err(|e| client_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}")))?;
    Ok(Json(serde_json::to_value(policy).unwrap_or(Value::Null)))
}

/// The API routes this module serves. `server::api_routes` merges them in.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing::get;
    axum::Router::new().route("/api/fleet/policy", get(show).put(put))
}
#[cfg(test)]
mod tests {
    use super::*;

    fn list(entries: &[&str]) -> Vec<String> {
        entries.iter().map(|entry| entry.to_string()).collect()
    }

    fn over(mode: Option<&str>, allow: &[&str], block: &[&str]) -> EgressOverrides {
        EgressOverrides {
            mode: mode.map(String::from),
            allow: Some(list(allow)),
            block: Some(list(block)),
        }
    }

    fn policy(mode: EgressMode, allow: &[&str], block: &[&str]) -> EgressPolicy {
        EgressPolicy {
            mode,
            allow: list(allow),
            block: list(block),
        }
    }

    fn named(refused: &[String], entry: &str) -> bool {
        refused.iter().any(|why| why.contains(&format!("`{entry}`")))
    }

    /// The heart of the slice: a member may tighten the fleet floor, never loosen it — a local
    /// `open`, an uncovered allow, the wildcard's apex and a dropped block are all refused, while a
    /// deny-all and a local narrowing are kept. Every refusal is named for the log and the record.
    #[test]
    fn a_member_cannot_loosen_the_fleet_policy() {
        use EgressMode::{Allowlist, Open};
        let floor = policy(Allowlist, &["api.github.com", "*.example.com"], &["10.0.0.0/8"]);
        // A local `open` had no fence to keep: it is refused the mode and inherits the floor whole.
        let (clamped, refused) = enforce(&floor, policy(Open, &["evil.example.org"], &[]));
        assert_eq!(clamped, floor, "a local `open` takes the floor's fence, block and all");
        assert!(named(&refused, "open"), "{refused:?}");
        // A local allowlist tightens: covered entries stay (exact, and a strict subdomain of the
        // wildcard, port and all); the apex is not a subdomain; an uncovered entry is dropped.
        let allows = ["api.github.com", "api.example.com:443", "example.com", "evil.example.org"];
        let (clamped, refused) = enforce(&floor, policy(Allowlist, &allows, &["192.0.2.0/24"]));
        assert_eq!(clamped.allow, ["api.github.com", "api.example.com:443"]);
        assert_eq!(clamped.block, ["10.0.0.0/8", "192.0.2.0/24"]);
        assert!(
            named(&refused, "evil.example.org") && named(&refused, "example.com"),
            "{refused:?}"
        );
        // A local deny-all is stricter than the floor and stays empty; a tightening under an open
        // floor is kept whole.
        assert!(
            enforce(&floor, policy(Allowlist, &[], &[])).0.allow.is_empty(),
            "a deny-all stays empty"
        );
        let (clamped, refused) = enforce(&policy(Open, &[], &[]), policy(Allowlist, &["api.github.com"], &[]));
        assert_eq!((clamped.mode, clamped.allow), (Allowlist, vec!["api.github.com".to_string()]));
        assert!(refused.is_empty(), "{refused:?}");
    }

    /// The floor merges the fleet, org and repo levels with the existing union semantics.
    #[test]
    fn the_floor_merges_the_fleet_org_and_repo_levels() {
        use EgressMode::Open;
        let mut policy = FleetPolicy {
            egress: Some(over(None, &["deb.debian.org"], &["10.0.0.0/8"])),
            ..Default::default()
        };
        policy
            .orgs
            .insert("acme".into(), over(Some("allowlist"), &["api.github.com"], &[]));
        policy
            .repos
            .insert("acme/rocket".into(), over(None, &["registry.npmjs.org"], &["240.0.0.0/4"]));
        // The mode is the most specific level that names one; the lists are the union of them all.
        let floor = policy.floor("acme", "acme/rocket").unwrap();
        assert_eq!(floor.mode, EgressMode::Allowlist, "the org names the most specific mode");
        assert_eq!(floor.allow, ["deb.debian.org", "api.github.com", "registry.npmjs.org"]);
        assert_eq!(floor.block, ["10.0.0.0/8", "240.0.0.0/4"]);
        // A repo with no level of its own gets the org and fleet floors; a bare name is found too.
        assert_eq!(
            policy.floor("acme", "acme/other").unwrap().allow,
            ["deb.debian.org", "api.github.com"]
        );
        assert_eq!(policy.floor("acme", "rocket").unwrap().allow, floor.allow);
        // An org with no entry gets the fleet level alone; a policy fencing one org is no floor for
        // another, and only the fleet-wide `egress` covers every colony.
        assert_eq!(policy.floor("other", "other/repo").unwrap().mode, Open);
        let org_only = FleetPolicy {
            orgs: policy.orgs.clone(),
            ..Default::default()
        };
        assert!(
            org_only.floor("other", "other/repo").is_none(),
            "an org entry is no floor elsewhere"
        );
        assert!(
            FleetPolicy::default().floor("acme", "acme/rocket").is_none(),
            "no entry names no floor"
        );
        // A hand-written entry that does not parse, and a bad mode, are dropped, never fatal.
        let bad = FleetPolicy {
            egress: Some(over(Some("sideways"), &["not a host"], &[])),
            ..Default::default()
        };
        let floor = bad.floor("o", "o/r").unwrap();
        assert!(
            floor.allow.is_empty() && floor.mode == Open,
            "a bad entry and mode are dropped"
        );
        assert!(FleetPolicy::default().floor("o", "o/r").is_none());
    }

    /// Reach: a key present may reach only its list; an absent key may reach every member.
    #[test]
    fn reach_says_who_may_reach_whom() {
        let mut policy = FleetPolicy::default();
        policy.reach.insert("mem_a".into(), vec!["mem_b".into(), "owner".into()]);
        assert!(policy.may_reach("mem_a", "mem_b") && policy.may_reach("mem_a", "owner"));
        assert!(!policy.may_reach("mem_a", "mem_c"));
        assert!(policy.may_reach("mem_z", "mem_c"), "an absent key is unconstrained");
    }
}
