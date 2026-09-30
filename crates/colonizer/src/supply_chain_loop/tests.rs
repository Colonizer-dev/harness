//! The supply-chain loop's tests. Every scanner answer comes from a fixture and every host read
//! from the fake below: no test runs a scanner, reads a mirror or reaches an advisory database.

use super::*;
use crate::sessions::tests::colony;
use chrono::TimeZone;

const CARGO_AUDIT: &str = include_str!("../../tests/fixtures/supply-chain/cargo-audit.json");
const NPM_AUDIT: &str = include_str!("../../tests/fixtures/supply-chain/npm-audit.json");
const OSV_SCANNER: &str = include_str!("../../tests/fixtures/supply-chain/osv-scanner.json");
const CARGO_DENY: &str = include_str!("../../tests/fixtures/supply-chain/cargo-deny.jsonl");

fn utc(d: u32, h: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, d, h, 0, 0).unwrap()
}

fn find<'a>(list: &'a [Finding], package: &str) -> &'a Finding {
    list.iter()
        .find(|f| f.package == package)
        .unwrap_or_else(|| panic!("no finding for {package}: {list:#?}"))
}

// --- the parsers, against each scanner's own JSON -----------------------------------------------

#[test]
fn cargo_audit_json_reads_vulnerabilities_yanked_and_unmaintained() {
    let found = parse_cargo_audit(CARGO_AUDIT, "Cargo.lock").unwrap();
    assert_eq!(found.len(), 4, "{found:#?}");
    let hyper = find(&found, "hyper");
    assert_eq!(hyper.kind, Kind::Vulnerability);
    assert_eq!(hyper.severity, Severity::Critical, "CVSS 9.8");
    assert_eq!(hyper.version.as_deref(), Some("0.14.28"));
    assert_eq!(hyper.fixed.as_deref(), Some("0.14.32"), "the minimal bump, not 1.4.2");
    assert!(hyper.fix_available && !hyper.major_bump);
    assert_eq!(hyper.id.as_deref(), Some("RUSTSEC-2026-0012"));
    let tls = find(&found, "ancient-tls");
    assert!(!tls.fix_available && tls.fixed.is_none(), "no patched version");
    assert_eq!(tls.severity, Severity::Moderate, "no CVSS reads as moderate");
    assert_eq!(find(&found, "atty").kind, Kind::Unmaintained);
    let yanked = find(&found, "futures-util");
    assert_eq!((yanked.kind, yanked.severity), (Kind::Yanked, Severity::High));
    assert!(yanked.dispatchable());
    assert!(parse_cargo_audit("error: not json", "Cargo.lock").is_err());
}

#[test]
fn npm_audit_json_reads_one_finding_per_advisory_with_its_fix() {
    let found = parse_npm_audit(NPM_AUDIT, "web/package-lock.json").unwrap();
    assert_eq!(found.len(), 3, "make-dir is vulnerable only through semver: {found:#?}");
    let semver = find(&found, "semver");
    assert_eq!(semver.severity, Severity::Moderate, "the advisory's own severity");
    assert!(semver.fix_available && semver.fixed.is_none());
    assert_eq!(semver.id.as_deref(), Some("GHSA-c2qf-rxjj-qqgw"));
    let lodash = find(&found, "lodash.template");
    assert_eq!(lodash.severity, Severity::Critical);
    assert!(!lodash.fix_available);
    let vite = find(&found, "vite");
    assert_eq!(vite.fixed.as_deref(), Some("6.0.9"));
    assert!(vite.major_bump);
    assert_eq!(vite.lockfile, "web/package-lock.json");
    assert!(parse_npm_audit(r#"{"error":{"code":"ENOLOCK","summary":"no lockfile"}}"#, "x").is_err());
}

#[test]
fn osv_scanner_json_folds_alias_groups_into_one_finding() {
    let found = parse_osv_scanner(OSV_SCANNER, "backend/poetry.lock").unwrap();
    assert_eq!(found.len(), 2, "GHSA and PYSEC for the same flaw are one: {found:#?}");
    let jinja = find(&found, "jinja2");
    assert_eq!((jinja.ecosystem.as_str(), jinja.severity), ("pypi", Severity::Moderate));
    assert_eq!(jinja.fixed.as_deref(), Some("3.1.3"));
    assert_eq!(
        jinja.id.as_deref(),
        Some("GHSA-h5c8-rqwp-cp95"),
        "the id with a summary leads"
    );
    let net = find(&found, "golang.org/x/net");
    assert_eq!((net.ecosystem.as_str(), net.severity), ("go", Severity::High));
    assert_eq!(net.fixed.as_deref(), Some("0.23.0"));
    assert!(!net.major_bump, "Go compares the major alone");
}

#[test]
fn cargo_deny_json_lines_read_licence_policy_violations() {
    let found = parse_cargo_deny(CARGO_DENY, "deny.toml").unwrap();
    assert_eq!(found.len(), 3, "warnings and the summary are not findings: {found:#?}");
    let gpl = find(&found, "gpl-thing");
    assert_eq!((gpl.kind, gpl.severity), (Kind::License, Severity::High));
    assert!(gpl.title.contains("GPL-3.0-only"), "{}", gpl.title);
    assert!(!gpl.dispatchable(), "a licence is a person's call, not a bump");
    assert_eq!(find(&found, "mystery").kind, Kind::License);
    assert_eq!(find(&found, "hyper").kind, Kind::Vulnerability);
    assert!(parse_cargo_deny("error: failed to fetch", "deny.toml").is_err());
    assert!(parse_cargo_deny("", "deny.toml").unwrap().is_empty());
}

#[test]
fn the_builtin_lookup_and_the_outdated_view_become_findings() {
    let risks = json!({"risks": [
        {"severity": "high", "kind": "vulnerability", "ecosystem": "npm", "name": "tar", "version": "6.1.0",
         "reason": "GHSA-f5x3-32g6-xq36: tar denial of service", "fix": {"available": true, "version": "6.2.1"},
         "url": "https://osv.dev/vulnerability/GHSA-f5x3-32g6-xq36", "users": [{"repo": "acme/app", "path": "bun.lock"}]},
        {"severity": "low", "kind": "fresh-release", "ecosystem": "npm", "name": "x", "version": "1.0.0", "reason": "new",
         "fix": {"available": false}, "users": [{"repo": "acme/app", "path": "bun.lock"}]}
    ]});
    let found = parse_builtin(&risks);
    assert_eq!(found.len(), 1, "only the kinds the loop reports");
    assert_eq!(found[0].id.as_deref(), Some("GHSA-f5x3-32g6-xq36"));
    assert_eq!(found[0].title, "tar denial of service");
    assert_eq!(
        (found[0].fixed.as_deref(), found[0].lockfile.as_str()),
        (Some("6.2.1"), "bun.lock")
    );

    let deps = json!({"packages": [
        {"ecosystem": "npm", "name": "react", "direct": true, "latest": "19.1.0",
         "versions": [{"version": "17.0.2", "users": [{"repo": "acme/app", "path": "package-lock.json"}]}]},
        {"ecosystem": "npm", "name": "lodash", "direct": true, "latest": "4.17.21", "versions": [{"version": "4.17.20"}]},
        {"ecosystem": "npm", "name": "deep", "direct": false, "latest": "9.0.0", "versions": [{"version": "1.0.0"}]}
    ]});
    let outdated = parse_outdated(&deps);
    assert_eq!(
        outdated.len(),
        1,
        "a minor behind and a transitive are not reported: {outdated:#?}"
    );
    assert_eq!(outdated[0].title, "2 major versions behind (latest 19.1.0)");
    assert!(!outdated[0].dispatchable(), "major bumps are never automatic");
}

#[test]
fn versions_scores_and_minimal_fixes() {
    assert_eq!(cvss3_score("CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H"), Some(9.8));
    assert_eq!(cvss3_score("CVSS:3.1/AV:N/AC:L/PR:N/UI:R/S:U/C:L/I:L/A:N"), Some(5.4));
    assert_eq!(cvss3_score("not a vector"), None);
    let patched = vec!["^0.14.32".to_string(), ">=1.4.2".to_string()];
    assert_eq!(minimal_fix(Some("0.14.28"), &bounds_in(&patched)).as_deref(), Some("0.14.32"));
    assert_eq!(minimal_fix(Some("1.0.0"), &bounds_in(&patched)).as_deref(), Some("1.4.2"));
    assert_eq!(minimal_fix(Some("2.0.0"), &bounds_in(&patched)), None);
    assert!(is_major_bump("cargo", "0.14.28", "0.15.0"));
    assert!(!is_major_bump("cargo", "1.2.0", "1.9.0"));
    assert!(!is_major_bump("go", "0.17.0", "0.23.0"));
}

// --- which scanner reads what, and what is missing ----------------------------------------------

#[test]
fn a_missing_scanner_is_named_with_how_to_install_it() {
    let locks = vec![
        "Cargo.lock".to_string(),
        "web/package-lock.json".to_string(),
        "bun.lock".to_string(),
    ];
    // Nothing installed and the built-in lookup off: nothing is checked, and the report says what to install.
    let none = plan_scans(&locks, &["deny.toml".to_string()], |_| false, false);
    assert!(none.jobs.is_empty() && none.builtin.is_empty());
    let said = none.missing.join("\n");
    assert!(
        said.contains("Cargo.lock was not checked: install cargo-audit (cargo install --locked cargo-audit)"),
        "{said}"
    );
    assert!(
        said.contains("web/package-lock.json was not checked: install npm audit"),
        "{said}"
    );
    assert!(said.contains("switch the built-in OSV lookup on"), "{said}");
    assert!(said.contains("install cargo-deny"), "the licence policy is named: {said}");
    // The built-in lookup covers, and the note still names the better tool.
    let covered = plan_scans(&locks, &[], |_| false, true);
    assert_eq!(covered.builtin, locks);
    assert!(covered.missing.is_empty());
    assert!(
        covered.notes.iter().any(|n| n.contains("install cargo-audit")),
        "{:?}",
        covered.notes
    );
    // Installed tools are preferred, best first.
    let tools = plan_scans(&locks, &["deny.toml".to_string()], |t| t != Tool::NpmAudit, true);
    assert_eq!(
        tools.jobs,
        vec![
            Job {
                tool: Tool::CargoAudit,
                path: "Cargo.lock".into()
            },
            Job {
                tool: Tool::OsvScanner,
                path: "web/package-lock.json".into()
            },
            Job {
                tool: Tool::CargoDeny,
                path: "deny.toml".into()
            },
        ]
    );
    assert_eq!(tools.builtin, vec!["bun.lock".to_string()]);
}

// --- grouping, duplicates, caps, cooldown -------------------------------------------------------

fn fixture_findings() -> Vec<Finding> {
    let mut all = parse_cargo_audit(CARGO_AUDIT, "Cargo.lock").unwrap();
    all.extend(parse_npm_audit(NPM_AUDIT, "web/package-lock.json").unwrap());
    all
}

#[test]
fn findings_group_into_one_target_per_repository_and_ecosystem() {
    let targets = group("acme/app", &fixture_findings(), Severity::Moderate);
    assert_eq!(targets.len(), 2, "one per ecosystem, not one per package: {targets:#?}");
    let cargo = targets.iter().find(|t| t.ecosystem == "cargo").unwrap();
    let names: Vec<&str> = cargo.findings.iter().map(|f| f.package.as_str()).collect();
    assert_eq!(names, vec!["hyper", "futures-util"], "fixable only, worst first");
    assert_eq!(cargo.worst, Severity::Critical);
    assert_eq!(targets[0].ecosystem, "cargo", "the worst target first");
    let npm = targets.iter().find(|t| t.ecosystem == "npm").unwrap();
    assert_eq!(npm.findings.len(), 2, "vite and semver; lodash.template has no fix");
    // The threshold drops what is below it.
    let high = group("acme/app", &fixture_findings(), Severity::High);
    assert_eq!(high.iter().map(|t| t.findings.len()).sum::<usize>(), 3);
    // The same finding from two scanners counts once.
    let mut twice = fixture_findings();
    twice.extend(parse_cargo_audit(CARGO_AUDIT, "Cargo.lock").unwrap());
    assert_eq!(group("acme/app", &twice, Severity::Moderate)[0].findings.len(), 2);

    let text = brief(cargo);
    for needle in [
        "hyper 0.14.28",
        "fixed in 0.14.32",
        "RUSTSEC-2026-0012",
        "minimal bump",
        "no unrelated upgrades",
        "No major version bumps unless",
        "Run the repository's own checks",
        "Do not add any Claude/AI attribution",
    ] {
        assert!(text.contains(needle), "{needle}: {text}");
    }
    assert!(
        brief(npm).contains("vite (vulnerability) GHSA-xxxx-yyyy-zzzz"),
        "{}",
        brief(npm)
    );
    assert!(brief(npm).contains("needs a major bump"), "the major bump is called out");
}

fn target_colony(id: &str, repo: &str, origin: &str, status: SessionStatus) -> Session {
    let mut s = colony("acme", status);
    s.id = id.into();
    s.repo = repo.into();
    s.origin = Some(origin.into());
    s
}

#[test]
fn a_duplicate_target_is_refused_while_its_colony_is_live_or_its_pr_open() {
    let now = utc(29, 9);
    let targets = group("acme/app", &fixture_findings(), Severity::Moderate);
    let cargo = targets.iter().find(|t| t.ecosystem == "cargo").unwrap().clone();
    let record = |keys: Vec<String>| TargetRecord {
        session: "sc1".into(),
        repo: "acme/app".into(),
        ecosystem: "cargo".into(),
        keys,
        at: now - ChronoDuration::days(1),
    };
    for status in [SessionStatus::Running, SessionStatus::Queued, SessionStatus::PrOpened] {
        let sessions = vec![target_colony("sc1", "acme/app", "supply-chain:cargo", status)];
        assert_eq!(
            duplicate_of(&cargo, &[record(cargo.keys())], &sessions).as_deref(),
            Some("sc1"),
            "{status:?}"
        );
        // A colony with no record is treated as overlapping.
        assert_eq!(duplicate_of(&cargo, &[], &sessions).as_deref(), Some("sc1"));
        // Other findings, another ecosystem, or another repository do not block.
        assert_eq!(
            duplicate_of(&cargo, &[record(vec!["cargo:other@1:yanked:".into()])], &sessions),
            None
        );
        let npm_colony = vec![target_colony("sc1", "acme/app", "supply-chain:npm", status)];
        assert_eq!(duplicate_of(&cargo, &[], &npm_colony), None);
        let elsewhere = vec![target_colony("sc1", "acme/web", "supply-chain:cargo", status)];
        assert_eq!(duplicate_of(&cargo, &[], &elsewhere), None);
    }
    // Once merged or closed, the target may go again.
    for status in [SessionStatus::Merged, SessionStatus::Closed, SessionStatus::Failed] {
        let sessions = vec![target_colony("sc1", "acme/app", "supply-chain:cargo", status)];
        assert_eq!(duplicate_of(&cargo, &[record(cargo.keys())], &sessions), None, "{status:?}");
    }
    // The Packages view's hand-off on one of these packages counts too.
    let mut hand = colony("acme", SessionStatus::Running);
    hand.id = "hand".into();
    hand.repo = "acme/app".into();
    hand.issue_title = "Supply chain: hyper".into();
    assert_eq!(duplicate_of(&cargo, &[], &[hand.clone()]).as_deref(), Some("hand"));

    let (go, skipped) = plan_dispatch(vec![cargo], &Settings::default(), &[], &BTreeMap::new(), &[hand], now, false);
    assert!(go.is_empty());
    assert!(
        skipped[0].reason.contains("colony hand already targets these findings"),
        "{skipped:?}"
    );
}

#[test]
fn the_per_repository_and_per_run_caps_and_the_cooldown_hold_dispatches_back() {
    let now = utc(29, 9);
    let mut targets = group("acme/app", &fixture_findings(), Severity::Moderate);
    targets.extend(group("acme/web", &fixture_findings(), Severity::Moderate));
    targets.extend(group("acme/api", &fixture_findings(), Severity::Moderate));
    let settings = Settings {
        max_per_run: 2,
        ..Settings::default()
    };
    let (go, skipped) = plan_dispatch(targets.clone(), &settings, &[], &BTreeMap::new(), &[], now, false);
    assert_eq!(go.len(), 2, "the run cap: {go:#?}");
    assert_ne!(go[0].repo, go[1].repo, "one per repository by default");
    assert_eq!(skipped.len(), 4);
    assert!(
        skipped
            .iter()
            .any(|s| s.reason == "at most 1 dispatch per repository per run")
    );
    assert!(skipped.iter().any(|s| s.reason == "at most 2 dispatches per run"));

    // A repository dispatched to an hour ago cools down for the setting's twelve hours.
    let cooldowns = BTreeMap::from([("acme/app".to_string(), now - ChronoDuration::hours(1))]);
    let (go, skipped) = plan_dispatch(targets.clone(), &Settings::default(), &[], &cooldowns, &[], now, false);
    assert!(go.iter().all(|t| t.repo != "acme/app"), "{go:#?}");
    assert!(
        skipped
            .iter()
            .any(|s| s.repo == "acme/app" && s.reason.contains("cooling down until 2026-09-29 20:00 UTC"))
    );
    let later = now + ChronoDuration::hours(12);
    let (go, _) = plan_dispatch(targets, &Settings::default(), &[], &cooldowns, &[], later, false);
    assert!(go.iter().any(|t| t.repo == "acme/app"), "after the cooldown it goes again");
}

#[test]
fn critical_findings_with_no_fix_raise_attention() {
    let items = attention_items("acme/app", &fixture_findings());
    assert_eq!(items.len(), 1, "{items:#?}");
    assert_eq!(
        (items[0].package.as_str(), items[0].severity),
        ("lodash.template", Severity::Critical)
    );
    assert!(items[0].reason.contains("no fixed version"));
    // A moderate one with no fix (ancient-tls) is reported but raises nothing.
    assert!(items.iter().all(|i| i.package != "ancient-tls"));
}

// --- settings and the API -----------------------------------------------------------------------

#[test]
fn it_is_off_with_an_empty_allowlist_by_default_and_runs_at_most_hourly() {
    let d = Settings::default();
    assert!(!d.enabled && d.allow.is_empty() && !d.active());
    assert_eq!(d.cadence, Cadence::Daily { hour: 6, minute: 17 });
    assert_eq!((d.max_per_repo, d.cooldown_hours, d.outdated), (1, 12, false));
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
        allow: vec![" acme ".into(), "globex/api".into(), "ACME".into(), "".into()],
        ..Settings::default()
    }
    .validated()
    .unwrap();
    assert_eq!(allow.allow, vec!["acme".to_string(), "globex/api".to_string()]);
    assert!(allow.covers("acme/anything") && allow.covers("globex/api") && !allow.covers("globex/web"));
    assert!(
        Settings {
            allow: vec!["../etc".into()],
            ..Settings::default()
        }
        .validated()
        .is_err()
    );
    assert!(
        Settings {
            max_per_repo: 0,
            ..Settings::default()
        }
        .validated()
        .is_err()
    );
}

fn root(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("colonizer-supply-{tag}-{}", short_id()))
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
        allow: vec!["acme/app".into()],
        cadence: Cadence::Interval { minutes: 60 },
        ..Settings::default()
    };
    let Json(v) = put(State(app.clone()), Json(on.clone())).await.unwrap();
    assert_eq!(v["settings"]["enabled"], true);
    assert_eq!(v["settings"]["allow"], json!(["acme/app"]));
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
    // A real run is only for the allowlist; a dry run may look at anything.
    let err = run_now(
        State(app.clone()),
        Some(Json(RunRequest {
            dry_run: false,
            repo: Some("globex/web".into()),
        })),
    )
    .await
    .unwrap_err();
    assert!(
        err.message().contains("not on the supply-chain loop's allowlist"),
        "{}",
        err.message()
    );
    let _ = std::fs::remove_dir_all(root);
}

// --- whole runs, against a fake host ------------------------------------------------------------

#[derive(Default)]
struct Fake {
    tools: Vec<Tool>,
    files: BTreeMap<String, Vec<&'static str>>,
    orgs: BTreeMap<String, Vec<String>>,
    builtin: Vec<Finding>,
    ran: std::sync::Mutex<Vec<Job>>,
}

impl Host for Fake {
    fn has(&self, tool: Tool) -> bool {
        self.tools.contains(&tool)
    }

    async fn org_repos(&self, org: &str) -> Result<Vec<String>> {
        self.orgs.get(org).cloned().context("unknown org")
    }

    async fn files(&self, repo: &str) -> Result<RepoFiles> {
        let paths = self.files.get(repo).context("no such repository")?;
        Ok(RepoFiles {
            sha: "abc123".into(),
            files: paths.iter().map(|p| (p.to_string(), String::new())).collect(),
            bare: None,
        })
    }

    async fn run(&self, job: &Job, _: &RepoFiles) -> Result<(String, String)> {
        self.ran.lock().unwrap().push(job.clone());
        Ok(match job.tool {
            Tool::CargoAudit => (CARGO_AUDIT.to_string(), String::new()),
            Tool::NpmAudit => (NPM_AUDIT.to_string(), String::new()),
            Tool::OsvScanner => (OSV_SCANNER.to_string(), String::new()),
            Tool::CargoDeny => (String::new(), CARGO_DENY.to_string()),
        })
    }

    async fn builtin(&self, _: &str) -> Result<Vec<Finding>> {
        Ok(self.builtin.clone())
    }

    async fn outdated(&self, _: &str) -> Result<Vec<Finding>> {
        Ok(Vec::new())
    }
}

fn fake() -> Fake {
    Fake {
        tools: vec![Tool::CargoAudit, Tool::NpmAudit],
        files: BTreeMap::from([(
            "acme/app".to_string(),
            vec!["Cargo.toml", "Cargo.lock", "web/package.json", "web/package-lock.json"],
        )]),
        orgs: BTreeMap::from([("acme".to_string(), vec!["acme/app".to_string()])]),
        ..Fake::default()
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
async fn a_run_reports_dispatches_one_colony_per_target_and_refuses_the_duplicate_next_time() {
    let root = root("run");
    let app = app_at(&root);
    opt_in(&app, "acme").await;
    let host = fake();
    let report = run_once(&app, &host, &RunRequest::default(), "manual", utc(29, 9)).await;
    assert_eq!(report.repos.len(), 1, "the org expanded to its repository");
    assert_eq!(
        report.repos[0].scanners,
        vec!["cargo-audit".to_string(), "npm audit".to_string()]
    );
    assert_eq!(report.counts.get("critical"), Some(&2), "{:?}", report.counts);
    assert_eq!(
        report.dispatched.len(),
        1,
        "one per repository per run by default: {report:#?}"
    );
    let d = &report.dispatched[0];
    assert_eq!((d.ecosystem.as_str(), d.worst), ("cargo", Severity::Critical));
    let id = d.session.clone().expect("a colony was started");
    let s = app.session(&id).await.unwrap();
    assert_eq!(s.origin.as_deref(), Some("supply-chain:cargo"));
    assert!(s.instructions.contains("hyper 0.14.28") && s.instructions.contains("futures-util"));
    assert!(
        !s.instructions.contains("lodash"),
        "another ecosystem's findings are another target"
    );
    assert!(
        report
            .skipped
            .iter()
            .any(|k| k.ecosystem.as_deref() == Some("npm") && k.reason.contains("per repository"))
    );
    assert!(report.skipped.iter().any(|k| k.reason.contains("with no fixed version")));
    // The critical with no fix raised attention, and all of it was kept.
    assert_eq!(report.attention.len(), 1);
    let st = app.supply_chain.snapshot().await;
    assert_eq!(st.attention.len(), 1);
    assert_eq!(st.history.len(), 1);
    assert_eq!(st.history[0].dispatched, 1);
    assert!(st.last_report.is_some());
    assert_eq!(st.targets.len(), 1);
    assert!(st.cooldowns.contains_key("acme/app"));
    let log = std::fs::read_to_string(app.cfg.data_dir.join("activity.jsonl")).unwrap_or_default();
    assert!(log.contains("\"loop.supply_chain\"") && log.contains(NAME), "{log}");

    // The next run, past the cooldown: the cargo colony is still live, so its target is refused
    // and the npm target goes instead.
    let report = run_once(&app, &host, &RunRequest::default(), "schedule", utc(30, 9)).await;
    assert!(
        report
            .skipped
            .iter()
            .any(|k| k.ecosystem.as_deref() == Some("cargo") && k.reason.contains(&format!("colony {id} already targets")))
    );
    assert_eq!(report.dispatched.len(), 1);
    assert_eq!(report.dispatched[0].ecosystem, "npm");
    assert_eq!(app.supply_chain.snapshot().await.history.len(), 2);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn the_kill_switch_reports_but_starts_nothing() {
    let root = root("kill");
    let app = app_at(&root);
    opt_in(&app, "acme/app").await;
    let _blocked = crate::authority::test_block_external_writes();
    let report = run_once(&app, &fake(), &RunRequest::default(), "schedule", utc(29, 9)).await;
    assert!(report.blocked);
    assert!(report.dispatched.is_empty());
    assert!(!report.repos[0].findings.is_empty(), "the findings are still reported");
    assert!(
        report
            .skipped
            .iter()
            .filter(|k| k.ecosystem.is_some())
            .all(|k| k.reason.contains("COLONIZER_NO_EXTERNAL_EFFECTS"))
    );
    assert!(app.sessions.read().await.is_empty(), "no colony");
    let st = app.supply_chain.snapshot().await;
    assert!(st.last_report.is_some(), "the report is kept");
    assert!(st.cooldowns.is_empty() && st.targets.is_empty());
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_dry_run_lists_what_it_would_dispatch_and_writes_nothing() {
    let root = root("dry");
    let app = app_at(&root);
    // Not even opted in: a dry run may look at any one repository.
    let req = RunRequest {
        dry_run: true,
        repo: Some("acme/app".into()),
    };
    let report = run_once(&app, &fake(), &req, "manual", utc(29, 9)).await;
    assert!(report.dry_run);
    assert_eq!(report.dispatched.len(), 1);
    assert!(report.dispatched[0].session.is_none(), "would dispatch, did not");
    assert!(report.summary().contains("would dispatch 1"), "{}", report.summary());
    assert!(app.sessions.read().await.is_empty());
    let st = app.supply_chain.snapshot().await;
    assert!(st.last_report.is_none() && st.history.is_empty() && st.attention.is_empty());
    assert!(!app.cfg.config_dir.join(FILE).exists(), "nothing saved");
    assert!(!app.cfg.data_dir.join("activity.jsonl").exists(), "nothing logged");
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn with_no_scanner_the_report_says_what_to_install() {
    let host = Fake {
        tools: Vec::new(),
        ..fake()
    };
    let off = Settings {
        builtin: false,
        ..Settings::default()
    };
    let r = check_repo(&host, "acme/app", &off).await;
    assert!(r.findings.is_empty() && r.scanners.is_empty());
    assert!(r.missing.iter().any(|m| m.contains("install cargo-audit")), "{:?}", r.missing);
    assert!(host.ran.lock().unwrap().is_empty(), "nothing ran");

    // The built-in lookup covers for the missing tools, only for the lockfiles left to it.
    let mut builtin = parse_npm_audit(NPM_AUDIT, "web/package-lock.json").unwrap();
    builtin.push(Finding {
        lockfile: "somewhere/else/package-lock.json".into(),
        ..builtin[0].clone()
    });
    let host = Fake {
        tools: Vec::new(),
        builtin,
        ..fake()
    };
    let r = check_repo(&host, "acme/app", &Settings::default()).await;
    assert_eq!(r.scanners, vec!["built-in OSV lookup".to_string()]);
    assert_eq!(r.findings.len(), 3, "{:#?}", r.findings);
    assert!(r.notes.iter().any(|n| n.contains("install cargo-audit")));

    // An unreadable mirror is the repository's error, not the run's.
    let r = check_repo(&host, "acme/gone", &Settings::default()).await;
    assert!(r.error.unwrap().contains("could not read the mirror"));
}

#[tokio::test]
async fn the_schedule_fires_only_when_switched_on_opted_in_and_due() {
    let root = root("tick");
    let app = app_at(&root);
    let host = fake();
    let now = Utc::now();
    assert!(tick(&app, &host, now).await.is_none(), "off by default");
    opt_in(&app, "acme/app").await;
    assert!(tick(&app, &host, now).await.is_none(), "not due yet");
    let later = now + ChronoDuration::days(2);
    let report = tick(&app, &host, later).await.expect("due");
    assert_eq!(report.trigger, "schedule");
    let st = app.supply_chain.snapshot().await;
    assert!(st.next_run_at.unwrap() > later, "the next slot is booked");
    let _ = std::fs::remove_dir_all(root);
}
