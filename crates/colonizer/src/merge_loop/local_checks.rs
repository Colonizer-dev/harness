//! Issue #969: the merge-train loop's local checks, for when GitHub CI cannot run at all — every
//! job refused at start because the org's Actions billing failed, no runner took it, or Actions is
//! switched off. "Could not run" is told apart from "ran and failed" by GitHub's own words on the
//! refused jobs ([`unavailable_reason`]): one check that ran and failed, one still running, and
//! there is no unavailability to act on. Only then, and only in a repository that opted in
//! (`.colonizer/merge.toml`'s `local_checks`, or the loop's `local_checks` list with the commands
//! detected from the stack), the loop runs the checks itself in a one-shot microVM on the pull
//! request's head merged with the current base, posts the result as the `colonizer/local-checks`
//! commit status, and merges only when every command passed.

use crate::{
    App, Shared,
    util::{exec_within, truncate},
    verify::{self, BaseFiles},
};
use serde::Deserialize;
use serde_json::Value;
use std::{collections::BTreeMap, path::Path, time::Duration};

/// The commit status the loop posts on a pull request's head.
pub(crate) const CONTEXT: &str = "colonizer/local-checks";
/// The repository's merge-train knobs, read from the base branch, never from the pull request.
pub(crate) const MERGE_TOML: &str = ".colonizer/merge.toml";
const GIT_LIMIT: Duration = Duration::from_secs(120);
const RUN_LIMIT: Duration = Duration::from_secs(45 * 60);
/// Check runs whose annotations are read per head; past it the rest count as failed, which holds.
const ANNOTATION_READS: usize = 30;

/// What GitHub says on a job it refused to start, and how the loop names it. Specific phrases first.
const NOT_STARTED: &[(&str, &str)] = &[
    ("spending limit", "the Actions spending limit was reached"),
    ("account payments have failed", "Actions billing failed"),
    ("billing issue", "Actions billing failed"),
    ("not acquired by runner", "no runner picked the jobs up"),
    ("no runner matching", "no runner matches the jobs' labels"),
    ("the job was not started", "GitHub did not start the jobs"),
];

/// The not-started reason in a refused job's texts (title, summary, annotations), with GitHub's
/// own words after it; `None` when the job ran.
fn not_started(texts: &[&str]) -> Option<String> {
    for text in texts {
        let lower = text.to_ascii_lowercase();
        if let Some((_, label)) = NOT_STARTED.iter().find(|(phrase, _)| lower.contains(phrase)) {
            return Some(format!("{label}: \"{}\"", truncate(text.trim(), 160)));
        }
    }
    None
}

/// Why GitHub CI could not run on a commit, from its check runs (`GET …/commits/{sha}/check-runs`),
/// the annotations of the failed ones by check-run id, and its combined status. `Some` only when
/// at least one job was refused at start and nothing else is red or unfinished: a check that ran
/// and failed is a real failure, and a running one may still go either way. The loop's own
/// [`CONTEXT`] is not a reading of CI.
pub(crate) fn unavailable_reason(check_runs: &Value, annotations: &BTreeMap<u64, Vec<String>>, status: &Value) -> Option<String> {
    let word = |v: &Value, key: &str| v[key].as_str().unwrap_or_default().trim().to_ascii_lowercase();
    let mut reason = None;
    for run in check_runs["check_runs"].as_array().into_iter().flatten() {
        if run["name"].as_str() == Some(CONTEXT) {
            continue;
        }
        if word(run, "status") != "completed" {
            return None;
        }
        if matches!(word(run, "conclusion").as_str(), "success" | "neutral" | "skipped") {
            continue;
        }
        let output = &run["output"];
        let mut texts: Vec<&str> = ["title", "summary", "text"]
            .iter()
            .filter_map(|k| output[*k].as_str())
            .collect();
        if let Some(notes) = run["id"].as_u64().and_then(|id| annotations.get(&id)) {
            texts.extend(notes.iter().map(String::as_str));
        }
        reason.get_or_insert(not_started(&texts)?);
    }
    for s in status["statuses"].as_array().into_iter().flatten() {
        if s["context"].as_str() != Some(CONTEXT) && word(s, "state") != "success" {
            return None;
        }
    }
    reason
}

/// Why a pull request has no checks at all, when that is Actions being off rather than a
/// repository without CI: Actions disabled (`GET …/actions/permissions`), or every workflow it has
/// disabled (`GET …/actions/workflows`).
pub(crate) fn actions_off(permissions: &Value, workflows: &Value) -> Option<String> {
    if permissions["enabled"] == Value::Bool(false) {
        return Some("GitHub Actions is disabled for this repository".to_string());
    }
    let list = workflows["workflows"].as_array().filter(|w| !w.is_empty())?;
    list.iter()
        .all(|w| w["state"].as_str().is_some_and(|s| s.starts_with("disabled")))
        .then(|| "every GitHub Actions workflow in this repository is disabled".to_string())
}

/// `.colonizer/merge.toml`, every key optional.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct MergeToml {
    /// The commands to run when GitHub CI cannot; `[]` keeps local checks off here.
    pub local_checks: Option<Vec<String>>,
    /// Issue #968: how conflicts are resolved (`merge_loop/resolve.rs`).
    pub resolve: Option<super::resolve::ResolveToml>,
}

/// Whether, and with what, a repository's local checks run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LocalChecks {
    Off(String),
    On { commands: Vec<String>, source: String },
}

/// The repository's file wins: its own list (`[]` switches local checks off), else — only when the
/// loop's `local_checks` lists the repository or its org — the commands detected from the stack.
pub(crate) fn resolve(merge_toml: Option<&str>, opted_in: bool, detected: Vec<String>) -> LocalChecks {
    let file = match merge_toml.map(toml::from_str::<MergeToml>) {
        Some(Err(e)) => return LocalChecks::Off(format!("{MERGE_TOML} does not parse ({})", e.message())),
        Some(Ok(f)) => f,
        None => MergeToml::default(),
    };
    match file.local_checks {
        Some(list) => {
            let commands: Vec<String> = list
                .into_iter()
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty())
                .collect();
            if commands.is_empty() {
                LocalChecks::Off(format!("{MERGE_TOML} switches local checks off here"))
            } else {
                LocalChecks::On {
                    commands,
                    source: MERGE_TOML.to_string(),
                }
            }
        }
        None if !opted_in => LocalChecks::Off(format!(
            "local checks are off here: list them in {MERGE_TOML} (local_checks), or add the repository to the loop's local_checks"
        )),
        None if detected.is_empty() => LocalChecks::Off(format!(
            "no checks could be detected for this repository's stack; list them in {MERGE_TOML} (local_checks)"
        )),
        None => LocalChecks::On {
            commands: detected,
            source: "detected from the stack".to_string(),
        },
    }
}

/// The checks a stack implies, from the base branch's root files: for Cargo, fmt, clippy with
/// warnings denied and the tests; for a JavaScript package, the install-and-test command the
/// verifier would run (`verify::declared_test_command`), then its `typecheck`, `lint` and `build`
/// scripts when it has them; `make test` when neither applies.
pub(crate) fn detected(files: &BaseFiles) -> Vec<String> {
    let mut out = Vec::new();
    if files.cargo_toml {
        out.extend([
            "cargo fmt --all --check".to_string(),
            "cargo clippy --workspace --all-targets -- -D warnings".to_string(),
            "cargo test --workspace".to_string(),
        ]);
    }
    let js = BaseFiles {
        package_json: files.package_json.clone(),
        lockfiles: files.lockfiles.clone(),
        yarn_berry_lock: files.yarn_berry_lock,
        ..BaseFiles::default()
    };
    if let Some(declared) = verify::declared_test_command(&js) {
        let last = declared.command.rsplit(" && ").next().unwrap_or_default();
        let runner = last
            .strip_suffix(" run test")
            .or_else(|| last.strip_suffix(" test"))
            .unwrap_or("npm")
            .to_string();
        let scripts: Value = serde_json::from_str(files.package_json.as_deref().unwrap_or("{}")).unwrap_or_default();
        out.push(declared.command);
        for script in ["typecheck", "lint", "build"] {
            if scripts["scripts"][script].is_string() {
                out.push(format!("{runner} run {script}"));
            }
        }
    }
    if out.is_empty()
        && let Some(make) = verify::declared_test_command(&BaseFiles {
            makefile: files.makefile.clone(),
            ..BaseFiles::default()
        })
    {
        out.push(make.command);
    }
    out
}

/// The guest's script: each command in turn in the merged checkout, combined output appended to the
/// report mount, the failing step and its exit number written where only this harness reads them.
pub(crate) fn guest_script(commands: &[String]) -> String {
    let mut script = String::from("cd /workspace || { echo 127 > /colonizer-checks/exit; exit 0; }\n");
    for (i, command) in commands.iter().enumerate() {
        script.push_str(&format!(
            "echo {} > /colonizer-checks/step\n({command}) >>/colonizer-checks/output 2>&1 || {{ echo $? > /colonizer-checks/exit; exit 0; }}\n",
            i + 1
        ));
    }
    script.push_str("echo 0 > /colonizer-checks/exit\n");
    script
}

/// What one local run answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LocalRun {
    /// Every command passed on the head merged with `base_sha`.
    Passed { base_sha: String },
    Failed {
        base_sha: String,
        command: String,
        tail: String,
    },
    /// Nothing to judge: a conflict with the base, a head that moved, broken infra.
    Unrunnable(String),
}

/// The verdict from the guest's own report: green needs its exit `0` after the last step.
pub(crate) fn outcome(commands: &[String], base_sha: String, exit: Option<i32>, step: Option<usize>, tail: String) -> LocalRun {
    match (exit, step) {
        (Some(0), Some(n)) if n == commands.len() => LocalRun::Passed { base_sha },
        (Some(code), Some(n)) if code != 0 && (1..=commands.len()).contains(&n) => LocalRun::Failed {
            base_sha,
            command: commands[n - 1].clone(),
            tail,
        },
        _ => LocalRun::Unrunnable("the checks never reported a result".to_string()),
    }
}

// ---------------------------------------------------------------------------------------------
// The real GitHub and microVM, behind the loop's `Ops`.
// ---------------------------------------------------------------------------------------------

async fn json(app: &App, path: &str) -> Value {
    crate::github::gh_get_json(app, path).await.unwrap_or(Value::Null)
}

/// Why CI could not run on `sha`, or `None` (it ran, is running, or could not be read).
pub(super) async fn head_unavailable(app: &App, repo: &str, sha: &str, no_checks: bool) -> Option<String> {
    if no_checks {
        let permissions = json(app, &format!("repos/{repo}/actions/permissions")).await;
        let workflows = json(app, &format!("repos/{repo}/actions/workflows?per_page=100")).await;
        return actions_off(&permissions, &workflows);
    }
    let runs = json(app, &format!("repos/{repo}/commits/{sha}/check-runs?per_page=100")).await;
    let mut annotations = BTreeMap::new();
    let failed = runs["check_runs"].as_array().into_iter().flatten().filter(|r| {
        r["output"]["annotations_count"].as_u64().unwrap_or(0) > 0
            && !matches!(r["conclusion"].as_str(), Some("success" | "neutral" | "skipped"))
    });
    for run in failed.take(ANNOTATION_READS) {
        let Some(id) = run["id"].as_u64() else { continue };
        let notes = json(app, &format!("repos/{repo}/check-runs/{id}/annotations")).await;
        let messages = notes.as_array().into_iter().flatten();
        annotations.insert(
            id,
            messages.filter_map(|n| n["message"].as_str().map(str::to_string)).collect(),
        );
    }
    let status = json(app, &format!("repos/{repo}/commits/{sha}/status")).await;
    unavailable_reason(&runs, &annotations, &status)
}

/// The repository's local-check config, read from the base branch of the mothership's bare clone.
pub(super) async fn config(app: &Shared, repo: &str, base: &str, opted_in: bool) -> Result<LocalChecks, String> {
    let bare = crate::code::ensure_bare(app, repo).await.map_err(|e| format!("{e:#}"))?;
    let show = |path: &str| {
        let mut c = app.git(&bare);
        c.args(["show", &format!("refs/remotes/origin/{base}:{path}")]);
        c
    };
    let file = |path: &'static str| async move { exec_within(GIT_LIMIT, &mut show(path)).await.ok() };
    let merge_toml = file(MERGE_TOML).await;
    if merge_toml.is_some() || !opted_in {
        return Ok(resolve(merge_toml.as_deref(), opted_in, Vec::new()));
    }
    let mut ls = app.git(&bare);
    ls.args(["ls-tree", "--name-only", &format!("refs/remotes/origin/{base}")]);
    let names = exec_within(GIT_LIMIT, &mut ls).await.map_err(|e| format!("{e:#}"))?;
    let names: Vec<&str> = names.lines().collect();
    let files = BaseFiles {
        package_json: file("package.json").await,
        lockfiles: verify::JS_LOCKFILES
            .iter()
            .filter(|l| names.contains(l))
            .map(|l| l.to_string())
            .collect(),
        yarn_berry_lock: names.contains(&"yarn.lock") && file("yarn.lock").await.is_some_and(|y| y.contains("__metadata:")),
        cargo_toml: names.contains(&"Cargo.toml"),
        makefile: file("Makefile").await,
        ..BaseFiles::default()
    };
    Ok(resolve(None, true, detected(&files)))
}

/// Fetches the base and the pull request's head into the bare clone, merges them on the host with
/// `git merge-tree` (objects only: nothing checked out, no hook or filter runs), exports the merged
/// tree and runs [`guest_script`] over it in a one-shot microVM with the colony image.
pub(super) async fn run(app: &Shared, repo: &str, pr: u64, head: &str, base: &str, commands: &[String]) -> LocalRun {
    let fail = |what: &str, e: anyhow::Error| LocalRun::Unrunnable(format!("{what}: {}", truncate(&format!("{e:#}"), 300)));
    let bare = match crate::code::ensure_bare(app, repo).await {
        Ok(b) => b,
        Err(e) => return fail("the clone could not be fetched", e),
    };
    let pr_ref = format!("refs/colonizer/local-checks/{pr}");
    let base_ref = format!("refs/remotes/origin/{base}");
    let mut fetch = app.git_authed(&bare);
    fetch.args([
        "fetch",
        "--quiet",
        "origin",
        &format!("+refs/heads/{base}:{base_ref}"),
        &format!("+refs/pull/{pr}/head:{pr_ref}"),
    ]);
    if let Err(e) = exec_within(GIT_LIMIT, &mut fetch).await {
        return fail("the head and base could not be fetched", e);
    }
    let git = |args: &[&str]| {
        let mut c = app.git(&bare);
        c.args(args);
        c
    };
    let rev = |r: String| async move {
        exec_within(GIT_LIMIT, &mut git(&["rev-parse", &r]))
            .await
            .map(|s| s.trim().to_string())
    };
    let (Ok(fetched), Ok(base_sha)) = (rev(pr_ref).await, rev(base_ref).await) else {
        return LocalRun::Unrunnable("the fetched head or base could not be read".to_string());
    };
    if fetched != head {
        return LocalRun::Unrunnable("the pull request's head moved; it is checked again on its new head".to_string());
    }
    let tree = match exec_within(
        GIT_LIMIT,
        &mut git(&["merge-tree", "--write-tree", "--no-messages", &base_sha, head]),
    )
    .await
    {
        Ok(out) => out.lines().next().unwrap_or_default().trim().to_string(),
        Err(_) => return LocalRun::Unrunnable("the pull request conflicts with its base".to_string()),
    };
    let dir = app
        .cfg
        .data_dir
        .join("local-checks")
        .join(format!("{}-{pr}", repo.replace('/', "-")));
    let (checkout, report) = (dir.join("checkout"), dir.join("report"));
    let _ = tokio::fs::remove_dir_all(&dir).await;
    let result = vm(app, &bare, &tree, &checkout, &report, commands).await;
    let read = |name: &str| std::fs::read_to_string(report.join(name)).unwrap_or_default();
    let (exit, step) = (read("exit").trim().parse().ok(), read("step").trim().parse().ok());
    let output = std::fs::read_to_string(report.join("output")).unwrap_or_default();
    let tail: Vec<&str> = output.lines().rev().take(40).collect();
    let tail = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
    let _ = tokio::fs::remove_dir_all(&dir).await;
    match result {
        Ok(()) => outcome(commands, base_sha, exit, step, tail),
        Err(e) => fail("the microVM run failed", e),
    }
}

async fn vm(app: &Shared, bare: &Path, tree: &str, checkout: &Path, report: &Path, commands: &[String]) -> anyhow::Result<()> {
    tokio::fs::create_dir_all(checkout).await?;
    tokio::fs::create_dir_all(report).await?;
    let tar = report.join("tree.tar");
    let mut archive = app.git(bare);
    archive.args(["archive", "--format=tar", "--output"]).arg(&tar).arg(tree);
    exec_within(GIT_LIMIT, &mut archive).await?;
    let mut extract = tokio::process::Command::new("tar");
    extract.arg("-xf").arg(&tar).arg("-C").arg(checkout);
    exec_within(GIT_LIMIT, &mut extract).await?;
    tokio::fs::remove_file(&tar).await?;
    let modules = app.modules.read().await.clone();
    let name = format!("local-checks-{}", crate::util::short_id());
    let mount = |source: &Path, target: &str| crate::sandbox::Mount {
        source: source.to_path_buf(),
        target: target.into(),
        read_only: false,
    };
    let spec = crate::sandbox::BootSpec {
        name: name.clone(),
        image: crate::sandbox::configured_image(app, &modules),
        cpus: 2,
        memory: "4G".into(),
        root_disk: "24G".into(),
        max_duration: "45m".into(),
        workdir: "/workspace".into(),
        mounts: vec![mount(checkout, "/workspace"), mount(report, "/colonizer-checks")],
        // The public-internet profile a colony boots with, so installs can fetch; nothing of the harness's.
        net_profiles: vec!["public".into()],
        command: vec!["sh".into(), "-c".into(), guest_script(commands)],
        ..Default::default()
    };
    match tokio::time::timeout(RUN_LIMIT, crate::sandbox::run_once(&app.cfg.msb, &spec)).await {
        Ok(result) => result.map(|_| ()),
        Err(_) => {
            crate::sandbox::remove(&app.cfg.msb, &name).await;
            anyhow::bail!("the checks timed out after {} minutes", RUN_LIMIT.as_secs() / 60)
        }
    }
}

/// Posts the [`CONTEXT`] commit status on a head.
pub(super) async fn post_status(app: &App, repo: &str, sha: &str, state: &str, description: &str) -> Result<(), String> {
    let mut gh = app.gh([
        "api".to_string(),
        "-X".into(),
        "POST".into(),
        format!("repos/{repo}/statuses/{sha}"),
        "-f".into(),
        format!("state={state}"),
        "-f".into(),
        format!("context={CONTEXT}"),
        "-f".into(),
        format!("description={}", truncate(description, 139)),
    ]);
    exec_within(Duration::from_secs(60), &mut gh)
        .await
        .map(|_| ())
        .map_err(|e| format!("{e:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const BILLING: &str = "The job was not started because recent account payments have failed or your spending limit needs to be increased. Please check the 'Billing & plans' section in your settings.";

    fn run(id: u64, conclusion: &str) -> Value {
        json!({"id": id, "name": format!("job-{id}"), "status": "completed", "conclusion": conclusion, "output": {"title": null, "summary": null, "annotations_count": 1}})
    }

    #[test]
    fn only_jobs_github_refused_to_start_read_as_unavailable() {
        let runs = json!({"check_runs": [run(1, "failure"), run(2, "failure"), run(3, "success")]});
        let notes = |a: &str, b: &str| BTreeMap::from([(1, vec![a.to_string()]), (2, vec![b.to_string()])]);
        let none = json!({"statuses": []});
        let reason = unavailable_reason(&runs, &notes(BILLING, BILLING), &none).expect("billing reads as unavailable");
        assert!(reason.starts_with("the Actions spending limit was reached"), "{reason}");
        // One job that ran and failed is a real failure, whatever the others say.
        assert_eq!(
            unavailable_reason(&runs, &notes(BILLING, "Process completed with exit code 1."), &none),
            None
        );
        // A failed job without GitHub's words is a real failure too.
        assert_eq!(
            unavailable_reason(&runs, &BTreeMap::from([(1, vec![BILLING.to_string()])]), &none),
            None
        );
        // Something still running may go either way: wait.
        let running = json!({"check_runs": [run(1, "failure"), {"id": 9, "status": "in_progress", "conclusion": null}]});
        assert_eq!(unavailable_reason(&running, &notes(BILLING, BILLING), &none), None);
        // A red commit status from anyone but the loop is a real failure; the loop's own is not CI.
        let theirs = json!({"statuses": [{"context": "ci/other", "state": "failure"}]});
        assert_eq!(unavailable_reason(&runs, &notes(BILLING, BILLING), &theirs), None);
        let ours = json!({"statuses": [{"context": CONTEXT, "state": "failure"}]});
        assert!(unavailable_reason(&runs, &notes(BILLING, BILLING), &ours).is_some());
        // Nothing refused: nothing unavailable.
        assert_eq!(
            unavailable_reason(&json!({"check_runs": [run(3, "success")]}), &BTreeMap::new(), &none),
            None
        );
        let runner = BTreeMap::from([
            (
                1,
                vec!["The job was not acquired by Runner of type hosted even after multiple attempts".to_string()],
            ),
            (2, vec![BILLING.to_string()]),
        ]);
        assert!(
            unavailable_reason(&runs, &runner, &none)
                .unwrap()
                .starts_with("no runner picked the jobs up")
        );
    }

    #[test]
    fn no_checks_reads_as_unavailable_only_when_actions_is_off() {
        assert!(actions_off(&json!({"enabled": false}), &Value::Null).is_some());
        let wf = |states: &[&str]| json!({"workflows": states.iter().map(|s| json!({"state": s})).collect::<Vec<_>>()});
        assert!(actions_off(&json!({"enabled": true}), &wf(&["disabled_manually", "disabled_inactivity"])).is_some());
        assert_eq!(
            actions_off(&json!({"enabled": true}), &wf(&["active", "disabled_manually"])),
            None
        );
        // A repository with no workflows has no CI to be unavailable.
        assert_eq!(actions_off(&json!({"enabled": true}), &wf(&[])), None);
        assert_eq!(actions_off(&Value::Null, &Value::Null), None);
    }

    #[test]
    fn the_repository_file_wins_and_the_loop_list_falls_back_to_the_stack() {
        let detected = vec!["cargo test".to_string()];
        let on = |commands: &[&str], source: &str| LocalChecks::On {
            commands: commands.iter().map(|c| c.to_string()).collect(),
            source: source.to_string(),
        };
        assert_eq!(
            resolve(Some("local_checks = ['npm ci', ' npm test ']"), false, Vec::new()),
            on(&["npm ci", "npm test"], MERGE_TOML)
        );
        assert!(matches!(
            resolve(Some("local_checks = []"), true, detected.clone()),
            LocalChecks::Off(_)
        ));
        assert!(
            matches!(resolve(Some("local_checks = 'oops'"), true, detected.clone()), LocalChecks::Off(why) if why.contains("does not parse"))
        );
        assert!(
            matches!(resolve(None, false, detected.clone()), LocalChecks::Off(_)),
            "off unless opted in"
        );
        assert_eq!(
            resolve(Some(""), true, detected.clone()),
            on(&["cargo test"], "detected from the stack")
        );
        assert!(matches!(resolve(None, true, Vec::new()), LocalChecks::Off(_)));
    }

    #[test]
    fn stack_defaults_cover_cargo_and_the_package_scripts() {
        let cargo = BaseFiles {
            cargo_toml: true,
            ..BaseFiles::default()
        };
        assert_eq!(detected(&cargo).len(), 3);
        assert!(detected(&cargo)[1].contains("-D warnings"));
        let npm = BaseFiles {
            package_json: Some(
                r#"{"scripts": {"test": "vitest run", "typecheck": "tsc --noEmit", "build": "vite build"}}"#.into(),
            ),
            lockfiles: vec!["package-lock.json".into()],
            ..BaseFiles::default()
        };
        assert_eq!(
            detected(&npm),
            vec!["npm ci && npm test", "npm run typecheck", "npm run build"]
        );
        let make = BaseFiles {
            makefile: Some("test:\n\tgo test ./...\n".into()),
            ..BaseFiles::default()
        };
        assert_eq!(detected(&make), vec!["make test"]);
        assert!(detected(&BaseFiles::default()).is_empty());
    }

    #[test]
    fn green_needs_the_guest_to_report_every_step() {
        let commands = vec!["a".to_string(), "b".to_string()];
        assert_eq!(
            outcome(&commands, "m".into(), Some(0), Some(2), String::new()),
            LocalRun::Passed { base_sha: "m".into() }
        );
        assert!(
            matches!(outcome(&commands, "m".into(), Some(1), Some(2), "boom".into()), LocalRun::Failed { command, .. } if command == "b")
        );
        assert!(matches!(
            outcome(&commands, "m".into(), Some(0), Some(1), String::new()),
            LocalRun::Unrunnable(_)
        ));
        assert!(matches!(
            outcome(&commands, "m".into(), None, None, String::new()),
            LocalRun::Unrunnable(_)
        ));
        let script = guest_script(&commands);
        assert!(script.contains("(a) >>/colonizer-checks/output") && script.ends_with("echo 0 > /colonizer-checks/exit\n"));
    }
}
