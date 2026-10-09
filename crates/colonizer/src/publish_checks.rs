//! Issue #1055: before autopilot opens a colony's pull request, the repository's own fast checks
//! run in fresh checkouts, so a colony pull request does not open red. The pass is separate from
//! the claim checks ([`crate::verify`]) — never on their list, which `verify_focus` would reorder —
//! and runs only when a verification has confirmed the claim and autopilot would publish. What
//! runs is what the merge-train's local checks detect ([`local_checks::detected`]): `cargo fmt
//! --check`, `clippy -D warnings`, a JavaScript package's `typecheck` and `lint`, plus the
//! repository's own `scripts/ci/check-*.sh`, but only the ones a `.github/workflows` file actually
//! runs. Tests are the claim checks' job already, and builds and e2e runs are left to CI
//! ([`is_test_command`]). The repository steers the whole pass from `.colonizer/checks.toml`
//! ([`ChecksToml`]), read from the base branch like every other input, so a colony cannot weaken
//! its own gate.
//!
//! A check that fails on the head but passes on the base is the change's own: the verification is
//! contradicted and the failure goes back to the agent as a fix round, bounded as ever. A check
//! that fails on the base branch too is not the colony's: the pull request still opens, as a
//! **draft** naming the failing checks, and never ready-for-review with known-red checks. A check
//! that cannot be judged at all — infra, the network with its retries spent — is treated as
//! [`crate::verify`] treats it: unverifiable, or held for the network.

use crate::{
    merge_loop::local_checks,
    sessions::Session,
    verify::{self, BaseFiles, BaseOut, Check, Checked, Compared, Failure, Outcome, Verification, VmRunner},
};
use serde::Deserialize;
use std::{path::Path, time::Duration};

/// The repository's pre-publish knobs, read from the base branch, never from the colony's branch.
pub(crate) const CHECKS_TOML: &str = ".colonizer/checks.toml";
/// The `Check::source` every pre-publish check carries.
const SOURCE: &str = "repo checks";
/// How many `.github/workflows` files are read to decide which `scripts/ci/check-*.sh` run; past
/// it the rest of the scripts simply do not run, which holds — the pass never gates on less than
/// it read.
pub(crate) const WORKFLOW_READS: usize = 20;

/// The runners whose `test`, `build` and `e2e` invocations are left to CI (`make test` included).
const MANAGERS: &[&str] = &["npm", "npx", "pnpm", "yarn", "bun", "corepack", "make"];

/// `.colonizer/checks.toml`, every key optional.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ChecksToml {
    /// The commands to run before a pull request is published; `[]` switches the pass off in this
    /// repository.
    pub(crate) pre_publish: Option<Vec<String>>,
}

/// The repository's `pre_publish` list when the file parses and sets one; `Err` is why the file
/// does not parse, for the caller's log.
pub(crate) fn parse_checks_toml(text: &str) -> Result<Option<Vec<String>>, String> {
    toml::from_str::<ChecksToml>(text)
        .map(|file| file.pre_publish)
        .map_err(|e| e.message().to_string())
}

/// Whether `path` is a workflow file (`.github/workflows/*.yml|yaml`).
pub(crate) fn is_workflow_path(path: &str) -> bool {
    let Some(name) = path.strip_prefix(".github/workflows/") else {
        return false;
    };
    (name.ends_with(".yml") || name.ends_with(".yaml")) && !name.contains('/')
}

/// Whether `path` is one of the repository's own CI check scripts, `scripts/ci/check-*.sh`.
pub(crate) fn is_ci_check_script(path: &str) -> bool {
    let Some(name) = path.strip_prefix("scripts/ci/check-") else {
        return false;
    };
    name.ends_with(".sh") && !name.contains('/')
}

/// Whether a command is one CI already owns and the pass therefore drops: the tests (the claim
/// checks run them), and builds and e2e runs. Compounds decide on their last segment —
/// `npm ci && npm test` is a test, `npm ci && npm run lint` is not.
pub(crate) fn is_test_command(command: &str) -> bool {
    let last = command.rsplit("&&").next().unwrap_or(command).trim();
    let lower = last.to_ascii_lowercase();
    if lower.starts_with("cargo test") || lower.starts_with("cargo nextest") {
        return true;
    }
    let head = lower
        .strip_suffix(" run test")
        .or_else(|| lower.strip_suffix(" run build"))
        .or_else(|| lower.strip_suffix(" run e2e"))
        .or_else(|| lower.strip_suffix(" test"))
        .or_else(|| lower.strip_suffix(" build"))
        .or_else(|| lower.strip_suffix(" e2e"));
    let Some(head) = head else {
        return false;
    };
    MANAGERS.contains(&head.split_whitespace().next().unwrap_or_default())
}

/// The name a check carries in the pull request note: `cargo fmt --all --check` is `fmt`, clippy
/// is `clippy`, `npm run typecheck` is `typecheck`, `bash scripts/ci/check-exec-bits.sh` is
/// `check-exec-bits`; anything else reads as itself.
pub(crate) fn short_name(command: &str) -> String {
    let command = command.trim();
    if command.starts_with("cargo fmt") {
        return "fmt".to_string();
    }
    if command.starts_with("cargo clippy") {
        return "clippy".to_string();
    }
    if command.starts_with("cargo test") || command.starts_with("cargo nextest") {
        return "test".to_string();
    }
    if let Some((_, script)) = command.split_once(" run ")
        && let Some(name) = script.split_whitespace().next()
    {
        return name.to_string();
    }
    for runner in ["bash ", "sh "] {
        if let Some(path) = command.strip_prefix(runner) {
            let path = path.trim();
            return path.rsplit('/').next().unwrap_or(path).trim_end_matches(".sh").to_string();
        }
    }
    command.to_string()
}

/// The root files that decide a JavaScript command's package manager, as the verifier reads them.
fn js_files(files: &BaseFiles) -> BaseFiles {
    BaseFiles {
        package_json: files.package_json.clone(),
        lockfiles: files.lockfiles.clone(),
        yarn_berry_lock: files.yarn_berry_lock,
        ..BaseFiles::default()
    }
}

/// The pre-publish commands for a repository, in the order they run, deduplicated. The
/// repository's own `checks.toml` `pre_publish` wins outright — `[]` disables the pass — and an
/// unparsable file falls back to detection (the caller logs why). Detection is
/// [`local_checks::detected`] minus what CI owns ([`is_test_command`]), each bare JavaScript
/// command led by its install (every check is a fresh checkout, so there is no `node_modules`
/// yet), then `merge.toml`'s `local_checks` commands under the same filter, then the
/// `scripts/ci/check-*.sh` a workflow text actually names, run with `bash` so an exec bit cannot
/// matter.
pub(crate) fn select(
    checks_toml: Option<&str>,
    merge_toml: Option<&str>,
    files: &BaseFiles,
    ci_scripts: &[String],
    workflow_texts: &[&str],
) -> Vec<String> {
    if let Some(text) = checks_toml
        && let Ok(file) = toml::from_str::<ChecksToml>(text)
        && let Some(list) = file.pre_publish
    {
        return dedupe(
            list.into_iter()
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty())
                .collect(),
        );
    }
    let install = verify::declared_test_command(&js_files(files))
        .as_ref()
        .and_then(|d| d.command.rsplit_once(" && "))
        .map(|(install, _)| install.to_string());
    let mut out = Vec::new();
    for command in local_checks::detected(files) {
        if is_test_command(&command) {
            continue;
        }
        let command = match (&install, command.split_whitespace().next()) {
            (Some(install), Some(runner)) if !command.contains("&&") && install.split_whitespace().next() == Some(runner) => {
                format!("{install} && {command}")
            }
            _ => command,
        };
        out.push(command);
    }
    if let Some(text) = merge_toml
        && let Ok(file) = toml::from_str::<local_checks::MergeToml>(text)
        && let Some(list) = file.local_checks
    {
        out.extend(
            list.into_iter()
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty() && !is_test_command(c)),
        );
    }
    let mut scripts: Vec<&str> = ci_scripts
        .iter()
        .map(String::as_str)
        .filter(|path| {
            let name = path.rsplit('/').next().unwrap_or(path);
            workflow_texts.iter().any(|text| text.contains(name))
        })
        .collect();
    scripts.sort_unstable();
    out.extend(scripts.into_iter().map(|path| format!("bash {path}")));
    dedupe(out)
}

/// One [`Check`] per command, at the repository root, sourced `repo checks`: a cargo command needs
/// cargo, a JavaScript command needs the package manager the repository's test command resolves
/// to, a `make` command needs make, and the `bash` scripts need nothing.
pub(crate) fn as_checks(commands: &[String], files: &BaseFiles) -> Vec<Check> {
    let js = verify::declared_test_command(&js_files(files)).map(|d| d.needs);
    commands
        .iter()
        .map(|command| {
            let needs = match command.split_whitespace().next().unwrap_or_default() {
                "cargo" => Some(&verify::CARGO),
                "make" => Some(&verify::MAKE),
                "npm" | "npx" | "pnpm" | "yarn" | "bun" | "corepack" => js,
                _ => None,
            };
            Check {
                dir: String::new(),
                command: command.clone(),
                source: SOURCE,
                needs,
                runs_script: false,
            }
        })
        .collect()
}

/// What the pre-publish pass came to.
#[derive(Debug, PartialEq)]
pub(crate) enum PrePublish {
    /// Every selected check ran green on the head: their short names, for the pull request note.
    Green(Vec<String>),
    /// Checks that fail on the base branch as well: publish as a draft naming them.
    Red(Vec<String>),
    /// A failure of the change's own, already recorded on the verification: the fix loop takes it.
    Contradicted,
    /// The checks could not be judged (infra): the summary says why.
    Infra(String),
    /// The network, the retries spent: the record carries the cause.
    Network,
}

/// Runs the pre-publish checks, one fresh checkout of `snapshot` each, comparing a failure against
/// the base like the claim loop does, and folds what it finds into `record`. Stops at the first
/// failure of the change's own (a fix round re-runs everything); the base-shared ones all pile up.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run(
    app: &crate::App,
    s: &Session,
    admin: &Path,
    cwd: &Path,
    snapshot: &str,
    merge_base: &str,
    checks: &[Check],
    runner: &VmRunner,
    delays: &[Duration],
    record: &mut Verification,
) -> PrePublish {
    let (mut ok, mut red) = (Vec::new(), Vec::new());
    for check in checks {
        let ran = match verify::run_check(app, s, admin, cwd, snapshot, check, runner, delays).await {
            Ok(Checked::Ran(ran)) => ran,
            Ok(Checked::Network(cause)) => {
                record.network = Some(cause.clone());
                return PrePublish::Network;
            }
            Err(e) => return PrePublish::Infra(format!("could not run the pre-publish checks: {e:#}")),
        };
        let evidence = ran.tail.clone().filter(|t| !t.trim().is_empty());
        match verify::classify(&ran) {
            Outcome::Green => ok.push(short_name(&check.command)),
            Outcome::Failed(code) => {
                match verify::base_run(app, s, admin, cwd, merge_base, check, runner, delays).await {
                    BaseOut::Passes => {
                        record.failures.push(verify::failure_of(check, evidence));
                        let why = verify::head_failure(cwd, check, code, ran.tail).await;
                        record.contradictions.push(format!("pre-publish check: {why}"));
                        return PrePublish::Contradicted;
                    }
                    BaseOut::FailsToo(base_tests) => {
                        let head_tests = verify::failing_test_names(evidence.as_deref().unwrap_or_default());
                        match verify::compare_failures(&head_tests, &base_tests) {
                            // Tests failing on the head alone are the change's own, whatever else
                            // the base trips over.
                            Compared::New { new, .. } => {
                                record.failures.push(Failure {
                                    command: check.command.clone(),
                                    tests: new,
                                    tail: evidence.unwrap_or_default(),
                                });
                                let why = verify::head_failure(cwd, check, code, ran.tail).await;
                                record.contradictions.push(format!("pre-publish check: {why}"));
                                return PrePublish::Contradicted;
                            }
                            Compared::AllShared(_) | Compared::Unnamed => {
                                red.push(short_name(&check.command));
                                record
                                    .inconclusive
                                    .push(format!("`{}` fails on the base commit as well", check.command));
                            }
                        }
                    }
                    BaseOut::Unchecked(why) => {
                        record.failures.push(verify::failure_of(check, evidence));
                        let head = verify::head_failure(cwd, check, code, ran.tail).await;
                        record.contradictions.push(format!(
                            "pre-publish check: {head} (the base commit could not be checked: {why})"
                        ));
                        return PrePublish::Contradicted;
                    }
                }
            }
            Outcome::MissingTool => {
                return PrePublish::Infra(format!(
                    "`{}` (picked from `{}`) is not in the colony image, so `{}` could not run (exit 127)",
                    check.needs.map_or("the check's tool", |n| n.tool),
                    check.source,
                    check.command
                ));
            }
            Outcome::Absent => {
                return PrePublish::Infra(format!("`{}` is not present in the colony image (exit 127)", check.command));
            }
            Outcome::NoReport => return PrePublish::Infra("the runner did not report an exit code".into()),
            Outcome::Sandbox(code) => return PrePublish::Infra(format!("the sandbox itself exited {code}")),
        }
    }
    let said = |names: &[String], mark: &str| names.iter().map(|n| format!("{n} {mark}")).collect::<Vec<_>>().join(", ");
    let list = [said(&ok, "✓"), said(&red, "✗")]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    if !list.is_empty() {
        app.session_log(&s.id, "info", format!("verification: pre-publish checks {list}"))
            .await;
    }
    if red.is_empty() {
        PrePublish::Green(ok)
    } else {
        PrePublish::Red(red)
    }
}

/// First occurrences only, in order.
fn dedupe(commands: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for command in commands {
        if !out.contains(&command) {
            out.push(command);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The root files a stack detection reads from, as pure inputs.
    fn files(package_json: Option<&str>, lockfiles: &[&str], cargo_toml: bool) -> BaseFiles {
        BaseFiles {
            package_json: package_json.map(str::to_string),
            lockfiles: lockfiles.iter().map(|l| (*l).to_string()).collect(),
            cargo_toml,
            ..BaseFiles::default()
        }
    }

    #[test]
    fn tests_builds_and_e2e_are_left_to_ci() {
        assert!(is_test_command("cargo test --workspace"));
        assert!(is_test_command("cargo test"));
        assert!(is_test_command("cargo nextest run"));
        assert!(is_test_command("cargo test -p colonizer"));
        assert!(is_test_command("npm ci && npm test"));
        assert!(is_test_command("npm test"));
        assert!(is_test_command("npm run test"));
        assert!(is_test_command("bun install --frozen-lockfile && bun run test"));
        assert!(is_test_command(
            "corepack pnpm install --frozen-lockfile && corepack pnpm test"
        ));
        assert!(is_test_command("yarn test"));
        assert!(is_test_command("make test"));
        assert!(is_test_command("npm run build"));
        assert!(is_test_command("pnpm run e2e"));
        assert!(!is_test_command("cargo fmt --all --check"));
        assert!(!is_test_command("cargo clippy --workspace --all-targets -- -D warnings"));
        assert!(!is_test_command("npm ci && npm run typecheck"));
        assert!(!is_test_command("npm run lint"));
        assert!(!is_test_command("npx tsc --noEmit"));
        assert!(!is_test_command("bash scripts/ci/check-exec-bits.sh"));
        assert!(!is_test_command(
            "cargo fmt --all --check && cargo clippy --workspace -D warnings"
        ));
    }

    #[test]
    fn workflow_files_and_ci_scripts_are_recognised_by_path() {
        assert!(is_workflow_path(".github/workflows/ci.yml"));
        assert!(is_workflow_path(".github/workflows/release.yaml"));
        assert!(!is_workflow_path(".github/workflows/sub/ci.yml"));
        assert!(!is_workflow_path(".github/workflows/ci.txt"));
        assert!(!is_workflow_path("github/workflows/ci.yml"));
        assert!(!is_workflow_path(".github/workflows"));
        assert!(is_ci_check_script("scripts/ci/check-exec-bits.sh"));
        assert!(is_ci_check_script("scripts/ci/check-rust-file-size.sh"));
        assert!(!is_ci_check_script("scripts/ci/other.sh"));
        assert!(!is_ci_check_script("scripts/ci/check-exec-bits.sh/x.sh"));
        assert!(!is_ci_check_script("scripts/test/check-exec-bits.test.sh"));
    }

    #[test]
    fn names_are_short_for_the_pull_request_note() {
        assert_eq!(short_name("cargo fmt --all --check"), "fmt");
        assert_eq!(short_name("cargo clippy --workspace --all-targets -- -D warnings"), "clippy");
        assert_eq!(short_name("npm ci && npm run typecheck"), "typecheck");
        assert_eq!(short_name("bash scripts/ci/check-exec-bits.sh"), "check-exec-bits");
        assert_eq!(short_name("bash scripts/ci/check-rust-file-size.sh"), "check-rust-file-size");
        assert_eq!(short_name("npx tsc --noEmit"), "npx tsc --noEmit");
    }

    #[test]
    fn the_repository_file_wins_and_can_switch_the_pass_off() {
        let cargo = files(None, &[], true);
        assert_eq!(
            select(
                Some("pre_publish = ['cargo fmt --all --check', ' cargo clippy --all-targets -- -D warnings ']"),
                None,
                &cargo,
                &[],
                &[],
            ),
            vec![
                "cargo fmt --all --check".to_string(),
                "cargo clippy --all-targets -- -D warnings".to_string(),
            ],
            "the repository's own list, trimmed, unfiltered"
        );
        assert_eq!(
            select(Some("pre_publish = []"), None, &cargo, &[], &[]),
            Vec::<String>::new(),
            "an empty list disables the pass"
        );
        // An unparsable file, or one without the key, falls back to what is detected.
        assert_eq!(
            select(Some("pre_publish = 'oops'"), None, &cargo, &[], &[]),
            select(None, None, &cargo, &[], &[])
        );
        assert_eq!(
            select(Some("unknown = 1"), None, &cargo, &[], &[]),
            select(None, None, &cargo, &[], &[]),
            "an unknown key is not one this file carries"
        );
        assert!(parse_checks_toml("pre_publish = 'oops'").is_err());
        assert_eq!(parse_checks_toml("pre_publish = ['x']"), Ok(Some(vec!["x".to_string()])));
        assert_eq!(parse_checks_toml(""), Ok(None));
    }

    #[test]
    fn detection_drops_the_tests_and_installs_before_the_js_commands() {
        let cargo = files(None, &[], true);
        assert_eq!(
            select(None, None, &cargo, &[], &[]),
            vec![
                "cargo fmt --all --check".to_string(),
                "cargo clippy --workspace --all-targets -- -D warnings".to_string(),
            ],
            "`cargo test --workspace` stays with the claim checks"
        );
        let npm = files(
            Some(
                r#"{"scripts": {"test": "vitest run", "typecheck": "tsc --noEmit", "lint": "eslint .", "build": "vite build"}}"#,
            ),
            &["package-lock.json"],
            false,
        );
        assert_eq!(
            select(None, None, &npm, &[], &[]),
            vec![
                "npm ci && npm run typecheck".to_string(),
                "npm ci && npm run lint".to_string(),
            ],
            "a fresh checkout has no node_modules: the install leads"
        );
        let bun = files(
            Some(r#"{"scripts": {"test": "jest", "typecheck": "tsc --noEmit"}}"#),
            &["bun.lock"],
            false,
        );
        assert_eq!(
            select(None, None, &bun, &[], &[]),
            vec!["bun install --frozen-lockfile && bun run typecheck".to_string()]
        );
        let make = BaseFiles {
            makefile: Some("test:\n\tgo test ./...\n".into()),
            ..BaseFiles::default()
        };
        assert_eq!(
            select(None, None, &make, &[], &[]),
            Vec::<String>::new(),
            "`make test` is CI's"
        );
        assert_eq!(select(None, None, &BaseFiles::default(), &[], &[]), Vec::<String>::new());
    }

    #[test]
    fn merge_toml_commands_and_ci_scripts_a_workflow_runs_are_added() {
        let cargo = files(None, &[], true);
        let merge = "local_checks = ['cargo fmt --all --check', 'cargo test --workspace', 'npx tsc --noEmit']";
        let scripts = vec![
            "scripts/ci/check-case-collisions.sh".to_string(),
            "scripts/ci/check-exec-bits.sh".to_string(),
            "scripts/ci/other.sh".to_string(),
        ];
        let workflow = "      - name: Check executable bits\n        run: sh scripts/ci/check-exec-bits.sh\n";
        assert_eq!(
            select(None, Some(merge), &cargo, &scripts, &[workflow]),
            vec![
                "cargo fmt --all --check".to_string(),
                "cargo clippy --workspace --all-targets -- -D warnings".to_string(),
                "npx tsc --noEmit".to_string(),
                "bash scripts/ci/check-exec-bits.sh".to_string(),
            ],
            "only the check script a workflow names runs, deduplicated, sorted"
        );
        // No workflows read: no script runs, whatever the base carries.
        assert_eq!(
            select(None, None, &cargo, &scripts, &[]),
            vec![
                "cargo fmt --all --check".to_string(),
                "cargo clippy --workspace --all-targets -- -D warnings".to_string(),
            ]
        );
    }

    #[test]
    fn every_check_is_a_root_check_with_the_tools_it_needs() {
        let npm = files(Some(r#"{"scripts": {"test": "jest"}}"#), &["package-lock.json"], false);
        let commands = vec![
            "cargo fmt --all --check".to_string(),
            "npm ci && npm run lint".to_string(),
            "bash scripts/ci/check-exec-bits.sh".to_string(),
        ];
        let checks = as_checks(&commands, &npm);
        assert_eq!(checks.len(), 3);
        assert!(
            checks
                .iter()
                .all(|c| c.dir.is_empty() && c.source == "repo checks" && !c.runs_script),
            "{checks:?}"
        );
        assert_eq!(checks[0].needs.map(|n| n.tool), Some("cargo"));
        assert_eq!(checks[1].needs.map(|n| n.tool), Some("npm"));
        assert_eq!(checks[2].needs, None, "the bash scripts need nothing but bash");
    }
}
