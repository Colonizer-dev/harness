use super::*;
use crate::sessions::tests::colony;
use std::process::Command;

// --- fixtures: real git repositories in temp dirs, never the network ---------------------------

fn root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("colonizer-docs-loop-{name}-{}", short_id()));
    std::fs::create_dir_all(dir.join("config")).unwrap();
    dir
}

fn git_in(dir: &FsPath, args: &[&str], date: Option<&str>) -> String {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(args);
    if let Some(date) = date {
        cmd.env("GIT_AUTHOR_DATE", date).env("GIT_COMMITTER_DATE", date);
    }
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A working repository whose history the test writes commit by commit.
struct Fixture {
    work: PathBuf,
}

impl Fixture {
    fn new(dir: &FsPath) -> Self {
        let work = dir.join("work");
        std::fs::create_dir_all(&work).unwrap();
        git_in(&work, &["-c", "init.defaultBranch=main", "init", "-q"], None);
        Fixture { work }
    }

    /// Writes files (an empty content deletes one) and commits them, dated `days_ago` days back.
    fn commit(&self, subject: &str, files: &[(&str, &str)], days_ago: i64) -> String {
        for (path, content) in files {
            let p = self.work.join(path);
            if content.is_empty() {
                let _ = std::fs::remove_file(&p);
            } else {
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(&p, content).unwrap();
            }
        }
        git_in(&self.work, &["add", "-A"], None);
        let date = (Utc::now() - ChronoDuration::days(days_ago)).to_rfc3339();
        git_in(&self.work, &["commit", "-q", "-m", subject], Some(&date));
        git_in(&self.work, &["rev-parse", "HEAD"], None)
    }

    /// The mothership's bare clone of it, where `app.bare_repo(repo)` looks.
    fn mirror(&self, app: &Shared, repo: &str) -> PathBuf {
        let bare = app.bare_repo(repo);
        let _ = std::fs::remove_dir_all(&bare);
        std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
        git_in(
            &self.work,
            &["clone", "-q", "--bare", self.work.to_str().unwrap(), bare.to_str().unwrap()],
            None,
        );
        bare
    }
}

const LIB_V1: &str = "pub fn greet_user() -> &'static str { \"hi\" }\n";
const LIB_V2: &str = "pub fn greet_user() -> &'static str { \"hello\" }\n";
const README: &str =
    "# App\n\nThe greeting lives in [greet.rs](src/greet.rs): `greet_user` returns it.\n\n## Usage\n\nRun `npm run build`.\n";
const PACKAGE: &str = r#"{"name":"app","scripts":{"build":"tsc"}}"#;

/// A repository with one documented source file, the base commit two days old.
fn documented_repo(dir: &FsPath) -> Fixture {
    let f = Fixture::new(dir);
    f.commit(
        "Initial",
        &[("README.md", README), ("src/greet.rs", LIB_V1), ("package.json", PACKAGE)],
        2,
    );
    f
}

fn settings_with(app: &Shared, allow: &[&str]) {
    let saved = Saved {
        settings: Settings {
            allow: allow.iter().map(|s| s.to_string()).collect(),
            ..Settings::default()
        },
        ..Saved::default()
    };
    std::fs::write(file(app), serde_json::to_vec(&saved).unwrap()).unwrap();
}

fn kinds(findings: &[Finding]) -> Vec<Kind> {
    findings.iter().map(|f| f.kind).collect()
}

// --- detection ----------------------------------------------------------------------------------

#[tokio::test]
async fn a_code_change_without_its_doc_is_found_through_the_docs_map() {
    let dir = root("map");
    let app = crate::tests::test_app(&dir);
    let f = documented_repo(&dir);
    f.commit("Say hello instead (#12)", &[("src/greet.rs", LIB_V2)], 0);
    f.commit(
        "Say hello, documented (#13)",
        &[("src/greet.rs", LIB_V1), ("README.md", &format!("{README}\nIt says hi.\n"))],
        0,
    );
    f.commit("Unmapped code (#14)", &[("src/other.rs", "pub fn x() {}\n")], 0);
    let bare = f.mirror(&app, "acme/app");
    let s = scan(&app, &bare, None, 24, Utc::now()).await.unwrap();
    let undocumented: Vec<&Finding> = s.findings.iter().filter(|x| x.kind == Kind::UndocumentedChange).collect();
    assert_eq!(undocumented.len(), 1, "{:#?}", s.findings);
    let found = undocumented[0];
    assert_eq!(found.change.as_deref(), Some("#12"));
    assert_eq!(found.file.as_deref(), Some("README.md"));
    assert!(
        found
            .message
            .contains("`src/greet.rs` changed in #12, touching `greet_user` that the doc names, without an update to README.md"),
        "{}",
        found.message
    );
    assert!(s.since.is_some(), "the window starts at the commit from before the interval");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_repository_map_file_adds_mappings_and_ignores_paths() {
    let dir = root("mapfile");
    let app = crate::tests::test_app(&dir);
    let f = documented_repo(&dir);
    f.commit(
        "Map the API",
        &[
            (
                DOCS_MAP,
                "ignore = [\"src/greet.rs\"]\n\n[[map]]\ncode = [\"src/api/**\"]\ndocs = [\"docs/api.md\"]\n",
            ),
            ("docs/api.md", "# API\n\nThe `route_one` handler.\n"),
            ("src/api/routes.rs", "pub fn route_one() {}\n"),
        ],
        2,
    );
    f.commit(
        "Change the API (#20)",
        &[("src/api/routes.rs", "pub fn route_one() -> u8 { 1 }\n")],
        0,
    );
    f.commit("Change the ignored file (#21)", &[("src/greet.rs", LIB_V2)], 0);
    let bare = f.mirror(&app, "acme/app");
    let s = scan(&app, &bare, None, 24, Utc::now()).await.unwrap();
    let undocumented: Vec<&Finding> = s.findings.iter().filter(|x| x.kind == Kind::UndocumentedChange).collect();
    assert_eq!(undocumented.len(), 1, "{:#?}", s.findings);
    assert_eq!(undocumented[0].change.as_deref(), Some("#20"));
    assert_eq!(undocumented[0].file.as_deref(), Some("docs/api.md"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn broken_links_and_anchors_are_found_and_good_ones_pass() {
    let readme = "# App\n\n\
        [guide](docs/guide.md#getting-started) and [bad anchor](docs/guide.md#nowhere)\n\
        [gone](docs/missing.md) [web](https://example.com) [self](#app) [dir](src)\n\
        [ref]: docs/guide.md#faq-1\n\n\
        ```\n[in code](docs/nope.md)\n```\n\
        and `[in a span](docs/nope.md)`\n";
    let guide =
        "# Guide\n\n## Getting started\n\n## FAQ\n\n## FAQ\n\n<a id=\"custom\"></a>\n[up](../README.md#app) [custom](#custom)\n";
    let tree = Tree::from_files([("README.md", readme), ("docs/guide.md", guide), ("src/greet.rs", LIB_V1)]);
    let found = link_findings(&tree);
    assert_eq!(kinds(&found), vec![Kind::BrokenAnchor, Kind::BrokenLink], "{found:#?}");
    assert!(found[0].message.contains("#nowhere"), "{}", found[0].message);
    assert_eq!((found[0].file.as_deref(), found[0].line), (Some("README.md"), Some(3)));
    assert!(found[1].message.contains("docs/missing.md"), "{}", found[1].message);
}

#[test]
fn anchors_follow_github_rules() {
    let md = "# Hello, World!\n## `host` and [link](x.md)\n## Repeat\n## Repeat\nSetext\n======\n";
    let a = anchors(md);
    for want in ["hello-world", "host-and-link", "repeat", "repeat-1", "setext"] {
        assert!(a.contains(want), "{want} in {a:?}");
    }
}

#[test]
fn commands_the_repository_no_longer_has_are_found() {
    let readme = "# App\n\n\
        ```sh\n$ npm run build\nnpm run gone\ncd web && npm run test\nnode scripts/check.mjs\nmake lint\ncargo test -p app-core\n```\n\
        Run `colonizer loop create acme/app --name Triage` or `colonizer launch --nope`, or `colonizer vanish`.\n\
        ```rust\nnode scripts/not-a-command-here.mjs\n```\n\
        `./target/release/app` is built by cargo; `npm run <script>` is a placeholder; `scripts/upstream.mjs` is a mention.\n\
        ```sh\n(cd web && npm run test)   # a subshell, with a comment\ncolonizer version   # also --version\n```\n";
    let cli = "#[derive(Subcommand)] enum Command { Version, Launch { #[arg(long)] repo: String }, Loop { #[command(subcommand)] cmd: LoopCmd } }\n\
        enum LoopCmd { Create { #[arg(long)] name: String } }\n";
    let tree = Tree::from_files([
        ("README.md", readme),
        ("package.json", PACKAGE),
        ("web/package.json", r#"{"scripts":{"test":"vitest"}}"#),
        ("Makefile", "build:\n\tcargo build\n"),
        ("crates/core/Cargo.toml", "[package]\nname = \"app-core\"\n"),
        ("src/cli.rs", cli),
    ]);
    let clis = [CliSpec {
        name: "colonizer".into(),
        sources: vec!["src/cli.rs".into()],
    }];
    let found: Vec<String> = command_findings(&tree, &clis).into_iter().map(|f| f.message).collect();
    assert_eq!(found.len(), 5, "{found:#?}");
    for want in ["\"gone\"", "scripts/check.mjs", "\"lint\"", "--nope", "\"vanish\""] {
        assert!(found.iter().any(|m| m.contains(want)), "{want} in {found:#?}");
    }
}

#[tokio::test]
async fn colonizer_routes_drift_against_protocol_md() {
    let dir = root("routes");
    let app = crate::tests::test_app(&dir);
    let f = Fixture::new(&dir);
    let snap_v1 = "/api/loops      GET    unauth=401    token=owner    activity=-\n\
                   /api/old/{id}   DELETE unauth=401    token=owner    activity=-\n";
    let protocol = "# Protocol\n\n| `GET /api/loops` | loops |\n| `DELETE /api/old/{name}` | gone soon |\n";
    f.commit(
        "Initial",
        &[
            (COLONIZER_ROUTES, snap_v1),
            (COLONIZER_PROTOCOL, protocol),
            ("README.md", "# Colonizer\n"),
        ],
        2,
    );
    let snap_v2 = "/api/loops           GET    unauth=401    token=owner    activity=-\n\
                   /api/docs-loop       GET    unauth=401    token=owner    activity=-\n";
    f.commit("Add a route, drop one (#30)", &[(COLONIZER_ROUTES, snap_v2)], 0);
    let bare = f.mirror(&app, "Colonizer-dev/harness");
    let s = scan(&app, &bare, None, 24, Utc::now()).await.unwrap();
    let routes: Vec<&String> = s
        .findings
        .iter()
        .filter(|x| x.kind == Kind::RoutesDrift)
        .map(|x| &x.message)
        .collect();
    assert_eq!(routes.len(), 2, "{:#?}", s.findings);
    assert!(routes.iter().any(|m| m.contains("/api/docs-loop is new")), "{routes:?}");
    assert!(routes.iter().any(|m| m.contains("/api/old/{} was removed")), "{routes:?}");

    // A generic repository has no route table, and nothing is checked.
    assert!(routes_findings(None, snap_v2, COLONIZER_PROTOCOL, protocol).is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

/// The first run after the snapshots were split: the revision the drift is measured from still has
/// the one `crates/colonizer/routes.snap` file, while the head has the `crates/colonizer/routes/`
/// directory. The old side is read from the legacy file and the new side from the directory, so the
/// drift is still found rather than silently missed.
#[tokio::test]
async fn colonizer_routes_drift_reads_a_split_snapshot_against_the_legacy_file() {
    let dir = root("routes-split");
    let app = crate::tests::test_app(&dir);
    let f = Fixture::new(&dir);
    let legacy = "/api/loops      GET    unauth=401    token=owner    activity=-\n\
                  /api/keep       GET    unauth=401    token=owner    activity=-\n\
                  /api/old/{id}   DELETE unauth=401    token=owner    activity=-\n";
    let protocol =
        "# Protocol\n\n| `GET /api/loops` | loops |\n| `GET /api/keep` | kept |\n| `DELETE /api/old/{name}` | gone soon |\n";
    f.commit(
        "Initial",
        &[
            ("crates/colonizer/routes.snap", legacy),
            (COLONIZER_PROTOCOL, protocol),
            ("README.md", "# Colonizer\n"),
        ],
        2,
    );
    // The split lands: one file per module. `loops` gains a route, `old` is dropped, `keep` stays.
    let loops_v2 = "/api/loops      GET    unauth=401    token=owner    activity=-\n\
                    /api/docs-loop  GET    unauth=401    token=owner    activity=-\n";
    f.commit(
        "Split the route snapshots (#30)",
        &[
            ("crates/colonizer/routes.snap", ""),
            ("crates/colonizer/routes/loops.snap", loops_v2),
            (
                "crates/colonizer/routes/keep.snap",
                "/api/keep  GET  unauth=401  token=owner  activity=-\n",
            ),
        ],
        0,
    );
    let bare = f.mirror(&app, "Colonizer-dev/harness");
    let s = scan(&app, &bare, None, 24, Utc::now()).await.unwrap();
    let routes: Vec<&String> = s
        .findings
        .iter()
        .filter(|x| x.kind == Kind::RoutesDrift)
        .map(|x| &x.message)
        .collect();
    assert_eq!(routes.len(), 2, "{:#?}", s.findings);
    assert!(routes.iter().any(|m| m.contains("/api/docs-loop is new")), "{routes:?}");
    assert!(routes.iter().any(|m| m.contains("/api/old/{} was removed")), "{routes:?}");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn colonizer_routes_drift_reads_the_split_protocol_directory() {
    let dir = root("routes-split-protocol");
    let app = crate::tests::test_app(&dir);
    let f = Fixture::new(&dir);
    let snap = "crates/colonizer/routes/loops.snap";
    let snap_v1 = "/api/loops      GET    unauth=401    token=owner    activity=-\n";
    let index = "# Protocol\n\nThe routes live under [the routes area](protocol/routes.md).\n";
    let area = "# Routes\n\n| `GET /api/docs-loop` | the docs loop |\n";
    f.commit(
        "Initial",
        &[
            (snap, snap_v1),
            (COLONIZER_PROTOCOL, index),
            ("docs/protocol/routes.md", area),
            ("README.md", "# Colonizer\n"),
        ],
        2,
    );
    let snap_v2 = "/api/loops           GET    unauth=401    token=owner    activity=-\n\
                   /api/docs-loop       GET    unauth=401    token=owner    activity=-\n\
                   /api/undocumented    GET    unauth=401    token=owner    activity=-\n";
    f.commit("Add two routes (#31)", &[(snap, snap_v2)], 0);
    let bare = f.mirror(&app, "Colonizer-dev/harness");
    let s = scan(&app, &bare, None, 24, Utc::now()).await.unwrap();
    let routes: Vec<&String> = s
        .findings
        .iter()
        .filter(|x| x.kind == Kind::RoutesDrift)
        .map(|x| &x.message)
        .collect();
    // The route named only in `docs/protocol/routes.md` is documented; the one none lists is not.
    assert_eq!(routes.len(), 1, "{:#?}", s.findings);
    assert!(routes[0].contains("/api/undocumented is new"), "{routes:?}");
    assert!(!routes.iter().any(|m| m.contains("/api/docs-loop")), "{routes:?}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn changelog_gaps_are_found_only_where_the_repository_keeps_one() {
    let code = |sha: &str, subject: &str, extra: &[&str]| Commit {
        sha: sha.into(),
        subject: subject.into(),
        files: std::iter::once("src/greet.rs")
            .chain(extra.iter().copied())
            .map(str::to_string)
            .collect(),
        ..Commit::default()
    };
    let commits = vec![
        code("a1", "Feature one (#40)", &["changelog.d/40.added.md"]),
        code("a2", "Feature two (#41)", &[]),
        code("a3", "Feature three (#42)", &[]),
    ];
    let fragments = Tree::from_files([("changelog.d/README.md", "# Fragments\n")]);
    let found = changelog_findings(&commits, &fragments, None);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert!(found[0].advisory, "the repository's own check only warns");
    assert!(
        found[0]
            .message
            .starts_with("2 merged changes touched code and added no changelog.d/ fragment: #41, #42;"),
        "{}",
        found[0].message
    );

    let empty = Tree::from_files([(
        "CHANGELOG.md",
        "# Changelog\n\n## Unreleased\n\n<!-- nothing yet -->\n\n## 1.0.0\n\n- First (#1)\n",
    )]);
    let found = changelog_findings(&commits, &empty, None);
    assert_eq!(found.len(), 1);
    assert!(found[0].message.contains("## Unreleased is empty"), "{}", found[0].message);

    let partial = Tree::from_files([(
        "CHANGELOG.md",
        "# Changelog\n\n## [Unreleased]\n\n- Feature one (#40)\n- Feature two (#41)\n",
    )]);
    let found = changelog_findings(&commits, &partial, None);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].message, "## Unreleased has no entry for #42");

    let none = Tree::from_files([("CHANGELOG.md", "# Changelog\n\n## 1.0.0\n\n- First\n")]);
    assert!(
        changelog_findings(&commits, &none, None).is_empty(),
        "no Unreleased section: not checked"
    );
}

#[test]
fn a_fragment_consumed_by_a_release_is_not_missing() {
    // Newest first: the release folded #40's fragment into CHANGELOG.md and deleted it; #40 added
    // it; #39 predates the convention; a bot bumped a dependency; #43 touched only docs.
    let commit = |sha: &str, subject: &str, files: &[&str], deleted: &[&str]| Commit {
        sha: sha.into(),
        subject: subject.into(),
        files: files.iter().map(|f| f.to_string()).collect(),
        deleted: deleted.iter().map(|f| f.to_string()).collect(),
        ..Commit::default()
    };
    let commits = vec![
        commit(
            "r1",
            "Release v0.2.0 (#44)",
            &["CHANGELOG.md", "changelog.d/40.added.md", "crates/app/Cargo.toml"],
            &["changelog.d/40.added.md"],
        ),
        commit("d1", "Docs only (#43)", &["docs/x.md"], &[]),
        commit(
            "b1",
            "Bump serde from 1.0 to 1.1 (#42)",
            &["crates/app/Cargo.toml", "crates/app/src/lib.rs"],
            &[],
        ),
        commit(
            "f1",
            "Feature (#40)",
            &["crates/app/src/lib.rs", "changelog.d/40.added.md"],
            &[],
        ),
        commit(
            "i1",
            "Adopt changelog fragments (#39)",
            &["changelog.d/README.md", "crates/app/src/lib.rs"],
            &[],
        ),
        commit("o1", "Old change (#38)", &["crates/app/src/lib.rs"], &[]),
    ];
    // The head tree no longer holds 40.added.md: the release consumed it.
    let mjs = "const CODE_PATHS = [/^crates\\//, /^scripts\\/[^/]+\\.(mjs|sh)$/];\n";
    let tree = Tree::from_files([("changelog.d/README.md", "# Fragments\n"), ("scripts/changelog.mjs", mjs)]);
    assert_eq!(
        code_paths(&tree),
        Some(vec!["crates".to_string(), "scripts/*.mjs".into(), "scripts/*.sh".into()])
    );
    assert!(
        changelog_findings(&commits, &tree, Some("i1")).is_empty(),
        "nothing is missing"
    );

    // A code change after the convention with no fragment is: a warning, as the repository's check says.
    let mut more = vec![commit("n1", "Refactor (#45)", &["crates/app/src/lib.rs"], &[])];
    more.extend(commits);
    let found = changelog_findings(&more, &tree, Some("i1"));
    assert_eq!(found.len(), 1);
    assert!(found[0].advisory && found[0].message.contains(": #45;"), "{:?}", found[0]);
    // A path outside the repository's CODE_PATHS needs none.
    let web = vec![commit("w1", "Public asset (#46)", &["web/public/sw.js"], &[])];
    assert!(changelog_findings(&web, &tree, None).is_empty());
}

#[test]
fn a_comment_only_or_formatting_change_is_not_stale() {
    let doc = "# Loops\n\nA colony calls `loop_next` to pace the loop, and `--max-runs` caps it; see `/api/loops/{id}`.\n";
    let names = doc_names(doc);
    for want in ["loop_next", "--max-runs", "/api/loops/{}"] {
        assert!(names.contains(want), "{want} in {names:?}");
    }
    let plain = doc_names(
        "Run `colonizer`, see `crates/app/src/boot.rs`, `install`, `/api/`, `/api/...`, `__Host-`, `OrgSettings` and `COLONIZER_LOOP`.",
    );
    assert_eq!(
        plain,
        ["colonizer_loop".to_string(), "orgsettings".into()].into(),
        "plain words and paths name nothing"
    );
    let tree = Tree::from_files([("docs/loops.md", doc), ("src/loops.rs", "")]);
    let mut map = DocsMap::default();
    map.entries
        .insert("src/loops.rs".into(), ["docs/loops.md".to_string()].into());
    let change = |sha: &str, diff: &str| Commit {
        sha: sha.into(),
        subject: format!("Change {sha} (#1{})", sha.len()),
        files: vec!["src/loops.rs".into()],
        diffs: [("src/loops.rs".to_string(), diff.to_string())].into(),
        ..Commit::default()
    };
    let comment = change("c", "@@ -1 +1 @@\n-// Paces the loop.\n+// Paces the loop; see loop_next.\n");
    let format = change(
        "fo",
        "@@ -1 +1,2 @@\n-fn loop_next(a: u32) {}\n+fn loop_next(\n+    a: u32) {}\n",
    );
    let unrelated = change("unr", "@@ -1 +1 @@\n-let widget = 1;\n+let widget = 2;\n");
    let map_file = MapFile::default();
    for c in [&comment, &format, &unrelated] {
        assert!(
            undocumented_findings(std::slice::from_ref(c), &map, &map_file, &tree).is_empty(),
            "{}",
            c.sha
        );
    }
    let named = change(
        "named",
        "@@ -1 +1 @@\n-pub fn loop_next(delay: u64) {}\n+pub fn loop_next(delay: u64, reason: &str) {}\n",
    );
    let found = undocumented_findings(std::slice::from_ref(&named), &map, &map_file, &tree);
    assert_eq!(found.len(), 1);
    assert!(found[0].message.contains("touching `loop_next`"), "{}", found[0].message);
    let route = change(
        "route",
        "@@ -1 +1 @@\n-.route(\"/api/loops/{id}\", get(x))\n+.route(\"/api/loops/{id}\", put(x))\n",
    );
    assert_eq!(
        undocumented_findings(std::slice::from_ref(&route), &map, &map_file, &tree).len(),
        1
    );
    // Code moved from another file (or within one) changes nothing the doc says.
    let mut moved = named.clone();
    moved.diffs.insert(
        "src/old.rs".into(),
        "@@ -1 +0,0 @@\n-pub fn loop_next(delay: u64, reason: &str) {}\n".into(),
    );
    moved.diffs.insert(
        "src/loops.rs".into(),
        "@@ -0,0 +1 @@\n+pub fn loop_next(delay: u64, reason: &str) {}\n".into(),
    );
    assert!(undocumented_findings(&[moved], &map, &map_file, &tree).is_empty());
    let flag = change(
        "flag",
        "@@ -1 +1 @@\n-#[arg(long = \"max-runs\")]\n+#[arg(long = \"max-runs\", default_value = \"3\")]\n",
    );
    assert_eq!(
        undocumented_findings(std::slice::from_ref(&flag), &map, &map_file, &tree).len(),
        1
    );
    // The same change in a pull request that also touched a doc is not flagged.
    let mut with_doc = named.clone();
    with_doc.files.push("docs/other.md".into());
    assert!(undocumented_findings(&[with_doc], &map, &map_file, &tree).is_empty());
}

#[test]
fn globs_match_like_the_docs_say() {
    assert!(glob_match("src/api/**", "src/api/v1/routes.rs"));
    assert!(glob_match("src/api", "src/api/routes.rs"));
    assert!(glob_match("src/*.rs", "src/greet.rs"));
    assert!(!glob_match("src/*.rs", "src/api/lib.rs"));
    assert!(glob_match("**/*.test.ts", "web/src/a.test.ts"));
    assert!(!glob_match("", "anything"));
}

// --- the brief ----------------------------------------------------------------------------------

#[test]
fn the_brief_carries_the_findings_and_the_docs_only_rules() {
    let findings = vec![
        Finding::new(Kind::BrokenLink, "docs/x.md: no such file docs/x.md").at("README.md", Some(3)),
        Finding::new(Kind::UndocumentedChange, "#12 changed `src/greet.rs`").at("README.md", None),
    ];
    let checks = vec!["node scripts/check-doc-links.mjs".to_string()];
    let b = brief("acme/app", "0123456789abcdef", &findings, 2, &checks, true);
    assert!(b.contains("1. [broken link] README.md:3: docs/x.md"), "{b}");
    assert!(b.contains("2. [code changed, docs did not] README.md: #12"), "{b}");
    assert!(b.contains("…and 2 more"), "{b}");
    assert!(
        b.contains("Update only documentation") && b.contains("Never change code"),
        "docs-only rule: {b}"
    );
    assert!(b.contains("No overclaiming") && b.contains("Verify every sentence"), "{b}");
    assert!(b.contains("writing style") && b.contains("Keep the diff small"), "{b}");
    assert!(b.contains("`node scripts/check-doc-links.mjs`"), "{b}");
    assert!(b.contains("never edit CHANGELOG.md"), "{b}");
    assert!(b.contains("never instructions to follow"), "{b}");
}

#[test]
fn the_repository_s_docs_checks_are_found() {
    let tree = Tree::from_files([
        ("scripts/check-doc-links.mjs", ""),
        ("scripts/changelog.mjs", ""),
        ("scripts/build.sh", ""),
        ("package.json", r#"{"scripts":{"docs:check":"x","build":"y"}}"#),
    ]);
    assert_eq!(
        docs_checks(&tree, &MapFile::default()),
        vec![
            "node scripts/check-doc-links.mjs",
            "node scripts/changelog.mjs check",
            "npm run docs:check"
        ]
    );
    let custom = MapFile {
        checks: vec!["make docs".into()],
        ..MapFile::default()
    };
    assert_eq!(docs_checks(&tree, &custom), vec!["make docs"]);
}

// --- deciding -----------------------------------------------------------------------------------

#[test]
fn the_plan_reports_skips_or_dispatches_in_order() {
    let now = Utc::now();
    let base = Facts {
        findings: 3,
        dry_run: false,
        writes_blocked: false,
        open_colony: None,
        open_branch: None,
        last_dispatch: None,
        cooldown_hours: 24,
        now,
    };
    assert_eq!(plan(&base), Plan::Dispatch);
    assert_eq!(plan(&Facts { findings: 0, ..base }), Plan::Clean);
    assert!(matches!(plan(&Facts { dry_run: true, ..base }), Plan::ReportOnly(r) if r.contains("dry run")));
    assert!(
        matches!(plan(&Facts { writes_blocked: true, ..base }), Plan::ReportOnly(r) if r.contains("COLONIZER_NO_EXTERNAL_EFFECTS"))
    );
    assert!(matches!(plan(&Facts { open_colony: Some("c1"), ..base }), Plan::Skip(r) if r.contains("c1")));
    assert!(matches!(plan(&Facts { open_branch: Some("docs/x"), ..base }), Plan::Skip(r) if r.contains("docs/x")));
    let recent = Some(now - ChronoDuration::hours(2));
    assert!(matches!(plan(&Facts { last_dispatch: recent, ..base }), Plan::Skip(r) if r.contains("cooling down")));
    let old = Some(now - ChronoDuration::hours(25));
    assert_eq!(
        plan(&Facts {
            last_dispatch: old,
            ..base
        }),
        Plan::Dispatch
    );
}

// --- whole runs ---------------------------------------------------------------------------------

/// Two allowlisted repositories, each with several findings.
fn two_drifting_repos(dir: &FsPath, app: &Shared) {
    for repo in ["acme/app", "acme/api"] {
        let f = Fixture::new(&dir.join(repo.replace('/', "-")));
        f.commit(
            "Initial",
            &[("README.md", README), ("src/greet.rs", LIB_V1), ("package.json", PACKAGE)],
            2,
        );
        f.commit(
            "Change and break things (#5)",
            &[
                ("src/greet.rs", LIB_V2),
                (
                    "docs/guide.md",
                    "# Guide\n\n[gone](missing.md) [bad](../README.md#nope)\n\nRun `npm run vanished`.\n",
                ),
            ],
            0,
        );
        f.mirror(app, repo);
    }
}

#[tokio::test]
async fn a_run_dispatches_one_colony_per_repository_then_skips_while_it_is_open() {
    let dir = root("dispatch");
    let app = crate::sessions::tests::app_that_can_create(&dir);
    settings_with(&app, &["acme/app", "acme/api"]);
    two_drifting_repos(&dir, &app);
    // The colonies queue rather than boot. A boot would run in the background while the next run
    // awaits its git reads, and reach the real GitHub for the fixture's made-up repositories: where
    // `gh` is signed in, that answers 404 within a second, the colony fails, and the next run sees
    // no open colony. Queued is still open, which is all this test is about.
    app.drain.enter();

    let report = run_once(&app, "run_now", false, Mirror::AsIs, Duration::ZERO).await.unwrap();
    assert_eq!(report.repos.len(), 2);
    for r in &report.repos {
        assert!(r.findings.len() >= 3, "{r:#?}");
        assert_eq!(r.action, Action::Dispatched, "{r:#?}");
    }
    let docs: Vec<Session> = app
        .sessions
        .read()
        .await
        .iter()
        .filter(|s| s.origin.as_deref() == Some(ORIGIN))
        .cloned()
        .collect();
    assert_eq!(docs.len(), 2, "exactly one colony per repository, whatever the findings");
    let mut repos: Vec<&str> = docs.iter().map(|s| s.repo.as_str()).collect();
    repos.sort();
    assert_eq!(repos, ["acme/api", "acme/app"]);
    assert!(docs.iter().all(|s| s.instructions.contains("Update only documentation")));

    // The history and the activity log both have the run.
    let saved = load(&file(&app));
    assert_eq!(saved.history.len(), 1);
    assert!(saved.next_run_at.is_some());
    let log = std::fs::read_to_string(app.cfg.data_dir.join(crate::activity::FILE)).unwrap();
    assert_eq!(log.matches("\"loop.docs\"").count(), 2, "{log}");

    // The next run finds the colonies still live: nothing more is launched.
    let again = run_once(&app, "run_now", false, Mirror::AsIs, Duration::ZERO).await.unwrap();
    for r in &again.repos {
        assert_eq!(r.action, Action::Skipped, "{r:#?}");
        assert!(r.reason.contains("still open"), "{}", r.reason);
    }
    let count = app
        .sessions
        .read()
        .await
        .iter()
        .filter(|s| s.origin.as_deref() == Some(ORIGIN))
        .count();
    assert_eq!(count, 2);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn an_open_docs_pull_request_skips_the_dispatch() {
    let dir = root("open-pr");
    let app = crate::sessions::tests::app_that_can_create(&dir);
    settings_with(&app, &["acme/app", "acme/api"]);
    two_drifting_repos(&dir, &app);
    // acme/app: a docs colony of this loop has its pull request open.
    let mut open = colony("acme", SessionStatus::PrOpened);
    open.id = "docs1".into();
    open.repo = "acme/app".into();
    open.origin = Some(ORIGIN.into());
    open.pr_url = Some("https://github.com/acme/app/pull/9".into());
    app.sessions.write().await.push(open);
    // acme/api: a docs branch someone pushed by hand, not yet merged.
    let api_work = dir.join("acme-api/work");
    git_in(&api_work, &["checkout", "-q", "-b", "docs/fix-readme"], None);
    std::fs::write(api_work.join("README.md"), format!("{README}\nFixed.\n")).unwrap();
    git_in(&api_work, &["commit", "-q", "-am", "docs: fix the readme"], None);
    git_in(&api_work, &["checkout", "-q", "main"], None);
    let bare = app.bare_repo("acme/api");
    git_in(&api_work, &["push", "-q", bare.to_str().unwrap(), "docs/fix-readme"], None);

    let report = run_once(&app, "schedule", false, Mirror::AsIs, Duration::ZERO).await.unwrap();
    let by_repo = |repo: &str| report.repos.iter().find(|r| r.repo == repo).unwrap().clone();
    let app_report = by_repo("acme/app");
    assert_eq!(app_report.action, Action::Skipped);
    assert!(app_report.reason.contains("docs1"), "{}", app_report.reason);
    let api_report = by_repo("acme/api");
    assert_eq!(api_report.action, Action::Skipped);
    assert!(api_report.reason.contains("docs/fix-readme"), "{}", api_report.reason);
    assert!(
        !app.sessions
            .read()
            .await
            .iter()
            .any(|s| s.origin.as_deref() == Some(ORIGIN) && s.id != "docs1")
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn the_kill_switch_reports_only() {
    let dir = root("kill");
    let app = crate::sessions::tests::app_that_can_create(&dir);
    settings_with(&app, &["acme/app"]);
    two_drifting_repos(&dir, &app);
    let _blocked = authority::test_block_external_writes();
    let report = run_once(&app, "schedule", false, Mirror::AsIs, Duration::ZERO).await.unwrap();
    assert!(report.external_writes_blocked);
    let r = &report.repos[0];
    assert_eq!(r.action, Action::ReportOnly);
    assert!(r.reason.contains("COLONIZER_NO_EXTERNAL_EFFECTS"), "{}", r.reason);
    assert!(!r.findings.is_empty());
    assert!(app.sessions.read().await.is_empty(), "nothing was launched");
    assert_eq!(load(&file(&app)).history.len(), 1, "the report is still kept");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_dry_run_writes_nothing() {
    let dir = root("dry");
    let app = crate::sessions::tests::app_that_can_create(&dir);
    settings_with(&app, &["acme/app"]);
    two_drifting_repos(&dir, &app);
    let before = std::fs::read(file(&app)).unwrap();
    let report = run_once(&app, "dry_run", true, Mirror::AsIs, Duration::ZERO).await.unwrap();
    assert!(report.dry_run);
    assert_eq!(report.repos[0].action, Action::ReportOnly);
    assert!(!report.repos[0].findings.is_empty());
    assert!(app.sessions.read().await.is_empty(), "no colony");
    assert_eq!(std::fs::read(file(&app)).unwrap(), before, "no state, no history");
    assert!(!app.cfg.data_dir.join(crate::activity::FILE).exists(), "no activity line");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_clean_repository_advances_its_window_and_dispatches_nothing() {
    let dir = root("clean");
    let app = crate::sessions::tests::app_that_can_create(&dir);
    settings_with(&app, &["acme/app"]);
    let f = documented_repo(&dir);
    let head = f.commit(
        "Docs and code together (#3)",
        &[("src/greet.rs", LIB_V2), ("README.md", &format!("{README}\nHello.\n"))],
        0,
    );
    f.mirror(&app, "acme/app");
    let report = run_once(&app, "schedule", false, Mirror::AsIs, Duration::ZERO).await.unwrap();
    assert_eq!(report.repos[0].action, Action::Clean, "{:#?}", report.repos[0]);
    assert_eq!(load(&file(&app)).repos["acme/app"].last_sha.as_deref(), Some(head.as_str()));
    let _ = std::fs::remove_dir_all(dir);
}

// --- the API ------------------------------------------------------------------------------------

#[tokio::test]
async fn the_api_enables_and_disables_per_repository_or_org() {
    let dir = root("api");
    let app = crate::tests::test_app(&dir);
    let Json(v) = get(State(app.clone())).await;
    assert_eq!(v["enabled"], false, "off by default");
    assert_eq!(v["settings"]["allow"], json!([]));
    assert_eq!(v["settings"]["interval_hours"], 24, "daily by default");
    assert_eq!(v["next_run_at"], Value::Null);

    let Json(v) = enable(
        State(app.clone()),
        Json(Target {
            target: "acme/app".into(),
        }),
    )
    .await
    .unwrap();
    assert_eq!(v["enabled"], true);
    assert!(v["next_run_at"].is_string(), "a newly enabled loop is scheduled");
    let Json(v) = enable(
        State(app.clone()),
        Json(Target {
            target: "Umbrella".into(),
        }),
    )
    .await
    .unwrap();
    assert_eq!(v["settings"]["allow"], json!(["acme/app", "Umbrella"]));
    let Json(v) = enable(
        State(app.clone()),
        Json(Target {
            target: "ACME/app".into(),
        }),
    )
    .await
    .unwrap();
    assert_eq!(v["settings"]["allow"].as_array().unwrap().len(), 2, "no duplicates");
    let err = enable(
        State(app.clone()),
        Json(Target {
            target: "not a repo".into(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.message(), "\"not a repo\" is not an owner or owner/name");

    let Json(v) = disable(
        State(app.clone()),
        Json(Target {
            target: "acme/app".into(),
        }),
    )
    .await
    .unwrap();
    assert_eq!(v["settings"]["allow"], json!(["Umbrella"]));
    let Json(v) = disable(
        State(app.clone()),
        Json(Target {
            target: "umbrella".into(),
        }),
    )
    .await
    .unwrap();
    assert_eq!(v["enabled"], false);
    assert_eq!(v["next_run_at"], Value::Null, "switched off: nothing scheduled");
    assert!(
        disable(
            State(app.clone()),
            Json(Target {
                target: "acme/app".into()
            })
        )
        .await
        .is_err()
    );

    // The interval goes down to hourly and no further.
    let hourly = Settings {
        allow: vec!["acme".into()],
        interval_hours: 1,
        cooldown_hours: 6,
    };
    let Json(v) = put(State(app.clone()), Json(hourly.clone())).await.unwrap();
    assert_eq!(v["settings"]["interval_hours"], 1);
    let too_fast = Settings {
        interval_hours: 0,
        ..hourly
    };
    assert!(put(State(app.clone()), Json(too_fast)).await.is_err());

    // A run of a switched-off loop is refused.
    let _ = put(State(app.clone()), Json(Settings::default())).await.unwrap();
    let err = run_now(State(app.clone()), None).await.unwrap_err();
    assert!(err.message().contains("is off"), "{}", err.message());
    let log = std::fs::read_to_string(app.cfg.data_dir.join(crate::activity::FILE)).unwrap();
    assert!(
        log.contains("enabled for acme/app") && log.contains("disabled for umbrella"),
        "{log}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn settings_and_history_survive_a_round_trip_and_old_files_read() {
    let saved: Saved = serde_json::from_str("{}").unwrap();
    assert_eq!(saved, Saved::default());
    assert!(!saved.settings.enabled());
    assert_eq!(check_settings(&Settings::default()), Ok(Settings::default()));
    let dupes = Settings {
        allow: vec!["acme".into(), " ACME ".into(), "acme/app".into()],
        ..Settings::default()
    };
    assert_eq!(check_settings(&dupes).unwrap().allow, vec!["acme", "acme/app"]);
}

/// Reads this repository's own clone and prints what the loop would report: a check that the
/// detectors do not flood a well-kept repository with false positives. Run it by hand:
/// `cargo test -p colonizer-harness docs_loop::tests::this_repository -- --ignored --nocapture`.
#[tokio::test]
#[ignore]
async fn this_repository() {
    let dir = root("self");
    let app = crate::tests::test_app(&dir);
    // This scan needs the repository itself, which lives outside the crate. The root comes from
    // COLONIZER_REPO_ROOT, which the workspace .cargo/config.toml sets; the published crate ships
    // no such config, so there the variable is unset and this skips.
    let Some(root) = std::env::var_os("COLONIZER_REPO_ROOT") else {
        eprintln!("skipping the docs-loop self-scan: COLONIZER_REPO_ROOT is unset (run it from the repository)");
        return;
    };
    let repo = FsPath::new(&root);
    let git_dir = git_in(repo, &["rev-parse", "--absolute-git-dir"], None);
    let s = scan(&app, FsPath::new(&git_dir), None, 24 * 7, Utc::now()).await.unwrap();
    let mut by_kind: BTreeMap<Kind, usize> = BTreeMap::new();
    for f in &s.findings {
        *by_kind.entry(f.kind).or_default() += 1;
        if f.kind != Kind::UndocumentedChange {
            println!("{}", f.describe());
        } else {
            println!("{}", f.describe().chars().take(300).collect::<String>());
        }
    }
    println!("{by_kind:?}");
    println!("checks: {:?}", s.checks);
    let _ = std::fs::remove_dir_all(dir);
}
