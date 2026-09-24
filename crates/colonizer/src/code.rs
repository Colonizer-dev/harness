//! The Code page: a repository as the operator reads and edits it, from the mothership's own bare
//! clone (`<data>/repos/<owner>/<name>.git`), so every read is local and cached per commit.
//!
//! - `GET  /api/repos/{o}/{r}/loc`          lines of code by language at the default branch
//! - `GET  /api/repos/{o}/{r}/coverage`     line coverage from CI artifacts, or "not measured"
//! - `GET  /api/repos/{o}/{r}/git-summary`  branch count, open pull requests, latest release or tag
//! - `GET  /api/repos/{o}/{r}/branches`     every branch: last commit, ahead/behind the default, its PR
//! - `GET  /api/repos/{o}/{r}/tree?ref=`    every file path at a ref
//! - `GET  /api/repos/{o}/{r}/blob?path=&ref=`     one file (text, or a binary placeholder)
//! - `GET  /api/repos/{o}/{r}/history?path=&ref=`  a file's recent commits
//! - `GET  /api/repos/{o}/{r}/blame?path=&ref=`    who last touched each line
//! - `POST /api/repos/{o}/{r}/edits`        commit edited files to a new branch and open a pull request
//! - `POST /api/repos/{o}/{r}/ask`          a quick answer about a file from the cheap summary model
//!
//! Every path is repository-relative and checked ([`crate::maps::valid_file_query`]); every ref is a
//! branch name or a commit sha ([`valid_ref`]). Nothing is written to GitHub except by `edits`, which
//! the cockpit only calls after an explicit confirm.

use crate::{
    ApiResult, Shared, client_error,
    maps::valid_file_query,
    util::{exec, exec_within, valid_repo},
};
use anyhow::{Context, Result, anyhow, bail};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::Path as FsPath,
    process::Stdio,
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A fetch of the bare clone at most this often per repository: the page reads are all local.
const FETCH_EVERY: Duration = Duration::from_secs(60);
/// How long one git or gh call may take.
const GIT_LIMIT: Duration = Duration::from_secs(20);
/// Largest file `blob` sends, and the largest file `loc` counts.
pub const BLOB_LIMIT: usize = 1024 * 1024;
/// Most paths `tree` sends.
const TREE_LIMIT: usize = 20_000;
/// Most files one edit may change, and the largest one.
const EDIT_FILES_LIMIT: usize = 50;
const EDIT_FILE_BYTES: usize = 1024 * 1024;
/// Most of a file the quick answer sends to the model.
const ASK_CONTEXT_LIMIT: usize = 60 * 1024;

// --- validation ---------------------------------------------------------------------------------

/// A `?ref=`: a branch name (letters, digits and `-_./`, no `..`, no leading `-` or `/`) or a
/// commit sha. `None` when refused.
pub fn valid_ref(raw: &str) -> Option<String> {
    let r = raw.trim();
    let ok = !r.is_empty()
        && r.len() <= 200
        && !r.starts_with('-')
        && !r.starts_with('/')
        && !r.ends_with('/')
        && !r.ends_with(".lock")
        && !r.contains("..")
        && !r.contains("//")
        && !r.contains("@{")
        && r.chars().all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c));
    ok.then(|| r.to_string())
}

fn is_sha(r: &str) -> bool {
    (7..=40).contains(&r.len()) && r.chars().all(|c| c.is_ascii_hexdigit())
}

/// An edited path: [`valid_file_query`], and never inside `.git`.
pub fn valid_edit_path(raw: &str) -> Option<String> {
    let path = valid_file_query(raw)?;
    let refused = path.split('/').any(|part| part.eq_ignore_ascii_case(".git"));
    (!refused).then_some(path)
}

fn repo_of(owner: &str, name: &str) -> Result<String, crate::AppError> {
    let repo = format!("{owner}/{name}");
    if !valid_repo(&repo) {
        return Err(client_error(StatusCode::BAD_REQUEST, "invalid repository name"));
    }
    Ok(repo)
}

fn bad(message: &str) -> crate::AppError {
    client_error(StatusCode::BAD_REQUEST, message)
}

// --- the bare clone -----------------------------------------------------------------------------

/// The bare clone, created on first use and fetched at most once per [`FETCH_EVERY`].
async fn ensure_bare(app: &Shared, repo: &str) -> Result<std::path::PathBuf> {
    let bare = app.bare_repo(repo);
    if !bare.join("HEAD").exists() {
        if let Some(parent) = bare.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let url = format!("https://github.com/{repo}.git");
        exec_within(
            Duration::from_secs(300),
            app.git_plain().args(["clone", "--bare", "--quiet"]).arg(&url).arg(&bare),
        )
        .await
        .with_context(|| format!("could not clone {repo}"))?;
        exec(
            app.git(&bare)
                .args(["config", "remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"]),
        )
        .await?;
    }
    let key = format!("code-fetch:{repo}");
    if !app.answer_cache_has(&key) {
        // One fetch at a time per repository; a failed one keeps the clone as it was.
        let _ = crate::cached_answer(app, key, FETCH_EVERY, {
            let bare = bare.clone();
            move |app| {
                let bare = bare.clone();
                async move {
                    exec_within(GIT_LIMIT, app.git(&bare).args(["fetch", "--quiet", "--prune", "origin"])).await?;
                    Ok(Value::Bool(true))
                }
            }
        })
        .await;
    }
    Ok(bare)
}

/// The default branch's name, from the bare clone's HEAD (set by `clone --bare`).
async fn default_branch(app: &Shared, bare: &FsPath) -> Result<String> {
    let head = exec(app.git(bare).args(["symbolic-ref", "--short", "HEAD"])).await?;
    let head = head.trim();
    if head.is_empty() {
        bail!("the clone has no default branch");
    }
    Ok(head.to_string())
}

/// A ref the bare clone resolves: a branch name maps to `refs/remotes/origin/<name>` (what the fetch
/// keeps current), a sha stays a sha. Returns `(ref_name, commit_sha)`.
async fn resolve(app: &Shared, bare: &FsPath, r: Option<&str>) -> Result<(String, String)> {
    let name = match r {
        Some(r) => valid_ref(r).ok_or_else(|| anyhow!("invalid ref"))?,
        None => default_branch(app, bare).await?,
    };
    let candidates = if is_sha(&name) {
        vec![name.clone()]
    } else {
        vec![
            format!("refs/remotes/origin/{name}"),
            format!("refs/heads/{name}"),
            format!("refs/tags/{name}"),
        ]
    };
    for c in candidates {
        if let Ok(sha) = exec(
            app.git(bare)
                .args(["rev-parse", "--verify", "--quiet"])
                .arg(format!("{c}^{{commit}}")),
        )
        .await
        {
            let sha = sha.trim().to_string();
            if !sha.is_empty() {
                return Ok((name, sha));
            }
        }
    }
    bail!("unknown ref {name}")
}

/// Raw bytes of `git <args>` against the bare clone, bounded.
async fn git_bytes(app: &Shared, bare: &FsPath, args: &[&str]) -> Result<Vec<u8>> {
    let mut cmd = app.git(bare);
    cmd.args(args).stdin(Stdio::null());
    let out = tokio::time::timeout(GIT_LIMIT, cmd.output())
        .await
        .map_err(|_| anyhow!("git {} timed out", args.first().unwrap_or(&"")))??;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

// --- tree, blob, history, blame -----------------------------------------------------------------

#[derive(Deserialize)]
pub struct RefQuery {
    #[serde(rename = "ref")]
    pub reference: Option<String>,
}

#[derive(Deserialize)]
pub struct PathQuery {
    pub path: String,
    #[serde(rename = "ref")]
    pub reference: Option<String>,
}

/// One `ls-tree -r -l` entry: mode, type, sha, size, path. `None` for anything else.
pub fn parse_ls_tree_line(line: &str) -> Option<(String, String, u64, String)> {
    let (meta, path) = line.split_once('\t')?;
    let mut parts = meta.split_whitespace();
    let _mode = parts.next()?;
    let kind = parts.next()?.to_string();
    let sha = parts.next()?.to_string();
    let size = parts.next()?.trim().parse::<u64>().unwrap_or(0);
    Some((kind, sha, size, path.to_string()))
}

async fn ls_tree(app: &Shared, bare: &FsPath, sha: &str) -> Result<Vec<(String, u64, String)>> {
    let out = git_bytes(app, bare, &["ls-tree", "-r", "-l", "-z", sha]).await?;
    Ok(out
        .split(|b| *b == 0)
        .filter_map(|entry| parse_ls_tree_line(&String::from_utf8_lossy(entry)))
        .filter(|(kind, ..)| kind == "blob")
        .map(|(_, blob, size, path)| (blob, size, path))
        .collect())
}

pub async fn tree(
    State(app): State<Shared>,
    Path((owner, name)): Path<(String, String)>,
    Query(q): Query<RefQuery>,
) -> ApiResult<Value> {
    let repo = repo_of(&owner, &name)?;
    let bare = ensure_bare(&app, &repo).await?;
    let (reference, sha) = resolve(&app, &bare, q.reference.as_deref())
        .await
        .map_err(|e| bad(&format!("{e:#}")))?;
    let mut paths: Vec<String> = ls_tree(&app, &bare, &sha).await?.into_iter().map(|(.., p)| p).collect();
    let truncated = paths.len() > TREE_LIMIT;
    paths.truncate(TREE_LIMIT);
    Ok(Json(
        json!({"repo": repo, "ref": reference, "sha": sha, "paths": paths, "truncated": truncated}),
    ))
}

/// Whether bytes look binary: a NUL in the first 8 KB, as git decides.
pub fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|b| *b == 0)
}

pub async fn blob(
    State(app): State<Shared>,
    Path((owner, name)): Path<(String, String)>,
    Query(q): Query<PathQuery>,
) -> ApiResult<Value> {
    let repo = repo_of(&owner, &name)?;
    let path = valid_file_query(&q.path).ok_or_else(|| bad("path must be repository-relative, without '..'"))?;
    let bare = ensure_bare(&app, &repo).await?;
    let (reference, sha) = resolve(&app, &bare, q.reference.as_deref())
        .await
        .map_err(|e| bad(&format!("{e:#}")))?;
    let spec = format!("{sha}:{path}");
    let size: usize = exec(app.git(&bare).args(["cat-file", "-s", &spec]))
        .await
        .map_err(|_| client_error(StatusCode::NOT_FOUND, "no such file at that ref"))?
        .trim()
        .parse()
        .unwrap_or(0);
    if size > BLOB_LIMIT {
        return Ok(Json(
            json!({"path": path, "ref": reference, "sha": sha, "size": size, "binary": false, "too_large": true, "text": null}),
        ));
    }
    let bytes = git_bytes(&app, &bare, &["cat-file", "blob", &spec]).await?;
    let binary = looks_binary(&bytes);
    let text = (!binary).then(|| String::from_utf8_lossy(&bytes).into_owned());
    Ok(Json(
        json!({"path": path, "ref": reference, "sha": sha, "size": size, "binary": binary, "too_large": false, "text": text}),
    ))
}

/// `git log` records split on the unit separator: sha, author, ISO date, subject.
pub fn parse_log(out: &str) -> Vec<Value> {
    out.lines()
        .filter_map(|line| {
            let mut f = line.split('\u{1f}');
            let sha = f.next()?.trim();
            if sha.is_empty() {
                return None;
            }
            Some(json!({"sha": sha, "author": f.next().unwrap_or(""), "date": f.next().unwrap_or(""), "message": f.next().unwrap_or("")}))
        })
        .collect()
}

pub async fn history(
    State(app): State<Shared>,
    Path((owner, name)): Path<(String, String)>,
    Query(q): Query<PathQuery>,
) -> ApiResult<Value> {
    let repo = repo_of(&owner, &name)?;
    let path = valid_file_query(&q.path).ok_or_else(|| bad("path must be repository-relative, without '..'"))?;
    let bare = ensure_bare(&app, &repo).await?;
    let (reference, sha) = resolve(&app, &bare, q.reference.as_deref())
        .await
        .map_err(|e| bad(&format!("{e:#}")))?;
    let out = git_bytes(
        &app,
        &bare,
        &["log", "-n", "30", "--format=%H%x1f%an%x1f%aI%x1f%s", &sha, "--", &path],
    )
    .await?;
    Ok(Json(
        json!({"path": path, "ref": reference, "commits": parse_log(&String::from_utf8_lossy(&out))}),
    ))
}

/// `git blame --porcelain` → the commits (author, epoch time, summary) and, per line, which one.
pub fn parse_blame(porcelain: &str) -> (BTreeMap<String, Value>, Vec<String>) {
    let mut commits: BTreeMap<String, Value> = BTreeMap::new();
    let mut lines = Vec::new();
    let mut current = String::new();
    for line in porcelain.lines() {
        if let Some(text) = line.strip_prefix('\t') {
            let _ = text;
            lines.push(current.clone());
            continue;
        }
        let mut words = line.split(' ');
        let first = words.next().unwrap_or("");
        if first.len() == 40 && first.chars().all(|c| c.is_ascii_hexdigit()) {
            current = first.to_string();
            commits.entry(current.clone()).or_insert_with(|| json!({}));
            continue;
        }
        let rest = line.split_once(' ').map(|(_, r)| r).unwrap_or("");
        if let Some(entry) = commits.get_mut(&current).and_then(Value::as_object_mut) {
            match first {
                "author" => {
                    entry.insert("author".into(), json!(rest));
                }
                "author-time" => {
                    entry.insert("time".into(), json!(rest.parse::<i64>().unwrap_or(0)));
                }
                "summary" => {
                    entry.insert("summary".into(), json!(rest));
                }
                _ => {}
            }
        }
    }
    (commits, lines)
}

pub async fn blame(
    State(app): State<Shared>,
    Path((owner, name)): Path<(String, String)>,
    Query(q): Query<PathQuery>,
) -> ApiResult<Value> {
    let repo = repo_of(&owner, &name)?;
    let path = valid_file_query(&q.path).ok_or_else(|| bad("path must be repository-relative, without '..'"))?;
    let bare = ensure_bare(&app, &repo).await?;
    let (reference, sha) = resolve(&app, &bare, q.reference.as_deref())
        .await
        .map_err(|e| bad(&format!("{e:#}")))?;
    let key = format!("code-blame:{repo}:{sha}:{path}");
    let value = crate::cached_answer(&app, key, Duration::from_secs(3600), move |app| {
        let (bare, sha, path, reference) = (bare.clone(), sha.clone(), path.clone(), reference.clone());
        async move {
            let out = git_bytes(&app, &bare, &["blame", "--porcelain", &sha, "--", &path]).await?;
            let (commits, lines) = parse_blame(&String::from_utf8_lossy(&out));
            Ok(json!({"path": path, "ref": reference, "sha": sha, "commits": commits, "lines": lines}))
        }
    })
    .await?;
    Ok(Json(value))
}

// --- lines of code ------------------------------------------------------------------------------

/// GitHub linguist names for common extensions (and a few file names). `None` for anything else,
/// which is not counted.
pub fn language_of(path: &str) -> Option<&'static str> {
    let file = path.rsplit('/').next().unwrap_or(path);
    match file {
        "Dockerfile" => return Some("Dockerfile"),
        "Makefile" | "makefile" => return Some("Makefile"),
        _ => {}
    }
    let ext = file.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase())?;
    Some(match ext.as_str() {
        "rs" => "Rust",
        "ts" | "mts" | "cts" => "TypeScript",
        "tsx" => "TSX",
        "js" | "mjs" | "cjs" => "JavaScript",
        "jsx" => "JavaScript",
        "py" => "Python",
        "go" => "Go",
        "swift" => "Swift",
        "kt" | "kts" => "Kotlin",
        "java" => "Java",
        "rb" => "Ruby",
        "c" | "h" => "C",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => "C++",
        "cs" => "C#",
        "dart" => "Dart",
        "vue" => "Vue",
        "svelte" => "Svelte",
        "res" | "resi" => "ReScript",
        "sh" | "bash" | "zsh" => "Shell",
        "html" | "htm" => "HTML",
        "css" => "CSS",
        "scss" => "SCSS",
        "md" => "Markdown",
        "mdx" => "MDX",
        "sql" => "SQL",
        "tf" | "hcl" => "HCL",
        "toml" => "TOML",
        "yml" | "yaml" => "YAML",
        "json" => "JSON",
        "proto" => "Protocol Buffer",
        "lua" => "Lua",
        "php" => "PHP",
        "ex" | "exs" => "Elixir",
        "zig" => "Zig",
        _ => return None,
    })
}

/// Vendored, generated or lock files that GitHub's language bar leaves out, and so does this.
pub fn is_vendored(path: &str) -> bool {
    let skip_dirs = [
        "node_modules/",
        "vendor/",
        "dist/",
        "build/",
        "target/",
        ".next/",
        "coverage/",
        "third_party/",
        "Pods/",
    ];
    let lower = path.to_ascii_lowercase();
    if skip_dirs
        .iter()
        .any(|d| lower.starts_with(d) || lower.contains(&format!("/{d}")))
    {
        return true;
    }
    let file = lower.rsplit('/').next().unwrap_or(&lower);
    file.ends_with(".lock")
        || file == "package-lock.json"
        || file == "pnpm-lock.yaml"
        || file == "yarn.lock"
        || file.ends_with(".min.js")
        || file.ends_with(".min.css")
        || file.ends_with(".map")
}

/// Code and blank lines in one file's text.
pub fn count_lines(text: &str) -> (u64, u64) {
    let mut code = 0;
    let mut blank = 0;
    for line in text.lines() {
        if line.trim().is_empty() {
            blank += 1;
        } else {
            code += 1;
        }
    }
    (code, blank)
}

/// Reads many blobs with one `git cat-file --batch`, calling `each` with every blob's bytes.
async fn cat_batch(app: &Shared, bare: &FsPath, blobs: &[String], mut each: impl FnMut(usize, &[u8])) -> Result<()> {
    let mut cmd = app.git(bare);
    cmd.args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn()?;
    let mut stdin = child.stdin.take().context("no stdin")?;
    let input = blobs.join("\n") + "\n";
    let writer = tokio::spawn(async move {
        let _ = stdin.write_all(input.as_bytes()).await;
        drop(stdin);
    });
    let mut stdout = child.stdout.take().context("no stdout")?;
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(120), stdout.read_to_end(&mut buf))
        .await
        .map_err(|_| anyhow!("git cat-file timed out"))??;
    let _ = writer.await;
    let _ = child.wait().await;
    // Each object: "<sha> <type> <size>\n<bytes>\n".
    let mut at = 0;
    let mut index = 0;
    while at < buf.len() && index < blobs.len() {
        let Some(nl) = buf[at..].iter().position(|b| *b == b'\n') else {
            break;
        };
        let header = String::from_utf8_lossy(&buf[at..at + nl]).into_owned();
        at += nl + 1;
        let size = header.rsplit(' ').next().and_then(|s| s.parse::<usize>().ok());
        match size {
            Some(size) if header.split(' ').nth(1) != Some("missing") => {
                let end = (at + size).min(buf.len());
                each(index, &buf[at..end]);
                at = end + 1;
            }
            _ => {}
        }
        index += 1;
    }
    Ok(())
}

pub async fn loc(State(app): State<Shared>, Path((owner, name)): Path<(String, String)>) -> ApiResult<Value> {
    let repo = repo_of(&owner, &name)?;
    let bare = ensure_bare(&app, &repo).await?;
    let (reference, sha) = resolve(&app, &bare, None).await?;
    let key = format!("code-loc:{repo}:{sha}");
    let value = crate::cached_answer(&app, key, Duration::from_secs(24 * 3600), move |app| {
        let (bare, sha, reference) = (bare.clone(), sha.clone(), reference.clone());
        async move {
            let files: Vec<(String, u64, String)> = ls_tree(&app, &bare, &sha)
                .await?
                .into_iter()
                .filter(|(_, size, path)| (*size as usize) <= BLOB_LIMIT && !is_vendored(path) && language_of(path).is_some())
                .collect();
            let blobs: Vec<String> = files.iter().map(|(b, ..)| b.clone()).collect();
            let mut by: HashMap<&'static str, (u64, u64, u64)> = HashMap::new();
            cat_batch(&app, &bare, &blobs, |i, bytes| {
                if looks_binary(bytes) {
                    return;
                }
                let Some(lang) = language_of(&files[i].2) else { return };
                let (code, blank) = count_lines(&String::from_utf8_lossy(bytes));
                let entry = by.entry(lang).or_default();
                entry.0 += 1;
                entry.1 += code;
                entry.2 += blank;
            })
            .await?;
            Ok(loc_json(&reference, &sha, by))
        }
    })
    .await?;
    Ok(Json(value))
}

/// The `loc` answer, languages by code lines, biggest first. Pure, for the tests.
pub fn loc_json(reference: &str, sha: &str, by: HashMap<&'static str, (u64, u64, u64)>) -> Value {
    let mut rows: Vec<(&str, (u64, u64, u64))> = by.into_iter().collect();
    rows.sort_by(|a, b| b.1.1.cmp(&a.1.1).then(a.0.cmp(b.0)));
    let total: u64 = rows.iter().map(|(_, (_, code, _))| code).sum();
    json!({
        "ref": reference,
        "sha": sha,
        "total": total,
        "by_language": rows.iter().map(|(name, (files, code, blank))| json!({"name": name, "files": files, "code": code, "blank": blank})).collect::<Vec<_>>(),
    })
}

// --- coverage -----------------------------------------------------------------------------------

/// Line coverage from an lcov report: the sums of `LH:` over `LF:`.
pub fn lcov_percent(text: &str) -> Option<f64> {
    let (mut found, mut hit) = (0u64, 0u64);
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("LF:") {
            found += v.trim().parse::<u64>().unwrap_or(0);
        } else if let Some(v) = line.strip_prefix("LH:") {
            hit += v.trim().parse::<u64>().unwrap_or(0);
        }
    }
    (found > 0).then(|| hit as f64 * 100.0 / found as f64)
}

/// Line coverage from an istanbul `coverage-summary.json` (`total.lines.pct`).
pub fn istanbul_percent(text: &str) -> Option<f64> {
    let v: Value = serde_json::from_str(text).ok()?;
    v["total"]["lines"]["pct"].as_f64()
}

/// Line coverage from a cobertura XML report (`line-rate` on the root `<coverage>`).
pub fn cobertura_percent(text: &str) -> Option<f64> {
    let start = text.find("<coverage")?;
    let tag = &text[start..text[start..].find('>').map(|e| start + e)?];
    let at = tag.find("line-rate=\"")? + "line-rate=\"".len();
    let rate: f64 = tag[at..].split('"').next()?.parse().ok()?;
    Some(rate * 100.0)
}

/// Line coverage from `cargo llvm-cov --json` (`data[0].totals.lines.percent`) or tarpaulin's
/// JSON (`coverage`).
pub fn json_percent(text: &str) -> Option<f64> {
    let v: Value = serde_json::from_str(text).ok()?;
    v["data"][0]["totals"]["lines"]["percent"]
        .as_f64()
        .or_else(|| v["coverage"].as_f64())
}

/// The coverage a report file states, by its name and contents.
pub fn coverage_from(file_name: &str, text: &str) -> Option<(f64, &'static str)> {
    let lower = file_name.to_ascii_lowercase();
    if lower.ends_with(".info") || lower.contains("lcov") {
        return lcov_percent(text).map(|p| (p, "lcov"));
    }
    if lower.ends_with("coverage-summary.json") {
        return istanbul_percent(text).map(|p| (p, "istanbul"));
    }
    if lower.ends_with(".xml") {
        return cobertura_percent(text).map(|p| (p, "cobertura"));
    }
    if lower.ends_with(".json") {
        return json_percent(text).map(|p| (p, "llvm-cov/tarpaulin"));
    }
    None
}

pub async fn coverage(State(app): State<Shared>, Path((owner, name)): Path<(String, String)>) -> ApiResult<Value> {
    let repo = repo_of(&owner, &name)?;
    let key = format!("code-coverage:{repo}");
    let value = crate::cached_answer(&app, key, Duration::from_secs(3600), move |app| {
        let repo = repo.clone();
        async move {
            Ok(find_coverage(&app, &repo)
                .await
                .unwrap_or_else(|e| not_measured(&format!("{e:#}"))))
        }
    })
    .await?;
    Ok(Json(value))
}

fn not_measured(reason: &str) -> Value {
    json!({"measured": false, "reason": reason})
}

/// Looks through the artifacts of the latest successful default-branch runs for a coverage report.
async fn find_coverage(app: &Shared, repo: &str) -> Result<Value> {
    let path = format!("repos/{repo}/actions/artifacts?per_page=50");
    let listing = exec_within(GIT_LIMIT, &mut app.gh(["api", path.as_str()])).await?;
    let listing: Value = serde_json::from_str(&listing)?;
    let candidate = listing["artifacts"].as_array().into_iter().flatten().find(|a| {
        let name = a["name"].as_str().unwrap_or("").to_ascii_lowercase();
        !a["expired"].as_bool().unwrap_or(false) && (name.contains("cov") || name.contains("lcov"))
    });
    let Some(artifact) = candidate else {
        return Ok(not_measured("no coverage report found in CI artifacts"));
    };
    let run = artifact["workflow_run"]["id"].as_u64().context("artifact has no run")?;
    let name = artifact["name"].as_str().unwrap_or_default().to_string();
    let dir = tempfile_dir()?;
    let run_id = run.to_string();
    exec_within(
        Duration::from_secs(60),
        &mut app.gh([
            "run",
            "download",
            run_id.as_str(),
            "-R",
            repo,
            "-n",
            name.as_str(),
            "-D",
            dir.to_str().unwrap_or("."),
        ]),
    )
    .await?;
    let found = scan_for_coverage(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(match found {
        Some((pct, format, file)) => json!({
            "measured": true,
            "percent": (pct * 10.0).round() / 10.0,
            "format": format,
            "file": file,
            "artifact": name,
            "run": run,
            "at": artifact["created_at"],
        }),
        None => not_measured("the coverage artifact had no report this reads (lcov, istanbul, cobertura, llvm-cov)"),
    })
}

fn tempfile_dir() -> Result<std::path::PathBuf> {
    let dir = std::env::temp_dir().join(format!("colonizer-coverage-{}", crate::util::short_id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn scan_for_coverage(dir: &FsPath) -> Option<(f64, &'static str, String)> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).ok()?.flatten() {
            let path = entry.path();
            let meta = entry.metadata().ok()?;
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() && meta.len() < 50 * 1024 * 1024 {
                let name = path.file_name()?.to_string_lossy().into_owned();
                if let Ok(text) = std::fs::read_to_string(&path)
                    && let Some((pct, format)) = coverage_from(&name, &text)
                {
                    return Some((pct, format, name));
                }
            }
        }
    }
    None
}

// --- branches and the git summary ---------------------------------------------------------------

/// `for-each-ref` records split on the unit separator: short name, sha, ISO date, author, subject.
pub fn parse_refs(out: &str) -> Vec<(String, String, String, String, String)> {
    out.lines()
        .filter_map(|line| {
            let mut f = line.split('\u{1f}');
            let name = f.next()?.trim().strip_prefix("origin/")?.to_string();
            if name == "HEAD" || name.is_empty() {
                return None;
            }
            Some((
                name,
                f.next()?.to_string(),
                f.next()?.to_string(),
                f.next().unwrap_or("").to_string(),
                f.next().unwrap_or("").to_string(),
            ))
        })
        .collect()
}

pub async fn branches(State(app): State<Shared>, Path((owner, name)): Path<(String, String)>) -> ApiResult<Value> {
    let repo = repo_of(&owner, &name)?;
    let bare = ensure_bare(&app, &repo).await?;
    let default = default_branch(&app, &bare).await?;
    let out = git_bytes(
        &app,
        &bare,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            "--count=100",
            "--format=%(refname:short)%1f%(objectname)%1f%(committerdate:iso-strict)%1f%(authorname)%1f%(contents:subject)",
            "refs/remotes/origin",
        ],
    )
    .await?;
    let refs = parse_refs(&String::from_utf8_lossy(&out));
    // Protection and open pull requests from GitHub; either may fail and leave the fields empty.
    let protected: Vec<String> = {
        let path = format!("repos/{repo}/branches?protected=true&per_page=100");
        exec_within(GIT_LIMIT, &mut app.gh(["api", path.as_str(), "--jq", ".[].name"]))
            .await
            .map(|s| s.lines().map(str::to_string).collect())
            .unwrap_or_default()
    };
    let prs: HashMap<String, Value> = exec_within(
        GIT_LIMIT,
        &mut app.gh([
            "pr",
            "list",
            "-R",
            repo.as_str(),
            "--state",
            "open",
            "--limit",
            "100",
            "--json",
            "number,title,url,headRefName,isDraft",
        ]),
    )
    .await
    .ok()
    .and_then(|s| serde_json::from_str::<Vec<Value>>(&s).ok())
    .unwrap_or_default()
    .into_iter()
    .filter_map(|pr| Some((pr["headRefName"].as_str()?.to_string(), pr)))
    .collect();
    let mut rows = Vec::new();
    for (branch, sha, date, author, subject) in refs {
        let (ahead, behind) = if branch == default {
            (0, 0)
        } else {
            let range = format!("refs/remotes/origin/{default}...{sha}");
            exec(app.git(&bare).args(["rev-list", "--left-right", "--count", &range]))
                .await
                .ok()
                .and_then(|s| {
                    let mut it = s.split_whitespace();
                    Some((it.next()?.parse::<u64>().ok()?, it.next()?.parse::<u64>().ok()?))
                })
                .map(|(behind, ahead)| (ahead, behind))
                .unwrap_or((0, 0))
        };
        rows.push(json!({
            "name": branch,
            "sha": sha,
            "date": date,
            "author": author,
            "message": subject,
            "default": branch == default,
            "protected": protected.contains(&branch),
            "colony": branch.starts_with("colonizer/"),
            "ahead": ahead,
            "behind": behind,
            "pr": prs.get(&branch),
        }));
    }
    Ok(Json(json!({"repo": repo, "default": default, "branches": rows})))
}

pub async fn git_summary(State(app): State<Shared>, Path((owner, name)): Path<(String, String)>) -> ApiResult<Value> {
    let repo = repo_of(&owner, &name)?;
    let key = format!("code-git-summary:{repo}");
    let value = crate::cached_answer(&app, key, Duration::from_secs(600), move |app| {
        let repo = repo.clone();
        async move {
            let bare = ensure_bare(&app, &repo).await?;
            let count = exec(
                app.git(&bare)
                    .args(["for-each-ref", "--format=%(refname)", "refs/remotes/origin"]),
            )
            .await
            .map(|s| s.lines().filter(|l| !l.ends_with("/HEAD")).count())
            .unwrap_or(0);
            let open_prs = exec_within(
                GIT_LIMIT,
                &mut app.gh([
                    "pr",
                    "list",
                    "-R",
                    repo.as_str(),
                    "--state",
                    "open",
                    "--limit",
                    "200",
                    "--json",
                    "number",
                    "--jq",
                    "length",
                ]),
            )
            .await
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok());
            let release = exec_within(
                GIT_LIMIT,
                &mut app.gh(["release", "view", "-R", repo.as_str(), "--json", "tagName,name,publishedAt"]),
            )
            .await
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok());
            let tag = if release.is_none() {
                exec(app.git(&bare).args([
                    "for-each-ref",
                    "--sort=-creatordate",
                    "--count=1",
                    "--format=%(refname:short)",
                    "refs/tags",
                ]))
                .await
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
            } else {
                None
            };
            Ok(json!({"repo": repo, "branches": count, "open_prs": open_prs, "release": release, "latest_tag": tag}))
        }
    })
    .await?;
    Ok(Json(value))
}

// --- edits → pull request -----------------------------------------------------------------------

#[derive(Deserialize)]
pub struct EditFile {
    pub path: String,
    pub content: String,
}

#[derive(Deserialize)]
pub struct EditsRequest {
    pub base: Option<String>,
    pub branch: String,
    pub message: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
    pub files: Vec<EditFile>,
}

/// Checks an edit before anything is written: a valid new branch name (not the base), a message and
/// title, 1..=[`EDIT_FILES_LIMIT`] files each with a valid path outside `.git` and at most
/// [`EDIT_FILE_BYTES`], no path twice. Pure, for the tests.
pub fn check_edits(req: &EditsRequest, base: &str) -> Result<Vec<(String, String)>, String> {
    let branch = valid_ref(&req.branch).ok_or("invalid branch name")?;
    if branch == base || is_sha(&branch) {
        return Err("the branch must be a new name, not the base".into());
    }
    if req.message.trim().is_empty() || req.title.trim().is_empty() {
        return Err("a commit message and a pull request title are required".into());
    }
    if req.files.is_empty() || req.files.len() > EDIT_FILES_LIMIT {
        return Err(format!("between 1 and {EDIT_FILES_LIMIT} files per edit"));
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for f in &req.files {
        let path = valid_edit_path(&f.path).ok_or_else(|| format!("refused path {:?}", f.path))?;
        if f.content.len() > EDIT_FILE_BYTES {
            return Err(format!("{path} is larger than {} KB", EDIT_FILE_BYTES / 1024));
        }
        if !seen.insert(path.clone()) {
            return Err(format!("{path} appears twice"));
        }
        out.push((path, f.content.clone()));
    }
    Ok(out)
}

pub async fn edits(
    State(app): State<Shared>,
    Path((owner, name)): Path<(String, String)>,
    Json(req): Json<EditsRequest>,
) -> ApiResult<Value> {
    let repo = repo_of(&owner, &name)?;
    if crate::authority::external_writes_blocked() {
        return Err(client_error(StatusCode::FORBIDDEN, crate::publish::BLOCKED));
    }
    let bare = ensure_bare(&app, &repo).await?;
    let base = match req.base.as_deref() {
        Some(b) => valid_ref(b).ok_or_else(|| bad("invalid base branch"))?,
        None => default_branch(&app, &bare).await?,
    };
    let files = check_edits(&req, &base).map_err(|e| bad(&e))?;
    let branch = valid_ref(&req.branch).expect("checked");
    let (_, base_sha) = resolve(&app, &bare, Some(&base)).await.map_err(|e| bad(&format!("{e:#}")))?;
    let url = open_edit_pr(&app, &repo, &bare, &base, &base_sha, &branch, &req, &files)
        .await
        .map_err(|e| client_error(StatusCode::BAD_GATEWAY, &format!("{e:#}")))?;
    // The edits are on GitHub now: their drafts on this ref are done.
    {
        let _guard = DRAFTS_LOCK.lock().await;
        let mut drafts = read_drafts(&app, &repo);
        for (path, _) in &files {
            drafts.remove(&draft_key(&base, path));
        }
        let _ = write_drafts(&app, &repo, &drafts).await;
    }
    Ok(Json(json!({"url": url, "branch": branch, "base": base})))
}

#[allow(clippy::too_many_arguments)]
async fn open_edit_pr(
    app: &Shared,
    repo: &str,
    bare: &FsPath,
    base: &str,
    base_sha: &str,
    branch: &str,
    req: &EditsRequest,
    files: &[(String, String)],
) -> Result<String> {
    // The operator's GitHub identity, never an AI co-author.
    let who = exec_within(
        GIT_LIMIT,
        &mut app.gh([
            "api",
            "user",
            "--jq",
            "[.login, (.id|tostring), (.name // .login)] | join(\"\\u001f\")",
        ]),
    )
    .await?;
    let mut it = who.trim().split('\u{1f}');
    let (login, id, name) = (it.next().unwrap_or(""), it.next().unwrap_or(""), it.next().unwrap_or(""));
    if login.is_empty() {
        bail!("could not read the GitHub user to commit as");
    }
    let email = format!("{id}+{login}@users.noreply.github.com");
    let wt = std::env::temp_dir().join(format!("colonizer-edit-{}", crate::util::short_id()));
    let result = async {
        commit_edit(app, bare, &wt, base_sha, branch, files, name, &email, req.message.trim()).await?;
        let mut push = app.git_plain();
        push.arg("-C")
            .arg(&wt)
            .args(["push", "--quiet", "origin", &format!("HEAD:refs/heads/{branch}")]);
        exec_within(Duration::from_secs(60), &mut push).await?;
        let url = exec_within(
            Duration::from_secs(60),
            &mut app.gh([
                "pr",
                "create",
                "-R",
                repo,
                "--base",
                base,
                "--head",
                branch,
                "--title",
                req.title.trim(),
                "--body",
                req.body.trim(),
            ]),
        )
        .await?;
        Ok(url.trim().lines().last().unwrap_or("").to_string())
    }
    .await;
    let _ = exec(app.git(bare).args(["worktree", "remove", "--force"]).arg(&wt)).await;
    let _ = tokio::fs::remove_dir_all(&wt).await;
    result
}

/// Checks out `base_sha` into a fresh worktree `wt` of the bare clone on a new `branch`, writes the
/// files and commits them as `name <email>`. Refuses to write through any symlink the repository
/// planted on the way to a path, so an edit can only land inside the worktree. The caller removes
/// the worktree.
#[allow(clippy::too_many_arguments)]
async fn commit_edit(
    app: &Shared,
    bare: &FsPath,
    wt: &FsPath,
    base_sha: &str,
    branch: &str,
    files: &[(String, String)],
    name: &str,
    email: &str,
    message: &str,
) -> Result<()> {
    exec(
        app.git(bare)
            .args(["worktree", "add", "--quiet", "--detach"])
            .arg(wt)
            .arg(base_sha),
    )
    .await?;
    let git = |args: &[&str]| {
        let mut c = app.git_plain();
        c.arg("-C").arg(wt).args(args);
        c
    };
    exec(&mut git(&["checkout", "--quiet", "-b", branch])).await?;
    for (path, content) in files {
        let mut at = wt.to_path_buf();
        for part in path.split('/') {
            at.push(part);
            if tokio::fs::symlink_metadata(&at)
                .await
                .is_ok_and(|m| m.file_type().is_symlink())
            {
                bail!("{path} passes through a symlink in the repository; refusing to write through it");
            }
        }
        let target = wt.join(path);
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&target, content).await?;
        exec(&mut git(&["add", "--", path])).await?;
    }
    exec(
        git(&[
            "-c",
            &format!("user.name={name}"),
            "-c",
            &format!("user.email={email}"),
            "commit",
            "--quiet",
            "-m",
            message,
        ])
        .env("GIT_AUTHOR_NAME", name)
        .env("GIT_AUTHOR_EMAIL", email)
        .env("GIT_COMMITTER_NAME", name)
        .env("GIT_COMMITTER_EMAIL", email),
    )
    .await?;
    Ok(())
}

// --- ask about a file ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct AskRequest {
    pub path: String,
    pub question: String,
    /// The file as the editor holds it (may carry unsaved edits); capped at [`ASK_CONTEXT_LIMIT`].
    pub content: String,
    /// `[first, last]` selected lines, 1-based, if any.
    #[serde(default)]
    pub selection: Option<(u32, u32)>,
}

/// The prompt for a question about a file: the file (cut to the limit), the selection, the question.
/// Pure, for the tests.
pub fn ask_prompt(path: &str, content: &str, selection: Option<(u32, u32)>, question: &str) -> String {
    let mut text = content.to_string();
    let mut cut = false;
    if text.len() > ASK_CONTEXT_LIMIT {
        let mut end = ASK_CONTEXT_LIMIT;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        cut = true;
    }
    let sel = selection
        .map(|(a, b)| format!("\nThe question is about lines {a}-{b}."))
        .unwrap_or_default();
    format!(
        "You answer questions about one source file, briefly and concretely, in Markdown.\n\nFile: {path}{}\n```\n{text}\n```\n{sel}\n\nQuestion: {}",
        if cut { " (cut to its first 60 KB)" } else { "" },
        question.trim()
    )
}

pub async fn ask(
    State(app): State<Shared>,
    Path((owner, name)): Path<(String, String)>,
    Json(req): Json<AskRequest>,
) -> ApiResult<Value> {
    let _repo = repo_of(&owner, &name)?;
    let path = valid_file_query(&req.path).ok_or_else(|| bad("path must be repository-relative, without '..'"))?;
    if req.question.trim().is_empty() || req.question.len() > 4000 {
        return Err(bad("ask a question of at most 4000 characters"));
    }
    let prompt = ask_prompt(&path, &req.content, req.selection, &req.question);
    let (answer, model) = crate::summaries::ask_freeform(&app, &prompt)
        .await
        .map_err(|e| client_error(StatusCode::BAD_GATEWAY, &e))?;
    Ok(Json(json!({"answer": answer, "model": model})))
}

// --- drafts: autosaved edits on the mothership ----------------------------------------------------

/// Largest draft, and the most drafted bytes one repository keeps.
const DRAFT_BYTES: usize = 1024 * 1024;
const DRAFTS_PER_REPO_BYTES: usize = 20 * 1024 * 1024;

static DRAFTS_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn drafts_file(app: &Shared, repo: &str) -> std::path::PathBuf {
    app.cfg.data_dir.join("drafts").join(format!("{repo}.json"))
}

fn editor_settings_file(app: &Shared) -> std::path::PathBuf {
    app.cfg.data_dir.join("drafts").join("settings.json")
}

/// Whether the editor autosaves drafts on this mothership (default on).
pub fn autosave_on(app: &Shared) -> bool {
    std::fs::read(editor_settings_file(app))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v["autosave"].as_bool())
        .unwrap_or(true)
}

pub async fn editor_settings(State(app): State<Shared>) -> Json<Value> {
    Json(json!({"autosave": autosave_on(&app)}))
}

#[derive(Deserialize)]
pub struct EditorSettings {
    pub autosave: bool,
}

pub async fn put_editor_settings(State(app): State<Shared>, Json(req): Json<EditorSettings>) -> ApiResult<Value> {
    let path = editor_settings_file(&app);
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    crate::util::write_atomic(&path, &serde_json::to_vec_pretty(&json!({"autosave": req.autosave}))?).await?;
    Ok(Json(json!({"autosave": req.autosave})))
}

fn read_drafts(app: &Shared, repo: &str) -> BTreeMap<String, Value> {
    std::fs::read(drafts_file(app, repo))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

async fn write_drafts(app: &Shared, repo: &str, drafts: &BTreeMap<String, Value>) -> Result<()> {
    let path = drafts_file(app, repo);
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    if drafts.is_empty() {
        let _ = tokio::fs::remove_file(&path).await;
        return Ok(());
    }
    crate::util::write_atomic(&path, &serde_json::to_vec(drafts)?).await?;
    // Drafts are unpublished work: readable by the operator only.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        if let Some(dir) = path.parent() {
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
    }
    Ok(())
}

/// The key a draft is stored under: its ref and path.
pub fn draft_key(reference: &str, path: &str) -> String {
    format!("{reference}\u{0}{path}")
}

#[derive(Deserialize)]
pub struct DraftRequest {
    #[serde(rename = "ref")]
    pub reference: String,
    pub path: String,
    pub content: String,
    pub base_sha: String,
}

pub async fn list_drafts(
    State(app): State<Shared>,
    Path((owner, name)): Path<(String, String)>,
    Query(q): Query<RefQuery>,
) -> ApiResult<Value> {
    let repo = repo_of(&owner, &name)?;
    let drafts: Vec<Value> = read_drafts(&app, &repo)
        .into_values()
        .filter(|d| q.reference.as_deref().is_none_or(|r| d["ref"].as_str() == Some(r)))
        .collect();
    Ok(Json(json!({"repo": repo, "autosave": autosave_on(&app), "drafts": drafts})))
}

pub async fn put_draft(
    State(app): State<Shared>,
    Path((owner, name)): Path<(String, String)>,
    Json(req): Json<DraftRequest>,
) -> ApiResult<Value> {
    let repo = repo_of(&owner, &name)?;
    if !autosave_on(&app) {
        return Err(client_error(StatusCode::CONFLICT, "autosave is off on this mothership"));
    }
    let reference = valid_ref(&req.reference).ok_or_else(|| bad("invalid ref"))?;
    let path = valid_edit_path(&req.path).ok_or_else(|| bad("refused path"))?;
    if req.content.len() > DRAFT_BYTES {
        return Err(bad("the draft is larger than 1 MB"));
    }
    if !(req.base_sha.len() == 40 && req.base_sha.chars().all(|c| c.is_ascii_hexdigit())) {
        return Err(bad("base_sha must be a full commit sha"));
    }
    let _guard = DRAFTS_LOCK.lock().await;
    let mut drafts = read_drafts(&app, &repo);
    let key = draft_key(&reference, &path);
    let saved_at = chrono::Utc::now();
    drafts.insert(
        key.clone(),
        json!({"ref": reference, "path": path, "content": req.content, "base_sha": req.base_sha, "saved_at": saved_at}),
    );
    let total: usize = drafts.values().map(|d| d["content"].as_str().map_or(0, str::len)).sum();
    if total > DRAFTS_PER_REPO_BYTES {
        return Err(client_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "this repository's drafts exceed 20 MB; create a PR or discard some",
        ));
    }
    write_drafts(&app, &repo, &drafts).await?;
    Ok(Json(json!({"saved_at": saved_at})))
}

#[derive(Deserialize)]
pub struct DraftDelete {
    #[serde(rename = "ref")]
    pub reference: String,
    /// One path, or every draft on the ref when absent.
    pub path: Option<String>,
}

pub async fn delete_draft(
    State(app): State<Shared>,
    Path((owner, name)): Path<(String, String)>,
    Query(q): Query<DraftDelete>,
) -> ApiResult<Value> {
    let repo = repo_of(&owner, &name)?;
    let reference = valid_ref(&q.reference).ok_or_else(|| bad("invalid ref"))?;
    let _guard = DRAFTS_LOCK.lock().await;
    let mut drafts = read_drafts(&app, &repo);
    let before = drafts.len();
    match q.path.as_deref() {
        Some(p) => {
            let path = valid_edit_path(p).ok_or_else(|| bad("refused path"))?;
            drafts.remove(&draft_key(&reference, &path));
        }
        None => drafts.retain(|_, d| d["ref"].as_str() != Some(reference.as_str())),
    }
    let removed = before - drafts.len();
    write_drafts(&app, &repo, &drafts).await?;
    Ok(Json(json!({"removed": removed})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refs_and_paths_are_validated() {
        assert!(valid_ref("main").is_some() && valid_ref("colonizer/issue-12-ab").is_some() && valid_ref("a1b2c3d").is_some());
        for bad in ["", "-x", "/a", "a/", "a..b", "a//b", "x@{1}", "a b", "a.lock", "a;rm"] {
            assert!(valid_ref(bad).is_none(), "{bad}");
        }
        assert!(valid_edit_path("src/main.rs").is_some());
        for bad in [".git/config", "a/.git/hooks/x", "../x", "/etc/passwd", "a/./b", ".GIT/HEAD"] {
            assert!(valid_edit_path(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn languages_vendoring_and_line_counts() {
        assert_eq!(language_of("crates/a/src/main.rs"), Some("Rust"));
        assert_eq!(language_of("web/App.tsx"), Some("TSX"));
        assert_eq!(language_of("Dockerfile"), Some("Dockerfile"));
        assert_eq!(language_of("LICENSE"), None);
        assert!(
            is_vendored("web/node_modules/x/index.js")
                && is_vendored("Cargo.lock")
                && is_vendored("a/b.min.js")
                && is_vendored("target/debug/x.rs")
        );
        assert!(!is_vendored("src/vendors.rs"));
        assert_eq!(count_lines("a\n\n  \nb\n"), (2, 2));
        let json = loc_json("main", "abc", HashMap::from([("Rust", (2, 100, 10)), ("TSX", (1, 40, 2))]));
        assert_eq!(json["total"], 140);
        assert_eq!(json["by_language"][0]["name"], "Rust");
    }

    #[test]
    fn coverage_formats_parse() {
        assert_eq!(
            lcov_percent("SF:a\nLF:10\nLH:5\nend_of_record\nSF:b\nLF:10\nLH:10\n"),
            Some(75.0)
        );
        assert_eq!(lcov_percent("nothing"), None);
        assert_eq!(istanbul_percent(r#"{"total":{"lines":{"pct":81.5}}}"#), Some(81.5));
        assert_eq!(
            cobertura_percent(r#"<?xml version="1.0"?><coverage line-rate="0.642" branch-rate="0"></coverage>"#)
                .map(|p| (p * 10.0).round() / 10.0),
            Some(64.2)
        );
        assert_eq!(
            json_percent(r#"{"data":[{"totals":{"lines":{"percent":70.25}}}]}"#),
            Some(70.25)
        );
        assert_eq!(json_percent(r#"{"coverage":55.5}"#), Some(55.5));
        assert_eq!(coverage_from("lcov.info", "LF:4\nLH:1\n"), Some((25.0, "lcov")));
        assert_eq!(coverage_from("notes.txt", "LF:4\nLH:1\n"), None);
    }

    #[test]
    fn git_outputs_parse() {
        let (kind, sha, size, path) =
            parse_ls_tree_line("100644 blob 3b18e512dba79e4c8300dd08aeb37f8e728b8dad      12\tsrc/a b.rs").unwrap();
        assert_eq!(
            (kind.as_str(), sha.len(), size, path.as_str()),
            ("blob", 40, 12, "src/a b.rs")
        );
        let log = parse_log("abc\u{1f}Ann\u{1f}2026-09-24T10:00:00+00:00\u{1f}Fix it\n");
        assert_eq!(log[0]["author"], "Ann");
        let refs = parse_refs("origin/HEAD\u{1f}x\u{1f}d\u{1f}a\u{1f}s\norigin/main\u{1f}abc\u{1f}2026\u{1f}Ann\u{1f}Msg\n");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].0, "main");
        let sha = "a".repeat(40);
        let porcelain = format!(
            "{sha} 1 1 2\nauthor Ann\nauthor-time 1700000000\nsummary First\nfilename x\n\tline one\n{sha} 2 2\n\tline two\n"
        );
        let (commits, lines) = parse_blame(&porcelain);
        assert_eq!(lines, vec![sha.clone(), sha.clone()]);
        assert_eq!(commits[&sha]["author"], "Ann");
        assert_eq!(commits[&sha]["time"], 1_700_000_000);
    }

    fn req(branch: &str, files: Vec<(&str, &str)>) -> EditsRequest {
        EditsRequest {
            base: None,
            branch: branch.into(),
            message: "Fix".into(),
            title: "Fix".into(),
            body: String::new(),
            files: files
                .into_iter()
                .map(|(p, c)| EditFile {
                    path: p.into(),
                    content: c.into(),
                })
                .collect(),
        }
    }

    #[test]
    fn edits_are_checked_before_anything_is_written() {
        assert!(check_edits(&req("edit/x", vec![("src/a.rs", "x")]), "main").is_ok());
        assert!(
            check_edits(&req("main", vec![("src/a.rs", "x")]), "main").is_err(),
            "not the base"
        );
        assert!(check_edits(&req("edit/x", vec![(".git/config", "x")]), "main").is_err());
        assert!(
            check_edits(&req("edit/x", vec![("a", "x"), ("a", "y")]), "main").is_err(),
            "twice"
        );
        assert!(check_edits(&req("edit/x", vec![]), "main").is_err());
        let big = "x".repeat(EDIT_FILE_BYTES + 1);
        assert!(check_edits(&req("edit/x", vec![("a", big.as_str())]), "main").is_err());
    }

    #[tokio::test]
    async fn edits_commit_on_a_new_branch_and_refuse_symlinks() {
        use crate::tests::test_app;
        let root = std::env::temp_dir().join(format!("colonizer-code-edit-{}", crate::util::short_id()));
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let run = |dir: &std::path::Path, args: &[&str]| {
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        };
        run(&src, &["init", "-q", "-b", "main"]);
        std::fs::write(src.join("a.txt"), "one\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/tmp", src.join("escape")).unwrap();
        run(&src, &["add", "-A"]);
        run(
            &src,
            &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"],
        );
        let bare = root.join("bare.git");
        let ok = std::process::Command::new("git")
            .args(["clone", "-q", "--bare"])
            .arg(&src)
            .arg(&bare)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let sha = String::from_utf8(
            std::process::Command::new("git")
                .arg("--git-dir")
                .arg(&bare)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        let app = test_app(&root.join("app"));

        let wt = root.join("wt1");
        commit_edit(
            &app,
            &bare,
            &wt,
            &sha,
            "edit/x",
            &[("a.txt".into(), "two\n".into()), ("new/b.txt".into(), "b\n".into())],
            "Ann",
            "1+ann@users.noreply.github.com",
            "Edit a",
        )
        .await
        .unwrap();
        let log = String::from_utf8(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&wt)
                .args(["log", "-1", "--format=%an <%ae>|%s|%D"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        assert!(
            log.starts_with("Ann <1+ann@users.noreply.github.com>|Edit a|HEAD -> edit/x"),
            "{log}"
        );
        assert!(!log.contains("Claude"), "no AI attribution: {log}");
        assert_eq!(std::fs::read_to_string(wt.join("a.txt")).unwrap(), "two\n");

        #[cfg(unix)]
        {
            let wt2 = root.join("wt2");
            let err = commit_edit(
                &app,
                &bare,
                &wt2,
                &sha,
                "edit/y",
                &[("escape/pwned.txt".into(), "x".into())],
                "Ann",
                "a@b",
                "Bad",
            )
            .await
            .unwrap_err();
            assert!(format!("{err:#}").contains("symlink"), "{err:#}");
            assert!(!std::path::Path::new("/tmp/pwned.txt").exists());
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn draft_keys_separate_ref_and_path() {
        assert_ne!(draft_key("a/b", "c"), draft_key("a", "b/c"));
    }

    #[test]
    fn the_ask_prompt_cuts_and_names_the_selection() {
        let p = ask_prompt("a.rs", "fn main() {}", Some((3, 9)), "what does it do?");
        assert!(p.contains("File: a.rs") && p.contains("lines 3-9") && p.contains("what does it do?"));
        let long = "é".repeat(ASK_CONTEXT_LIMIT);
        assert!(ask_prompt("a.rs", &long, None, "q").contains("cut to its first 60 KB"));
    }
}
