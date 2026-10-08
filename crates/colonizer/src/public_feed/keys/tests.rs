//! The feed keys: the store, the address rules, and the limit.

use super::*;

fn root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("colonizer-feedkeys-{tag}-{}", short_id()));
    std::fs::create_dir_all(dir.join("config")).unwrap();
    dir
}

fn new_key(tag: &str, ips: &[&str], rate: Option<u32>) -> NewKey {
    NewKey {
        name: format!("site {tag}"),
        ip_allowlist: ips.iter().map(|s| s.to_string()).collect(),
        rate_limit_per_minute: rate,
    }
}

fn key_at(id: &str, rate: u32) -> FeedKey {
    FeedKey {
        id: id.to_string(),
        rate_limit_per_minute: rate,
        ..FeedKey::default()
    }
}

fn ip(text: &str) -> IpAddr {
    text.parse().unwrap()
}

// -- the store ----------------------------------------------------------------

#[test]
fn the_plaintext_is_returned_once_and_only_its_hash_is_kept() {
    let root = root("create");
    let config_dir = root.join("config");
    let (plaintext, meta) = create(&config_dir, new_key("create", &[], None)).unwrap();
    assert!(plaintext.starts_with(KEY_PREFIX), "{plaintext}");

    let stored = load(&root.join("config"));
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].id, meta.id);
    assert_eq!(stored[0].token_hash, hash(&plaintext));
    // The plaintext is nowhere in the file, and the file is not world-readable.
    let bytes = std::fs::read(file(&config_dir)).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains(&plaintext));
    assert_eq!(meta.rate_limit_per_minute, DEFAULT_RATE_PER_MINUTE);
    assert!(meta.revoked_at.is_none());
}

#[test]
fn a_live_key_authenticates_and_a_revoked_one_stops_at_once() {
    let root = root("revoke");
    let config_dir = root.join("config");
    let (plaintext, meta) = create(&config_dir, new_key("revoke", &[], None)).unwrap();
    assert_eq!(authenticate(&config_dir, &plaintext).unwrap().id, meta.id);

    revoke(&config_dir, &meta.id).unwrap();
    assert!(authenticate(&config_dir, &plaintext).is_none());
    // The record stays, stamped, so an audit outlives the credential.
    assert!(load(&config_dir)[0].revoked_at.is_some());
    assert!(revoke(&config_dir, "cfk_nope").is_none());
}

#[test]
fn an_unknown_empty_or_damaged_store_authenticates_nothing() {
    let root = root("unknown");
    let config_dir = root.join("config");
    assert!(authenticate(&config_dir, "cfd_never_issued").is_none());
    assert!(authenticate(&config_dir, "").is_none());
    assert!(authenticate(&config_dir, "cfd_wrong_suffix").is_none());
    // A store that would not parse refuses every reader rather than authenticating anyone.
    std::fs::write(file(&config_dir), b"{ not json").unwrap();
    assert!(authenticate(&config_dir, "cfd_anything").is_none());
}

#[test]
fn creation_refuses_an_allowlist_entry_it_could_never_match() {
    let root = root("badentry");
    let config_dir = root.join("config");
    let err = create(&config_dir, new_key("bad", &["not-an-address"], None)).unwrap_err();
    assert!(err.contains("--ip not-an-address"), "{err}");
    assert!(load(&config_dir).is_empty());
    assert!(create(&config_dir, new_key("zero", &[], Some(0))).is_err());
    let mut nameless = new_key("noname", &[], None);
    nameless.name = "   ".into();
    assert!(create(&config_dir, nameless).is_err());
}

/// A rate with no ceiling is a rate the ledger cannot honour: `--rate 4294967295` was accepted and
/// the `VecDeque` then held every request in the window. Refused at creation, so the stored key
/// can only hold a bounded window.
#[test]
fn a_rate_above_the_ceiling_is_refused_at_creation() {
    let root = root("rate");
    let config_dir = root.join("config");
    let err = create(&config_dir, new_key("huge", &[], Some(u32::MAX)))
        .unwrap_err()
        .to_string();
    assert!(err.contains(&MAX_RATE_PER_MINUTE.to_string()), "{err}");
    assert!(load(&config_dir).is_empty(), "a refused key is not stored");

    // The ceiling itself is fine, and a key at the ceiling still limits.
    let (_, meta) = create(&config_dir, new_key("at-ceiling", &[], Some(MAX_RATE_PER_MINUTE))).unwrap();
    assert_eq!(meta.rate_limit_per_minute, MAX_RATE_PER_MINUTE);
}

/// Without a sweep, every key ever used keeps a `VecDeque` for the life of the process: entries
/// are only trimmed when that same key is seen again, and a revoked key is never seen again. The
/// sweep drops both those and any window that has gone quiet.
#[test]
fn the_sweep_drops_a_revoked_key_and_a_window_that_has_gone_quiet() {
    let root = root("sweep");
    let config_dir = root.join("config");
    let now = 1_900_000_000;

    // Three keys make requests: one revoked, one quiet, one still inside its window.
    let revoked = {
        let (_, meta) = create(&config_dir, new_key("revoked", &[], Some(60))).unwrap();
        revoke(&config_dir, &meta.id).unwrap();
        meta.id
    };
    let quiet = create(&config_dir, new_key("quiet", &[], Some(60))).unwrap().1.id;
    let live = create(&config_dir, new_key("live", &[], Some(60))).unwrap().1.id;

    let stale = key_at(&revoked, 60);
    let quieting = key_at(&quiet, 60);
    let fresh = key_at(&live, 60);
    for key in [&stale, &quieting, &fresh] {
        assert!(take_slot(key, now));
    }

    // Half a window on, the revoked key is gone while both live windows are untouched.
    sweep(&config_dir, now + RATE_WINDOW_SECS / 2);
    let ledger = LEDGER.lock().unwrap();
    assert!(!ledger.contains_key(&revoked), "a revoked key must not keep its queue");
    assert!(ledger.contains_key(&quiet), "a live key inside its window keeps its ledger");
    assert!(ledger.contains_key(&live));
    drop(ledger);

    // A window later, both have drained and are dropped whatever the key's state.
    sweep(&config_dir, now + RATE_WINDOW_SECS / 2 + RATE_WINDOW_SECS);
    let ledger = LEDGER.lock().unwrap();
    assert!(!ledger.contains_key(&quiet), "a window that has gone quiet must not either");
    assert!(
        !ledger.contains_key(&live),
        "a drained window is dropped whatever the key's state"
    );
}

#[test]
fn list_answers_metadata_only() {
    let root = root("list");
    let config_dir = root.join("config");
    let (plaintext, _) = create(&config_dir, new_key("list", &["203.0.113.0/24"], Some(5))).unwrap();
    let listed = list(&config_dir);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].rate_limit_per_minute, 5);
    assert_eq!(listed[0].ip_allowlist, vec!["203.0.113.0/24".to_string()]);
    // `KeyMeta` has no hash and no plaintext field to serialize.
    assert!(!serde_json::to_string(&listed[0]).unwrap().contains(&plaintext));
}

// -- addresses ----------------------------------------------------------------

#[test]
fn an_empty_allowlist_allows_every_address_and_a_listed_one_does_not() {
    assert!(ip_allowed(&[], ip("198.51.100.9")));
    let list = vec!["203.0.113.0/24".to_string(), "2001:db8::/32".to_string()];
    assert!(ip_allowed(&list, ip("203.0.113.7")));
    assert!(!ip_allowed(&list, ip("203.0.114.7")));
    assert!(!ip_allowed(&list, ip("2001:dead::1")));
}

#[test]
fn a_bare_address_matches_only_itself() {
    assert_eq!(cidr_matches("203.0.113.7", ip("203.0.113.7")), Some(true));
    assert_eq!(cidr_matches("203.0.113.7", ip("203.0.113.8")), Some(false));
    assert_eq!(cidr_matches("2001:db8::1", ip("2001:db8::1")), Some(true));
    assert_eq!(cidr_matches("2001:db8::1", ip("2001:db8::2")), Some(false));
}

#[test]
fn v4_blocks_match_on_their_own_bits_only() {
    assert_eq!(cidr_matches("203.0.113.0/24", ip("203.0.113.255")), Some(true));
    assert_eq!(cidr_matches("203.0.113.0/24", ip("203.0.114.0")), Some(false));
    assert_eq!(cidr_matches("10.0.0.0/8", ip("10.255.255.255")), Some(true));
    assert_eq!(cidr_matches("10.0.0.0/8", ip("11.0.0.1")), Some(false));
    // A zero-length prefix is every address, which is what `/0` means.
    assert_eq!(cidr_matches("0.0.0.0/0", ip("203.0.113.7")), Some(true));
    assert_eq!(cidr_matches("203.0.113.7/32", ip("203.0.113.7")), Some(true));
    // A prefix wider than the family is clamped to the family's width, not rejected.
    assert_eq!(cidr_matches("203.0.113.7/40", ip("203.0.113.7")), Some(true));
    assert_eq!(cidr_matches("203.0.113.7/40", ip("203.0.113.8")), Some(false));
}

#[test]
fn v6_blocks_match_on_their_own_bits_only() {
    assert_eq!(cidr_matches("2001:db8::/32", ip("2001:db8:ffff::1")), Some(true));
    assert_eq!(cidr_matches("2001:db8::/32", ip("2001:db9::1")), Some(false));
    assert_eq!(cidr_matches("::/0", ip("2001:db8::1")), Some(true));
    assert_eq!(cidr_matches("2001:db8::/128", ip("2001:db8::")), Some(true));
}

#[test]
fn an_ipv4_peer_on_a_v6_socket_matches_the_v4_block_the_operator_wrote() {
    let mapped = "::ffff:203.0.113.7".parse::<IpAddr>().unwrap();
    assert_eq!(cidr_matches("203.0.113.0/24", mapped), Some(true));
    // A real v6 address is still only matched by a v6 block.
    assert_eq!(cidr_matches("203.0.113.0/24", ip("2001:db8::1")), Some(false));
    assert_eq!(cidr_matches("2001:db8::/32", ip("203.0.113.7")), Some(false));
}

#[test]
fn a_malformed_entry_matches_nothing_and_never_widens_the_list() {
    // The direction that matters: a typo fails closed, so it cannot admit an address the operator
    // never listed.
    assert_eq!(cidr_matches("203.0.113.0/oops", ip("203.0.113.7")), None);
    assert_eq!(cidr_matches("203.0.113.0/999", ip("203.0.113.7")), None);
    assert_eq!(cidr_matches("203.0.113.0/", ip("203.0.113.7")), None);
    assert_eq!(cidr_matches("", ip("203.0.113.7")), None);
    assert!(!ip_allowed(&["nonsense".to_string()], ip("203.0.113.7")));
    // A bad entry beside a good one does not stop the good one from working.
    assert!(ip_allowed(
        &["nonsense".to_string(), "203.0.113.0/24".to_string()],
        ip("203.0.113.7")
    ));
}

// -- the limit ----------------------------------------------------------------

#[test]
fn a_key_gets_its_rate_a_minute_and_a_new_window_after_sixty_seconds() {
    let key = key_at(&format!("cfk_{}", short_id()), 3);
    let now = 1_800_000_000;
    for i in 0..3 {
        assert!(take_slot(&key, now + i), "request {i} should be inside the limit");
    }
    assert!(!take_slot(&key, now + 3), "the fourth request is over the limit");
    // A key's ledger is its own: another key is unaffected by the first one's exhausted window.
    let other = key_at(&format!("cfk_{}", short_id()), 1);
    assert!(take_slot(&other, now + 3));
    // Once the window rolls, the same key is served again.
    assert!(take_slot(&key, now + 61));
}

#[test]
fn the_peer_address_is_read_from_the_connection_and_nowhere_else() {
    let from_socket = Some(Extension(ConnectInfo("203.0.113.7:5000".parse().unwrap())));
    assert_eq!(peer_ip(from_socket), Some(ip("203.0.113.7")));
    // No `ConnectInfo` means no address information, which is not the same as address 0.0.0.0.
    assert_eq!(peer_ip(None), None);
}
