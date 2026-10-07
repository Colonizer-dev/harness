//! The Nest's frontier badge (issue #1144): how many open issues wait in the repositories the
//! operator colonizes, served from `GET /api/status` as `backlog: {issues, repos, as_of, by_org}`.
//!
//! GitHub's `open_issues_count` (what the badge used to sum over `/user/repos`) counts open pull
//! requests too, and it is summed over forks, archived repositories and repositories with issues
//! switched off, and over every org the token can see. This counts **issues only**, with the search
//! API's `is:issue is:open` once per org, over the orgs Colonizer works in — the workspaces: orgs
//! it has seen on the account or saved settings for, or that have colonies, minus the ones awaiting
//! the operator's answer or switched off. The numbers are cached ten minutes and refreshed behind
//! the status poll, which never waits for GitHub; the search API allows 30 calls a minute, and a
//! refresh costs one call per org (plus one per few hundred characters of excluded repository names).

use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::{App, Shared, orgs::OrgSettings, util::exec_within};

/// A refresh that succeeded serves the status poll for this long.
const TTL: Duration = Duration::from_secs(10 * 60);
/// After a refresh that got nothing, the next attempt waits this long.
const FAILURE_TTL: Duration = Duration::from_secs(60);
/// One `gh api search/issues` call.
const SEARCH_LIMIT: Duration = Duration::from_secs(20);
/// GitHub refuses search queries over 256 characters; the base query and a margin come off that.
const QUERY_BUDGET: usize = 200;

/// One org's share of the badge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OrgBacklog {
    pub issues: u64,
    pub repos: u64,
}

/// The badge's numbers as of one refresh.
#[derive(Clone, Debug)]
pub(crate) struct Backlog {
    pub by_org: BTreeMap<String, OrgBacklog>,
    pub as_of: DateTime<Utc>,
}

impl Backlog {
    /// `{issues, repos, as_of, by_org}`: the totals over every org counted, and each org's own for
    /// the cockpit's selected workspace.
    pub(crate) fn to_json(&self) -> Value {
        let issues: u64 = self.by_org.values().map(|o| o.issues).sum();
        let repos: u64 = self.by_org.values().map(|o| o.repos).sum();
        let by_org: serde_json::Map<String, Value> = self
            .by_org
            .iter()
            .map(|(org, o)| (org.clone(), json!({"issues": o.issues, "repos": o.repos})))
            .collect();
        json!({"issues": issues, "repos": repos, "as_of": self.as_of, "by_org": by_org})
    }
}

#[derive(Default)]
struct State {
    last: Option<Backlog>,
    refreshed: Option<Instant>,
    failed: Option<Instant>,
    running: bool,
}

/// The cached badge numbers and the refresh bookkeeping, held on the [`App`].
#[derive(Default)]
pub struct Cache(Mutex<State>);

/// Whether a refresh is due: neither a recent success nor a recent failure holds it off.
fn refresh_due(refreshed: Option<Instant>, failed: Option<Instant>, now: Instant) -> bool {
    let within = |at: Option<Instant>, ttl: Duration| at.is_some_and(|at| now.duration_since(at) < ttl);
    !within(refreshed, TTL) && !within(failed, FAILURE_TTL)
}

/// The `backlog` value of `/api/status`: the last numbers, or `null` before the first refresh has
/// finished. Starts a refresh behind the answer when one is due; never waits for GitHub.
pub(crate) async fn status_json(app: &Shared) -> Value {
    let mut state = app.backlog.0.lock().await;
    if !state.running && refresh_due(state.refreshed, state.failed, Instant::now()) {
        state.running = true;
        let app = app.clone();
        tokio::spawn(async move {
            let fresh = refresh(&app).await;
            let mut state = app.backlog.0.lock().await;
            state.running = false;
            match fresh {
                Ok(backlog) => {
                    state.last = Some(backlog);
                    state.refreshed = Some(Instant::now());
                }
                Err(e) => {
                    eprintln!("backlog: could not count open issues, retrying in a minute: {e:#}");
                    state.failed = Some(Instant::now());
                }
            }
        });
    }
    state.last.as_ref().map_or(Value::Null, Backlog::to_json)
}

/// The orgs the badge counts in the all-workspaces view: the ones Colonizer works in. `known` is
/// the record of orgs seen on the account, `saved` the orgs with settings of their own, `colony_orgs`
/// the orgs with colonies. An org awaiting the operator's answer is not one yet, and a switched-off
/// org is not one any more. Orgs the token merely sees — collaborator access to someone's
/// repository — are in none of these, which is the point.
pub(crate) fn scope_orgs<'a>(
    known: impl IntoIterator<Item = &'a str>,
    saved: &'a BTreeMap<String, OrgSettings>,
    colony_orgs: impl IntoIterator<Item = &'a str>,
    awaiting: &'a BTreeSet<String>,
) -> BTreeSet<String> {
    known
        .into_iter()
        .chain(saved.keys().map(String::as_str))
        .chain(colony_orgs)
        .filter(|org| !org.is_empty())
        .filter(|org| !awaiting.contains(*org) || saved.contains_key(*org))
        .filter(|org| saved.get(*org).is_none_or(crate::orgs::org_enabled))
        .map(String::from)
        .collect()
}

/// What the repository list says about one owner.
#[derive(Debug, Default, PartialEq, Eq)]
struct OwnerRepos {
    /// Repositories that count: not a fork, not archived, issues on.
    counted: u64,
    /// Repositories whose open issues the org-wide search would add but must not: forks and
    /// repositories with issues switched off that still report open items. Archived ones are left
    /// out of the search itself (`archived:false`).
    excluded: Vec<String>,
}

fn owner_repos(repos: &[Value], owner: &str) -> OwnerRepos {
    let mut out = OwnerRepos::default();
    for repo in repos {
        let Some(full_name) = repo["full_name"].as_str() else {
            continue;
        };
        if !full_name.split('/').next().is_some_and(|o| o.eq_ignore_ascii_case(owner)) {
            continue;
        }
        if repo["archived"].as_bool() == Some(true) {
            continue;
        }
        let issues_off = repo["has_issues"].as_bool() == Some(false);
        if repo["fork"].as_bool() == Some(true) || issues_off {
            if repo["open_issues_count"].as_u64().unwrap_or(0) > 0 {
                out.excluded.push(full_name.to_string());
            }
        } else {
            out.counted += 1;
        }
    }
    out
}

/// A source of `total_count`s for issue-search queries. A trait so the counting is tested against a
/// fake GitHub.
pub(crate) trait IssueSearch {
    fn total(&self, query: &str) -> impl Future<Output = Result<u64>> + Send;
}

struct Gh<'a>(&'a App);

impl IssueSearch for Gh<'_> {
    async fn total(&self, query: &str) -> Result<u64> {
        let q = format!("q={query}");
        let out = exec_within(
            SEARCH_LIMIT,
            &mut self.0.gh([
                "api",
                "-X",
                "GET",
                "search/issues",
                "-f",
                q.as_str(),
                "-F",
                "per_page=1",
                "--jq",
                ".total_count",
            ]),
        )
        .await?;
        out.trim()
            .parse()
            .map_err(|_| anyhow!("unexpected search answer: {}", out.trim()))
    }
}

/// One org's open issues: everything the search counts in the org's unarchived repositories, less
/// what the excluded repositories hold. Pull requests are not issues to `is:issue`.
async fn count_org<S: IssueSearch>(search: &S, qualifier: &str, org: &str, repos: &[Value]) -> Result<OrgBacklog> {
    let plan = owner_repos(repos, org);
    let base = format!("{qualifier}:{org} is:issue is:open archived:false");
    let mut issues = search.total(&base).await?;
    // Their issues are in the total above; take them back out, a query's worth of names at a time.
    let mut names = plan.excluded.iter().peekable();
    while names.peek().is_some() {
        let mut query = base.clone();
        while let Some(name) = names.peek() {
            if query.len() + name.len() + 6 > QUERY_BUDGET + base.len() && query.len() > base.len() {
                break;
            }
            query.push_str(&format!(" repo:{name}"));
            names.next();
        }
        issues = issues.saturating_sub(search.total(&query).await?);
    }
    Ok(OrgBacklog {
        issues,
        repos: plan.counted,
    })
}

/// Counts every org in `orgs` (`(login, qualifier)`), keeping the previous numbers of an org whose
/// count failed. `Err` only when nothing could be counted and there was something to count.
async fn count_all<S: IssueSearch>(
    search: &S,
    orgs: &[(String, &'static str)],
    repos: &[Value],
    previous: &BTreeMap<String, OrgBacklog>,
) -> Result<BTreeMap<String, OrgBacklog>> {
    let mut out = BTreeMap::new();
    let mut last_error = None;
    for (org, qualifier) in orgs {
        match count_org(search, qualifier, org, repos).await {
            Ok(counted) => {
                out.insert(org.clone(), counted);
            }
            Err(e) => {
                last_error = Some(e);
                if let Some(old) = previous.get(org) {
                    out.insert(org.clone(), old.clone());
                }
            }
        }
    }
    match last_error {
        Some(e) if out.is_empty() => Err(e),
        _ => Ok(out),
    }
}

async fn refresh(app: &Shared) -> Result<Backlog> {
    let viewer = crate::github::viewer(app).await?;
    let own = viewer["login"].as_str().unwrap_or_default().to_string();
    let repos = crate::github::repos_cached(app).await?;
    let known = app.known_orgs().unwrap_or_default();
    let saved = app.all_org_settings();
    let colony_orgs: Vec<String> = app.sessions.read().await.iter().map(|s| s.org.clone()).collect();
    let awaiting: BTreeSet<String> = app.new_orgs.read().await.keys().cloned().collect();
    let mut scope = scope_orgs(
        known.keys().map(String::as_str),
        &saved,
        colony_orgs.iter().map(String::as_str),
        &awaiting,
    );
    // The signed-in account's own repositories are a workspace unless switched off.
    if !own.is_empty() && saved.get(&own).is_none_or(crate::orgs::org_enabled) {
        scope.insert(own.clone());
    }
    let orgs: Vec<(String, &'static str)> = scope
        .into_iter()
        .filter(|org| crate::orgs::valid_org(org))
        .map(|org| {
            let qualifier = if org.eq_ignore_ascii_case(&own) { "user" } else { "org" };
            (org, qualifier)
        })
        .collect();
    let previous = app
        .backlog
        .0
        .lock()
        .await
        .last
        .as_ref()
        .map(|b| b.by_org.clone())
        .unwrap_or_default();
    let by_org = count_all(&Gh(app), &orgs, &repos, &previous).await?;
    Ok(Backlog {
        by_org,
        as_of: Utc::now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// A GitHub with a few repositories' worth of open items, answering the query forms
    /// `count_org` sends: `org:`/`user:`, `repo:` (any of), `is:issue`, `is:open`, `archived:false`.
    struct Fake {
        /// `(repo, is_pull_request, repo_archived)`, all open.
        items: Vec<(&'static str, bool, bool)>,
        queries: StdMutex<Vec<String>>,
    }

    impl Fake {
        fn new(items: &[(&'static str, bool, bool)]) -> Self {
            Self {
                items: items.to_vec(),
                queries: StdMutex::default(),
            }
        }
    }

    impl IssueSearch for Fake {
        async fn total(&self, query: &str) -> Result<u64> {
            self.queries.lock().unwrap().push(query.to_string());
            let terms: Vec<&str> = query.split_whitespace().collect();
            let owner = terms
                .iter()
                .find_map(|t| t.strip_prefix("org:").or_else(|| t.strip_prefix("user:")))
                .expect("every query names an owner");
            let repos: Vec<&str> = terms.iter().filter_map(|t| t.strip_prefix("repo:")).collect();
            let issues_only = terms.contains(&"is:issue");
            let unarchived = terms.contains(&"archived:false");
            Ok(self
                .items
                .iter()
                .filter(|(repo, pr, archived)| {
                    repo.split('/').next() == Some(owner)
                        && (repos.is_empty() || repos.contains(repo))
                        && (!issues_only || !pr)
                        && (!unarchived || !archived)
                })
                .count() as u64)
        }
    }

    fn repo(full_name: &str, fork: bool, archived: bool, has_issues: bool, open: u64) -> Value {
        json!({"full_name": full_name, "fork": fork, "archived": archived, "has_issues": has_issues, "open_issues_count": open})
    }

    #[tokio::test]
    async fn a_repo_with_only_open_pull_requests_counts_zero_issues() {
        let fake = Fake::new(&[("acme/web", true, false), ("acme/web", true, false)]);
        let repos = [repo("acme/web", false, false, true, 2)];
        let got = count_org(&fake, "org", "acme", &repos).await.unwrap();
        // The repository is still one the operator colonizes; it just has no issues.
        assert_eq!(got, OrgBacklog { issues: 0, repos: 1 });
    }

    #[tokio::test]
    async fn issues_are_counted_apart_from_pull_requests() {
        let fake = Fake::new(&[
            ("acme/web", false, false),
            ("acme/web", true, false),
            ("acme/api", false, false),
        ]);
        let repos = [
            repo("acme/web", false, false, true, 2),
            repo("acme/api", false, false, true, 1),
        ];
        let got = count_org(&fake, "org", "acme", &repos).await.unwrap();
        assert_eq!(got, OrgBacklog { issues: 2, repos: 2 });
    }

    #[tokio::test]
    async fn forks_archived_repos_and_repos_with_issues_off_are_excluded() {
        let fake = Fake::new(&[
            ("acme/web", false, false),
            ("acme/forked", false, false),
            ("acme/forked", false, false),
            ("acme/old", false, true),
            ("acme/quiet", false, false),
        ]);
        let repos = [
            repo("acme/web", false, false, true, 1),
            repo("acme/forked", true, false, true, 2),
            repo("acme/old", false, true, true, 1),
            repo("acme/quiet", false, false, false, 1),
        ];
        let got = count_org(&fake, "org", "acme", &repos).await.unwrap();
        assert_eq!(got, OrgBacklog { issues: 1, repos: 1 });
    }

    #[tokio::test]
    async fn a_fork_without_open_items_costs_no_extra_search() {
        let fake = Fake::new(&[("acme/web", false, false)]);
        let repos = [
            repo("acme/web", false, false, true, 1),
            repo("acme/forked", true, false, false, 0),
        ];
        count_org(&fake, "org", "acme", &repos).await.unwrap();
        assert_eq!(fake.queries.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn many_excluded_repos_are_subtracted_in_queries_under_the_length_limit() {
        let names: Vec<&'static str> = (0..40)
            .map(|i| &*Box::leak(format!("acme/forked-repository-{i}").into_boxed_str()))
            .collect();
        let mut items = vec![("acme/web", false, false)];
        items.extend(names.iter().map(|n| (*n, false, false)));
        let fake = Fake::new(&items);
        let mut repos = vec![repo("acme/web", false, false, true, 1)];
        repos.extend(names.iter().map(|n| repo(n, true, false, true, 1)));
        let got = count_org(&fake, "org", "acme", &repos).await.unwrap();
        assert_eq!(got, OrgBacklog { issues: 1, repos: 1 });
        let queries = fake.queries.lock().unwrap();
        assert!(queries.len() > 2, "{queries:?}");
        assert!(queries.iter().all(|q| q.len() <= 256), "{queries:?}");
    }

    #[test]
    fn an_org_the_token_merely_sees_is_not_in_the_all_workspaces_scope() {
        let saved = BTreeMap::from([(
            "off".to_string(),
            OrgSettings {
                enabled: Some(false),
                ..Default::default()
            },
        )]);
        let awaiting = BTreeSet::from(["new".to_string()]);
        let scope = scope_orgs(["acme", "off", "new"], &saved, ["colony-org"], &awaiting);
        // `stranger` is on the token's repository list only: in none of the sources.
        assert_eq!(scope, BTreeSet::from(["acme".to_string(), "colony-org".to_string()]));
    }

    #[tokio::test]
    async fn only_the_orgs_in_scope_are_counted_and_summed() {
        let fake = Fake::new(&[
            ("acme/web", false, false),
            ("acme/web", false, false),
            ("stranger/lib", false, false),
        ]);
        let repos = [
            repo("acme/web", false, false, true, 2),
            repo("stranger/lib", false, false, true, 1),
        ];
        let orgs = [("acme".to_string(), "org")];
        let by_org = count_all(&fake, &orgs, &repos, &BTreeMap::new()).await.unwrap();
        let json = Backlog {
            by_org,
            as_of: Utc::now(),
        }
        .to_json();
        assert_eq!((json["issues"].as_u64(), json["repos"].as_u64()), (Some(2), Some(1)));
        assert_eq!(json["by_org"]["acme"], json!({"issues": 2, "repos": 1}));
        assert!(json["by_org"].get("stranger").is_none());
        assert!(json["as_of"].is_string());
    }

    #[tokio::test]
    async fn a_failed_org_keeps_its_previous_numbers_and_total_failure_is_an_error() {
        struct Broken;
        impl IssueSearch for Broken {
            async fn total(&self, _: &str) -> Result<u64> {
                Err(anyhow!("403: secondary rate limit"))
            }
        }
        let orgs = [("acme".to_string(), "org")];
        let previous = BTreeMap::from([("acme".to_string(), OrgBacklog { issues: 7, repos: 3 })]);
        let kept = count_all(&Broken, &orgs, &[], &previous).await.unwrap();
        assert_eq!(kept["acme"], OrgBacklog { issues: 7, repos: 3 });
        assert!(count_all(&Broken, &orgs, &[], &BTreeMap::new()).await.is_err());
    }

    #[test]
    fn a_refresh_is_due_after_ten_minutes_or_a_failed_minute() {
        let now = Instant::now();
        let ago = |s| now.checked_sub(Duration::from_secs(s));
        assert!(refresh_due(None, None, now));
        assert!(!refresh_due(ago(599), None, now));
        assert!(refresh_due(ago(601), None, now));
        assert!(!refresh_due(None, ago(59), now));
        assert!(refresh_due(None, ago(61), now));
    }
}
