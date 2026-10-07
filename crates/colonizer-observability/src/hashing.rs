//! The per-install key behind `repo_names = hashed` (P9 in docs/design/observability.md).
//!
//! `<data>/observability/hash.key` holds 32 random bytes, mode 0600 (a looser file is narrowed to
//! 0600 when loaded), created on first use and kept afterwards, so a repository hashes the same across restarts — and across a fleet whose members
//! were given the same key file, which is what makes the hash joinable. A name hashes to
//! HMAC-SHA256(key, name), first 12 bytes, as 24 lowercase hex characters.

use ring::hmac;
use ring::rand::{SecureRandom, SystemRandom};
use std::fmt;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// The key's length in bytes.
pub const KEY_BYTES: usize = 32;
/// A hash's length in bytes, before hex: 24 hex characters.
pub const HASH_BYTES: usize = 12;

/// The loaded key. Its bytes never leave this type, and its `Debug` does not print them.
#[derive(Clone)]
pub struct HashKey {
    key: hmac::Key,
}

impl fmt::Debug for HashKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HashKey(..)")
    }
}

/// Where the key lives under a data dir.
pub fn key_path(data_dir: &Path) -> PathBuf {
    data_dir.join("observability").join("hash.key")
}

impl HashKey {
    /// The key at `<data_dir>/observability/hash.key`, created (with its directory) when missing.
    /// Two processes creating it at once agree: one writes it, the other reads what was written.
    /// A key file of the wrong length is an error, never silently replaced, since replacing it
    /// would change every hash a backend already holds.
    pub fn load_or_create(data_dir: &Path) -> io::Result<HashKey> {
        let path = key_path(data_dir);
        match read_key(&path) {
            Ok(key) => return Ok(key),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut bytes = [0u8; KEY_BYTES];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| io::Error::other("no system randomness for the hash key"))?;
        // Written whole to a private file of its own, then linked into place: the link fails if the
        // key already exists, so a key is never clobbered, and never seen half-written.
        let mut nonce = [0u8; 8];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| io::Error::other("no system randomness for the hash key"))?;
        let staging = path.with_file_name(format!("hash.key.{}.tmp", hex(&nonce)));
        let written = create_new(&staging).and_then(|mut file| {
            file.write_all(&bytes)?;
            file.sync_all()
        });
        let linked = written.and_then(|()| std::fs::hard_link(&staging, &path));
        let _ = std::fs::remove_file(&staging);
        match linked {
            Ok(()) => Ok(HashKey::from_bytes(&bytes)),
            // Someone else created it between the read and here: theirs is the key.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => read_key(&path),
            Err(e) => Err(e),
        }
    }

    /// A key from raw bytes: a fleet's copied key, or a test's.
    pub fn from_bytes(bytes: &[u8; KEY_BYTES]) -> HashKey {
        HashKey {
            key: hmac::Key::new(hmac::HMAC_SHA256, bytes),
        }
    }

    /// `name` hashed: HMAC-SHA256 under the key, first 12 bytes, lowercase hex.
    pub fn hash_name(&self, name: &str) -> String {
        hex(&hmac::sign(&self.key, name.as_bytes()).as_ref()[..HASH_BYTES])
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn read_key(path: &Path) -> io::Result<HashKey> {
    let mut file = std::fs::File::open(path)?;
    owner_only(&file, path)?;
    let mut bytes = Vec::with_capacity(KEY_BYTES + 1);
    // One byte past the key's length is enough to tell a long file from a right one.
    (&mut file).take(KEY_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    let bytes: [u8; KEY_BYTES] = bytes.as_slice().try_into().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} is not a {KEY_BYTES}-byte key; restore the right file or remove it",
                path.display()
            ),
        )
    })?;
    Ok(HashKey::from_bytes(&bytes))
}

/// A key file others can read or write (a fleet copy made at 0644, say) is narrowed to 0600 before
/// it is used; one that cannot be narrowed is an error rather than a key in use at the wrong mode.
#[cfg(unix)]
fn owner_only(file: &std::fs::File, path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = file.metadata()?.permissions().mode();
    if mode & 0o077 == 0 {
        return Ok(());
    }
    file.set_permissions(std::fs::Permissions::from_mode(0o600)).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!(
                "{} is mode {:o} and could not be made 0600: {e}",
                path.display(),
                mode & 0o777
            ),
        )
    })
}

#[cfg(not(unix))]
fn owner_only(_file: &std::fs::File, _path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn create_new(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn create_new(path: &Path) -> io::Result<std::fs::File> {
    std::fs::OpenOptions::new().write(true).create_new(true).open(path)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use tests::temp_dir;
