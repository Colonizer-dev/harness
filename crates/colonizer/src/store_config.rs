//! Which session store this install runs on, and `colonizer sessions migrate`, which moves every
//! colony into another store and switches to it (docs/session-store.md, "Configuration" and
//! "Migration and rollback").
//!
//! The choice is one small file, `<config_dir>/session-store.json`. No file is the default: the
//! local disk under the data dir, exactly where every earlier release kept its colonies, so an
//! existing install reads unchanged. The migration command is the one writer of the file, and only
//! after a copy that verified itself.

use crate::config::Settings;
use crate::store::{self, LocalDirStore, MigrationReport, SessionStore};
use crate::store_s3::{Credentials, MirroredStore, S3Config, S3Store};
use crate::util;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The file under the config dir that names the store.
pub(crate) const CONFIG_FILE: &str = "session-store.json";

/// A session store backend, as `session-store.json` records it. Whatever the backend, the data
/// dir keeps a local working copy of every session — a microVM mounts its session's `vm/` and
/// `out/` from a host path — so the backend decides where the records are kept, not where a
/// colony runs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "backend", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Backend {
    /// The local disk under the data dir: `sessions.json` and `sessions/<id>/`, the layout every
    /// release has written.
    #[default]
    Local,
    /// An S3-compatible bucket (R2, MinIO, AWS S3), mirrored through the data dir's working copy
    /// (`store_s3::MirroredStore`). Credentials come from the secrets mechanism, never this file.
    S3(S3Config),
}

impl Backend {
    /// Opens the backend the way a mothership runs on it: a bucket through the mirror over the
    /// data dir's working copy (hydrated from the bucket when the working copy has no index), with
    /// its uploader started.
    pub(crate) async fn open(&self, cfg: &Settings) -> Result<Arc<dyn SessionStore>> {
        match self {
            Backend::Local => Ok(Arc::new(LocalDirStore::new(cfg.data_dir.clone()))),
            Backend::S3(_) => {
                let remote = self.open_direct(cfg).await?;
                let mirror = MirroredStore::open(cfg.data_dir.clone(), remote)
                    .await
                    .with_context(|| format!("could not open {}", self.describe(&cfg.data_dir)))?;
                mirror.spawn_uploader();
                Ok(mirror)
            }
        }
    }

    /// Opens the backend's store of record itself, with no working copy: what a migration copies
    /// into and out of.
    pub(crate) async fn open_direct(&self, cfg: &Settings) -> Result<Arc<dyn SessionStore>> {
        match self {
            Backend::Local => Ok(Arc::new(LocalDirStore::new(cfg.data_dir.clone()))),
            Backend::S3(config) => {
                let creds = Credentials::resolve(&cfg.config_dir)?;
                Ok(Arc::new(S3Store::new(config.clone(), creds)?))
            }
        }
    }

    /// One line naming the store, for messages.
    pub(crate) fn describe(&self, data_dir: &Path) -> String {
        match self {
            Backend::Local => format!("the local store in {}", data_dir.display()),
            Backend::S3(config) => format!("the bucket {}", config.describe()),
        }
    }
}

/// A store as `--to` and `--from` name one: a backend this install can be switched to, or a plain
/// directory laid out like the local store (a copy to another disk, a backup, a second install's
/// data dir).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StoreRef {
    Backend(Backend),
    Dir(PathBuf),
}

impl StoreRef {
    /// Parses `local` (this install's local store), `local:<dir>`, a bare directory path (anything
    /// with a `/`, or starting with `.` or `~`), or a bucket,
    /// `s3://<bucket>[/<prefix>]?endpoint=<url>[&region=<region>]`.
    pub(crate) fn parse(spec: &str) -> Result<StoreRef> {
        let spec = spec.trim();
        if spec.starts_with("s3://") {
            return Ok(StoreRef::Backend(Backend::S3(S3Config::parse(spec)?)));
        }
        if spec == "local" {
            return Ok(StoreRef::Backend(Backend::Local));
        }
        if let Some(dir) = spec.strip_prefix("local:") {
            if dir.is_empty() {
                bail!("`local:` needs a directory, as in local:/srv/colonizer-copy");
            }
            return Ok(StoreRef::Dir(expand_home(dir)));
        }
        if spec.contains('/') || spec.starts_with('.') || spec.starts_with('~') {
            return Ok(StoreRef::Dir(expand_home(spec)));
        }
        bail!(
            "unknown session store {spec:?}: use local, local:<dir>, a directory path, or s3://<bucket>/<prefix>?endpoint=<url>"
        )
    }

    async fn open(&self, cfg: &Settings) -> Result<Arc<dyn SessionStore>> {
        match self {
            StoreRef::Backend(backend) => backend.open_direct(cfg).await,
            StoreRef::Dir(dir) => Ok(Arc::new(LocalDirStore::new(dir.clone()))),
        }
    }

    fn describe(&self, data_dir: &Path) -> String {
        match self {
            StoreRef::Backend(backend) => backend.describe(data_dir),
            StoreRef::Dir(dir) => format!("the directory {}", dir.display()),
        }
    }

    /// The local directory this store keeps its files in, when it is one.
    fn local_dir<'a>(&'a self, data_dir: &'a Path) -> Option<&'a Path> {
        match self {
            StoreRef::Backend(Backend::Local) => Some(data_dir),
            StoreRef::Dir(dir) => Some(dir),
            StoreRef::Backend(Backend::S3(_)) => None,
        }
    }

    /// Whether two references name the same store (a local one by its canonical directory).
    fn same_store(&self, other: &StoreRef, data_dir: &Path) -> bool {
        match (self.local_dir(data_dir), other.local_dir(data_dir)) {
            (Some(a), Some(b)) => same_dir(a, b),
            _ => self == other,
        }
    }
}

/// `~/x` against `$HOME`; anything else as given.
fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(path),
    }
}

/// Whether two paths name the same directory. Canonicalized when both exist, so `./data`, an
/// absolute path and a symlink to the one directory all compare equal; the raw paths otherwise,
/// which is a destination the migration is about to create (it cannot canonicalize what is not
/// there).
pub(crate) fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// The configured backend: the file's, or the default local store when there is none. A file that
/// is there but unreadable or malformed is an error, never a silent fall back to the local disk —
/// that would start a mothership on an empty colony list while its colonies sit elsewhere.
pub(crate) fn load(config_dir: &Path) -> Result<Backend> {
    let path = config_dir.join(CONFIG_FILE);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).with_context(|| {
            format!(
                "{} does not name a session store this build understands; fix it, or remove it to use the local disk",
                path.display()
            )
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Backend::default()),
        Err(e) => Err(e).with_context(|| format!("could not read {}", path.display())),
    }
}

/// Records `backend` as this install's store, atomically.
pub(crate) async fn save(config_dir: &Path, backend: &Backend) -> Result<()> {
    tokio::fs::create_dir_all(config_dir).await?;
    let mut bytes = serde_json::to_vec_pretty(backend)?;
    bytes.push(b'\n');
    util::write_atomic(&config_dir.join(CONFIG_FILE), &bytes).await
}

/// Refuses to copy a store a running mothership is writing: a listener on the mothership's port is
/// the tell. The port could be held by something else, so the guard only warns off the likely
/// case. Bound, then dropped at once, so nothing listens on the strength of this check.
pub(crate) async fn refuse_while_serving(cfg: &Settings) -> Result<()> {
    match tokio::net::TcpListener::bind(&cfg.bind).await {
        Ok(listener) => drop(listener),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => bail!(
            "something is listening on {}, most likely a running mothership: stop it before migrating its \
             store, or the colonies it is writing would be lost from the copy — then run this again.",
            cfg.bind
        ),
        // Bound for some other reason (the bind is malformed, or the port is momentarily taken):
        // not evidence of a mothership, so not this command's guard to raise.
        Err(_) => {}
    }
    Ok(())
}

/// `colonizer sessions migrate --to <store> [--from <store>] [--dry-run]`: copies every colony from
/// the configured store (or `--from`) into `--to` and verifies the copy. Then, only when the source
/// was the configured store, the copy proved itself and `--to` is a backend, it records `--to` in
/// `session-store.json`, so the next mothership start runs on it. A directory target is a copy, not
/// a switch: the switch to a directory is `COLONIZER_DATA_DIR`, and the command says so. The source
/// is never written, so rollback is restoring the previous setting.
pub(crate) async fn migrate_command(cfg: &Settings, from: Option<&str>, to: &str, dry_run: bool, json: bool) -> Result<()> {
    let configured = StoreRef::Backend(load(&cfg.config_dir)?);
    let source = match from {
        Some(spec) => StoreRef::parse(spec)?,
        None => configured.clone(),
    };
    let target = StoreRef::parse(to)?;
    let (from_text, to_text) = (source.describe(&cfg.data_dir), target.describe(&cfg.data_dir));
    if source.same_store(&target, &cfg.data_dir) {
        bail!("--from and --to name the same store ({to_text}); there is nothing to copy");
    }
    let from_configured = source.same_store(&configured, &cfg.data_dir);
    // A migration only ever reads its source, so what must not happen is a running mothership
    // writing that source while it is copied — the single-writer rule.
    if from_configured {
        refuse_while_serving(cfg).await?;
    }
    let (src, dst) = (source.open(cfg).await?, target.open(cfg).await?);
    let report = store::migrate(src.as_ref(), dst.as_ref(), dry_run).await?;
    // Only after the copy verified itself (`migrate` errs otherwise), and never on a dry run.
    let switched = match &target {
        StoreRef::Backend(backend) if from_configured && !dry_run => {
            save(&cfg.config_dir, backend).await?;
            true
        }
        _ => false,
    };
    if json {
        let mut value = serde_json::to_value(&report)?;
        value["dry_run"] = dry_run.into();
        value["switched"] = switched.into();
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }
    println!("{}", summary(&report, dry_run, &from_text, &to_text));
    if dry_run {
        return Ok(());
    }
    match &target {
        _ if switched => println!(
            "this install now uses {to_text} ({} updated); the old store is untouched — roll back by restoring the previous setting",
            cfg.config_dir.join(CONFIG_FILE).display()
        ),
        StoreRef::Dir(dir) => println!(
            "to run on the copy, point the mothership at it with COLONIZER_DATA_DIR={} (the old store is untouched)",
            dir.display()
        ),
        StoreRef::Backend(_) => println!("the configured store was not the source, so the setting was left as it is"),
    }
    Ok(())
}

/// The one-line account of a migration, dry or real.
pub(crate) fn summary(report: &MigrationReport, dry_run: bool, from: &str, to: &str) -> String {
    let verb = if dry_run { "would copy" } else { "copied" };
    let mut line = format!(
        "{verb} {} colonies, {} of {} files ({} of {}) from {from} to {to}",
        report.sessions,
        report.copied,
        report.files,
        util::format_disk_size(report.copied_bytes),
        util::format_disk_size(report.bytes),
    );
    if report.skipped > 0 {
        line.push_str(&format!("; {} already there", report.skipped));
    }
    if report.removed > 0 {
        let verb = if dry_run { "to remove" } else { "removed" };
        line.push_str(&format!("; {} stale destination files {verb}", report.removed));
    }
    if report.already_done {
        line.push_str("; an earlier run had finished, and this one re-verified it");
    }
    line.push_str(&format!("; checksum {}", report.checksum));
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SessionStore;

    fn temp(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("colonizer-store-config-{tag}-{}", util::short_id()))
    }

    /// Settings for a local command: a data dir and a config dir of the test's own, and a bind no
    /// mothership holds.
    fn settings(root: &Path) -> Settings {
        Settings {
            bind: "127.0.0.1:0".into(),
            data_dir: root.join("data"),
            config_dir: root.join("config"),
            runtime_dir: PathBuf::new(),
            assets: None,
            msb: "msb".into(),
            claude_bin: None,
            gateway_bind: "127.0.0.1:0".parse().unwrap(),
            allowed_hosts: Vec::new(),
            fleet_peers: Vec::new(),
            bench_pool: None,
        }
    }

    async fn seed(store: &dyn SessionStore) {
        store.write_file("a1b2c3d4", "events.jsonl", b"{\"seq\":1}\n").await.unwrap();
        store.write_file("a1b2c3d4", "out/pr.md", b"# Title\n").await.unwrap();
        store.write_index(br#"[{"id":"a1b2c3d4"}]"#).await.unwrap();
    }

    #[test]
    fn a_store_is_named_local_by_directory_or_by_path() {
        assert_eq!(StoreRef::parse("local").unwrap(), StoreRef::Backend(Backend::Local));
        assert_eq!(StoreRef::parse("local:/srv/copy").unwrap(), StoreRef::Dir("/srv/copy".into()));
        assert_eq!(StoreRef::parse("/srv/copy").unwrap(), StoreRef::Dir("/srv/copy".into()));
        assert_eq!(StoreRef::parse("./copy").unwrap(), StoreRef::Dir("./copy".into()));
        assert!(StoreRef::parse("local:").is_err());
        let err = StoreRef::parse("dropbox").unwrap_err().to_string();
        assert!(err.contains("unknown session store"), "{err}");
    }

    #[tokio::test]
    async fn no_setting_is_the_local_store_and_a_broken_one_is_refused() {
        let root = temp("load");
        let config = root.join("config");
        assert_eq!(load(&config).unwrap(), Backend::Local, "no file: the local store, as always");
        save(&config, &Backend::Local).await.unwrap();
        assert_eq!(load(&config).unwrap(), Backend::Local, "the saved setting reads back");
        let saved = std::fs::read_to_string(config.join(CONFIG_FILE)).unwrap();
        assert_eq!(saved, "{\n  \"backend\": \"local\"\n}\n");
        // A setting this build cannot read stops the mothership rather than starting it empty.
        std::fs::write(config.join(CONFIG_FILE), r#"{"backend":"tape"}"#).unwrap();
        let err = format!("{:#}", load(&config).unwrap_err());
        assert!(err.contains("does not name a session store"), "{err}");
        let _ = std::fs::remove_dir_all(root);
    }

    /// `colonizer sessions migrate` from the configured store into a directory: the dry run writes
    /// nothing, the real run copies and verifies, a re-run is a no-op that re-verifies, and a copy
    /// to a directory never touches the setting (the switch to a directory is COLONIZER_DATA_DIR).
    #[tokio::test]
    async fn sessions_migrate_dry_runs_copies_reruns_and_leaves_the_setting_for_a_directory() {
        let root = temp("cmd");
        let cfg = settings(&root);
        let source = LocalDirStore::new(cfg.data_dir.clone());
        seed(&source).await;
        let copy = root.join("copy");
        let to = format!("local:{}", copy.display());

        migrate_command(&cfg, None, &to, true, false).await.unwrap();
        let target = LocalDirStore::new(copy.clone());
        assert_eq!(target.read_index().await.unwrap(), None, "a dry run writes nothing");
        assert!(target.list_sessions().await.unwrap().is_empty());

        migrate_command(&cfg, None, &to, false, false).await.unwrap();
        assert_eq!(target.read_index().await.unwrap(), source.read_index().await.unwrap());
        assert_eq!(target.list_files("a1b2c3d4").await.unwrap(), ["events.jsonl", "out/pr.md"]);
        assert!(!cfg.config_dir.join(CONFIG_FILE).exists(), "a directory copy is not a switch");

        migrate_command(&cfg, None, &to, false, true).await.unwrap();

        // Into itself, by either spelling, is refused before anything is read.
        let err = migrate_command(&cfg, None, "local", false, false)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("the same store"), "{err}");
        let data = format!("local:{}", cfg.data_dir.display());
        let err = migrate_command(&cfg, None, &data, false, false)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("the same store"), "{err}");
        let _ = std::fs::remove_dir_all(root);
    }
}
