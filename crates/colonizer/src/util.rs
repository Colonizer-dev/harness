//! Small process, file and string helpers shared across the harness.

use anyhow::{Context, Result, bail};
use std::{
    net::IpAddr,
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
    // A call the GitHub breaker refused (issue #1074) is a shell standing in for `gh`/`git`.
    if std.get_envs().any(|(k, _)| k == crate::github_breaker::REFUSAL_ENV) {
        return "github (paused)".into();
    }
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
        let stderr = String::from_utf8_lossy(&out.stderr);
        // A GitHub call's refusal feeds the account's circuit breaker (issue #1074).
        crate::github_breaker::observe_command(cmd, &stderr);
        bail!("`{desc}` failed ({}): {}", out.status, stderr.trim());
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

/// Runs a command with a deadline and returns `(stdout, stderr)` whatever the exit status: `gh api
/// -i` prints a 304's head on stdout and then exits 1.
pub async fn exec_capture(limit: Duration, cmd: &mut Command) -> Result<(String, String)> {
    let desc = describe(cmd);
    let out = tokio::time::timeout(limit, cmd.stdin(Stdio::null()).kill_on_drop(true).output())
        .await
        .with_context(|| format!("`{desc}` timed out after {limit:?}"))?
        .with_context(|| format!("failed to start `{desc}`"))?;
    let (stdout, stderr) = (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    );
    // `gh api -i` puts the status line and the body on stdout, so both feed the breaker (issue #1074).
    if !out.status.success() {
        crate::github_breaker::observe_command(cmd, &format!("{stdout}\n{stderr}"));
    }
    Ok((stdout, stderr))
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
/// The plaintext fallback is a *local* convenience, and `COLONIZER_DEPLOYMENT` is the switch that
/// picks the contract: unset (the default) or `local` keeps today's behaviour, while any other
/// value — a typo included, so an unrecognised value fails closed rather than downgrading — is
/// `hosted`, and then this refuses to store anything while `COLONIZER_MASTER_KEY` is unset: an
/// error, no plaintext file, no `.enc`, no keychain entry, and any stored value left untouched
/// (see [`credential_writable`]). A hosted install that has not been given a key must not quietly
/// grow plaintext secrets for its disk image, backups or control plane to leak.
///
/// What this protects, honestly: a copied, synced or backed-up config dir no longer leaks the
/// secrets. What it does not: a process running as the user can read the env var and the files,
/// so this is not a defense against local malware or the user themselves. A second machine with
/// a copy of the config dir needs the same `COLONIZER_MASTER_KEY` or its copy is dead by design.
/// Rotation means setting a new key value and re-saving each secret; if the old key is lost,
/// delete the `.enc` files and re-enter the secrets.
///
/// With the secret store installed (secrets.rs), a secret that lives in the system keychain — or a
/// new one while the keychain is available — is saved there instead, and no file is written.
pub fn write_secret(path: &Path, value: &str) -> Result<()> {
    if let Err(message) = credential_writable() {
        bail!("{message}");
    }
    if let Some(store) = crate::secrets::global() {
        if store.write(path, value)? {
            return Ok(());
        }
        write_file_secret(path, value)?;
        store.note_file(path);
        return Ok(());
    }
    write_file_secret(path, value)
}

/// The [`write_secret`] gate, pure so tests can pin it without racing env vars. `deployment` is the
/// raw `COLONIZER_DEPLOYMENT` value (`None` when unset): `local` and unset are local installs;
/// anything else is hosted — and hosted must never accept a credential without the master key.
fn plaintext_refused(deployment: Option<&str>, has_master_key: bool) -> bool {
    deployment.is_some_and(|v| v.trim() != "local") && !has_master_key
}

/// Whether storing a credential is allowed right now; `Err` carries the message both
/// [`write_secret`] and the provider PUT handler's pre-check surface, naming both env vars and
/// saying the credential was not stored.
pub(crate) fn credential_writable() -> Result<(), String> {
    let deployment = env_nonempty("COLONIZER_DEPLOYMENT");
    if plaintext_refused(deployment.as_deref(), master_key().is_some()) {
        return Err(format!(
            "refusing to store a credential in plaintext: COLONIZER_DEPLOYMENT={} requires \
             COLONIZER_MASTER_KEY; the credential was not stored",
            deployment.unwrap().trim()
        ));
    }
    Ok(())
}

/// The file half of [`write_secret`]: plaintext 0600, or the `.enc` envelope under
/// `COLONIZER_MASTER_KEY`. The secret store calls it directly when it moves a secret to its file,
/// so it carries the same [`credential_writable`] gate as [`write_secret`] — moving a secret out
/// of the keychain onto disk is exactly the write hosted mode must refuse.
pub fn write_file_secret(path: &Path, value: &str) -> Result<()> {
    if let Err(message) = credential_writable() {
        bail!("{message}");
    }
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
    if let Some(store) = crate::secrets::global() {
        store.forget(path);
    }
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(enc_path(path));
}

/// Reads a secret written by [`write_secret`]. If `<path>.enc` holds any non-empty content the
/// master key is required and the envelope must open, otherwise `None` is returned — fail
/// closed, never falling back to a stale plaintext and never returning ciphertext as if it
/// were the key (which would surface as silent 401s). With no `.enc` file the plaintext at
/// `path` is read, so pre-encryption secrets keep working.
pub fn read_secret(path: &Path) -> Option<String> {
    if let Some(value) = crate::secrets::global().and_then(|store| store.read(path)) {
        return value;
    }
    read_file_secret(path)
}

/// The file half of [`read_secret`], which the secret store also uses to move a file into the
/// keychain.
pub fn read_file_secret(path: &Path) -> Option<String> {
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
pub(crate) fn b64_encode(bytes: &[u8]) -> String {
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
pub(crate) fn b64_decode(s: &str) -> Option<Vec<u8>> {
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
    if s.is_empty() || !s.len().is_multiple_of(4) || !s.is_ascii() {
        return None;
    }
    let bytes = s.as_bytes();
    let pad = bytes.iter().rev().take_while(|&&c| c == b'=').count();
    if pad > 2 || bytes[..bytes.len() - pad].contains(&b'=') {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.as_chunks::<4>().0 {
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
    let unbound = aead::UnboundKey::new(&aead::CHACHA20_POLY1305, key).map_err(|_| anyhow::anyhow!("bad master key"))?;
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
    // The temp is `<whole file name>.<short id>.tmp` next to the target: unique per call, so two
    // writers never share one temp file, and the whole file name is kept — `with_extension` would
    // *replace* the extension, making `foo.jsonl` and `foo.json` collide on one temp path.
    let tmp = path
        .file_name()
        .map(|name| path.with_file_name(format!("{}.{}.tmp", name.to_string_lossy(), short_id())))
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

/// Reads a JSON file strictly: a missing file is `T::default()` (a first run), while any other
/// read error or a parse error is `Err` naming the file. The caller decides what an `Err` means —
/// a writer refuses to save over a file it could not read, and an infallible reader falls back to
/// the default loudly. This is the half of the fix for silent `unwrap_or_default` reads, whose
/// other half is that no save may follow a read that failed (#408).
pub fn read_json_or_default<T: serde::de::DeserializeOwned + Default>(path: &Path) -> Result<T> {
    let data = match std::fs::read(path) {
        Ok(data) => data,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
        Err(e) => return Err(e).with_context(|| format!("could not read {}", path.display())),
    };
    serde_json::from_slice(&data).with_context(|| format!("{} is not JSON the harness understands", path.display()))
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
/// move-aside and salvage copy in app.rs), so the error paths around persistence can be exercised
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

/// Whether an IP address is one only this machine, its network or a link-local service reaches:
/// loopback, the RFC1918 ranges, link-local (where the cloud metadata addresses live), unspecified,
/// broadcast, multicast, `0.0.0.0/8` and carrier-grade NAT for IPv4 — and for IPv6 the same
/// through any form that carries an IPv4 address (IPv4-mapped, IPv4-compatible, the NAT64 prefixes
/// and 6to4), plus the unique-local, link-local and site-local scopes.
pub fn private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                // 0.0.0.0/8: "this network"; Linux dials 0.x.x.x as this machine.
                || v4.octets()[0] == 0
                // 100.64.0.0/10: carrier-grade NAT, where tailnets live.
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64)
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_unspecified() {
                return true;
            }
            // ::ffff:a.b.c.d and the deprecated ::a.b.c.d answer for their IPv4 address.
            if let Some(v4) = v6.to_ipv4() {
                return private_ip(IpAddr::V4(v4));
            }
            let seg = v6.segments();
            let embedded = |hi: u16, lo: u16| std::net::Ipv4Addr::from((u32::from(hi) << 16) | u32::from(lo));
            // NAT64 (64:ff9b::/96, 64:ff9b:1::/48) carries the IPv4 address in its last 32 bits, and a
            // NAT64 gateway on the mothership's network would carry the request there.
            if seg[0] == 0x64 && seg[1] == 0xff9b {
                return private_ip(IpAddr::V4(embedded(seg[6], seg[7])));
            }
            // 6to4 (2002::/16) carries it in bits 16..48.
            if seg[0] == 0x2002 {
                return private_ip(IpAddr::V4(embedded(seg[1], seg[2])));
            }
            v6.is_multicast()
                || (seg[0] & 0xfe00) == 0xfc00 // unique-local, fc00::/7
                || (seg[0] & 0xffc0) == 0xfe80 // link-local, fe80::/10
                || (seg[0] & 0xffc0) == 0xfec0 // the deprecated site-local, fec0::/10
        }
    }
}

/// Whether the host a URL carries names this machine, its network or a link-local service — refused
/// where a request must stay off them (a scoped token's webhook, in `notify::subscriptions`). A
/// literal private address, or a name that means the same: `localhost` and its subdomains, `.local`,
/// `.internal`, or a dotless single label. A literal check: a public name that resolves to a private
/// address is not caught here — [`resolves_to_public`] does that at save time, and
/// [`public_redirect_policy`] on every hop of a delivery.
pub fn private_host(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else { return true };
    private_host_str(host)
}

/// [`private_host`] on one host string, as `Url::host_str` gives it — an IPv6 literal still in its
/// brackets, which come off before anything parses it.
pub fn private_host_str(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = host.parse::<IpAddr>() {
        return private_ip(ip);
    }
    let name = host.trim_end_matches('.').to_ascii_lowercase();
    name == "localhost"
        || name.ends_with(".localhost")
        || name.ends_with(".local")
        || name.ends_with(".internal")
        || !name.contains('.')
}

/// The pure core of [`resolves_to_public`]: there is at least one address, and every one of them is
/// public. A name that round-robins a public and a private address is refused whole, rather than
/// betting on which answer a delivery happens to get.
pub fn all_public_ips(ips: &[IpAddr]) -> bool {
    !ips.is_empty() && ips.iter().all(|ip| !private_ip(*ip))
}

/// Resolves `host` (tokio DNS, so the caller stays async) and answers whether every address it
/// resolved to is public, by way of [`all_public_ips`]. A name that does not resolve is refused —
/// the webhook would be broken anyway. An IPv6 literal's brackets come off first, so a caller may
/// hand over `Url::host_str` untouched.
pub async fn resolves_to_public(host: &str) -> bool {
    #[cfg(test)]
    {
        if let Some(public) = test_resolves_to_public(host) {
            return public;
        }
    }
    let host = host.trim_start_matches('[').trim_end_matches(']');
    match tokio::net::lookup_host((host, 0)).await {
        Ok(addrs) => all_public_ips(&addrs.map(|addr| addr.ip()).collect::<Vec<_>>()),
        Err(_) => false,
    }
}

/// A test seam for [`resolves_to_public`]: while the returned guard lives, hosts are answered by
/// `resolve` — `Some` its addresses, `None` that it does not resolve — instead of the network. A
/// published crate's tests reach nowhere outside their own crate, DNS included. Same shape and
/// reasoning as [`faults`]: a thread-local, because `#[tokio::test]` defaults to a current-thread
/// runtime, and production compiles this away.
#[cfg(test)]
fn test_resolves_to_public(host: &str) -> Option<bool> {
    let resolved = RESOLVER.with_borrow(|stack| stack.last().map(|resolve| resolve(host)))?;
    Some(resolved.is_some_and(|ips| all_public_ips(&ips)))
}

/// [`resolves_to_public`] for the one place that cannot await: a redirect policy, which reqwest
/// calls synchronously. An IP literal is judged as it is; a name is looked up through the system
/// resolver, on a multi-thread runtime inside `block_in_place` so the worker's other tasks move
/// elsewhere while it blocks. Redirects are rare and the lookup is the one the hop's own connect
/// would make anyway.
fn hop_resolves_to_public(host: &str) -> bool {
    use std::net::ToSocketAddrs;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = host.parse::<IpAddr>() {
        return !private_ip(ip);
    }
    #[cfg(test)]
    {
        if let Some(public) = test_resolves_to_public(host) {
            return public;
        }
    }
    let lookup = || match (host, 0).to_socket_addrs() {
        Ok(addrs) => all_public_ips(&addrs.map(|addr| addr.ip()).collect::<Vec<_>>()),
        Err(_) => false,
    };
    match tokio::runtime::Handle::try_current().map(|h| h.runtime_flavor()) {
        Ok(tokio::runtime::RuntimeFlavor::MultiThread) => tokio::task::block_in_place(lookup),
        _ => lookup(),
    }
}

#[cfg(test)]
pub(crate) fn install_test_resolver(resolve: TestResolve) -> ResolverGuard {
    RESOLVER.with_borrow_mut(|stack| stack.push(resolve));
    ResolverGuard
}

/// One installed resolver: a host's addresses, or `None` when it does not resolve.
#[cfg(test)]
pub(crate) type TestResolve = fn(&str) -> Option<Vec<IpAddr>>;

/// Pops the resolver [`install_test_resolver`] pushed when dropped, so one test's answers cannot
/// leak into the next.
#[cfg(test)]
pub(crate) struct ResolverGuard;

#[cfg(test)]
impl Drop for ResolverGuard {
    fn drop(&mut self) {
        RESOLVER.with_borrow_mut(|stack| {
            stack.pop();
        });
    }
}

#[cfg(test)]
thread_local! {
    static RESOLVER: std::cell::RefCell<Vec<TestResolve>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// How many hops a delivery client follows — the default policy's own limit, spelled out because a
/// custom policy does not inherit one.
const REDIRECT_HOPS: usize = 10;

/// The decision [`public_redirect_policy`] makes about one hop: follow it only while it stays an
/// http(s) address on a public host. A scheme change or a private host — this machine, its network,
/// the cloud metadata address — refuses the hop, so a public receiver's 302 cannot aim the
/// mothership's POST at somewhere the save-time check would never have allowed.
pub fn follow_public_hop(url: &reqwest::Url) -> bool {
    matches!(url.scheme(), "http" | "https") && !private_host(url)
}

/// The redirect policy for the notify module's delivery clients: [`follow_public_hop`] on every
/// hop, then the hop's name resolved and refused unless every address is public (a public-looking
/// name pointed at `169.254.169.254` passes the literal check), with the default policy's hop limit
/// kept (a custom policy has none of its own). The first URL never passes through a redirect policy
/// — the owner's webhook may point at their own machine — so this only ever narrows where a
/// redirect may carry a POST.
pub fn public_redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= REDIRECT_HOPS {
            attempt.error("too many redirects")
        } else if follow_public_hop(attempt.url()) && hop_resolves_to_public(attempt.url().host_str().unwrap_or_default()) {
            attempt.follow()
        } else {
            attempt.error("a redirect to a private or non-http(s) address is refused")
        }
    })
}

/// Owner-set private hosts allowed through the resolver, e.g. "owner-box.example". Only owner-created
/// webhooks and subscriptions without scoped tokens may be added here. Scoped-token deliveries never
/// bypass the policy.
static ALLOWED_PRIVATE_HOSTS: std::sync::LazyLock<std::sync::RwLock<std::collections::HashSet<String>>> =
    std::sync::LazyLock::new(|| std::sync::RwLock::new(std::collections::HashSet::new()));

/// Adds a private host to the allowlist, enabling owner-created subscriptions or webhooks pointing
/// there. Scoped-token subscriptions are never allowlisted.
pub fn allow_private_host(host: &str) {
    if !host.is_empty() {
        ALLOWED_PRIVATE_HOSTS
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(host.to_ascii_lowercase());
    }
}

/// A DNS resolver that pins resolved addresses, refusing private addresses unless the host is
/// allowlisted. This prevents DNS rebinding attacks where a name answers with a public address
/// at check time but rebinds to a private address when the connection is made.
struct PinnedResolver;

impl reqwest::dns::Resolve for PinnedResolver {
    fn resolve(
        &self,
        name: reqwest::dns::Name,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        Box<dyn Iterator<Item = std::net::SocketAddr> + Send + 'static>,
                        Box<dyn std::error::Error + Send + Sync>,
                    >,
                > + Send
                + 'static,
        >,
    > {
        Box::pin(async move {
            // Convert Name to lowercase string for matching against allowlist and test resolver
            let host = name.as_str().to_ascii_lowercase();

            let addrs = {
                #[cfg(test)]
                {
                    if let Some(resolved) = RESOLVER.with_borrow(|stack| stack.last().map(|resolve| resolve(&host))) {
                        if let Some(ips) = resolved {
                            ips.into_iter().map(|ip| (ip, 0).into()).collect::<Vec<_>>()
                        } else {
                            return Err("name does not resolve".into());
                        }
                    } else {
                        Self::system_resolve(&host).await?
                    }
                }
                #[cfg(not(test))]
                {
                    Self::system_resolve(&host).await?
                }
            };

            // Check if all addresses are public, or if the host is allowlisted
            let all_public = addrs.iter().all(|addr| !private_ip(addr.ip()));
            let is_allowlisted = ALLOWED_PRIVATE_HOSTS
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .contains(host.as_str());

            if all_public || is_allowlisted {
                tracing::debug!(host = %host, addrs = ?addrs, "pinned resolver: allowing addresses");
                Ok(Box::new(addrs.into_iter()) as Box<dyn Iterator<Item = std::net::SocketAddr> + Send>)
            } else {
                let ip_list = addrs.iter().map(|a| a.ip().to_string()).collect::<Vec<_>>().join(", ");
                let msg = format!("{host} resolves to a private address ({ip_list}); refused");
                tracing::warn!(host = %host, addrs = %ip_list, "pinned resolver: rejecting private address");
                Err(msg.into())
            }
        })
    }
}

impl PinnedResolver {
    async fn system_resolve(host: &str) -> Result<Vec<std::net::SocketAddr>, Box<dyn std::error::Error + Send + Sync>> {
        match tokio::net::lookup_host((host, 0)).await {
            Ok(addrs) => {
                let saddrs: Vec<_> = addrs.collect();
                if saddrs.is_empty() {
                    Err("no addresses found".into())
                } else {
                    Ok(saddrs)
                }
            }
            Err(e) => Err(format!("DNS lookup failed: {e}").into()),
        }
    }
}

/// Creates a DNS resolver that pins addresses to prevent rebinding attacks.
pub fn build_pinned_resolver() -> impl reqwest::dns::Resolve {
    PinnedResolver
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

/// Lowercase hex of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Streams `url` into `part`, checking its sha256 against `sha256` (lowercase hex). `progress` is told the
/// content length once and the running byte count about every megabyte. Redirects are followed: GitHub release
/// downloads redirect to object storage. A 404 says the file is not published, which is the usual reason a
/// freshly pinned release is not there yet. Leaves `part` behind on failure; the caller removes it.
pub async fn download_sha256<F, Fut>(url: &str, sha256: &str, part: &Path, progress: F) -> Result<()>
where
    F: Fn(Option<u64>, u64) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    use futures_util::StreamExt;
    let client = reqwest::Client::builder().connect_timeout(Duration::from_secs(30)).build()?;
    let response = client.get(url).send().await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        bail!("{url} is not published (404): the pinned release may not have been built yet");
    }
    let response = response.error_for_status()?;
    progress(response.content_length(), 0).await;
    let mut file = tokio::fs::File::create(part).await?;
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let mut stream = response.bytes_stream();
    let mut bytes = 0u64;
    let mut reported = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("the download was interrupted")?;
        digest.update(&chunk);
        file.write_all(&chunk).await?;
        bytes += chunk.len() as u64;
        if bytes - reported >= 1 << 20 {
            reported = bytes;
            progress(None, bytes).await;
        }
    }
    file.flush().await?;
    drop(file);
    progress(None, bytes).await;
    let got = hex(digest.finish().as_ref());
    if got != sha256 {
        bail!("checksum mismatch: expected {sha256}, got {got}");
    }
    Ok(())
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
    fn private_ips_cover_both_families_and_their_special_ranges() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        for bad in [
            "127.0.0.1",
            "10.0.0.1",
            "10.255.255.255",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254", // the cloud metadata address
            "100.64.0.1",      // carrier-grade NAT, where tailnets live
            "100.127.255.255", // the top of 100.64.0.0/10
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
            "::1",
            "::",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "ff02::1",
            "::ffff:10.0.0.1", // an IPv4-mapped address answers for its IPv4
            "::ffff:169.254.169.254",
            "::127.0.0.1",        // so does the deprecated IPv4-compatible form
            "0.1.2.3",            // 0.0.0.0/8
            "64:ff9b::a9fe:a9fe", // NAT64 to the metadata address
            "64:ff9b:1::a00:1",   // the local-use NAT64 prefix, to 10.0.0.1
            "2002:7f00:1::1",     // 6to4 around 127.0.0.1
            "fec0::1",            // deprecated site-local
        ] {
            assert!(private_ip(ip(bad)), "{bad} is private");
        }
        for good in [
            "1.1.1.1",
            "8.8.8.8",
            "93.184.216.34",
            "100.0.0.1",   // below 100.64.0.0/10
            "100.128.0.1", // above it
            "172.15.0.1",  // below 172.16.0.0/12
            "172.32.0.1",  // above it
            "2606:4700:4700::1111",
            "2001:4860:4860::8888",
            "64:ff9b::808:808", // NAT64 to a public address
            "2002:808:808::1",  // 6to4 around a public address
        ] {
            assert!(!private_ip(ip(good)), "{good} is public");
        }
    }

    #[test]
    fn private_host_names_cover_this_machine_and_its_neighborhood() {
        let url = |s: &str| reqwest::Url::parse(s).unwrap();
        for bad in [
            "http://localhost/hook",
            "http://api.localhost/hook",
            "http://printer.local/hook",
            "http://db.internal/hook",
            "http://localhost./hook", // a trailing dot is still localhost
            "http://intranet/hook",   // dotless: a name only this network knows
            "http://127.0.0.1/hook",
            "http://10.0.0.1/hook",
            "http://169.254.169.254/latest/meta-data",
            "http://[::1]/hook",
            "http://[fd00::1]/hook",
            "http://[::ffff:169.254.169.254]/", // the URL parser writes it as ::ffff:a9fe:a9fe
            "http://2852039166/",               // 169.254.169.254 as one decimal number
            "http://0x7f.1/",                   // 127.0.0.1 in hex shorthand
        ] {
            assert!(private_host(&url(bad)), "{bad} names this machine or its network");
        }
        for good in [
            "https://example.com/hook",
            "http://hooks.example.com/hook",
            "https://8.8.8.8/hook",
            "https://[2606:4700:4700::1111]/hook",
        ] {
            assert!(!private_host(&url(good)), "{good} is a public address");
        }
    }

    #[test]
    fn the_all_public_core_refuses_no_address_and_any_private_one() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        assert!(!all_public_ips(&[]), "a name with no address is not public");
        assert!(all_public_ips(&[ip("93.184.216.34"), ip("2606:4700::1")]));
        assert!(
            !all_public_ips(&[ip("93.184.216.34"), ip("10.0.0.5")]),
            "one private address in the round-robin refuses the name"
        );
        assert!(!all_public_ips(&[ip("127.0.0.1")]));
    }

    #[test]
    fn redirect_hops_are_followed_only_to_public_http_hosts() {
        let url = |s: &str| reqwest::Url::parse(s).unwrap();
        assert!(follow_public_hop(&url("https://example.com/x")));
        assert!(follow_public_hop(&url("http://example.com/x")));
        assert!(!follow_public_hop(&url("http://169.254.169.254/")));
        assert!(!follow_public_hop(&url("http://10.0.0.1/")));
        assert!(!follow_public_hop(&url("http://localhost/")));
        assert!(!follow_public_hop(&url("http://printer.local/")));
        assert!(
            !follow_public_hop(&url("ftp://example.com/x")),
            "the scheme must stay http(s)"
        );
        assert!(!follow_public_hop(&url("file:///etc/passwd")));
    }

    #[tokio::test]
    async fn the_dns_check_reads_the_installed_resolver_without_the_network() {
        fn resolve(host: &str) -> Option<Vec<IpAddr>> {
            let ip = |s: &str| s.parse::<IpAddr>().unwrap();
            match host {
                "public.example" => Some(vec![ip("93.184.216.34"), ip("2606:4700::1")]),
                "roundrobin.example" => Some(vec![ip("93.184.216.34"), ip("10.0.0.5")]),
                "intranet.example" => Some(vec![ip("10.0.0.5")]),
                _ => None, // nothing else resolves
            }
        }
        let _guard = install_test_resolver(resolve);
        assert!(resolves_to_public("public.example").await);
        assert!(!resolves_to_public("roundrobin.example").await);
        assert!(!resolves_to_public("intranet.example").await);
        assert!(
            !resolves_to_public("nx.example").await,
            "a name that does not resolve is refused"
        );
    }

    #[tokio::test]
    async fn localhost_resolves_to_a_private_address_and_is_refused() {
        // /etc/hosts answers this one, so no network is reached; the seam above is not installed,
        // which is the point — this is the real resolver path refusing a resolvable private name.
        assert!(!resolves_to_public("localhost").await);
    }

    #[tokio::test]
    async fn a_redirect_to_a_private_host_is_refused_and_never_dialed() {
        use std::io::{Read, Write};
        let first = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let second = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let (hop1, hop2) = (
            first.local_addr().unwrap().to_string(),
            second.local_addr().unwrap().to_string(),
        );
        std::thread::spawn(move || {
            if let Ok((mut conn, _)) = first.accept() {
                let mut buf = [0u8; 1024];
                let _ = conn.read(&mut buf); // the request head; the answer does not depend on it
                let _ = conn.write_all(
                    format!("HTTP/1.1 302 Found\r\nLocation: http://{hop2}/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .as_bytes(),
                );
            }
        });
        let client = reqwest::Client::builder().redirect(public_redirect_policy()).build().unwrap();
        let err = client
            .get(format!("http://{hop1}/"))
            .send()
            .await
            .expect_err("the 302 aims the request at 127.0.0.1, which the policy refuses");
        // reqwest wraps the policy's refusal ("a redirect to a private … address is refused") as
        // the source of its own "error following redirect", so the whole chain is checked.
        let mut names_refusal = false;
        let mut layer = Some(&err as &dyn std::error::Error);
        while let Some(e) = layer {
            names_refusal |= e.to_string().contains("private");
            layer = e.source();
        }
        assert!(names_refusal, "the error names the refusal: {err}");
        // And the refused hop was never dialed: the policy answers before any connection is made,
        // so the second listener sees nothing for as long as we care to look.
        second.set_nonblocking(true).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_millis(300);
        loop {
            match second.accept() {
                Ok(_) => panic!("the refused hop was dialed anyway"),
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if std::time::Instant::now() > deadline {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("accepting on the second listener: {e}"),
            }
        }
    }

    /// A hop to a public-looking name that resolves to a private address is refused like a literal
    /// one: the literal check alone would let `metadata.example` (pointed at 127.0.0.1 here, at
    /// 169.254.169.254 in the wild) through.
    #[tokio::test]
    async fn a_redirect_to_a_name_resolving_privately_is_refused() {
        use std::io::{Read, Write};
        fn resolve(host: &str) -> Option<Vec<IpAddr>> {
            (host == "metadata.example").then(|| vec!["127.0.0.1".parse().unwrap()])
        }
        let _guard = install_test_resolver(resolve);
        let first = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let hop1 = first.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            if let Ok((mut conn, _)) = first.accept() {
                let mut buf = [0u8; 1024];
                let _ = conn.read(&mut buf);
                let _ = conn.write_all(
                    b"HTTP/1.1 302 Found\r\nLocation: http://metadata.example/latest/meta-data\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
        });
        let client = reqwest::Client::builder().redirect(public_redirect_policy()).build().unwrap();
        let err = client
            .get(format!("http://{hop1}/"))
            .send()
            .await
            .expect_err("the hop resolves privately");
        let mut names_refusal = false;
        let mut layer = Some(&err as &dyn std::error::Error);
        while let Some(e) = layer {
            names_refusal |= e.to_string().contains("private");
            layer = e.source();
        }
        assert!(names_refusal, "refused by the policy, not by a failed connect: {err}");
        assert!(
            !hop_resolves_to_public("nx.example"),
            "a hop that does not resolve is refused"
        );
        assert!(
            !hop_resolves_to_public("[::ffff:7f00:1]"),
            "a bracketed literal is judged as it is"
        );
    }

    /// DNS rebinding attack: first query answers with a public address to pass the policy check,
    /// but the second query (when the connection is made) answers with loopback to redirect there.
    /// The pinned resolver refuses this by pinning the first answer.
    #[tokio::test]
    async fn pinned_resolver_refuses_rebinding_redirects() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static QUERY_COUNT: AtomicUsize = AtomicUsize::new(0);

        fn resolve(host: &str) -> Option<Vec<IpAddr>> {
            let ip = |s: &str| s.parse::<IpAddr>().unwrap();
            match host {
                "rebind.example" => {
                    // First query returns public, second+ returns loopback
                    let count = QUERY_COUNT.fetch_add(1, Ordering::SeqCst);
                    if count == 0 {
                        Some(vec![ip("93.184.216.34")]) // public on first query
                    } else {
                        Some(vec![ip("127.0.0.1")]) // loopback on rebind
                    }
                }
                _ => None,
            }
        }
        let _guard = install_test_resolver(resolve);

        // Server A: accepts one connection and 302-redirects to rebind.example
        let server_a = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let server_a_addr = server_a.local_addr().unwrap().to_string();

        // Server B: counts connections (should be zero if resolver is pinning)
        let server_b = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let server_b_addr = server_b.local_addr().unwrap().port();
        let connection_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let connection_count_clone = connection_count.clone();

        std::thread::spawn(move || {
            if let Ok((mut conn, _)) = server_a.accept() {
                use std::io::{Read, Write};
                let mut buf = [0u8; 1024];
                let _ = conn.read(&mut buf);
                // Redirect to rebind.example on server B's port
                let resp = format!(
                    "HTTP/1.1 302 Found\r\nLocation: http://rebind.example:{}/\r\n\
                    Content-Length: 0\r\nConnection: close\r\n\r\n",
                    server_b_addr
                );
                let _ = conn.write_all(resp.as_bytes());
            }
        });

        std::thread::spawn(move || {
            // Server B: count any connections (there should be none because the resolver refuses)
            server_b.set_nonblocking(true).ok();
            let deadline = std::time::Instant::now() + Duration::from_millis(500);
            loop {
                match server_b.accept() {
                    Ok(_) => {
                        connection_count_clone.store(1, Ordering::SeqCst);
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        if std::time::Instant::now() > deadline {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        // Client with pinned resolver should reject the rebinding
        let client = reqwest::Client::builder()
            .redirect(public_redirect_policy())
            .dns_resolver(build_pinned_resolver())
            .build()
            .unwrap();

        let err = client
            .get(format!("http://{server_a_addr}/"))
            .send()
            .await
            .expect_err("the pinned resolver should refuse the rebinding");

        // Verify the refusal is in the error chain
        let mut sees_refusal = false;
        let mut layer = Some(&err as &dyn std::error::Error);
        while let Some(e) = layer {
            sees_refusal |= e.to_string().contains("private");
            layer = e.source();
        }
        assert!(sees_refusal, "error should mention private address refusal: {err}");

        // Verify server B was never reached
        assert_eq!(
            connection_count.load(Ordering::SeqCst),
            0,
            "the loopback server should never be dialed"
        );
    }

    #[tokio::test]
    async fn pinned_resolver_refuses_private_addresses() {
        fn resolve(host: &str) -> Option<Vec<IpAddr>> {
            let ip = |s: &str| s.parse::<IpAddr>().unwrap();
            match host {
                "rebind.example" => Some(vec![ip("127.0.0.1")]),
                "public.example" => Some(vec![ip("93.184.216.34")]),
                _ => None,
            }
        }
        let _guard = install_test_resolver(resolve);

        // Build a client with the pinned resolver
        let client = reqwest::Client::builder()
            .dns_resolver(build_pinned_resolver())
            .build()
            .unwrap();

        // A request to a public address should work (or at least not fail on DNS resolution)
        // A request to a private address should fail
        let err = client
            .get("http://rebind.example/test")
            .send()
            .await
            .expect_err("the resolver should refuse the private address");

        // The error should mention the refusal
        let mut mentions_refusal = false;
        let mut layer = Some(&err as &dyn std::error::Error);
        while let Some(e) = layer {
            mentions_refusal |= e.to_string().contains("private") || e.to_string().contains("refused");
            layer = e.source();
        }
        assert!(mentions_refusal, "error should mention the refusal: {err}");
    }

    #[tokio::test]
    async fn pinned_resolver_allows_allowlisted_private_hosts() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        fn resolve(host: &str) -> Option<Vec<IpAddr>> {
            let ip = |s: &str| s.parse::<IpAddr>().unwrap();
            match host {
                "owner-box.example" => Some(vec![ip("127.0.0.1")]),
                _ => None,
            }
        }
        let _guard = install_test_resolver(resolve);

        // Before allowing, the resolver should reject
        let client1 = reqwest::Client::builder()
            .dns_resolver(build_pinned_resolver())
            .build()
            .unwrap();

        let err = client1
            .get(format!("http://owner-box.example:{port}/test"))
            .send()
            .await
            .expect_err("private address should be refused before allowlisting");

        let mut mentions_refusal = false;
        let mut layer = Some(&err as &dyn std::error::Error);
        while let Some(e) = layer {
            mentions_refusal |= e.to_string().contains("private") || e.to_string().contains("refused");
            layer = e.source();
        }
        assert!(mentions_refusal, "should refuse private address initially");

        // After allowlisting, the resolver should allow it
        allow_private_host("owner-box.example");

        // Set up a simple server that responds with 200
        std::thread::spawn(move || {
            if let Ok((mut conn, _)) = listener.accept() {
                use std::io::{Read, Write};
                let mut buf = [0u8; 1024];
                let _ = conn.read(&mut buf);
                let _ = conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK");
            }
        });

        let client2 = reqwest::Client::builder()
            .dns_resolver(build_pinned_resolver())
            .build()
            .unwrap();

        // Now it should reach the address after allowlisting
        let result = client2.get(format!("http://owner-box.example:{port}/test")).send().await;

        // Either it succeeds, or it fails with a connection error (not a DNS refusal)
        match result {
            Ok(_) => {} // Success
            Err(e) => {
                // Make sure it's not a DNS refusal
                let mut mentions_refusal = false;
                let mut layer = Some(&e as &dyn std::error::Error);
                while let Some(err) = layer {
                    mentions_refusal |= err.to_string().contains("private") || err.to_string().contains("refused");
                    layer = err.source();
                }
                assert!(!mentions_refusal, "after allowlisting, should not refuse: {e}");
            }
        }
    }

    /// Allowlisted private host delivers end-to-end: an allowlisted hostname resolving to 127.0.0.1
    /// can deliver through a client with the pinned resolver.
    #[tokio::test]
    async fn allowlisted_private_webhook_delivers_end_to_end() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        fn resolve(host: &str) -> Option<Vec<IpAddr>> {
            let ip = |s: &str| s.parse::<IpAddr>().unwrap();
            match host {
                "owner-private.example" => Some(vec![ip("127.0.0.1")]),
                _ => None,
            }
        }
        let _guard = install_test_resolver(resolve);

        // Set up a loopback server that accepts one connection and responds with 200
        std::thread::spawn(move || {
            if let Ok((mut conn, _)) = listener.accept() {
                use std::io::{Read, Write};
                let mut buf = [0u8; 1024];
                let _ = conn.read(&mut buf);
                let _ = conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello");
            }
        });

        // Allowlist the hostname
        allow_private_host("owner-private.example");

        // Build a client like the delivery clients: pinned resolver + redirect policy
        let client = reqwest::Client::builder()
            .dns_resolver(build_pinned_resolver())
            .redirect(public_redirect_policy())
            .build()
            .unwrap();

        // Request to the allowlisted private hostname succeeds
        let resp = client
            .get(format!("http://owner-private.example:{port}/test"))
            .send()
            .await
            .expect("allowlisted private host should deliver");

        assert_eq!(resp.status(), 200);
        let body = resp.text().await.expect("read body");
        assert_eq!(body, "hello");
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

    /// Every leftover temp file name in `dir` — the thing every `write_atomic` test wants to say is
    /// empty, whichever unique name the temp carried.
    fn temps_in(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|name| name.ends_with(".tmp"))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn write_atomic_replaces_the_target_and_leaves_no_temp_behind() {
        let dir = temp_root("atomic-ok");
        let path = dir.join("sessions.json");
        write_atomic(&path, b"first").await.unwrap();
        write_atomic(&path, b"second").await.unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
        assert!(
            temps_in(&dir).is_empty(),
            "a successful write renames its unique temp away: {:?}",
            temps_in(&dir)
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
        assert!(
            temps_in(&dir).is_empty(),
            "the leftover temp is cleaned up: {:?}",
            temps_in(&dir)
        );
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
        assert!(temps_in(&dir).is_empty(), "{:?}", temps_in(&dir));
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
        assert!(temps_in(&dir).is_empty(), "{:?}", temps_in(&dir));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn write_atomic_temps_are_unique_and_named_for_the_whole_file() {
        let dir = temp_root("atomic-tmp-name");
        let path = dir.join("events.jsonl");
        let _guard = inject("events.jsonl", Op::Write, enospc);
        let err = write_atomic(&path, b"new").await.unwrap_err();
        assert!(
            err.to_string().contains("events.jsonl."),
            "the temp keeps the target's full name, not a `with_extension` .json.tmp: {err:#}"
        );
        drop(_guard);
        // Two writes in flight at once name two different temps, so one's rename cannot consume
        // the other's file — the reason the temp carries a per-call id.
        let first = write_atomic(&path, b"one");
        let second = write_atomic(&path, b"two");
        let (first, second) = tokio::join!(first, second);
        first.unwrap();
        second.unwrap();
        assert!(temps_in(&dir).is_empty(), "{:?}", temps_in(&dir));
        let err = write_atomic(Path::new("/"), b"new").await.unwrap_err();
        assert!(
            err.to_string().contains("names no file"),
            "a path with no file name errors instead of panicking: {err:#}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn read_json_or_default_treats_a_missing_file_as_the_default_and_refuses_damaged_ones() {
        #[derive(Debug, Default, PartialEq, serde::Deserialize)]
        struct Cfg {
            #[serde(default)]
            on: bool,
        }
        let dir = temp_root("read-json");
        let path = dir.join("cfg.json");
        assert_eq!(
            read_json_or_default::<Cfg>(&path).unwrap(),
            Cfg::default(),
            "missing is a first run"
        );
        std::fs::write(&path, b"{\"on\":true}").unwrap();
        assert_eq!(read_json_or_default::<Cfg>(&path).unwrap(), Cfg { on: true });
        std::fs::write(&path, b"not json").unwrap();
        let err = read_json_or_default::<Cfg>(&path).unwrap_err();
        assert!(err.to_string().contains("cfg.json"), "the error names the file: {err:#}");
        // A directory in the file's place is a read error, not a parse guess.
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(read_json_or_default::<Cfg>(&path).is_err());
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

    /// Runs `f` with the env var `key` set to `value` (or removed when `None`), restoring whatever
    /// was there before. Callers must already hold `ENV_LOCK`.
    fn with_env_var(key: &str, value: Option<&str>, f: impl FnOnce()) {
        let prev = std::env::var(key).ok();
        // SAFETY: env-mutating tests are serialised on ENV_LOCK, so no other test in this
        // process can observe the variable mid-change.
        unsafe {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
        f();
        unsafe {
            match prev {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }

    /// Runs `f` with `COLONIZER_MASTER_KEY` set to `key` (or removed when `None`), restoring
    /// whatever was there before. Callers must already hold `ENV_LOCK`.
    fn with_master_key_env(key: Option<&str>, f: impl FnOnce()) {
        with_env_var("COLONIZER_MASTER_KEY", key, f)
    }

    /// Same, for `COLONIZER_DEPLOYMENT`. Callers must already hold `ENV_LOCK`.
    fn with_deployment_env(value: Option<&str>, f: impl FnOnce()) {
        with_env_var("COLONIZER_DEPLOYMENT", value, f)
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
    fn hosted_deployment_refuses_a_credential_without_a_master_key_and_writes_nothing() {
        let _lock = ENV_LOCK.lock().unwrap();
        // The pure gate: hosted refuses exactly when the key is missing; local never refuses, and
        // an unrecognised deployment value counts as hosted (fail closed).
        assert!(plaintext_refused(Some("hosted"), false));
        assert!(!plaintext_refused(Some("hosted"), true));
        assert!(!plaintext_refused(None, false));
        assert!(!plaintext_refused(Some("local"), false));
        assert!(!plaintext_refused(Some(" local "), false));
        assert!(plaintext_refused(Some("prod"), false));

        let dir = temp_root("hosted-refusal");
        let path = dir.join("github-token");
        std::fs::write(&path, "pre-existing-token").unwrap();
        with_deployment_env(Some("hosted"), || {
            with_master_key_env(None, || {
                let err = write_secret(&path, "ghp_newsecret").unwrap_err().to_string();
                assert!(err.contains("COLONIZER_DEPLOYMENT"), "{err}");
                assert!(err.contains("COLONIZER_MASTER_KEY"), "{err}");
                assert!(err.contains("not stored"), "{err}");
                // The pre-existing value is untouched, and nothing shadows it.
                assert_eq!(std::fs::read_to_string(&path).unwrap(), "pre-existing-token");
                assert!(!enc_path(&path).exists());
                // A first save leaves nothing on disk at all: no plaintext, no .enc.
                let fresh = dir.join("provider-keys/deepseek");
                assert!(write_secret(&fresh, "sk-test-SECRET").is_err());
                assert!(!fresh.exists());
                assert!(!enc_path(&fresh).exists());
                // The file half is gated too: the secret store's move-to-file path (and any other
                // direct caller) refuses exactly the same way.
                assert!(write_file_secret(&fresh, "sk-test-SECRET").is_err());
                assert!(!fresh.exists());
                assert!(!enc_path(&fresh).exists());
            });
        });
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn hosted_with_a_master_key_still_seals_instead_of_storing_plaintext() {
        let _lock = ENV_LOCK.lock().unwrap();
        let dir = temp_root("hosted-sealed");
        let path = dir.join("provider-key");
        with_deployment_env(Some("hosted"), || {
            with_master_key_env(Some("test-master-key-for-hosted-012345678900"), || {
                write_secret(&path, "sk-ant-hosted-secret").unwrap();
                assert_eq!(read_secret(&path).as_deref(), Some("sk-ant-hosted-secret"));
                assert!(!path.exists(), "hosted stores ciphertext, never plaintext");
                let raw = std::fs::read_to_string(enc_path(&path)).unwrap();
                assert!(!raw.contains("sk-ant-hosted-secret"), "the .enc must not carry the value");
            });
            // A wrong key on read fails closed like any other install.
            with_master_key_env(Some("a-different-key-0000000000000000000000"), || {
                assert_eq!(read_secret(&path), None);
            });
        });
        let _ = std::fs::remove_dir_all(dir);
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
