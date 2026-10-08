//! The feed's own read-only keys (issue #895), and the client-address rules they carry.
//!
//! These are deliberately *not* scoped API tokens (`api_tokens.rs`). A scoped token is a
//! least-privilege credential inside the cockpit's own permission model, and it authenticates in
//! `host_guard` before the router runs. A feed key is the opposite: a read-only credential for a
//! third-party site that holds no cockpit token at all, and it is checked by the handler on the
//! feed's two routes alone. Sharing one store would have meant either publishing the scope model
//! to the public internet or teaching `host_guard` about a credential every other route must
//! refuse.
//!
//! The store is `<config_dir>/feed-keys.json`, holding only a SHA-256 hash of each key: the
//! plaintext is printed once at creation and exists nowhere else. A file that will not read or
//! parse is reported on stderr and answers no keys, which refuses every reader — the direction a
//! damaged credential store has to fail in.

use crate::util::{self, short_id};
use axum::extract::{ConnectInfo, Extension};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
};

/// The prefix every feed key carries, so it can never be mistaken at a glance for the install's
/// API token, a scoped `col_` token or a phone's `cph_` cookie.
pub(crate) const KEY_PREFIX: &str = "cfd_";
/// The rate a key gets when its record does not say: a handful of reads a minute is a page
/// polling, more than that is a client that should ask for a window of its own.
const DEFAULT_RATE_PER_MINUTE: u32 = 60;
/// A key name is a label, not a value: long enough to be descriptive, short enough for a log line.
const MAX_NAME: usize = 120;
/// The window the per-key rate limit counts in.
const RATE_WINDOW_SECS: i64 = 60;
/// The ceiling on `rate_limit_per_minute`. A key reads the feed and nothing else; ten requests a
/// second is already far past what any site needs, and above it the ledger's `VecDeque` is
/// unbounded — `--rate 4294967295` would otherwise hold every request in the window in memory, for
/// a limit that exists to stop a runaway client rather than to account for one.
const MAX_RATE_PER_MINUTE: u32 = 600;

/// One key as it is persisted: everything but the plaintext, which is never here.
/// `#[serde(default)]` throughout, so a record written by an older build still reads.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct FeedKey {
    pub id: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    /// When the key was revoked. A revoked record is kept, not deleted, so an audit of who held a
    /// key outlives the credential.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
    /// Addresses and CIDR blocks (v4 and v6) allowed to present this key. Empty means any
    /// address: the key alone is then the whole control, which is why creation says so.
    pub ip_allowlist: Vec<String>,
    /// How many requests a minute this key may make.
    pub rate_limit_per_minute: u32,
    /// SHA-256 of the plaintext key, hex. The hash is what makes the plaintext disposable.
    pub token_hash: String,
}

impl Default for FeedKey {
    fn default() -> Self {
        FeedKey {
            id: String::new(),
            name: String::new(),
            created_at: Utc::now(),
            revoked_at: None,
            ip_allowlist: Vec::new(),
            rate_limit_per_minute: DEFAULT_RATE_PER_MINUTE,
            token_hash: String::new(),
        }
    }
}

impl FeedKey {
    /// Whether the key has been revoked. A revoked key is a record, not a credential.
    pub(crate) fn live(&self) -> bool {
        self.revoked_at.is_none()
    }

    /// The view `feed-key list` prints: everything but the hash.
    pub(crate) fn meta(&self) -> KeyMeta {
        KeyMeta {
            id: self.id.clone(),
            name: self.name.clone(),
            created_at: self.created_at,
            revoked_at: self.revoked_at,
            ip_allowlist: self.ip_allowlist.clone(),
            rate_limit_per_minute: self.rate_limit_per_minute,
        }
    }
}

/// One key as `feed-key list` answers: metadata only, never the hash and never the plaintext.
#[derive(Clone, Debug, Serialize)]
pub struct KeyMeta {
    pub id: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
    pub ip_allowlist: Vec<String>,
    pub rate_limit_per_minute: u32,
}

/// What `feed-key create` takes. The plaintext is not asked for and cannot be set.
#[derive(Clone, Debug)]
pub struct NewKey {
    pub name: String,
    pub ip_allowlist: Vec<String>,
    pub rate_limit_per_minute: Option<u32>,
}

/// SHA-256 of a feed key, hex — the same reduction `api_tokens::hash_token` uses for its tokens.
fn hash(plaintext: &str) -> String {
    util::hex(ring::digest::digest(&ring::digest::SHA256, plaintext.as_bytes()).as_ref())
}

// ---------------------------------------------------------------------------
// Client addresses: the per-key allowlist, matched by hand.
// ---------------------------------------------------------------------------

/// Addresses already warned about as unparseable, so a stored typo is reported once per run
/// rather than on every request it fails to match.
static WARNED: LazyLock<Mutex<Vec<String>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// Whether `ip` is inside any entry of `allowlist`. An empty allowlist allows every address; an
/// entry that does not parse matches nothing, so a malformed CIDR can never widen the list.
pub(crate) fn ip_allowed(allowlist: &[String], ip: IpAddr) -> bool {
    if allowlist.is_empty() {
        return true;
    }
    for entry in allowlist {
        match cidr_matches(entry, ip) {
            Some(true) => return true,
            Some(false) => {}
            None => warn_unparseable(entry),
        }
    }
    false
}

/// One allowlist entry read: a bare address (an exact match) or `addr/bits`. `None` when it does
/// not parse. The prefix is clamped to the address family's width, so `/40` on a v4 address is the
/// `/32` the operator obviously meant rather than a rejected allowlist entry.
fn parse_cidr(entry: &str) -> Option<(IpAddr, u8)> {
    let entry = entry.trim();
    let (address, bits) = match entry.split_once('/') {
        Some((address, bits)) => (address.trim(), Some(bits.trim().parse::<u8>().ok()?)),
        None => (entry, None),
    };
    let network: IpAddr = address.parse().ok()?;
    let width = match network {
        IpAddr::V4(_) => 32,
        IpAddr::V6(_) => 128,
    };
    Some((network, bits.unwrap_or(width).min(width)))
}

/// Whether `ip` falls inside one allowlist entry. `None` when the entry does not parse — never
/// `true`, so a bad entry fails closed rather than failing open.
pub(crate) fn cidr_matches(entry: &str, ip: IpAddr) -> Option<bool> {
    let (network, bits) = parse_cidr(entry)?;
    // A v4 peer arriving on a v6 socket is `::ffff:a.b.c.d`; the operator wrote the v4 address.
    let ip = match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        v4 => v4,
    };
    Some(match (network, ip) {
        (IpAddr::V4(net), IpAddr::V4(addr)) => {
            let mask = v4_mask(bits);
            u32::from(net) & mask == u32::from(addr) & mask
        }
        (IpAddr::V6(net), IpAddr::V6(addr)) => {
            let mask = v6_mask(bits);
            u128::from(net) & mask == u128::from(addr) & mask
        }
        // A v6 block never covers a v4 address, or the other way round.
        _ => false,
    })
}

fn v4_mask(bits: u8) -> u32 {
    if bits == 0 { 0 } else { u32::MAX << (32 - u32::from(bits)) }
}

fn v6_mask(bits: u8) -> u128 {
    if bits == 0 { 0 } else { u128::MAX << (128 - u32::from(bits)) }
}

fn warn_unparseable(entry: &str) {
    let Ok(mut warned) = WARNED.lock() else { return };
    if warned.iter().any(|e| e == entry) {
        return;
    }
    warned.push(entry.to_string());
    eprintln!("public feed: ignoring ip_allowlist entry \"{entry}\": not an address or CIDR");
}

/// The per-key request ledger: the timestamps of a key's requests inside the window. In memory
/// only — a restart hands every key a fresh window, which is the cheap direction for a limit that
/// exists to stop a runaway client, not to account for one.
static LEDGER: LazyLock<Mutex<HashMap<String, VecDeque<i64>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Records one request against `key` and reports whether it is inside the limit. `now` is epoch
/// seconds, passed in so the shape is testable without a clock.
pub(crate) fn take_slot(key: &FeedKey, now: i64) -> bool {
    let Ok(mut ledger) = LEDGER.lock() else {
        // A poisoned ledger must not fail the request path: one lost window is better than every
        // reader of the feed being refused.
        return true;
    };
    let hits = ledger.entry(key.id.clone()).or_default();
    while hits.front().is_some_and(|at| *at <= now - RATE_WINDOW_SECS) {
        hits.pop_front();
    }
    if hits.len() >= key.rate_limit_per_minute as usize {
        return false;
    }
    hits.push_back(now);
    true
}

/// When the ledger was last swept, so the sweep costs at most one walk a minute whatever the
/// request rate.
static LAST_SWEEP: LazyLock<Mutex<i64>> = LazyLock::new(|| Mutex::new(i64::MIN));

/// Drops ledger entries that no longer describe anything: a key whose window has gone quiet, and
/// a key that is no longer live.
///
/// Entries are otherwise only trimmed when that same key is seen again, so without this every key
/// ever used — revoked ones included — keeps a `VecDeque` for the life of the process. Called
/// from the request path on a timer rather than from `revoke`, because a key can also go quiet on
/// its own. A revoked key loses its window, not the record: the audit still lives in the file.
pub(crate) fn sweep(config_dir: &Path, now: i64) {
    {
        let Ok(mut last) = LAST_SWEEP.lock() else { return };
        if now.saturating_sub(*last) < RATE_WINDOW_SECS {
            return;
        }
        *last = now;
    }
    let live: HashSet<String> = load(config_dir)
        .into_iter()
        .filter(|key| key.live())
        .map(|key| key.id)
        .collect();
    let Ok(mut ledger) = LEDGER.lock() else { return };
    ledger.retain(|id, hits| {
        if !live.contains(id.as_str()) {
            return false;
        }
        while hits.front().is_some_and(|at| *at <= now - RATE_WINDOW_SECS) {
            hits.pop_front();
        }
        !hits.is_empty()
    });
}

// ---------------------------------------------------------------------------
// The store.
// ---------------------------------------------------------------------------

/// The key file, beside the other saved credentials in the config dir.
pub fn file(config_dir: &Path) -> PathBuf {
    config_dir.join("feed-keys.json")
}

/// Every key this install has, read fresh from the file. Never fails: a file that cannot be read
/// or parsed is named on stderr and answers no keys, which refuses every reader rather than
/// letting a damaged store authenticate anything.
pub fn load(config_dir: &Path) -> Vec<FeedKey> {
    let path = file(config_dir);
    match std::fs::read(&path) {
        // A missing file is a first use, not a fault.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            eprintln!(
                "public feed: could not read {} ({e}); refusing every feed key",
                path.display()
            );
            Vec::new()
        }
        Ok(bytes) => match serde_json::from_slice::<Vec<FeedKey>>(&bytes) {
            Ok(keys) => keys,
            Err(e) => {
                eprintln!(
                    "public feed: {} does not parse ({e}); refusing every feed key until the file is repaired or removed",
                    path.display()
                );
                Vec::new()
            }
        },
    }
}

/// Writes the key list through to disk, 0600 like the owner token beside it. A failure is loud but
/// not fatal: the keys just made or revoked work for this run, and the next change retries.
fn save(config_dir: &Path, keys: &[FeedKey]) {
    let path = file(config_dir);
    let Ok(bytes) = serde_json::to_vec_pretty(keys) else {
        return; // a fixed-shape list cannot fail to serialize
    };
    if let Err(e) = util::write_private(&path, &bytes) {
        eprintln!("public feed: could not save {}: {e}", path.display());
    }
}

/// Mints a key: validates, stores only the hash, and returns the plaintext exactly once.
pub fn create(config_dir: &Path, req: NewKey) -> Result<(String, KeyMeta), String> {
    let name = req.name.trim();
    if name.is_empty() {
        return Err("name is required".to_string());
    }
    if name.len() > MAX_NAME {
        return Err(format!("name is {} characters; keep it under {MAX_NAME}", name.len()));
    }
    let ip_allowlist = req
        .ip_allowlist
        .iter()
        .map(|entry| entry.trim().to_string())
        .filter(|entry| !entry.is_empty())
        .collect::<Vec<_>>();
    // Refused at creation rather than ignored at match time: an allowlist that silently drops an
    // entry the operator believed in is a list they cannot reason about.
    for entry in &ip_allowlist {
        if parse_cidr(entry).is_none() {
            return Err(format!(
                "--ip {entry} is not an address or CIDR (for example 203.0.113.7 or 203.0.113.0/24)"
            ));
        }
    }
    let rate = match req.rate_limit_per_minute {
        Some(0) => return Err("--rate must be at least 1 request a minute".to_string()),
        // Refused rather than clamped: a limit the operator cannot reason about is not a control,
        // and the ceiling exists to keep the ledger bounded, not to negotiate.
        Some(rate) if rate > MAX_RATE_PER_MINUTE => {
            return Err(format!(
                "--rate {rate} is above the ceiling of {MAX_RATE_PER_MINUTE} requests a minute"
            ));
        }
        Some(rate) => rate,
        None => DEFAULT_RATE_PER_MINUTE,
    };
    let plaintext = format!("{KEY_PREFIX}{}", util::random_token());
    let stored = FeedKey {
        id: format!("cfk_{}", short_id()),
        name: name.to_string(),
        created_at: Utc::now(),
        revoked_at: None,
        ip_allowlist,
        rate_limit_per_minute: rate,
        token_hash: hash(&plaintext),
    };
    let meta = stored.meta();
    let mut keys = load(config_dir);
    keys.push(stored);
    save(config_dir, &keys);
    Ok((plaintext, meta))
}

/// Revokes a key; `None` when no key carries the id. The record stays, stamped `revoked_at`, so an
/// audit of who held a key outlives the credential. Presenting it stops working at once: the next
/// request reads the file and finds nothing live.
pub fn revoke(config_dir: &Path, id: &str) -> Option<KeyMeta> {
    let mut keys = load(config_dir);
    let key = keys.iter_mut().find(|k| k.id == id)?;
    if key.revoked_at.is_none() {
        key.revoked_at = Some(Utc::now());
    }
    let meta = key.meta();
    save(config_dir, &keys);
    Some(meta)
}

/// Every key's metadata, oldest first.
pub fn list(config_dir: &Path) -> Vec<KeyMeta> {
    load(config_dir).iter().map(FeedKey::meta).collect()
}

/// The live key a presented plaintext belongs to, or `None` — which is also the answer for an
/// unknown key, a revoked one and a store that would not parse, so a caller cannot tell which of
/// the three it was.
pub fn authenticate(config_dir: &Path, presented: &str) -> Option<FeedKey> {
    let presented = presented.trim();
    if presented.is_empty() {
        return None;
    }
    // Here rather than in `take_slot`, which has no config dir to read liveness from.
    sweep(config_dir, Utc::now().timestamp());
    let digest = hash(presented);
    load(config_dir)
        .into_iter()
        .find(|key| key.live() && crate::gateway::constant_time_eq(key.token_hash.as_bytes(), digest.as_bytes()))
}

/// The address a request arrived from, or `None` when the server was not given one. Absent means
/// "no address information", which the handler reads as "cannot judge the allowlist" rather than
/// as a refusal; docs/protocol/public-feed.md says so.
pub(crate) fn peer_ip(connect: Option<Extension<ConnectInfo<SocketAddr>>>) -> Option<IpAddr> {
    connect.map(|Extension(ConnectInfo(addr))| addr.ip())
}

#[cfg(test)]
mod tests;
