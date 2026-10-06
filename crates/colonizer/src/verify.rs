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
//! **Contradicted** when the branch disagrees with the claim — the description names files in the
//! repository and **none** of them is on the branch or in the diff, so the work it describes is not
//! there — or the tests fail in the fresh checkout in a way the base commit does not; **confirmed**
//! only when the checks ran green; **inconclusive** when a check fails on the base commit too, so
//! the failure predates the change; **unverifiable** otherwise — an empty branch, no check that
//! applies to the diff, or infra that broke, which is not the colony's fault. `verify: none`
//! records `unverifiable` by declaration without any work.
//!
//! The checks are diff-scoped: the files the branch changes (merge-base..snapshot) pick which
//! commands run, and where — `cargo test` at the changed Rust code's nearest Cargo.toml ancestor,
//! a touched JavaScript package's own test script, the root `make test` for what neither covers.
//!
//! A described path that is missing while other described paths *are* there is only an
//! **advisory** (`advisories`): pull request descriptions routinely name files that were
//! deliberately not created, belong to other or future work, or were renamed on the way. Advisories
//! are shown with the verdict and in the published pull request, and never change the verdict.

use crate::{
    App, Shared,
    sessions::Session,
    util::{exec_within, short_id},
};
use anyhow::{Result, bail};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{Arc, LazyLock, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt},
    process::Command,
};

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
    /// A check fails on the base commit as well: the failure predates the change, so it is not
    /// the colony's. Publishes, with a note for the reviewer.
    Inconclusive,
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
    /// Observations worth a reviewer's look that do not contradict the claim — a described path
    /// that is not on the branch while other described paths are. Never changes the verdict.
    #[serde(default)]
    pub advisories: Vec<String>,
    /// Checks that fail on the base commit as well, as reviewer-ready clauses — the failure
    /// predates the change, so it is not attributed to it. Never changes the verdict to a hold.
    #[serde(default)]
    pub inconclusive: Vec<String>,
    /// Why the checks never got to run, when the network is why and the retries ran out (issue
    /// #1117): "rustup toolchain download, connection reset". Set only on an unverifiable verdict,
    /// and autopilot then holds the colony with this cause on its card instead of publishing.
    #[serde(default)]
    pub network: Option<String>,
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
            advisories: Vec::new(),
            inconclusive: Vec::new(),
            network: None,
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

/// The verdict from the pieces: contradicted if anything contradicts, inconclusive when a check's
/// failure is the base's too, confirmed only on a green fresh run, unverifiable otherwise. Pure so
/// the transitions are tested directly.
fn decide(contradictions: &[String], inconclusive: bool, green: Option<bool>) -> Verdict {
    if !contradictions.is_empty() {
        Verdict::Contradicted
    } else if inconclusive {
        Verdict::Inconclusive
    } else if green == Some(true) {
        Verdict::Confirmed
    } else {
        Verdict::Unverifiable
    }
}

/// The files on the base branch that can declare a test command, as pure inputs.
#[derive(Debug, Default)]
pub(crate) struct BaseFiles {
    pub package_json: Option<String>,
    /// The JavaScript lockfiles at the base branch's root, by file name ([`JS_LOCKFILES`]).
    pub lockfiles: Vec<String>,
    /// `yarn.lock` is Yarn 2+'s format (it carries `__metadata:`), not Yarn 1's.
    pub yarn_berry_lock: bool,
    /// The base branch carries files bun's own runner would pick up (`*.test.ts`, `*_spec.js`, …):
    /// only asked when bun is the package manager and there is no `scripts.test` to run instead.
    pub bun_test_files: bool,
    pub cargo_toml: bool,
    pub makefile: Option<String>,
}

/// The lockfiles that name a JavaScript package manager, in the order they are trusted when a
/// base branch carries more than one (and no `packageManager` field settles it).
pub(crate) const JS_LOCKFILES: &[&str] = &[
    "bun.lock",
    "bun.lockb",
    "pnpm-lock.yaml",
    "yarn.lock",
    "package-lock.json",
    "npm-shrinkwrap.json",
];

/// A tool a declared command needs in the colony image, and the shell test that says it is there.
/// The fresh-checkout run checks it first: a missing tool is the image's gap, never the colony's,
/// so it makes the claim unverifiable instead of letting the install fail and contradict it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Needs {
    pub tool: &'static str,
    pub check: &'static str,
}

const fn needs(tool: &'static str, check: &'static str) -> Needs {
    Needs { tool, check }
}

static NPM: Needs = needs("npm", "command -v npm >/dev/null 2>&1");
static BUN: Needs = needs("bun", "command -v bun >/dev/null 2>&1");
static PNPM: Needs = needs("pnpm", "command -v pnpm >/dev/null 2>&1");
static YARN: Needs = needs("yarn", "command -v yarn >/dev/null 2>&1");
/// Yarn 1 (what node images carry globally) does not read a Yarn 2+ lockfile: running it on one
/// fails the install, which would read as the colony's failure.
static YARN_BERRY: Needs = needs(
    "yarn 2 or later",
    r#"[ "$(yarn --version 2>/dev/null | cut -d. -f1)" -ge 2 ] 2>/dev/null"#,
);
/// Runs the exact pnpm or yarn a `packageManager` field pins, downloading it on first use.
static COREPACK: Needs = needs("corepack", "command -v corepack >/dev/null 2>&1");
static CARGO: Needs = needs("cargo", "command -v cargo >/dev/null 2>&1");
static MAKE: Needs = needs("make", "command -v make >/dev/null 2>&1");

/// The command a repository declares for its tests, where it came from, and what it needs.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Declared {
    /// What decided the command: `packageManager` (the package.json field), the lockfile that
    /// named the package manager, `package.json` (no lockfile: npm), `Cargo.toml` or `Makefile`.
    pub source: &'static str,
    pub command: String,
    pub needs: &'static Needs,
    /// The command runs package.json's `scripts.test`, so a branch that rewrote that entry would
    /// be grading itself.
    pub runs_script: bool,
}

/// package.json's `scripts.test`, when the file parses and carries one.
fn scripts_test(package: Option<&str>) -> Option<String> {
    let value = serde_json::from_str::<Value>(package?).ok()?;
    value["scripts"]["test"].as_str().map(str::to_string)
}

/// `scripts.test` when it runs something: npm's `no test specified` placeholder does not.
fn usable_script(package: Option<&str>) -> Option<String> {
    scripts_test(package).filter(|t| !t.is_empty() && !t.contains("no test specified"))
}

/// package.json's `packageManager` field (corepack's `name@version[+hash]`), as `(name, major)` —
/// only for the four managers this knows how to run. The major is `None` when it does not parse.
fn package_manager(package: Option<&str>) -> Option<(&'static str, Option<u64>)> {
    let value = serde_json::from_str::<Value>(package?).ok()?;
    let (name, version) = value["packageManager"].as_str()?.trim().split_once('@')?;
    let name = ["npm", "pnpm", "yarn", "bun"].into_iter().find(|n| *n == name)?;
    Some((name, version.split('.').next().and_then(|m| m.parse().ok())))
}

/// Whether bun's own test runner would find a test file at this path.
pub(crate) fn is_bun_test_file(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return false;
    };
    matches!(ext, "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" | "mts" | "cts")
        && [".test", "_test", ".spec", "_spec"].iter().any(|s| stem.ends_with(s))
        && !path.split('/').any(|c| c == "node_modules")
}

/// The test command a repository declares, from its base branch's file contents alone. Never
/// guessed from chat text. In order:
///
/// 1. package.json's `scripts.test` (unless it is npm's placeholder), run by the repository's own
///    package manager: the `packageManager` field (corepack) when it names one, else the first
///    lockfile of [`JS_LOCKFILES`], else npm. A frozen install only when that manager's lockfile
///    is there to freeze. A bun repository without the script runs `bun test` when the base
///    branch has test files for it.
/// 2. `cargo test` for a Cargo.toml.
/// 3. `make test` for a Makefile with a `test:` target.
pub(crate) fn declared_test_command(files: &BaseFiles) -> Option<Declared> {
    let script = usable_script(files.package_json.as_deref());
    if files.package_json.is_some() {
        let has = |lock: &str| files.lockfiles.iter().any(|l| l == lock);
        let lockfile = JS_LOCKFILES.iter().copied().find(|l| has(l));
        let pinned = package_manager(files.package_json.as_deref());
        let (source, manager) = match (pinned, lockfile) {
            (Some((name, _)), _) => ("packageManager", name),
            (None, Some(lock)) => (
                lock,
                match lock {
                    "bun.lock" | "bun.lockb" => "bun",
                    "pnpm-lock.yaml" => "pnpm",
                    "yarn.lock" => "yarn",
                    _ => "npm",
                },
            ),
            (None, None) => ("package.json", "npm"),
        };
        let js = |command: String, needs: &'static Needs, runs_script: bool| {
            Some(Declared {
                source,
                command,
                needs,
                runs_script,
            })
        };
        match manager {
            "bun" => {
                let install = if has("bun.lock") || has("bun.lockb") {
                    "bun install --frozen-lockfile"
                } else {
                    "bun install"
                };
                if script.is_some() {
                    return js(format!("{install} && bun run test"), &BUN, true);
                }
                if files.bun_test_files {
                    return js(format!("{install} && bun test"), &BUN, false);
                }
            }
            _ if script.is_none() => {}
            "pnpm" => {
                // A pinned version runs through corepack, which fetches exactly that pnpm.
                let (pnpm, needs) = if pinned.is_some() {
                    ("corepack pnpm", &COREPACK)
                } else {
                    ("pnpm", &PNPM)
                };
                let frozen = if has("pnpm-lock.yaml") { " --frozen-lockfile" } else { "" };
                return js(format!("{pnpm} install{frozen} && {pnpm} test"), needs, true);
            }
            "yarn" => {
                // Berry (Yarn 2+) spells a frozen install `--immutable`; Yarn 1 `--frozen-lockfile`.
                let berry = match pinned {
                    Some((_, major)) => major.is_some_and(|m| m >= 2),
                    None => files.yarn_berry_lock,
                };
                let (yarn, needs) = match (pinned.is_some(), berry) {
                    (true, _) => ("corepack yarn", &COREPACK),
                    (false, true) => ("yarn", &YARN_BERRY),
                    (false, false) => ("yarn", &YARN),
                };
                let frozen = match (has("yarn.lock"), berry) {
                    (false, _) => "",
                    (true, true) => " --immutable",
                    (true, false) => " --frozen-lockfile",
                };
                return js(format!("{yarn} install{frozen} && {yarn} test"), needs, true);
            }
            _ => {
                let install = if has("package-lock.json") || has("npm-shrinkwrap.json") {
                    "npm ci"
                } else {
                    "npm install"
                };
                return js(format!("{install} && npm test"), &NPM, true);
            }
        }
    }
    if files.cargo_toml {
        return Some(Declared {
            source: "Cargo.toml",
            command: "cargo test".into(),
            needs: &CARGO,
            runs_script: false,
        });
    }
    if files
        .makefile
        .as_deref()
        .is_some_and(|m| m.lines().any(|l| l.starts_with("test:")))
    {
        return Some(Declared {
            source: "Makefile",
            command: "make test".into(),
            needs: &MAKE,
            runs_script: false,
        });
    }
    None
}

/// The base branch's tree as far as check selection reads it: every file path, plus the contents
/// of the few files that declare commands. Read out of git before the pure [`select_checks`], so
/// the selection itself is testable without a repository.
#[derive(Debug, Default)]
pub(crate) struct BaseTree {
    /// Every path in the base tree.
    pub files: HashSet<String>,
    /// `package.json` / `yarn.lock` / `Makefile` contents, by path.
    pub contents: HashMap<String, String>,
}

impl BaseTree {
    fn carries(&self, path: &str) -> bool {
        self.files.contains(path)
    }

    fn content(&self, path: &str) -> Option<&str> {
        self.contents.get(path).map(String::as_str)
    }
}

/// `dir/name`, the way a path sits in the tree.
fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// A file's directory and every ancestor up to the root: `"web/src"` → `"web/src"`, `"web"`, `""`;
/// a root file's single ancestor is the root itself.
fn ancestors(dir: &str) -> impl Iterator<Item = &str> {
    std::iter::successors(Some(dir), |d| match d.rsplit_once('/') {
        Some((parent, _)) => Some(parent),
        None => (!d.is_empty()).then_some(""),
    })
}

/// Whether cargo owns a changed file: Rust source, a manifest, or the lockfile.
fn is_rust_path(path: &str) -> bool {
    matches!(path.rsplit('/').next().unwrap_or(path), "Cargo.toml" | "Cargo.lock") || path.ends_with(".rs")
}

/// What one directory of the base tree declares, so [`declared_test_command`] answers for a
/// package, not just the root.
pub(crate) fn dir_files(base: &BaseTree, dir: &str) -> BaseFiles {
    let at = |name: &str| join(dir, name);
    let package_json = base.content(&at("package.json")).map(str::to_string);
    let lockfiles: Vec<String> = JS_LOCKFILES
        .iter()
        .filter(|l| base.carries(&at(l)))
        .map(|l| l.to_string())
        .collect();
    let yarn_berry_lock = lockfiles.iter().any(|l| l == "yarn.lock")
        && base
            .content(&at("yarn.lock"))
            .is_some_and(|lock| lock.lines().any(|l| l.starts_with("__metadata:")));
    let bun = lockfiles.iter().any(|l| l.starts_with("bun.lock"))
        || package_manager(package_json.as_deref()).is_some_and(|(name, _)| name == "bun");
    let prefix = format!("{dir}/");
    BaseFiles {
        bun_test_files: bun
            && usable_script(package_json.as_deref()).is_none()
            && base.files.iter().any(|p| p.starts_with(&prefix) && is_bun_test_file(p)),
        cargo_toml: base.carries(&at("Cargo.toml")),
        makefile: base.content(&at("Makefile")).map(str::to_string),
        package_json,
        lockfiles,
        yarn_berry_lock,
    }
}

/// One test check a fresh checkout owes: the directory to run it in (repo-root-relative, `""` =
/// the root) and the command to run there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Check {
    pub dir: String,
    pub command: String,
    pub source: &'static str,
    pub needs: Option<&'static Needs>,
    /// The command runs an entry a branch could rewrite to grade itself (`scripts.test`, a
    /// Makefile `test:` target).
    pub runs_script: bool,
}

/// The checks a diff owes, chosen purely from the changed files (merge-base..snapshot) against the
/// base tree: a changed Rust file runs `cargo test` at its nearest ancestor carrying a Cargo.toml;
/// any other file runs its nearest `package.json` ancestor's declared test command (a package that
/// declares none gets none); a file covered by neither gets the root `make test`, only when the
/// root declares it. Deduplicated, in the changed files' order.
pub(crate) fn select_checks(changed: &[String], base: &BaseTree) -> Vec<Check> {
    let mut out: Vec<Check> = Vec::new();
    let mut push = |check: Check| {
        if !out.contains(&check) {
            out.push(check);
        }
    };
    for path in changed {
        let dir = path.rsplit_once('/').map_or("", |(d, _)| d);
        if is_rust_path(path)
            && let Some(cargo_dir) = ancestors(dir).find(|d| base.carries(&join(d, "Cargo.toml")))
        {
            push(Check {
                dir: cargo_dir.to_string(),
                command: "cargo test".into(),
                source: "Cargo.toml",
                needs: Some(&CARGO),
                runs_script: false,
            });
            continue;
        }
        if let Some(pkg_dir) = ancestors(dir).find(|d| base.carries(&join(d, "package.json"))) {
            if let Some(declared) = declared_test_command(&dir_files(base, pkg_dir)) {
                push(Check {
                    dir: pkg_dir.to_string(),
                    command: declared.command,
                    source: declared.source,
                    needs: Some(declared.needs),
                    runs_script: declared.runs_script,
                });
            }
            continue;
        }
        // Covered by neither — docs, config: only a root Makefile `test:` target speaks for them.
        if base
            .content("Makefile")
            .is_some_and(|m| m.lines().any(|l| l.starts_with("test:")))
        {
            push(Check {
                dir: String::new(),
                command: "make test".into(),
                source: "Makefile",
                needs: Some(&MAKE),
                runs_script: false,
            });
        }
    }
    out
}

/// The evidence a failing run leaves behind: the last lines of the guest's output, written to the
/// session's `out` directory. Roughly two hundred lines is what a reviewer reads.
const LOG_LINES: usize = 200;
/// How many failing tests a contradiction message names before it says "and N more".
const NAME_CAP: usize = 5;

/// The base branch's tree for check selection: every file path, plus the few contents that declare
/// commands. Read from the base ref only, never the colony's branch.
async fn base_tree(app: &App, admin: &Path, cwd: &Path, base_ref: &str, changed: &[String]) -> Result<BaseTree> {
    let files: HashSet<String> = read(app, admin, cwd, &["ls-tree", "-r", "--name-only", base_ref])
        .await?
        .lines()
        .map(str::to_string)
        .collect();
    let mut contents = HashMap::new();
    let mut wanted = vec!["Makefile".to_string()];
    for path in changed {
        let dir = path.rsplit_once('/').map_or("", |(d, _)| d);
        if is_rust_path(path) && ancestors(dir).any(|d| files.contains(&join(d, "Cargo.toml"))) {
            continue;
        }
        if let Some(pkg_dir) = ancestors(dir).find(|d| files.contains(&join(d, "package.json"))) {
            for name in ["package.json", "yarn.lock"] {
                let path = join(pkg_dir, name);
                if files.contains(&path) && !wanted.contains(&path) {
                    wanted.push(path);
                }
            }
        }
    }
    for path in wanted {
        if let Some(text) = file_at(app, admin, cwd, base_ref, &path).await {
            contents.insert(path, text);
        }
    }
    Ok(BaseTree { files, contents })
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

/// Weighs the described paths against the branch. `corroborated` says the description and the
/// branch agree somewhere — a described in-repo path the branch or the diff carries, or a changed
/// file the description names — and `missing` are the described in-repo paths the branch lacks
/// (deduplicated here, in order). Answers `(contradictions, advisories)`: only a description whose
/// in-repo paths are **all** missing, with nothing it says found in the diff, contradicts the claim
/// — the work it describes is not there. A missing path beside corroborated ones is an advisory:
/// descriptions name files that were deliberately avoided, belong to other or future work, or were
/// renamed on the way. Pure so the rule is tested directly.
fn weigh_described(corroborated: bool, missing: &[String]) -> (Vec<String>, Vec<String>) {
    let mut unique: Vec<&str> = Vec::new();
    for path in missing {
        if !unique.contains(&path.as_str()) {
            unique.push(path);
        }
    }
    match (corroborated, unique.as_slice()) {
        (_, []) => (Vec::new(), Vec::new()),
        (false, [only]) => (
            vec![format!(
                "described `{only}` is not on the branch, and it is the only file the description names"
            )],
            Vec::new(),
        ),
        (false, all) => (
            vec![format!(
                "none of the {} files the description names is on the branch: {}",
                all.len(),
                all.iter().map(|p| format!("`{p}`")).collect::<Vec<_>>().join(", ")
            )],
            Vec::new(),
        ),
        (true, some) => (
            Vec::new(),
            some.iter().map(|p| format!("described `{p}` is not on the branch")).collect(),
        ),
    }
}

/// Whether the description names a file the diff changed: its path, or its file name (one with an
/// extension) anywhere in the text — prose like "I named the file remote-tunnel.md" counts, not
/// only backticked paths.
fn names_a_changed_file(claim: &str, changed: &[String]) -> bool {
    changed.iter().any(|path| {
        let name = path.rsplit('/').next().unwrap_or(path);
        claim.contains(path.as_str()) || (name.contains('.') && claim.contains(name))
    })
}

/// The published pull request's verification notes: the inconclusive checks and the advisories,
/// when there are any, as a quoted block a reviewer reads before merging. `None` when there is
/// nothing to note.
pub(crate) fn pr_notes(verification: Option<&Verification>) -> Option<String> {
    let v = verification.filter(|v| !v.advisories.is_empty() || !v.inconclusive.is_empty())?;
    let mut out = String::from("> **Verification notes** (advisory; they did not change the verdict):");
    for note in &v.inconclusive {
        out.push_str(&format!(
            "\n> - Verification was inconclusive: {note}, so the failure is not attributed to this change."
        ));
    }
    for note in &v.advisories {
        out.push_str(&format!("\n> - {note}"));
    }
    Some(out)
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
/// already exists to hold. What the path policy put in the worktree, or hid, is held back exactly
/// as publish holds it back (`path_policy::hold_back_staged`): an empty placeholder is not work,
/// and must not count as a changed file or reach the fresh checkout.
async fn snapshot_work(app: &App, s: &Session, admin: &Path, cwd: &Path) -> Result<String> {
    let index = cwd.join(format!("verify-index-{}", short_id()));
    let at_index = || {
        let mut c = disowned(app.git(admin), cwd);
        c.arg("--work-tree").arg(&s.worktree).env("GIT_INDEX_FILE", &index);
        c
    };
    let git = |args: &[&str]| {
        let mut c = at_index();
        c.args(args);
        c
    };
    let result = async {
        exec_within(GIT_LIMIT, &mut git(&["read-tree", "HEAD"])).await?;
        exec_within(GIT_LIMIT, &mut git(&["add", "-A"])).await?;
        let rec = crate::path_policy::Recorded::read(&cwd.join("vm"));
        crate::path_policy::hold_back_staged(at_index, Path::new(&s.worktree), &rec, GIT_LIMIT).await?;
        let tree = exec_within(GIT_LIMIT, &mut git(&["write-tree"])).await?;
        let mut c = disowned(app.git(admin), cwd);
        c.args(crate::github::HOST_GIT_IDENTITY)
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
async fn verify_claim(app: &App, s: &Session, runner: &VmRunner, delays: &[Duration]) -> Verification {
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
    // An empty branch is a "no change needed" answer, not a claim of work that is missing: there
    // is nothing to check, so it is unverifiable and publishing records it as no changes.
    if changed.is_empty() {
        unverifiable!(format!("branch has no changes against {base}; nothing to verify"));
    }
    let mut contradictions = {
        let claim = tokio::fs::read_to_string(cwd.join("out").join("pr.md"))
            .await
            .unwrap_or_default();
        let (mut present, mut missing) = (false, Vec::new());
        for path in claimed_paths(&claim) {
            // A described path is in the repository when the branch or the diff carries it, or
            // could have — its directory is on the branch. An example URL or a gitignored build
            // under an untracked dir is wording, not a described change, and is not weighed.
            if on_branch.contains(&path) || changed.contains(&path) {
                present = true;
                continue;
            }
            let tracked_here = |dir: &str| on_branch.iter().any(|t| t.starts_with(&format!("{dir}/")));
            if path.rsplit_once('/').is_some_and(|(dir, _)| tracked_here(dir)) {
                missing.push(path);
            }
        }
        let corroborated = present || names_a_changed_file(&claim, &changed);
        let (contradictions, advisories) = weigh_described(corroborated, &missing);
        record.advisories = advisories;
        contradictions
    };

    // The checks: an explicit `verify` setting is one root check; `auto` picks the base branch's
    // own declarations for the directories the diff touches.
    let tree = match base_tree(app, admin, &cwd, &base_ref, &changed).await {
        Ok(tree) => tree,
        Err(e) => unverifiable!(format!("could not read {base_ref} to pick the checks: {e:#}")),
    };
    let mut checks = match configured {
        "auto" => select_checks(&changed, &tree),
        command => vec![Check {
            dir: String::new(),
            command: command.to_string(),
            source: "config",
            needs: None,
            runs_script: false,
        }],
    };
    // A diff that deletes a package directory the base declared tests for leaves a check pointing
    // at a directory the snapshot no longer has: dropped, so the `cd`'s exit 1 cannot contradict.
    checks.retain(|check| check.dir.is_empty() || on_branch.iter().any(|p| p.starts_with(&format!("{}/", check.dir))));
    record.command = checks.first().map(|c| c.command.clone());
    record.command_source = checks.first().map(|c| c.source.to_string());

    // The colony must not grade its own homework: an `auto` command is the base branch's, so a
    // branch that rewrote the entry defining it would run its own replacement. (`cargo test`
    // has no entry to rewrite, and an explicit command is the operator's choice.)
    let mut forced: Option<String> = None; // an infra-style unverifiable summary
    for check in &checks {
        let graded = if check.runs_script {
            let path = join(&check.dir, "package.json");
            scripts_test(tree.content(&path)) != scripts_test(file_at(app, admin, &cwd, &snapshot, &path).await.as_deref())
        } else if check.source == "Makefile" {
            tree.content("Makefile") != file_at(app, admin, &cwd, &snapshot, "Makefile").await.as_deref()
        } else {
            continue;
        };
        if graded {
            let entry = if check.runs_script {
                "`scripts.test`"
            } else {
                "the `test` target"
            };
            forced = Some(format!("the branch changes {entry}, the command this check would run"));
            break;
        }
    }

    // Contradictions settle it without paying for a VM run; the verdict is the same either way.
    let mut green: Option<bool> = None;
    let (mut ran_any, mut all_green) = (false, true);
    let mut first_failure: Option<i32> = None;
    let mut reported_zero = true;
    // Focused-first (#584): which check goes first, and what each check cost and said.
    let focus = crate::verify_focus::mode(app).await;
    let candidates = crate::verify_focus::focus_candidates(&checks, &changed);
    let chosen = crate::verify_focus::choose(&candidates);
    let mut runs: Vec<crate::verify_focus::Run> = Vec::new();
    if contradictions.is_empty() && forced.is_none() {
        if checks.is_empty() {
            forced = Some(format!(
                "no test command applies to the files this diff touches (nothing usable on {base})"
            ));
        } else {
            let mut order: Vec<usize> = (0..checks.len()).collect();
            if let (crate::verify_focus::Mode::Act, Some(first)) = (focus, chosen) {
                order.retain(|&i| i != first);
                order.insert(0, first);
            }
            for &index in &order {
                let check = &checks[index];
                let (check_started, contradicted_before) = (Instant::now(), contradictions.len());
                let ran = match run_check(app, s, admin, &cwd, &snapshot, check, runner, delays).await {
                    Ok(Checked::Ran(ran)) => ran,
                    // Issue #1117: the run never reached the tests and the retries are spent —
                    // nothing contradicts the claim, and nothing confirms it either.
                    Ok(Checked::Network(cause)) => {
                        forced = Some(format!("verification could not run (network: {cause})"));
                        record.network = Some(cause);
                        break;
                    }
                    Err(e) => {
                        forced = Some(format!("could not run the tests: {e:#}"));
                        break;
                    }
                };
                ran_any = true;
                record.tests_ms = Some(record.tests_ms.unwrap_or(0) + ran.ms);
                first_failure = first_failure.or(ran.reported.filter(|code| *code != 0));
                reported_zero &= ran.reported.is_some();
                match classify(&ran) {
                    Outcome::Green => {}
                    // A failure the base shares is not this change's: the same check runs on the
                    // merge-base, in the same kind of fresh checkout, before a hold.
                    Outcome::Failed(code) => {
                        all_green = false;
                        match base_run(app, s, admin, &cwd, &merge_base, check, runner, delays).await {
                            BaseOut::FailsToo => record
                                .inconclusive
                                .push(format!("`{}` fails on the base commit as well", check.command)),
                            BaseOut::Passes => contradictions.push(head_failure(&cwd, check, code, ran.tail).await),
                            BaseOut::Unchecked(why) => contradictions.push(format!(
                                "{} (the base commit could not be checked: {why})",
                                head_failure(&cwd, check, code, ran.tail).await
                            )),
                        }
                    }
                    // Everything else is the image's or the sandbox's: unverifiable, named plainly.
                    Outcome::MissingTool => {
                        forced = Some(format!(
                            "`{}` (picked from `{}`) is not in the colony image, so `{}` could not run (exit 127)",
                            check.needs.map_or("the test tool", |n| n.tool),
                            check.source,
                            check.command
                        ));
                        break;
                    }
                    Outcome::Absent => {
                        forced = Some(format!("`{}` is not present in the colony image (exit 127)", check.command));
                        break;
                    }
                    Outcome::NoReport => {
                        forced = Some("the runner did not report an exit code".into());
                        break;
                    }
                    Outcome::Sandbox(sandbox) => {
                        forced = Some(format!("the sandbox itself exited {sandbox}"));
                        break;
                    }
                }
                let failed = contradictions.len() > contradicted_before;
                runs.push(crate::verify_focus::Run {
                    check: index,
                    ms: check_started.elapsed().as_millis() as u64,
                    failed,
                });
                // Act: the focused check's own contradiction settles the verdict, so the rest of
                // the suite is not paid for. Nothing else stops early.
                if failed && focus == crate::verify_focus::Mode::Act && Some(index) == chosen {
                    break;
                }
            }
            // Confirmed needs the whole suite: a check that never ran can never count as green.
            green = ran_any.then_some(all_green && runs.len() == checks.len());
        }
    }
    record.exit_code = first_failure.or_else(|| (ran_any && reported_zero).then_some(0));

    record.contradictions = contradictions;
    record.verdict = if forced.is_some() {
        Verdict::Unverifiable
    } else {
        decide(&record.contradictions, !record.inconclusive.is_empty(), green)
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
        Verdict::Confirmed => format!("changes passed `{}` in a fresh checkout", checks[0].command),
        Verdict::Inconclusive => format!("inconclusive: {}", record.inconclusive.join("; ")),
        Verdict::Unverifiable => "the tests could not be judged".into(),
    });
    let record = record.finished(started);
    if focus != crate::verify_focus::Mode::Off && !runs.is_empty() {
        crate::verify_focus::record(app, &s.id, focus, &candidates, chosen, &runs, record.verdict, record.ms).await;
    }
    record
}

/// What one fresh-checkout run said — the same reading for the head and the base: the guest's own
/// report decides, and everything but a green or red command is the image's or the sandbox's
/// trouble, not the colony's.
enum Outcome {
    Green,
    Failed(i32),
    /// Exit 127 with the missing-tool marker: the package manager is not in the image.
    MissingTool,
    /// Exit 127 without it: the command itself is not in the image.
    Absent,
    NoReport,
    Sandbox(i32),
}

fn classify(ran: &Ran) -> Outcome {
    match ran.reported {
        None => Outcome::NoReport,
        Some(127) if ran.missing_tool => Outcome::MissingTool,
        Some(127) => Outcome::Absent,
        Some(0) if ran.sandbox == 0 => Outcome::Green,
        Some(0) => Outcome::Sandbox(ran.sandbox),
        Some(code) => Outcome::Failed(code),
    }
}

/// What the same check answered on the merge-base commit, separating a failure this change
/// introduced from one it inherited.
enum BaseOut {
    /// Green on the base: the failure is new, and contradicts the claim.
    Passes,
    /// Red on the base too: inconclusive, not contradicted.
    FailsToo,
    /// The base could not be judged (infra): the head failure still contradicts, caveated.
    Unchecked(String),
}

/// Base-check answers, keyed by worktree, image, merge-base, dir and command, so the same failing
/// branch need not pay for a second base VM. Exit codes only — infra trouble is never cached.
static BASE_RESULTS: LazyLock<Mutex<HashMap<String, i32>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// What running one check came to once network failures were retried (issue #1117).
enum Checked {
    /// The run reached an answer — green, red, or the image's or sandbox's trouble.
    Ran(Ran),
    /// Every attempt failed before the tests for the network; the cause, for the card.
    Network(String),
}

/// Runs one check in a fresh checkout of `rev`, running it again on [`crate::retry::verify_network_cause`]'s
/// shapes — a rustup toolchain or registry download, a DNS lookup that failed before any test ran —
/// after each of `delays`, so a blip never reads as failing tests (issue #1117). A real failure, and
/// every other outcome, is answered at once.
#[allow(clippy::too_many_arguments)]
async fn run_check(
    app: &App,
    s: &Session,
    admin: &Path,
    cwd: &Path,
    rev: &str,
    check: &Check,
    runner: &VmRunner,
    delays: &[Duration],
) -> Result<Checked> {
    let mut retries = delays.iter();
    loop {
        let ran = run_tests(app, s, admin, cwd, rev, &check.dir, &check.command, check.needs, runner).await?;
        let cause = match classify(&ran) {
            Outcome::Failed(_) => ran.tail.as_deref().and_then(crate::retry::verify_network_cause),
            _ => None,
        };
        let Some(cause) = cause else { return Ok(Checked::Ran(ran)) };
        let Some(delay) = retries.next() else {
            return Ok(Checked::Network(cause));
        };
        app.session_log(
            &s.id,
            "info",
            format!(
                "verification: `{}` could not reach the network before its tests ran ({cause}); retrying in {}s",
                check.command,
                delay.as_secs()
            ),
        )
        .await;
        tokio::time::sleep(*delay).await;
    }
}

/// Runs one check on the merge-base's own fresh checkout (`git archive` of it), answering whether
/// the head failure is the base's too. Never contradicts; at worst it could not be checked.
#[allow(clippy::too_many_arguments)]
async fn base_run(
    app: &App,
    s: &Session,
    admin: &Path,
    cwd: &Path,
    merge_base: &str,
    check: &Check,
    runner: &VmRunner,
    delays: &[Duration],
) -> BaseOut {
    let modules = app.modules.read().await.clone();
    let image = s
        .boot_image
        .clone()
        .unwrap_or_else(|| crate::sandbox::configured_image(app, &modules));
    let key = format!("{}\0{image}\0{merge_base}\0{}\0{}", s.worktree, check.dir, check.command);
    let cache = || BASE_RESULTS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(code) = cache().get(&key).copied() {
        return if code == 0 { BaseOut::Passes } else { BaseOut::FailsToo };
    }
    let out = match run_check(app, s, admin, cwd, merge_base, check, runner, delays).await {
        Ok(Checked::Network(cause)) => BaseOut::Unchecked(format!("the base run could not reach the network ({cause})")),
        Ok(Checked::Ran(ran)) => match classify(&ran) {
            Outcome::Green => BaseOut::Passes,
            Outcome::Failed(_) => BaseOut::FailsToo,
            Outcome::MissingTool => BaseOut::Unchecked("the base image is missing the tool the check needs".into()),
            Outcome::Absent => BaseOut::Unchecked("the base image does not carry the command".into()),
            Outcome::NoReport => BaseOut::Unchecked("the base run did not report an exit code".into()),
            Outcome::Sandbox(sandbox) => BaseOut::Unchecked(format!("the base sandbox itself exited {sandbox}")),
        },
        Err(e) => BaseOut::Unchecked(format!("could not run it: {e:#}")),
    };
    if let BaseOut::Passes | BaseOut::FailsToo = out {
        let code = i32::from(matches!(out, BaseOut::FailsToo));
        let mut results = cache();
        if results.len() >= 256 {
            results.clear();
        }
        results.insert(key, code);
    }
    out
}

/// A filesystem-safe log name for a check: `cargo test` at the root gives `cargo-test`,
/// an `npm test` in `web` gives `web-npm-test`.
fn check_slug(check: &Check) -> String {
    let mut slug = String::new();
    for c in format!("{} {}", check.dir, check.command).chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_matches('-').chars().take(48).collect()
}

/// The contradiction a failing head check earns, with its evidence: the failing tests the output
/// names, and where the last lines of the run live (written to the session's `out` directory).
async fn head_failure(cwd: &Path, check: &Check, code: i32, tail: Option<String>) -> String {
    let mut msg = format!("`{}` exited {code} in a fresh checkout", check.command);
    let Some(tail) = tail.filter(|t| !t.trim().is_empty()) else {
        return msg;
    };
    let (names, extra) = failing_tests(&tail);
    if !names.is_empty() {
        msg.push_str("; failing: ");
        msg.push_str(&names.join(", "));
        if extra > 0 {
            msg.push_str(&format!(" and {extra} more"));
        }
    }
    let file = format!("verify-{}.log", check_slug(check));
    let _ = tokio::fs::create_dir_all(cwd.join("out")).await;
    if tokio::fs::write(cwd.join("out").join(&file), format!("{tail}\n"))
        .await
        .is_ok()
    {
        msg.push_str(&format!(" (last {LOG_LINES} lines in out/{file})"));
    }
    msg
}

/// Strips ANSI escape sequences (colour, cursor movement) so run output matches plainly.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut it = text.chars();
    while let Some(c) = it.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match it.next() {
            // CSI: parameter and intermediate bytes, then one final byte @ through ~.
            Some('[') => {
                for c in it.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            // OSC: a title or hyperlink, closed by BEL or ST.
            Some(']') => {
                let mut prev = ' ';
                for c in it.by_ref() {
                    if c == '\x07' || prev == '\x1b' {
                        break;
                    }
                    prev = c;
                }
            }
            _ => {}
        }
    }
    out
}

/// The failing tests a run's output names, deduplicated in order, capped at [`NAME_CAP`] with the
/// count of the rest: cargo's `test <path> ... FAILED` lines and vitest/jest's `FAIL  file >
/// suite` and `×`/`✕`/`✗` marks.
fn failing_tests(output: &str) -> (Vec<String>, usize) {
    let mut names: Vec<String> = Vec::new();
    for line in strip_ansi(output).lines() {
        let line = line.trim();
        let name = line
            .strip_prefix("test ")
            .and_then(|rest| rest.strip_suffix(" ... FAILED"))
            .map(|name| name.trim().to_string())
            .or_else(|| {
                ["FAIL ", "×", "✕", "✗"].iter().find_map(|mark| {
                    line.strip_prefix(mark)
                        .map(str::trim)
                        .filter(|n| !n.is_empty())
                        .map(str::to_string)
                })
            })
            .filter(|name| !names.contains(name));
        if let Some(name) = name {
            names.push(name);
        }
    }
    let extra = names.len().saturating_sub(NAME_CAP);
    (names.into_iter().take(NAME_CAP).collect(), extra)
}

/// The last `lines` lines of a file — the evidence a failing run leaves. `None` when there is
/// nothing to read. A giant log is read from its tail only.
async fn tail_lines(path: &Path, lines: usize) -> Option<String> {
    let mut file = tokio::fs::File::open(path).await.ok()?;
    let len = file.metadata().await.ok()?.len();
    file.seek(std::io::SeekFrom::Start(len.saturating_sub(1 << 20))).await.ok()?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).await.ok()?;
    let text = String::from_utf8_lossy(&buf);
    let mut all: Vec<&str> = text.lines().collect();
    if len > 1 << 20 {
        all.remove(0); // the seek may have landed mid-line
    }
    let start = all.len().saturating_sub(lines);
    Some(all[start..].join("\n"))
}

/// What one fresh-checkout run answered.
struct Ran {
    /// The sandbox's own exit code: corroboration only.
    sandbox: i32,
    /// The number the guest reported, the one the verdict trusts.
    reported: Option<i32>,
    /// The guest found the tool the command needs missing, and ran nothing.
    missing_tool: bool,
    /// The last lines of the command's combined output, when it failed — the evidence.
    tail: Option<String>,
    ms: u64,
}

/// Single-quotes a path for the guest's `sh`, so any check directory reaches the script verbatim.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The guest's script: into the fresh checkout (the check's own subdirectory when it has one),
/// check for the tool the command needs (a miss leaves a `missing` marker and stands in exit 127),
/// run the command with its combined output in the report mount, and report its exit number where
/// only this harness reads it. The redirect is on the command's subshell, so `$?` survives.
fn guest_script(dir: &str, command: &str, needs: Option<&Needs>, toolchain: Option<&str>) -> String {
    let mut cd = if dir.is_empty() {
        "cd /workspace".to_string()
    } else {
        format!("cd {}", shell_quote(&format!("/workspace/{dir}")))
    };
    // Issue #1117: a pinned toolchain the image already has is used as installed, so rustup never
    // goes to static.rust-lang.org for its manifest; with none installed, rustup resolves it as before.
    if let Some(channel) = toolchain {
        cd.push_str(&format!(
            " && if command -v rustup >/dev/null 2>&1; then t=$(rustup toolchain list 2>/dev/null | \
             awk -v c={channel} '$1 == c || index($1, c \"-\") == 1 {{ print $1; exit }}'); \
             if [ -n \"$t\" ]; then export RUSTUP_TOOLCHAIN=\"$t\"; fi; fi",
            channel = shell_quote(channel)
        ));
    }
    match needs {
        Some(needs) => format!(
            "{cd} && if {}; then ({command}) >/colonizer-verify/output 2>&1; else touch /colonizer-verify/missing; \
             (exit 127); fi; echo $? > /colonizer-verify/exit",
            needs.check
        ),
        None => format!("{cd} && ({command}) >/colonizer-verify/output 2>&1; echo $? > /colonizer-verify/exit"),
    }
}

/// Exports the snapshot (or the merge-base, for a base run) into a fresh temp dir under the
/// session directory (no `.git`, nothing shared with the agent's worktree), boots a one-shot
/// microVM from the colony's image with that dir mounted at `/workspace` and a second, empty dir
/// at `/colonizer-verify`, and runs [`guest_script`] there. On a timeout the VM is removed here
/// rather than left to `--max-duration`.
#[allow(clippy::too_many_arguments)]
async fn run_tests(
    app: &App,
    s: &Session,
    admin: &Path,
    cwd: &Path,
    snapshot: &str,
    dir: &str,
    command: &str,
    needs: Option<&Needs>,
    runner: &VmRunner,
) -> Result<Ran> {
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
        // The same public-internet profile a colony boots with (`colony_network` in boot.rs), so
        // `npm ci` / `cargo test` can fetch dependencies, and no host or mesh rules: this VM talks
        // to nothing of the harness's. Left empty, msb's own default would decide instead.
        net_profiles: vec!["public".into()],
        command: vec![
            "sh".into(),
            "-c".into(),
            guest_script(dir, command, needs, pinned_toolchain(&checkout, dir).await.as_deref()),
        ],
        ..Default::default()
    };
    let started = Instant::now();
    let outcome = tokio::time::timeout(TEST_LIMIT, runner(spec)).await;
    let (sandbox, reported, missing_tool) = match outcome {
        Ok(Ok(code)) => (
            code,
            read_exit_report(&report).await,
            tokio::fs::try_exists(report.join("missing")).await.unwrap_or(false),
        ),
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
    // The evidence comes off the report mount before it is swept: the tail of a failing run's
    // output is what a contradiction shows the reviewer.
    let tail = match reported {
        Some(code) if code > 0 => tail_lines(&report.join("output"), LOG_LINES).await,
        _ => None,
    };
    clean_up(&[&checkout, &report]).await;
    Ok(Ran {
        sandbox,
        reported,
        missing_tool,
        tail,
        ms: started.elapsed().as_millis() as u64,
    })
}

/// The Rust toolchain the fresh checkout pins for `dir` — the nearest `rust-toolchain.toml` or
/// `rust-toolchain` at or above it, as rustup looks for one — when its channel is a plain name
/// (`1.98.1`, `stable`, `nightly-2026-01-01`). Read on the host, never executed.
async fn pinned_toolchain(checkout: &Path, dir: &str) -> Option<String> {
    for at in ancestors(dir) {
        for name in ["rust-toolchain.toml", "rust-toolchain"] {
            if let Ok(text) = tokio::fs::read_to_string(checkout.join(join(at, name))).await {
                return pinned_channel(&text);
            }
        }
    }
    None
}

/// The channel a toolchain file names: TOML's `[toolchain] channel = "…"`, or the legacy file's
/// single bare line. `None` for anything that is not a plain toolchain name, so nothing odd ever
/// reaches the guest's shell.
fn pinned_channel(text: &str) -> Option<String> {
    let channel = match toml::from_str::<toml::Table>(text) {
        Ok(table) => table.get("toolchain")?.get("channel")?.as_str()?.trim().to_string(),
        Err(_) => {
            let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#'));
            let line = lines.next()?;
            if lines.next().is_some() {
                return None;
            }
            line.to_string()
        }
    };
    let plain = !channel.is_empty()
        && channel.len() <= 64
        && channel
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        && !channel.starts_with('-');
    plain.then_some(channel)
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
    let verification = verify_claim(&app, &s, &runner, &crate::retry::VERIFY_RETRY_DELAYS).await;
    let verdict = verification.verdict;
    let network = verification.network.clone().filter(|_| verdict == Verdict::Unverifiable);
    let detail = verification.contradictions.join("; ");
    let summary = verification.summary.clone();
    let advisories = verification.advisories.clone();
    let event = verification.event();
    app.update_session(&id, |x| x.verification = Some(verification)).await;
    app.session_log(
        &id,
        if verdict == Verdict::Contradicted { "warn" } else { "info" },
        format!("verification: {summary}"),
    )
    .await;
    for note in advisories {
        app.session_log(&id, "info", format!("verification note (advisory): {note}"))
            .await;
    }
    crate::validation::emit_chain(&app, &id, event).await;
    if !gate_publish {
        return;
    }
    // Issue #1117: the checks never ran for the network, and the retries are spent. Not a
    // contradiction — no test ran — but not a verified claim to publish unseen either: held, with
    // the cause on the card.
    if let Some(cause) = network {
        app.session_log(
            &id,
            "warn",
            format!(
                "autopilot: not publishing, verification could not run (network: {cause}); \
                 press Create PR to publish anyway, or message the agent to verify again"
            ),
        )
        .await;
        app.update_session(&id, |x| {
            x.attention = Some(crate::events::verify_network_held_attention(&cause))
        })
        .await;
        return;
    }
    use crate::events::Autopilot;
    match crate::events::verdict_step(&verdict) {
        Autopilot::Publish => {
            if crate::authority::external_writes_blocked() {
                app.session_log(&id, "warn", crate::events::AUTOPILOT_BLOCKED.into()).await;
            } else if let Some(open) = crate::github_breaker::hold_publish(&app, &id) {
                // Issue #1074: GitHub refuses the account, so the publish waits, like the kill-switch
                // holds it, and goes out by itself once the breaker's probe finds GitHub working.
                app.session_log(
                    &id,
                    "warn",
                    format!(
                        "autopilot: not publishing yet, {}; the publish is held and goes out once GitHub works again",
                        crate::github_breaker::pause_message(&open)
                    ),
                )
                .await;
            } else if let Some(s) = app.session(&id).await.filter(|s| s.status.is_live()) {
                // Issue #98: the confirmed verdict is the approval. The host verifier mints the
                // publish grant — reviewer is the verifier, builder the colony's agent — bound to
                // the tree the publish would commit right now (the same computation the operator's
                // click binds) plus the pr.md bytes; checked at each effect site inside the publish.
                let grant = match crate::publish::mint_publish_grant(&app, &s, "host-verifier").await {
                    Ok(grant) => grant,
                    Err(e) => {
                        app.session_log(
                            &id,
                            "warn",
                            format!("autopilot: not publishing, the publish approval could not be bound: {e:#}"),
                        )
                        .await;
                        return;
                    }
                };
                app.session_log(&id, "info", "autopilot: the claim checked out, publishing".into())
                    .await;
                crate::publish::publish_session(app.clone(), id, Some(grant)).await;
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
        // verdict_step never waits on a verdict, nor schedules a provider-error retry.
        Autopilot::Wait(_) | Autopilot::Retry(_) => {}
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::sessions::{SessionStatus, tests::app_with_colony};
    use std::path::PathBuf;

    #[test]
    fn the_verdict_never_confirms_without_a_green_run() {
        let contradiction = vec!["described `src/absent.rs` is not on the branch".to_string()];
        assert_eq!(decide(&[], false, Some(true)), Verdict::Confirmed);
        // A contradiction wins even over a green run: the branch disagrees with the claim.
        assert_eq!(decide(&contradiction, false, Some(true)), Verdict::Contradicted);
        // An inconclusive failure wins over green — it is not a confirmation — but loses to a
        // contradiction; nothing inconclusive and no green run leaves the claim unjudged.
        assert_eq!(decide(&[], true, Some(true)), Verdict::Inconclusive);
        assert_eq!(decide(&contradiction, true, None), Verdict::Contradicted);
        assert_eq!(decide(&[], false, None), Verdict::Unverifiable);
        // A failing run reaches decide as a contradiction (verify_claim pushes one), so this
        // branch is the belt to that braces.
        assert_eq!(decide(&[], false, Some(false)), Verdict::Unverifiable);
        // The event's shape is the contract the cockpit renders (web/src/types.ts): the record's
        // fields plus its `type`, no more.
        let event = Verification::blank().event();
        assert_eq!(event["type"], "verification");
        assert_eq!(event["advisories"], json!([]));
        assert_eq!(event["inconclusive"], json!([]));
        assert_eq!(event.as_object().unwrap().len(), 16);
        // A record persisted before advisories or inconclusive checks existed still loads.
        let mut old = serde_json::to_value(Verification::blank()).unwrap();
        old.as_object_mut().unwrap().remove("advisories");
        old.as_object_mut().unwrap().remove("inconclusive");
        old.as_object_mut().unwrap().remove("network");
        assert_eq!(serde_json::from_value::<Verification>(old).unwrap(), Verification::blank());
    }

    /// A runner that must never be asked anything: this verdict must not boot a VM.
    fn panicking_runner() -> VmRunner {
        Arc::new(|_| Box::pin(async { panic!("the VM must not boot for this verdict") }))
    }

    /// A runner that answers `sandbox` as the sandbox's own exit and, when `reported` is set,
    /// has the guest's report file say so — the number the verdict trusts.
    fn fake_runner(sandbox: i32, reported: Option<i32>) -> VmRunner {
        Arc::new(move |spec| {
            // Every fresh-checkout VM: the public profile only, no host rules, no secrets.
            assert_eq!(spec.net_profiles, ["public"]);
            assert!(spec.net_rules.is_empty() && spec.secrets.is_empty() && spec.env.is_empty());
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

    /// A runner that answers `(sandbox, reported)` differently for the head checkout and the base
    /// one, told apart by a file only the head snapshot carries; a non-empty `head_output` is
    /// written beside the report on the head run — the evidence a failing run leaves.
    fn phased_runner(
        marker: &'static str,
        base: (i32, Option<i32>),
        head: (i32, Option<i32>),
        head_output: &'static str,
    ) -> VmRunner {
        Arc::new(move |spec| {
            assert_eq!(spec.net_profiles, ["public"]);
            Box::pin(async move {
                let workspace = &spec
                    .mounts
                    .iter()
                    .find(|m| m.target == "/workspace")
                    .expect("the fresh checkout is mounted at /workspace")
                    .source;
                let head_run = tokio::fs::try_exists(workspace.join(marker)).await.unwrap_or(false);
                let (sandbox, reported) = if head_run { head } else { base };
                let report = spec
                    .mounts
                    .iter()
                    .find(|m| m.target == "/colonizer-verify")
                    .expect("the report mount")
                    .source
                    .clone();
                if head_run && !head_output.is_empty() {
                    tokio::fs::write(report.join("output"), head_output).await?;
                }
                if let Some(code) = reported {
                    tokio::fs::write(report.join("exit"), code.to_string()).await?;
                }
                Ok(sandbox)
            })
        })
    }

    #[test]
    fn the_command_resolution_order_is_js_then_cargo_then_make() {
        let package = |test: &str| Some(format!(r#"{{"scripts": {{"test": "{test}"}}}}"#));
        let base = |package_json, lockfiles: &[&str], cargo_toml, makefile| BaseFiles {
            package_json,
            lockfiles: lockfiles.iter().map(|l| l.to_string()).collect(),
            cargo_toml,
            makefile,
            ..Default::default()
        };
        // (files, the source and command they declare) — JS first, then cargo, then make, and
        // npm's placeholder script (or no files at all, or a Makefile without a `test:` target)
        // declares nothing.
        let cases: Vec<(BaseFiles, Option<(&str, &str)>)> = vec![
            (
                base(package("node --test"), &["package-lock.json"], true, Some("test:\n".into())),
                Some(("package-lock.json", "npm ci && npm test")),
            ),
            // No lockfile on the base branch means npm install, not npm ci.
            (
                base(package("jest"), &[], false, None),
                Some(("package.json", "npm install && npm test")),
            ),
            (
                base(
                    package(r#"echo \"Error: no test specified\" && exit 1"#),
                    &["package-lock.json"],
                    true,
                    None,
                ),
                Some(("Cargo.toml", "cargo test")),
            ),
            (
                base(None, &[], false, Some("build:\n\techo hi\ntest: build\n".into())),
                Some(("Makefile", "make test")),
            ),
            (base(None, &[], false, Some("build:\n".into())), None),
            (base(None, &[], false, None), None),
        ];
        for (files, want) in cases {
            assert_eq!(
                declared_test_command(&files).map(|d| (d.source, d.command)),
                want.map(|(source, command)| (source, command.to_string())),
                "{files:?}"
            );
        }
    }

    /// Colony 4ddc1540 on a bun repository (bun.lock, no package-lock.json, no `packageManager`,
    /// `scripts.test` = jest) was verified with `npm install && npm test`, which failed in the fresh
    /// checkout and contradicted the claim for twenty hours. The package manager is the
    /// repository's own: the `packageManager` field, else the lockfile, else npm.
    #[test]
    fn the_test_command_runs_the_repositorys_own_package_manager() {
        let package = |extra: &str| Some(format!(r#"{{"scripts": {{"test": "jest"}}{extra}}}"#));
        let files = |package_json: Option<String>, lockfiles: &[&str]| BaseFiles {
            package_json,
            lockfiles: lockfiles.iter().map(|l| l.to_string()).collect(),
            ..Default::default()
        };
        // (files, source, command, tool it needs)
        let cases: Vec<(BaseFiles, &str, &str, &str)> = vec![
            // The incident: bun.lock alone picks bun, and runs the repository's own script.
            (
                files(package(""), &["bun.lock"]),
                "bun.lock",
                "bun install --frozen-lockfile && bun run test",
                "bun",
            ),
            (
                files(package(""), &["bun.lockb"]),
                "bun.lockb",
                "bun install --frozen-lockfile && bun run test",
                "bun",
            ),
            (
                files(package(""), &["pnpm-lock.yaml"]),
                "pnpm-lock.yaml",
                "pnpm install --frozen-lockfile && pnpm test",
                "pnpm",
            ),
            (
                files(package(""), &["yarn.lock"]),
                "yarn.lock",
                "yarn install --frozen-lockfile && yarn test",
                "yarn",
            ),
            (
                BaseFiles {
                    yarn_berry_lock: true,
                    ..files(package(""), &["yarn.lock"])
                },
                "yarn.lock",
                "yarn install --immutable && yarn test",
                "yarn 2 or later",
            ),
            (
                files(package(""), &["package-lock.json"]),
                "package-lock.json",
                "npm ci && npm test",
                "npm",
            ),
            (
                files(package(""), &["npm-shrinkwrap.json"]),
                "npm-shrinkwrap.json",
                "npm ci && npm test",
                "npm",
            ),
            // No lockfile: today's npm fallback.
            (files(package(""), &[]), "package.json", "npm install && npm test", "npm"),
            // A stray package-lock beside bun.lock does not outvote it.
            (
                files(package(""), &["bun.lock", "package-lock.json"]),
                "bun.lock",
                "bun install --frozen-lockfile && bun run test",
                "bun",
            ),
            // The `packageManager` field (corepack) outranks every lockfile, and pins the version
            // corepack runs.
            (
                files(
                    package(r#", "packageManager": "pnpm@9.12.0+sha512.abc""#),
                    &["package-lock.json", "pnpm-lock.yaml"],
                ),
                "packageManager",
                "corepack pnpm install --frozen-lockfile && corepack pnpm test",
                "corepack",
            ),
            (
                files(package(r#", "packageManager": "yarn@4.5.0""#), &["yarn.lock"]),
                "packageManager",
                "corepack yarn install --immutable && corepack yarn test",
                "corepack",
            ),
            (
                files(package(r#", "packageManager": "yarn@1.22.22""#), &["yarn.lock"]),
                "packageManager",
                "corepack yarn install --frozen-lockfile && corepack yarn test",
                "corepack",
            ),
            (
                files(package(r#", "packageManager": "bun@1.2.0""#), &["package-lock.json"]),
                "packageManager",
                "bun install && bun run test",
                "bun",
            ),
            (
                files(package(r#", "packageManager": "npm@10.9.0""#), &["package-lock.json"]),
                "packageManager",
                "npm ci && npm test",
                "npm",
            ),
            // A manager this does not know is ignored, and the lockfile decides.
            (
                files(package(r#", "packageManager": "deno@2.0.0""#), &["pnpm-lock.yaml"]),
                "pnpm-lock.yaml",
                "pnpm install --frozen-lockfile && pnpm test",
                "pnpm",
            ),
        ];
        for (files, source, command, tool) in cases {
            let d = declared_test_command(&files).unwrap_or_else(|| panic!("{files:?} declares nothing"));
            assert_eq!(
                (d.source, d.command.as_str(), d.needs.tool),
                (source, command, tool),
                "{files:?}"
            );
            assert!(d.runs_script, "every one of these runs `scripts.test`: {files:?}");
        }

        // bun without `scripts.test`: bun's own runner, but only over test files it would find —
        // `bun test` with none fails, and that failure would not be the colony's.
        let no_script = || files(Some(r#"{"name": "x"}"#.into()), &["bun.lock"]);
        let d = declared_test_command(&BaseFiles {
            bun_test_files: true,
            ..no_script()
        })
        .expect("bun test files declare `bun test`");
        assert_eq!(
            (d.source, d.command.as_str()),
            ("bun.lock", "bun install --frozen-lockfile && bun test")
        );
        assert!(!d.runs_script, "`bun test` runs no script a branch could rewrite");
        assert_eq!(declared_test_command(&no_script()), None);
        // Without a script, pnpm, yarn and npm declare nothing and the next source is asked.
        let d = declared_test_command(&BaseFiles {
            cargo_toml: true,
            ..files(Some("{}".into()), &["pnpm-lock.yaml"])
        })
        .unwrap();
        assert_eq!((d.source, d.command.as_str()), ("Cargo.toml", "cargo test"));

        for (path, found) in [
            ("src/a.test.ts", true),
            ("test/b_spec.js", true),
            ("c.spec.tsx", true),
            ("lib/d_test.mjs", true),
            ("src/a.ts", false),
            ("src/test.ts", false),
            ("src/a.test.rs", false),
            ("node_modules/x/a.test.js", false),
        ] {
            assert_eq!(is_bun_test_file(path), found, "{path}");
        }
    }

    /// Each tool check is a shell test the guest runs before the command, and a miss leaves the
    /// marker the verdict reads.
    #[test]
    fn the_guest_checks_the_tool_before_it_runs_the_command() {
        assert_eq!(
            guest_script("", "true", None, None),
            "cd /workspace && (true) >/colonizer-verify/output 2>&1; echo $? > /colonizer-verify/exit"
        );
        assert_eq!(
            guest_script("web", "bun install && bun run test", Some(&BUN), None),
            "cd '/workspace/web' && if command -v bun >/dev/null 2>&1; then (bun install && bun run test) \
             >/colonizer-verify/output 2>&1; else touch /colonizer-verify/missing; (exit 127); fi; \
             echo $? > /colonizer-verify/exit"
        );
        // The directory is quoted, so anything in it reaches the script verbatim.
        assert_eq!(
            guest_script("weird dir", "true", None, None),
            "cd '/workspace/weird dir' && (true) >/colonizer-verify/output 2>&1; echo $? > /colonizer-verify/exit"
        );
        // Run for real: a present tool runs the command, a missing one reports 127 and the marker,
        // and the command's own output — not its $? — is what lands in the report mount.
        let dir = std::env::temp_dir().join(format!("colonizer-guest-{}", short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let run = |needs: &Needs, command: &str| {
            let _ = std::fs::remove_file(dir.join("missing"));
            let script = guest_script("", command, Some(needs), None)
                .replace("cd /workspace", &format!("cd {}", dir.display()))
                .replace("/colonizer-verify", &dir.display().to_string());
            let out = std::process::Command::new("sh").args(["-c", &script]).status().unwrap();
            assert!(out.success());
            let code: i32 = std::fs::read_to_string(dir.join("exit")).unwrap().trim().parse().unwrap();
            (code, dir.join("missing").exists())
        };
        let sh = needs("sh", "command -v sh >/dev/null 2>&1");
        assert_eq!(run(&sh, "echo hello; exit 3"), (3, false));
        assert_eq!(std::fs::read_to_string(dir.join("output")).unwrap(), "hello\n");
        assert_eq!(run(&sh, "true"), (0, false));
        let absent = needs("no-such-tool", "command -v colonizer-no-such-tool >/dev/null 2>&1");
        assert_eq!(run(&absent, "true"), (127, true));
        let _ = std::fs::remove_dir_all(dir);
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

    #[test]
    fn only_a_description_with_nothing_on_the_branch_contradicts() {
        let paths = |ps: &[&str]| ps.iter().map(|p| p.to_string()).collect::<Vec<_>>();
        // Nothing missing: nothing to say.
        assert_eq!(weigh_described(true, &[]), (vec![], vec![]));
        assert_eq!(weigh_described(false, &[]), (vec![], vec![]));
        // Missing paths beside corroborated ones are advisories, once each.
        let (contradictions, advisories) = weigh_described(true, &paths(&["docs/a.md", "docs/a.md", "src/b.rs"]));
        assert!(contradictions.is_empty());
        assert_eq!(
            advisories,
            vec![
                "described `docs/a.md` is not on the branch".to_string(),
                "described `src/b.rs` is not on the branch".to_string(),
            ]
        );
        // Every described path missing, nothing corroborated: the claim is about work that is not there.
        let (contradictions, advisories) = weigh_described(false, &paths(&["src/absent.rs"]));
        assert_eq!(
            contradictions,
            vec!["described `src/absent.rs` is not on the branch, and it is the only file the description names".to_string()]
        );
        assert!(advisories.is_empty());
        let (contradictions, _) = weigh_described(false, &paths(&["src/a.rs", "src/b.rs", "src/a.rs"]));
        assert_eq!(
            contradictions,
            vec!["none of the 2 files the description names is on the branch: `src/a.rs`, `src/b.rs`".to_string()]
        );
        // The diff corroborates a description that names a changed file, by path or by file name in prose.
        let changed = paths(&["docs/remote-tunnel.md"]);
        assert!(names_a_changed_file(
            "I named the file remote-tunnel.md to avoid a clash",
            &changed
        ));
        assert!(names_a_changed_file("adds `docs/remote-tunnel.md`", &changed));
        assert!(!names_a_changed_file("a likely `docs/remote-access.md`", &changed));
        assert!(
            !names_a_changed_file("anything", &paths(&["Makefile"])),
            "no extension, no file-name match"
        );
    }

    #[test]
    fn the_pull_request_carries_advisories_as_notes() {
        assert_eq!(pr_notes(None), None);
        assert_eq!(pr_notes(Some(&Verification::blank())), None, "no advisories, no notes");
        let mut v = Verification::blank();
        v.advisories = vec!["described `docs/remote-access.md` is not on the branch".into()];
        assert_eq!(
            pr_notes(Some(&v)).as_deref(),
            Some(
                "> **Verification notes** (advisory; they did not change the verdict):\n\
                 > - described `docs/remote-access.md` is not on the branch"
            )
        );
        // An inconclusive check is noted too, so the reviewer knows the failure is not this change's.
        let mut v = Verification::blank();
        v.inconclusive = vec!["`cargo test` fails on the base commit as well".into()];
        assert_eq!(
            pr_notes(Some(&v)).as_deref(),
            Some(
                "> **Verification notes** (advisory; they did not change the verdict):\n\
                 > - Verification was inconclusive: `cargo test` fails on the base commit as well, \
                 so the failure is not attributed to this change."
            )
        );
    }

    /// Runs `git` synchronously against a fixture repo — setup and inspection, not the code
    /// under test. The `GIT_DIR`/`GIT_WORK_TREE`/`GIT_INDEX_FILE` a colony sandbox exports for
    /// its own worktree are dropped, so the fixture is the only repo git sees.
    pub(crate) fn git(dir: &Path, args: &[&str]) -> String {
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

    pub(crate) fn git_commit(dir: &Path, message: &str) {
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
        verify_claim(app, &s, runner, &[Duration::ZERO; 3]).await
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

        let red = verify(&app, &phased_runner("uncommitted.txt", (0, Some(0)), (3, Some(3)), "")).await;
        assert_eq!(red.verdict, Verdict::Contradicted, "{red:?}");
        assert_eq!(red.exit_code, Some(3));
        assert!(red.summary.contains("exited 3 in a fresh checkout"), "{}", red.summary);
        assert_eq!(red.contradictions, vec!["`true` exited 3 in a fresh checkout".to_string()]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn an_empty_branch_or_a_claim_whose_described_files_are_all_absent() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        // A green run cannot save a claim whose described files are all missing from the branch —
        // counting only paths whose directory exists on the branch: `example.com/foo.md` and a
        // gitignored `dist/bundle.js` are the claim's wording, not evidence against it.
        worktree_fixture(
            &app,
            Some("true"),
            "rewrote `src/absent.rs` and `src/gone.rs`, see `example.com/foo.md`, built `dist/bundle.js`",
            true,
        )
        .await;
        let v = verify(&app, &fake_runner(0, Some(0))).await;
        assert_eq!(v.verdict, Verdict::Contradicted, "{v:?}");
        assert_eq!(v.exit_code, None, "no VM run is paid for once the git state contradicts");
        assert_eq!(
            v.contradictions,
            vec!["none of the 2 files the description names is on the branch: `src/absent.rs`, `src/gone.rs`".to_string()]
        );
        assert!(v.advisories.is_empty(), "{v:?}");

        // One real file described beside the absent one: the absent one is advisory, the tests
        // decide the verdict.
        worktree_fixture(
            &app,
            Some("true"),
            "added `src/real.txt`; `src/absent.rs` is left for #9",
            true,
        )
        .await;
        let v = verify(&app, &fake_runner(0, Some(0))).await;
        assert_eq!(v.verdict, Verdict::Confirmed, "{v:?}");
        assert!(v.contradictions.is_empty(), "{v:?}");
        assert_eq!(
            v.advisories,
            vec!["described `src/absent.rs` is not on the branch".to_string()]
        );

        // A branch with nothing on it against the base is a "no change needed" answer: nothing
        // to verify, so unverifiable — it publishes as no changes instead of holding autopilot —
        // and no VM run is paid for.
        worktree_fixture(&app, Some("true"), "the repository needs no change", false).await;
        let v = verify(&app, &fake_runner(0, Some(0))).await;
        assert_eq!(v.verdict, Verdict::Unverifiable, "{v:?}");
        assert!(v.contradictions.is_empty(), "{v:?}");
        assert_eq!(v.exit_code, None, "{v:?}");
        assert_eq!(v.summary, "branch has no changes against main; nothing to verify");
        let _ = std::fs::remove_dir_all(root);
    }

    /// Colony 5b460871 on #531: a docs-only branch adding `docs/remote-tunnel.md`, whose description
    /// explains the name by pointing at a `docs/remote-access.md` it deliberately did not create
    /// (twice). That is a note for the reviewer, listed once — not a contradiction, and autopilot
    /// is not held.
    #[tokio::test]
    async fn a_described_file_the_colony_deliberately_avoided_is_advisory_not_contradicted() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        let pr = "Adds a remote-access tunnel doc for #531.\n\n\
                  The #536 review may write its own remote-access doc. I named the file remote-tunnel.md \
                  to avoid clashing with a likely `docs/remote-access.md`; if #536 lands `docs/remote-access.md`, \
                  the two should link to each other.";
        let repo = worktree_fixture(&app, Some("true"), pr, false).await;
        std::fs::create_dir_all(repo.join("docs")).unwrap();
        std::fs::write(repo.join("docs/remote-tunnel.md"), "# Remote tunnel\n").unwrap();
        git(&repo, &["add", "-A"]);
        git_commit(&repo, "docs: remote tunnel");
        let v = verify(&app, &fake_runner(0, Some(0))).await;
        assert_eq!(v.verdict, Verdict::Confirmed, "{v:?}");
        assert!(v.contradictions.is_empty(), "{v:?}");
        assert_eq!(
            v.advisories,
            vec!["described `docs/remote-access.md` is not on the branch".to_string()],
            "once, however often the description names it"
        );
        assert_eq!(
            crate::events::verdict_step(&v.verdict),
            crate::events::Autopilot::Publish,
            "autopilot is not held"
        );

        // The same branch, the new file backticked too: still advisory.
        let out = app.session_dir("abc").join("out");
        std::fs::write(
            out.join("pr.md"),
            "Adds `docs/remote-tunnel.md`, not `docs/remote-access.md`.",
        )
        .unwrap();
        let v = verify(&app, &fake_runner(0, Some(0))).await;
        assert_eq!(v.verdict, Verdict::Confirmed, "{v:?}");
        assert_eq!(v.advisories.len(), 1, "{v:?}");
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

    /// The six placeholders colony 4ddc1540's boot left in its worktree for the path policy's binds
    /// — empty, untracked, not ignored — as the policy list and placeholder list record them.
    pub(crate) const INCIDENT_PLACEHOLDERS: [&str; 6] =
        [".envrc", ".git-credentials", ".gitmodules", ".mcp.json", ".netrc", ".pypirc"];

    /// Writes a boot's path-policy records into the colony's `vm/` dir and creates the placeholders
    /// in the worktree, the way `boot.rs` does: `.env` masked over a real tracked file, the six
    /// incident paths as empty placeholders.
    pub(crate) fn materialise_incident_policy(vm_dir: &Path, repo: &Path, with_list: bool) {
        let _ = std::fs::remove_dir_all(vm_dir);
        std::fs::create_dir_all(vm_dir).unwrap();
        let binds = [
            "mask-file .env",
            "mask-file .envrc",
            "mask-file .netrc",
            "mask-file .git-credentials",
            "mask-file .pypirc",
            "protect .gitmodules",
            "protect .mcp.json",
        ];
        std::fs::write(vm_dir.join("path-policy"), binds.join("\n") + "\n").unwrap();
        if with_list {
            std::fs::write(
                vm_dir.join("path-policy.placeholders"),
                INCIDENT_PLACEHOLDERS.join("\n") + "\n",
            )
            .unwrap();
        }
        for p in INCIDENT_PLACEHOLDERS {
            std::fs::write(repo.join(p), "").unwrap();
        }
    }

    /// Colony 4ddc1540: the boot's empty placeholders sat untracked in the worktree, and the
    /// verification snapshot counted all six as changed files. They are the policy's, not the
    /// colony's work: the snapshot holds them back (with or without the placeholder list), a
    /// masked real file stays as the repository has it, and the agent's worktree is untouched.
    #[tokio::test]
    async fn path_policy_placeholders_are_not_the_colonys_changes() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        for with_list in [true, false] {
            let repo = worktree_fixture(&app, Some("true"), "fixed `src/fix.ts`", false).await;
            std::fs::write(repo.join(".env"), "SECRET=real\n").unwrap();
            git(&repo, &["add", "-A"]);
            git_commit(&repo, "the repository carries a real .env");
            git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
            materialise_incident_policy(&app.session_dir("abc").join("vm"), &repo, with_list);
            std::fs::create_dir_all(repo.join("src")).unwrap();
            std::fs::write(repo.join("src/fix.ts"), "export const fixed = true;\n").unwrap();
            // Suppose the masked .env changed on the host all the same (a colony that got past
            // its mask): still not the colony's to change.
            std::fs::write(repo.join(".env"), "").unwrap();
            let status = git(&repo, &["status", "--porcelain"]);

            let v = verify(&app, &fake_runner(0, Some(0))).await;
            assert_eq!(v.verdict, Verdict::Confirmed, "with_list={with_list}: {v:?}");
            assert_eq!(v.files_changed, vec!["src/fix.ts".to_string()], "with_list={with_list}");
            let snapshot = v.snapshot.expect("a snapshot");
            let tree = git(&repo, &["ls-tree", "-r", "--name-only", &snapshot]);
            for p in INCIDENT_PLACEHOLDERS {
                assert!(!tree.lines().any(|l| l == p), "{p} is not in the snapshot: {tree}");
            }
            assert_eq!(
                git(&repo, &["show", &format!("{snapshot}:.env")]),
                "SECRET=real",
                "the masked file is snapshotted as the repository has it"
            );
            assert_eq!(git(&repo, &["status", "--porcelain"]), status, "the worktree is untouched");
            for p in INCIDENT_PLACEHOLDERS {
                assert!(repo.join(p).exists(), "verification removes nothing: {p}");
            }
        }
        let _ = std::fs::remove_dir_all(root);
    }

    /// A runner standing in for a colony image without the tool the command checks for: the guest
    /// script's check fails, so it leaves the `missing` marker and reports 127.
    fn missing_tool_runner(tool_check: &'static str) -> VmRunner {
        Arc::new(move |spec| {
            assert!(
                spec.command.last().is_some_and(|script| script.contains(tool_check)),
                "the guest checks for the tool first: {:?}",
                spec.command
            );
            Box::pin(async move {
                let report = spec.mounts.iter().find(|m| m.target == "/colonizer-verify").unwrap();
                tokio::fs::write(report.source.join("missing"), "").await?;
                tokio::fs::write(report.source.join("exit"), "127").await?;
                Ok(0)
            })
        })
    }

    /// The incident end to end: a bun repository's base branch picks bun from bun.lock (never
    /// `npm install`), the colony image has no bun, and the claim is unverifiable — named plainly —
    /// instead of contradicted, so autopilot is not held on it.
    #[tokio::test]
    async fn a_bun_repository_without_bun_in_the_image_is_unverifiable_not_contradicted() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        let repo = worktree_fixture(&app, None, "did the work", false).await;
        std::fs::write(
            repo.join("package.json"),
            r#"{"name": "chi-backend", "scripts": {"test": "jest"}}"#,
        )
        .unwrap();
        std::fs::write(repo.join("bun.lock"), "{\n  \"lockfileVersion\": 1,\n}\n").unwrap();
        git(&repo, &["add", "-A"]);
        git_commit(&repo, "a bun repository");
        git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/fix.ts"), "export const fixed = true;\n").unwrap();

        let v = verify(&app, &missing_tool_runner(BUN.check)).await;
        assert_eq!(v.verdict, Verdict::Unverifiable, "{v:?}");
        assert!(v.contradictions.is_empty(), "{v:?}");
        assert_eq!(v.command.as_deref(), Some("bun install --frozen-lockfile && bun run test"));
        assert_eq!(v.command_source.as_deref(), Some("bun.lock"));
        assert_eq!(v.exit_code, Some(127));
        assert_eq!(
            v.summary,
            "`bun` (picked from `bun.lock`) is not in the colony image, so \
             `bun install --frozen-lockfile && bun run test` could not run (exit 127)"
        );
        assert_eq!(
            crate::events::verdict_step(&v.verdict),
            crate::events::Autopilot::Publish,
            "an unverifiable claim does not hold autopilot"
        );

        // With bun in the image the same run decides as usual: green confirms, and a red the base
        // commit does not share contradicts.
        let v = verify(&app, &fake_runner(0, Some(0))).await;
        assert_eq!(v.verdict, Verdict::Confirmed, "{v:?}");
        let v = verify(&app, &phased_runner("src/fix.ts", (0, Some(0)), (1, Some(1)), "")).await;
        assert_eq!(v.verdict, Verdict::Contradicted, "{v:?}");
        assert_eq!(
            v.contradictions,
            vec!["`bun install --frozen-lockfile && bun run test` exited 1 in a fresh checkout".to_string()]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// The same fallback for pnpm (#589): the stock node image carries neither bun nor pnpm, so a
    /// pnpm repository verified there is named unverifiable, never contradicted by a failed install.
    #[tokio::test]
    async fn a_pnpm_repository_without_pnpm_in_the_image_is_unverifiable_not_contradicted() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        let repo = worktree_fixture(&app, None, "did the work", false).await;
        std::fs::write(
            repo.join("package.json"),
            r#"{"name": "web", "scripts": {"test": "vitest run"}}"#,
        )
        .unwrap();
        std::fs::write(repo.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'\n").unwrap();
        git(&repo, &["add", "-A"]);
        git_commit(&repo, "a pnpm repository");
        git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/fix.ts"), "export const fixed = true;\n").unwrap();

        let v = verify(&app, &missing_tool_runner(PNPM.check)).await;
        assert_eq!(v.verdict, Verdict::Unverifiable, "{v:?}");
        assert!(v.contradictions.is_empty(), "{v:?}");
        assert_eq!(v.command_source.as_deref(), Some("pnpm-lock.yaml"));
        assert_eq!(v.exit_code, Some(127));
        assert_eq!(
            v.summary,
            "`pnpm` (picked from `pnpm-lock.yaml`) is not in the colony image, so \
             `pnpm install --frozen-lockfile && pnpm test` could not run (exit 127)"
        );
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

        // `auto` with nothing that applies to the diff names no command to run.
        worktree_fixture(&app, None, "did the work", true).await;
        let v = verify(&app, &panicking_runner()).await;
        assert_eq!(v.verdict, Verdict::Unverifiable, "{v:?}");
        assert!(
            v.summary.contains("no test command applies to the files this diff touches"),
            "{}",
            v.summary
        );
        assert_eq!(v.command, None);
        let _ = std::fs::remove_dir_all(root);
    }

    /// A branch that deletes a package directory the base declares tests for leaves the check
    /// pointing at a directory the snapshot no longer has: skipped — the `cd`'s own exit 1 must
    /// never become a contradiction.
    #[tokio::test]
    async fn a_deleted_package_directory_is_skipped_not_contradicted() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        let repo = worktree_fixture(&app, None, "removed the web package", false).await;
        std::fs::create_dir_all(repo.join("web/src")).unwrap();
        std::fs::write(repo.join("web/package.json"), r#"{"scripts": {"test": "vitest"}}"#).unwrap();
        std::fs::write(repo.join("web/package-lock.json"), "{}\n").unwrap();
        std::fs::write(repo.join("web/src/a.ts"), "export {};\n").unwrap();
        git(&repo, &["add", "-A"]);
        git_commit(&repo, "a web package");
        git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(&repo, &["rm", "-q", "-r", "web"]);
        git_commit(&repo, "remove web");

        let v = verify(&app, &panicking_runner()).await;
        assert_eq!(v.verdict, Verdict::Unverifiable, "{v:?}");
        assert!(v.summary.contains("no test command applies"), "{}", v.summary);
        assert_eq!(v.exit_code, None, "no VM booted to fail in a deleted directory");
        let _ = std::fs::remove_dir_all(root);
    }

    /// Two crates on the base, `a` and `b`; the branch changes one file in `a` and two in `b`, so
    /// the diff's order runs `a` first while focus (#584) picks `b`. `mode` is `verify_focus`.
    async fn two_crate_fixture(app: &crate::Shared, mode: &str) {
        app.modules
            .write()
            .await
            .publish
            .settings
            .insert("verify_focus".into(), json!(mode));
        let repo = worktree_fixture(app, None, "did the work", false).await;
        for dir in ["a", "b"] {
            std::fs::create_dir_all(repo.join(dir).join("src")).unwrap();
            std::fs::write(repo.join(dir).join("Cargo.toml"), "[package]\n").unwrap();
        }
        git(&repo, &["add", "-A"]);
        git_commit(&repo, "two crates");
        git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        for file in ["a/src/x.rs", "b/src/y.rs", "b/src/z.rs"] {
            std::fs::write(repo.join(file), "fn main() {}\n").unwrap();
        }
        git(&repo, &["add", "-A"]);
        git_commit(&repo, "work");
    }

    /// Each run a [`crate_runner`] saw, as `(dir, head?)`, in order.
    type Calls = Arc<Mutex<Vec<(String, bool)>>>;

    /// A runner for [`two_crate_fixture`] that logs each run and fails the head run in `failing`
    /// (the base always passes, so that failure contradicts).
    fn crate_runner(failing: Option<&'static str>) -> (VmRunner, Calls) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let log = calls.clone();
        let runner: VmRunner = Arc::new(move |spec| {
            let log = log.clone();
            Box::pin(async move {
                let ws = spec.mounts.iter().find(|m| m.target == "/workspace").unwrap().source.clone();
                let head = tokio::fs::try_exists(ws.join("b/src/y.rs")).await.unwrap_or(false);
                let dir = if spec.command[2].contains("/workspace/a'") { "a" } else { "b" };
                log.lock().unwrap().push((dir.to_string(), head));
                let code = i32::from(head && failing == Some(dir));
                let report = spec.mounts.iter().find(|m| m.target == "/colonizer-verify").unwrap();
                tokio::fs::write(report.source.join("exit"), code.to_string()).await?;
                Ok(code)
            })
        });
        (runner, calls)
    }

    fn focus_rows(app: &crate::Shared) -> Vec<Value> {
        std::fs::read_to_string(app.jev_focus_file())
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn runs(calls: &Calls) -> Vec<(String, bool)> {
        calls.lock().unwrap().clone()
    }

    /// Act: the focused check runs first; its contradiction stops the verification there.
    #[tokio::test]
    async fn act_mode_stops_at_the_focused_checks_failure() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        two_crate_fixture(&app, "act").await;
        let (runner, calls) = crate_runner(Some("b"));
        let v = verify(&app, &runner).await;
        assert_eq!(v.verdict, Verdict::Contradicted, "{v:?}");
        assert_eq!(runs(&calls), [("b".into(), true), ("b".into(), false)], "a never ran");
        let rows = focus_rows(&app);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["mode"], "act");
        assert_eq!(rows[0]["chosen"], "b: cargo test");
        assert_eq!(rows[0]["would_catch"], true);
        assert_eq!(rows[0]["checks_run"], 1);
        assert_eq!(rows[0]["candidates"][1], json!({"label": "b: cargo test", "owned": 2}));
        let _ = std::fs::remove_dir_all(root);
    }

    /// The acceptance test for #584: in act mode a confirmed verdict still ran the full suite.
    #[tokio::test]
    async fn act_mode_confirms_only_after_the_full_suite() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        two_crate_fixture(&app, "act").await;
        let (runner, calls) = crate_runner(None);
        let v = verify(&app, &runner).await;
        assert_eq!(v.verdict, Verdict::Confirmed, "{v:?}");
        assert_eq!(
            runs(&calls),
            [("b".into(), true), ("a".into(), true)],
            "focused first, then the rest"
        );
        assert_eq!(focus_rows(&app)[0]["would_catch"], Value::Null, "nothing failed");
        // A failure outside the focused check is still found by the rest of the suite.
        two_crate_fixture(&app, "act").await;
        let (runner, calls) = crate_runner(Some("a"));
        let v = verify(&app, &runner).await;
        assert_eq!(v.verdict, Verdict::Contradicted, "{v:?}");
        assert_eq!(runs(&calls), [("b".into(), true), ("a".into(), true), ("a".into(), false)]);
        assert_eq!(focus_rows(&app)[1]["would_catch"], false);
        let _ = std::fs::remove_dir_all(root);
    }

    /// Shadow runs exactly what off runs, in the same order, with the same verdict — and only
    /// records what focus would have done.
    #[tokio::test]
    async fn shadow_mode_changes_nothing_but_the_ledger() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        let mut seen = Vec::new();
        for mode in ["off", "shadow"] {
            two_crate_fixture(&app, mode).await;
            let (runner, calls) = crate_runner(Some("b"));
            let v = verify(&app, &runner).await;
            seen.push((v.verdict, v.contradictions, runs(&calls)));
            assert_eq!(focus_rows(&app).len(), usize::from(mode == "shadow"), "off records nothing");
        }
        assert_eq!(seen[0], seen[1]);
        assert_eq!(seen[1].0, Verdict::Contradicted);
        assert_eq!(seen[1].2, [("a".into(), true), ("b".into(), true), ("b".into(), false)]);
        let row = &focus_rows(&app)[0];
        assert_eq!(
            (&row["mode"], &row["chosen"], &row["would_catch"]),
            (&json!("shadow"), &json!("b: cargo test"), &json!(true))
        );
        assert_eq!(row["verdict"], "contradicted");
        assert_eq!(row["checks_run"], 2);
        let _ = std::fs::remove_dir_all(root);
    }

    /// A base tree for the pure selection: the listed paths carry, the pairs read as contents.
    fn tree(files: &[&str], contents: &[(&str, &str)]) -> BaseTree {
        BaseTree {
            files: files.iter().map(|f| f.to_string()).collect(),
            contents: contents.iter().map(|(p, c)| (p.to_string(), c.to_string())).collect(),
        }
    }

    fn picked(paths: &[&str], base: &BaseTree) -> Vec<(String, String)> {
        select_checks(&paths.iter().map(|p| p.to_string()).collect::<Vec<_>>(), base)
            .into_iter()
            .map(|c| (c.dir, c.command))
            .collect()
    }

    /// The checks are diff-scoped: a Rust file runs cargo at its nearest Cargo.toml ancestor and
    /// never a package's test; a touched package runs its own declared test; a lockfile wakes only
    /// its own package; docs wake nothing the base does not declare at the root.
    #[test]
    fn checks_follow_the_diff_not_the_whole_tree() {
        let package = |test: &str| format!(r#"{{"scripts": {{"test": "{test}"}}}}"#);
        let monorepo = tree(
            &[
                "Cargo.toml",
                "crates/colonizer/Cargo.toml",
                "package.json",
                "web/package.json",
                "web/package-lock.json",
            ],
            &[
                ("package.json", package("node --test").as_str()),
                ("web/package.json", package("vitest").as_str()),
            ],
        );
        // Rust changes run cargo — at the nearest manifest's directory, not the workspace root's —
        // and nothing else.
        assert_eq!(
            picked(&["crates/colonizer/src/verify.rs"], &monorepo),
            vec![("crates/colonizer".into(), "cargo test".into())]
        );
        // A web-only change runs the web package's own declared test: no cargo, no root npm.
        assert_eq!(
            picked(&["web/src/a.ts"], &monorepo),
            vec![("web".into(), "npm ci && npm test".into())]
        );
        // The lockfile alone is a web change all the same; a module's lockfile wakes only that
        // module, and only when it declares a test.
        assert_eq!(
            picked(&["web/package-lock.json"], &monorepo),
            vec![("web".into(), "npm ci && npm test".into())]
        );
        let with_module = tree(
            &["modules/agents/pi/package.json", "modules/agents/pi/package-lock.json"],
            &[("modules/agents/pi/package.json", package("node --test").as_str())],
        );
        assert_eq!(
            picked(&["modules/agents/pi/package-lock.json"], &with_module),
            vec![("modules/agents/pi".into(), "npm ci && npm test".into())]
        );
        let mute_module = tree(
            &["modules/agents/pi/package.json", "modules/agents/pi/package-lock.json"],
            &[("modules/agents/pi/package.json", r#"{"name": "pi"}"#)],
        );
        assert_eq!(picked(&["modules/agents/pi/package-lock.json"], &mute_module), vec![]);

        // Docs (or anything covered by neither) run only a root Makefile `test:` the base declares.
        let docs = tree(&["docs/guide.md"], &[]);
        assert_eq!(picked(&["docs/guide.md"], &docs), vec![]);
        assert_eq!(
            picked(
                &["docs/guide.md"],
                &tree(
                    &["docs/guide.md", "Makefile"],
                    &[("Makefile", "build:\n\techo\ntest: build\n")]
                )
            ),
            vec![("".into(), "make test".into())]
        );
    }

    /// ANSI is stripped before matching (CSI and OSC, truncated sequences included); cargo's
    /// FAILED lines, vitest/jest `FAIL` chains and `×`/`✕`/`✗` marks are the failing tests,
    /// deduplicated, capped at five plus the count.
    #[test]
    fn failing_test_names_come_from_the_run_output() {
        assert_eq!(
            strip_ansi("\x1b[31mred\x1b[0m \x1b]8;;http://x\x07link\x1b]8;;\x07 plain \x1b[31"),
            "red link plain "
        );
        let output = "\x1b[1mrunning 4 tests\x1b[0m\n\
                      test a::b ... \x1b[31mFAILED\x1b[0m\n\
                      test c::d ... FAILED\n\
                      \n\
                      FAIL  web/src/a.test.ts > suite > name\n\
                      × other > case 12ms\n\
                      ✕ jest style\n\
                      ✗ bun style\n\
                      test a::b ... FAILED\n\
                      test result: FAILED. 1 passed; 5 failed\n";
        assert_eq!(
            failing_tests(output),
            (
                vec![
                    "a::b".to_string(),
                    "c::d".to_string(),
                    "web/src/a.test.ts > suite > name".to_string(),
                    "other > case 12ms".to_string(),
                    "jest style".to_string(),
                ],
                1
            )
        );
        assert!(
            failing_tests("all green\ntest x ... ok\ntest result: ok. 2 passed\n")
                .0
                .is_empty()
        );
    }

    #[test]
    fn a_checks_log_name_is_its_place_and_command() {
        let at = |dir: &str, command: &str| {
            check_slug(&Check {
                dir: dir.into(),
                command: command.into(),
                source: "config",
                needs: None,
                runs_script: false,
            })
        };
        assert_eq!(at("", "cargo test"), "cargo-test");
        // Whatever the command says, the name stays filesystem-safe and short.
        assert_eq!(at("", "script --weird --flags ../../etc"), "script-weird-flags-etc");
    }

    /// A check that fails on the base commit as well predates the change: inconclusive — published,
    /// never a hold. The same failure on a base that passes is the change's own: contradicted.
    #[tokio::test]
    async fn a_failure_the_base_shares_is_inconclusive_not_contradicted() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        worktree_fixture(&app, Some("exit 3"), "did the work", true).await;
        let v = verify(&app, &phased_runner("uncommitted.txt", (3, Some(3)), (3, Some(3)), "")).await;
        assert_eq!(v.verdict, Verdict::Inconclusive, "{v:?}");
        assert_eq!(v.exit_code, Some(3));
        assert!(v.contradictions.is_empty(), "{v:?}");
        assert_eq!(v.inconclusive, vec!["`exit 3` fails on the base commit as well".to_string()]);
        assert_eq!(v.summary, "inconclusive: `exit 3` fails on the base commit as well");
        assert_eq!(crate::events::verdict_step(&v.verdict), crate::events::Autopilot::Publish);

        // A different repository (the cache is per repo): the base passes, the head fails.
        worktree_fixture(&app, Some("exit 3"), "did the work", true).await;
        let v = verify(&app, &phased_runner("uncommitted.txt", (0, Some(0)), (3, Some(3)), "")).await;
        assert_eq!(v.verdict, Verdict::Contradicted, "{v:?}");
        assert_eq!(
            crate::events::verdict_step(&v.verdict),
            crate::events::Autopilot::Hold("the completion claim was contradicted")
        );
        assert_eq!(v.contradictions, vec!["`exit 3` exited 3 in a fresh checkout".to_string()]);
        assert!(v.inconclusive.is_empty(), "{v:?}");
        let _ = std::fs::remove_dir_all(root);
    }

    /// The base answer is paid for once: a second verification of the same branch re-asks only the
    /// head, with a runner that must never see a base checkout — and still answers the same.
    #[tokio::test]
    async fn the_base_answer_is_cached_across_verifications() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        worktree_fixture(&app, Some("exit 3"), "did the work", true).await;
        let first = verify(&app, &phased_runner("uncommitted.txt", (3, Some(3)), (3, Some(3)), "")).await;
        assert_eq!(first.verdict, Verdict::Inconclusive, "{first:?}");
        let head_only: VmRunner = Arc::new(|spec| {
            Box::pin(async move {
                let ws = spec.mounts.iter().find(|m| m.target == "/workspace").unwrap().source.clone();
                assert!(
                    tokio::fs::try_exists(ws.join("uncommitted.txt")).await.unwrap_or(false),
                    "the base answer must come from the cache, not a second boot"
                );
                let report = spec.mounts.iter().find(|m| m.target == "/colonizer-verify").unwrap();
                tokio::fs::write(report.source.join("exit"), "3").await?;
                Ok(3)
            })
        });
        let second = verify(&app, &head_only).await;
        assert_eq!(second.verdict, Verdict::Inconclusive, "{second:?}");
        let _ = std::fs::remove_dir_all(root);
    }

    /// When the base run itself hits infra trouble (here: the base sandbox exits nonzero) the head
    /// failure still contradicts, saying the base could not be checked — never downgraded.
    #[tokio::test]
    async fn a_base_run_that_cannot_be_judged_leaves_the_head_failure_contradicted() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        worktree_fixture(&app, Some("exit 3"), "did the work", true).await;
        let v = verify(&app, &phased_runner("uncommitted.txt", (2, Some(0)), (3, Some(3)), "")).await;
        assert_eq!(v.verdict, Verdict::Contradicted, "{v:?}");
        assert_eq!(
            v.contradictions,
            vec![
                "`exit 3` exited 3 in a fresh checkout (the base commit could not be checked: \
                 the base sandbox itself exited 2)"
                    .to_string()
            ]
        );
        assert!(v.inconclusive.is_empty(), "{v:?}");
        let _ = std::fs::remove_dir_all(root);
    }

    /// A failing run leaves its evidence: the last 200 lines in the session's out directory, the
    /// failing tests named in the contradiction, capped at five plus the count of the rest.
    #[tokio::test]
    async fn a_failing_run_leaves_its_tail_and_the_failing_tests_names() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        worktree_fixture(&app, Some("exit 101"), "did the work", true).await;
        let mut output = String::new();
        for i in 0..206 {
            output.push_str(&format!("\x1b[31mtest verify::case_{i:03} ... \x1b[0mFAILED\n"));
        }
        let runner = phased_runner(
            "uncommitted.txt",
            (0, Some(0)),
            (101, Some(101)),
            Box::leak(output.into_boxed_str()),
        );
        let v = verify(&app, &runner).await;
        assert_eq!(v.verdict, Verdict::Contradicted, "{v:?}");
        let log = app.session_dir("abc").join("out").join("verify-exit-101.log");
        let saved = std::fs::read_to_string(&log).unwrap();
        assert_eq!(
            saved.lines().count(),
            LOG_LINES,
            "the log carries the last 200 lines, not all 206"
        );
        assert!(saved.contains("case_205"), "the tail keeps the last failures");
        assert!(!saved.contains("case_005"), "the head of the run is cut");
        // The names are read off the tail that was kept, not the whole run.
        let names = (6..6 + NAME_CAP)
            .map(|i| format!("verify::case_{i:03}"))
            .collect::<Vec<_>>()
            .join(", ");
        assert_eq!(
            v.contradictions,
            vec![format!(
                "`exit 101` exited 101 in a fresh checkout; failing: {names} and 195 more \
                 (last 200 lines in out/verify-exit-101.log)"
            )]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// The tail of issue #1117's run: rustup could not fetch the pinned toolchain's manifest, and no
    /// test ran.
    const RUSTUP_RESET: &str = "info: syncing channel updates for '1.98.1-x86_64-unknown-linux-gnu'\n\
        error: could not download file from 'https://static.rust-lang.org/dist/channel-rust-1.98.1.toml.sha256' \
        to '/root/.rustup/tmp/abc_file': error during download: connection reset by peer (os error 104)";
    /// cargo could not reach the crates.io index before compiling anything.
    const REGISTRY_DNS: &str = "    Updating crates.io index\n\
        error: failed to get `serde` as a dependency of package `demo v0.1.0 (/workspace)`\n\
        Caused by:\n  download of config.json failed\n\
        Caused by:\n  [6] Could not resolve host: index.crates.io";
    /// A real failure: the harness ran and a test failed — even one whose message names a socket.
    const REAL_FAILURE: &str = "running 3 tests\n\
        test api::serves ... ok\n\
        test api::reconnects ... FAILED\n\
        ---- api::reconnects stdout ----\n\
        thread 'api::reconnects' panicked: connection refused (os error 111)\n\
        test result: FAILED. 2 passed; 1 failed; 0 ignored";

    /// A runner whose head runs answer `answers` in turn (exit code, output), repeating the last;
    /// base runs (no `marker` in the checkout) answer green. Counts the head runs.
    fn sequence_runner(answers: Vec<(i32, &'static str)>) -> (VmRunner, Arc<std::sync::atomic::AtomicUsize>) {
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = count.clone();
        let runner: VmRunner = Arc::new(move |spec| {
            let (seen, answers) = (seen.clone(), answers.clone());
            Box::pin(async move {
                let source = |target: &str| spec.mounts.iter().find(|m| m.target == target).unwrap().source.clone();
                let report = source("/colonizer-verify");
                if !tokio::fs::try_exists(source("/workspace").join("uncommitted.txt"))
                    .await
                    .unwrap_or(false)
                {
                    tokio::fs::write(report.join("exit"), "0").await?;
                    return Ok(0);
                }
                let n = seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let (code, output) = answers[n.min(answers.len() - 1)];
                tokio::fs::write(report.join("output"), output).await?;
                tokio::fs::write(report.join("exit"), code.to_string()).await?;
                Ok(code)
            })
        });
        (runner, count)
    }

    /// Issue #1117: a run that dies on a rustup download before any test is retried, and the retry
    /// that gets through decides — no contradiction, no hold.
    #[tokio::test]
    async fn a_rustup_download_failure_is_retried_not_held() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        worktree_fixture(&app, Some("cargo test"), "did the work", true).await;
        let (runner, runs) = sequence_runner(vec![(1, RUSTUP_RESET), (1, RUSTUP_RESET), (0, "")]);
        let v = verify(&app, &runner).await;
        assert_eq!(v.verdict, Verdict::Confirmed, "{v:?}");
        assert!(v.contradictions.is_empty() && v.network.is_none(), "{v:?}");
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 3, "two retries, then green");
        let _ = std::fs::remove_dir_all(root);
    }

    /// Issue #1117: when every attempt dies on the network, the claim is not contradicted: the
    /// verdict is unverifiable with the cause, which autopilot holds on the card.
    #[tokio::test]
    async fn a_download_failure_that_outlasts_the_retries_reports_that_verification_could_not_run() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        worktree_fixture(&app, Some("cargo test"), "did the work", true).await;
        for (output, cause) in [
            (RUSTUP_RESET, "rustup toolchain download, connection reset"),
            (REGISTRY_DNS, "crates.io registry, DNS lookup failed"),
        ] {
            let (runner, runs) = sequence_runner(vec![(1, output)]);
            let v = verify(&app, &runner).await;
            assert_eq!(v.verdict, Verdict::Unverifiable, "{v:?}");
            assert!(v.contradictions.is_empty(), "{v:?}");
            assert_eq!(v.network.as_deref(), Some(cause));
            assert_eq!(v.summary, format!("verification could not run (network: {cause})"));
            assert_eq!(
                runs.load(std::sync::atomic::Ordering::SeqCst),
                4,
                "the first run and three retries"
            );
            assert_eq!(crate::events::verdict_step(&v.verdict), crate::events::Autopilot::Publish);
            let card = crate::events::verify_network_held_attention(v.network.as_deref().unwrap());
            assert_eq!(card["reason"], "autopilot_held");
            assert_eq!(card["cause"], crate::events::VERIFY_NETWORK_CAUSE);
            assert_eq!(card["detail"], format!("verification could not run (network: {cause})"));
        }
        let _ = std::fs::remove_dir_all(root);
    }

    /// Issue #1117: a real test failure is still held at once — no retry, even when its output
    /// names a refused connection.
    #[tokio::test]
    async fn a_real_test_failure_still_contradicts_without_a_retry() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        worktree_fixture(&app, Some("cargo test"), "did the work", true).await;
        let (runner, runs) = sequence_runner(vec![(101, REAL_FAILURE), (0, "")]);
        let v = verify(&app, &runner).await;
        assert_eq!(v.verdict, Verdict::Contradicted, "{v:?}");
        assert!(v.network.is_none());
        assert!(v.contradictions[0].contains("failing: api::reconnects"), "{v:?}");
        assert_eq!(
            runs.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a real failure is not retried"
        );
        assert_eq!(
            crate::events::verdict_step(&v.verdict),
            crate::events::Autopilot::Hold("the completion claim was contradicted")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// Issue #1117: a pinned toolchain is read from the nearest toolchain file, and only a plain
    /// name reaches the guest, which prefers an installed match over a rustup download.
    #[tokio::test]
    async fn a_pinned_toolchain_is_preferred_as_installed() {
        assert_eq!(
            pinned_channel("[toolchain]\nchannel = \"1.98.1\"\ncomponents = [\"clippy\"]\n").as_deref(),
            Some("1.98.1")
        );
        assert_eq!(pinned_channel("nightly-2026-01-01\n").as_deref(), Some("nightly-2026-01-01"));
        assert_eq!(pinned_channel("[toolchain]\npath = \"/opt/rust\"\n"), None);
        assert_eq!(pinned_channel("1.98.1; rm -rf /\n"), None);
        assert_eq!(pinned_channel("[toolchain]\nchannel = \"$(id)\"\n"), None);
        let root = std::env::temp_dir().join(format!("colonizer-toolchain-{}", short_id()));
        std::fs::create_dir_all(root.join("crates/a")).unwrap();
        assert_eq!(pinned_toolchain(&root, "crates/a").await, None);
        std::fs::write(root.join("rust-toolchain.toml"), "[toolchain]\nchannel = \"1.98.1\"\n").unwrap();
        assert_eq!(pinned_toolchain(&root, "crates/a").await.as_deref(), Some("1.98.1"));
        let _ = std::fs::remove_dir_all(root);
        let script = guest_script("", "cargo test", None, Some("1.98.1"));
        assert!(
            script.starts_with("cd /workspace && if command -v rustup >/dev/null 2>&1; then t=$(rustup toolchain list"),
            "{script}"
        );
        assert!(
            script.contains("awk -v c='1.98.1' '$1 == c || index($1, c \"-\") == 1 { print $1; exit }'"),
            "{script}"
        );
        assert!(
            script.contains("export RUSTUP_TOOLCHAIN=\"$t\"; fi; fi && (cargo test)"),
            "{script}"
        );
        assert!(!guest_script("", "cargo test", None, None).contains("rustup"));
    }
}
