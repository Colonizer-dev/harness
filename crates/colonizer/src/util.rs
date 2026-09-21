//! Small process, file and string helpers shared across the harness.

use anyhow::{Context, Result, bail};
use std::{
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{fs::OpenOptions, io::AsyncWriteExt, process::Command};

pub fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

/// Renders a command for error messages without leaking environment values (tokens live in env).
pub fn describe(cmd: &Command) -> String {
    let std = cmd.as_std();
    let program = std.get_program().to_string_lossy().into_owned();
    let mut parts = vec![program.clone()];
    let mut args = std.get_args();
    while let Some(arg) = args.next() {
        if program == "git" && arg.to_str() == Some("-c") {
            args.next();
            continue;
        }
        parts.push(arg.to_string_lossy().into_owned());
    }
    parts.join(" ")
}

pub async fn exec(cmd: &mut Command) -> Result<String> {
    let desc = describe(cmd);
    let out = cmd
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .with_context(|| format!("failed to start `{desc}`"))?;
    if !out.status.success() {
        bail!(
            "`{desc}` failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Runs a command with a deadline. Timing out drops the future, which is what fires `exec`'s
/// `kill_on_drop`, so a wedged binary is killed rather than left running.
pub async fn exec_within(limit: Duration, cmd: &mut Command) -> Result<String> {
    let desc = describe(cmd);
    tokio::time::timeout(limit, exec(cmd))
        .await
        .with_context(|| format!("`{desc}` timed out after {limit:?}"))?
}

/// Runs a command for its exit status only.
pub async fn exec_status(cmd: &mut Command) -> Result<bool> {
    let desc = describe(cmd);
    let status = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .with_context(|| format!("failed to start `{desc}`"))?;
    Ok(status.success())
}

pub fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Writes a secret with 0600 permissions, tightening the parent directory to 0700.
///
/// When `COLONIZER_MASTER_KEY` is set, the value is envelope-encrypted (ChaCha20-Poly1305 via
/// ring, key = SHA256 of the env value) and stored as `<path>.enc`, and any stale plaintext at
/// `path` is removed. Without the env var the value is stored as plaintext at `path` and any
/// stale `.enc` is removed, so a downgrade never leaves a shadowing ciphertext behind.
///
/// What this protects, honestly: a copied, synced or backed-up config dir no longer leaks the
/// secrets. What it does not: a process running as the user can read the env var and the files,
/// so this is not a defense against local malware or the user themselves. A second machine with
/// a copy of the config dir needs the same `COLONIZER_MASTER_KEY` or its copy is dead by design.
/// Rotation means setting a new key value and re-saving each secret; if the old key is lost,
/// delete the `.enc` files and re-enter the secrets.
pub fn write_secret(path: &Path, value: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    if let Some(key) = master_key() {
        let envelope = seal(value, &key)?;
        write_private(&enc_path(path), envelope.as_bytes())?;
        let _ = std::fs::remove_file(path);
        return Ok(());
    }
    write_private(path, value.as_bytes())?;
    let _ = std::fs::remove_file(enc_path(path));
    Ok(())
}

/// Removes both the plaintext secret at `path` and its encrypted sibling `<path>.enc`,
/// ignoring errors. Every secret delete must go through here: removing only the plaintext
/// would leave the `.enc` behind shadowing (and resurrecting) the secret.
pub fn delete_secret(path: &Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(enc_path(path));
}

/// Reads a secret written by [`write_secret`]. If `<path>.enc` holds any non-empty content the
/// master key is required and the envelope must open, otherwise `None` is returned — fail
/// closed, never falling back to a stale plaintext and never returning ciphertext as if it
/// were the key (which would surface as silent 401s). With no `.enc` file the plaintext at
/// `path` is read, so pre-encryption secrets keep working.
pub fn read_secret(path: &Path) -> Option<String> {
    let enc = enc_path(path);
    match std::fs::read(&enc) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return None,
        Ok(bytes) => {
            let envelope = String::from_utf8(bytes).ok()?;
            if envelope.trim().is_empty() {
                // Empty/whitespace-only .enc counts as absent: fall back to plaintext below.
            } else {
                let key = master_key()?;
                return open_envelope(&envelope, &key);
            }
        }
    }
    read_trimmed(path)
}

/// The encrypted sibling of a secret path: `<name>.enc` next to `<name>`. Versioned by
/// filename, so an older binary that does not know about encryption sees a missing key
/// (fail closed) rather than mistaking ciphertext for the key.
pub fn enc_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".enc");
    PathBuf::from(name)
}

/// The envelope-encryption key: SHA256 of the raw `COLONIZER_MASTER_KEY` bytes. Supply a long
/// random value; hashing preserves its entropy and keeps key handling to these few lines.
/// A low-entropy `COLONIZER_MASTER_KEY` value is offline-brute-forceable from a copied `.enc`
/// file, so the value must be a long random string (e.g. 32+ random bytes); the single SHA256
/// derivation is a binding, not a stretching KDF.
/// `None` when the env var is unset or blank, meaning plaintext storage.
fn master_key() -> Option<[u8; 32]> {
    env_nonempty("COLONIZER_MASTER_KEY").map(|v| {
        let digest = ring::digest::digest(&ring::digest::SHA256, v.as_bytes());
        let mut key = [0u8; 32];
        key.copy_from_slice(digest.as_ref());
        key
    })
}

const B64_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 (alphabet `A–Z a–z 0–9 + /` with `=` padding), implemented by hand so no
/// new crate is needed.
fn b64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    let mut i = 0;
    while i < bytes.len() {
        let b0 = bytes[i] as u32;
        let b1 = if i + 1 < bytes.len() { bytes[i + 1] as u32 } else { 0 };
        let b2 = if i + 2 < bytes.len() { bytes[i + 2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64_ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(B64_ALPHABET[((n >> 12) & 63) as usize] as char);
        if i + 1 < bytes.len() {
            out.push(B64_ALPHABET[((n >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }
        if i + 2 < bytes.len() {
            out.push(B64_ALPHABET[(n & 63) as usize] as char);
        } else {
            out.push('=');
        }
        i += 3;
    }
    out
}

/// Inverse of [`b64_encode`]. Outer whitespace is trimmed first; anything else outside the
/// standard alphabet (including inner whitespace) is rejected with `None`.
fn b64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a') as u32 + 26),
            b'0'..=b'9' => Some((c - b'0') as u32 + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let s = s.trim();
    if s.is_empty() || s.len() % 4 != 0 || !s.is_ascii() {
        return None;
    }
    let bytes = s.as_bytes();
    let pad = bytes.iter().rev().take_while(|&&c| c == b'=').count();
    if pad > 2 || bytes[..bytes.len() - pad].contains(&b'=') {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks_exact(4) {
        let mut n = 0u32;
        for (j, &c) in chunk.iter().enumerate() {
            let v = if c == b'=' { 0 } else { val(c)? };
            n |= v << (18 - 6 * j);
        }
        out.push(((n >> 16) & 0xff) as u8);
        if chunk[2] != b'=' {
            out.push(((n >> 8) & 0xff) as u8);
        }
        if chunk[3] != b'=' {
            out.push((n & 0xff) as u8);
        }
    }
    Some(out)
}

/// Seals `plaintext` under `key` with ChaCha20-Poly1305 (fresh `SystemRandom` nonce per seal,
/// empty additional data) and returns the envelope line `v1:<base64-nonce12>:<base64-ct+tag>`.
fn seal(plaintext: &str, key: &[u8; 32]) -> Result<String> {
    use ring::{aead, rand::SecureRandom};
    let mut nonce_bytes = [0u8; 12];
    ring::rand::SystemRandom::new()
        .fill(&mut nonce_bytes)
        .map_err(|_| anyhow::anyhow!("could not generate an encryption nonce"))?;
    let unbound =
        aead::UnboundKey::new(&aead::CHACHA20_POLY1305, key).map_err(|_| anyhow::anyhow!("bad master key"))?;
    let less = aead::LessSafeKey::new(unbound);
    let nonce = aead::Nonce::assume_unique_for_key(nonce_bytes);
    let mut data = plaintext.as_bytes().to_vec();
    less.seal_in_place_append_tag(nonce, aead::Aad::empty(), &mut data)
        .map_err(|_| anyhow::anyhow!("could not encrypt the secret"))?;
    Ok(format!("v1:{}:{}", b64_encode(&nonce_bytes), b64_encode(&data)))
}

/// Opens an envelope line made by [`seal`]. Returns `None` on any malformed input, failed
/// authentication, non-UTF-8 plaintext or an empty/whitespace-only secret.
fn open_envelope(envelope: &str, key: &[u8; 32]) -> Option<String> {
    let rest = envelope.trim().strip_prefix("v1:")?;
    let (nonce_b64, ct_b64) = rest.split_once(':')?;
    if ct_b64.contains(':') {
        return None;
    }
    let nonce_bytes = b64_decode(nonce_b64)?;
    if nonce_bytes.len() != 12 {
        return None;
    }
    let mut data = b64_decode(ct_b64)?;
    if data.is_empty() {
        return None;
    }
    let unbound = ring::aead::UnboundKey::new(&ring::aead::CHACHA20_POLY1305, key).ok()?;
    let less = ring::aead::LessSafeKey::new(unbound);
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&nonce_bytes);
    let plain = less
        .open_in_place(
            ring::aead::Nonce::assume_unique_for_key(nonce),
            ring::aead::Aad::empty(),
            &mut data,
        )
        .ok()?;
    let text = std::str::from_utf8(plain).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

/// Writes a file with 0600 permissions without touching the parent directory's mode.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    f.write_all(bytes)?;
    Ok(())
}

/// Writes `data` to `path` atomically: a temp file next to the target, flushed with `sync_all`,
/// then renamed into place. A reader never sees a partial file, and a failed write leaves the
/// previous contents intact and reports the failure. Not crash-durable: there is no fsync of the
/// parent directory after the rename, so a power loss can revert the rename — the right trade for
/// these files, which are rewritten on their next change.
pub async fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    // The temp is `<whole file name>.tmp` next to the target. `with_extension` would *replace* the
    // extension, so `foo.jsonl` and `foo.json` would share one temp path; spelled this way,
    // `sessions.json` still writes its historical `sessions.json.tmp`.
    let tmp = path
        .file_name()
        .map(|name| path.with_file_name(format!("{}.tmp", name.to_string_lossy())))
        .with_context(|| {
            format!(
                "could not write {}: it names no file, so there is no temp path to write",
                path.display()
            )
        })?;
    let write = async {
        faults::check(path, faults::Op::Write)?;
        let mut f = OpenOptions::new().create(true).truncate(true).write(true).open(&tmp).await?;
        f.write_all(data).await?;
        f.sync_all().await?;
        std::io::Result::Ok(())
    }
    .await;
    if let Err(e) = write {
        // The temp file is ours alone, so removing it is best effort: the error that matters is the write.
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e).with_context(|| format!("could not write {} (temp file {})", path.display(), tmp.display()));
    }
    let rename = async {
        faults::check(path, faults::Op::Rename)?;
        tokio::fs::rename(&tmp, path).await?;
        std::io::Result::Ok(())
    }
    .await;
    if let Err(e) = rename {
        // Same best-effort cleanup; the target still holds the previous contents.
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e).with_context(|| format!("could not put {} in place (temp file {})", path.display(), tmp.display()));
    }
    Ok(())
}

/// Appends one line (a trailing newline is added) to `path`, creating it if needed. The append is
/// flushed before returning, so a confirmed line is visible to readers — it has left the process's
/// write buffers — but it is not fsynced, so a power loss can still lose it.
pub async fn append_line(path: &Path, line: &str) -> Result<()> {
    let write = async {
        faults::check(path, faults::Op::Append)?;
        let mut f = OpenOptions::new().create(true).append(true).open(path).await?;
        f.write_all(format!("{line}\n").as_bytes()).await?;
        f.flush().await?;
        std::io::Result::Ok(())
    }
    .await;
    write.with_context(|| format!("could not append to {}", path.display()))
}

/// A test-only seam for injecting filesystem faults into the two helpers above (and the startup
/// move-aside and salvage copy in main.rs), so the error paths around persistence can be exercised
/// deterministically.
/// Real-filesystem tricks are unreliable here: the container runs as root, so chmod-based permission
/// denial does not fail. Production builds compile this away — `check` becomes an inlined no-op.
pub(crate) mod faults {
    /// The step a fault applies to: the temp write or the append, or the rename into place.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum Op {
        Write,
        Rename,
        Append,
    }

    /// The first injected fault whose step matches and whose path fragment occurs in `path`.
    #[cfg(not(test))]
    #[inline]
    pub fn check(path: &std::path::Path, op: Op) -> std::io::Result<()> {
        let _ = (path, op);
        Ok(())
    }

    #[cfg(test)]
    pub fn check(path: &std::path::Path, op: Op) -> std::io::Result<()> {
        let text = path.display().to_string();
        INJECTED.with_borrow(|faults| {
            faults
                .iter()
                .find(|(contains, wants, _)| *wants == op && text.contains(contains.as_str()))
                .map(|(.., make)| make())
                .map_or(Ok(()), Err)
        })
    }

    /// Injects a fault for every `check` on a path containing `path_contains` with step `op`, until
    /// the returned guard is dropped. The error is made by `make`, so tests can inject real I/O
    /// errors: `|| std::io::Error::from_raw_os_error(5)` for EIO, `ErrorKind::StorageFull.into()`
    /// for ENOSPC.
    #[cfg(test)]
    pub fn inject(path_contains: &str, op: Op, make: fn() -> std::io::Error) -> FaultGuard {
        INJECTED.with_borrow_mut(|faults| faults.push((path_contains.to_string(), op, make)));
        FaultGuard {
            contains: path_contains.to_string(),
            op,
        }
    }

    /// Clears the fault injected by `inject` when dropped.
    #[cfg(test)]
    pub struct FaultGuard {
        contains: String,
        op: Op,
    }

    #[cfg(test)]
    impl std::ops::Drop for FaultGuard {
        fn drop(&mut self) {
            INJECTED.with_borrow_mut(|faults| {
                if let Some(at) = faults
                    .iter()
                    .position(|(contains, op, _)| *contains == self.contains && *op == self.op)
                {
                    faults.remove(at);
                }
            });
        }
    }

    // A thread-local rather than a global: `#[tokio::test]` defaults to a current-thread runtime,
    // so every fault check runs on the test's own thread and parallel tests cannot interfere.
    #[cfg(test)]
    thread_local! {
        static INJECTED: std::cell::RefCell<Vec<Fault>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    /// One injected fault: the path fragment it matches, the step it fails, and the error it makes.
    #[cfg(test)]
    type Fault = (String, Op, fn() -> std::io::Error);
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}

pub fn valid_repo(repo: &str) -> bool {
    let mut parts = repo.split('/');
    let (Some(owner), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    [owner, name].iter().all(|p| {
        !p.is_empty()
            && p.len() <= 100
            && *p != "."
            && *p != ".."
            && p.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    })
}

pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

pub fn is_elf(path: &Path) -> bool {
    use std::io::Read;
    let mut magic = [0u8; 4];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut magic)).is_ok() && magic == *b"\x7fELF"
}

/// microsandbox mount specs are `SRC:DST[:OPTS]`, so paths must not contain separators.
pub fn mount_spec(src: &Path, dst: &str, read_only: bool) -> Result<String> {
    let s = src.display().to_string();
    if s.contains(':') || s.contains(',') {
        bail!("cannot mount {s}: path contains ':' or ','");
    }
    Ok(if read_only {
        format!("{s}:{dst}:ro")
    } else {
        format!("{s}:{dst}")
    })
}

pub fn short_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}

/// 244 bits of randomness, hex encoded.
pub fn random_token() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

/// A short, non-reversible fingerprint of a credential, used as a cache key so a new credential
/// invalidates the cached lookup. The credential itself must never be recoverable from it.
pub fn fingerprint(secret: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, secret.as_bytes());
    let hex: String = digest.as_ref().iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("len={}:sha256={hex}", secret.len())
}

/// Parses a disk size the way the sandbox settings write them (`memory` is `"8G"`, `root_disk` is `"16G"`):
/// `512M`, `16G`, a bare byte count, a `K`/`M`/`G`/`T` suffix in either case. `""` and `"0"` parse to `0`,
/// which every reader treats as unlimited. `None` is malformed input: settings validation refuses it at
/// save time, so a stored value is unparseable only if the JSON was hand-edited, and readers then treat
/// the quota as unset rather than guessing at what was meant.
pub fn parse_disk_size(text: &str) -> Option<u64> {
    let text = text.trim();
    if text.is_empty() {
        return Some(0);
    }
    let (digits, unit) = match text.as_bytes()[text.len() - 1] {
        b'0'..=b'9' => (text, 1),
        b'K' | b'k' => (&text[..text.len() - 1], 1 << 10),
        b'M' | b'm' => (&text[..text.len() - 1], 1 << 20),
        b'G' | b'g' => (&text[..text.len() - 1], 1 << 30),
        b'T' | b't' => (&text[..text.len() - 1], 1 << 40),
        _ => return None,
    };
    digits.parse::<u64>().ok()?.checked_mul(unit)
}

/// The inverse of [`parse_disk_size`], for messages: the largest unit that fits, one decimal when there is
/// a fraction — `16G`, `1.5G`, `512M`, `4K`, `512B`.
pub fn format_disk_size(bytes: u64) -> String {
    let Some((unit, suffix)) = [(1u64 << 40, "T"), (1 << 30, "G"), (1 << 20, "M"), (1 << 10, "K")]
        .into_iter()
        .find(|(unit, _)| bytes >= *unit)
    else {
        return format!("{bytes}B");
    };
    let (whole, tenths) = (bytes / unit, bytes % unit * 10 / unit);
    if tenths == 0 {
        format!("{whole}{suffix}")
    } else {
        format!("{whole}.{tenths}{suffix}")
    }
}

/// Sums the size of everything under `root`, never following symlinks (a symlink counts its own few
/// bytes, never its target's), so a walk of one directory cannot wander out of it. Whatever cannot be
/// read — a missing root, an entry deleted mid-walk, a permission error — contributes nothing: half a
/// measurement still says something, and none of it fails or panics. Hard links count once per name,
/// which is the right overestimate for a quota. The walk keeps its to-do list on the heap instead of
/// the call stack: the tree is the colony's own worktree, which the agent inside fully controls, and
/// a runaway build can nest directories far deeper than a thread's stack cares about.
pub fn dir_size(root: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(root) else { return 0 };
    if !meta.is_dir() {
        return meta.len();
    }
    let mut total = 0;
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for entry in entries.flatten() {
            // DirEntry::metadata, like `symlink_metadata`, never follows the entry's own symlink.
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                pending.push(entry.path());
            } else {
                total += meta.len();
            }
        }
    }
    total
}

/// A single path segment with no separators, no traversal and no leading dot.
///
/// Used where a setting names something the harness will resolve under a
/// directory it owns: a name that is allowed to contain `/` or `..` is a way to
/// reach the rest of the host's filesystem.
pub fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
        && !name.contains(':')
        && !name.contains(',')
        && !name.contains('\0')
}

#[cfg(test)]
mod tests {
    #[test]
    fn plain_names_accept_only_a_single_segment() {
        for good in ["ecc", "house-style", "team_rules", "v2.2.1"] {
            assert!(is_plain_name(good), "{good} should be accepted");
        }
    }

    #[test]
    fn plain_names_reject_anything_that_escapes_the_directory() {
        // Each of these is a way for a settings string to name something the
        // harness never intended to mount.
        for bad in [
            "",
            "..",
            "../../etc",
            "a/b",
            "a\\b",
            ".hidden",
            "has:colon",
            "has,comma",
            "/absolute",
        ] {
            assert!(!is_plain_name(bad), "{bad:?} should be rejected");
        }
    }

    use super::*;

    #[test]
    fn repo_names_are_validated() {
        assert!(valid_repo("owner/repo.name-1"));
        assert!(valid_repo("owner/.github"));
        assert!(!valid_repo("../../etc"));
        assert!(!valid_repo("owner/.."));
        assert!(!valid_repo("owner/repo/extra"));
        assert!(!valid_repo("owner"));
    }

    #[test]
    fn mount_specs_reject_separators() {
        assert_eq!(mount_spec(Path::new("/a/b"), "/c", true).unwrap(), "/a/b:/c:ro");
        assert!(mount_spec(Path::new("/a:b"), "/c", false).is_err());
    }

    #[test]
    fn is_elf_judges_a_file_by_its_magic_bytes() {
        let dir = temp_root("is-elf");
        let elf = dir.join("agentd");
        std::fs::write(&elf, b"\x7fELF and a little padding").unwrap();
        assert!(is_elf(&elf), "the 4 magic bytes are the whole test");

        // What a Mac build drops into dist/bin/, which a Linux colony cannot exec.
        let macho = dir.join("agentd.macho");
        std::fs::write(&macho, b"\xcf\xfa\xed\xfe and padding").unwrap();
        assert!(!is_elf(&macho), "a Mach-O binary is not an ELF");

        let text = dir.join("notes.txt");
        std::fs::write(&text, "plain words").unwrap();
        assert!(!is_elf(&text));

        let stub = dir.join("stub");
        std::fs::write(&stub, b"\x7f").unwrap();
        assert!(!is_elf(&stub), "shorter than the magic cannot match it");

        assert!(!is_elf(&dir.join("nowhere")), "a path that names no file is not an ELF");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn disk_sizes_parse_with_their_suffixes_and_empty_or_zero_mean_unlimited() {
        assert_eq!(parse_disk_size("16G"), Some(16 * 1024 * 1024 * 1024));
        assert_eq!(
            parse_disk_size("512m"),
            Some(512 * 1024 * 1024),
            "the suffix is case-insensitive"
        );
        assert_eq!(parse_disk_size("4K"), Some(4 * 1024));
        assert_eq!(parse_disk_size("2t"), Some(2u64 * 1024 * 1024 * 1024 * 1024));
        assert_eq!(parse_disk_size("1024"), Some(1024), "a bare number is bytes");
        assert_eq!(
            parse_disk_size(" 8G "),
            Some(8 * 1024 * 1024 * 1024),
            "surrounding space is trimmed"
        );
        assert_eq!(parse_disk_size("0"), Some(0), "0 means unlimited");
        assert_eq!(parse_disk_size(""), Some(0), "and so does nothing set");
    }

    #[test]
    fn malformed_disk_sizes_are_rejected_not_guessed_at() {
        for bad in ["eight", "-1", "1.5G", "16 GB", "G", "16Gi", "999999999999T"] {
            assert_eq!(parse_disk_size(bad), None, "{bad:?} is not a size");
        }
    }

    #[test]
    fn disk_sizes_are_formatted_back_the_way_they_are_written() {
        assert_eq!(format_disk_size(0), "0B");
        assert_eq!(format_disk_size(512), "512B");
        assert_eq!(
            format_disk_size(4 * 1024 + 512),
            "4.5K",
            "one decimal when there is a fraction"
        );
        assert_eq!(format_disk_size(512 * 1024 * 1024), "512M");
        assert_eq!(format_disk_size(1_610_612_736), "1.5G");
        assert_eq!(format_disk_size(16 * 1024 * 1024 * 1024), "16G");
    }

    #[test]
    fn dir_size_sums_a_tree_without_following_symlinks() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!("colonizer-util-test-{}", short_id()));
        std::fs::create_dir_all(root.join("nested/deeper")).unwrap();
        std::fs::write(root.join("nested/deeper/big"), vec![0u8; 4096]).unwrap();
        std::fs::write(root.join("nested/small"), vec![0u8; 10]).unwrap();
        // A symlink to the big file counts as the link itself, never its target; a dangling one counts too.
        symlink(root.join("nested/deeper/big"), root.join("link")).unwrap();
        symlink(root.join("nowhere"), root.join("dangling")).unwrap();
        let linked = std::fs::symlink_metadata(root.join("link")).unwrap().len();
        let dangling = std::fs::symlink_metadata(root.join("dangling")).unwrap().len();
        assert_eq!(
            dir_size(&root),
            4096 + 10 + linked + dangling,
            "the targets of symlinks stay out of the sum"
        );
        assert_eq!(
            dir_size(&root.join("nowhere")),
            0,
            "a missing root measures nothing instead of failing"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn dir_size_walks_a_very_deep_tree_without_blowing_the_stack() {
        let root = std::env::temp_dir().join(format!("colonizer-util-test-{}", short_id()));
        std::fs::create_dir_all(&root).unwrap();
        // Go as deep as the kernel allows a single path to be, which is as deep as any walk that builds
        // paths can follow: about 2,000 one-letter levels inside Linux's ~4K, about 500 inside macOS's
        // 1K. The limit is found by hitting it rather than assumed, because assuming Linux's made this
        // test fail on a Mac. A runaway build inside a colony nests this deep, and a recursive walk
        // rides it down until the thread's stack gives out and takes the mothership with it.
        let mut here = root.clone();
        let mut levels = 0;
        while levels < 4_000 {
            here.push("d");
            match std::fs::create_dir(&here) {
                Ok(()) => levels += 1,
                // ENAMETOOLONG: this is the deepest the platform goes.
                Err(e) if e.kind() == std::io::ErrorKind::InvalidFilename => {
                    here.pop();
                    break;
                }
                Err(e) => panic!("could not nest {levels} deep: {e}"),
            }
        }
        assert!(
            levels > 200,
            "only nested {levels} deep; too shallow to say anything about the walk"
        );
        // The deepest directory is at the limit itself, so a file name may no longer fit beside it: back
        // out a level at a time until one does.
        loop {
            match std::fs::write(here.join("b"), b"deep payload") {
                Ok(()) => break,
                Err(e) if e.kind() == std::io::ErrorKind::InvalidFilename => {
                    here.pop();
                    levels -= 1;
                }
                Err(e) => panic!("could not write the file at the bottom: {e}"),
            }
        }
        assert!(
            dir_size(&root) >= "deep payload".len() as u64,
            "a tree {levels} directories deep is summed right down to its bottom file"
        );
        // `remove_dir_all` walks with `openat`, so it takes the tree apart without ever naming a path
        // too long to open — which is exactly why the walk under test does not build paths either.
        let _ = std::fs::remove_dir_all(&root);
    }

    use std::path::PathBuf;

    use faults::{Op, inject};

    /// A fresh directory per test, as in memory.rs; there is no tempfile dev-dependency.
    fn temp_root(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("colonizer-util-{label}-{}", short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn enospc() -> std::io::Error {
        std::io::Error::from(std::io::ErrorKind::StorageFull)
    }

    fn eio() -> std::io::Error {
        std::io::Error::from_raw_os_error(5)
    }

    fn denied() -> std::io::Error {
        std::io::Error::from(std::io::ErrorKind::PermissionDenied)
    }

    #[tokio::test]
    async fn write_atomic_replaces_the_target_and_leaves_no_temp_behind() {
        let dir = temp_root("atomic-ok");
        let path = dir.join("sessions.json");
        write_atomic(&path, b"first").await.unwrap();
        write_atomic(&path, b"second").await.unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
        assert!(
            !path.with_extension("json.tmp").exists(),
            "sessions.json must get its old sessions.json.tmp sibling"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_failed_temp_write_is_reported_and_keeps_the_previous_contents() {
        let dir = temp_root("atomic-enospc");
        let path = dir.join("sessions.json");
        std::fs::write(&path, b"previous").unwrap();
        let _guard = inject("sessions.json", Op::Write, enospc);
        let err = write_atomic(&path, b"new").await.unwrap_err();
        assert!(err.to_string().contains("sessions.json"), "{err:#}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "previous",
            "no silent success, no lost contents"
        );
        assert!(!path.with_extension("json.tmp").exists(), "the leftover temp is cleaned up");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn an_eio_during_the_temp_write_never_replaces_the_target() {
        let dir = temp_root("atomic-eio");
        let path = dir.join("sessions.json");
        std::fs::write(&path, b"previous").unwrap();
        let _guard = inject("sessions.json", Op::Write, eio);
        let err = write_atomic(&path, b"new").await.unwrap_err();
        let cause = err.root_cause().downcast_ref::<std::io::Error>().unwrap();
        assert_eq!(cause.raw_os_error(), Some(5), "the injected EIO is the cause: {err:#}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "previous");
        assert!(!path.with_extension("json.tmp").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_failed_rename_reports_and_keeps_the_previous_contents() {
        let dir = temp_root("atomic-rename");
        let path = dir.join("sessions.json");
        std::fs::write(&path, b"previous").unwrap();
        let _guard = inject("sessions.json", Op::Rename, eio);
        let err = write_atomic(&path, b"new").await.unwrap_err();
        assert!(err.to_string().contains("in place"), "{err:#}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "previous",
            "the old file is still intact"
        );
        assert!(!path.with_extension("json.tmp").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn write_atomic_temps_are_named_for_the_whole_file_so_jsonl_and_json_do_not_collide() {
        let dir = temp_root("atomic-tmp-name");
        let path = dir.join("events.jsonl");
        let _guard = inject("events.jsonl", Op::Write, enospc);
        let err = write_atomic(&path, b"new").await.unwrap_err();
        assert!(
            err.to_string().contains("events.jsonl.tmp"),
            "the temp keeps the target's full name, not a `with_extension` .json.tmp: {err:#}"
        );
        drop(_guard);
        let err = write_atomic(Path::new("/"), b"new").await.unwrap_err();
        assert!(
            err.to_string().contains("names no file"),
            "a path with no file name errors instead of panicking: {err:#}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn append_line_creates_the_file_and_reports_denied_appends() {
        let dir = temp_root("append");
        let path = dir.join("events.jsonl");
        append_line(&path, "{\"seq\":1}").await.unwrap();
        append_line(&path, "{\"seq\":2}").await.unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"seq\":1}\n{\"seq\":2}\n");

        let _guard = inject("events.jsonl", Op::Append, denied);
        let err = append_line(&path, "{\"seq\":3}").await.unwrap_err();
        assert!(err.to_string().contains("events.jsonl"), "{err:#}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\"seq\":1}\n{\"seq\":2}\n",
            "the line did not land"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn exec_within_returns_the_output_of_a_command_that_finishes_in_time() {
        let out = exec_within(Duration::from_secs(5), Command::new("echo").arg("answered"))
            .await
            .unwrap();
        assert_eq!(out, "answered\n");
    }

    #[tokio::test]
    async fn exec_within_gives_up_promptly_and_names_the_command_when_it_outruns_the_limit() {
        let started = std::time::Instant::now();
        let err = exec_within(Duration::from_millis(200), Command::new("sleep").arg("5"))
            .await
            .unwrap_err();
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(2),
            "returned after {elapsed:?}; the future was waited out, not dropped"
        );
        let message = err.to_string();
        assert!(message.contains("sleep 5"), "the command is named: {message}");
        assert!(message.contains("timed out"), "{message}");
    }

    /// Serialises tests that mutate `COLONIZER_MASTER_KEY`: the harness runs tests in parallel
    /// in one process, so two env-mutating tests at once would read each other's key.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Runs `f` with `COLONIZER_MASTER_KEY` set to `key` (or removed when `None`), restoring
    /// whatever was there before. Callers must already hold `ENV_LOCK`.
    fn with_master_key_env(key: Option<&str>, f: impl FnOnce()) {
        let prev = std::env::var("COLONIZER_MASTER_KEY").ok();
        // SAFETY: env-mutating tests are serialised on ENV_LOCK, so no other test in this
        // process can observe the variable mid-change.
        unsafe {
            match key {
                Some(k) => std::env::set_var("COLONIZER_MASTER_KEY", k),
                None => std::env::remove_var("COLONIZER_MASTER_KEY"),
            }
        }
        f();
        unsafe {
            match prev {
                Some(v) => std::env::set_var("COLONIZER_MASTER_KEY", v),
                None => std::env::remove_var("COLONIZER_MASTER_KEY"),
            }
        }
    }

    #[test]
    fn secret_roundtrip_with_master_key() {
        // The crypto core, without touching the process env: a fixed key seals and opens.
        let key = [7u8; 32];
        let envelope = seal("sk-ant-test-secret", &key).unwrap();
        assert!(envelope.starts_with("v1:"), "the envelope names its version: {envelope}");
        assert_eq!(open_envelope(&envelope, &key).as_deref(), Some("sk-ant-test-secret"));
        // A wrong key fails authentication instead of returning garbage.
        assert_eq!(open_envelope(&envelope, &[8u8; 32]), None);
        // Tampering with any version/nonce/ciphertext part fails closed too.
        assert_eq!(open_envelope("v2:AAAA:BBBB", &key), None);
        assert_eq!(open_envelope("v1:!!!:!!!", &key), None);
        assert_eq!(open_envelope(&envelope[..envelope.len() - 4], &key), None);

        // And the file layer, with the env var set and restored inside the lock.
        let _lock = ENV_LOCK.lock().unwrap();
        with_master_key_env(Some("test-master-key-for-roundtrip-0123456789"), || {
            let dir = temp_root("secret-roundtrip");
            let path = dir.join("provider-key");
            write_secret(&path, "sk-ant-test-secret").unwrap();
            assert_eq!(read_secret(&path).as_deref(), Some("sk-ant-test-secret"));
            delete_secret(&path);
            assert_eq!(read_secret(&path), None, "delete removes both files");
            assert!(!enc_path(&path).exists());
            let _ = std::fs::remove_dir_all(dir);
        });
    }

    #[test]
    fn secret_fails_closed_without_key() {
        let _lock = ENV_LOCK.lock().unwrap();
        let dir = temp_root("secret-fail-closed");
        let path = dir.join("github-token");
        with_master_key_env(Some("test-master-key-for-fail-closed-0123456789"), || {
            write_secret(&path, "ghp_testsecret").unwrap();
            assert!(enc_path(&path).exists());
        });
        // The key is gone: the reader must return None, never the ciphertext.
        with_master_key_env(None, || {
            assert_eq!(read_secret(&path), None);
            let raw = std::fs::read_to_string(enc_path(&path)).unwrap();
            assert!(raw.starts_with("v1:"), "what is on disk is an envelope, not the secret");
            assert!(!raw.contains("ghp_testsecret"));
        });
        // A wrong key also fails closed.
        with_master_key_env(Some("a-different-key-0000000000000000000000"), || {
            assert_eq!(read_secret(&path), None);
        });
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn secret_plaintext_fallback() {
        let _lock = ENV_LOCK.lock().unwrap();
        with_master_key_env(None, || {
            let dir = temp_root("secret-plaintext");
            let path = dir.join("notify-secret");
            write_secret(&path, "webhook-secret").unwrap();
            assert_eq!(read_secret(&path).as_deref(), Some("webhook-secret"));
            assert!(
                !enc_path(&path).exists(),
                "no master key means no .enc file beside the plaintext"
            );
            // A secret saved before encryption existed reads back unchanged.
            std::fs::write(&path, "  legacy-token  \n").unwrap();
            assert_eq!(read_secret(&path).as_deref(), Some("legacy-token"));
            let _ = std::fs::remove_dir_all(dir);
        });
    }

    #[test]
    fn secret_versioned_filename() {
        let _lock = ENV_LOCK.lock().unwrap();
        with_master_key_env(Some("test-master-key-for-filename-012345678900"), || {
            let dir = temp_root("secret-filename");
            let path = dir.join("github-token");
            // A stale plaintext must not survive the move to encryption.
            std::fs::write(&path, "old-plaintext").unwrap();
            write_secret(&path, "ghp_newsecret").unwrap();
            assert!(!path.exists(), "the plaintext file is gone once the secret is encrypted");
            let enc = enc_path(&path);
            assert!(enc.exists());
            let raw = std::fs::read_to_string(&enc).unwrap();
            assert!(raw.starts_with("v1:"), "the envelope is versioned: {raw}");
            assert_eq!(read_secret(&path).as_deref(), Some("ghp_newsecret"));
            // Downgrading (key removed) writes plaintext and drops the stale .enc,
            // so it can never shadow the new value. Afterwards the outer key is
            // restored, but with no .enc left the plaintext reads back either way.
            with_master_key_env(None, || {
                write_secret(&path, "plain-again").unwrap();
            });
            assert_eq!(read_secret(&path).as_deref(), Some("plain-again"));
            assert!(!enc.exists(), "the stale .enc is removed on downgrade");
            let _ = std::fs::remove_dir_all(dir);
        });
    }

    #[test]
    fn b64_roundtrip() {
        for (plain, encoded) in [
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
            ("hello world", "aGVsbG8gd29ybGQ="),
        ] {
            assert_eq!(b64_encode(plain.as_bytes()), encoded, "encode {plain:?}");
            assert_eq!(b64_decode(encoded).unwrap(), plain.as_bytes(), "decode {encoded:?}");
        }
        // Twelve zero bytes (a nonce) round-trip, and outer whitespace is tolerated.
        let nonce = [0u8; 12];
        assert_eq!(b64_decode(&format!("  {}  ", b64_encode(&nonce))).unwrap(), nonce);
        // Malformed input is rejected, never decoded into something adjacent.
        for bad in ["", "abc", "Zg", "====", "Zg===", "Z g==", "Zg==\nZg==", "!!!=", "v1:Zg=="] {
            assert_eq!(b64_decode(bad), None, "{bad:?} must not decode");
        }
    }
}
