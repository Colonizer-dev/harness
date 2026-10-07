//! The merge-train loop against a scripted GitHub: every reading is queued up front, every write
//! is logged, and time only moves when the loop sleeps. Nothing here reaches the network.

use super::*;
use crate::github::{PrCommit, PrInfo, PrState};
use crate::sessions::tests::colony;
use chrono::TimeZone;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

mod local;
mod quiet_head;
mod resolve;

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 30, 9, 0, 0).unwrap()
}

/// Pops a scripted answer; the last one repeats forever.
fn next<T: Clone>(q: &mut VecDeque<T>) -> T {
    if q.len() > 1 {
        q.pop_front().unwrap()
    } else {
        q.front().cloned().expect("nothing scripted")
    }
}

#[derive(Default)]
struct Fake {
    now: Cell<Option<DateTime<Utc>>>,
    main: RefCell<VecDeque<Result<MainCi, String>>>,
    prs: RefCell<BTreeMap<String, VecDeque<Result<Reading, String>>>>,
    /// What a pull request reads as once its branch was updated.
    after_update: RefCell<BTreeMap<String, Vec<Reading>>>,
    rebase: RefCell<BTreeMap<String, RebaseResult>>,
    /// Every call, in order: `main`, `read s1`, `merge s1`, `update s1`, `rerun 77`, `dispatch fix` …
    log: RefCell<Vec<String>>,
    merged_at: RefCell<Vec<DateTime<Utc>>>,
    /// Issue #969: the repository's local-check settings (off when unset) and the local runs' answers.
    local: RefCell<Option<LocalChecks>>,
    local_runs: RefCell<VecDeque<LocalRun>>,
    /// Issue #968: what starting a resolve answers (resuming on `src/shared.rs` when unset).
    resolves: RefCell<VecDeque<super::resolve::Started>>,
    /// Issue #1075: each head's own checks and push time (green, pushed an hour before `t0`, when
    /// unset), the merge answers in order (merged when none), the branch tips after a merge (the
    /// merged head when unset), and the quiet period in minutes (10 when unset).
    heads: RefCell<BTreeMap<String, HeadReading>>,
    merges: RefCell<VecDeque<Result<Option<String>, String>>>,
    tips: RefCell<BTreeMap<String, String>>,
    last_merged: RefCell<Option<String>>,
    quiet: Cell<Option<u64>>,
}

impl Fake {
    fn new() -> Self {
        let f = Fake::default();
        f.now.set(Some(t0()));
        f
    }
    fn main(self, answers: Vec<Result<MainCi, String>>) -> Self {
        *self.main.borrow_mut() = answers.into();
        self
    }
    fn pr(self, id: &str, answers: Vec<Result<Reading, String>>) -> Self {
        self.prs.borrow_mut().insert(id.to_string(), answers.into());
        self
    }
    fn writes(&self) -> Vec<String> {
        self.log
            .borrow()
            .iter()
            .filter(|l| {
                ![
                    "main",
                    "read",
                    "guards",
                    "branch",
                    "config",
                    "head",
                    "tip",
                    "delete-branch",
                    "said",
                ]
                .iter()
                .any(|r| l.starts_with(r))
            })
            .cloned()
            .collect()
    }
    fn say(&self, line: String) {
        self.log.borrow_mut().push(line);
    }
}

impl HeadOps for Fake {
    async fn read_head(&self, _repo: &str, sha: &str) -> Result<HeadReading, String> {
        self.say(format!("head {sha}"));
        Ok(self.heads.borrow().get(sha).cloned().unwrap_or(HeadReading {
            ci: merge_head::HeadCi::Green,
            pushed_at: Some(t0() - ChronoDuration::hours(1)),
        }))
    }
    async fn merge_pinned(&self, _repo: &str, number: u64, head: &str, title: &str) -> Result<Option<String>, String> {
        let answer = self
            .merges
            .borrow_mut()
            .pop_front()
            .unwrap_or_else(|| Ok(Some(format!("sha-s{number}"))));
        if answer.is_ok() {
            // The sessions here are `s<n>` for pull request `n`.
            self.say(format!("merge s{number} {title}"));
            self.merged_at.borrow_mut().push(self.now());
            *self.last_merged.borrow_mut() = Some(head.to_string());
        } else {
            self.say(format!("refused s{number} sha={head}"));
        }
        answer
    }
    async fn branch_tip(&self, _repo: &str, branch: &str) -> Result<Option<String>, String> {
        self.say(format!("tip {branch}"));
        Ok(self
            .tips
            .borrow()
            .get(branch)
            .cloned()
            .or_else(|| self.last_merged.borrow().clone()))
    }
    async fn delete_branch(&self, _repo: &str, branch: &str) -> Result<(), String> {
        self.say(format!("delete-branch {branch}"));
        Ok(())
    }
}

impl Ops for Fake {
    async fn quiet_minutes(&self) -> u64 {
        self.quiet.get().unwrap_or(10)
    }
    async fn merged(&self, s: &Session, head: &str) {
        self.say(format!("said merged {} {head}", s.id));
    }
    fn now(&self) -> DateTime<Utc> {
        self.now.get().unwrap()
    }
    async fn sleep(&self, d: Duration) {
        self.now.set(Some(self.now() + ChronoDuration::from_std(d).unwrap()));
    }
    async fn guards(&self) -> Result<Guards, String> {
        self.say("guards".into());
        Ok(Guards {
            allowed_authors: vec!["colonizer-settlers".to_string()],
            forbidden: Vec::new(),
        })
    }
    async fn default_branch(&self, repo: &str) -> Result<String, String> {
        self.say(format!("branch {repo}"));
        Ok("main".to_string())
    }
    async fn main_ci(&self, _repo: &str, _branch: &str) -> Result<MainCi, String> {
        self.say("main".into());
        next(&mut self.main.borrow_mut())
    }
    async fn read_pr(&self, s: &Session, _base: &str) -> Result<Reading, String> {
        self.say(format!("read {}", s.id));
        let mut prs = self.prs.borrow_mut();
        next(prs.get_mut(&s.id).expect("pull request not scripted"))
    }
    async fn update_branch(&self, s: &Session, _head: &str) -> Result<(), String> {
        self.say(format!("update {}", s.id));
        if let Some(after) = self.after_update.borrow_mut().remove(&s.id) {
            self.prs
                .borrow_mut()
                .insert(s.id.clone(), after.into_iter().map(Ok).collect());
        }
        Ok(())
    }
    async fn rebase(&self, s: &Session) -> RebaseResult {
        self.say(format!("rebase {}", s.id));
        self.rebase
            .borrow()
            .get(&s.id)
            .cloned()
            .unwrap_or(RebaseResult::Rebased("main-tip".into()))
    }
    async fn rerun(&self, _repo: &str, run_id: u64) -> Result<(), String> {
        self.say(format!("rerun {run_id}"));
        Ok(())
    }
    async fn failure_log(&self, _repo: &str, run_id: u64) -> Result<String, String> {
        self.say(format!("log {run_id}"));
        Ok("error[E0308]: mismatched types".to_string())
    }
    async fn dispatch(&self, d: Dispatch) -> Result<String, String> {
        let what = match &d {
            Dispatch::Redo { session, .. } => format!("redo {}", session.id),
            Dispatch::Fix { log, .. } => format!("fix ({log})"),
            Dispatch::Revert { merged, .. } => format!("revert {}", merged.sha.clone().unwrap_or_default()),
        };
        self.say(format!("dispatch {what}"));
        Ok("c0lony".to_string())
    }
    async fn local_config(&self, _repo: &str, _base: &str, opted_in: bool) -> Result<LocalChecks, String> {
        self.say(format!("config opted_in={opted_in}"));
        Ok(self
            .local
            .borrow()
            .clone()
            .unwrap_or(LocalChecks::Off("local checks are off here".into())))
    }
    async fn local_run(&self, s: &Session, head: &str, _base: &str, commands: &[String]) -> LocalRun {
        self.say(format!("local {} {head} [{}]", s.id, commands.join("; ")));
        next(&mut self.local_runs.borrow_mut())
    }
    async fn post_status(&self, _repo: &str, sha: &str, state: &str, _description: &str) -> Result<(), String> {
        self.say(format!("status {sha} {state}"));
        Ok(())
    }
    async fn resolve(&self, s: &Session, base: &str) -> super::resolve::Started {
        self.say(format!("resolve {} onto {base}", s.id));
        let mut q = self.resolves.borrow_mut();
        if q.is_empty() {
            return super::resolve::Started::Resuming(vec!["src/shared.rs".into()]);
        }
        next(&mut q)
    }
    async fn reset_resolve(&self, s: &Session) -> Result<(), String> {
        self.say(format!("reset {}", s.id));
        Ok(())
    }
    async fn label_needs_human(&self, s: &Session) -> Result<(), String> {
        self.say(format!("label {} needs-human", s.id));
        Ok(())
    }
}

fn session(id: &str, n: u64) -> Session {
    let mut s = colony("acme", SessionStatus::PrOpened);
    s.id = id.into();
    s.repo = "acme/web".into();
    s.branch = format!("colonizer/issue-{n}");
    s.issue_title = format!("Change {n}");
    s.pr_url = Some(format!("https://github.com/acme/web/pull/{n}"));
    s.pr_opened_at = Some(t0() + ChronoDuration::minutes(n as i64));
    s
}

fn reading(n: u64, mergeability: Mergeability, ci: CiState, behind: u64) -> Reading {
    Reading {
        facts: PrFacts {
            info: PrInfo {
                state: PrState::Open,
                mergeability,
                merge_state_status: "CLEAN".into(),
                merged_at: None,
                base_ref_oid: Some("main-tip".into()),
                created_at: None,
                ci,
                is_draft: false,
                title: format!("Change {n}"),
                labels: Vec::new(),
                head_ref_name: Some(format!("colonizer/issue-{n}")),
                head_ref_oid: Some(format!("head{n}")),
                base_ref_name: Some("main".into()),
            },
            commits: vec![PrCommit {
                authors: vec![(
                    "colonizer-settlers".into(),
                    "331648616+colonizer-settlers@users.noreply.github.com".into(),
                )],
                message: "do the thing".into(),
            }],
            behind_base: Some(behind),
            base_is_default: true,
        },
        failing: Vec::new(),
        unavailable: None,
    }
}

fn green(n: u64) -> Result<Reading, String> {
    Ok(reading(n, Mergeability::Clean, CiState::Success, 0))
}

fn main_green() -> Result<MainCi, String> {
    Ok(MainCi::Green { sha: "tip0".into() })
}

fn cfg() -> Settings {
    Settings {
        enabled: true,
        allow: vec!["acme/web".into()],
        ..Settings::default()
    }
}

async fn run(fake: &Fake, cfg: &Settings, sessions: &[Session], memory: &mut BTreeMap<String, RepoMemory>, dry: bool) -> Report {
    run_all(fake, cfg, |_| false, sessions, memory, dry).await
}

fn item<'a>(r: &'a Report, session: &str) -> &'a Item {
    r.repos
        .iter()
        .flat_map(|x| &x.items)
        .find(|i| i.session == session)
        .unwrap_or_else(|| panic!("no item for {session}: {r:#?}"))
}

// -- Rule 1: main's CI.

#[test]
fn main_is_green_only_on_a_completed_successful_run_of_its_tip() {
    let run = |id: u64, wf: u64, status: &str, conclusion: Option<&str>, at: &str| json!({"id": id, "workflow_id": wf, "name": format!("ci-{wf}"), "head_sha": "tip", "status": status, "conclusion": conclusion, "created_at": at});
    let runs = |items: Vec<Value>| json!({ "workflow_runs": items });
    assert_eq!(
        main_ci_from(
            "tip",
            &runs(vec![run(1, 1, "completed", Some("success"), "2026-09-30T09:00:00Z")])
        ),
        MainCi::Green { sha: "tip".into() }
    );
    // In progress is a wait, not red.
    assert!(matches!(
        main_ci_from("tip", &runs(vec![run(1, 1, "in_progress", None, "2026-09-30T09:00:00Z")])),
        MainCi::Pending { .. }
    ));
    // Cancelled and re-queued is a wait, judged by the re-queued run.
    let requeued = main_ci_from(
        "tip",
        &runs(vec![
            run(2, 1, "queued", None, "2026-09-30T09:05:00Z"),
            run(1, 1, "completed", Some("cancelled"), "2026-09-30T09:00:00Z"),
        ]),
    );
    assert!(
        matches!(&requeued, MainCi::Pending { reason } if reason.contains("still running")),
        "{requeued:?}"
    );
    // Cancelled alone is not red either.
    let cancelled = main_ci_from(
        "tip",
        &runs(vec![run(1, 1, "completed", Some("cancelled"), "2026-09-30T09:00:00Z")]),
    );
    assert!(
        matches!(&cancelled, MainCi::Pending { reason } if reason.contains("not red")),
        "{cancelled:?}"
    );
    // A newer green run of the same workflow replaces an older red one.
    assert_eq!(
        main_ci_from(
            "tip",
            &runs(vec![
                run(2, 1, "completed", Some("success"), "2026-09-30T09:05:00Z"),
                run(1, 1, "completed", Some("failure"), "2026-09-30T09:00:00Z"),
            ])
        ),
        MainCi::Green { sha: "tip".into() }
    );
    // Any workflow failing is red, with the runs to re-run.
    assert_eq!(
        main_ci_from(
            "tip",
            &runs(vec![
                run(3, 2, "completed", Some("failure"), "2026-09-30T09:05:00Z"),
                run(1, 1, "completed", Some("success"), "2026-09-30T09:00:00Z"),
            ])
        ),
        MainCi::Red {
            sha: "tip".into(),
            run_ids: vec![3],
            detail: "failed: ci-2".into()
        }
    );
    // No run on the tip yet (or only runs of an older commit): wait.
    let older = json!({"workflow_runs": [{"id": 1, "workflow_id": 1, "head_sha": "old", "status": "completed", "conclusion": "success"}]});
    assert!(matches!(main_ci_from("tip", &older), MainCi::Pending { .. }));
    assert!(matches!(main_ci_from("tip", &Value::Null), MainCi::Pending { .. }));
}

#[tokio::test]
async fn a_main_that_is_pending_or_red_merges_nothing() {
    for main in [
        Ok(MainCi::Pending {
            reason: "still running: ci".into(),
        }),
        Ok(MainCi::Pending {
            reason: "cancelled, not red: waiting for ci to run again".into(),
        }),
        Ok(MainCi::Red {
            sha: "someone-elses".into(),
            run_ids: vec![9],
            detail: "failed: ci".into(),
        }),
    ] {
        let fake = Fake::new().main(vec![main.clone()]).pr("s1", vec![green(1)]);
        let mut memory = BTreeMap::new();
        let r = run(&fake, &cfg(), &[session("s1", 1)], &mut memory, false).await;
        assert!(fake.writes().is_empty(), "{main:?}: {:?}", fake.writes());
        assert!(
            !fake.log.borrow().iter().any(|l| l.starts_with("read")),
            "no pull request is even read"
        );
        let i = item(&r, "s1");
        assert_eq!(i.action, Action::Waiting);
        assert!(
            i.reason.contains("merging nothing") || i.reason.contains("is red"),
            "{}",
            i.reason
        );
        if matches!(main, Ok(MainCi::Red { .. })) {
            assert!(r.repos[0].heal[0].contains("not the train's merge"), "{:?}", r.repos[0].heal);
            assert_eq!(
                memory["acme/web"].paused, None,
                "someone else's red main does not pause the train"
            );
        }
    }
}

// -- Rule 2 and 3: fresh CI, one at a time, the cap and the cooldown.

#[tokio::test]
async fn a_green_pull_request_on_the_current_base_is_squash_merged_with_its_title() {
    let fake = Fake::new().main(vec![main_green()]).pr("s1", vec![green(1)]);
    let mut memory = BTreeMap::new();
    let r = run(&fake, &cfg(), &[session("s1", 1)], &mut memory, false).await;
    assert_eq!(fake.writes(), vec!["merge s1 Change 1 (#1)".to_string()]);
    assert_eq!(item(&r, "s1").action, Action::Merged);
    let merged = memory["acme/web"].last_train_merge.clone().unwrap();
    assert_eq!(
        (merged.sha.as_deref(), merged.pr_url.as_str()),
        (Some("sha-s1"), "https://github.com/acme/web/pull/1")
    );
}

#[tokio::test]
async fn a_pull_request_behind_by_one_is_updated_not_merged() {
    let fake = Fake::new()
        .main(vec![main_green()])
        .pr("s1", vec![Ok(reading(1, Mergeability::Clean, CiState::Success, 1))]);
    // After the update its fresh CI never finishes inside the wait.
    fake.after_update
        .borrow_mut()
        .insert("s1".into(), vec![reading(1, Mergeability::Clean, CiState::Pending, 0)]);
    let mut memory = BTreeMap::new();
    let r = run(&fake, &cfg(), &[session("s1", 1)], &mut memory, false).await;
    assert_eq!(fake.writes(), vec!["update s1".to_string()], "updated, never merged");
    let i = item(&r, "s1");
    assert_eq!(i.action, Action::Updated);
    assert!(i.reason.contains("still running after 20 min"), "{}", i.reason);
    // GitHub's BEHIND reading goes the same way.
    let fake = Fake::new()
        .main(vec![main_green()])
        .pr("s1", vec![Ok(reading(1, Mergeability::Behind, CiState::Success, 1))]);
    let r = run(&fake, &cfg(), &[session("s1", 1)], &mut BTreeMap::new(), false).await;
    assert_eq!(fake.writes(), vec!["update s1".to_string()]);
    assert_eq!(item(&r, "s1").action, Action::Updated);
}

#[tokio::test]
async fn after_a_merge_the_next_one_is_updated_waited_for_and_merged_on_fresh_ci() {
    let fake = Fake::new()
        // Green; the new tip's CI still running right after the merge; then green for good.
        .main(vec![
            main_green(),
            Ok(MainCi::Pending {
                reason: "still running: ci".into(),
            }),
            main_green(),
        ])
        .pr("s1", vec![green(1)])
        // Mergeable on the first reading, behind once s1 landed.
        .pr("s2", vec![green(2), Ok(reading(2, Mergeability::Clean, CiState::Success, 1))]);
    fake.after_update.borrow_mut().insert(
        "s2".into(),
        vec![
            reading(2, Mergeability::Clean, CiState::Pending, 0),
            reading(2, Mergeability::Clean, CiState::Success, 0),
        ],
    );
    let mut memory = BTreeMap::new();
    let r = run(&fake, &cfg(), &[session("s1", 1), session("s2", 2)], &mut memory, false).await;
    assert_eq!(
        fake.writes(),
        vec![
            "merge s1 Change 1 (#1)".to_string(),
            "update s2".to_string(),
            "merge s2 Change 2 (#2)".to_string()
        ]
    );
    assert_eq!(item(&r, "s2").action, Action::Merged);
    let at = fake.merged_at.borrow();
    assert!(at[1] - at[0] >= ChronoDuration::seconds(120), "the cooldown held: {at:?}");
}

/// A session whose pull request touched `path`, so the merge train knows it overlaps another.
fn touching(id: &str, n: u64, path: &str) -> Session {
    let mut s = session(id, n);
    s.changed_paths = vec![path.to_string()];
    s
}

#[tokio::test]
async fn a_merge_brings_the_candidates_that_share_its_files_onto_the_new_base_before_their_turn() {
    // Three pull requests all touching one file: s1 merges, s2 and s3 are brought onto the new base
    // at once; merging s2 moves the base again, so s3 is brought up to date a second time.
    let fake = Fake::new()
        // Green; the new tip's CI still running right after s1; then green for the rest.
        .main(vec![
            main_green(),
            Ok(MainCi::Pending {
                reason: "still running: ci".into(),
            }),
            main_green(),
            main_green(),
            main_green(),
        ])
        .pr("s1", vec![green(1)])
        // Mergeable, then behind once s1 landed, then waiting on its fresh CI, then green.
        .pr(
            "s2",
            vec![
                green(2),
                Ok(reading(2, Mergeability::Behind, CiState::Success, 1)),
                Ok(reading(2, Mergeability::Clean, CiState::Pending, 0)),
                Ok(reading(2, Mergeability::Clean, CiState::Success, 0)),
            ],
        )
        // Behind once s1 landed; updated; waiting while s2 is dealt with; behind again once s2
        // landed; updated once more; then green.
        .pr(
            "s3",
            vec![
                green(3),
                Ok(reading(3, Mergeability::Behind, CiState::Success, 1)),
                Ok(reading(3, Mergeability::Clean, CiState::Pending, 0)),
                Ok(reading(3, Mergeability::Clean, CiState::Pending, 0)),
                Ok(reading(3, Mergeability::Behind, CiState::Success, 1)),
                Ok(reading(3, Mergeability::Clean, CiState::Success, 0)),
            ],
        );
    let mut memory = BTreeMap::new();
    let sessions = [
        touching("s1", 1, "docs/protocol.md"),
        touching("s2", 2, "docs/protocol.md"),
        touching("s3", 3, "docs/protocol.md"),
    ];
    let r = run(&fake, &cfg(), &sessions, &mut memory, false).await;
    assert_eq!(
        fake.writes(),
        vec![
            "merge s1 Change 1 (#1)".to_string(),
            "update s2".to_string(),
            "update s3".to_string(),
            "merge s2 Change 2 (#2)".to_string(),
            "update s3".to_string(),
            "merge s3 Change 3 (#3)".to_string(),
        ]
    );
    for id in ["s1", "s2", "s3"] {
        assert_eq!(item(&r, id).action, Action::Merged, "{id}: {:#?}", r.lines);
    }
    assert!(
        !fake.writes().iter().any(|w| w.starts_with("rebase")),
        "none of them ever read dirty: {:?}",
        fake.writes()
    );
}

#[tokio::test]
async fn a_candidate_that_shares_no_files_waits_for_its_own_turn_to_be_updated() {
    let fake = Fake::new()
        .main(vec![
            main_green(),
            Ok(MainCi::Pending {
                reason: "still running: ci".into(),
            }),
            main_green(),
            main_green(),
        ])
        .pr("s1", vec![green(1)])
        .pr(
            "s2",
            vec![
                green(2),
                Ok(reading(2, Mergeability::Behind, CiState::Success, 1)),
                Ok(reading(2, Mergeability::Clean, CiState::Success, 0)),
            ],
        );
    let mut memory = BTreeMap::new();
    let sessions = [touching("s1", 1, "docs/a.md"), touching("s2", 2, "docs/b.md")];
    let r = run(&fake, &cfg(), &sessions, &mut memory, false).await;
    assert_eq!(
        fake.writes(),
        vec![
            "merge s1 Change 1 (#1)".to_string(),
            "update s2".to_string(),
            "merge s2 Change 2 (#2)".to_string(),
        ]
    );
    // The update came from the head of the queue, after main was read again — not fanned out by
    // the merge, which nothing shares a file with.
    let log = fake.log.borrow();
    let at = log.iter().position(|l| l == "merge s1 Change 1 (#1)").unwrap();
    // After the merge's own bookkeeping: the branch tip, its delete, and the log line.
    let next = log[at + 1..]
        .iter()
        .find(|l| !["tip", "delete-branch", "said"].iter().any(|p| l.starts_with(p)));
    assert_eq!(next.map(String::as_str), Some("main"), "{log:?}");
    assert_eq!(item(&r, "s2").action, Action::Merged);
}

#[tokio::test]
async fn a_candidate_that_shares_files_and_conflicts_after_the_merge_is_rebased() {
    let fake = Fake::new().main(vec![main_green()]).pr("s1", vec![green(1)]).pr(
        "s2",
        vec![green(2), Ok(reading(2, Mergeability::Conflicted, CiState::Success, 1))],
    );
    let mut memory = BTreeMap::new();
    let sessions = [touching("s1", 1, "docs/protocol.md"), touching("s2", 2, "docs/protocol.md")];
    let r = run(&fake, &cfg(), &sessions, &mut memory, false).await;
    assert_eq!(
        fake.writes(),
        vec!["merge s1 Change 1 (#1)".to_string(), "rebase s2".to_string()]
    );
    assert_eq!(item(&r, "s2").action, Action::Rebased);
}

#[tokio::test]
async fn a_merge_that_reaches_the_cap_does_not_bring_the_rest_onto_the_new_base() {
    // One more merge was allowed than this run may take: after s1 the cap is spent, so s2 is left
    // for the next run rather than updated — which would only wait out CI it can no longer use.
    let mut settings = cfg();
    settings.max_merges = 1;
    let fake = Fake::new().main(vec![main_green()]).pr("s1", vec![green(1)]).pr(
        "s2",
        vec![green(2), Ok(reading(2, Mergeability::Behind, CiState::Success, 1))],
    );
    let mut memory = BTreeMap::new();
    let sessions = [touching("s1", 1, "docs/a.md"), touching("s2", 2, "docs/a.md")];
    let r = run(&fake, &settings, &sessions, &mut memory, false).await;
    assert_eq!(
        fake.writes(),
        vec!["merge s1 Change 1 (#1)".to_string()],
        "{:?}",
        fake.writes()
    );
    let i = item(&r, "s2");
    assert_eq!(i.action, Action::Waiting);
    assert!(i.reason.contains("merge cap (1)"), "{}", i.reason);
}

#[tokio::test]
async fn the_merge_cap_and_the_cooldown_hold_within_a_run_and_across_runs() {
    let mut settings = cfg();
    settings.max_merges = 2;
    settings.cooldown_secs = 300;
    let fake = Fake::new()
        .main(vec![main_green()])
        .pr("s1", vec![green(1)])
        .pr("s2", vec![green(2)])
        .pr("s3", vec![green(3)]);
    let mut memory = BTreeMap::new();
    // The previous run merged a minute ago: the first merge here still waits out the cooldown.
    memory.insert(
        "acme/web".to_string(),
        RepoMemory {
            last_merge_at: Some(t0() - ChronoDuration::seconds(60)),
            ..RepoMemory::default()
        },
    );
    let sessions = [session("s1", 1), session("s2", 2), session("s3", 3)];
    let r = run(&fake, &settings, &sessions, &mut memory, false).await;
    let at = fake.merged_at.borrow().clone();
    assert_eq!(at.len(), 2, "the cap: {:?}", fake.writes());
    assert!(
        at[0] >= t0() + ChronoDuration::seconds(240),
        "the cooldown carried across runs: {at:?}"
    );
    assert!(at[1] - at[0] >= ChronoDuration::seconds(300), "and between merges: {at:?}");
    let third = item(&r, "s3");
    assert_eq!(third.action, Action::Waiting);
    assert!(third.reason.contains("merge cap (2)"), "{}", third.reason);
    // A per-repository cap replaces the global one.
    settings.repo_max_merges.insert("acme/web".into(), 1);
    let fake = Fake::new()
        .main(vec![main_green()])
        .pr("s1", vec![green(1)])
        .pr("s2", vec![green(2)]);
    run(&fake, &settings, &sessions[..2], &mut BTreeMap::new(), false).await;
    assert_eq!(fake.merged_at.borrow().len(), 1);
}

// -- Rule 4: eligibility.

#[tokio::test]
async fn hold_draft_superseded_held_and_not_opted_in_pull_requests_are_skipped() {
    let mut hold = reading(1, Mergeability::Clean, CiState::Success, 0);
    hold.facts.info.labels = vec!["HOLD".into()];
    let mut draft = reading(2, Mergeability::Clean, CiState::Success, 0);
    draft.facts.info.is_draft = true;
    let fake = Fake::new()
        .main(vec![main_green()])
        .pr("hold", vec![Ok(hold)])
        .pr("draft", vec![Ok(draft)])
        .pr("fine", vec![green(6)]);
    let mut superseded = session("old", 3);
    superseded.issue = Some(42);
    let mut redo = colony("acme", SessionStatus::Running);
    redo.id = "redo".into();
    redo.repo = "acme/web".into();
    redo.origin = Some(format!("{ORIGIN_REDO}old"));
    let mut foreign = session("elsewhere", 4);
    foreign.repo = "other/app".into();
    let mut hand = session("hand", 5);
    hand.branch = "feature/by-hand".into();
    let mut settings = cfg();
    settings.held = vec!["kept".into()];
    let sessions = vec![
        session("hold", 1),
        session("draft", 2),
        superseded,
        redo,
        foreign,
        hand,
        session("kept", 7),
        session("fine", 6),
    ];
    let r = run(&fake, &settings, &sessions, &mut BTreeMap::new(), false).await;
    let why = |id: &str| {
        let i = item(&r, id);
        assert_eq!(i.action, Action::Skipped, "{id}: {i:?}");
        i.reason.clone()
    };
    assert!(why("hold").contains("marked do-not-merge"));
    assert!(why("draft").contains("draft"));
    assert!(why("old").contains("superseded by redo"));
    assert!(why("elsewhere").contains("not opted in"));
    assert!(why("hand").contains("colonizer/*"));
    assert!(why("kept").contains("held"));
    assert_eq!(item(&r, "fine").action, Action::Merged);
    let log = fake.log.borrow();
    for id in ["elsewhere", "old", "hand", "kept"] {
        assert!(!log.contains(&format!("read {id}")), "{id} was never even read");
    }
    assert!(
        !log.iter().any(|l| l == "branch other/app"),
        "a repository not opted in is not touched"
    );
}

#[test]
fn the_never_list_and_the_deny_orgs_beat_the_allowlist_which_is_empty_by_default() {
    let d = Settings::default();
    assert_eq!(opt_in(&d, "acme/web", false), OptIn::NotOptedIn);
    let mut s = cfg();
    s.allow = vec!["acme".into()];
    assert_eq!(
        opt_in(&s, "Acme/Web", false),
        OptIn::In,
        "an org entry covers its repositories"
    );
    s.never = vec!["acme/web".into()];
    assert!(matches!(opt_in(&s, "acme/web", false), OptIn::Never(_)));
    assert_eq!(opt_in(&s, "acme/api", false), OptIn::In);
    assert!(matches!(opt_in(&s, "acme/api", true), OptIn::Never(_)));
    assert!(drives(&s, "acme/api"));
    s.enabled = false;
    assert!(
        !drives(&s, "acme/api"),
        "a switched-off loop leaves the train's own tick alone"
    );
}

// -- Rule 5: rebases only when mechanical.

#[tokio::test]
async fn a_dirty_pull_request_is_rebased_mechanically_or_becomes_needs_redo_dispatched_once() {
    let dirty = || Ok(reading(1, Mergeability::Conflicted, CiState::Success, 3));
    // Mechanical: rebased, and it needs fresh CI before it can merge.
    let fake = Fake::new().main(vec![main_green()]).pr("s1", vec![dirty()]);
    let r = run(&fake, &cfg(), &[session("s1", 1)], &mut BTreeMap::new(), false).await;
    assert_eq!(fake.writes(), vec!["rebase s1".to_string()]);
    let i = item(&r, "s1");
    assert_eq!(i.action, Action::Rebased);
    assert!(i.reason.contains("fresh CI"), "{}", i.reason);

    // Conflicting, redo off: needs_redo, nothing dispatched.
    let fake = Fake::new().main(vec![main_green()]).pr("s1", vec![dirty()]);
    fake.rebase.borrow_mut().insert("s1".into(), RebaseResult::Conflicted);
    let mut memory = BTreeMap::new();
    let r = run(&fake, &cfg(), &[session("s1", 1)], &mut memory, false).await;
    assert_eq!(item(&r, "s1").action, Action::NeedsRedo);
    assert!(
        memory["acme/web"]
            .needs_redo
            .contains_key("https://github.com/acme/web/pull/1")
    );
    assert!(!fake.writes().iter().any(|w| w.starts_with("dispatch")));

    // Redo on: dispatched exactly once, on this run or a later one, and never rebased again.
    let mut settings = cfg();
    settings.redo_on_conflict = true;
    let r = run(&fake, &settings, &[session("s1", 1)], &mut memory, false).await;
    assert_eq!(item(&r, "s1").action, Action::RedoDispatched);
    let r = run(&fake, &settings, &[session("s1", 1)], &mut memory, false).await;
    assert_eq!(item(&r, "s1").action, Action::NeedsRedo);
    assert!(item(&r, "s1").reason.contains("already dispatched"));
    let writes = fake.writes();
    assert_eq!(
        writes.iter().filter(|w| w.starts_with("dispatch redo s1")).count(),
        1,
        "{writes:?}"
    );
    assert_eq!(writes.iter().filter(|w| w.starts_with("rebase")).count(), 1, "{writes:?}");
}

#[test]
fn a_redo_colony_is_told_to_use_the_pull_request_as_its_reference() {
    let body = dispatch_body(&Dispatch::Redo {
        session: Box::new(session("s1", 12)),
        pr_url: "https://github.com/acme/web/pull/12".into(),
        base: "main".into(),
        why: "the mechanical rebase onto main conflicted".into(),
    });
    assert_eq!(body["allow_duplicate"], true);
    assert_eq!(body["origin"], "merge-train:redo:s1");
    assert!(
        body["instructions"]
            .as_str()
            .unwrap()
            .contains("git fetch origin pull/12/head")
    );
    let fix = dispatch_body(&Dispatch::Fix {
        repo: "acme/web".into(),
        base: "main".into(),
        merged: TrainMerge {
            pr_url: "https://github.com/acme/web/pull/3".into(),
            title: "Change 3".into(),
            sha: Some("abc".into()),
            head: None,
            at: t0(),
        },
        detail: "failed: ci".into(),
        log: "boom".into(),
    });
    let text = fix["instructions"].as_str().unwrap();
    assert!(text.contains("minimally") && text.contains("boom"), "{text}");
}

// -- Rule 6: main red after the train's own merge.

#[tokio::test]
async fn main_red_after_a_train_merge_reruns_once_then_sends_a_fix_colony_and_pauses_until_green() {
    let red = || {
        Ok(MainCi::Red {
            sha: "sha-s0".into(),
            run_ids: vec![77],
            detail: "failed: ci".into(),
        })
    };
    let mut memory = BTreeMap::new();
    memory.insert(
        "acme/web".to_string(),
        RepoMemory {
            last_train_merge: Some(TrainMerge {
                pr_url: "https://github.com/acme/web/pull/9".into(),
                title: "Change 9".into(),
                sha: Some("sha-s0".into()),
                head: None,
                at: t0(),
            }),
            ..RepoMemory::default()
        },
    );
    let sessions = [session("s1", 1)];

    // Self-heal off (the default): paused, nothing re-run or sent.
    let fake = Fake::new().main(vec![red()]).pr("s1", vec![green(1)]);
    let mut off = memory.clone();
    let r = run(&fake, &cfg(), &sessions, &mut off, false).await;
    assert!(fake.writes().is_empty(), "{:?}", fake.writes());
    assert!(
        off["acme/web"]
            .paused
            .as_deref()
            .unwrap()
            .contains("went red after the train merged")
    );
    assert!(r.repos[0].heal[0].contains("self-heal is off"));

    let mut settings = cfg();
    settings.self_heal = true;
    // Run 1: the failed jobs are re-run once — maybe a flake.
    let fake = Fake::new().main(vec![red()]).pr("s1", vec![green(1)]);
    let r = run(&fake, &settings, &sessions, &mut memory, false).await;
    assert_eq!(fake.writes(), vec!["rerun 77".to_string()]);
    assert!(r.repos[0].paused.is_some(), "the train pauses");
    assert_eq!(item(&r, "s1").action, Action::Waiting);
    // Run 2: still red — a fix colony, handed the failure log.
    let r = run(&fake, &settings, &sessions, &mut memory, false).await;
    assert_eq!(
        fake.writes(),
        vec![
            "rerun 77".to_string(),
            "log 77".to_string(),
            "dispatch fix (error[E0308]: mismatched types)".to_string()
        ]
    );
    assert!(r.repos[0].heal.iter().any(|h| h.contains("dispatched fix colony")));
    // Run 3: still red — nothing new; still paused.
    let r = run(&fake, &settings, &sessions, &mut memory, false).await;
    assert_eq!(fake.writes().len(), 3);
    assert!(r.repos[0].paused.is_some());
    // Main green again: the train resumes and merges.
    let fake = Fake::new().main(vec![main_green()]).pr("s1", vec![green(1)]);
    let r = run(&fake, &settings, &sessions, &mut memory, false).await;
    assert_eq!(r.repos[0].paused, None);
    assert!(r.repos[0].heal[0].contains("green again"));
    assert_eq!(item(&r, "s1").action, Action::Merged);
}

#[tokio::test]
async fn revert_on_red_reverts_only_the_train_s_own_merge() {
    let mut settings = cfg();
    settings.self_heal = true;
    settings.revert_on_red = true;
    let mut memory = BTreeMap::new();
    memory.insert(
        "acme/web".to_string(),
        RepoMemory {
            reran_main: Some("sha-s0".into()),
            last_train_merge: Some(TrainMerge {
                pr_url: "https://github.com/acme/web/pull/9".into(),
                title: "Change 9".into(),
                sha: Some("sha-s0".into()),
                head: None,
                at: t0(),
            }),
            ..RepoMemory::default()
        },
    );
    let red = |sha: &str| {
        Ok(MainCi::Red {
            sha: sha.into(),
            run_ids: vec![77],
            detail: "failed: ci".into(),
        })
    };
    let fake = Fake::new().main(vec![red("sha-s0")]).pr("s1", vec![green(1)]);
    run(&fake, &settings, &[session("s1", 1)], &mut memory, false).await;
    assert_eq!(fake.writes(), vec!["dispatch revert sha-s0".to_string()]);
    // Someone else's red commit on top: never reverted.
    let fake = Fake::new().main(vec![red("someone-else")]).pr("s1", vec![green(1)]);
    memory.get_mut("acme/web").unwrap().healed = None;
    run(&fake, &settings, &[session("s1", 1)], &mut memory, false).await;
    assert!(fake.writes().is_empty());
}

// -- Rule 7: a red pull request.

#[tokio::test]
async fn a_red_pull_request_is_rerun_once_only_when_every_failure_is_known_flaky() {
    let mut red = reading(1, Mergeability::Clean, CiState::Failure, 0);
    red.failing = failing_checks_from(&json!([
        {"name": "e2e (chromium)", "conclusion": "FAILURE", "detailsUrl": "https://github.com/acme/web/actions/runs/555/job/1"},
    ]));
    let fake = Fake::new().main(vec![main_green()]).pr("s1", vec![Ok(red.clone())]);
    // Not on the list: left red, reported.
    let r = run(&fake, &cfg(), &[session("s1", 1)], &mut BTreeMap::new(), false).await;
    assert_eq!(item(&r, "s1").action, Action::Red);
    assert!(item(&r, "s1").reason.contains("e2e (chromium)"));
    assert!(fake.writes().is_empty());
    // On the list (a prefix pattern): re-run once per head.
    let mut settings = cfg();
    settings.flaky_checks = vec!["e2e*".into()];
    let mut memory = BTreeMap::new();
    let r = run(&fake, &settings, &[session("s1", 1)], &mut memory, false).await;
    assert_eq!(item(&r, "s1").action, Action::Rerun);
    let r = run(&fake, &settings, &[session("s1", 1)], &mut memory, false).await;
    assert_eq!(item(&r, "s1").action, Action::Red);
    assert!(item(&r, "s1").reason.contains("after its one re-run"));
    assert_eq!(fake.writes(), vec!["rerun 555".to_string()]);
}

// -- Rule 3: GitHub pushing back stops the run.

#[tokio::test]
async fn a_403_or_429_stops_the_whole_run_without_retrying() {
    for error in [
        "gh: You have exceeded a secondary rate limit (HTTP 403)",
        "HTTP 429: too many requests",
        "gh: abuse detection mechanism triggered",
    ] {
        let fake = Fake::new()
            .main(vec![main_green()])
            .pr("s1", vec![Err(error.to_string())])
            .pr("s2", vec![green(2)]);
        let mut other = session("o1", 5);
        other.repo = "acme/xyz".into();
        let mut settings = cfg();
        settings.allow = vec!["acme".into()];
        let sessions = [session("s1", 1), session("s2", 2), other];
        let r = run(&fake, &settings, &sessions, &mut BTreeMap::new(), false).await;
        let stopped = r.stopped.clone().unwrap_or_default();
        assert!(stopped.contains("does not retry"), "{error}: {stopped}");
        assert!(fake.writes().is_empty(), "{error}: {:?}", fake.writes());
        assert_eq!(
            fake.log.borrow().last().map(String::as_str),
            Some("read s1"),
            "{error}: nothing after the push-back"
        );
        assert!(item(&r, "o1").reason.contains("not reached"));
        assert!(r.summary.contains("stopped"), "{}", r.summary);
    }
    // A plain failure (a 404, a timeout) is only that pull request's.
    let fake = Fake::new()
        .main(vec![main_green()])
        .pr("s1", vec![Err("HTTP 404: Not Found".to_string())])
        .pr("s2", vec![green(2)]);
    let r = run(
        &fake,
        &cfg(),
        &[session("s1", 1), session("s2", 2)],
        &mut BTreeMap::new(),
        false,
    )
    .await;
    assert_eq!(r.stopped, None);
    assert_eq!(item(&r, "s2").action, Action::Merged);
}

#[tokio::test]
async fn the_call_budget_and_the_gap_pace_every_github_call() {
    let mut settings = cfg();
    settings.max_api_calls = 20;
    settings.min_call_gap_ms = 2000;
    let fake = Fake::new().main(vec![main_green()]).pr("s1", vec![green(1)]);
    let r = run(&fake, &settings, &[session("s1", 1)], &mut BTreeMap::new(), false).await;
    // guards, branch, main, read, then the quiet-head merge's head, merge, tip and branch delete:
    // eight calls, seven gaps.
    assert_eq!(r.api_calls, 8);
    assert!(fake.now() - t0() >= ChronoDuration::seconds(14));
    settings.max_api_calls = 3;
    let fake = Fake::new().main(vec![main_green()]).pr("s1", vec![green(1)]);
    let r = run(&fake, &settings, &[session("s1", 1)], &mut BTreeMap::new(), false).await;
    assert!(r.stopped.unwrap().contains("budget"));
    assert!(fake.writes().is_empty());
}

#[test]
fn push_back_is_recognised_and_a_pull_request_number_is_not() {
    assert!(is_throttle("gh: API rate limit exceeded for user"));
    assert!(is_throttle("HTTP 403: Forbidden"));
    assert!(!is_throttle("checking https://github.com/acme/web/pull/403 failed: HTTP 404"));
}

// -- Dry runs and the kill switch.

#[tokio::test]
async fn a_dry_run_lists_what_it_would_do_and_writes_nothing() {
    let mut red = reading(3, Mergeability::Clean, CiState::Failure, 0);
    red.failing = vec![FailingCheck {
        name: "lint".into(),
        run_id: Some(8),
    }];
    let fake = Fake::new()
        .main(vec![main_green()])
        .pr("s1", vec![green(1)])
        .pr("s2", vec![green(2)])
        .pr("s3", vec![Ok(red)])
        .pr("s4", vec![Ok(reading(4, Mergeability::Conflicted, CiState::Success, 2))]);
    let mut settings = cfg();
    settings.flaky_checks = vec!["lint".into()];
    let mut memory = BTreeMap::new();
    let sessions = [session("s1", 1), session("s2", 2), session("s3", 3), session("s4", 4)];
    let r = run(&fake, &settings, &sessions, &mut memory, true).await;
    assert!(fake.writes().is_empty(), "{:?}", fake.writes());
    assert_eq!(
        fake.now(),
        t0() + ChronoDuration::seconds(r.api_calls as i64 - 1),
        "no waits beyond the call gap"
    );
    assert!(r.dry_run);
    assert_eq!(item(&r, "s1").action, Action::Merged);
    assert!(item(&r, "s1").reason.starts_with("would"));
    assert_eq!(item(&r, "s2").action, Action::Updated);
    assert_eq!(item(&r, "s3").action, Action::Rerun);
    assert_eq!(item(&r, "s4").action, Action::Rebased);
    assert!(
        r.summary.starts_with("dry run: would merge 1 · would update 1"),
        "{}",
        r.summary
    );
}

#[test]
fn the_kill_switch_forces_a_dry_run() {
    assert_eq!(effective_dry(false, false), (false, false));
    assert_eq!(effective_dry(true, false), (true, false));
    assert_eq!(effective_dry(false, true), (true, true), "forced, and said so");
    assert_eq!(effective_dry(true, true), (true, false));
}

#[tokio::test]
async fn a_real_run_under_the_kill_switch_is_recorded_as_a_forced_dry_run() {
    let _blocked = crate::authority::test_block_external_writes();
    let root = std::env::temp_dir().join(format!("colonizer-merge-loop-{}", crate::util::short_id()));
    std::fs::create_dir_all(root.join("config")).unwrap();
    let app = crate::tests::test_app(&root);
    let report = execute(&app, false).await.unwrap();
    assert!(report.dry_run && report.forced_dry_run);
    assert!(report.lines.iter().any(|l| l.contains("COLONIZER_NO_EXTERNAL_EFFECTS")));
    let saved = load(&app.cfg.config_dir).await;
    assert_eq!(saved.history.len(), 1, "the run is in the history");
    assert!(saved.history[0].forced_dry_run);
    let _ = std::fs::remove_dir_all(root);
}

// -- Rule 9: the report, and the settings.

#[tokio::test]
async fn the_report_says_merged_updated_red_redo_and_skipped_with_reasons() {
    let mut red = reading(3, Mergeability::Clean, CiState::Failure, 0);
    red.failing = vec![FailingCheck {
        name: "unit".into(),
        run_id: None,
    }];
    let mut draft = reading(4, Mergeability::Clean, CiState::Success, 0);
    draft.facts.info.is_draft = true;
    let fake = Fake::new()
        .main(vec![
            main_green(),
            Ok(MainCi::Pending {
                reason: "still running: ci".into(),
            }),
        ])
        .pr("s1", vec![green(1)])
        .pr(
            "s2",
            vec![green(2), Ok(reading(2, Mergeability::Behind, CiState::Success, 1))],
        )
        .pr("s3", vec![Ok(red)])
        .pr("s4", vec![Ok(draft)])
        .pr("s5", vec![Ok(reading(5, Mergeability::Conflicted, CiState::Success, 1))]);
    fake.after_update
        .borrow_mut()
        .insert("s2".into(), vec![reading(2, Mergeability::Clean, CiState::Pending, 0)]);
    fake.rebase.borrow_mut().insert("s5".into(), RebaseResult::Conflicted);
    let mut settings = cfg();
    settings.redo_on_conflict = true;
    let sessions = [
        session("s1", 1),
        session("s2", 2),
        session("s3", 3),
        session("s4", 4),
        session("s5", 5),
    ];
    let r = run(&fake, &settings, &sessions, &mut BTreeMap::new(), false).await;
    assert_eq!(
        r.summary, "merged 1 · updated (CI running) 1 · red 1 · redo dispatched 1 · skipped 1",
        "{:#?}",
        r.lines
    );
    let text = r.lines.join("\n");
    for expected in [
        "acme/web — main:",
        "#1 Change 1: merged — squash-merged",
        "#2 Change 2: updated (CI running) — updated onto main",
        "#3 Change 3: red — failing: unit",
        "#4 Change 4: skipped — the pull request is a draft",
        "#5 Change 5: redo dispatched — needs_redo (the mechanical rebase onto main conflicted)",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
    }
    // It round-trips through the history file.
    let back: Report = serde_json::from_value(serde_json::to_value(&r).unwrap()).unwrap();
    assert_eq!(back, r);
}

#[test]
fn the_settings_default_to_off_hourly_and_gentle_and_refuse_unsafe_values() {
    let d = Settings::default();
    assert!(!d.enabled && d.allow.is_empty() && d.never.is_empty());
    assert_eq!(d.cadence, Cadence::Interval { minutes: 60 });
    assert_eq!((d.max_merges, d.cooldown_secs, d.ci_wait_minutes), (4, 120, 20));
    assert!(!d.self_heal && !d.revert_on_red && !d.redo_on_conflict);
    // A body missing fields reads as the defaults.
    let partial: Settings = serde_json::from_value(json!({"enabled": true, "allow": ["Acme/Web"]})).unwrap();
    let tidy = normalize(partial).unwrap();
    assert_eq!(tidy.allow, vec!["acme/web".to_string()]);
    assert_eq!(tidy.max_merges, 4);
    for bad in [
        json!({"allow": ["not a repo"]}),
        json!({"max_merges": 0}),
        json!({"cooldown_secs": 1}),
        json!({"min_call_gap_ms": 0}),
        json!({"max_api_calls": 100000}),
        json!({"revert_on_red": true}),
        json!({"cadence": {"every": "interval", "minutes": 1}}),
        json!({"repo_max_merges": {"acme": 2}}),
    ] {
        let s: Settings = serde_json::from_value(bad.clone()).unwrap();
        assert!(normalize(s).is_err(), "{bad} should be refused");
    }
}

#[tokio::test]
async fn the_route_saves_the_settings_and_books_the_next_run() {
    let root = std::env::temp_dir().join(format!("colonizer-merge-loop-put-{}", crate::util::short_id()));
    std::fs::create_dir_all(root.join("config")).unwrap();
    let app = crate::tests::test_app(&root);
    let body = Settings {
        enabled: true,
        allow: vec!["acme/web".into()],
        ..Settings::default()
    };
    let Json(v) = put_loop(State(app.clone()), Json(body)).await.unwrap();
    assert_eq!(v["settings"]["enabled"], true);
    assert!(v["next_run_at"].is_string(), "switching it on books the next run");
    let off = Settings::default();
    let Json(v) = put_loop(State(app.clone()), Json(off)).await.unwrap();
    assert!(v["next_run_at"].is_null());
    let err = put_loop(
        State(app.clone()),
        Json(Settings {
            max_merges: 0,
            ..Settings::default()
        }),
    )
    .await
    .unwrap_err();
    assert!(err.message().contains("max_merges"));
    let _ = std::fs::remove_dir_all(root);
}
