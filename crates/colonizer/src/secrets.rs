//! Opt-in at-rest encryption for the mothership's saved credentials (issue #154).
//!
//! Set `COLONIZER_MASTER_KEY` to 64 hex chars (32 bytes, e.g. from `openssl rand -hex 32`) and the
//! saved secrets — the GitHub token, the Claude token, model provider keys, the mem0 key and the
//! notify signing secret — are stored as ChaCha20-Poly1305 envelopes in a `.enc` sidecar next to
//! each plaintext path, instead of as plaintext. Unset it and everything behaves as before.
//!
//! Deliberately no OS keyring and no KMS: both would add platform daemons or network dependencies
//! to a harness that runs headless on servers, and both would still leave the key on this machine.
//! The passphrase lives in the process environment, so this is one locked drawer, not two.
//!
//! What it does: a config-dir-only backup, sync or tar stops yielding working credentials when the
//! passphrase lives elsewhere (another file, a password manager, deployment env). What it does not:
//! no defense against code running as this user — the key is re-parsed from the environment on
//! every read, there is no daemon or cache holding it apart — and two motherships sharing one
//! `COLONIZER_CONFIG_DIR` with different keys make the second copy dead by design: its `.enc`
//! files open to `None`, which every reader treats as unset, never as ciphertext-as-key.
//!
//! Rollback safety: old binaries only ever look at the plaintext path, so after a migration they
//! see "unset" (and fall back to env or `gh` CLI login) rather than decrypting garbage. The same
//! shape holds for a new binary without the key. Explicitly out of scope: the per-colony
//! `<session>/gateway-token` and `<vm>/token` (ephemeral per-boot randoms rotated every boot, so
//! encryption buys nothing) and `host_id` (not a secret).

use crate::util::{read_trimmed, write_private};
use anyhow::Result;
use ring::aead::{self, Nonce, UnboundKey};
use ring::rand::SecureRandom;
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

/// The version prefix of every envelope: `cz1.<nonce_hex>.<ciphertext_hex>`.
const PREFIX: &str = "cz1.";
/// 12 random bytes per seal, the only thing that must never repeat for one key.
const NONCE_LEN: usize = 12;

/// The master key, parsed from `COLONIZER_MASTER_KEY` on every call: absent or malformed reads as
/// `None`, so callers store plaintext and old readers keep working. Parsed per call on purpose —
/// there is no daemon or cache holding the key apart from the environment.
pub fn master_key() -> Option<[u8; 32]> {
    std::env::var("COLONIZER_MASTER_KEY").ok().and_then(|text| parse_key(&text))
}

/// Parses 64 hex chars into 32 key bytes; anything else is `None`. Surrounding whitespace is
/// tolerated, everything else about the shape is strict.
fn parse_key(text: &str) -> Option<[u8; 32]> {
    let text = text.trim();
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut key = [0u8; 32];
    for (i, pair) in text.as_bytes().chunks(2).enumerate() {
        key[i] = hex_val(pair[0])? << 4 | hex_val(pair[1])?;
    }
    Some(key)
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    text.as_bytes()
        .chunks(2)
        .map(|pair| Some(hex_val(pair[0])? << 4 | hex_val(pair[1])?))
        .collect()
}

/// The `.enc` sidecar of a plaintext path: the extension is appended, never replaced, so
/// `provider-keys/deepseek` maps to `provider-keys/deepseek.enc`.
pub fn enc_path(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    path.with_file_name(format!("{name}.enc"))
}

/// Seals raw secret bytes under `key`: one fresh random nonce per call, single-line envelope out.
fn seal(key: &[u8; 32], plaintext: &[u8]) -> Result<String> {
    let fail = || anyhow::anyhow!("could not seal the secret");
    let aead = aead::LessSafeKey::new(UnboundKey::new(&aead::CHACHA20_POLY1305, key).map_err(|_| fail())?);
    let mut nonce_bytes = [0u8; NONCE_LEN];
    ring::rand::SystemRandom::new().fill(&mut nonce_bytes).map_err(|_| fail())?;
    let mut in_out = plaintext.to_vec();
    aead.seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce_bytes), aead::Aad::empty(), &mut in_out)
        .map_err(|_| fail())?;
    Ok(format!("{PREFIX}{}.{}", hex(&nonce_bytes), hex(&in_out)))
}

/// Opens an envelope under `key`: the raw bytes back, or `None` for anything that is not a
/// well-formed envelope sealed under this key — wrong key, tampered bytes, malformed text.
fn open(key: &[u8; 32], envelope: &str) -> Option<Vec<u8>> {
    let rest = envelope.trim().strip_prefix(PREFIX)?;
    let (nonce_hex, ct_hex) = rest.split_once('.')?;
    if ct_hex.contains('.') {
        return None;
    }
    let nonce = unhex(nonce_hex)?;
    if nonce.len() != NONCE_LEN {
        return None;
    }
    let mut in_out = unhex(ct_hex)?;
    let aead = aead::LessSafeKey::new(UnboundKey::new(&aead::CHACHA20_POLY1305, key).ok()?);
    let plain = aead
        .open_in_place(Nonce::try_assume_unique_for_key(&nonce).ok()?, aead::Aad::empty(), &mut in_out)
        .ok()?;
    Some(plain.to_vec())
}

/// Reads a saved secret: the `.enc` sidecar wins when present (a wrong or missing key, or a
/// corrupt envelope, reads as unset — never ciphertext-as-key), else the plaintext file, with a
/// lazy migration that seals the plaintext away on first read when the key is set. Migration
/// write failures are ignored: the value is still returned, and the next read tries again.
pub fn read_secret(path: &Path) -> Option<String> {
    read_secret_with(path, master_key())
}

fn read_secret_with(path: &Path, key: Option<[u8; 32]>) -> Option<String> {
    let enc = enc_path(path);
    if enc.exists() {
        let key = key?;
        let envelope = std::fs::read_to_string(&enc).ok()?;
        let plain = open(&key, &envelope)?;
        let value = String::from_utf8(plain).ok()?;
        let trimmed = value.trim().to_string();
        return (!trimmed.is_empty()).then_some(trimmed);
    }
    let value = read_trimmed(path)?;
    if let Some(key) = key
        && let Ok(envelope) = seal(&key, value.as_bytes())
        && write_enc(&enc, &envelope).is_ok()
    {
        let _ = std::fs::remove_file(path);
    }
    Some(value)
}

/// Writes `path`'s parent the way [`crate::util::write_secret`] does (created, 0700), then the
/// envelope at 0600.
fn write_enc(enc: &Path, envelope: &str) -> Result<()> {
    if let Some(dir) = enc.parent() {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    write_private(enc, envelope.as_bytes())
}

/// Saves a secret: sealed into the `.enc` sidecar (plaintext removed) when the master key is set,
/// else plaintext exactly as before. A stale sidecar left from an earlier key is removed on the
/// plaintext path, or it would shadow the new value with unreadable bytes.
pub fn write_secret_value(path: &Path, value: &str) -> Result<()> {
    write_secret_with(path, value, master_key())
}

fn write_secret_with(path: &Path, value: &str, key: Option<[u8; 32]>) -> Result<()> {
    match key {
        Some(key) => {
            let envelope = seal(&key, value.as_bytes())?;
            write_enc(&enc_path(path), &envelope)?;
            let _ = std::fs::remove_file(path);
            Ok(())
        }
        None => {
            let _ = std::fs::remove_file(enc_path(path));
            crate::util::write_secret(path, value)
        }
    }
}

/// Removes a secret in both forms. Missing files are not errors.
pub fn clear_secret(path: &Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(enc_path(path));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::short_id;

    /// A fresh directory per test, as in util.rs: there is no tempfile dependency.
    fn temp_root(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("colonizer-secrets-{label}-{}", short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const KEY_A: [u8; 32] = *b"0123456789abcdef0123456789abcdef";
    const KEY_B: [u8; 32] = *b"fedcba9876543210fedcba9876543210";

    #[test]
    fn sealed_secrets_open_back_to_the_exact_bytes_they_were_sealed_from() {
        for secret in ["ghp_token123", "sk-ant-oat-…unicode…", "x", &"long-".repeat(200)] {
            let envelope = seal(&KEY_A, secret.as_bytes()).unwrap();
            assert!(envelope.starts_with(PREFIX), "{envelope}");
            assert_eq!(open(&KEY_A, &envelope).unwrap(), secret.as_bytes());
        }
    }

    #[test]
    fn decrypted_values_are_trimmed_and_empty_values_read_as_unset_like_plaintext() {
        let dir = temp_root("trim");
        let path = dir.join("token");
        let envelope = seal(&KEY_A, b"  padded-value \n").unwrap();
        std::fs::write(enc_path(&path), envelope).unwrap();
        assert_eq!(read_secret_with(&path, Some(KEY_A)).as_deref(), Some("padded-value"));
        let envelope = seal(&KEY_A, b"   \n ").unwrap();
        std::fs::write(enc_path(&path), envelope).unwrap();
        assert_eq!(read_secret_with(&path, Some(KEY_A)), None, "whitespace-only decrypts to unset, as read_trimmed does");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn opening_with_a_different_master_key_reads_as_unset_not_garbage() {
        let envelope = seal(&KEY_A, b"the-real-secret").unwrap();
        assert_eq!(open(&KEY_B, &envelope), None);
    }

    #[test]
    fn a_tampered_ciphertext_envelope_reads_as_unset() {
        let envelope = seal(&KEY_A, b"the-real-secret").unwrap();
        let mut tampered = envelope.clone();
        let last = tampered.pop().unwrap();
        tampered.push(if last == '0' { '1' } else { '0' });
        assert_ne!(tampered, envelope);
        assert_eq!(open(&KEY_A, &tampered), None, "flipping one hex digit breaks the Poly1305 tag");
        let truncated = &envelope[..envelope.len() - 4];
        assert_eq!(open(&KEY_A, truncated), None);
    }

    #[test]
    fn malformed_envelopes_read_as_unset_rather_than_erroring() {
        for bad in [
            "",
            "   \n",
            "cz1.only-two-parts",
            "cz1.a.b.c",
            "cz0.00112233445566778899aabb.aabbcc",
            "plaintext-that-was-never-sealed",
            "cz1.zzzzzzzzzzzzzzzzzzzzzzzz.aabbcc",
            "cz1.001122.aabbcc",
            "cz1.00112233445566778899aabb.",
        ] {
            assert_eq!(open(&KEY_A, bad), None, "{bad:?} must not open");
        }
    }

    #[test]
    fn the_master_key_parses_64_hex_chars_and_rejects_everything_else() {
        assert_eq!(parse_key(&"ab".repeat(32)), Some([0xab; 32]));
        assert_eq!(parse_key(&"AB".repeat(32)), Some([0xab; 32]), "uppercase hex parses too");
        assert_eq!(parse_key(&format!("  {} \n", "ab".repeat(32))), Some([0xab; 32]));
        for bad in ["", "abc", &"0".repeat(63), &"0".repeat(65), &"z".repeat(64), &"0".repeat(62).chars().chain("zz".chars()).collect::<String>()] {
            assert_eq!(parse_key(bad), None, "{bad:?} is not a key");
        }
    }

    #[test]
    fn encrypted_sidecars_append_dot_enc_without_replacing_the_file_extension() {
        assert_eq!(enc_path(Path::new("/cfg/github-token")).to_string_lossy(), "/cfg/github-token.enc");
        assert_eq!(
            enc_path(Path::new("/cfg/provider-keys/deepseek")).to_string_lossy(),
            "/cfg/provider-keys/deepseek.enc"
        );
        assert_eq!(enc_path(Path::new("/cfg/archive.tar")).to_string_lossy(), "/cfg/archive.tar.enc");
    }

    #[test]
    fn written_secrets_read_back_through_the_testable_core_with_an_explicit_key() {
        let dir = temp_root("write-read");
        let path = dir.join("nested").join("token");
        write_secret_with(&path, "s3cr3t-value", Some(KEY_A)).unwrap();
        assert_eq!(read_secret_with(&path, Some(KEY_A)).as_deref(), Some("s3cr3t-value"));
        assert!(!path.exists(), "no plaintext remains beside the sidecar");
        assert!(enc_path(&path).exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn without_a_master_key_saved_secrets_still_read_from_plaintext() {
        let dir = temp_root("fallback");
        let path = dir.join("token");
        write_secret_with(&path, "plain-value", None).unwrap();
        assert_eq!(read_secret_with(&path, None).as_deref(), Some("plain-value"));
        assert!(path.exists(), "nothing was migrated without a key");
        assert!(!enc_path(&path).exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_encrypted_secret_without_its_master_key_reads_as_unset() {
        let dir = temp_root("no-key");
        let path = dir.join("token");
        write_secret_with(&path, "s3cr3t-value", Some(KEY_A)).unwrap();
        assert_eq!(read_secret_with(&path, None), None, "missing key is unset, never ciphertext-as-key");
        assert_eq!(read_secret_with(&path, Some(KEY_B)), None, "and so is the wrong key");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_encrypted_secret_wins_over_a_stale_plaintext_beside_it() {
        let dir = temp_root("prefer-enc");
        let path = dir.join("token");
        write_secret_with(&path, "new-value", Some(KEY_A)).unwrap();
        std::fs::write(&path, "stale-plaintext").unwrap();
        assert_eq!(read_secret_with(&path, Some(KEY_A)).as_deref(), Some("new-value"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_plaintext_secret_migrates_to_encrypted_storage_on_first_read_with_a_key() {
        let dir = temp_root("migrate");
        let path = dir.join("token");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "legacy-value\n").unwrap();
        assert_eq!(read_secret_with(&path, Some(KEY_A)).as_deref(), Some("legacy-value"));
        assert!(!path.exists(), "the plaintext is gone after migration");
        assert_eq!(read_secret_with(&path, Some(KEY_A)).as_deref(), Some("legacy-value"), "the sidecar answers alone now");
        assert_eq!(read_secret_with(&path, None), None, "and without the key the migrated secret is unset");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn clearing_a_secret_removes_both_the_plaintext_and_the_encrypted_sidecar() {
        let dir = temp_root("clear");
        let path = dir.join("token");
        write_secret_with(&path, "s3cr3t-value", Some(KEY_A)).unwrap();
        std::fs::write(&path, "stale-plaintext").unwrap();
        clear_secret(&path);
        assert!(!path.exists() && !enc_path(&path).exists());
        clear_secret(&path);
        let _ = std::fs::remove_dir_all(dir);
    }
}
