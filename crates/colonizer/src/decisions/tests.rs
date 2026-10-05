use super::*;
use crate::merge_loop::{Item, RepoMemory, RepoReport, Report, Resolving};
use crate::sessions::tests::colony;

// ---------------------------------------------------------------------------------------------
// A fake GitHub.
// ---------------------------------------------------------------------------------------------

struct FakeGh {
    /// The search answer, or the error it fails with.
    search: Mutex<Result<Value, String>>,
    /// `owner/repo#n` → the latest comment.
    comments: HashMap<String, String>,
    calls: Mutex<Vec<String>>,
    fail_label: bool,
}

impl Default for FakeGh {
    fn default() -> Self {
        FakeGh {
            search: Mutex::new(Ok(json!({ "items": [] }))),
            comments: HashMap::new(),
            calls: Mutex::new(Vec::new()),
            fail_label: false,
        }
    }
}

impl FakeGh {
    fn with_items(items: Value) -> Self {
        FakeGh {
            search: Mutex::new(Ok(json!({ "items": items }))),
            ..FakeGh::default()
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn count(&self, prefix: &str) -> usize {
        self.calls().iter().filter(|c| c.starts_with(prefix)).count()
    }
}

impl Gh for FakeGh {
    async fn search(&self, query: &str) -> Result<Value, String> {
        self.calls.lock().unwrap().push(format!("search {query}"));
        self.search.lock().unwrap().clone()
    }

    async fn latest_comment(&self, repo: &str, number: u64, count: u64) -> Result<Option<String>, String> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("comments {repo}#{number} page {count}"));
        Ok(self.comments.get(&format!("{repo}#{number}")).cloned())
    }

    async fn comment(&self, repo: &str, number: u64, body: &str) -> Result<(), String> {
        self.calls.lock().unwrap().push(format!("comment {repo}#{number} {body}"));
        Ok(())
    }

    async fn remove_label(&self, repo: &str, number: u64, label: &str) -> Result<(), String> {
        self.calls.lock().unwrap().push(format!("unlabel {repo}#{number} {label}"));
        if self.fail_label { Err("HTTP 500".into()) } else { Ok(()) }
    }

    async fn pause(&self) {}
}

fn issue(repo: &str, number: u64, labels: &[&str], body: &str, comments: u64, updated: &str) -> Value {
    json!({
        "number": number,
        "title": format!("Issue {number}"),
        "html_url": format!("https://github.com/{repo}/issues/{number}"),
        "repository_url": format!("https://api.github.com/repos/{repo}"),
        "labels": labels.iter().map(|l| json!({"name": l})).collect::<Vec<_>>(),
        "body": body,
        "comments": comments,
        "updated_at": updated,
        "user": {"login": "maintainer"},
    })
}

fn review(repo: &str, number: u64) -> Value {
    let mut item = issue(repo, number, &[], "", 0, "2026-10-05T00:00:00Z");
    item["pull_request"] = json!({ "html_url": format!("https://github.com/{repo}/pull/{number}") });
    item["user"] = json!({"login": "alice"});
    item
}

fn at(minutes: i64) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-05T08:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
        + Duration::minutes(minutes)
}

fn test_root(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("colonizer-decisions-{name}-{}", uuid::Uuid::new_v4()))
}

// ---------------------------------------------------------------------------------------------
// Parsing.
// ---------------------------------------------------------------------------------------------

#[test]
fn a_marker_line_is_the_question_and_the_options_list_its_choices() {
    let body = "Some context.\n\nOpen decision: Should the cache live on disk or in memory?\n\nOptions:\n- On disk\n- In memory\n- Both, disk behind memory\n\nMore prose after.";
    let p = parse_decision(body).unwrap();
    assert_eq!(p.question, "Should the cache live on disk or in memory?");
    assert_eq!(p.options, ["On disk", "In memory", "Both, disk behind memory"]);
    assert_eq!(p.more, 0);
}

#[test]
fn decorated_markers_numbered_options_and_a_question_on_the_next_line_parse() {
    let p = parse_decision("**Decision needed:** Which licence?\n**Options:**\n1. MIT\n2) Apache-2.0\n* [ ] GPL").unwrap();
    assert_eq!(p.question, "Which licence?");
    assert_eq!(p.options, ["MIT", "Apache-2.0", "GPL"]);

    let p = parse_decision("> ### Open decision:\n> Ship it on Friday?\n").unwrap();
    assert_eq!(p.question, "Ship it on Friday?");
    assert!(p.options.is_empty(), "no Options: line, so the card offers free text");
}

#[test]
fn bullets_without_an_options_line_are_not_options_and_further_markers_are_counted() {
    let p = parse_decision("Open decision: name?\n- foo\n- bar\n\nOpen decision: colour?\nOptions:\n- red").unwrap();
    assert_eq!(p.question, "name?");
    assert!(p.options.is_empty(), "the first question's list has no Options: line");
    assert_eq!(p.more, 1);
}

#[test]
fn text_without_a_marker_asks_nothing() {
    assert_eq!(parse_decision("We decided: open decision is not a marker mid-line."), None);
    assert_eq!(parse_decision(""), None);
    assert_eq!(
        parse_decision("Open decision:\n\n"),
        None,
        "a marker with no question is not a card"
    );
}

#[test]
fn an_answer_comment_is_recognised_and_built_on_one_line_with_the_note_below() {
    assert!(is_answer("Decision (maintainer): On disk"));
    assert!(is_answer("  decision (MAINTAINER): x"));
    assert!(!is_answer("I think Decision (maintainer): x"));
    assert_eq!(answer_body("On  disk\n", None), "Decision (maintainer): On disk");
    assert_eq!(
        answer_body("On disk", Some(" Memory is too small. ")),
        "Decision (maintainer): On disk\n\nMemory is too small."
    );
}

#[test]
fn a_card_comes_from_the_label_the_latest_comment_or_an_unanswered_body() {
    let labelled = issue("acme/web", 1, &["Needs-Decision"], "No marker here.", 0, "t1");
    let card = decision_from_item(&labelled, None, false).unwrap();
    assert_eq!(
        (card.source, card.labelled, card.question.as_str()),
        ("label", true, "Issue 1")
    );
    assert_eq!(card.id, "acme/web#1");
    assert_eq!(card.org, "acme");

    let body = issue("acme/web", 2, &[], "Open decision: A or B?\nOptions:\n- A\n- B", 1, "t1");
    let card = decision_from_item(&body, Some("just a comment"), false).unwrap();
    assert_eq!((card.source, card.options.len()), ("body", 2));
    // Answered on GitHub, or here: the body's marker no longer counts.
    assert_eq!(decision_from_item(&body, Some("Decision (maintainer): A"), false), None);
    assert_eq!(decision_from_item(&body, None, true), None);

    // A newer question in the latest comment wins over the body's.
    let card = decision_from_item(&body, Some("Decision needed: C or D?\nOptions:\n- C\n- D"), false).unwrap();
    assert_eq!((card.source, card.question.as_str()), ("comment", "C or D?"));

    // The label holds even after an answer comment someone else wrote…
    let card = decision_from_item(&labelled, Some("Decision (maintainer): x"), false).unwrap();
    assert_eq!(card.source, "label");
    // …but not after the operator's own answer whose label would not come off: no second answer.
    assert_eq!(decision_from_item(&labelled, Some("Decision (maintainer): x"), true), None);
    // A new question after it brings the card back.
    assert!(decision_from_item(&labelled, Some("Open decision: and now?"), true).is_some());
}

// ---------------------------------------------------------------------------------------------
// Answering through a fake GitHub.
// ---------------------------------------------------------------------------------------------

fn card(labelled: bool) -> DecisionCard {
    decision_from_item(
        &issue(
            "acme/web",
            7,
            if labelled { &["needs-decision"] } else { &[] },
            "Open decision: A or B?\nOptions:\n- A\n- B",
            0,
            "t",
        ),
        None,
        false,
    )
    .unwrap()
}

#[tokio::test]
async fn an_answer_posts_exactly_one_comment_and_removes_the_label() {
    let gh = FakeGh::default();
    let answered = answer_with(&gh, &card(true), "A", Some("because")).await.unwrap();
    assert_eq!(
        gh.calls(),
        [
            "comment acme/web#7 Decision (maintainer): A\n\nbecause".to_string(),
            "unlabel acme/web#7 needs-decision".to_string(),
        ]
    );
    assert!(answered.label_removed);

    // No label: one comment and nothing else.
    let gh = FakeGh::default();
    answer_with(&gh, &card(false), "B", None).await.unwrap();
    assert_eq!(gh.calls(), ["comment acme/web#7 Decision (maintainer): B".to_string()]);

    // A label that will not come off is reported, not retried: the comment stands.
    let gh = FakeGh {
        fail_label: true,
        ..FakeGh::default()
    };
    let answered = answer_with(&gh, &card(true), "A", None).await.unwrap();
    assert_eq!(gh.count("comment "), 1);
    assert!(!answered.label_removed);
    assert!(answered.label_error.is_some());
}

#[tokio::test]
async fn the_kill_switch_posts_nothing() {
    let _blocked = crate::authority::test_block_external_writes();
    let gh = FakeGh::default();
    assert!(answer_with(&gh, &card(true), "A", None).await.is_err());
    assert!(gh.calls().is_empty());
}

/// An App whose `acme` org has a colony, so it is opted in by default, with the search cache seeded.
async fn app_with_cards(name: &str, items: Value) -> (PathBuf, Shared) {
    let root = test_root(name);
    let app = crate::tests::test_app(&root);
    let mut s = colony("acme", SessionStatus::Running);
    s.id = "c-acme".into();
    app.sessions.write().await.push(s);
    let gh = FakeGh::with_items(items);
    tick(&app, &gh, at(0)).await;
    (root, app)
}

#[tokio::test]
async fn answering_through_the_route_posts_once_then_the_card_is_gone() {
    let (root, app) = app_with_cards(
        "answer",
        json!([issue(
            "acme/web",
            7,
            &["needs-decision"],
            "Open decision: A or B?\nOptions:\n- A\n- B",
            0,
            "t"
        )]),
    )
    .await;
    assert_eq!(cards(&app).await.decisions.len(), 1);
    let gh = FakeGh::default();
    let body = |choice: &str| AnswerBody {
        id: "acme/web#7".into(),
        choice: choice.into(),
        note: None,
    };
    let reply = answer_on(&app, &gh, body("A")).await.unwrap();
    assert_eq!(reply["label_removed"], true);
    assert_eq!(gh.count("comment "), 1);
    assert_eq!(gh.count("unlabel "), 1);
    // Answered: the card is gone, and a second click finds nothing to post on.
    assert!(cards(&app).await.decisions.is_empty());
    let err = answer_on(&app, &gh, body("A")).await.unwrap_err();
    assert_eq!(err.0, StatusCode::NOT_FOUND);
    assert_eq!(gh.count("comment "), 1);
    assert!(load(&app).answered.contains_key("acme/web#7"));
    // An empty answer is refused before anything reaches GitHub.
    let err = answer_on(&app, &gh, body("  ")).await.unwrap_err();
    assert_eq!(err.0, StatusCode::BAD_REQUEST);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn the_route_refuses_while_writes_are_blocked() {
    let (root, app) = app_with_cards("blocked", json!([issue("acme/web", 7, &["needs-decision"], "", 0, "t")])).await;
    let _blocked = crate::authority::test_block_external_writes();
    let gh = FakeGh::default();
    let err = answer_on(
        &app,
        &gh,
        AnswerBody {
            id: "acme/web#7".into(),
            choice: "A".into(),
            note: None,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.0, StatusCode::CONFLICT);
    assert!(gh.calls().is_empty());
    assert_eq!(view(&app).await["writes_blocked"], true);
    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------------------------
// Polling.
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn one_search_per_org_and_a_comment_is_read_only_when_its_issue_changed() {
    let items = json!([
        issue("acme/web", 1, &["needs-decision"], "", 3, "u1"),
        issue("acme/web", 2, &[], "Open decision: X?", 0, "u1"),
        review("acme/api", 9),
    ]);
    let gh = FakeGh::with_items(items.clone());
    let first = poll_org(&gh, "acme", &OrgData::default(), &BTreeMap::new(), at(0))
        .await
        .unwrap();
    assert_eq!(gh.count("search "), 1);
    assert_eq!(gh.calls()[0], format!("search {}", search_query("acme")));
    assert_eq!(gh.count("comments "), 1, "only the issue with comments is read");
    assert_eq!(gh.calls()[1], "comments acme/web#1 page 3");
    assert_eq!(first.decisions.len(), 2);
    assert_eq!(first.reviews.len(), 1);
    assert_eq!(first.reviews[0].reason, PrReason::ReviewRequested);

    // Unchanged: the comment is not read again.
    let gh = FakeGh::with_items(items);
    poll_org(&gh, "acme", &first, &BTreeMap::new(), at(5)).await.unwrap();
    assert_eq!(gh.count("comments "), 0);

    // Changed: read again.
    let gh = FakeGh::with_items(json!([issue("acme/web", 1, &["needs-decision"], "", 4, "u2")]));
    poll_org(&gh, "acme", &first, &BTreeMap::new(), at(10)).await.unwrap();
    assert_eq!(gh.count("comments "), 1);
}

#[tokio::test]
async fn a_poll_reads_at_most_ten_comments() {
    let items: Vec<Value> = (1..=14)
        .map(|n| issue("acme/web", n, &["needs-decision"], "", 1, "u"))
        .collect();
    let gh = FakeGh::with_items(json!(items));
    let data = poll_org(&gh, "acme", &OrgData::default(), &BTreeMap::new(), at(0))
        .await
        .unwrap();
    assert_eq!(gh.count("comments "), MAX_COMMENT_READS);
    assert_eq!(
        data.decisions.len(),
        14,
        "a labelled issue is a card even before its comment is read"
    );
}

#[test]
fn each_org_is_searched_at_most_every_five_minutes_one_at_a_time() {
    let mut gate = Gate::default();
    let orgs = vec!["acme".to_string(), "beta".to_string()];
    assert_eq!(gate.next(&orgs, at(0)).as_deref(), Some("acme"));
    gate.succeeded("acme", at(0));
    assert_eq!(gate.next(&orgs, at(1)).as_deref(), Some("beta"));
    gate.succeeded("beta", at(1));
    assert_eq!(gate.next(&orgs, at(4)), None);
    assert!(!gate.due("acme", at(4)));
    assert!(gate.due("acme", at(5)));
    assert_eq!(
        gate.next(&orgs, at(6)).as_deref(),
        Some("acme"),
        "the one searched longest ago first"
    );
}

#[test]
fn a_failing_org_waits_longer_and_a_push_back_pauses_every_org_with_a_doubling_backoff() {
    let mut gate = Gate::default();
    gate.failed("acme", at(0));
    assert!(!gate.due("acme", at(9)));
    assert!(gate.due("acme", at(10)), "one failure doubles five minutes");
    gate.failed("acme", at(10));
    assert!(!gate.due("acme", at(29)));
    assert!(gate.due("acme", at(30)));
    gate.succeeded("acme", at(30));
    assert!(gate.due("acme", at(35)), "success resets the wait");

    let mut gate = Gate::default();
    assert_eq!(gate.throttled("acme", at(0)), at(15));
    assert!(!gate.due("beta", at(14)), "a push-back pauses every org");
    assert!(gate.due("beta", at(15)));
    assert_eq!(gate.throttled("beta", at(15)), at(45));
    assert_eq!(gate.throttled("beta", at(45)), at(105));
    for _ in 0..10 {
        gate.throttled("beta", at(0));
    }
    assert_eq!(
        gate.paused_until(at(0)),
        Some(at(BACKOFF_MAX_MINUTES)),
        "capped at four hours"
    );
    gate.succeeded("beta", at(300));
    assert_eq!(gate.throttled("beta", at(300)), at(315), "a success resets the strikes");
}

#[tokio::test]
async fn a_rate_limited_search_pauses_the_poll() {
    let root = test_root("throttle");
    let app = crate::tests::test_app(&root);
    let mut s = colony("acme", SessionStatus::Running);
    s.id = "c1".into();
    app.sessions.write().await.push(s);
    let gh = FakeGh::default();
    *gh.search.lock().unwrap() = Err("HTTP 403: You have exceeded a secondary rate limit".into());
    tick(&app, &gh, at(0)).await;
    assert_eq!(gh.count("search "), 1);
    // Paused: later ticks inside the pause make no request at all.
    tick(&app, &gh, at(6)).await;
    tick(&app, &gh, at(14)).await;
    assert_eq!(gh.count("search "), 1);
    tick(&app, &gh, at(15)).await;
    assert_eq!(gh.count("search "), 2);
    assert!(view(&app).await["orgs"][0]["error"].as_str().unwrap().contains("pushed back"));
    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------------------------
// Opt-in and the count.
// ---------------------------------------------------------------------------------------------

#[test]
fn an_org_is_on_by_default_only_with_colonies_and_switched_on_and_the_operator_overrides_either_way() {
    let set = |xs: &[&str]| xs.iter().map(|x| x.to_string()).collect::<BTreeSet<String>>();
    let mut stored = Stored::default();
    stored.orgs.insert("gamma".into(), true);
    stored.orgs.insert("acme".into(), false);
    let list = opt_ins(
        &stored,
        &set(&["acme", "beta", "Delta"]),
        &set(&["acme", "beta", "omega"]),
        &set(&["omega"]),
    );
    let get = |o: &str| list.iter().find(|x| x.org == o).unwrap().clone();
    assert_eq!(
        (get("acme").enabled, get("acme").default_on, get("acme").explicit),
        (false, true, true)
    );
    assert_eq!((get("beta").enabled, get("beta").explicit), (true, false));
    assert!(!get("delta").enabled, "known, but no colonies yet");
    assert!(!get("omega").enabled, "switched off as a workspace");
    assert!(get("gamma").enabled, "opted in by hand");
}

#[tokio::test]
async fn an_opted_out_org_is_not_searched_and_its_cards_leave_the_count() {
    let (root, app) = app_with_cards(
        "optout",
        json!([issue("acme/web", 1, &["needs-decision"], "", 0, "t"), review("acme/api", 4)]),
    )
    .await;
    assert_eq!(view(&app).await["count"], 2);
    let _ = put_org(
        State(app.clone()),
        Path("acme".into()),
        Json(OrgBody { enabled: Some(false) }),
    )
    .await
    .unwrap();
    let v = view(&app).await;
    assert_eq!(v["count"], 0);
    assert_eq!(v["orgs"][0]["enabled"], false);
    let gh = FakeGh::with_items(json!([]));
    tick(&app, &gh, at(60)).await;
    assert!(gh.calls().is_empty(), "an opted-out org is never searched");
    // Back to the default.
    let _ = put_org(State(app.clone()), Path("acme".into()), Json(OrgBody { enabled: None }))
        .await
        .unwrap();
    assert_eq!(view(&app).await["count"], 2);
    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------------------------
// Why a pull request needs a person.
// ---------------------------------------------------------------------------------------------

fn pr_colony(id: &str, n: u64) -> Session {
    let mut s = colony("acme", SessionStatus::PrOpened);
    s.id = id.into();
    s.repo = "acme/web".into();
    s.issue_title = format!("Colony {id}");
    s.pr_url = Some(format!("https://github.com/acme/web/pull/{n}"));
    s
}

fn reasons(
    sessions: &[Session],
    state: &LoopState,
    driven: &[&str],
    secret: &[&str],
    reviews: &[PrCard],
) -> Vec<(String, PrReason, Vec<&'static str>)> {
    let driven: BTreeSet<String> = driven.iter().map(|x| x.to_string()).collect();
    let secret: HashSet<String> = secret.iter().map(|x| x.to_string()).collect();
    let orgs: BTreeSet<String> = ["acme".to_string()].into();
    pr_cards(
        sessions,
        &PrInputs {
            loop_state: state,
            driven: &driven,
            secret_held: &secret,
            orgs: &orgs,
        },
        reviews,
    )
    .into_iter()
    .map(|c| (c.colony.unwrap_or(c.id), c.reason, c.actions))
    .collect()
}

#[test]
fn each_pull_request_card_says_why_it_needs_a_person() {
    let url = |n: u64| format!("https://github.com/acme/web/pull/{n}");
    let mut redo = pr_colony("redo", 1);
    redo.ci_state = Some(github::CiState::Failure);
    let mut gave_up = pr_colony("gaveup", 2);
    gave_up.ci_state = Some(github::CiState::Success);
    let mut conflicted = pr_colony("conflicted", 3);
    conflicted.needs_rebase = true;
    let mut red = pr_colony("red", 4);
    red.ci_state = Some(github::CiState::Failure);
    let mut flaky = pr_colony("flaky", 5);
    flaky.ci_state = Some(github::CiState::Failure);
    let mut green = pr_colony("green", 6);
    green.ci_state = Some(github::CiState::Success);
    let mut pending = pr_colony("pending", 7);
    pending.ci_state = Some(github::CiState::Pending);
    let mut defeat = colony("acme", SessionStatus::Idle);
    defeat.id = "defeat".into();
    defeat.attention = Some(json!({"reason": "control_defeat", "detail": "kept retrying a masked path"}));
    let mut secret = colony("acme", SessionStatus::Idle);
    secret.id = "secret".into();
    secret.attention = Some(json!({"reason": "autopilot_held"}));
    let mut plain_hold = colony("acme", SessionStatus::Idle);
    plain_hold.id = "plain".into();
    plain_hold.attention = Some(json!({"reason": "autopilot_held"}));
    let mut refused = colony("acme", SessionStatus::Stopped);
    refused.id = "refused".into();
    refused.error = Some("refusing to open a pull request: not authorized (no grant)".into());
    let mut other_org = pr_colony("other", 8);
    other_org.org = "beta".into();
    other_org.ci_state = Some(github::CiState::Failure);
    let mut redone = pr_colony("redone", 9);
    redone.needs_rebase = true;

    let mut state = LoopState::default();
    let memory = RepoMemory {
        needs_redo: [(url(1), "conflict in src/lib.rs".to_string())].into(),
        resolving: [(
            url(2),
            Resolving {
                gave_up: Some("the conflict is a design choice".into()),
                ..Resolving::default()
            },
        )]
        .into(),
        redo_dispatched: [url(9)].into(),
        ..RepoMemory::default()
    };
    state.repos.insert("acme/web".into(), memory);
    state.history.push(Report {
        repos: vec![RepoReport {
            repo: "acme/web".into(),
            items: vec![
                Item {
                    session: "red".into(),
                    pr_url: url(4),
                    title: String::new(),
                    action: Action::Red,
                    reason: "checks failing: test (ubuntu)".into(),
                },
                Item {
                    session: "flaky".into(),
                    pr_url: url(5),
                    title: String::new(),
                    action: Action::Rerun,
                    reason: "re-ran e2e".into(),
                },
            ],
            ..RepoReport::default()
        }],
        ..Report::default()
    });
    let review_card = PrCard {
        id: "https://github.com/acme/api/pull/3".into(),
        org: "acme".into(),
        repo: "acme/api".into(),
        number: Some(3),
        title: "Docs".into(),
        url: Some("https://github.com/acme/api/pull/3".into()),
        colony: None,
        reason: PrReason::ReviewRequested,
        why: "alice asked you for a review".into(),
        actions: Vec::new(),
    };
    let sessions = vec![
        redo, gave_up, conflicted, red, flaky, green, pending, defeat, secret, plain_hold, refused, other_org, redone,
    ];
    let got = reasons(&sessions, &state, &[], &["secret"], std::slice::from_ref(&review_card));
    let want: Vec<(String, PrReason, Vec<&'static str>)> = vec![
        ("defeat".into(), PrReason::PolicyHold, vec![]),
        ("secret".into(), PrReason::PolicyHold, vec![]),
        ("refused".into(), PrReason::PolicyHold, vec![]),
        ("redo".into(), PrReason::NeedsRedo, vec!["redo"]),
        ("gaveup".into(), PrReason::NeedsRedo, vec!["redo"]),
        ("conflicted".into(), PrReason::Conflicted, vec!["redo"]),
        ("red".into(), PrReason::RedCi, vec!["rerun"]),
        (review_card.id.clone(), PrReason::ReviewRequested, vec![]),
        ("green".into(), PrReason::AwaitingMerge, vec![]),
    ];
    assert_eq!(got, want);

    // The why names the merge loop's own words.
    let driven: BTreeSet<String> = BTreeSet::new();
    let orgs: BTreeSet<String> = ["acme".to_string()].into();
    let cards = pr_cards(
        &sessions,
        &PrInputs {
            loop_state: &state,
            driven: &driven,
            secret_held: &HashSet::new(),
            orgs: &orgs,
        },
        &[],
    );
    let why = |id: &str| cards.iter().find(|c| c.colony.as_deref() == Some(id)).unwrap().why.clone();
    assert!(why("red").contains("checks failing: test (ubuntu)"));
    assert!(why("redo").contains("conflict in src/lib.rs"));
    assert!(why("defeat").contains("kept retrying a masked path"));
    assert!(why("refused").starts_with("the publish was refused"));

    // A repository the merge train drives merges its green pull requests itself.
    let got = reasons(&sessions, &state, &["acme/web"], &[], &[]);
    assert!(!got.iter().any(|(_, r, _)| *r == PrReason::AwaitingMerge));
}

#[test]
fn commits_not_merged_after_a_train_merge_lead_the_inbox_until_dismissed() {
    // Issue #1075: the colony is merged, so the card comes from the train's record alone.
    let mut merged = pr_colony("merged", 9);
    merged.status = SessionStatus::Merged;
    let mut red = pr_colony("red", 4);
    red.ci_state = Some(github::CiState::Failure);
    let mut state = LoopState::default();
    let url = "https://github.com/acme/web/pull/9".to_string();
    state.commits_not_merged.insert(
        url.clone(),
        crate::merge_head::Unmerged {
            pr_url: url.clone(),
            repo: "acme/web".into(),
            title: "Change 9".into(),
            colony: Some("merged".into()),
            merged_head: "aaaaaaaa1111".into(),
            tip: "bbbbbbbb2222".into(),
            at: None,
            by: "merge train".into(),
        },
    );
    // Another org's record stays out of an inbox not opted in to it.
    state.commits_not_merged.insert(
        "https://github.com/other/x/pull/1".into(),
        crate::merge_head::Unmerged {
            pr_url: "https://github.com/other/x/pull/1".into(),
            repo: "other/x".into(),
            ..Default::default()
        },
    );
    let got = reasons(&[merged.clone(), red.clone()], &state, &[], &[], &[]);
    assert_eq!(
        got,
        vec![
            ("merged".to_string(), PrReason::CommitsNotMerged, vec!["dismiss"]),
            ("red".to_string(), PrReason::RedCi, vec!["rerun"]),
        ]
    );
    state.commits_not_merged.clear();
    assert_eq!(reasons(&[merged, red], &state, &[], &[], &[]).len(), 1);
}
