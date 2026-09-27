//! deja (github.com/vshulcz/deja-vu, issue #495): an optional, off-by-default memory feature that
//! indexes each finished colony's Claude Code transcripts into a per-org deja index on the host and
//! serves read-only recall to later colonies of the same org, over the provider gateway with the
//! colony's own token. deja is a local Go binary — no LLM, nothing leaves the machine.
//!
//! Everything is keyed by org: a colony without an org is never indexed and gets no recall, and one
//! org's index never answers another's query. Transcripts are scrubbed of every secret value the
//! mothership knows *before* they are written anywhere under the org's directory, so the index holds
//! no more than the colony already saw in its own worktree. The scrub catches each value raw and
//! JSON-escaped, six characters and up, and deja's own pattern redaction runs on top; base64- or
//! percent-encoded forms of a secret are not caught.

use crate::{ApiResult, App, Shared, client_error, orgs, util};
use anyhow::{Context, Result, bail};
use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
    routing,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};
use tokio::sync::{Mutex as AsyncMutex, Semaphore};

/// Where the vendored binary lands, relative to the app assets (scripts/fetch-vendor.sh).
const BIN_ASSET: &str = "vendor/deja/deja";
/// `COLONIZER_DEJA_BIN` overrides the vendored binary's location.
const BIN_ENV: &str = "COLONIZER_DEJA_BIN";
/// Secrets shorter than this are not scrubbed: a five-letter word would mangle whole transcripts.
const MIN_SECRET_LEN: usize = 6;
/// What a scrubbed value is replaced with, in the copied transcripts and so in the index.
const REDACTED: &str = "[redacted]";
/// The recall query is bounded: it is an argv element, not a paste.
const MAX_QUERY_CHARS: usize = 512;
const DEFAULT_LIMIT: u32 = 5;
const MAX_LIMIT: u32 = 20;
/// deja walks the whole index per query; a wedged binary must not wedge a stop forever.
const INDEX_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
const SEARCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// At most this many deja searches at once, however many colonies ask: each search walks the whole
/// index. Searches queue rather than get refused — each is bounded by [`SEARCH_TIMEOUT`], so the
/// queue drains. (Indexing is not bounded here; the per-org lock already serialises it.)
static SEARCH_SLOTS: Semaphore = Semaphore::const_new(2);

#[derive(Default)]
pub struct Deja {
    /// One lock per org: two colonies of an org finishing at once must not run two `deja index`
    /// processes against one index. Keys are validated org keys ([`org_key`]).
    locks: AsyncMutex<HashMap<String, std::sync::Arc<AsyncMutex<()>>>>,
    /// Set once the "not installed" line has been printed, so an install without the binary says so
    /// once rather than on every colony stop or recall.
    warned: AtomicBool,
    /// Test-only binary override, so the stub tests never point a process-global env var at a script.
    #[cfg(test)]
    test_bin: std::sync::Mutex<Option<PathBuf>>,
}

/// The deja binary: the test override, then `COLONIZER_DEJA_BIN`, then the vendored one. `None`
/// means deja is simply unavailable — logged once, never a crash, recall just stays empty.
fn binary(app: &App) -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(bin) = app.deja.test_bin.lock().unwrap().clone() {
        return Some(bin);
    }
    let found = util::env_nonempty(BIN_ENV)
        .map(PathBuf::from)
        .filter(|path| path.exists())
        .or_else(|| app.cfg.asset(BIN_ASSET).ok());
    if found.is_none() && !app.deja.warned.swap(true, Ordering::Relaxed) {
        eprintln!("deja: the deja binary is not installed (run scripts/install.sh); transcript recall stays empty until it is");
    }
    found
}

/// The org's directory name under `<data_dir>/deja`: the org, lowercased (GitHub org names are
/// case-insensitive — "Acme" and "acme" are one org and share one index), when it is a safe single
/// path component with no surrounding whitespace; refused otherwise. A name that could climb out of
/// the deja directory never gets an index, and a colony whose org is empty is never indexed at all.
fn org_key(org: &str) -> Result<String> {
    let key = org.to_lowercase();
    if key.trim() == key && util::is_plain_name(&key) && key.len() <= 64 {
        Ok(key)
    } else {
        bail!("{org:?} is not usable as an org key for deja (it must be a plain name)")
    }
}

/// The repository's slug as deja's project directory; deja derives the `project` a hit reports back
/// from it. Every character that is not plain in a path component becomes `-`, so the slug is one
/// safe directory name whatever the repository is called — including a repository named `.` or `..`,
/// which would otherwise slug to a traversal.
fn repo_slug(repo: &str) -> String {
    let slug: String = repo
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect();
    match slug.as_str() {
        "." | ".." => "-".to_string(),
        _ => slug,
    }
}

// ---------------------------------------------------------------------------------------------
// Indexing
// ---------------------------------------------------------------------------------------------

/// Indexes a colony's transcripts once it has stopped, when deja is effective-enabled for its org.
/// Fire-and-forget: a deja failure is a lost index update, never a failed stop.
pub(crate) fn spawn_after_stop(app: Shared, s: &crate::sessions::Session) {
    let s = s.clone();
    tokio::spawn(async move {
        if let Err(e) = index_colony(&app, &s).await {
            eprintln!("deja: colony {}: {e:#}", s.id);
        }
    });
}

async fn index_colony(app: &Shared, s: &crate::sessions::Session) -> Result<()> {
    let modules = app.modules.read().await.clone();
    if s.org.is_empty() || !orgs::effective_deja_enabled(&modules, &app.org_settings(&s.org)) {
        return Ok(());
    }
    let Some(bin) = binary(app) else {
        return Ok(());
    };
    let org = org_key(&s.org)?;
    // Serialised per org, held across the copy and the deja run.
    let lock = app.deja.locks.lock().await.entry(org.clone()).or_default().clone();
    let _guard = lock.lock().await;

    let (repo, id, data_dir) = (s.repo.clone(), s.id.clone(), app.cfg.data_dir.clone());
    let (blocked_app, org_for_copy) = (app.clone(), org.clone());
    let copied = tokio::task::spawn_blocking(move || -> Result<usize> {
        let secrets = crate::secrets::saved_values(&blocked_app);
        copy_transcripts(&data_dir, &id, &org_for_copy, &repo, &secrets)
    })
    .await
    .context("the transcript copy did not finish")??;
    if copied == 0 {
        return Ok(());
    }
    run_deja(
        &bin,
        &app.cfg.data_dir.join("deja").join(&org),
        Invocation::Index,
        INDEX_TIMEOUT,
    )
    .await?;
    // When the index was last built, for `GET /api/deja`. A lost stamp loses the timestamp only.
    let _ = tokio::fs::write(
        app.cfg.data_dir.join("deja").join(&org).join("last-indexed"),
        chrono::Utc::now().to_rfc3339(),
    )
    .await;
    Ok(())
}

/// Copies a colony's Claude Code transcripts into the org's deja source tree, with every known
/// secret value replaced by [`REDACTED`] before a byte is written there. Returns how many
/// transcripts were copied; zero when the colony's agent kept none. The copy produces deja's
/// required `<project-slug>/<file>.jsonl` layout, with the colony id prefixed so one project
/// directory holds every session of its repository.
fn copy_transcripts(data_dir: &Path, session: &str, org: &str, repo: &str, secrets: &[String]) -> Result<usize> {
    let source = data_dir.join("sessions").join(session).join("transcripts");
    if !source.is_dir() {
        return Ok(0);
    }
    let dest = data_dir.join("deja").join(org).join("sessions").join(repo_slug(repo));
    std::fs::create_dir_all(&dest)?;
    let mut copied = 0;
    // Claude Code keeps one `<uuid>.jsonl` per session under one directory per project; two levels
    // is the whole layout, so a plain walk finds everything and nothing else.
    for project in std::fs::read_dir(&source)?.flatten() {
        if !project.path().is_dir() {
            continue;
        }
        for file in std::fs::read_dir(project.path())?.flatten() {
            let path = file.path();
            if path.extension().is_none_or(|e| e != "jsonl") {
                continue;
            }
            let text = std::fs::read_to_string(&path)?;
            let name = format!("{session}-{}", file.file_name().to_string_lossy());
            std::fs::write(dest.join(name), scrub(&text, secrets))?;
            copied += 1;
        }
    }
    Ok(copied)
}

/// Replaces every known secret value with [`REDACTED`], in both the raw form and the JSON-escaped
/// form a transcript actually carries the value in. Values below [`MIN_SECRET_LEN`] are left alone.
fn scrub(text: &str, secrets: &[String]) -> String {
    let mut out = text.to_string();
    for secret in secrets {
        if secret.len() < MIN_SECRET_LEN {
            continue;
        }
        out = out.replace(secret.as_str(), REDACTED);
        if let Ok(escaped) = serde_json::to_string(secret) {
            let escaped = escaped.trim_matches('"');
            if escaped != secret.as_str() {
                out = out.replace(escaped, REDACTED);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Running deja
// ---------------------------------------------------------------------------------------------

/// One deja invocation. The environment is built from nothing: an isolated `HOME` under the org's
/// own directory keeps deja off the host user's `~/.claude`, `~/.codex` and the rest of what it
/// scans by default, and `DEJA_OFFLINE=1` keeps it from phoning home.
enum Invocation {
    Index,
    Search { query: String, limit: u32 },
}

async fn run_deja(bin: &Path, org_dir: &Path, invocation: Invocation, timeout: std::time::Duration) -> Result<String> {
    let home = org_dir.join("home");
    tokio::fs::create_dir_all(&home).await?;
    let mut cmd = tokio::process::Command::new(bin);
    match &invocation {
        Invocation::Index => {
            cmd.args(["index", "--quiet"])
                .env("DEJA_CLAUDE_ROOT", org_dir.join("sessions"));
        }
        Invocation::Search { query, limit } => {
            // Search gets no `DEJA_CLAUDE_ROOT`: it must answer from the index alone, never
            // re-index. `--` ends the flags, so a query starting with `-` stays a query.
            cmd.arg("--json").arg("--limit").arg(limit.to_string()).arg("--").arg(query);
        }
    }
    let output = tokio::time::timeout(
        timeout,
        // kill_on_drop: a timed-out deja must not live on past its future.
        cmd.env("HOME", home)
            .env("DEJA_OFFLINE", "1")
            .env("DEJA_INDEX_DIR", org_dir.join("index"))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("deja did not finish within {}s", timeout.as_secs()))?
    .context("could not run the deja binary")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "deja failed ({}): {}",
            output.status,
            stderr.lines().next_back().unwrap_or_default()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

// ---------------------------------------------------------------------------------------------
// Recall
// ---------------------------------------------------------------------------------------------

/// The recall answer for one org: deja searched over that org's index alone. An org that is
/// disabled, unnamed or never indexed — like an install without the binary — gets empty hits, the
/// same answer a colony would get from an empty index.
pub(crate) async fn search(app: &App, org: &str, query: &str, limit: Option<u32>) -> Result<Value> {
    let query: String = query.trim().chars().take(MAX_QUERY_CHARS).collect();
    let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    if query.chars().count() < 2 {
        return Ok(json!({"hits": []}));
    }
    let Ok(org) = org_key(org) else {
        return Ok(json!({"hits": []}));
    };
    let Some(bin) = binary(app) else {
        return Ok(json!({"hits": []}));
    };
    let org_dir = app.cfg.data_dir.join("deja").join(&org);
    if !org_dir.join("index").is_dir() {
        return Ok(json!({"hits": []}));
    }
    let _slot = SEARCH_SLOTS.acquire().await.map_err(|e| anyhow::anyhow!("{e}"))?;
    let stdout = run_deja(&bin, &org_dir, Invocation::Search { query, limit }, SEARCH_TIMEOUT).await?;
    Ok(to_hits(&stdout))
}

/// deja's `--json` envelope, cut down to what a colony may see: the project, the title, when the
/// session was last active and the matching snippets. Host paths and session ids stay on the host.
fn to_hits(stdout: &str) -> Value {
    // Tolerant on purpose: only a parseable envelope maps, anything else reads as no hits.
    let start = stdout.find('{').unwrap_or(stdout.len());
    let Ok(body) = serde_json::from_str::<Value>(&stdout[start..]) else {
        return json!({"hits": []});
    };
    let hits = body["hits"]
        .as_array()
        .map(|hits| {
            hits.iter()
                .map(|hit| {
                    json!({
                        "project": hit["session"]["project"],
                        "title": hit["session"]["title"],
                        "updated": hit["session"]["updated"],
                        "snippets": hit["snippets"],
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({"hits": hits})
}

// ---------------------------------------------------------------------------------------------
// Owner API
// ---------------------------------------------------------------------------------------------

/// `GET /api/deja`: whether the binary is installed, and per org with a deja directory under the
/// data dir, whether deja is on for it, how big the index is and when it was last built.
pub async fn status(State(app): State<Shared>) -> ApiResult<Value> {
    let modules = app.modules.read().await.clone();
    let installed = binary(&app).is_some();
    let root = app.cfg.data_dir.join("deja");
    // The per-org walk (sizes, stamps) is sync fs; keep it off the async executor.
    let walked = tokio::task::spawn_blocking(move || {
        let mut dirs: Vec<_> = std::fs::read_dir(&root)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        dirs.into_iter()
            .filter_map(|dir| {
                let name = dir.file_name()?.to_string_lossy().into_owned();
                util::is_plain_name(&name).then(|| {
                    (
                        name,
                        dir_size(&dir.join("index")),
                        util::read_trimmed(&dir.join("last-indexed")),
                    )
                })
            })
            .collect::<Vec<_>>()
    })
    .await
    .context("the deja directory walk did not finish")?;
    let orgs = walked
        .into_iter()
        .map(|(org, index_bytes, last_indexed_at)| {
            json!({
                "org": org,
                "enabled": orgs::effective_deja_enabled(&modules, &app.org_settings(&org)),
                "index_bytes": index_bytes,
                "last_indexed_at": last_indexed_at,
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({"installed": installed, "orgs": orgs})))
}

#[derive(Deserialize)]
struct SearchParams {
    org: String,
    q: String,
    limit: Option<u32>,
}

/// `GET /api/deja/search?org=<org>&q=<query>`: recall exactly as a colony of that org would get it,
/// for trying the feature out from the cockpit.
async fn search_route(State(app): State<Shared>, Query(params): Query<SearchParams>) -> ApiResult<Value> {
    search(&app, &params.org, &params.q, params.limit)
        .await
        .map(Json)
        .map_err(|e| client_error(StatusCode::BAD_GATEWAY, &format!("deja: {e:#}")))
}

/// The size of a directory tree, 0 when it does not exist.
fn dir_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| {
            let path = e.path();
            if path.is_dir() {
                dir_size(&path)
            } else {
                e.metadata().map(|m| m.len()).unwrap_or(0)
            }
        })
        .sum()
}

pub(crate) fn routes() -> axum::Router<crate::Shared> {
    axum::Router::new()
        .route("/api/deja", routing::get(status))
        .route("/api/deja/search", routing::get(search_route))
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::SessionStatus;
    use crate::sessions::tests::colony;
    use crate::tests::{temp_root, test_app_with};
    use std::io::Write as _;
    use std::sync::Arc;

    /// A stand-in for the deja binary, small enough to reason about: `index` copies every transcript
    /// under `$DEJA_CLAUDE_ROOT` into `$DEJA_INDEX_DIR` (one flat file each, prefixed with its
    /// project slug), and a search prints one hit per copied file containing the query, in deja's
    /// `--json` envelope shape. The stub's hit carries no transcript text, so its stdout is always
    /// valid JSON.
    fn stub_deja(root: &Path) -> PathBuf {
        let dir = root.join("stub");
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("deja");
        let mut script = std::fs::File::create(&bin).unwrap();
        script
            .write_all(
                br#"#!/bin/sh
case "$1" in
index)
  mkdir -p "$DEJA_INDEX_DIR" || exit 1
  find "$DEJA_CLAUDE_ROOT" -type f -name '*.jsonl' | while read -r f; do
    slug=$(basename "$(dirname "$f")")
    cp "$f" "$DEJA_INDEX_DIR/$slug-$(basename "$f")" || exit 1
  done
  ;;
*)
  query=""
  seen=""
  for arg in "$@"; do
    if [ -n "$seen" ]; then query=$arg; break; fi
    [ "$arg" = "--" ] && seen=1
  done
  total=0
  hits=""
  for f in "$DEJA_INDEX_DIR"/*.jsonl; do
    [ -f "$f" ] || continue
    grep -qF -- "$query" "$f" || continue
    total=$((total + 1))
    hit=$(printf '{"session":{"project":"%s","title":"t","updated":"u"},"snippets":["match"]}' "$(basename "$f")")
    if [ -z "$hits" ]; then hits=$hit; else hits="$hits,$hit"; fi
  done
  printf '{"schema_version":2,"tier":"exact","total":%s,"hits":[%s]}' "$total" "$hits"
  ;;
esac
"#,
            )
            .unwrap();
        drop(script);
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    /// An app whose deja runs the stub, with the install's memory `deja` switch on.
    fn stub_app(root: &Path, bin: &Path) -> Shared {
        let mut app = test_app_with(root, |_| {});
        app.modules
            .try_write()
            .unwrap()
            .memory
            .settings
            .insert("deja".into(), json!(true));
        Arc::get_mut(&mut app)
            .unwrap()
            .deja
            .test_bin
            .lock()
            .unwrap()
            .replace(bin.to_path_buf());
        app
    }

    /// Writes an org's deja override into orgs.json, the way a Settings save would.
    fn set_org_deja(app: &Shared, deja: bool) {
        let mut all: std::collections::BTreeMap<String, crate::orgs::OrgSettings> =
            crate::util::read_json_or_default(&app.cfg.config_dir.join("orgs.json")).unwrap();
        all.entry("acme".into()).or_default().memory.get_or_insert_default().deja = Some(deja);
        std::fs::create_dir_all(&app.cfg.config_dir).unwrap();
        std::fs::write(app.cfg.config_dir.join("orgs.json"), serde_json::to_vec_pretty(&all).unwrap()).unwrap();
    }

    /// A stopped colony of `acme` with one transcript to index.
    fn colony_with_transcript(app: &Shared, id: &str, text: &str) -> crate::sessions::Session {
        let mut s = colony("acme", SessionStatus::Stopped);
        s.id = id.into();
        let project = app.cfg.data_dir.join("sessions").join(id).join("transcripts/acme-repo");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("11111111-2222-3333-4444-555555555555.jsonl"), text).unwrap();
        s
    }

    fn walk_text(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .flat_map(|e| {
                let path = e.path();
                if path.is_dir() {
                    walk_text(&path)
                } else {
                    std::fs::read_to_string(&path).map(|t| vec![t]).unwrap_or_default()
                }
            })
            .collect()
    }

    /// (a) a colony of org A indexes and a later colony of org A recalls it; (b) org B gets
    /// nothing; (c) a planted Secrets-page value and a planted colony secret never appear anywhere
    /// under the org's deja dir.
    #[tokio::test]
    async fn a_colonys_transcripts_reach_only_its_own_org_index_redacted() {
        let root = temp_root();
        let bin = stub_deja(&root);
        let app = stub_app(&root, &bin);

        // A planted Secrets-page value and a planted colony secret, both in the transcript.
        let page_secret = "sk-ant-page-secret-0123456789";
        let colony_secret = "colony-token-secret-9876543210";
        let dir = app.cfg.config_dir.join("voice-keys");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("elevenlabs"), format!("{page_secret}\n")).unwrap();
        std::fs::create_dir_all(app.cfg.config_dir.join("colony-secrets")).unwrap();
        std::fs::write(
            app.cfg.config_dir.join("colony-secrets/VENDOR_TOKEN"),
            format!("{colony_secret}\n"),
        )
        .unwrap();
        std::fs::write(
            app.cfg.config_dir.join("colony-secrets.json"),
            json!([{
                "env": "VENDOR_TOKEN",
                "hosts": ["api.vendor.test"],
                "scope": {"kind": "org", "org": "acme"},
            }])
            .to_string(),
        )
        .unwrap();

        let s = colony_with_transcript(
            &app,
            "s1",
            &format!(
                "the token is {page_secret} and the other is {colony_secret}; in JSON it reads \"{}\"; the deploy pipeline ships weekly",
                colony_secret
            ),
        );
        index_colony(&app, &s).await.unwrap();

        // (c) No known secret value anywhere under the org's deja dir, sessions or index.
        let org_dir = app.cfg.data_dir.join("deja/acme");
        assert!(org_dir.join("index").is_dir(), "the stub built an index");
        for secret in [page_secret, colony_secret] {
            assert!(
                !walk_text(&org_dir).into_iter().any(|text| text.contains(secret)),
                "{secret} leaked into the deja dir"
            );
        }
        let copied =
            std::fs::read_to_string(org_dir.join("sessions/acme-repo/s1-11111111-2222-3333-4444-555555555555.jsonl")).unwrap();
        assert!(copied.contains(REDACTED), "the copy is scrubbed: {copied}");

        // (a) A later colony of the same org recalls it.
        let later = colony("acme", SessionStatus::Idle);
        let hits = search(&app, &later.org, "deploy", None).await.unwrap();
        assert_eq!(hits["hits"].as_array().unwrap().len(), 1, "{hits}");
        assert_eq!(
            hits["hits"][0]["project"], "acme-repo-s1-11111111-2222-3333-4444-555555555555.jsonl",
            "{hits}"
        );
        assert!(
            hits["hits"][0].get("path").is_none() && hits["hits"][0].get("id").is_none(),
            "hits carry no host paths or session ids: {hits}"
        );

        // (b) An org with no index of its own gets nothing, even for the same query.
        let other = colony("other", SessionStatus::Idle);
        let hits = search(&app, &other.org, "deploy", None).await.unwrap();
        assert_eq!(hits["hits"].as_array().unwrap().len(), 0, "{hits}");

        std::fs::remove_dir_all(&root).ok();
    }

    /// (e) Off by default: nothing is indexed and recall is empty. The org override decides within
    /// an enabled install, both ways.
    #[tokio::test]
    async fn indexing_runs_only_when_both_levels_enable_deja() {
        let root = temp_root();
        let bin = stub_deja(&root);
        let app = test_app_with(&root, |_| {});
        let s = colony_with_transcript(&app, "s1", "the deploy pipeline notes");
        index_colony(&app, &s).await.unwrap();
        assert!(
            !app.cfg.data_dir.join("deja/acme").exists(),
            "nothing is indexed while deja is off"
        );
        let hits = search(&app, "acme", "deploy", None).await.unwrap();
        assert_eq!(hits["hits"].as_array().unwrap().len(), 0);

        // Install on, org opted out: still nothing.
        let app = stub_app(&root, &bin);
        set_org_deja(&app, false);
        index_colony(&app, &s).await.unwrap();
        assert!(!app.cfg.data_dir.join("deja/acme").exists(), "an opt-out org is not indexed");

        // The org opting back in is what turns it on.
        set_org_deja(&app, true);
        index_colony(&app, &s).await.unwrap();
        assert!(app.cfg.data_dir.join("deja/acme/index").is_dir(), "the org opt-in indexes");
        std::fs::remove_dir_all(&root).ok();
    }

    /// (d) An org key that would climb out of the deja directory is refused, and case-only
    /// differences share one key.
    #[test]
    fn an_org_key_that_climbs_out_is_refused() {
        assert_eq!(org_key("acme").unwrap(), "acme");
        assert_eq!(org_key("Acme").unwrap(), "acme", "org names are case-insensitive");
        for bad in [
            "../victim",
            "a/b",
            "..",
            ".",
            "",
            "a:b",
            ".hidden",
            " acme",
            "acme ",
            "\tacme",
        ] {
            assert!(org_key(bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn repo_slugs_are_single_safe_components() {
        assert_eq!(repo_slug("acme/app"), "acme-app");
        assert_eq!(repo_slug("Acme/App.test"), "Acme-App.test");
        assert_eq!(
            repo_slug("weird/../org name"),
            "weird-..-org-name",
            "dots inside stay: still one component"
        );
        assert_eq!(repo_slug(".."), "-", "a repo called `..` must not slug to a traversal");
    }

    #[test]
    fn scrub_replaces_raw_and_json_escaped_secrets_but_not_short_words() {
        let text = r#"{"text":"key hunter2 and \"hunter2\" escaped, plus trust and trust4you"}"#;
        let out = scrub(text, &["hunter2".into(), "trust4you".into(), "trust".into()]);
        assert!(out.contains("trust "), "a word below the length floor is left alone: {out}");
        assert!(!out.contains("hunter2"), "{out}");
        assert!(!out.contains("trust4you"), "{out}");
        assert_eq!(out.matches(REDACTED).count(), 3, "{out}");
    }

    /// The colony-facing route: a colony token reaches only its own org's index, a wrong token gets
    /// a 401, and the answer carries no host paths.
    #[tokio::test]
    async fn the_colony_recall_route_scopes_to_the_tokens_org() {
        let root = temp_root();
        let bin = stub_deja(&root);
        let app = stub_app(&root, &bin);
        let mut s = colony("acme", SessionStatus::Idle);
        s.id = "recall1".into();
        app.sessions.write().await.push(s);
        let token = crate::util::random_token();
        std::fs::create_dir_all(app.session_dir("recall1")).unwrap();
        std::fs::write(app.gateway_token_file("recall1"), token.as_bytes()).unwrap();
        let idx = app.cfg.data_dir.join("deja/acme/index");
        std::fs::create_dir_all(&idx).unwrap();
        std::fs::write(idx.join("acme-app-x.jsonl"), "the deploy pipeline notes\n").unwrap();

        let router = crate::gateway::router(app.clone());
        use tower::ServiceExt as _;
        let recall = |token: Option<&str>| {
            let router = router.clone();
            let body = json!({"query": "deploy pipeline", "limit": 3}).to_string();
            let mut req = axum::http::Request::builder()
                .method(axum::http::Method::POST)
                .uri("/recall")
                .header(axum::http::header::CONTENT_TYPE, "application/json");
            if let Some(token) = token {
                req = req.header(axum::http::header::AUTHORIZATION, token);
            }
            async move { router.oneshot(req.body(axum::body::Body::from(body)).unwrap()).await.unwrap() }
        };
        let body_of = |res: axum::response::Response| async {
            serde_json::from_slice::<Value>(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap()).unwrap()
        };

        let res = recall(Some(&format!("Bearer {token}"))).await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_of(res).await;
        assert_eq!(body["hits"].as_array().unwrap().len(), 1, "{body}");
        assert_eq!(body["hits"][0]["project"], "acme-app-x.jsonl", "{body}");
        assert!(body["hits"][0].get("path").is_none(), "no host paths over the wire: {body}");

        // (b) The same query with an org B colony's token: nothing from acme's index.
        let mut other = colony("other", SessionStatus::Idle);
        other.id = "recall2".into();
        app.sessions.write().await.push(other);
        let other_token = crate::util::random_token();
        std::fs::create_dir_all(app.session_dir("recall2")).unwrap();
        std::fs::write(app.gateway_token_file("recall2"), other_token.as_bytes()).unwrap();
        let res = recall(Some(&format!("Bearer {other_token}"))).await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_of(res).await;
        assert_eq!(body["hits"].as_array().unwrap().len(), 0, "{body}");

        // A wrong token, or none, is a 401.
        assert_eq!(
            recall(Some("Bearer not-a-real-token-at-all")).await.status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(recall(None).await.status(), StatusCode::UNAUTHORIZED);
        std::fs::remove_dir_all(&root).ok();
    }

    /// The owner route: per-org installed/enabled/size/last-indexed for the cockpit. (The owner
    /// search route is the same `search` the colony-route test covers.)
    #[tokio::test]
    async fn the_owner_status_route_reports_per_org_state() {
        let root = temp_root();
        let bin = stub_deja(&root);
        let app = stub_app(&root, &bin);
        let idx = app.cfg.data_dir.join("deja/acme/index");
        std::fs::create_dir_all(&idx).unwrap();
        std::fs::write(idx.join("bucket"), "0123456789").unwrap();
        std::fs::write(app.cfg.data_dir.join("deja/acme/last-indexed"), "2026-09-01T00:00:00+00:00").unwrap();

        let router = routes().with_state(app.clone());
        use tower::ServiceExt as _;
        let res = router
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/deja")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body: Value = serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap()).unwrap();
        assert_eq!(body["installed"], json!(true), "{body}");
        let acme = body["orgs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["org"] == "acme")
            .expect("acme listed");
        assert_eq!(acme["enabled"], json!(true), "{acme}");
        assert_eq!(acme["index_bytes"], json!(10), "{acme}");
        assert_eq!(acme["last_indexed_at"], json!("2026-09-01T00:00:00+00:00"), "{acme}");
        std::fs::remove_dir_all(&root).ok();
    }

    /// The same index, recall and redaction checks against the real binary. Run with:
    /// `COLONIZER_DEJA_BIN=/path/to/deja cargo test -p colonizer-harness deja -- --ignored`
    #[tokio::test]
    #[ignore = "needs the real deja binary: COLONIZER_DEJA_BIN=/path/to/deja"]
    async fn against_the_real_deja_binary() {
        let Some(bin) = util::env_nonempty(BIN_ENV).map(PathBuf::from) else {
            panic!("set COLONIZER_DEJA_BIN to a deja binary to run this test");
        };
        let root = temp_root();
        let app = stub_app(&root, &bin);
        let page_secret = "sk-ant-real-binary-secret-0123456789";
        let dir = app.cfg.config_dir.join("voice-keys");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("elevenlabs"), format!("{page_secret}\n")).unwrap();
        let s = colony_with_transcript(
            &app,
            "s1",
            // Real Claude Code transcript lines: deja parses this shape (type/message), not bare
            // role/text JSON.
            &format!(
                "{{\"parentUuid\":null,\"sessionId\":\"11111111-2222-3333-4444-555555555555\",\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"how do I configure the {page_secret} deploy pipeline\"}},\"uuid\":\"a1\",\"timestamp\":\"2026-09-26T10:00:00.000Z\"}}\n{{\"parentUuid\":\"a1\",\"sessionId\":\"11111111-2222-3333-4444-555555555555\",\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"The deploy pipeline lives in .github/workflows/deploy.yml.\"}}]}},\"uuid\":\"a2\",\"timestamp\":\"2026-09-26T10:00:05.000Z\"}}\n"
            ),
        );
        index_colony(&app, &s).await.unwrap();

        let org_dir = app.cfg.data_dir.join("deja/acme");
        assert!(
            !walk_text(&org_dir).into_iter().any(|t| t.contains(page_secret)),
            "the secret leaked"
        );

        let later = colony("acme", SessionStatus::Idle);
        let hits = search(&app, &later.org, "deploy pipeline", None).await.unwrap();
        assert!(!hits["hits"].as_array().unwrap().is_empty(), "{hits}");

        // A query that reads like a flag must stay a query: the `--` guards it. Nothing in this
        // corpus matches, so empty hits is the expected answer — what matters is that it is a 200
        // shape, not a deja usage error.
        let hits = search(&app, &later.org, "-rf zanzibar", None).await.unwrap();
        assert!(hits["hits"].is_array(), "{hits}");

        let other = colony("other", SessionStatus::Idle);
        let hits = search(&app, &other.org, "deploy pipeline", None).await.unwrap();
        assert_eq!(hits["hits"].as_array().unwrap().len(), 0, "{hits}");
        std::fs::remove_dir_all(&root).ok();
    }
}
