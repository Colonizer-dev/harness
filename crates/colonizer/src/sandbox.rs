//! Sandbox module: microsandbox provider.

use axum::extract::State;

use crate::util::{exec, exec_within, mount_spec};
use anyhow::{Context, Result, bail};
use std::{collections::HashSet, path::PathBuf, time::Duration};
use tokio::process::Command;

pub struct Mount {
    pub source: PathBuf,
    pub target: String,
    pub read_only: bool,
}

/// The file, inside the read-only `/colonizer` mount, listing the colony's writable binds: one
/// guest target per line, written by the boot from [`host_mounts_text`] and read by the guest's
/// exec policy (`modules/agents/*/execpolicy.mjs`, issue #877) to tell a write into the microVM's
/// own root filesystem — discarded with the VM — from one into a host-backed mount.
pub(crate) const HOST_MOUNTS_FILE: &str = "host-mounts";

/// The [`HOST_MOUNTS_FILE`] text: each writable mount's guest target on its own line, in mount
/// order, newline-terminated like the path policy's list.
pub(crate) fn host_mounts_text(mounts: &[Mount]) -> String {
    mounts
        .iter()
        .filter(|m| !m.read_only)
        .map(|m| format!("{}\n", m.target))
        .collect()
}

pub struct Secret {
    pub env: String,
    pub value: String,
    pub hosts: Vec<String>,
}

#[derive(Default)]
pub struct BootSpec {
    pub name: String,
    pub image: String,
    pub cpus: u64,
    pub memory: String,
    pub root_disk: String,
    pub max_duration: String,
    pub workdir: String,
    pub mounts: Vec<Mount>,
    pub env: Vec<(String, String)>,
    pub secrets: Vec<Secret>,
    pub net_profiles: Vec<String>,
    pub net_rules: Vec<String>,
    /// Allowlist mode (#303): pass no `--net` (no profile allow at all) and set
    /// `--net-default-egress deny`, which lands in msb as a default-deny egress policy whose only
    /// rules are `net_rules`. Ingress keeps the profile baseline (allow), so the mesh-off
    /// published port keeps working — `--net none` would deny it too.
    pub net_deny_egress: bool,
    /// `(host_port, guest_port)` published on 127.0.0.1.
    pub publish: Option<(u16, u16)>,
    pub command: Vec<String>,
}

/// Boots a detached microVM running `spec.command` as its main process.
pub async fn boot(msb: &str, spec: &BootSpec) -> Result<()> {
    let mut cmd = Command::new(msb);
    cmd.args(["run", "--detach", "--replace", "--quiet", "--name", spec.name.as_str()])
        .arg("--cpus")
        .arg(spec.cpus.to_string())
        .args(["--memory", spec.memory.as_str()])
        .args(["--root-disk", spec.root_disk.as_str()])
        .args(["--max-duration", spec.max_duration.as_str()])
        .args(["--workdir", spec.workdir.as_str()]);
    for mount in &spec.mounts {
        cmd.arg("-v").arg(mount_spec(&mount.source, &mount.target, mount.read_only)?);
    }
    for (key, value) in &spec.env {
        cmd.arg("-e").arg(format!("{key}={value}"));
    }
    for secret in &spec.secrets {
        // The value stays in msb's host process; the guest env only holds a placeholder.
        cmd.arg("--secret")
            .arg(format!("{}@{}", secret.env, secret.hosts.join(",")))
            .env(&secret.env, &secret.value);
    }
    if !spec.net_profiles.is_empty() {
        cmd.arg("--net").arg(spec.net_profiles.join(","));
    }
    if spec.net_deny_egress {
        cmd.args(["--net-default-egress", "deny"]);
    }
    for rule in &spec.net_rules {
        cmd.arg("--net-rule").arg(rule);
    }
    if let Some((host, guest)) = spec.publish {
        cmd.arg("-p").arg(format!("127.0.0.1:{host}:{guest}"));
    }
    cmd.arg(&spec.image).arg("--").args(&spec.command);
    exec(&mut cmd)
        .await
        .with_context(|| format!("microVM {} failed to boot", spec.name))?;
    Ok(())
}

pub async fn remove(msb: &str, name: &str) {
    let _ = exec(Command::new(msb).args(["rm", "--force", "--quiet", name])).await;
}

/// How long a confirmed removal's `msb rm` and `msb ls` may each take: long enough for a forced
/// removal of a real microVM, bounded so a wedged `msb` cannot hold a caller on it for good.
pub(crate) const REMOVE_CONFIRM_TIMEOUT: Duration = Duration::from_secs(60);

/// Removes a sandbox and answers whether it is really gone. Nothing here takes `msb rm`'s word for
/// it: after the removal (run under `limit`, `--force`, exactly like [`remove`]) the sandbox counts
/// as gone only when a full `msb ls --quiet` ([`all`], running or not) no longer lists it — a
/// listed-but-stopped sandbox is not gone. A failed or slow removal is not a verdict either way,
/// since it may have removed the sandbox anyway (or found it already gone); the listing is the one
/// verdict. Everything else — a listing that fails or times out, the state then being unknown, or
/// the name still listed afterwards — is an error, and the caller must treat the microVM as
/// possibly still there.
pub async fn remove_confirmed(msb: &str, name: &str, limit: Duration) -> Result<()> {
    // A timeout drops the future, which is what fires `exec`'s `kill_on_drop` — a wedged `msb rm`
    // is killed rather than left running.
    let removal = exec_within(limit, Command::new(msb).args(["rm", "--force", "--quiet", name])).await;
    let listed = tokio::time::timeout(limit, all(msb))
        .await
        .map_err(|_| anyhow::anyhow!("`msb ls --quiet` timed out after {limit:?}, so {name} could not be confirmed removed"))?
        .with_context(|| format!("could not list the sandboxes to confirm {name} was removed"))?;
    if listed.contains(name) {
        match removal {
            Ok(_) => bail!("microVM {name} is still listed after `msb rm --force --quiet` ran, so its removal is not confirmed"),
            Err(e) => bail!("microVM {name} is still listed and `msb rm --force --quiet` failed: {e:#}"),
        }
    }
    Ok(())
}

/// Whether this sandbox provider can snapshot a running microVM's memory, so a colony waiting on its
/// user could be frozen in place and thawed with the conversation and every open process intact
/// (issue #562, re-measured on the 0.7.3 pin of issue #639). The capture now exists — `msb snapshot
/// create --full` checkpoints a running VM in about half a second — but its restore cannot bring a
/// colony back, so the answer is still no: a sandbox that has ever carried a `--secret` (every
/// colony's credential rides in one) fails its restore outright (`restore virtio device
/// virtio_fs1`), whether or not the source sandbox still runs, and `msb snapshot restore` accepts
/// no `--secret` that could re-register one. A suspend therefore stops the VM and keeps the
/// worktree plus the agent's own session transcript, and the answer re-boots a fresh VM that
/// resumes that session. If upstream ever restores a colony-shaped sandbox, this is the seam that
/// switches.
pub(crate) fn supports_memory_snapshot() -> bool {
    false
}

/// Runs a one-shot command in a fresh microVM and answers its exit code: [`boot`] without
/// `--detach`, so `msb run` stays attached, the VM lives for the command, and the CLI exits with
/// the command's own code. The VM is removed either way, so nothing outlives the call.
pub async fn run_once(msb: &str, spec: &BootSpec) -> Result<i32> {
    let mut cmd = Command::new(msb);
    cmd.args(["run", "--replace", "--quiet", "--name", spec.name.as_str()])
        .arg("--cpus")
        .arg(spec.cpus.to_string())
        .args(["--memory", spec.memory.as_str()])
        .args(["--root-disk", spec.root_disk.as_str()])
        .args(["--max-duration", spec.max_duration.as_str()])
        .args(["--workdir", spec.workdir.as_str()]);
    for mount in &spec.mounts {
        cmd.arg("-v").arg(mount_spec(&mount.source, &mount.target, mount.read_only)?);
    }
    for (key, value) in &spec.env {
        cmd.arg("-e").arg(format!("{key}={value}"));
    }
    for secret in &spec.secrets {
        // The value stays in msb's host process; the guest env only holds a placeholder.
        cmd.arg("--secret")
            .arg(format!("{}@{}", secret.env, secret.hosts.join(",")))
            .env(&secret.env, &secret.value);
    }
    if !spec.net_profiles.is_empty() {
        cmd.arg("--net").arg(spec.net_profiles.join(","));
    }
    if spec.net_deny_egress {
        cmd.args(["--net-default-egress", "deny"]);
    }
    for rule in &spec.net_rules {
        cmd.arg("--net-rule").arg(rule);
    }
    if let Some((host, guest)) = spec.publish {
        cmd.arg("-p").arg(format!("127.0.0.1:{host}:{guest}"));
    }
    cmd.arg(&spec.image).arg("--").args(&spec.command);
    let status = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .status()
        .await
        .with_context(|| format!("microVM {} failed to run", spec.name))?;
    remove(msb, &spec.name).await;
    // A process killed by a signal has no code; 127 (command not found) is the safer reading —
    // infra noise reads unverifiable rather than failed.
    Ok(status.code().unwrap_or(127))
}

pub async fn running(msb: &str) -> Result<HashSet<String>> {
    let out = exec(Command::new(msb).args(["ls", "--running", "--quiet"])).await?;
    Ok(out.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
}

/// Every sandbox microsandbox knows about, running or not: the orphan-VM
/// sweep diffs this against the session list and the running set above.
pub async fn all(msb: &str) -> Result<HashSet<String>> {
    let out = exec(Command::new(msb).args(["ls", "--quiet"])).await?;
    Ok(out.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
}

/// Images already in the local cache, as `msb image list` reports them.
///
/// Used to decide whether a launch is about to pay for a download. A failure
/// here is not fatal anywhere it is used: the worst case is pulling an image
/// that was already cached, which `msb pull` turns into a no-op.
pub async fn cached_images(msb: &str) -> Result<HashSet<String>> {
    let out = exec(Command::new(msb).args(["image", "list"])).await?;
    Ok(out
        .lines()
        .skip(1) // header
        .filter_map(|line| line.split_whitespace().next())
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect())
}

/// A bare `name` is cached as `name:latest`, so a lookup has to try both.
fn matches_cache(image: &str, cached: &HashSet<String>) -> bool {
    if cached.contains(image) {
        return true;
    }
    if image.contains(':') {
        false
    } else {
        cached.contains(&format!("{image}:latest"))
    }
}

pub async fn is_cached(msb: &str, image: &str) -> bool {
    // A cache check that fails is reported as "not cached": pulling an image
    // that was already there is a no-op, but skipping a pull that was needed
    // puts the download back on the launch path unannounced.
    match cached_images(msb).await {
        Ok(images) => matches_cache(image, &images),
        Err(_) => false,
    }
}

/// Downloads an image into the local cache. A no-op when it is already there.
pub async fn pull(msb: &str, image: &str) -> Result<()> {
    exec(Command::new(msb).args(["pull", "--quiet", image])).await?;
    Ok(())
}

/// Where a background image pull has got to.
///
/// There is no percentage here on purpose. `msb pull` draws its progress bar
/// only on a terminal; piped, it prints a single line when it is finished, and
/// `--info` adds nothing but migration logs. Scraping the bar through a pty
/// would parse an undocumented format that can change with any msb release, so
/// the UI shows what is actually known: which image, since when, and the
/// outcome.
#[derive(Clone, Debug, Default, serde::Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PullState {
    #[default]
    Idle,
    /// Already in the local cache; nothing to do.
    Cached,
    Pulling,
    Done,
    Failed,
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct PullStatus {
    pub image: String,
    pub state: PullState,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    pub error: Option<String>,
    /// Bumped for every pull started, so a slow pull that finishes after a newer
    /// one was requested cannot overwrite the newer one's status.
    #[serde(skip)]
    pub generation: u64,
}

/// The image the sandbox module is configured to boot, after the stack preset. The Setup pane's
/// pre-pull runs before any colony exists, so there is no repository to detect from and `auto`
/// resolves to its fallback here — the per-repository answer happens when a colony's worktree is
/// checked out, and by then this download has to be done or waiting on.
pub(crate) fn configured_image(app: &crate::App, modules: &crate::config::ModulesConfig) -> String {
    let schema = crate::modules::schema_for("sandbox", &modules.sandbox.provider, &app.agents);
    let preset = crate::config::setting_str(&modules.sandbox, &schema, "preset");
    let settings = crate::config::with_preset(&modules.sandbox, &crate::presets::defaults(crate::presets::resolved(&preset)));
    crate::config::setting_str(&settings, &schema, "image")
}

/// `POST /api/sandbox/pull` — start downloading the configured colony image.
///
/// Returns at once. A cold pull of the default image measured 108 s, which is
/// too long to hold a request open, so the download runs in the background and
/// `GET /api/sandbox/pull` reports on it. Calling this again while the same
/// image is already pulling returns the running pull rather than starting a
/// second one.
pub async fn pull_configured(State(app): State<crate::Shared>) -> crate::ApiResult<PullStatus> {
    let modules = app.modules.read().await.clone();
    let image = configured_image(&app, &modules);
    if image.is_empty() {
        return Err(crate::client_error(
            axum::http::StatusCode::BAD_REQUEST,
            "no colony image is configured",
        ));
    }

    {
        let status = app.pull.lock().await;
        if status.state == PullState::Pulling && status.image == image {
            return Ok(axum::Json(status.clone()));
        }
    }

    if is_cached(&app.cfg.msb, &image).await {
        let mut status = app.pull.lock().await;
        *status = PullStatus {
            image,
            state: PullState::Cached,
            generation: status.generation,
            ..Default::default()
        };
        return Ok(axum::Json(status.clone()));
    }

    let started = {
        let mut status = app.pull.lock().await;
        let generation = status.generation + 1;
        *status = PullStatus {
            image: image.clone(),
            state: PullState::Pulling,
            started_at: Some(chrono::Utc::now()),
            finished_at: None,
            error: None,
            generation,
        };
        status.clone()
    };

    let background = app.clone();
    tokio::spawn(async move {
        let result = pull(&background.cfg.msb, &image).await;
        let mut status = background.pull.lock().await;
        if status.generation != started.generation {
            return; // superseded by a pull for a different image
        }
        status.finished_at = Some(chrono::Utc::now());
        match result {
            Ok(()) => status.state = PullState::Done,
            Err(e) => {
                status.state = PullState::Failed;
                status.error = Some(crate::util::truncate(&format!("{e:#}"), 2000));
            }
        }
    });

    Ok(axum::Json(started))
}

/// What an `Idle` pull status becomes once the cache has been looked at: `Cached` for the configured
/// image when it is already there, otherwise nothing changes. The status lives in memory, so after
/// every restart it starts `Idle`, which says "unknown", not "absent" (issue #1200). Any other state
/// is a real verdict (a running, finished or failed pull) and is never overwritten here.
fn settle_idle(current: &PullStatus, image: &str, cached: bool) -> Option<PullStatus> {
    (current.state == PullState::Idle && cached && !image.is_empty()).then(|| PullStatus {
        image: image.to_string(),
        state: PullState::Cached,
        generation: current.generation,
        ..Default::default()
    })
}

/// Looks at the image cache when nothing has been pulled yet since the mothership started, so a
/// colony image that is already on disk reads `cached` without anybody clicking a stack.
pub(crate) async fn refresh_idle(app: &crate::Shared) {
    if app.pull.lock().await.state != PullState::Idle {
        return;
    }
    let modules = app.modules.read().await.clone();
    let image = configured_image(app, &modules);
    if image.is_empty() {
        return;
    }
    let cached = is_cached(&app.cfg.msb, &image).await; // not under the lock: it spawns msb
    let mut status = app.pull.lock().await;
    if let Some(settled) = settle_idle(&status, &image, cached) {
        *status = settled;
    }
}

/// Settles the pull status once at startup, so the first Setup poll already has the answer.
pub(crate) fn start_tasks(app: &crate::Shared) {
    let app = app.clone();
    tokio::spawn(async move { refresh_idle(&app).await });
}

/// `GET /api/sandbox/pull` — the status of the most recent pull.
pub async fn pull_status(State(app): State<crate::Shared>) -> axum::Json<PullStatus> {
    refresh_idle(&app).await;
    axum::Json(app.pull.lock().await.clone())
}

/// The API routes this module serves. `server::api_routes` merges them into the cockpit's router,
/// behind the activity log's route layer and `host_guard`.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new().route("/api/sandbox/pull", routing::post(pull_configured).get(pull_status))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_mounts_lists_only_the_writable_binds() {
        let mount = |target: &str, read_only: bool| Mount {
            source: PathBuf::from("/host/unused"),
            target: target.into(),
            read_only,
        };
        let mounts = vec![
            mount("/workspace", false),
            mount("/colonizer", true),
            mount("/harness/out", false),
            mount("/root/.claude/projects", false),
        ];
        assert_eq!(
            host_mounts_text(&mounts),
            "/workspace\n/harness/out\n/root/.claude/projects\n",
            "read-only mounts are the guest's own, not the host's to protect"
        );
        assert_eq!(host_mounts_text(&[]), "", "no mounts, no lines");
    }

    #[test]
    fn pull_states_serialise_as_the_ui_expects() {
        // web/src/types.ts spells these out as a string union. A rename here
        // without that file following would leave Settings stuck on a state it
        // does not recognise, with no compile error on either side.
        let spelled: Vec<String> = [
            PullState::Idle,
            PullState::Cached,
            PullState::Pulling,
            PullState::Done,
            PullState::Failed,
        ]
        .iter()
        .map(|s| serde_json::to_value(s).unwrap().as_str().unwrap().to_string())
        .collect();
        assert_eq!(spelled, ["idle", "cached", "pulling", "done", "failed"]);
    }

    #[test]
    fn an_idle_status_after_a_restart_reads_cached_when_the_image_is_on_disk() {
        // Issue #1200: the in-memory status is Idle after every restart; a cached image must not read as absent.
        let idle = PullStatus {
            generation: 3,
            ..Default::default()
        };
        let settled = settle_idle(&idle, "node:24-bookworm", true).expect("settles to cached");
        assert_eq!(settled.state, PullState::Cached);
        assert_eq!(settled.image, "node:24-bookworm");
        assert_eq!(settled.generation, 3, "the generation guard is kept");
        assert!(
            settle_idle(&idle, "node:24-bookworm", false).is_none(),
            "not on disk: stay idle"
        );
        assert!(settle_idle(&idle, "", true).is_none(), "no image configured");
    }

    #[test]
    fn a_real_pull_verdict_is_never_overwritten_by_the_cache_check() {
        for state in [PullState::Pulling, PullState::Done, PullState::Failed, PullState::Cached] {
            let current = PullStatus {
                state,
                image: "x".into(),
                ..Default::default()
            };
            assert!(settle_idle(&current, "node:24-bookworm", true).is_none());
        }
    }

    #[test]
    fn the_generation_guard_is_not_part_of_the_api() {
        // It exists so a slow pull cannot overwrite a newer one's status; it is
        // bookkeeping, and the UI has no use for it.
        let v = serde_json::to_value(PullStatus {
            generation: 7,
            ..Default::default()
        })
        .unwrap();
        assert!(v.get("generation").is_none(), "{v}");
        assert_eq!(v["state"], "idle");
    }

    fn cache(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn an_exact_reference_matches() {
        assert!(matches_cache("node:24-bookworm", &cache(&["node:24-bookworm"])));
    }

    #[test]
    fn a_bare_name_matches_the_latest_tag() {
        assert!(matches_cache("python", &cache(&["python:latest"])));
    }

    #[test]
    fn a_tagged_reference_does_not_fall_back_to_latest() {
        // Asking for :24-bookworm and finding :latest is a different image.
        assert!(!matches_cache("node:24-bookworm", &cache(&["node:latest"])));
    }

    #[test]
    fn an_empty_cache_matches_nothing() {
        assert!(!matches_cache("node:24-bookworm", &cache(&[])));
    }

    #[test]
    fn a_digest_reference_matches_only_its_exact_cache_entry() {
        let pinned = "node:24-bookworm@sha256:6dac556d980b7f0e5498d08f08cee0ca67798b4ad6c23964a9214920e67758d0";
        assert!(matches_cache(pinned, &cache(&[pinned])));
        // A digest reference contains ':', so it never falls back to :latest —
        // or to the plain tag the digest was pinned from.
        assert!(!matches_cache(pinned, &cache(&["node:latest"])));
        assert!(!matches_cache(pinned, &cache(&["node:24-bookworm"])));
    }

    // -- remove_confirmed, against a stand-in `msb` ----------------------------------------------

    use crate::util::short_id;

    /// A stand-in `msb` running `body` on every call (the pattern of the recover tests in
    /// lifecycle.rs): an executable script in a throwaway directory, returned as the path a
    /// backend would be given. The caller removes the directory when done.
    ///
    /// The script is written through a child `sh` (`cat`, fed on stdin) and not with `fs::write`.
    /// These tests run on many threads at once, and `fs::write` holds the file open for writing
    /// while it writes: a `fork` in another thread — any test spawning a child — inherits that
    /// write descriptor, and an `execve` of the script inside that window then fails with
    /// `ETXTBSY`, "Text file busy". `O_CLOEXEC` closes the descriptor only once the forked child
    /// itself execs, which is the very window being raced, so the descriptor must never live in
    /// this process at all. Writing in a child keeps it out, and waiting for that child means the
    /// file is complete and closed before any caller can run it.
    fn stand_in_msb(body: &str) -> (String, std::path::PathBuf) {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("colonizer-remove-confirm-{}", short_id()));
        std::fs::create_dir_all(&root).unwrap();
        let msb = root.join("msb");
        let mut writer = std::process::Command::new("sh")
            .arg("-c")
            .arg("cat > \"$1\"")
            .arg("sh")
            .arg(&msb)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        writer
            .stdin
            .take()
            .unwrap()
            .write_all(format!("#!/bin/sh\n{body}").as_bytes())
            .unwrap();
        assert!(writer.wait().unwrap().success(), "writing the stand-in msb failed");
        std::fs::set_permissions(&msb, std::fs::Permissions::from_mode(0o755)).unwrap();
        (msb.display().to_string(), root)
    }

    /// The failure's one-line chain, so a regression reads as text and not just as `is_err()`.
    fn message(e: &anyhow::Error) -> String {
        format!("{e:#}")
    }

    #[tokio::test]
    async fn a_removal_that_leaves_the_sandbox_listed_is_not_confirmed() {
        let (msb, root) = stand_in_msb(
            "if [ \"$1\" = rm ]; then exit 1; fi\nif [ \"$1\" = ls ]; then printf '%s\\n' other colonizer-abc; fi\nexit 0\n",
        );
        let e = remove_confirmed(&msb, "colonizer-abc", Duration::from_secs(10))
            .await
            .unwrap_err();
        let m = message(&e);
        assert!(m.contains("still listed"), "{m}");
        assert!(m.contains("colonizer-abc"), "{m}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_successful_removal_the_listing_still_sees_is_not_confirmed() {
        // `rm` exits 0, but the sandbox stays in `msb ls`: a listed-but-stopped sandbox is not
        // gone, and the listing is the verdict, not the removal's exit code.
        let (msb, root) = stand_in_msb("if [ \"$1\" = ls ]; then printf '%s\\n' colonizer-abc; fi\nexit 0\n");
        let e = remove_confirmed(&msb, "colonizer-abc", Duration::from_secs(10))
            .await
            .unwrap_err();
        assert!(message(&e).contains("still listed"), "{}", message(&e));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_failed_removal_of_an_already_gone_sandbox_is_confirmed_by_the_listing() {
        // `rm` fails, but only because there was nothing left to remove: the listing says so.
        let (msb, root) = stand_in_msb("if [ \"$1\" = rm ]; then exit 1; fi\nexit 0\n");
        remove_confirmed(&msb, "colonizer-abc", Duration::from_secs(10))
            .await
            .unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_listing_that_fails_leaves_the_removal_unconfirmed() {
        // `rm` reports success, but the listing cannot answer, so the state is unknown: refused.
        let (msb, root) = stand_in_msb("if [ \"$1\" = ls ]; then exit 3; fi\nexit 0\n");
        let e = remove_confirmed(&msb, "colonizer-abc", Duration::from_secs(10))
            .await
            .unwrap_err();
        assert!(message(&e).contains("could not list the sandboxes"), "{}", message(&e));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_removal_the_listing_confirms_is_ok() {
        let asked = std::env::temp_dir().join(format!("colonizer-remove-args-{}", short_id()));
        let (msb, root) = stand_in_msb(&format!("echo \"$*\" >> {asked:?}\nexit 0\n"));
        remove_confirmed(&msb, "colonizer-abc", Duration::from_secs(10))
            .await
            .unwrap();
        let calls = std::fs::read_to_string(&asked).unwrap();
        assert!(calls.contains("rm --force --quiet colonizer-abc"), "{calls}");
        assert!(calls.contains("ls --quiet"), "{calls}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_wedged_removal_is_killed_and_still_judged_by_the_listing() {
        // `rm` hangs: the deadline kills it, and the listing decides. Gone, so confirmed — with
        // the sandbox still listed it would not be. `exec` makes the wedged `sleep` itself the
        // process the deadline kills, rather than a `sleep` forked off and left behind.
        let (msb, root) = stand_in_msb("if [ \"$1\" = rm ]; then exec sleep 30; fi\nexit 0\n");
        let started = std::time::Instant::now();
        remove_confirmed(&msb, "colonizer-abc", Duration::from_millis(200))
            .await
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(10), "the deadline fired");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_wedged_removal_with_the_sandbox_still_listed_is_refused() {
        let (msb, root) = stand_in_msb(
            "if [ \"$1\" = rm ]; then exec sleep 30; fi\nif [ \"$1\" = ls ]; then printf '%s\\n' colonizer-abc; fi\nexit 0\n",
        );
        let e = remove_confirmed(&msb, "colonizer-abc", Duration::from_millis(200))
            .await
            .unwrap_err();
        assert!(message(&e).contains("still listed"), "{}", message(&e));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_listing_that_never_answers_leaves_the_removal_unconfirmed() {
        let (msb, root) = stand_in_msb("if [ \"$1\" = ls ]; then exec sleep 30; fi\nexit 0\n");
        let e = remove_confirmed(&msb, "colonizer-abc", Duration::from_millis(200))
            .await
            .unwrap_err();
        let m = message(&e);
        assert!(m.contains("timed out") && m.contains("colonizer-abc"), "{m}");
        let _ = std::fs::remove_dir_all(root);
    }
}
