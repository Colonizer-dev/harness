//! Issue #328: never take a colony's word that it is done. When a turn ends with a completion
//! claim (a clean turn that wrote `pr.md` — the same predicate that makes autopilot publish),
//! the mothership verifies it on its own and attaches a three-way verdict.
//!
//! Verification is mechanical, mothership-only inputs: a snapshot of the colony's work taken
//! without touching its worktree, the claim's described paths read out of `pr.md`, and the
//! repository's own test command — resolved from the **base branch's** tree, never the colony's
//! branch (a branch that rewrote the entry defining it is refused as unverifiable). The test run
//! happens in a fresh microVM on a fresh `git archive` export of the snapshot; the host never
//! executes repository code (github.rs `HOST_GIT_NO_EXEC`), and green needs the guest's own
//! report — its exit number written to a file only this harness reads — with the sandbox's exit
//! code corroborating, never deciding alone.
//!
//! **Contradicted** when the branch disagrees with the claim (empty diff, or a described file
//! missing where its directory exists) or the tests fail in the fresh checkout; **confirmed**
//! only when the tests ran green; **unverifiable** otherwise — no known command, or infra that
//! broke, which is not the colony's fault. `verify: none` records `unverifiable` by declaration
//! without any work.

use crate::{
    App, Shared,
    sessions::Session,
    util::{exec_within, short_id},
};
use anyhow::{Result, bail};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::process::Command;

/// How long one host-side git read may take before the verification gives up on it.
const GIT_LIMIT: Duration = Duration::from_secs(30);
/// How long the fresh-checkout test run may take. Infra ceiling, not a judgement.
const TEST_LIMIT: Duration = Duration::from_secs(20 * 60);
/// How many changed files the record carries; the full set still feeds the claim check.
const FILES_CAP: usize = 50;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Confirmed,
    Contradicted,
    Unverifiable,
}

/// One verification's outcome: exactly what the `verification` chain event carries (minus its
/// `type`), and what `Session.verification` persists.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Verification {
    pub verdict: Verdict,
    pub by_declaration: bool,
    pub summary: String,
    pub contradictions: Vec<String>,
    pub command: Option<String>,
    /// `"config"` (an explicit command), the base branch file that declared it, or null.
    pub command_source: Option<String>,
    pub exit_code: Option<i32>,
    pub tests_ms: Option<u64>,
    pub commits: u64,
    pub files_changed: Vec<String>,
    pub snapshot: Option<String>,
    /// Total wall time — the cost. No model calls.
    pub ms: u64,
}

impl Verification {
    /// The host chain event body: this record plus its `type`.
    pub(crate) fn event(&self) -> Value {
        let mut v = serde_json::to_value(self).expect("a Verification serialises");
        v["type"] = json!("verification");
        v
    }

    /// The empty record every path through a verification fills in as the facts arrive.
    fn blank() -> Self {
        Verification {
            verdict: Verdict::Unverifiable,
            by_declaration: false,
            summary: String::new(),
            contradictions: Vec::new(),
            command: None,
            command_source: None,
            exit_code: None,
            tests_ms: None,
            commits: 0,
            files_changed: Vec::new(),
            snapshot: None,
            ms: 0,
        }
    }

    fn finished(mut self, started: Instant) -> Self {
        self.ms = started.elapsed().as_millis() as u64;
        self
    }
}

/// The verdict from the pieces: contradicted if anything contradicts, confirmed only on a green
/// fresh run, unverifiable otherwise. Pure so the transitions are tested directly.
fn decide(contradictions: &[String], green: Option<bool>) -> Verdict {
    if !contradictions.is_empty() {
        Verdict::Contradicted
    } else if green == Some(true) {
        Verdict::Confirmed
    } else {
        Verdict::Unverifiable
    }
}

/// The files on the base branch that can declare a test command, as pure inputs.
#[derive(Debug)]
pub(crate) struct BaseFiles {
    pub package_json: Option<String>,
    pub package_lock: bool,
    pub cargo_toml: bool,
    pub makefile: Option<String>,
}

/// package.json's `scripts.test`, when the file parses and carries one.
fn scripts_test(package: Option<&str>) -> Option<String> {
    let value = serde_json::from_str::<Value>(package?).ok()?;
    value["scripts"]["test"].as_str().map(str::to_string)
}

/// The test command a repository declares, from its base branch's file contents alone: npm's
/// `scripts.test` (unless it is npm's placeholder) with `npm ci` when a lockfile exists, else
/// `cargo test` for a Cargo.toml, else `make test` for a Makefile with a `test:` target. Never
/// guessed from chat text.
pub(crate) fn declared_test_command(files: &BaseFiles) -> Option<(&'static str, String)> {
    let npm = scripts_test(files.package_json.as_deref()).filter(|t| !t.is_empty() && !t.contains("no test specified"));
    if npm.is_some() {
        let install = if files.package_lock { "npm ci" } else { "npm install" };
        return Some(("package.json", format!("{install} && npm test")));
    }
    if files.cargo_toml {
        return Some(("Cargo.toml", "cargo test".into()));
    }
    if files
        .makefile
        .as_deref()
        .is_some_and(|m| m.lines().any(|l| l.starts_with("test:")))
    {
        return Some(("Makefile", "make test".into()));
    }
    None
}

/// The paths a pull request description claims to have touched: backtick tokens that look like
/// paths (they contain `/`, the last segment has an extension, no spaces, `*` or `:`), with
/// `:123` line suffixes (and `:12-30` ranges) and a leading `./` stripped.
pub(crate) fn claimed_paths(markdown: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in markdown.split('`').skip(1).step_by(2) {
        let trimmed = raw.trim();
        let token = trimmed.strip_prefix("./").unwrap_or(trimmed);
        let token = match token.rsplit_once(':') {
            Some((path, lines))
                if lines.bytes().any(|b| b.is_ascii_digit()) && lines.bytes().all(|b| b.is_ascii_digit() || b == b'-') =>
            {
                path
            }
            _ => token,
        };
        let looks_like_path = token.contains('/')
            && token.rsplit('/').next().is_some_and(|last| last.contains('.'))
            && !token.contains([' ', '*', ':']);
        if looks_like_path && !out.iter().any(|p| p == token) {
            out.push(token.to_string());
        }
    }
    out
}

/// How the fresh-checkout run executes: normally microsandbox, overridable in tests. Takes the
/// spec and answers the command's exit code; failures are infra, not the colony.
pub(crate) type VmRunner = Arc<dyn Fn(crate::sandbox::BootSpec) -> BoxFuture<'static, Result<i32>> + Send + Sync>;

fn microsandbox_runner(msb: String) -> VmRunner {
    Arc::new(move |spec| {
        let msb = msb.clone();
        Box::pin(async move { crate::sandbox::run_once(&msb, &spec).await })
    })
}

/// One host-side git with cwd pinned to a directory that is no repository and the `GIT_*` a
/// sandbox may have exported dropped: git must discover nothing (the harness's own repo, when
/// it runs from inside one) — every input is `--git-dir`, `--work-tree` or the args — while
/// keeping the hardened configuration (github.rs `HOST_GIT_NO_EXEC`).
fn disowned(mut c: Command, cwd: &Path) -> Command {
    c.current_dir(cwd)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    c
}

fn git_at(app: &App, admin: &Path, cwd: &Path, args: &[&str]) -> Command {
    let mut c = disowned(app.git(admin), cwd);
    c.args(args);
    c
}

/// Snapshots the colony's work — commits plus everything uncommitted, .gitignore respected, like
/// publish's `add -A` — into a commit object **without** mutating the agent's worktree, index or
/// branch: a temp `GIT_INDEX_FILE` outside the worktree, seeded from HEAD, then `add -A`,
/// `write-tree`, `commit-tree -p HEAD`. Only objects are written, which the worktree's git dir
/// already exists to hold.
async fn snapshot_work(app: &App, s: &Session, admin: &Path, cwd: &Path) -> Result<String> {
    let index = cwd.join(format!("verify-index-{}", short_id()));
    let git = |args: &[&str]| {
        let mut c = disowned(app.git(admin), cwd);
        c.arg("--work-tree").arg(&s.worktree).env("GIT_INDEX_FILE", &index).args(args);
        c
    };
    let result = async {
        exec_within(GIT_LIMIT, &mut git(&["read-tree", "HEAD"])).await?;
        exec_within(GIT_LIMIT, &mut git(&["add", "-A"])).await?;
        let tree = exec_within(GIT_LIMIT, &mut git(&["write-tree"])).await?;
        let mut c = disowned(app.git(admin), cwd);
        c.args([
            "-c",
            "user.name=colonizer",
            "-c",
            "user.email=colonizer@users.noreply.github.com",
        ])
        .args(["commit-tree", tree.trim(), "-p", "HEAD", "-m", "verification snapshot"]);
        Ok(exec_within(GIT_LIMIT, &mut c).await?.trim().to_string())
    }
    .await;
    let _ = tokio::fs::remove_file(&index).await;
    result
}

/// One file's content at a revision, or `None` when it is absent or unreadable.
async fn file_at(app: &App, admin: &Path, cwd: &Path, rev: &str, path: &str) -> Option<String> {
    let at = format!("{rev}:{path}");
    exec_within(GIT_LIMIT, &mut git_at(app, admin, cwd, &["show", at.as_str()]))
        .await
        .ok()
}

/// One host-side git read against the worktree's admin dir.
async fn read(app: &App, admin: &Path, cwd: &Path, args: &[&str]) -> Result<String> {
    exec_within(GIT_LIMIT, &mut git_at(app, admin, cwd, args)).await
}

/// Deletes an earlier verification's leftovers in the session dir: a temp index from a run that
/// died mid-snapshot, a checkout or report dir from a run that never cleaned up after itself.
async fn sweep_stale(dir: &Path) {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let Ok(name) = entry.file_name().into_string() else { continue };
        let path = entry.path();
        if name.starts_with("verify-index-") {
            let _ = tokio::fs::remove_file(&path).await;
        } else if name.starts_with("verify-checkout") || name.starts_with("verify-report") {
            let _ = tokio::fs::remove_dir_all(&path).await;
        }
    }
}

async fn clean_up(dirs: &[&Path]) {
    for dir in dirs {
        let _ = tokio::fs::remove_dir_all(dir).await;
    }
}

/// Runs the verification for one completion claim and answers the record. Every failure is part
/// of the verdict: git that cannot be read, a missing command or broken infra make the claim
/// unverifiable with the reason; only real disagreements contradict it.
async fn verify_claim(app: &App, s: &Session, runner: &VmRunner) -> Verification {
    let started = Instant::now();
    let mut record = Verification::blank();
    macro_rules! unverifiable {
        ($why:expr) => {{
            record.verdict = Verdict::Unverifiable;
            record.summary = $why;
            return record.finished(started);
        }};
    }
    let configured = s.verify.as_deref().map(str::trim).filter(|v| !v.is_empty()).unwrap_or("auto");
    if configured == "none" {
        record.by_declaration = true;
        unverifiable!("unverifiable by declaration (verify is none)".into());
    }
    // The git observations need a worktree and a base to be against; without either there is
    // nothing mechanical to check, so the claim is unverifiable, never confirmed.
    let (Some(admin), Some(base)) = (s.git_admin_dir.as_deref(), s.base.as_deref()) else {
        unverifiable!("the colony has no worktree or base to verify against".into());
    };
    let (admin, cwd) = (Path::new(admin), app.session_dir(&s.id));
    sweep_stale(&cwd).await;
    let base_ref = format!("origin/{base}");
    let snapshot = match snapshot_work(app, s, admin, &cwd).await {
        Ok(sha) => sha,
        Err(e) => unverifiable!(format!("could not snapshot the worktree: {e:#}")),
    };
    record.snapshot = Some(snapshot.clone());
    let range = format!("{base_ref}..HEAD");
    let (commits, merge_base) = match (
        read(app, admin, &cwd, &["rev-list", "--count", &range]).await,
        read(app, admin, &cwd, &["merge-base", &base_ref, "HEAD"]).await,
    ) {
        (Ok(commits), Ok(merge_base)) => (commits.trim().parse().unwrap_or(0), merge_base.trim().to_string()),
        _ => unverifiable!(format!("could not read the branch against {base_ref}")),
    };
    record.commits = commits;
    let changed = match read(app, admin, &cwd, &["diff", "--name-only", &merge_base, &snapshot]).await {
        Ok(out) => out
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>(),
        Err(e) => unverifiable!(format!("could not diff the snapshot: {e:#}")),
    };
    record.files_changed = changed.iter().take(FILES_CAP).cloned().collect();
    let on_branch = match read(app, admin, &cwd, &["ls-tree", "-r", "--name-only", &snapshot]).await {
        Ok(out) => out.lines().map(str::to_string).collect::<HashSet<_>>(),
        Err(e) => unverifiable!(format!("could not list the snapshot's files: {e:#}")),
    };

    // The claim versus the observation.
    let mut contradictions = Vec::new();
    if changed.is_empty() {
        contradictions.push(format!("branch has no changes against {base}"));
    } else {
        let claim = tokio::fs::read_to_string(cwd.join("out").join("pr.md"))
            .await
            .unwrap_or_default();
        for path in claimed_paths(&claim) {
            // A described path counts against the claim only where it could have existed — its
            // directory on the branch — while neither the branch nor the diff carries the file.
            // An example URL or a gitignored build under an untracked dir is wording, not
            // evidence.
            if on_branch.contains(&path) || changed.contains(&path) {
                continue;
            }
            let tracked_here = |dir: &str| on_branch.iter().any(|t| t.starts_with(&format!("{dir}/")));
            if path.rsplit_once('/').is_some_and(|(dir, _)| tracked_here(dir)) {
                contradictions.push(format!("described `{path}` is not on the branch"));
            }
        }
    }

    // The command: explicit configuration first, then the repository's own declaration on the
    // base branch.
    let files = BaseFiles {
        package_json: file_at(app, admin, &cwd, &base_ref, "package.json").await,
        package_lock: file_at(app, admin, &cwd, &base_ref, "package-lock.json").await.is_some(),
        cargo_toml: file_at(app, admin, &cwd, &base_ref, "Cargo.toml").await.is_some(),
        makefile: file_at(app, admin, &cwd, &base_ref, "Makefile").await,
    };
    let (command, source) = if configured != "auto" {
        (configured.to_string(), "config")
    } else {
        match declared_test_command(&files) {
            Some((source, command)) => (command, source),
            None => (String::new(), ""),
        }
    };
    record.command = (!command.is_empty()).then_some(command.clone());
    record.command_source = (!source.is_empty()).then_some(source.to_string());

    // The colony must not grade its own homework: an `auto` command is the base branch's, so a
    // branch that rewrote the entry defining it would run its own replacement. (`cargo test`
    // has no entry to rewrite, and an explicit command is the operator's choice.)
    let self_graded = if source == "package.json"
        && scripts_test(files.package_json.as_deref())
            != scripts_test(file_at(app, admin, &cwd, &snapshot, "package.json").await.as_deref())
    {
        Some("the branch changes `scripts.test`, the command this check would run")
    } else if source == "Makefile" && files.makefile != file_at(app, admin, &cwd, &snapshot, "Makefile").await {
        Some("the branch changes the `test` target, the command this check would run")
    } else {
        None
    };

    // Contradictions settle it without paying for a VM run; the verdict is the same either way.
    let mut forced = self_graded.map(str::to_string); // an infra-style unverifiable summary
    let mut green = None;
    if contradictions.is_empty() && forced.is_none() {
        if command.is_empty() {
            forced = Some(format!(
                "no test command is known for this repository (nothing usable on {base})"
            ));
        } else {
            match run_tests(app, s, admin, &cwd, &snapshot, &command, runner).await {
                Ok((sandbox, reported, ms)) => {
                    record.tests_ms = Some(ms);
                    record.exit_code = reported;
                    match reported {
                        // The guest's own report decides; the sandbox's exit corroborates.
                        None => forced = Some("the runner did not report an exit code".into()),
                        Some(127) => forced = Some(format!("`{command}` is not present in the colony image (exit 127)")),
                        Some(0) if sandbox == 0 => green = Some(true),
                        Some(0) => forced = Some(format!("the sandbox itself exited {sandbox}")),
                        Some(code) => contradictions.push(format!("`{command}` exited {code} in a fresh checkout")),
                    }
                }
                Err(e) => forced = Some(format!("could not run the tests: {e:#}")),
            }
        }
    }

    record.contradictions = contradictions;
    record.verdict = if forced.is_some() {
        Verdict::Unverifiable
    } else {
        decide(&record.contradictions, green)
    };
    record.summary = forced.unwrap_or_else(|| match record.verdict {
        Verdict::Contradicted => format!(
            "contradicted: {}{}",
            record.contradictions[0],
            match record.contradictions.len() {
                1 => String::new(),
                more => format!(" (and {} more)", more - 1),
            }
        ),
        Verdict::Confirmed => format!("changes passed `{command}` in a fresh checkout"),
        Verdict::Unverifiable => "the tests could not be judged".into(),
    });
    record.finished(started)
}

/// Exports the snapshot into a fresh temp dir under the session directory (no `.git`, nothing
/// shared with the agent's worktree), boots a one-shot microVM from the colony's image with that
/// dir mounted at `/workspace` and a second, empty dir at `/colonizer-verify`, and runs
/// `cd /workspace && (<command>); echo $? > /colonizer-verify/exit`. Answers the sandbox's own
/// exit code, the number the guest reported (the one the verdict trusts), and the elapsed ms. On
/// a timeout the VM is removed here rather than left to `--max-duration`.
async fn run_tests(
    app: &App,
    s: &Session,
    admin: &Path,
    cwd: &Path,
    snapshot: &str,
    command: &str,
    runner: &VmRunner,
) -> Result<(i32, Option<i32>, u64)> {
    let (checkout, report) = (cwd.join("verify-checkout"), cwd.join("verify-report"));
    clean_up(&[&checkout, &report]).await;
    let exported = async {
        tokio::fs::create_dir_all(&checkout).await?;
        tokio::fs::create_dir_all(&report).await?;
        let tar = checkout.join("snapshot.tar");
        let mut archive = git_at(app, admin, cwd, &["archive", "--format=tar", "--output"]);
        archive.arg(&tar).arg(snapshot);
        exec_within(GIT_LIMIT, &mut archive).await?;
        let mut extract = Command::new("tar");
        extract.args(["-xf"]).arg(&tar).arg("-C").arg(&checkout);
        let extracted = exec_within(GIT_LIMIT, &mut extract).await;
        let _ = tokio::fs::remove_file(&tar).await;
        extracted
    }
    .await;
    if let Err(e) = exported {
        clean_up(&[&checkout, &report]).await;
        bail!("could not export a fresh checkout of the snapshot: {e:#}");
    }
    let name = format!("{}-verify", s.sandbox);
    let modules = app.modules.read().await.clone();
    let spec = crate::sandbox::BootSpec {
        name: name.clone(),
        image: s
            .boot_image
            .clone()
            .unwrap_or_else(|| crate::sandbox::configured_image(app, &modules)),
        cpus: s.boot_cpus.unwrap_or(2),
        memory: s.boot_memory.clone().unwrap_or_else(|| "2G".into()),
        root_disk: "16G".into(),
        max_duration: "25m".into(),
        workdir: "/workspace".into(),
        mounts: vec![
            crate::sandbox::Mount {
                source: checkout.clone(),
                target: "/workspace".into(),
                read_only: false,
            },
            crate::sandbox::Mount {
                source: report.clone(),
                target: "/colonizer-verify".into(),
                read_only: false,
            },
        ],
        env: Vec::new(),
        secrets: Vec::new(),
        command: vec![
            "sh".into(),
            "-c".into(),
            format!("cd /workspace && ({command}); echo $? > /colonizer-verify/exit"),
        ],
        ..Default::default()
    };
    let started = Instant::now();
    let outcome = tokio::time::timeout(TEST_LIMIT, runner(spec)).await;
    let (sandbox, reported) = match outcome {
        Ok(Ok(code)) => (code, read_exit_report(&report).await),
        Ok(Err(e)) => {
            clean_up(&[&checkout, &report]).await;
            bail!("{e:#}");
        }
        Err(_) => {
            crate::sandbox::remove(&app.cfg.msb, &name).await;
            clean_up(&[&checkout, &report]).await;
            bail!("the test run timed out after {} minutes", TEST_LIMIT.as_secs() / 60);
        }
    };
    clean_up(&[&checkout, &report]).await;
    Ok((sandbox, reported, started.elapsed().as_millis() as u64))
}

/// The exit number the guest wrote to `/colonizer-verify/exit` (mounted from `report`). `None`
/// when the runner never reported one — no verdict may rest on that.
async fn read_exit_report(report: &Path) -> Option<i32> {
    tokio::fs::read_to_string(report.join("exit")).await.ok()?.trim().parse().ok()
}

/// Runs after every completion claim, whatever the autopilot switch: verifies the claim, records
/// the verdict on the session and as a `verification` chain event, and — only when autopilot was
/// about to publish (`gate_publish`) — lets the verdict gate the publish. The per-runtime lock
/// serialises verifications for one colony, so a second claim that lands mid-run queues behind
/// it and then verifies the newer state.
pub(crate) async fn after_turn(app: Shared, id: String, gate_publish: bool) {
    // A deleted colony must not gain a runtime (and a log file) back just to be verified.
    if app.session(&id).await.is_none() {
        return;
    }
    let rt = app.runtime(&id).await;
    let _serial = rt.verify_lock.lock().await;
    let Some(s) = app.session(&id).await else { return };
    let runner = microsandbox_runner(app.cfg.msb.clone());
    let verification = verify_claim(&app, &s, &runner).await;
    let verdict = verification.verdict;
    let detail = verification.contradictions.join("; ");
    let summary = verification.summary.clone();
    let event = verification.event();
    app.update_session(&id, |x| x.verification = Some(verification)).await;
    app.session_log(
        &id,
        if verdict == Verdict::Contradicted { "warn" } else { "info" },
        format!("verification: {summary}"),
    )
    .await;
    crate::validation::emit_chain(&app, &id, event).await;
    if !gate_publish {
        return;
    }
    use crate::events::Autopilot;
    match crate::events::verdict_step(&verdict) {
        Autopilot::Publish => {
            if crate::authority::external_writes_blocked() {
                app.session_log(&id, "warn", crate::events::AUTOPILOT_BLOCKED.into()).await;
            } else if app.session(&id).await.is_some_and(|s| s.status.is_live()) {
                app.session_log(&id, "info", "autopilot: the claim checked out, publishing".into())
                    .await;
                crate::publish::publish_session(app.clone(), id).await;
            } else {
                app.session_log(&id, "info", "autopilot: not publishing, the colony is no longer live".into())
                    .await;
            }
        }
        Autopilot::Hold(_) => {
            app.session_log(
                &id,
                "warn",
                format!(
                    "autopilot: not publishing, the completion claim was contradicted — {detail}; \
                     press Create PR when the work is ready"
                ),
            )
            .await;
            app.update_session(&id, |x| {
                x.attention =
                    Some(json!({"reason": "autopilot_held", "since": chrono::Utc::now(), "nudges": 0, "detail": detail}));
            })
            .await;
        }
        // verdict_step never waits on a verdict.
        Autopilot::Wait(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::{SessionStatus, tests::app_with_colony};
    use std::path::PathBuf;

    #[test]
    fn the_verdict_never_confirms_without_a_green_run() {
        let contradiction = vec!["branch has no changes against main".to_string()];
        assert_eq!(decide(&[], Some(true)), Verdict::Confirmed);
        // A contradiction wins even over a green run: the branch disagrees with the claim.
        assert_eq!(decide(&contradiction, Some(true)), Verdict::Contradicted);
        assert_eq!(decide(&[], None), Verdict::Unverifiable);
        // A failing run reaches decide as a contradiction (verify_claim pushes one), so this
        // branch is the belt to that braces.
        assert_eq!(decide(&[], Some(false)), Verdict::Unverifiable);
        // The event's shape is the contract the cockpit renders (web/src/types.ts): the record's
        // twelve fields plus its `type`, no more.
        let event = Verification::blank().event();
        assert_eq!(event["type"], "verification");
        assert_eq!(event.as_object().unwrap().len(), 13);
    }

    /// A runner that must never be asked anything: this verdict must not boot a VM.
    fn panicking_runner() -> VmRunner {
        Arc::new(|_| Box::pin(async { panic!("the VM must not boot for this verdict") }))
    }

    /// A runner that answers `sandbox` as the sandbox's own exit and, when `reported` is set,
    /// has the guest's report file say so — the number the verdict trusts.
    fn fake_runner(sandbox: i32, reported: Option<i32>) -> VmRunner {
        Arc::new(move |spec| {
            Box::pin(async move {
                if let (Some(dir), Some(code)) = (spec.mounts.iter().find(|m| m.target == "/colonizer-verify"), reported) {
                    tokio::fs::write(dir.source.join("exit"), code.to_string()).await?;
                }
                Ok(sandbox)
            })
        })
    }

    fn dead_runner() -> VmRunner {
        Arc::new(|_| Box::pin(async { anyhow::bail!("microsandbox is not installed") }))
    }

    #[test]
    fn the_command_resolution_order_is_npm_then_cargo_then_make() {
        let package = |test: &str| Some(format!(r#"{{"scripts": {{"test": "{test}"}}}}"#));
        let base = |package_json, package_lock, cargo_toml, makefile| BaseFiles {
            package_json,
            package_lock,
            cargo_toml,
            makefile,
        };
        // (files, the command they declare) — npm first, then cargo, then make, and npm's
        // placeholder script (or no files at all, or a Makefile without a `test:` target)
        // declares nothing.
        let cases: Vec<(BaseFiles, Option<(&'static str, &str)>)> = vec![
            (
                base(package("node --test"), true, true, Some("test:\n".into())),
                Some(("package.json", "npm ci && npm test")),
            ),
            // No lockfile on the base branch means npm install, not npm ci.
            (
                base(package("jest"), false, false, None),
                Some(("package.json", "npm install && npm test")),
            ),
            (
                base(package(r#"echo \"Error: no test specified\" && exit 1"#), true, true, None),
                Some(("Cargo.toml", "cargo test")),
            ),
            (
                base(None, false, false, Some("build:\n\techo hi\ntest: build\n".into())),
                Some(("Makefile", "make test")),
            ),
            (base(None, false, false, Some("build:\n".into())), None),
            (base(None, false, false, None), None),
        ];
        for (files, want) in cases {
            assert_eq!(
                declared_test_command(&files),
                want.map(|(source, command)| (source, command.to_string())),
                "{files:?}"
            );
        }
    }

    #[test]
    fn claimed_paths_are_backticked_path_like_tokens() {
        let claim = "Changed `src/lib.rs` and `web/src/a.tsx:42`, kept `docs/guide.md:12-30` in sync, \
                     touched `./scripts/run.py`; not `README.md`, `a b/c`, `src/*.rs` or `https://x/y.md`.";
        assert_eq!(
            claimed_paths(claim),
            vec![
                "src/lib.rs".to_string(),
                "web/src/a.tsx".to_string(),
                "docs/guide.md".to_string(),
                "scripts/run.py".to_string(),
            ]
        );
        assert!(claimed_paths("no backticks, no paths").is_empty());
    }

    /// Runs `git` synchronously against a fixture repo — setup and inspection, not the code
    /// under test. The `GIT_DIR`/`GIT_WORK_TREE`/`GIT_INDEX_FILE` a colony sandbox exports for
    /// its own worktree are dropped, so the fixture is the only repo git sees.
    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("could not run git {args:?} in {}: {e}", dir.display()));
        assert!(out.status.success(), "git {args:?} in {} failed", dir.display());
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn git_commit(dir: &Path, message: &str) {
        git(
            dir,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                message,
            ],
        );
    }

    /// A colony worktree fixture: a base commit on `origin/main`, a branch a commit ahead of it
    /// (under `src/`, so described paths have a tracked directory to sit in), plus one
    /// uncommitted file — and the session pointing at it. `verify` names the setting stored on
    /// the colony; `pr` is the pull request description the claim is read from.
    async fn worktree_fixture(app: &crate::Shared, verify: Option<&str>, pr: &str, with_changes: bool) -> PathBuf {
        let root = std::env::temp_dir().join(format!("colonizer-verify-{}", short_id()));
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git_commit(&repo, "base");
        git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(&repo, &["checkout", "-q", "-b", "colonizer/work"]);
        if with_changes {
            std::fs::create_dir_all(repo.join("src")).unwrap();
            std::fs::write(repo.join("src/real.txt"), "on the branch\n").unwrap();
            git(&repo, &["add", "-A"]);
            git_commit(&repo, "work");
            std::fs::write(repo.join("uncommitted.txt"), "work in progress\n").unwrap();
        }
        let out = app.session_dir("abc").join("out");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("pr.md"), pr).unwrap();
        app.update_session("abc", |x| {
            x.worktree = repo.display().to_string();
            x.git_admin_dir = Some(repo.join(".git").display().to_string());
            x.base = Some("main".into());
            x.branch = "colonizer/work".into();
            x.verify = verify.map(str::to_string);
        })
        .await;
        repo
    }

    async fn verify(app: &crate::Shared, runner: &VmRunner) -> Verification {
        let s = app.session("abc").await.expect("the colony exists");
        verify_claim(app, &s, runner).await
    }

    /// The heart of it: the snapshot captures the colony's uncommitted work, the agent's
    /// worktree/index/HEAD survive it untouched, and the verdict follows the fresh run — green
    /// confirms, a nonzero exit contradicts.
    #[tokio::test]
    async fn the_snapshot_captures_uncommitted_work_and_leaves_the_worktree_alone() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        let repo = worktree_fixture(&app, Some("true"), "did the work", true).await;
        let (head, index, status) = (
            git(&repo, &["rev-parse", "HEAD"]),
            git(&repo, &["ls-files", "-s"]),
            git(&repo, &["status", "--porcelain"]),
        );
        let v = verify(&app, &fake_runner(0, Some(0))).await;
        assert_eq!(v.verdict, Verdict::Confirmed, "{v:?}");
        assert_eq!(v.command.as_deref(), Some("true"));
        assert_eq!(
            v.command_source.as_deref(),
            Some("config"),
            "the stored command came from configuration"
        );
        assert_eq!(v.exit_code, Some(0));
        assert!(v.tests_ms.is_some(), "the run is timed");
        assert!(v.commits >= 1, "the branch's own commit is counted");
        assert!(
            v.files_changed.iter().any(|f| f == "uncommitted.txt"),
            "{:?}",
            v.files_changed
        );
        assert!(v.snapshot.as_deref().is_some_and(|sha| sha.len() == 40), "{:?}", v.snapshot);
        // The agent's worktree is exactly as it was: same HEAD, same index, same status.
        assert_eq!(git(&repo, &["rev-parse", "HEAD"]), head);
        assert_eq!(git(&repo, &["ls-files", "-s"]), index, "the agent's index is untouched");
        assert_eq!(git(&repo, &["status", "--porcelain"]), status);
        let leftovers: Vec<_> = std::fs::read_dir(app.session_dir("abc"))
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .filter(|n| n.starts_with("verify-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp index, checkout and report are cleaned up: {leftovers:?}"
        );

        let red = verify(&app, &fake_runner(3, Some(3))).await;
        assert_eq!(red.verdict, Verdict::Contradicted, "{red:?}");
        assert_eq!(red.exit_code, Some(3));
        assert!(red.summary.contains("exited 3 in a fresh checkout"), "{}", red.summary);
        assert_eq!(red.contradictions, vec!["`true` exited 3 in a fresh checkout".to_string()]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn an_empty_branch_or_an_absent_described_path_contradicts_the_claim() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        // A green run cannot save a claim describing a file the branch does not carry — but only
        // a path whose directory exists on the branch: `example.com/foo.md` and a gitignored
        // `dist/bundle.js` are the claim's wording, not evidence against it.
        worktree_fixture(
            &app,
            Some("true"),
            "rewrote `src/absent.rs`, see `example.com/foo.md`, built `dist/bundle.js`",
            true,
        )
        .await;
        let v = verify(&app, &fake_runner(0, Some(0))).await;
        assert_eq!(v.verdict, Verdict::Contradicted, "{v:?}");
        assert_eq!(v.exit_code, None, "no VM run is paid for once the git state contradicts");
        assert_eq!(
            v.contradictions,
            vec!["described `src/absent.rs` is not on the branch".to_string()]
        );

        // A branch with nothing on it against the base contradicts the claim by itself.
        worktree_fixture(&app, Some("true"), "did all the work", false).await;
        let v = verify(&app, &fake_runner(0, Some(0))).await;
        assert_eq!(v.verdict, Verdict::Contradicted, "{v:?}");
        assert_eq!(
            v.contradictions,
            vec!["branch has no changes against main".to_string()],
            "{v:?}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// The colony must not grade its own homework: the command is the base branch's, so a branch
    /// that rewrote `scripts.test` would run its own replacement. The claim is unverifiable, the
    /// rewrite said plainly — and no VM is booted to run the doctored command.
    #[tokio::test]
    async fn a_branch_that_rewrites_the_test_entry_cannot_grade_itself() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        let repo = worktree_fixture(&app, None, "did the work", false).await;
        let package = |test: &str| format!(r#"{{"scripts": {{"test": "{test}"}}}}"#);
        std::fs::write(repo.join("package.json"), package("node --test")).unwrap();
        git(&repo, &["add", "-A"]);
        git_commit(&repo, "declare");
        git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        std::fs::write(repo.join("package.json"), package("true")).unwrap();
        let v = verify(&app, &panicking_runner()).await;
        assert_eq!(v.verdict, Verdict::Unverifiable, "{v:?}");
        assert!(v.summary.contains("changes `scripts.test`"), "{}", v.summary);
        assert_eq!(
            v.command.as_deref(),
            Some("npm install && npm test"),
            "no lockfile on the base"
        );
        assert_eq!(v.exit_code, None, "nothing ran");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn broken_infra_or_a_missing_command_leaves_the_claim_unverifiable() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        // A colony with no worktree or base yet has nothing mechanical to verify against.
        let v = verify(&app, &fake_runner(0, Some(0))).await;
        assert_eq!(v.verdict, Verdict::Unverifiable, "{v:?}");
        assert_eq!(v.snapshot, None, "no git was read");

        // `verify: none` opts out by declaration: no git, no VM, no command.
        app.update_session("abc", |x| x.verify = Some("none".into())).await;
        let v = verify(&app, &panicking_runner()).await;
        assert_eq!(v.verdict, Verdict::Unverifiable);
        assert!(v.by_declaration, "{v:?}");
        assert_eq!(v.summary, "unverifiable by declaration (verify is none)");
        assert_eq!(v.command, None);

        worktree_fixture(&app, Some("true"), "did the work", true).await;
        // The runner failing (no msb, no boot) is infra, not the colony's fault.
        let v = verify(&app, &dead_runner()).await;
        assert_eq!(v.verdict, Verdict::Unverifiable, "{v:?}");
        assert!(v.summary.contains("could not run the tests"), "{}", v.summary);
        // Exit 127 in the image names a command the colony image does not carry.
        let v = verify(&app, &fake_runner(127, Some(127))).await;
        assert_eq!(v.verdict, Verdict::Unverifiable, "{v:?}");
        assert!(v.summary.contains("exit 127"), "{}", v.summary);
        // A sandbox that exits cleanly without the guest's report has not said anything.
        let v = verify(&app, &fake_runner(0, None)).await;
        assert_eq!(v.verdict, Verdict::Unverifiable, "{v:?}");
        assert!(v.summary.contains("did not report an exit code"), "{}", v.summary);
        // And a guest's 0 does not outweigh a sandbox that failed itself.
        let v = verify(&app, &fake_runner(2, Some(0))).await;
        assert_eq!(v.verdict, Verdict::Unverifiable, "{v:?}");
        assert!(v.summary.contains("the sandbox itself exited 2"), "{}", v.summary);

        // `auto` with nothing usable on the base branch names no command to run.
        worktree_fixture(&app, None, "did the work", true).await;
        let v = verify(&app, &panicking_runner()).await;
        assert_eq!(v.verdict, Verdict::Unverifiable, "{v:?}");
        assert!(v.summary.contains("no test command is known"), "{}", v.summary);
        assert_eq!(v.command, None);
        let _ = std::fs::remove_dir_all(root);
    }
}
