//! The TypeScript any loop's tests. Every source is a fixture written to a temporary directory and
//! every host read comes from the fake below: no test runs node, reads a mirror or reaches the
//! network.

use super::*;
use crate::sessions::tests::colony;
use chrono::TimeZone;

fn utc(d: u32, h: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, d, h, 0, 0).unwrap()
}

fn root(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("colonizer-ts-any-{tag}-{}", short_id()))
}

/// Writes `files` under a fresh directory and returns it.
fn tree(tag: &str, files: &[(&str, String)]) -> PathBuf {
    let dir = root(tag);
    for (path, text) in files {
        let dest = dir.join(path);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(dest, text).unwrap();
    }
    dir
}

/// Every file under `dir`, repository-relative.
fn walk(dir: &FsPath) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p.strip_prefix(dir).unwrap().to_string_lossy().replace('\\', "/"));
            }
        }
    }
    out.sort();
    out
}

/// Every form of explicit `any`, and every place one must not be counted.
const FORMS: &str = r#"// a comment: x: any, y as any, <any>z, Array<any>
/* block: Record<string, any>, any[] */
import { any as anyAlias, other } from "./x";
export * as ns from "./y";
const s = "x: any as any <any>";
const q = 'Array<any>';
const t = `template : any ${value as any} tail any[] ${`nested: any`}`;
const re = /: any|as any/g;
export function f(a: any, b: any[], c: Array<any>, d: Record<string, any>): void {}
export class Box<T = any> {
  value = <any>null;
  items: Map<string, any> = new Map();
}
const n = (window as any).x;
type Loose = any;
type U = string | any;
const obj = { any: 1 };
obj.any;
let anyThing = 1;
const y = x as Foo;
const z = [1, 2] as const;
// @ts-ignore
/* eslint-disable no-console */
"#;

// --- the counter --------------------------------------------------------------------------------

#[test]
fn the_counter_finds_each_form_and_ignores_comments_strings_and_names() {
    let scan = scan_source(FORMS);
    let forms: Vec<Form> = scan.explicit.iter().map(|(_, _, f)| *f).collect();
    let count = |f: Form| forms.iter().filter(|x| **x == f).count();
    assert_eq!(
        count(Form::As),
        2,
        "`${{value as any}}` and `(window as any)`: {:?}",
        scan.explicit
    );
    assert_eq!(count(Form::Annotation), 1, "{:?}", scan.explicit);
    assert_eq!(count(Form::Array), 1);
    assert_eq!(count(Form::ArrayGeneric), 1);
    assert_eq!(count(Form::Record), 1);
    assert_eq!(count(Form::GenericDefault), 1);
    assert_eq!(count(Form::Angle), 1);
    assert_eq!(count(Form::TypeArgument), 1);
    assert_eq!(count(Form::Other), 2, "`type Loose = any` and `string | any`");
    assert_eq!(
        scan.explicit.len(),
        11,
        "nothing from comments, strings, templates, regexes or names: {:?}",
        scan.explicit
    );
    // Positions are 1-based lines and columns.
    assert!(scan.explicit.contains(&(9, 22, Form::Annotation)), "{:?}", scan.explicit);
    assert!(
        scan.explicit.contains(&(7, 38, Form::As)),
        "inside `${{…}}`: {:?}",
        scan.explicit
    );
    // `x as Foo` is a cast; `as const`, `as any` and the import/export renames are not.
    assert_eq!(scan.as_casts, 1);
    assert_eq!(scan.suppressions, 2);
}

#[test]
fn the_counter_reads_nested_generics_and_tuples_and_skips_values() {
    let src = "let a: Promise<Array<any>>;\nlet b: [string, any];\nlet c = [x, any];\nf(any);\nlet d = cond ? (v as any) : w;\nfunction g<T extends any>() {}\nconst e: Array<any>[] = [];\n";
    let scan = scan_source(src);
    let forms: Vec<Form> = scan.explicit.iter().map(|(_, _, f)| *f).collect();
    assert_eq!(
        forms,
        vec![Form::ArrayGeneric, Form::Other, Form::As, Form::Other, Form::ArrayGeneric],
        "{:?}",
        scan.explicit
    );
}

#[test]
fn files_and_modules_are_counted_per_directory_the_most_first() {
    let dir = tree(
        "measure",
        &[
            ("tsconfig.json", "{}".into()),
            (
                "src/api/client.ts",
                "export const a: any = 1;\nexport const b = x as any;\n".into(),
            ),
            ("src/api/types.d.ts", "declare const c: any[];\n".into()),
            ("src/ui/View.tsx", "export const v = (p: any) => <div>any</div>;\n".into()),
            ("index.ts", "// nothing here\n".into()),
            ("node_modules/lib/index.ts", "export const skip: any = 1;\n".into()),
            ("dist/out.ts", "export const skip: any = 1;\n".into()),
            ("src/api/readme.md", "x: any".into()),
        ],
    );
    let files = walk(&dir);
    let m = measure(&dir, &files, None, "token scan: test".into());
    assert_eq!(m.method, Method::TokenScan);
    assert_eq!(m.total, 4, "{:#?}", m.files);
    assert_eq!(m.ts_files, 4, "node_modules and dist are not counted");
    assert_eq!(
        m.modules,
        vec![
            ModuleCount {
                module: "src/api".into(),
                explicit: 3,
                files: 2
            },
            ModuleCount {
                module: "src/ui".into(),
                explicit: 1,
                files: 1
            },
        ]
    );
    assert_eq!(m.files[0].path, "src/api/client.ts");
    assert_eq!(m.forms.get("annotation"), Some(&2));
    assert_eq!(m.occurrences[0].text, "export const a: any = 1;");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_repositorys_typescript_answer_is_used_when_it_ran() {
    let answer = r#"{"version":"5.6.3","occurrences":[
        {"file":"src/a.ts","line":1,"col":17,"form":"annotation"},
        {"file":"src/a.ts","line":2,"col":5,"form":"record"}],
        "implicit":{"src/a.ts":3},"errors":[]}"#;
    let tsc = parse_tsc_output(answer).unwrap();
    assert_eq!(tsc.version, "5.6.3");
    assert_eq!(tsc.occurrences[1].3, Form::Record);
    let dir = tree(
        "tsc",
        &[
            ("tsconfig.json", "{}".into()),
            ("src/a.ts", "export const a: any = 1;\nlet r;\n".into()),
        ],
    );
    let m = measure(&dir, &walk(&dir), Some(&tsc), "tsc".into());
    assert_eq!(m.method, Method::Typescript);
    assert_eq!(m.ts_version.as_deref(), Some("5.6.3"));
    assert_eq!(m.total, 2, "the compiler's count, not the token scan's");
    assert_eq!(m.implicit, Some(3));
    assert_eq!(m.files[0].implicit, Some(3));
    assert!(parse_tsc_output("Error: Cannot find module 'typescript'").is_err());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_package_manager_comes_from_the_root_lockfile() {
    let f = |names: &[&str]| names.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(package_manager(&f(&["package.json", "package-lock.json"])), Ok("npm"));
    assert_eq!(package_manager(&f(&["pnpm-lock.yaml"])), Ok("pnpm"));
    assert_eq!(package_manager(&f(&["yarn.lock"])), Ok("yarn"));
    assert_eq!(package_manager(&f(&["yarn.lock", ".yarnrc.yml"])), Ok("yarn-berry"));
    assert!(package_manager(&f(&["bun.lock"])).unwrap_err().contains("offline"));
    assert!(package_manager(&f(&["web/package-lock.json"])).is_err(), "only the root's");
}

#[test]
fn the_repositorys_typescript_is_found_and_a_native_one_is_named() {
    let dir = tree(
        "find-ts",
        &[
            ("web/tsconfig.json", "{}".into()),
            ("web/node_modules/typescript/lib/typescript.js", "module.exports = {};".into()),
        ],
    );
    let files = vec!["web/tsconfig.json".to_string()];
    let found = find_typescript(&dir, &files).unwrap().unwrap();
    assert!(found.ends_with("web/node_modules/typescript/lib/typescript.js"));
    assert!(find_typescript(&dir, &[]).is_none(), "nothing at the root");
    let native = tree(
        "find-ts7",
        &[(
            "node_modules/typescript/package.json",
            r#"{"name":"typescript","version":"7.0.2"}"#.into(),
        )],
    );
    let why = find_typescript(&native, &[]).unwrap().unwrap_err();
    assert!(why.contains("TypeScript 7.0.2 has no JavaScript compiler API"), "{why}");
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(native);
}

// --- the batch ----------------------------------------------------------------------------------

/// `src/big` holds 25 explicit any over two files, `src/small` 3, the root 1.
fn busy_repo(tag: &str) -> PathBuf {
    let many = |n: usize| (0..n).map(|i| format!("export const v{i}: any = {i};\n")).collect::<String>();
    tree(
        tag,
        &[
            ("tsconfig.json", "{}".into()),
            ("src/big/b.ts", many(10)),
            ("src/big/a.ts", many(15)),
            ("src/small/s.ts", many(3)),
            ("root.ts", many(1)),
        ],
    )
}

#[test]
fn the_module_with_the_most_any_is_picked_and_capped() {
    let dir = busy_repo("select");
    let m = measure(&dir, &walk(&dir), None, "token scan".into());
    assert_eq!(m.total, 29);
    let (t, skipped) = select("acme/app", "abc1234", &m, 20, &[]);
    let t = t.unwrap();
    assert!(skipped.is_empty());
    assert_eq!((t.module.as_str(), t.module_total), ("src/big", 25));
    assert_eq!(t.occurrences.len(), 20, "capped at 20");
    assert_eq!(
        (t.occurrences[0].file.as_str(), t.occurrences[0].line),
        ("src/big/a.ts", 1),
        "file then line order"
    );
    assert_eq!(t.occurrences[19].file, "src/big/b.ts");
    assert_eq!(t.origin(), "ts-any:src/big");
    assert_eq!(t.title(), "TypeScript: remove any in src/big (20 of 25)");
    assert_eq!(t.before.module_explicit, 25);
    assert_eq!(t.before.total, 29);
    let (t, _) = select("acme/app", "abc1234", &m, 5, &[]);
    assert_eq!(t.unwrap().occurrences.len(), 5, "the cap is a setting");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_brief_lists_the_occurrences_and_the_rules() {
    let dir = busy_repo("brief");
    let m = measure(&dir, &walk(&dir), None, "token scan".into());
    let (t, _) = select("acme/app", "abc1234def", &m, 20, &[]);
    let b = brief(&t.unwrap());
    for needle in [
        "src/big/a.ts:1:",
        "`export const v0: any = 0;`",
        "abc1234",
        "25 explicit `any`",
        "real type",
        "`unknown` plus narrowing",
        "generic",
        "NEVER",
        "`as` casts",
        "@ts-ignore",
        "@ts-expect-error",
        "eslint-disable",
        "no new `any`",
        "runtime behaviour",
        "tsc --noEmit",
        "tsc -b",
        "tests",
        "Keep the diff small",
        "Do not add any Claude/AI attribution",
    ] {
        assert!(b.contains(needle), "the brief lacks {needle:?}:\n{b}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

fn target_colony(id: &str, repo: &str, origin: &str, status: SessionStatus) -> Session {
    let mut s = colony("acme", status);
    s.id = id.into();
    s.repo = repo.into();
    s.origin = Some(origin.into());
    s
}

#[test]
fn an_open_pull_request_or_live_colony_on_the_module_skips_it() {
    let dir = busy_repo("dup");
    let m = measure(&dir, &walk(&dir), None, "token scan".into());
    for status in [SessionStatus::PrOpened, SessionStatus::Running, SessionStatus::Queued] {
        let sessions = vec![target_colony("c1", "acme/app", "ts-any:src/big", status)];
        let (t, skipped) = select("acme/app", "abc", &m, 20, &sessions);
        assert_eq!(t.unwrap().module, "src/small", "the next module goes instead ({status:?})");
        assert_eq!(skipped.len(), 1);
        assert!(
            skipped[0].reason.contains("colony c1 already targets this module"),
            "{skipped:?}"
        );
    }
    // A hand-started colony with the batch title counts too.
    let mut hand = colony("acme", SessionStatus::PrOpened);
    hand.repo = "acme/app".into();
    hand.issue_title = "TypeScript: remove any in src/big".into();
    assert_eq!(duplicate_of("acme/app", "src/big", &[hand.clone()]), Some(hand.id.clone()));
    // Merged, closed, another repository or another module do not block.
    for s in [
        target_colony("c1", "acme/app", "ts-any:src/big", SessionStatus::Merged),
        target_colony("c1", "acme/app", "ts-any:src/big", SessionStatus::Failed),
        target_colony("c1", "acme/web", "ts-any:src/big", SessionStatus::Running),
        target_colony("c1", "acme/app", "ts-any:src/big/deeper", SessionStatus::Running),
    ] {
        assert_eq!(duplicate_of("acme/app", "src/big", &[s]), None);
    }
    // Every module taken: nothing to dispatch.
    let all: Vec<Session> = ["src/big", "src/small", "."]
        .iter()
        .enumerate()
        .map(|(i, m)| target_colony(&format!("c{i}"), "acme/app", &format!("ts-any:{m}"), SessionStatus::Running))
        .collect();
    let (t, skipped) = select("acme/app", "abc", &m, 20, &all);
    assert!(t.is_none());
    assert_eq!(skipped.len(), 3);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_cooldown_the_run_cap_and_the_kill_switch_hold_dispatches_back() {
    let dir = busy_repo("plan");
    let m = measure(&dir, &walk(&dir), None, "token scan".into());
    let t = |repo: &str| select(repo, "abc", &m, 20, &[]).0.unwrap();
    let targets = vec![t("acme/a"), t("acme/b"), t("acme/c"), t("acme/d")];
    let now = utc(29, 9);
    let (go, skipped) = plan_dispatch(targets.clone(), &Settings::default(), &BTreeMap::new(), now, false);
    assert_eq!(go.len(), 3, "three per run by default");
    assert_eq!(skipped[0].reason, "at most 3 dispatches per run");
    let cooldowns = BTreeMap::from([("acme/a".to_string(), now - ChronoDuration::hours(2))]);
    let (go, skipped) = plan_dispatch(targets.clone(), &Settings::default(), &cooldowns, now, false);
    assert!(go.iter().all(|t| t.repo != "acme/a"));
    assert!(
        skipped[0].reason.contains("cooling down until 2026-09-30 03:00 UTC"),
        "{skipped:?}"
    );
    let (go, _) = plan_dispatch(
        targets.clone(),
        &Settings::default(),
        &cooldowns,
        now + ChronoDuration::hours(18),
        false,
    );
    assert!(
        go.iter().any(|t| t.repo == "acme/a"),
        "after the 20-hour cooldown it goes again"
    );
    let (go, skipped) = plan_dispatch(targets, &Settings::default(), &BTreeMap::new(), now, true);
    assert!(go.is_empty());
    assert!(skipped.iter().all(|s| s.reason.contains("COLONIZER_NO_EXTERNAL_EFFECTS")));
    let _ = std::fs::remove_dir_all(dir);
}

// --- the post-check -----------------------------------------------------------------------------

fn record_for(before: &Measurement) -> TargetRecord {
    TargetRecord {
        session: "c1".into(),
        repo: "acme/app".into(),
        module: "src/big".into(),
        sha: "abc".into(),
        at: utc(28, 9),
        occurrences: vec!["src/big/a.ts:1".into()],
        before: Baseline::of(before, "src/big"),
        check: None,
    }
}

#[test]
fn the_post_check_flags_added_suppressions_casts_and_a_count_that_did_not_drop() {
    let dir = busy_repo("post-before");
    let before = measure(&dir, &walk(&dir), None, "token scan".into());
    let rec = record_for(&before);

    // An honest batch: five any typed properly, nothing else touched.
    let honest = busy_repo("post-honest");
    let a = (0..15)
        .map(|i| {
            if i < 5 {
                format!("export const v{i}: number = {i};\n")
            } else {
                format!("export const v{i}: any = {i};\n")
            }
        })
        .collect::<String>();
    std::fs::write(honest.join("src/big/a.ts"), a).unwrap();
    let after = measure(&honest, &walk(&honest), None, "token scan".into());
    let check = post_check(&rec, &after, "def", utc(29, 9));
    assert!(!check.flagged, "{check:?}");
    assert_eq!((check.module_before, check.module_after), (25, Some(20)));
    assert!(check.summary().ends_with("ok"));

    // A cheat: the any are gone, silenced with casts and ts-ignore, and one moved elsewhere.
    let cheat = busy_repo("post-cheat");
    let a = (0..15)
        .map(|i| format!("// @ts-ignore\nexport const v{i} = load() as Thing;\n"))
        .collect::<String>();
    std::fs::write(cheat.join("src/big/a.ts"), a).unwrap();
    std::fs::write(cheat.join("src/small/extra.ts"), "export const moved: any = 1;\n").unwrap();
    let after = measure(&cheat, &walk(&cheat), None, "token scan".into());
    let check = post_check(&rec, &after, "def", utc(29, 9));
    assert!(check.flagged);
    assert_eq!(check.suppressions_added, 15);
    assert_eq!(check.as_casts_added, 15);
    assert_eq!(check.any_added_elsewhere, 1);
    assert!(
        check.problems.iter().any(|p| p.contains("suppression comments added")),
        "{check:?}"
    );
    assert!(check.problems.iter().any(|p| p.contains("`as` casts added")));
    assert!(check.problems.iter().any(|p| p.contains("new any outside src/big")));

    // Nothing changed: the count did not drop.
    let check = post_check(&rec, &before, "abc", utc(29, 9));
    assert!(check.flagged);
    assert!(check.problems[0].contains("did not drop (25 → 25)"), "{check:?}");
    for d in [dir, honest, cheat] {
        let _ = std::fs::remove_dir_all(d);
    }
}

// --- settings and the API -----------------------------------------------------------------------

#[test]
fn it_is_off_with_an_empty_allowlist_by_default_and_runs_at_most_hourly() {
    let d = Settings::default();
    assert!(!d.enabled && d.allow.is_empty() && !d.active());
    assert_eq!(d.cadence, Cadence::Daily { hour: 7, minute: 43 });
    assert_eq!((d.batch_cap, d.max_per_run, d.cooldown_hours), (20, 3, 20));
    assert!(!d.implicit && d.offline_install && d.autopilot);
    let with = |cadence: Cadence| {
        Settings {
            cadence,
            ..Settings::default()
        }
        .validated()
    };
    assert!(with(Cadence::Interval { minutes: 60 }).is_ok());
    assert!(
        with(Cadence::Interval { minutes: 30 })
            .unwrap_err()
            .contains("at most hourly")
    );
    assert!(with(Cadence::SelfPaced {}).is_err());
    let allow = Settings {
        allow: vec![
            " acme ".into(),
            "globex/api".into(),
            "ACME".into(),
            "".into(),
            "initech/*".into(),
        ],
        ..Settings::default()
    }
    .validated()
    .unwrap();
    assert_eq!(
        allow.allow,
        vec!["acme".to_string(), "globex/api".to_string(), "initech".to_string()]
    );
    assert!(allow.covers("acme/anything") && allow.covers("globex/api") && !allow.covers("globex/web"));
    for bad in [
        Settings {
            allow: vec!["../etc".into()],
            ..Settings::default()
        },
        Settings {
            batch_cap: 0,
            ..Settings::default()
        },
        Settings {
            max_per_run: 11,
            ..Settings::default()
        },
    ] {
        assert!(bad.validated().is_err());
    }
}

#[tokio::test]
async fn the_api_enables_and_disables_the_loop_and_saves_the_allowlist() {
    let root = root("api");
    std::fs::create_dir_all(root.join("config")).unwrap();
    let app = crate::tests::test_app(&root);
    let Json(v) = get(State(app.clone())).await;
    assert_eq!(v["name"], NAME);
    assert_eq!(v["settings"]["enabled"], false);
    assert_eq!(v["settings"]["allow"], json!([]));
    assert!(v["next_run_at"].is_null());

    let on = Settings {
        enabled: true,
        allow: vec!["acme/app".into(), "globex".into()],
        cadence: Cadence::Interval { minutes: 60 },
        ..Settings::default()
    };
    let Json(v) = put(State(app.clone()), Json(on.clone())).await.unwrap();
    assert_eq!(v["settings"]["enabled"], true);
    assert_eq!(v["settings"]["allow"], json!(["acme/app", "globex"]));
    assert!(v["next_run_at"].is_string(), "an enabled loop with an allowlist is booked");
    // Saved, and read back after a restart.
    let again = Store::new(&app.cfg.config_dir);
    assert_eq!(again.snapshot().await.settings, on);

    // Enabled with an empty allowlist still runs nothing.
    let empty = Settings {
        enabled: true,
        ..Settings::default()
    };
    let Json(v) = put(State(app.clone()), Json(empty)).await.unwrap();
    assert!(v["next_run_at"].is_null());
    let off = Settings { enabled: false, ..on };
    let Json(v) = put(State(app.clone()), Json(off)).await.unwrap();
    assert_eq!(v["settings"]["enabled"], false);
    assert!(v["next_run_at"].is_null());

    let bad = Settings {
        cadence: Cadence::Interval { minutes: 15 },
        ..Settings::default()
    };
    let err = put(State(app.clone()), Json(bad)).await.unwrap_err();
    assert_eq!(err.status(), StatusCode::BAD_REQUEST);
    let bad = Settings {
        allow: vec!["not a repo!".into()],
        ..Settings::default()
    };
    assert_eq!(
        put(State(app.clone()), Json(bad)).await.unwrap_err().status(),
        StatusCode::BAD_REQUEST
    );
    // A real run is only for the allowlist; a dry run may look at anything.
    let err = run_now(
        State(app.clone()),
        Some(Json(RunRequest {
            dry_run: false,
            repo: Some("initech/web".into()),
        })),
    )
    .await
    .unwrap_err();
    assert!(
        err.message().contains("not on the TypeScript any loop's allowlist"),
        "{}",
        err.message()
    );
    let _ = std::fs::remove_dir_all(root);
}

// --- whole runs, against a fake host ------------------------------------------------------------

struct Fake {
    /// `(repo, rev)` → a fixture tree; `None` is the default branch.
    trees: BTreeMap<(String, Option<String>), PathBuf>,
    orgs: BTreeMap<String, Vec<String>>,
    checkouts: std::sync::Mutex<Vec<(String, Option<String>)>>,
}

impl Host for Fake {
    async fn org_repos(&self, org: &str) -> Result<Vec<String>> {
        self.orgs.get(org).cloned().context("unknown org")
    }

    async fn checkout(&self, repo: &str, rev: Option<&str>) -> Result<Checkout> {
        self.checkouts
            .lock()
            .unwrap()
            .push((repo.to_string(), rev.map(str::to_string)));
        let dir = self
            .trees
            .get(&(repo.to_string(), rev.map(str::to_string)))
            .context("no such ref")?;
        Ok(Checkout {
            sha: format!("sha-{}", rev.unwrap_or("main")),
            dir: dir.clone(),
            files: walk(dir),
            owned: false,
        })
    }

    async fn typescript(&self, _: &Checkout, _: &Settings) -> std::result::Result<TscOutput, String> {
        Err("the fake host has no node".to_string())
    }
}

fn fake(tag: &str) -> Fake {
    Fake {
        trees: BTreeMap::from([(("acme/app".to_string(), None), busy_repo(tag))]),
        orgs: BTreeMap::from([("acme".to_string(), vec!["acme/app".to_string()])]),
        checkouts: Default::default(),
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        for dir in self.trees.values() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

async fn opt_in(app: &Shared, allow: &str) {
    let settings = Settings {
        enabled: true,
        allow: vec![allow.into()],
        ..Settings::default()
    };
    let _ = put(State(app.clone()), Json(settings)).await.unwrap();
}

fn app_at(root: &FsPath) -> Shared {
    std::fs::create_dir_all(root.join("config")).unwrap();
    crate::sessions::tests::app_that_can_create(root)
}

#[tokio::test]
async fn a_run_reports_dispatches_one_batch_and_skips_the_module_next_time() {
    let root = root("run");
    let app = app_at(&root);
    opt_in(&app, "acme").await;
    let host = fake("run-tree");
    let report = run_once(&app, &host, &RunRequest::default(), "manual", utc(29, 9)).await;
    assert_eq!(report.repos.len(), 1, "the org expanded to its repository");
    let r = &report.repos[0];
    assert!(r.typescript);
    assert_eq!(r.method, Some(Method::TokenScan));
    assert!(
        r.method_note
            .as_deref()
            .unwrap()
            .contains("token scan: the fake host has no node")
    );
    assert_eq!((r.total, report.total), (29, 29));
    assert_eq!(r.modules[0].module, "src/big");
    assert_eq!(report.dispatched.len(), 1, "one colony per repository per run: {report:#?}");
    let d = &report.dispatched[0];
    assert_eq!((d.module.as_str(), d.occurrences, d.module_total), ("src/big", 20, 25));
    let id = d.session.clone().expect("a colony was started");
    let s = app.session(&id).await.unwrap();
    assert_eq!(s.origin.as_deref(), Some("ts-any:src/big"));
    assert!(s.instructions.contains("src/big/a.ts:1:") && s.instructions.contains("NEVER"));
    assert!(!s.instructions.contains("src/small"), "one module per batch");
    let st = app.ts_any.snapshot().await;
    assert_eq!(st.history.len(), 1);
    assert_eq!(st.history[0].total, 29);
    assert_eq!(st.targets.len(), 1);
    assert_eq!(st.targets[0].before.module_explicit, 25);
    assert!(st.cooldowns.contains_key("acme/app"));
    assert_eq!(st.trend["acme/app"].len(), 1);
    let log = std::fs::read_to_string(app.cfg.data_dir.join("activity.jsonl")).unwrap_or_default();
    assert!(log.contains("\"loop.ts_any\"") && log.contains(NAME), "{log}");

    // The next day, past the cooldown: the colony on src/big is still live, so src/small goes.
    let report = run_once(&app, &host, &RunRequest::default(), "schedule", utc(30, 9)).await;
    assert!(
        report
            .skipped
            .iter()
            .any(|k| k.module.as_deref() == Some("src/big") && k.reason.contains(&format!("colony {id} already targets"))),
        "{:?}",
        report.skipped
    );
    assert_eq!(report.dispatched.len(), 1);
    assert_eq!(report.dispatched[0].module, "src/small");
    assert_eq!(report.repos[0].previous, vec![29], "the trend against the last run");
    assert!(report.summary().contains("(unchanged)"), "{}", report.summary());
    assert_eq!(app.ts_any.snapshot().await.history.len(), 2);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn the_kill_switch_reports_but_starts_nothing() {
    let root = root("kill");
    let app = app_at(&root);
    opt_in(&app, "acme/app").await;
    let _blocked = crate::authority::test_block_external_writes();
    let report = run_once(&app, &fake("kill-tree"), &RunRequest::default(), "schedule", utc(29, 9)).await;
    assert!(report.blocked);
    assert!(report.dispatched.is_empty());
    assert_eq!(report.total, 29, "the counts are still reported");
    assert!(
        report
            .skipped
            .iter()
            .any(|k| k.module.as_deref() == Some("src/big") && k.reason.contains("COLONIZER_NO_EXTERNAL_EFFECTS"))
    );
    assert!(app.sessions.read().await.is_empty(), "no colony");
    let st = app.ts_any.snapshot().await;
    assert!(st.last_report.is_some(), "the report is kept");
    assert!(st.cooldowns.is_empty() && st.targets.is_empty());
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_dry_run_lists_the_counts_and_what_it_would_dispatch_and_writes_nothing() {
    let root = root("dry");
    let app = app_at(&root);
    // Not even opted in: a dry run may look at any one repository.
    let req = RunRequest {
        dry_run: true,
        repo: Some("acme/app".into()),
    };
    let report = run_once(&app, &fake("dry-tree"), &req, "manual", utc(29, 9)).await;
    assert!(report.dry_run);
    assert_eq!(report.total, 29);
    assert_eq!(report.dispatched.len(), 1);
    assert!(report.dispatched[0].session.is_none(), "would dispatch, did not");
    assert!(report.summary().contains("would dispatch 1"), "{}", report.summary());
    assert!(app.sessions.read().await.is_empty());
    let st = app.ts_any.snapshot().await;
    assert!(st.last_report.is_none() && st.history.is_empty() && st.trend.is_empty());
    assert!(!app.cfg.config_dir.join(FILE).exists(), "nothing saved");
    assert!(!app.cfg.data_dir.join("activity.jsonl").exists(), "nothing logged");
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_published_batch_is_recounted_and_a_cheat_raises_attention() {
    let root = root("post");
    let app = app_at(&root);
    opt_in(&app, "acme/app").await;
    let mut host = fake("post-tree");
    let report = run_once(&app, &host, &RunRequest::default(), "manual", utc(28, 9)).await;
    let id = report.dispatched[0].session.clone().unwrap();

    // The colony's pull request is published, with the any silenced rather than typed.
    {
        let mut sessions = app.sessions.write().await;
        let s = sessions.iter_mut().find(|s| s.id == id).unwrap();
        s.status = SessionStatus::PrOpened;
        s.branch = "colonizer/ts-any".into();
        s.pr_url = Some("https://github.com/acme/app/pull/7".into());
    }
    let cheat = busy_repo("post-cheat-tree");
    let a = (0..15)
        .map(|i| format!("// @ts-expect-error\nexport const v{i} = load() as Thing;\n"))
        .collect::<String>();
    std::fs::write(cheat.join("src/big/a.ts"), a).unwrap();
    host.trees
        .insert(("acme/app".to_string(), Some("colonizer/ts-any".to_string())), cheat);

    let report = run_once(&app, &host, &RunRequest::default(), "schedule", utc(29, 12)).await;
    assert_eq!(report.checks.len(), 1, "{report:#?}");
    assert!(report.checks[0].flagged);
    assert_eq!(report.attention.len(), 1);
    let item = &report.attention[0];
    assert_eq!((item.module.as_str(), item.session.as_str()), ("src/big", id.as_str()));
    assert_eq!(item.pr_url.as_deref(), Some("https://github.com/acme/app/pull/7"));
    assert!(item.reason.contains("suppression comments added"), "{}", item.reason);
    assert!(
        host.checkouts
            .lock()
            .unwrap()
            .iter()
            .any(|(_, rev)| rev.as_deref() == Some("colonizer/ts-any"))
    );
    let st = app.ts_any.snapshot().await;
    assert_eq!(st.attention.len(), 1, "kept as an attention item");
    assert!(st.targets[0].check.as_ref().unwrap().flagged);
    assert_eq!(st.history[0].flagged, 1);
    // Checked once: the next run does not recount it.
    let report = run_once(&app, &host, &RunRequest::default(), "schedule", utc(30, 12)).await;
    assert!(report.checks.is_empty());
    assert_eq!(app.ts_any.snapshot().await.attention.len(), 1);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_repository_without_a_tsconfig_is_not_counted() {
    let dir = tree("plain", &[("src/a.ts", "export const a: any = 1;\n".into())]);
    let host = Fake {
        trees: BTreeMap::from([(("acme/plain".to_string(), None), dir)]),
        orgs: BTreeMap::new(),
        checkouts: Default::default(),
    };
    let (r, m) = check_repo(&host, "acme/plain", &Settings::default()).await;
    assert!(m.is_none() && !r.typescript);
    assert!(r.notes[0].contains("no tsconfig.json"));
    let (r, _) = check_repo(&host, "acme/gone", &Settings::default()).await;
    assert!(r.error.unwrap().contains("could not read the mirror"));
}

#[tokio::test]
async fn the_schedule_fires_only_when_switched_on_opted_in_and_due() {
    let root = root("tick");
    let app = app_at(&root);
    let host = fake("tick-tree");
    let now = Utc::now();
    assert!(tick(&app, &host, now).await.is_none(), "off by default");
    opt_in(&app, "acme/app").await;
    assert!(tick(&app, &host, now).await.is_none(), "not due yet");
    let later = now + ChronoDuration::days(2);
    let report = tick(&app, &host, later).await.expect("due");
    assert_eq!(report.trigger, "schedule");
    assert!(
        app.ts_any.snapshot().await.next_run_at.unwrap() > later,
        "the next slot is booked"
    );
    let _ = std::fs::remove_dir_all(root);
}
