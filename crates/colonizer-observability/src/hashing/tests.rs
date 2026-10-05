//! The hash key's tests: created once, private, reused, refused when malformed, raced safely.

use super::*;

/// A fresh directory under the system temp dir, removed by the caller.
pub(crate) fn temp_dir(tag: &str) -> PathBuf {
    let mut nonce = [0u8; 8];
    SystemRandom::new().fill(&mut nonce).unwrap();
    let dir = std::env::temp_dir().join(format!("colonizer-observability-{tag}-{}", hex(&nonce)));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn the_key_is_created_once_private_and_reused() {
    let dir = temp_dir("hash-key");
    let first = HashKey::load_or_create(&dir).unwrap();
    let path = key_path(&dir);
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes.len(), KEY_BYTES);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "hash.key is owner-only");
    }
    let hash = first.hash_name("acme/widgets");
    assert_eq!(hash.len(), 2 * HASH_BYTES);
    assert!(
        hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "{hash}"
    );

    // A restart: the same file, the same key, the same hash, and the file is not rewritten.
    let again = HashKey::load_or_create(&dir).unwrap();
    assert_eq!(again.hash_name("acme/widgets"), hash);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_ne!(again.hash_name("acme/gadgets"), hash);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
#[cfg(unix)]
fn a_copied_key_left_readable_by_others_is_narrowed_to_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp_dir("hash-key-mode");
    let path = key_path(&dir);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    // A fleet copy, as `cp` under a 022 umask leaves it.
    std::fs::write(&path, [9u8; KEY_BYTES]).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let key = HashKey::load_or_create(&dir).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(
        key.hash_name("acme/widgets"),
        HashKey::from_bytes(&[9; KEY_BYTES]).hash_name("acme/widgets")
    );
    assert_eq!(std::fs::read(&path).unwrap(), [9u8; KEY_BYTES], "the key itself is kept");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_different_key_hashes_differently_and_a_copied_key_the_same() {
    let a = HashKey::from_bytes(&[1; KEY_BYTES]);
    let b = HashKey::from_bytes(&[2; KEY_BYTES]);
    assert_ne!(a.hash_name("acme/widgets"), b.hash_name("acme/widgets"));
    assert_eq!(
        a.hash_name("acme/widgets"),
        HashKey::from_bytes(&[1; KEY_BYTES]).hash_name("acme/widgets")
    );
    assert_eq!(format!("{a:?}"), "HashKey(..)");
}

#[test]
fn a_key_of_the_wrong_length_is_refused_not_replaced() {
    let dir = temp_dir("hash-key-bad");
    let path = key_path(&dir);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    for bad in [&b"short"[..], &[7u8; KEY_BYTES + 1][..], &[][..]] {
        std::fs::write(&path, bad).unwrap();
        let err = HashKey::load_or_create(&dir).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(std::fs::read(&path).unwrap(), bad, "left as it was");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn concurrent_creators_agree_on_one_key() {
    let dir = temp_dir("hash-key-race");
    let hashes: Vec<String> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..8)
            .map(|_| s.spawn(|| HashKey::load_or_create(&dir).unwrap().hash_name("acme/widgets")))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert!(hashes.windows(2).all(|w| w[0] == w[1]), "{hashes:?}");
    // The key alone: no creator's staging file is left behind.
    let names: Vec<_> = std::fs::read_dir(key_path(&dir).parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, ["hash.key"]);
    std::fs::remove_dir_all(&dir).unwrap();
}
