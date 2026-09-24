//! `GET /api/repos/{owner}/{repo}/meta`: what the cockpit's repository picker shows for a repository —
//! its description, languages (as GitHub's bar shows them), a year of weekly commits and its top
//! contributors — read through the operator's `gh` login and cached, since it changes slowly and the
//! picker asks for many repositories at once.

use crate::{ApiResult, Shared, client_error, util::exec};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde_json::{Value, json};
use std::time::Duration;

/// How long a complete answer serves before it is refreshed behind the next request.
const META_FRESH: Duration = Duration::from_secs(60 * 60);
/// GitHub computes commit statistics lazily and answers 202 until it has them; an answer made while
/// they were missing is refreshed soon instead of in an hour.
const META_RETRY: Duration = Duration::from_secs(60);

fn valid_part(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && s != "."
        && s != ".."
}

/// Languages by bytes, largest first, each with its share of the total rounded to one decimal.
pub(crate) fn language_shares(languages: &Value) -> Vec<Value> {
    let Some(map) = languages.as_object() else { return Vec::new() };
    let mut rows: Vec<(String, u64)> = map.iter().filter_map(|(k, v)| v.as_u64().map(|b| (k.clone(), b))).collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let total: u64 = rows.iter().map(|(_, b)| b).sum();
    rows.into_iter()
        .map(|(name, bytes)| {
            let percent = if total == 0 {
                0.0
            } else {
                (bytes as f64 * 1000.0 / total as f64).round() / 10.0
            };
            json!({"name": name, "bytes": bytes, "percent": percent})
        })
        .collect()
}

/// The 52 weekly commit counts of `/stats/participation`'s `all`, oldest first; empty when GitHub
/// has not computed them yet (a 202 body is empty) or the shape is unexpected.
pub(crate) fn weekly_commits(participation: &Value) -> Vec<u64> {
    participation["all"]
        .as_array()
        .map(|weeks| weeks.iter().map(|w| w.as_u64().unwrap_or(0)).collect())
        .unwrap_or_default()
}

/// The top contributors: login, avatar and commit count, bots included as GitHub lists them.
pub(crate) fn contributors(list: &Value, limit: usize) -> Vec<Value> {
    list.as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|c| {
                    Some(json!({
                        "login": c["login"].as_str()?,
                        "avatar_url": c["avatar_url"].as_str().unwrap_or_default(),
                        "contributions": c["contributions"].as_u64().unwrap_or(0),
                    }))
                })
                .take(limit)
                .collect()
        })
        .unwrap_or_default()
}

/// One `gh api` call parsed as JSON; an empty body (202, 204) or a failure reads as `Null`, so one
/// missing part never costs the others.
async fn gh_json(app: &Shared, path: &str) -> Value {
    match exec(&mut app.gh(["api", path])).await {
        Ok(out) if !out.trim().is_empty() => serde_json::from_str(&out).unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

async fn fetch(app: &Shared, repo: &str) -> anyhow::Result<Value> {
    let (info_path, languages_path, stats_path, people_path) = (
        format!("repos/{repo}"),
        format!("repos/{repo}/languages"),
        format!("repos/{repo}/stats/participation"),
        format!("repos/{repo}/contributors?per_page=8"),
    );
    let (info, languages, participation, people) = tokio::join!(
        gh_json(app, &info_path),
        gh_json(app, &languages_path),
        gh_json(app, &stats_path),
        gh_json(app, &people_path),
    );
    if info.is_null() {
        anyhow::bail!("GitHub did not answer for {repo}");
    }
    let weekly = weekly_commits(&participation);
    Ok(json!({
        "full_name": info["full_name"].as_str().unwrap_or(repo),
        "description": info["description"].as_str(),
        "homepage": info["homepage"].as_str().filter(|h| !h.is_empty()),
        "stars": info["stargazers_count"].as_u64().unwrap_or(0),
        "primary_language": info["language"].as_str(),
        "languages": language_shares(&languages),
        "stats_pending": weekly.is_empty(),
        "commits_weekly": weekly,
        "contributors": contributors(&people, 8),
        "pushed_at": info["pushed_at"].as_str(),
        "html_url": info["html_url"].as_str(),
    }))
}

pub async fn meta(State(app): State<Shared>, Path((owner, name)): Path<(String, String)>) -> ApiResult<Value> {
    if !valid_part(&owner) || !valid_part(&name) {
        return Err(client_error(StatusCode::BAD_REQUEST, "invalid repository name"));
    }
    let repo = format!("{owner}/{name}");
    let key = format!("repo-meta:{repo}");
    // A first answer made while GitHub was still computing statistics is kept only briefly.
    let pending_key = format!("repo-meta-pending:{repo}");
    let fresh = if app.answer_cache_has(&pending_key) {
        META_RETRY
    } else {
        META_FRESH
    };
    let value = crate::cached_answer(&app, key, fresh, move |app| {
        let repo = repo.clone();
        let pending_key = pending_key.clone();
        async move {
            let value = fetch(&app, &repo).await?;
            app.answer_cache_mark(&pending_key, value["stats_pending"].as_bool().unwrap_or(false));
            Ok(value)
        }
    })
    .await?;
    Ok(Json(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_shares_sort_by_bytes_and_round_to_a_tenth() {
        let shares = language_shares(&json!({"Rust": 700, "TypeScript": 250, "Shell": 50}));
        assert_eq!(shares[0]["name"], "Rust");
        assert_eq!(shares[0]["percent"], 70.0);
        assert_eq!(shares[1]["percent"], 25.0);
        assert_eq!(shares[2]["percent"], 5.0);
        assert!(language_shares(&json!({})).is_empty());
        assert!(language_shares(&Value::Null).is_empty());
        assert_eq!(language_shares(&json!({"A": 1, "B": 2}))[0]["percent"], 66.7);
    }

    #[test]
    fn weekly_commits_read_all_and_treat_pending_as_empty() {
        assert_eq!(weekly_commits(&json!({"all": [1, 0, 3], "owner": [0, 0, 1]})), vec![1, 0, 3]);
        assert!(weekly_commits(&Value::Null).is_empty(), "a 202 while GitHub computes");
    }

    #[test]
    fn contributors_keep_login_avatar_count_up_to_the_limit() {
        let list = json!([
            {"login": "a", "avatar_url": "u", "contributions": 9},
            {"login": "b", "contributions": 3},
            {"nologin": true},
            {"login": "c", "contributions": 1}
        ]);
        let top = contributors(&list, 2);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0], json!({"login": "a", "avatar_url": "u", "contributions": 9}));
        assert_eq!(top[1]["avatar_url"], "");
    }

    #[test]
    fn repo_parts_are_validated() {
        assert!(valid_part("chi-web") && valid_part("a.b_c"));
        assert!(!valid_part("") && !valid_part("..") && !valid_part("a/b") && !valid_part(&"x".repeat(101)));
    }
}
