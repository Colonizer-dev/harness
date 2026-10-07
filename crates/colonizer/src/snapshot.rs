//! Memory snapshots for suspended colonies (issue #702): the encrypted-at-rest store a frozen
//! colony is kept in, and the credentials a restore re-mints. Host-side and gated off —
//! `sandbox::supports_memory_snapshot()` is false until the pinned microsandbox can restore a
//! `--secret`-carrying sandbox — so the msb create/restore half and delivery of the rotated
//! credentials are the follow-up; the sealing, key handling, decision and rotation below are built.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use ring::aead;
use ring::rand::{SecureRandom, SystemRandom};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::util::write_private;

/// A colony coming back from its sealed snapshot, alongside [`crate::sessions::SESSION_RESUME`].
pub(crate) const MEMORY_SNAPSHOT: &str = "memory_snapshot";

/// `<session dir>/snapshots/` (counted by the host-disk walk, removed with the colony), and the key
/// dir, a sibling of `sessions/` under the data dir — never inside the session directory.
const SNAPSHOT_DIR: &str = "snapshots";
const SNAPSHOT_KEYS_DIR: &str = "snapshot-keys";

/// Key length, per-chunk plaintext (only one chunk plus tag in memory), resident-memory cap, TTL.
pub(crate) const KEY_LEN: usize = 32;
pub(crate) const CHUNK_SIZE: usize = 1024 * 1024;
pub(crate) const MAX_RESIDENT_BYTES: u64 = 8 * 1024 * 1024 * 1024;
pub(crate) const TTL: chrono::Duration = chrono::Duration::hours(48);

/// Framing: `COLSNP` + version, the sealed/image extensions, the final-chunk flag, the AEAD tag,
/// and the most a sealed header may declare per chunk (bounding a hostile file's allocation).
const MAGIC: &[u8; 8] = b"COLSNP\x00\x01";
const SEALED_EXT: &str = "cseal";
const IMAGE_EXT: &str = "raw";
const FINAL_FLAG: u8 = 0x01;
const TAG_LEN: usize = 16;
const MAX_CHUNK: usize = 16 * 1024 * 1024;

/// A per-colony snapshot key, hex-encoded and 0600 under `App::snapshot_key_file`.
pub(crate) struct SnapshotKey([u8; KEY_LEN]);

impl SnapshotKey {
    pub(crate) fn generate() -> Result<Self> {
        let mut key = [0u8; KEY_LEN];
        SystemRandom::new()
            .fill(&mut key)
            .map_err(|_| anyhow::anyhow!("no system randomness for a snapshot key"))?;
        Ok(Self(key))
    }

    fn aead_key(&self) -> Result<aead::LessSafeKey> {
        let key =
            aead::UnboundKey::new(&aead::CHACHA20_POLY1305, &self.0).map_err(|_| anyhow::anyhow!("invalid snapshot key"))?;
        Ok(aead::LessSafeKey::new(key))
    }

    pub(crate) fn to_hex(&self) -> String {
        crate::util::hex(&self.0)
    }

    pub(crate) fn from_hex(text: &str) -> Result<Self> {
        let text = text.trim();
        if !text.len().is_multiple_of(2) {
            bail!("snapshot key is odd-length hex");
        }
        let bytes: Vec<u8> = (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).context("snapshot key is not valid hex"))
            .collect::<Result<_>>()?;
        let key = bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("snapshot key is not {KEY_LEN} bytes"))?;
        Ok(Self(key))
    }
}

/// What a suspension records: the msb name (the sealed file's stem), the plaintext size, the seal time.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub(crate) struct SnapshotMeta {
    pub name: String,
    pub bytes: u64,
    pub sealed_at: DateTime<Utc>,
}

/// How a suspended colony comes back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResumePath {
    MemorySnapshot,
    SessionResume,
}

/// Whether a colony comes back from its snapshot, from facts gathered up front so the decision is
/// pure. Any missing piece or failure is the transcript resume; only `Some(Ok(()))` fully in place
/// restores.
pub(crate) fn decide_resume(
    now: DateTime<Utc>,
    snapshot: Option<&SnapshotMeta>,
    key_present: bool,
    restore: Option<Result<(), String>>,
) -> ResumePath {
    let Some(meta) = snapshot else {
        return ResumePath::SessionResume;
    };
    if meta.bytes > MAX_RESIDENT_BYTES || now >= meta.sealed_at + TTL || !key_present {
        return ResumePath::SessionResume;
    }
    match restore {
        Some(Ok(())) => ResumePath::MemorySnapshot,
        _ => ResumePath::SessionResume,
    }
}

impl crate::App {
    /// `<session dir>/snapshots/`, inside the session dir so the host-disk walk counts it.
    pub(crate) fn snapshot_dir(&self, session: &str) -> PathBuf {
        self.session_dir(session).join(SNAPSHOT_DIR)
    }

    /// The key file under the mothership's private state; 0600, like `gateway_token_file`.
    pub(crate) fn snapshot_key_file(&self, session: &str) -> PathBuf {
        self.cfg.data_dir.join(SNAPSHOT_KEYS_DIR).join(session)
    }
}

fn snap_file(app: &crate::App, session: &str, name: &str, ext: &str) -> PathBuf {
    app.snapshot_dir(session).join(format!("{name}.{ext}"))
}

/// The plaintext image a capture leaves for [`create`] to seal.
pub(crate) fn snapshot_image(app: &crate::App, session: &str, name: &str) -> PathBuf {
    snap_file(app, session, name, IMAGE_EXT)
}

fn sealed_path(app: &crate::App, session: &str, name: &str) -> PathBuf {
    snap_file(app, session, name, SEALED_EXT)
}

/// Where [`restore`] decrypts to, for the (follow-up) msb restore; removed once it has run.
pub(crate) fn staging_path(app: &crate::App, session: &str, name: &str) -> PathBuf {
    snap_file(app, session, name, "staging")
}

/// Seals a captured plaintext image into the colony's encrypted store, removing it; `None` (no
/// image, over the cap, or a seal failure) is the caller's transcript-resume fallback.
pub(crate) async fn capture(app: &crate::Shared, session: &str, name: &str) -> Option<SnapshotMeta> {
    let image = snapshot_image(app, session, name);
    let (session, name) = (session.to_string(), name.to_string());
    let (block_app, block_session, block_name) = (app.clone(), session.clone(), name.clone());
    let block = tokio::task::spawn_blocking(move || create(&block_app, &block_session, &block_name, &image)).await;
    let why = match block {
        Ok(Ok(meta)) => return Some(meta),
        Ok(Err(e)) => format!("memory snapshot capture failed ({e:#})"),
        Err(e) => format!("memory snapshot capture did not finish ({e})"),
    };
    app.session_log(&session, "warn", format!("{why}; resuming from the transcript"))
        .await;
    None
}

/// Seals the image at `plaintext` into the colony's store and removes it, minting the key on first
/// use. Whatever happens the plaintext image and any partial `.part` temp are removed.
pub(crate) fn create(app: &crate::App, session: &str, name: &str, plaintext: &Path) -> Result<SnapshotMeta> {
    seal_image(app, session, name, plaintext, MAX_RESIDENT_BYTES)
}

/// The cap is a parameter so a test can force the over-cap failure without an 8 GiB file.
fn seal_image(app: &crate::App, session: &str, name: &str, plaintext: &Path, cap: u64) -> Result<SnapshotMeta> {
    let sealed = sealed_path(app, session, name);
    let temp = sealed.with_extension("part");
    let outcome = (|| -> Result<SnapshotMeta> {
        let bytes = std::fs::metadata(plaintext).context("the snapshot image is gone")?.len();
        if bytes > cap {
            bail!("the snapshot image is {bytes} bytes, past the {cap}-byte cap");
        }
        let dir = app.snapshot_dir(session);
        std::fs::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;
        let key = load_or_create_key(app, session)?;
        seal_file(&key, plaintext, &temp, CHUNK_SIZE)?;
        std::fs::rename(&temp, &sealed)
            .with_context(|| format!("could not place the sealed snapshot at {}", sealed.display()))?;
        Ok(SnapshotMeta {
            name: name.to_string(),
            bytes,
            sealed_at: Utc::now(),
        })
    })();
    let _ = std::fs::remove_file(plaintext);
    let _ = std::fs::remove_file(&temp);
    outcome
}

/// Decrypts a sealed snapshot into the [staging](staging_path) file and returns its path for the
/// (follow-up) msb restore. Any failure is an `Err`, and the caller falls back to the transcript.
pub(crate) fn restore(app: &crate::App, session: &str, meta: &SnapshotMeta) -> Result<PathBuf> {
    let key = read_key(app, session)?;
    let sealed = sealed_path(app, session, &meta.name);
    if !sealed.exists() {
        bail!("no sealed snapshot at {}", sealed.display());
    }
    let staging = staging_path(app, session, &meta.name);
    open_file(&key, &sealed, &staging)?;
    Ok(staging)
}

/// Removes a decrypt's staging file; best-effort, so a colony that already fell back is not failed.
pub(crate) fn remove_staging(app: &crate::App, session: &str, name: &str) {
    let _ = std::fs::remove_file(staging_path(app, session, name));
}

/// The msb thaw, `msb snapshot restore <sandbox>` on the staging file. Not built: the pinned msb
/// cannot restore a `--secret`-carrying sandbox (sandbox.rs), so this always fails.
fn thaw(_staging: &Path, _sandbox: &str) -> Result<()> {
    bail!("memory-snapshot restore is not supported by the pinned microsandbox yet")
}

/// The restore half of a suspension's come-back, behind the false gate. It decrypts the snapshot
/// and runs the (stub) msb thaw, then on a real success would re-mint the credentials. Because the
/// thaw always fails this always decides the session-resume fallback and always removes the staging
/// plaintext, so it can never claim a memory resume today; replacing [`thaw`] lights up rotation.
pub(crate) async fn resume(app: &crate::Shared, session: &str, sandbox: &str, snapshot: Option<serde_json::Value>) -> bool {
    let Some(meta) = snapshot.and_then(|v| serde_json::from_value::<SnapshotMeta>(v).ok()) else {
        return false;
    };
    let key_present = app.snapshot_key_file(session).exists();
    let outcome: Option<Result<(), String>> = Some(
        restore(app, session, &meta)
            .and_then(|staging| thaw(&staging, sandbox))
            .map_err(|e| format!("{e:#}")),
    );
    if decide_resume(Utc::now(), Some(&meta), key_present, outcome) == ResumePath::MemorySnapshot {
        let _ = rotate_credentials(app, session, sandbox).await;
    }
    remove_staging(app, session, &meta.name);
    false
}

/// Drops a colony's snapshot for good: best-effort `msb snapshot remove <name>`, then the sealed
/// directory and the key. Nothing here fails the caller — a deleted colony must go regardless.
pub(crate) async fn remove(app: &crate::App, session: &str, name: Option<&str>) {
    if let Some(name) = name {
        let mut cmd = tokio::process::Command::new(&app.cfg.msb);
        cmd.args(["snapshot", "remove", name]);
        let _ = crate::util::exec(&mut cmd).await;
    }
    remove_files(app, session);
}

/// The file half of [`remove`], synchronous so it is testable: the sealed dir and the key file.
pub(crate) fn remove_files(app: &crate::App, session: &str) {
    let _ = std::fs::remove_dir_all(app.snapshot_dir(session));
    let _ = std::fs::remove_file(app.snapshot_key_file(session));
}

/// The key file's hex, or a freshly minted key (dir 0700, file written to a temp then renamed). A
/// read error other than "not found" is propagated, never silently overwritten.
fn load_or_create_key(app: &crate::App, session: &str) -> Result<SnapshotKey> {
    use std::os::unix::fs::DirBuilderExt;
    let path = app.snapshot_key_file(session);
    match std::fs::read_to_string(&path) {
        Ok(text) => SnapshotKey::from_hex(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let key = SnapshotKey::generate()?;
            let dir = path.parent().unwrap_or(&path);
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true).mode(0o700);
            builder
                .create(dir)
                .with_context(|| format!("could not create {}", dir.display()))?;
            let temp = path.with_extension("tmp");
            write_private(&temp, key.to_hex().as_bytes())?;
            std::fs::rename(&temp, &path).with_context(|| format!("could not place the snapshot key at {}", path.display()))?;
            Ok(key)
        }
        Err(e) => Err(e).with_context(|| format!("could not read the snapshot key at {}", path.display())),
    }
}

fn read_key(app: &crate::App, session: &str) -> Result<SnapshotKey> {
    let path = app.snapshot_key_file(session);
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("could not read the snapshot key at {}", path.display()))?;
    SnapshotKey::from_hex(&text)
}

/// Mints a fresh gateway token over the colony's, revoking the old one: the gateway validates by
/// reading this file per request (`gateway::colony_for_token`), so the old one stops matching.
pub(crate) async fn rotate_gateway_token(app: &crate::App, session: &str) -> Result<String> {
    let token = crate::util::random_token();
    app.store()
        .write_private(session, crate::gateway::GATEWAY_TOKEN_FILE, token.as_bytes())
        .await?;
    Ok(token)
}

/// Re-mints both credentials a restored colony carries, for the caller to deliver: the gateway token
/// (old one revoked) and a fresh tailnet VM key, the old node dropped first so two nodes never claim
/// the sandbox name. If the mesh step fails the gateway token is already rotated; that is acceptable,
/// since the caller falls back and the fresh boot re-mints both.
pub(crate) async fn rotate_credentials(app: &crate::App, session: &str, sandbox: &str) -> Result<(String, String)> {
    let gateway_token = rotate_gateway_token(app, session).await?;
    let mesh = app.mesh().await?;
    mesh.delete_nodes_named(sandbox).await?;
    let vm_key = mesh.mint_vm_key().await?;
    Ok((gateway_token, vm_key))
}

/// Seals `plaintext` into `sealed` under `key` a chunk at a time. The file is
/// `magic(8) | nonce_prefix(8) | chunk_size(4 LE)`, then repeated `flags(1) | len(4 LE) | ciphertext`;
/// each chunk's nonce is `nonce_prefix || counter(4 BE)`, so a reordered chunk fails, and the
/// header/flags/length are the AEAD's associated data, so none can be edited. `FINAL_FLAG` on the
/// last chunk makes a file cut at a record boundary read as truncated, not as a shorter file.
pub(crate) fn seal_file(key: &SnapshotKey, plaintext: &Path, sealed: &Path, chunk: usize) -> Result<u64> {
    let aead = key.aead_key()?;
    if chunk == 0 || chunk > MAX_CHUNK {
        bail!("chunk size {chunk} is out of range");
    }
    let mut nonce_prefix = [0u8; 8];
    SystemRandom::new()
        .fill(&mut nonce_prefix)
        .map_err(|_| anyhow::anyhow!("no system randomness for a snapshot nonce"))?;
    let mut header = Vec::with_capacity(20);
    header.extend_from_slice(MAGIC);
    header.extend_from_slice(&nonce_prefix);
    header.extend_from_slice(&(chunk as u32).to_le_bytes());

    let mut input = std::fs::File::open(plaintext).with_context(|| format!("could not open {}", plaintext.display()))?;
    let mut out = std::fs::File::create(sealed).with_context(|| format!("could not create {}", sealed.display()))?;
    out.write_all(&header)?;

    let mut buf = vec![0u8; chunk];
    let mut carry: Option<u8> = None;
    let mut counter = 0u32;
    let mut total = 0u64;
    loop {
        let mut filled = 0;
        if let Some(byte) = carry.take() {
            buf[0] = byte;
            filled = 1;
        }
        while filled < chunk {
            let n = input.read(&mut buf[filled..])?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        if filled == 0 {
            // An empty image still gets one empty, final chunk, so opening yields an empty file.
            write_record(&mut out, &aead, &header, &nonce_prefix, counter, FINAL_FLAG, &[])?;
            break;
        }
        let mut probe = [0u8; 1];
        let more = input.read(&mut probe)? == 1;
        if more {
            carry = Some(probe[0]);
        }
        let flags = if more { 0 } else { FINAL_FLAG };
        write_record(&mut out, &aead, &header, &nonce_prefix, counter, flags, &buf[..filled])?;
        total += filled as u64;
        counter += 1;
        if !more {
            break;
        }
    }
    out.flush()?;
    Ok(total)
}

fn record_aad(header: &[u8], flags: u8, len: [u8; 4]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(header.len() + 5);
    aad.extend_from_slice(header);
    aad.push(flags);
    aad.extend_from_slice(&len);
    aad
}

fn chunk_nonce(prefix: &[u8; 8], counter: u32) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[..8].copy_from_slice(prefix);
    nonce[8..].copy_from_slice(&counter.to_be_bytes());
    nonce
}

#[allow(clippy::too_many_arguments)]
fn write_record(
    out: &mut std::fs::File,
    aead: &aead::LessSafeKey,
    header: &[u8],
    prefix: &[u8; 8],
    counter: u32,
    flags: u8,
    data: &[u8],
) -> Result<()> {
    let len = (data.len() as u32).to_le_bytes();
    out.write_all(&[flags])?;
    out.write_all(&len)?;
    let nonce = aead::Nonce::assume_unique_for_key(chunk_nonce(prefix, counter));
    let aad = record_aad(header, flags, len);
    let mut in_out = data.to_vec();
    aead.seal_in_place_append_tag(nonce, aead::Aad::from(&aad), &mut in_out)
        .map_err(|_| anyhow::anyhow!("could not seal snapshot chunk {counter}"))?;
    out.write_all(&in_out)?;
    Ok(())
}

/// Opens a file sealed by [`seal_file`], writing the plaintext to `plaintext`. Refuses a truncated
/// file, a reordered or edited chunk, and trailing bytes after the final chunk.
pub(crate) fn open_file(key: &SnapshotKey, sealed: &Path, plaintext: &Path) -> Result<u64> {
    let aead = key.aead_key()?;
    let mut f = std::fs::File::open(sealed).with_context(|| format!("could not open {}", sealed.display()))?;
    let mut header = [0u8; 20];
    f.read_exact(&mut header)
        .with_context(|| format!("{} is not a snapshot file", sealed.display()))?;
    if &header[..8] != MAGIC {
        bail!("{} is not a Colonizer snapshot file", sealed.display());
    }
    let mut nonce_prefix = [0u8; 8];
    nonce_prefix.copy_from_slice(&header[8..16]);
    let chunk = u32::from_le_bytes([header[16], header[17], header[18], header[19]]) as usize;
    if chunk == 0 || chunk > MAX_CHUNK {
        bail!("{} declares a chunk size of {chunk}, out of range", sealed.display());
    }

    let mut out = std::fs::File::create(plaintext).with_context(|| format!("could not create {}", plaintext.display()))?;
    let mut counter = 0u32;
    let mut total = 0u64;
    loop {
        let mut head = [0u8; 5];
        let mut first = [0u8; 1];
        if f.read(&mut first)? == 0 {
            bail!("{} is truncated (it ended before its final chunk)", sealed.display());
        }
        head[0] = first[0];
        f.read_exact(&mut head[1..])
            .with_context(|| format!("{} is truncated mid-chunk", sealed.display()))?;
        let flags = head[0];
        if flags & !FINAL_FLAG != 0 {
            bail!("{} has a chunk with unknown flags", sealed.display());
        }
        let len = u32::from_le_bytes([head[1], head[2], head[3], head[4]]) as usize;
        if len > chunk {
            bail!(
                "{} has a chunk of {len} bytes, larger than its {chunk}-byte chunk size",
                sealed.display()
            );
        }
        let mut ciphertext = vec![0u8; len + TAG_LEN];
        f.read_exact(&mut ciphertext)
            .with_context(|| format!("{} is truncated mid-chunk", sealed.display()))?;
        let nonce = aead::Nonce::assume_unique_for_key(chunk_nonce(&nonce_prefix, counter));
        let len_bytes = [head[1], head[2], head[3], head[4]];
        let aad = record_aad(&header, flags, len_bytes);
        let mut in_out = ciphertext;
        let plain = aead
            .open_in_place(nonce, aead::Aad::from(&aad), &mut in_out)
            .map_err(|_| anyhow::anyhow!("{} failed to open at chunk {counter} (corrupt or tampered)", sealed.display()))?;
        out.write_all(plain)?;
        total += plain.len() as u64;
        counter += 1;
        if flags & FINAL_FLAG != 0 {
            let mut probe = [0u8; 1];
            if f.read(&mut probe)? != 0 {
                bail!("{} has trailing bytes after its final chunk", sealed.display());
            }
            break;
        }
    }
    out.flush()?;
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::short_id;

    const SESSION: &str = "c1";

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("colonizer-snap-{label}-{}", short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn key() -> SnapshotKey {
        SnapshotKey::generate().unwrap()
    }

    fn seal_to(dir: &Path, k: &SnapshotKey, plaintext: &Path, label: &str, chunk: usize) -> PathBuf {
        let sealed = dir.join(format!("{label}.{SEALED_EXT}"));
        seal_file(k, plaintext, &sealed, chunk).unwrap();
        sealed
    }

    /// Seals `content` and opens it again, asserting the bytes come back.
    fn roundtrip(k: &SnapshotKey, dir: &Path, content: &[u8]) {
        let plaintext = dir.join("image");
        std::fs::write(&plaintext, content).unwrap();
        let sealed = seal_to(dir, k, &plaintext, "image", 64);
        let opened = dir.join("opened");
        assert_eq!(open_file(k, &sealed, &opened).unwrap(), content.len() as u64);
        assert_eq!(std::fs::read(&opened).unwrap(), content);
    }

    fn files_under(root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                if entry.path().is_dir() {
                    stack.push(entry.path());
                } else {
                    out.push(entry.path());
                }
            }
        }
        out
    }
    /// An app whose `SESSION` holds a sealed snapshot of `bytes`, plus the root to remove after.
    fn sealed_app(label: &str, bytes: &[u8]) -> (PathBuf, crate::Shared, SnapshotMeta) {
        let root = scratch(label);
        let app = crate::tests::test_app(&root);
        let image = snapshot_image(&app, SESSION, "colonizer-c1");
        std::fs::create_dir_all(image.parent().unwrap()).unwrap();
        std::fs::write(&image, bytes).unwrap();
        let meta = create(&app, SESSION, "colonizer-c1", &image).unwrap();
        (root, app, meta)
    }

    #[test]
    fn a_multi_chunk_round_trip_returns_the_same_bytes() {
        let dir = scratch("roundtrip");
        let k = key();
        // The key round-trips through its hex form and rejects junk.
        let hex = k.to_hex();
        assert_eq!(hex.len(), KEY_LEN * 2);
        assert_eq!(SnapshotKey::from_hex(&hex).unwrap().to_hex(), hex);
        assert!(SnapshotKey::from_hex("not hex").is_err());
        assert!(SnapshotKey::from_hex("abcd").is_err(), "wrong length");
        // Spans several chunks and ends on a short one; an empty image gets one empty final chunk.
        roundtrip(&k, &dir, &(0..(64 * 3 + 17)).map(|i| (i % 251) as u8).collect::<Vec<_>>());
        roundtrip(&k, &dir, b"");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_damaged_missing_or_unkeyed_snapshot_never_restores() {
        let dir = scratch("damaged");
        let plaintext = dir.join("image");
        std::fs::write(&plaintext, vec![3u8; 64 * 2]).unwrap();
        let k = key();
        let base = std::fs::read(seal_to(&dir, &k, &plaintext, "base", 64)).unwrap();
        let opened = dir.join("opened");
        let mut flipped = base.clone();
        let last = flipped.len() - 1;
        flipped[last] ^= 0x40;
        let mut trailing = base.clone();
        trailing.push(0);
        // Cut on a record boundary before the last chunk, so the final flag is missing.
        let boundary = base.len() - (64 + TAG_LEN) - 5;
        let cases: [(&str, &[u8]); 4] = [
            ("flip", &flipped),
            ("midchunk", &base[..base.len() - 3]),
            ("trailing", &trailing),
            ("boundary", &base[..boundary]),
        ];
        for (label, bytes) in cases {
            let sealed = dir.join(format!("{label}.{SEALED_EXT}"));
            std::fs::write(&sealed, bytes).unwrap();
            assert!(open_file(&k, &sealed, &opened).is_err(), "{label} must not open");
        }
        // A different key.
        let sealed = seal_to(&dir, &k, &plaintext, "wrongkey", 64);
        assert!(open_file(&key(), &sealed, &opened).is_err(), "the wrong key must not open");
        let _ = std::fs::remove_dir_all(dir);

        // The restore layer: staging is cleaned up, and a missing sealed file or key is an Err.
        let (root, app, meta) = sealed_app("restore", &[1u8; 200]);
        let staging = restore(&app, SESSION, &meta).unwrap();
        assert!(staging.exists());
        remove_staging(&app, SESSION, &meta.name);
        assert!(!staging.exists(), "the staging plaintext is removed");
        std::fs::remove_file(sealed_path(&app, SESSION, &meta.name)).unwrap();
        assert!(restore(&app, SESSION, &meta).is_err(), "missing sealed file");
        let image = snapshot_image(&app, SESSION, &meta.name);
        std::fs::write(&image, [2u8; 200]).unwrap();
        create(&app, SESSION, &meta.name, &image).unwrap();
        std::fs::remove_file(app.snapshot_key_file(SESSION)).unwrap();
        assert!(restore(&app, SESSION, &meta).is_err(), "missing key");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn no_plaintext_is_left_on_disk_and_the_key_is_not_in_the_snapshot_dir() {
        let content = b"distinctive-colony-memory-9f3a2b7c-the-quick-brown-fox-jumps-over-the-lazy-dog";
        let (root, app, meta) = sealed_app("noplaintext", content);
        assert_eq!(meta.bytes, content.len() as u64);
        assert!(!snapshot_image(&app, SESSION, &meta.name).exists(), "plaintext image left");
        let snapshot_dir = app.snapshot_dir(SESSION);
        for file in files_under(&snapshot_dir) {
            let bytes = std::fs::read(&file).unwrap();
            let hit = bytes.windows(content.len()).any(|w| w == content);
            assert!(!hit, "{} holds plaintext", file.display());
        }
        let key_file = app.snapshot_key_file(SESSION);
        assert!(key_file.exists());
        assert!(!key_file.starts_with(&snapshot_dir), "the key sits in the snapshot dir");
        // Removing the colony's snapshot takes the sealed dir and the key with it.
        remove_files(&app, SESSION);
        assert!(!snapshot_dir.exists() && !key_file.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_failed_capture_leaves_no_plaintext_or_partial_file() {
        let root = scratch("failclean");
        let app = crate::tests::test_app(&root);
        let image = snapshot_image(&app, SESSION, "colonizer-c1");
        std::fs::create_dir_all(image.parent().unwrap()).unwrap();
        // Over the cap.
        std::fs::write(&image, b"more than four bytes").unwrap();
        assert!(seal_image(&app, SESSION, "colonizer-c1", &image, 4).is_err());
        assert!(!image.exists(), "the plaintext survived a failed capture");
        // A seal that gets as far as a .part temp: the rename target is a directory.
        let image2 = snapshot_image(&app, SESSION, "colonizer-c2");
        std::fs::write(&image2, b"more bytes").unwrap();
        std::fs::create_dir_all(sealed_path(&app, SESSION, "colonizer-c2")).unwrap();
        assert!(create(&app, SESSION, "colonizer-c2", &image2).is_err());
        assert!(!image2.exists(), "the plaintext survived a seal failure");
        let part = sealed_path(&app, SESSION, "colonizer-c2").with_extension("part");
        assert!(!part.exists(), "the .part temp survived");
        let _ = std::fs::remove_dir_all(root);
    }

    fn sealed_meta(sealed_at: DateTime<Utc>, bytes: u64) -> SnapshotMeta {
        SnapshotMeta {
            name: "c1".into(),
            bytes,
            sealed_at,
        }
    }

    #[test]
    fn decide_resume_falls_back_for_every_missing_piece_or_failure() {
        let now = Utc::now();
        let fresh = sealed_meta(now, 512 * 1024 * 1024);
        let stale = sealed_meta(now - TTL, 512 * 1024 * 1024);
        let huge = sealed_meta(now, MAX_RESIDENT_BYTES + 1);
        let edge = sealed_meta(now - TTL + chrono::Duration::seconds(1), MAX_RESIDENT_BYTES);
        let ok: Option<Result<(), String>> = Some(Ok(()));
        let cases = [
            ("present", Some(&fresh), true, Some(Ok(())), ResumePath::MemorySnapshot),
            ("no snapshot", None, true, ok.clone(), ResumePath::SessionResume),
            ("no key", Some(&fresh), false, ok.clone(), ResumePath::SessionResume),
            ("not attempted", Some(&fresh), true, None, ResumePath::SessionResume),
            (
                "corrupt",
                Some(&fresh),
                true,
                Some(Err("x".into())),
                ResumePath::SessionResume,
            ),
            ("expired", Some(&stale), true, ok.clone(), ResumePath::SessionResume),
            ("over cap", Some(&huge), true, ok.clone(), ResumePath::SessionResume),
            ("at the edge", Some(&edge), true, ok.clone(), ResumePath::MemorySnapshot),
        ];
        for (label, snap, keyed, restore, want) in cases {
            assert_eq!(decide_resume(now, snap, keyed, restore), want, "{label}");
        }
    }

    #[tokio::test]
    async fn rotating_the_gateway_token_revokes_the_old_one() {
        let root = scratch("rotate");
        let app = crate::tests::test_app(&root);
        std::fs::create_dir_all(app.session_dir(SESSION)).unwrap();
        let old = crate::util::random_token();
        write_private(&app.gateway_token_file(SESSION), old.as_bytes()).unwrap();
        let new = rotate_gateway_token(&app, SESSION).await.unwrap();
        assert_ne!(new, old);
        // The gateway reads this file per request (gateway::colony_for_token), so the file holding
        // only the new token is what revokes the old.
        let on_disk = std::fs::read_to_string(app.gateway_token_file(SESSION)).unwrap();
        assert_eq!(on_disk.trim(), new);
        assert_ne!(on_disk.trim(), old);
        let _ = std::fs::remove_dir_all(root);
    }
}
