//! open-code-review (issue #1274): the `ocr` CLI of github.com/alibaba/open-code-review, run
//! host-side on a colony's diff as it is published. Opt-in per org, `shadow` mode only in this PR:
//! findings are recorded — the colony log and `sessions/<id>/open-code-review.json` — and nothing
//! changes; there is no `on` mode and no severity threshold yet.
//!
//! The binary is not shipped: the mothership downloads the pinned release from
//! open-code-review.lock when asked in Settings, checks its sha256 on the stream and installs it to
//! `<data>/open-code-review/<release>/ocr`, on graft's state machine (`graft.rs`).
//!
//! What ocr reviews is a snapshot, not the worktree: a throwaway git repo under
//! `<data>/open-code-review/` holding the changed paths against the merge base — base versions from
//! git, head versions from disk, untracked files included — every text through the redactor, with
//! credential-like and policy-masked paths, symlinks, binaries and oversized files left out. The
//! CLI runs env-cleared (it would otherwise read `~/.opencodereview/config.json` and the ambient
//! `ANTHROPIC_*`), pointed at the org's model or the install's cheap route, its key passed as
//! `OCR_LLM_TOKEN` and never written to disk. The snapshot is deleted once the run ends.

use crate::{App, Shared, graft::State, util::exec, util::exec_within};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::process::Command;

/// The tool's name: its file name inside `<data>/open-code-review/<release>/`.
pub const NAME: &str = "ocr";

/// Compiled in, so the pin always matches the harness that was built.
const LOCK: &str = include_str!("../open-code-review.lock");

/// One review's ceiling; a wedged run is killed rather than left to outlive the colony.
const REVIEW_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// The snapshot's ceiling: it runs on the publish path, so it must not hold the publish for a
/// colony with thousands of changed paths — one warn line and no review instead.
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(30);

/// Per-file ceiling, so one huge generated file cannot become the whole review.
const MAX_FILE_BYTES: usize = 1024 * 1024;

/// How many changed paths one snapshot carries; the summary says when more were left out.
const MAX_PATHS: usize = 500;

/// How many findings become colony-log lines; the rest are the report's to carry.
const MAX_FINDING_LINES: usize = 20;

/// How old a leftover snapshot dir or download partial must be before a sweep may take it — young
/// ones may belong to a review or download still in flight.
const SWEEP_AGE: Duration = Duration::from_secs(60 * 60);

/// Where the findings are written, inside the session's directory.
const REPORT_FILE: &str = "open-code-review.json";

#[derive(Clone, Debug, PartialEq)]
pub struct Pin {
    pub release: String,
    pub sha256: String,
    pub url: String,
}

/// The binary for the machine running the mothership — the review is host-side, so the platform is
/// the host's own OS and architecture, not a colony's.
pub fn pin() -> Option<Pin> {
    pin_for(LOCK, std::env::consts::OS, std::env::consts::ARCH)
}

fn pin_for(lock: &str, os: &str, arch: &str) -> Option<Pin> {
    let platform = match (os, arch) {
        ("linux", "x86_64") => "linux-amd64",
        ("linux", "aarch64") => "linux-arm64",
        ("darwin", "x86_64") => "darwin-amd64",
        ("darwin", "aarch64") => "darwin-arm64",
        _ => return None,
    };
    lock.lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .find_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            match fields.as_slice() {
                [NAME, release, p, "binary", sha256, url]
                    if *p == platform
                        && sha256.len() == 64
                        && sha256.bytes().all(|b| b.is_ascii_hexdigit())
                        && url.starts_with("https://") =>
                {
                    Some(Pin {
                        release: release.to_string(),
                        sha256: sha256.to_ascii_lowercase(),
                        url: url.to_string(),
                    })
                }
                _ => None,
            }
        })
}

/// Everything open-code-review keeps under the data dir: the installed releases, the throwaway
/// shadow snapshots, the download partials. Ours alone, so unlike `plugins/` there is no
/// operator's-own state to protect.
fn base_dir(app: &App) -> PathBuf {
    app.cfg.data_dir.join("open-code-review")
}

/// The installed binary for `release`, when a download put it there.
fn installed(release: &str, base: &Path) -> Option<PathBuf> {
    let bin = base.join(release).join(NAME);
    bin.is_file().then_some(bin)
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Status {
    pub name: &'static str,
    pub release: Option<String>,
    /// The release on disk when one is — `state: installed` only when it is the pinned one.
    pub installed_release: Option<String>,
    pub state: State,
    /// The installed binary's path, when there is one.
    pub path: Option<String>,
    pub bytes: u64,
    pub total: Option<u64>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    pub error: Option<String>,
    /// Bumped for every download started, so a stale one cannot overwrite a newer status.
    #[serde(skip)]
    pub generation: u64,
}

/// What is on disk against what is pinned, for a status with no download running.
fn settled(base: &Path, pin: Option<&Pin>) -> (State, Option<String>) {
    let mut found: Vec<String> = std::fs::read_dir(base)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|release| !release.starts_with('.') && installed(release, base).is_some())
        .collect();
    found.sort();
    let on_disk = pin
        .and_then(|pin| found.iter().find(|release| **release == pin.release).cloned())
        .or_else(|| found.into_iter().next());
    match (pin, on_disk) {
        (_, Some(release)) if pin.is_none_or(|p| p.release == release) => (State::Installed, Some(release)),
        (None, _) => (State::Unavailable, None),
        (Some(_), on_disk) => (State::Idle, on_disk),
    }
}

/// What Settings shows: the running download if there is one, otherwise what is on disk.
pub async fn current(app: &App) -> Status {
    let status = app.open_code_review.lock().await.clone();
    if matches!(status.state, State::Downloading | State::Unpacking | State::Failed) {
        return Status { name: NAME, ..status };
    }
    let pin = pin();
    let base = base_dir(app);
    let (state, installed_release) = settled(&base, pin.as_ref());
    let error = (state == State::Unavailable)
        .then(|| "no ocr binary is pinned in this build for this machine (crates/colonizer/open-code-review.lock)".to_string());
    Status {
        name: NAME,
        release: pin.map(|pin| pin.release),
        path: installed_release
            .as_ref()
            .and_then(|release| installed(release, &base))
            .map(|path| path.display().to_string()),
        installed_release,
        state,
        error,
        ..status
    }
}

/// `GET /api/open-code-review`
pub async fn status(axum::extract::State(app): axum::extract::State<Shared>) -> axum::Json<Status> {
    axum::Json(current(&app).await)
}

/// `POST /api/open-code-review/download` — start downloading the pinned binary, or report that it
/// is already there or already coming. Returns at once; the GET follows.
pub async fn download(axum::extract::State(app): axum::extract::State<Shared>) -> crate::ApiResult<Status> {
    let status = current(&app).await;
    let pin = match status.state {
        State::Installed | State::Downloading | State::Unpacking => return Ok(axum::Json(status)),
        State::Unavailable => {
            return Err(crate::client_error(
                axum::http::StatusCode::CONFLICT,
                "no ocr binary is pinned in this build for this machine's platform yet",
            ));
        }
        // `Local` cannot come back: this directory is only ever ours.
        State::Idle | State::Failed | State::Local => pin().expect("current() returned a pinned state"),
    };
    let started = {
        let mut status = app.open_code_review.lock().await;
        *status = Status {
            name: NAME,
            release: Some(pin.release.clone()),
            state: State::Downloading,
            started_at: Some(chrono::Utc::now()),
            generation: status.generation + 1,
            ..Default::default()
        };
        status.clone()
    };
    let background = app.clone();
    tokio::spawn(async move {
        let result = fetch(&background, &pin, started.generation).await;
        let mut status = background.open_code_review.lock().await;
        if status.generation != started.generation {
            return;
        }
        status.finished_at = Some(chrono::Utc::now());
        match result {
            // Settled from disk on the next read, like every other idle state.
            Ok(()) => status.state = State::Idle,
            Err(e) => {
                status.state = State::Failed;
                status.error = Some(crate::util::truncate(&format!("{e:#}"), 2000));
            }
        }
    });
    Ok(axum::Json(started))
}

/// Deletes the leftovers a crashed review or download leaves — `.shadow-*` dirs and `*.part`
/// partials older than [`SWEEP_AGE`] — so they cannot pile up under the data dir.
async fn sweep(base: &Path) {
    let Ok(mut entries) = tokio::fs::read_dir(base).await else {
        return;
    };
    let mut stale = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(".shadow-") && !name.ends_with(".part") {
            continue;
        }
        let fresh = entry
            .metadata()
            .await
            .and_then(|m| m.modified())
            .ok()
            .and_then(|at| at.elapsed().ok())
            .is_some_and(|age| age < SWEEP_AGE);
        if !fresh {
            stale.push(entry.path());
        }
    }
    for path in stale {
        if path.is_dir() {
            let _ = tokio::fs::remove_dir_all(path).await;
        } else {
            let _ = tokio::fs::remove_file(path).await;
        }
    }
}

/// Downloads the binary, checks its sha256 on the stream, and installs it: the exec bit goes on the
/// verified `.part` and one rename moves it into `<release>/ocr`, so the final name only ever holds
/// a complete, checksum-passing binary.
async fn fetch(app: &Shared, pin: &Pin, generation: u64) -> Result<()> {
    let base = base_dir(app);
    tokio::fs::create_dir_all(&base).await?;
    sweep(&base).await;
    let part = base.join(format!(".{NAME}-{generation}.part"));
    let progress = |total: Option<u64>, bytes: u64| {
        let app = app.clone();
        async move {
            let mut status = app.open_code_review.lock().await;
            if status.generation == generation {
                if total.is_some() {
                    status.total = total;
                }
                status.bytes = bytes;
            }
        }
    };
    if let Err(e) = crate::util::download_sha256(&pin.url, &pin.sha256, &part, progress).await {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(e).with_context(|| format!("ocr {}", pin.release));
    }
    app.open_code_review.lock().await.state = State::Unpacking;
    let dir = base.join(&pin.release);
    let installed = async {
        tokio::fs::create_dir_all(&dir).await?;
        std::fs::set_permissions(&part, std::fs::Permissions::from_mode(0o755))?;
        tokio::fs::rename(&part, dir.join(NAME)).await?;
        Ok(())
    }
    .await;
    let _ = tokio::fs::remove_file(&part).await;
    installed
}

/// The org's switch (issue #1274): off unless the operator opted in, and `shadow` is all this PR
/// builds — findings are recorded, nothing changes.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Off,
    Shadow,
}

/// One ocr finding: the fields this module reads and writes back to the record. `path` and
/// `content` are the CLI's always-present pair; the rest arrive when the model sent them.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Finding {
    pub path: String,
    pub content: String,
    pub start_line: Option<u64>,
    pub end_line: Option<u64>,
    pub category: Option<String>,
    pub severity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion_code: Option<String>,
}

/// Parses ocr's JSON reply: the report whole, every string in it redacted — the snapshot was
/// redacted going in, but the model's own words come back unredacted and the record outlives the
/// run — and the findings read out of it. A comment that cannot parse is dropped, not fatal.
fn parse_report(stdout: &str) -> Result<(Value, Vec<Finding>)> {
    let mut report: Value = serde_json::from_str(stdout).context("ocr's reply was not the JSON it promised")?;
    crate::redact::redact_value(&mut report);
    let findings = report
        .get("comments")
        .and_then(Value::as_array)
        .map(|comments| {
            comments
                .iter()
                .filter_map(|c| serde_json::from_value(c.clone()).ok())
                .collect()
        })
        .unwrap_or_default();
    Ok((report, findings))
}

/// `N findings — x critical, y high, …`, naming only the severities that occurred.
fn summary_line(findings: &[Finding]) -> String {
    let mut parts = Vec::new();
    for severity in ["critical", "high", "medium", "low"] {
        let n = findings
            .iter()
            .filter(|f| f.severity.as_deref().is_some_and(|s| s.eq_ignore_ascii_case(severity)))
            .count();
        if n > 0 {
            parts.push(format!("{n} {severity}"));
        }
    }
    if parts.is_empty() {
        format!("{} findings", findings.len())
    } else {
        format!("{} findings — {}", findings.len(), parts.join(", "))
    }
}

/// `path:start [category · severity] content`, one clipped line per finding for the colony log.
fn finding_line(finding: &Finding) -> String {
    let place = match (finding.start_line, finding.end_line) {
        (Some(start), Some(end)) if start != end => format!("{}:{}-{}", finding.path, start, end),
        (Some(start), _) => format!("{}:{}", finding.path, start),
        _ => finding.path.clone(),
    };
    let tag = [finding.category.as_deref(), finding.severity.as_deref()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
    let text = finding.content.replace('\n', " ");
    let line = if tag.is_empty() {
        format!("{place} {text}")
    } else {
        format!("{place} [{tag}] {text}")
    };
    crate::util::truncate(&line, 500)
}

/// One shadow review's model endpoint, as ocr's `OCR_LLM_*` environment: the org's own
/// `provider/model`, else the install's cheap route ([`crate::summaries::cheap_route`]). The key
/// reaches the process as `OCR_LLM_TOKEN` and is never written anywhere.
async fn model_env(app: &Shared, org_model: Option<&str>) -> Result<Vec<(String, String)>> {
    let model = match org_model.map(str::trim).filter(|m| !m.is_empty()) {
        Some(model) => model.to_string(),
        None => match crate::summaries::cheap_route(app).await {
            Some(crate::summaries::Route::Provider(model)) => model,
            Some(crate::summaries::Route::ApiKey(model)) => {
                // The install's real Anthropic API key, exactly as summaries would spend it.
                let key = app
                    .claude_cred()
                    .and_then(|c| crate::summaries::api_key_of(&c.value).map(str::to_string))
                    .context("the Anthropic API key the cheap route picked is gone")?;
                return Ok(vec![
                    ("OCR_LLM_URL".into(), "https://api.anthropic.com".into()),
                    ("OCR_LLM_TOKEN".into(), key),
                    ("OCR_LLM_MODEL".into(), crate::providers::api_model(&model).to_string()),
                    ("OCR_LLM_PROTOCOL".into(), "anthropic".into()),
                    ("OCR_LLM_AUTH_HEADER".into(), "x-api-key".into()),
                ]);
            }
            None => bail!(
                "no model to review with: set the org's open_code_review_model to provider/model, \
                 or give the install a summary model (Settings → Modules → agent)"
            ),
        },
    };
    let providers = app.providers();
    let (provider, upstream) =
        crate::autonomy::route(&model, &providers).with_context(|| format!("cannot route the review model {model:?}"))?;
    let mut env = vec![
        ("OCR_LLM_URL".into(), provider.base_url.trim_end_matches('/').to_string()),
        ("OCR_LLM_MODEL".into(), upstream),
        (
            "OCR_LLM_PROTOCOL".into(),
            match provider.wire {
                crate::providers::Wire::Anthropic => "anthropic",
                crate::providers::Wire::Openai => "openai",
            }
            .to_string(),
        ),
    ];
    if provider.auth != "none" {
        let key = app
            .provider_key(&provider.id)
            .with_context(|| format!("provider {} has no key saved", provider.id))?;
        // `x-api-key` is not the CLI's default header, so it is named; a bearer key rides the
        // default Authorization header.
        if provider.auth == "x-api-key" {
            env.push(("OCR_LLM_AUTH_HEADER".into(), "x-api-key".into()));
        }
        env.push(("OCR_LLM_TOKEN".into(), key));
    }
    Ok(env)
}

/// The throwaway repo a shadow review runs over, and the empty HOME its CLI runs with.
struct Snapshot {
    root: PathBuf,
    repo: PathBuf,
    home: PathBuf,
    /// Changed paths left out over [`MAX_PATHS`], said out loud in the summary.
    skipped_paths: usize,
}

/// One line in the colony's harness log, prefixed so the cockpit can tell these apart.
async fn note(app: &Shared, id: &str, level: &str, message: String) {
    app.session_log(id, level, format!("open-code-review (shadow): {message}"))
        .await;
}

/// The shadow review (issue #1274), first thing in `publish::publish_session` — every publish path
/// lands there, and nothing has rewritten the worktree yet. When the session's org opted in and the
/// binary is installed, the diff is snapshotted synchronously under [`SNAPSHOT_TIMEOUT`] — the
/// publish rewrites the worktree moments later — and the review is spawned, its findings landing in
/// the colony log and `sessions/<id>/open-code-review.json` whenever they are ready. Every failure
/// is one log line; nothing about the publish waits on this or changes for it.
pub(crate) async fn shadow_review(app: &Shared, id: &str) {
    let Some(s) = app.session(id).await else { return };
    let org = app.org_settings(&s.org);
    if crate::orgs::open_code_review_mode(&org) != Mode::Shadow {
        return;
    }
    let Some(pin) = pin() else {
        return note(
            app,
            id,
            "info",
            "this machine's platform has no ocr binary pinned (crates/colonizer/open-code-review.lock)".into(),
        )
        .await;
    };
    let Some(bin) = installed(&pin.release, &base_dir(app)) else {
        return note(
            app,
            id,
            "info",
            format!("ocr {} is not installed; download it in Settings", pin.release),
        )
        .await;
    };
    let env = match model_env(app, crate::orgs::open_code_review_model(&org).as_deref()).await {
        Ok(env) => env,
        Err(e) => return note(app, id, "warn", format!("{e:#}")).await,
    };
    let snapshot = match tokio::time::timeout(SNAPSHOT_TIMEOUT, snapshot_diff(app, &s)).await {
        Ok(Ok(snapshot)) => snapshot,
        Ok(Err(e)) => return note(app, id, "warn", format!("could not snapshot the diff: {e:#}")).await,
        Err(_) => {
            return note(
                app,
                id,
                "warn",
                format!(
                    "the diff snapshot outran its {}s budget; no review this time",
                    SNAPSHOT_TIMEOUT.as_secs()
                ),
            )
            .await;
        }
    };
    let skipped = snapshot.skipped_paths;
    let app = app.clone();
    let id = id.to_string();
    tokio::spawn(async move {
        let reviewed = run_review(&bin, &snapshot, &env).await;
        let _ = tokio::fs::remove_dir_all(&snapshot.root).await;
        match reviewed {
            Ok((report, findings)) => {
                if let Ok(bytes) = serde_json::to_vec(&report) {
                    let _ = app.store().write_file(&id, REPORT_FILE, &bytes).await;
                }
                let mut summary = summary_line(&findings);
                if skipped > 0 {
                    summary.push_str(&format!(" ({skipped} more changed paths were left out over the cap)"));
                }
                note(&app, &id, "info", summary).await;
                for finding in findings.iter().take(MAX_FINDING_LINES) {
                    app.session_log(&id, "info", finding_line(finding)).await;
                }
                if findings.len() > MAX_FINDING_LINES {
                    let more = findings.len() - MAX_FINDING_LINES;
                    app.session_log(&id, "info", format!("… and {more} more (see {REPORT_FILE})"))
                        .await;
                }
            }
            Err(e) => note(&app, &id, "warn", format!("the review failed: {e:#}")).await,
        }
    });
}

/// A hardened git pinned to the colony's worktree, like the publish's own: the admin dir and the
/// worktree both named, so a `.git` file the colony rewrote is not read at all.
fn wt_git(app: &App, admin: &Path, wt: &Path) -> Command {
    let mut c = app.git(admin);
    c.arg("--work-tree").arg(wt);
    c
}

/// Runs one git that answers a question about the colony's tree, `None` when it cannot — for one
/// path in one commit, a refusal and an absence mean the same thing.
async fn git_opt(mut cmd: Command) -> Result<Option<Vec<u8>>> {
    let out = cmd
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .context("asking git about the colony's tree")?;
    Ok(out.status.success().then_some(out.stdout))
}

/// Snapshots the colony's diff into a fresh git repo under `<data>/open-code-review/`: for every
/// changed path — against the merge base of `origin/<base>` and HEAD, working-tree and untracked
/// files included, which is the tree the publish commits — the base version from git and the
/// working-tree version from disk, both redacted, with credential-like and policy-masked paths,
/// symlinks, binaries and oversized files left out. Two commits, `base` then `head`, on branches of
/// the same names: that pair is what ocr is pointed at.
async fn snapshot_diff(app: &App, s: &crate::sessions::Session) -> Result<Snapshot> {
    let admin = PathBuf::from(s.git_admin_dir.as_deref().context("the session has no worktree yet")?);
    let base_branch = s.base.as_deref().context("the session has no base branch")?;
    let wt = PathBuf::from(&s.worktree);
    let base = base_dir(app);
    sweep(&base).await;
    let git = |args: &[&str]| {
        let mut c = wt_git(app, &admin, &wt);
        c.args(args);
        c
    };
    // The merge base, not `origin/<base>` itself: what this branch changed since the two histories
    // met, which is what the pull request would show.
    let origin = format!("origin/{base_branch}");
    // A nested fn, not a closure: the future `exec_within` returns borrows the command, a tie a
    // closure cannot express.
    async fn within(cmd: &mut Command) -> Result<String> {
        exec_within(SNAPSHOT_TIMEOUT, cmd).await
    }
    let merge_base = within(&mut git(&["merge-base", &origin, "HEAD"])).await?.trim().to_string();
    // The working tree against the merge base, plus the untracked files a `git diff` never lists.
    let tracked = within(&mut git(&["diff", "--name-only", "-z", &merge_base])).await?;
    let untracked = within(&mut git(&["ls-files", "--others", "--exclude-standard", "-z"])).await?;
    let mut paths = crate::path_policy::z_paths(&format!("{tracked}{untracked}"));
    paths.sort();
    paths.dedup();
    // What the review may not see: the paths the path policy hid from the colony (the same recorded
    // list the publish checks) and the credential-like names no diff should carry anyway.
    let policy = crate::path_policy::Recorded::read(&app.session_dir(&s.id).join("vm")).policy;
    let paths: Vec<String> = paths
        .into_iter()
        .filter(|path| safe_rel(path) && !crate::path_policy::credential_like(path))
        .filter(|path| !crate::path_policy::matches_any(path, &policy.masked))
        .collect();
    let skipped_paths = paths.len().saturating_sub(MAX_PATHS);
    let paths = &paths[..paths.len().min(MAX_PATHS)];
    if paths.is_empty() {
        bail!("nothing changed against {base_branch} that a review may see");
    }

    let root = base.join(format!(".shadow-{}", crate::util::short_id()));
    let repo = root.join("repo");
    let home = root.join("home");
    tokio::fs::create_dir_all(&home).await?;
    let snap = |args: &[&str]| {
        let mut c = app.git_plain();
        c.arg("-C").arg(&repo).args(args);
        c
    };
    let commit = |message: &str| {
        snap(&[
            "-c",
            "user.name=open-code-review",
            "-c",
            "user.email=ocr@colonizer.invalid",
            "commit",
            "-q",
            "-m",
            message,
        ])
    };
    exec(&mut snap(&["init", "-q", "-b", "base"])).await?;
    for path in paths {
        let at_base = format!("{merge_base}:{path}");
        // The blob's size first: `git show` would happily pour a packed giant through stdout.
        let small = git_opt(git(&["cat-file", "-s", &at_base]))
            .await?
            .and_then(|out| String::from_utf8_lossy(&out).trim().parse::<u64>().ok())
            .is_some_and(|len| len <= MAX_FILE_BYTES as u64);
        if small && let Some(text) = git_opt(git(&["show", &at_base])).await?.and_then(|out| reviewable(Some(out))) {
            write_file(&repo, path, &text).await?;
        }
    }
    // `-f`, so a `.gitignore` the diff itself carries cannot hide the paths put here on purpose.
    exec(&mut snap(&["add", "-A", "-f"])).await?;
    exec(&mut commit("base")).await?;
    exec(&mut snap(&["checkout", "-q", "-b", "head"])).await?;
    for path in paths {
        match head_version(&wt, path).await? {
            Some(text) => write_file(&repo, path, &text).await?,
            None => {
                let _ = tokio::fs::remove_file(repo.join(path)).await;
            }
        }
    }
    exec(&mut snap(&["add", "-A", "-f"])).await?;
    exec(&mut commit("head")).await?;
    Ok(Snapshot {
        root,
        repo,
        home,
        skipped_paths,
    })
}

/// The head side: the working-tree file as redacted text. `symlink_metadata`, so a symlink the
/// colony left is never followed — the link is not reviewable content, and following it could read
/// outside the worktree. `None` when the path is gone, is not a regular file, or is over the cap.
async fn head_version(wt: &Path, path: &str) -> Result<Option<String>> {
    let file = wt.join(path);
    let Ok(meta) = tokio::fs::symlink_metadata(&file).await else {
        return Ok(None);
    };
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES as u64 {
        return Ok(None);
    }
    Ok(reviewable(tokio::fs::read(&file).await.ok()))
}

/// Colony-chosen bytes as text a model may review: UTF-8 (a binary's lossy text is noise), not
/// empty, not over [`MAX_FILE_BYTES`], and redacted.
fn reviewable(bytes: Option<Vec<u8>>) -> Option<String> {
    bytes
        .filter(|bytes| !bytes.is_empty() && bytes.len() <= MAX_FILE_BYTES)
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .map(|text| crate::redact::redact_text(&text).into_owned())
}

/// Writes one snapshot file, making its directories as needed.
async fn write_file(repo: &Path, path: &str, text: &str) -> Result<()> {
    let file = repo.join(path);
    if let Some(parent) = file.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&file, text)
        .await
        .with_context(|| format!("writing {path} into the snapshot"))
}

/// A path git itself could have named in a diff: relative, no traversal, and not into a `.git`.
/// Git only ever names such paths, but the guard is one line against colony-chosen data.
fn safe_rel(path: &str) -> bool {
    !path.is_empty() && !path.starts_with('/') && !path.split('/').any(|c| c == ".." || c == ".git")
}

/// Runs ocr over the snapshot with a cleared environment — only `PATH`, the throwaway `HOME` and
/// the `OCR_LLM_*` variables of [`model_env`] — so the CLI reads none of its own config files and
/// none of the ambient `ANTHROPIC_*`. Killed on drop, so the timeout cannot leak the process.
async fn run_review(bin: &Path, snapshot: &Snapshot, env: &[(String, String)]) -> Result<(Value, Vec<Finding>)> {
    let mut cmd = Command::new(bin);
    cmd.arg("review")
        .arg("--repo")
        .arg(&snapshot.repo)
        .args(["--from", "base", "--to", "head", "--format", "json", "-o", "-"])
        .env_clear()
        .env("HOME", &snapshot.home)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    if let Some(path) = std::env::var_os("PATH") {
        cmd.env("PATH", path);
    }
    for (name, value) in env {
        cmd.env(name, value);
    }
    let out = tokio::time::timeout(REVIEW_TIMEOUT, cmd.output())
        .await
        .map_err(|_| anyhow::anyhow!("ocr did not finish within {} minutes", REVIEW_TIMEOUT.as_secs() / 60))?
        .context("failed to start the ocr binary")?;
    if !out.status.success() {
        bail!(
            "ocr exited {}: {}",
            out.status,
            crate::util::truncate(String::from_utf8_lossy(&out.stderr).trim(), 300)
        );
    }
    parse_report(&String::from_utf8_lossy(&out.stdout))
}

/// The API routes this module serves.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new()
        .route("/api/open-code-review", routing::get(status))
        .route("/api/open-code-review/download", routing::post(download))
}

/// open-code-review as a migrated feature (`features.rs`): owner-only to scoped API tokens
/// (`token_scope: None`) — the switch decides what every colony of an org spends on reviews, so
/// only the owner may point it. No background work of its own: the review runs off the publish
/// path, and the download off its route.
pub(crate) const FEATURE: crate::features::Feature = crate::features::Feature {
    name: "open_code_review",
    routes,
    token_scope: None,
    activity: &[],
    kinds: &[],
    start_tasks: None,
};

#[cfg(test)]
mod tests {
    use super::*;

    /// Spliced together at compile time so the source never holds a key-shaped literal; at
    /// runtime it is an ordinary key and trips the redactor exactly as a real one would.
    fn anthropic_key_fixture() -> &'static str {
        concat!("sk-", "ant-", "api03-", "AbCdEf123456_GhIjKl-", "789012MnOpQr")
    }

    #[test]
    fn the_real_lock_pins_every_platform_this_module_names() {
        for (os, arch, platform, sha) in [
            (
                "linux",
                "x86_64",
                "linux-amd64",
                "879d6eda27d136473f7af680ed5009516baf6236d8a0e5f5f14f3e6dececdd1f",
            ),
            (
                "linux",
                "aarch64",
                "linux-arm64",
                "2d66c71eeea08730e1394cc39884b9644d301288443d644a02c2adebb0425b93",
            ),
            (
                "darwin",
                "x86_64",
                "darwin-amd64",
                "9bc7717b680cb1620adbf449446f51fcae74537efbf00442a235e7e62099527d",
            ),
            (
                "darwin",
                "aarch64",
                "darwin-arm64",
                "ccc1f3c14395313332451cb1c6a7c91616a9f36058412ac502eada7394d3c5ec",
            ),
        ] {
            let pin = pin_for(LOCK, os, arch).unwrap_or_else(|| panic!("{os}/{arch} is not pinned"));
            assert_eq!(pin.release, "v1.13.0");
            assert_eq!(pin.sha256, sha);
            assert_eq!(
                pin.url,
                format!("https://github.com/alibaba/open-code-review/releases/download/v1.13.0/opencodereview-{platform}")
            );
        }
        assert_eq!(pin_for(LOCK, "windows", "x86_64"), None, "no other platform is pinned");
    }

    #[test]
    fn only_a_real_checksum_and_an_https_url_pin() {
        let row = |platform: &str, sha: &str, url: &str| format!("ocr v1.13.0 {platform} binary {sha} {url}\n");
        let good = row("linux-amd64", &"a".repeat(64), "https://x");
        assert_eq!(pin_for(&good, "linux", "x86_64").unwrap().release, "v1.13.0");
        for bad in [
            row("linux-amd64", &"a".repeat(63), "https://x"),
            row("linux-amd64", &"z".repeat(64), "https://x"),
            row("linux-amd64", &"a".repeat(64), "http://x"),
            // The host's platform, not a colony's: an arm pin does not serve an amd64 host.
            row("linux-arm64", &"a".repeat(64), "https://x"),
            // Another tool's row, a template row, an archive rather than a binary.
            "graft 0.19.0-1 linux-amd64 bundle <sha256> https://x".to_string(),
            "ocr v1.13.0 linux-amd64 binary <sha256> https://x".to_string(),
            row("linux-amd64", &"a".repeat(64), "https://x").replace("binary", "bundle"),
        ] {
            assert_eq!(pin_for(&bad, "linux", "x86_64"), None, "pinned: {bad}");
        }
    }

    fn finding(severity: Option<&str>, category: Option<&str>) -> Finding {
        Finding {
            path: "src/lib.rs".into(),
            content: "this loop never ends".into(),
            start_line: Some(7),
            end_line: Some(7),
            category: category.map(String::from),
            severity: severity.map(String::from),
            suggestion_code: None,
        }
    }

    #[test]
    fn ocr_json_parses_and_its_findings_are_redacted() {
        let stdout = r#"{"status":"completed","summary":{"total_comments":2},"comments":[
            {"path":"src/lib.rs","content":"the key {key} appears here","start_line":1,"end_line":1,
             "category":"correctness","severity":"high","suggestion_code":"fix it"},
            {"path":"other.txt","content":"fine"}]}"#
            .replace("{key}", anthropic_key_fixture());
        let (report, findings) = parse_report(&stdout).unwrap();
        assert_eq!(findings.len(), 2);
        assert!(findings[0].content.contains("[REDACTED"), "{}", findings[0].content);
        assert!(!findings[0].content.contains(concat!("sk-", "ant-api")));
        assert!(report["comments"][0]["content"].as_str().unwrap().contains("[REDACTED"));
        assert_eq!(findings[1].start_line, None, "a missing line is None, not an error");
        let (_, findings) = parse_report(r#"{"status":"completed","comments":[{"path":null}]}"#).unwrap();
        assert!(findings.is_empty(), "an unparseable comment is dropped, not fatal");
        assert!(parse_report("not json").is_err());
    }

    #[test]
    fn the_summary_names_only_the_severities_that_occurred() {
        assert_eq!(summary_line(&[]), "0 findings");
        assert_eq!(
            summary_line(&[
                finding(Some("critical"), None),
                finding(Some("HIGH"), None),
                finding(None, None)
            ]),
            "3 findings — 1 critical, 1 high"
        );
        assert_eq!(
            summary_line(&[finding(Some("low"), Some("style")), finding(Some("low"), None)]),
            "2 findings — 2 low"
        );
    }

    #[test]
    fn one_finding_is_one_clipped_log_line() {
        assert_eq!(
            finding_line(&finding(Some("high"), Some("correctness"))),
            "src/lib.rs:7 [correctness · high] this loop never ends"
        );
        assert_eq!(
            finding_line(&Finding {
                content: "first\nsecond".into(),
                end_line: Some(9),
                ..finding(None, None)
            }),
            "src/lib.rs:7-9 first second"
        );
        let long = finding_line(&Finding {
            content: "x".repeat(1000),
            ..finding(None, None)
        });
        // `util::truncate` clips to the bound and adds its ellipsis on top.
        assert!(long.chars().count() <= 501, "clipped to {} chars", long.chars().count());
    }

    #[test]
    fn the_path_filter_keeps_out_traversal_git_and_credentials() {
        for good in ["src/lib.rs", "a/b/c.txt"] {
            assert!(safe_rel(good), "{good} is a path git could name");
        }
        for bad in ["", "/etc/passwd", "../outside", "a/../../outside", "sub/.git/config", ".git"] {
            assert!(!safe_rel(bad), "{bad} must not reach the snapshot");
        }
        let masked = vec!["secrets/".to_string(), ".npmrc".to_string()];
        for hidden in ["secrets/prod.txt", "a/secrets/b.txt", "sub/.npmrc"] {
            assert!(crate::path_policy::matches_any(hidden, &masked), "{hidden} is masked");
        }
        assert!(!crate::path_policy::matches_any("src/other.txt", &masked));
        for credential in [".env", "deploy/.env.production", "id_ed25519"] {
            assert!(
                crate::path_policy::credential_like(credential),
                "{credential} is credential-like"
            );
        }
        assert!(!crate::path_policy::credential_like(".env.example"));
        assert_eq!(
            reviewable(Some(format!("token {}", anthropic_key_fixture()).into_bytes())).as_deref(),
            Some("token [REDACTED:anthropic_key]")
        );
        assert_eq!(reviewable(Some(vec![0, 159, 146, 150])), None, "not UTF-8");
        assert_eq!(reviewable(Some(Vec::new())), None);
        assert_eq!(reviewable(None), None);
    }

    #[tokio::test]
    async fn the_head_side_reads_regular_files_only() {
        let wt = std::env::temp_dir().join(format!("ocr-head-{}", crate::util::short_id()));
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join("real.txt"), format!("token {}", anthropic_key_fixture())).unwrap();
        std::fs::write(wt.join("big.txt"), vec![b'x'; MAX_FILE_BYTES + 1]).unwrap();
        std::fs::write(wt.join("outside.txt"), "not the colony's business").unwrap();
        std::os::unix::fs::symlink(wt.join("outside.txt"), wt.join("link.txt")).unwrap();
        std::os::unix::fs::symlink("/etc/hostname", wt.join("absolute.txt")).unwrap();
        assert_eq!(
            head_version(&wt, "real.txt").await.unwrap().as_deref(),
            Some("token [REDACTED:anthropic_key]")
        );
        assert_eq!(head_version(&wt, "gone.txt").await.unwrap(), None);
        assert_eq!(head_version(&wt, "big.txt").await.unwrap(), None, "over the per-file cap");
        for link in ["link.txt", "absolute.txt"] {
            assert_eq!(head_version(&wt, link).await.unwrap(), None, "{link} is not followed");
        }
        let _ = std::fs::remove_dir_all(&wt);
    }
}
