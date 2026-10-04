//! The route table, snapshotted: every API route's method and path, what an unauthenticated
//! request gets, what the scoped-token allowlist (`api_tokens::classify`) decides for it, and the
//! activity-log kind it records. Rebuilding how the router is assembled must leave these files
//! unchanged; a pull request that adds or changes a route updates its module's file in the same
//! diff, so the change to the API's surface and its access rules is visible in review — and two
//! features never edit the same snapshot.
//!
//! One snapshot per source module, under `crates/colonizer/routes/`: `src/maps.rs` is
//! `routes/maps.snap`, `src/sessions/api.rs` is `routes/sessions.api.snap`. A module with no API
//! routes has no file; the ones left over from routes it no longer serves are stale and must go.
//!
//! Regenerate with `UPDATE_ROUTE_SNAPSHOT=1 cargo test -p colonizer-harness route_table`.
//!
//! How it reads the table without running a handler: the candidate paths are every `.route("/api…"`
//! or `.route("/uhp…"` literal in the crate's sources, and each is asked with `OPTIONS`, a method
//! no API route registers. The router answers from the matched route's method fallback, a 405 whose `Allow`
//! header lists the methods the route has, and a probe route layer reports which route matched. Test
//! code is skipped: a test router in `api_tokens.rs` naming `/api/status` does not make it
//! `api_tokens`'s route, so a path belongs to the file that really serves it.
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use axum::{
    body::Body,
    extract::{MatchedPath, Request},
    http::{HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::Response,
};
use tower::ServiceExt as _;

/// The directory of per-module snapshots, under the crate root.
const SNAPSHOT_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/routes");

/// Every string literal passed to `.route(` under `src/`, that starts with `/api` or `/uhp`,
/// grouped by the module it was written in. Each path is attributed to the first *real* module
/// (sorted) that mentions it; a path only test code mentions falls back to the first test module,
/// so the union of the snapshots keeps every line.
fn candidate_paths() -> BTreeMap<String, BTreeSet<String>> {
    fn walk(
        base: &Path,
        dir: &Path,
        real: &mut BTreeMap<String, BTreeSet<String>>,
        test: &mut BTreeMap<String, BTreeSet<String>>,
    ) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(base, &path, real, test);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                let module = module_name(base, &path);
                // A `tests.rs`/`*_tests.rs` file is all test code; elsewhere, everything from the
                // `#[cfg(test)] mod` block on is.
                let stem = path.file_stem().unwrap().to_string_lossy();
                let cut = if stem == "tests" || stem.ends_with("_tests") {
                    0
                } else {
                    test_code_at(&text).unwrap_or(text.len())
                };
                let mut found_real = Vec::new();
                let mut found_test = Vec::new();
                for (at, _) in text.match_indices(".route(") {
                    let rest = text[at + ".route(".len()..].trim_start();
                    if let Some(literal) = rest.strip_prefix('"')
                        && let Some(end) = literal.find('"')
                        && (literal[..end].starts_with("/api") || literal[..end].starts_with("/uhp"))
                    {
                        if at < cut {
                            found_real.push(literal[..end].to_string());
                        } else {
                            found_test.push(literal[..end].to_string());
                        }
                    }
                }
                for path in found_real {
                    real.entry(module.clone()).or_default().insert(path);
                }
                for path in found_test {
                    test.entry(module.clone()).or_default().insert(path);
                }
            }
        }
    }
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let (mut real, mut test) = (BTreeMap::new(), BTreeMap::new());
    walk(&src, &src, &mut real, &mut test);
    real.retain(|_, paths| !paths.is_empty());
    test.retain(|_, paths| !paths.is_empty());

    // The first module (sorted) that mentions each path holds it, so it appears exactly once — and
    // a path a real module serves is never captured by a test router that also names it.
    let mut seen = BTreeSet::new();
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (module, paths) in real.into_iter().chain(test) {
        for path in paths {
            if seen.insert(path.clone()) {
                out.entry(module.clone()).or_default().insert(path);
            }
        }
    }
    out
}

/// Where a source file's test code begins: the first `#[cfg(test)]` that is immediately followed
/// (over whitespace and any further attributes) by a `mod` item — the `#[cfg(test)] mod tests`
/// shape. An `#[cfg(test)]` on a function is not a test *module* and does not move the boundary.
/// `None` when the file has no such block.
fn test_code_at(text: &str) -> Option<usize> {
    const ATTR: &str = "#[cfg(test)]";
    let mut from = 0;
    while let Some(found) = text[from..].find(ATTR) {
        let at = from + found;
        let mut rest = text[at + ATTR.len()..].trim_start();
        // Skip any further attributes before the item's own keyword.
        while let Some(body) = rest.strip_prefix("#[") {
            match body.find(']') {
                Some(end) => rest = body[end + 1..].trim_start(),
                None => break,
            }
        }
        if rest
            .strip_prefix("mod")
            .is_some_and(|after| !after.starts_with(|c: char| c.is_alphanumeric() || c == '_'))
        {
            return Some(at);
        }
        from = at + ATTR.len();
    }
    None
}

/// The snapshot's name for a source file: its path under `src/` without `.rs`, a trailing `/mod`
/// dropped, `/` replaced by `.` (`maps.rs` → `maps`, `server/mod.rs` → `server`).
fn module_name(base: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(base).unwrap().with_extension("");
    let rel = rel.to_string_lossy().replace('\\', "/");
    rel.strip_suffix("/mod").unwrap_or(&rel).replace('/', ".")
}

/// A request path for a route template: each `{param}` becomes `x`.
fn concrete(template: &str) -> String {
    template
        .split('/')
        .map(|seg| if seg.starts_with('{') { "x" } else { seg })
        .collect::<Vec<_>>()
        .join("/")
}

/// Stamps the matched route on the response, then lets the request go on to the route's method
/// fallback (the only thing an `OPTIONS` request reaches).
async fn probe(req: Request, next: Next) -> Response {
    let matched = req.extensions().get::<MatchedPath>().map(|m| m.as_str().to_string());
    let mut res = next.run(req).await;
    if let Some(m) = matched {
        res.headers_mut().insert("x-route", HeaderValue::from_str(&m).unwrap());
    }
    res
}

fn request(method: Method, uri: &str) -> Request {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "127.0.0.1:7878")
        .body(Body::empty())
        .unwrap()
}

/// The table as the snapshots hold it: one line per method and route, keyed by the module that
/// serves them and sorted by path within each.
async fn route_table() -> BTreeMap<String, Vec<String>> {
    let root = std::env::temp_dir().join(format!("colonizer-routes-{}", crate::util::short_id()));
    let app = crate::tests::test_app(&root);
    let probed = crate::server::api_routes()
        .route_layer(middleware::from_fn(probe))
        .with_state(app.clone());
    let full = crate::server::router(&app);

    let mut table: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (module, templates) in candidate_paths() {
        let mut lines = Vec::new();
        for template in templates {
            let uri = concrete(&template);
            let res = probed.clone().oneshot(request(Method::OPTIONS, &uri)).await.unwrap();
            if res.headers().get("x-route").and_then(|v| v.to_str().ok()) != Some(template.as_str()) {
                // Not a route of the real API: a test router's path, or one another template shadows.
                continue;
            }
            assert_eq!(
                res.status(),
                StatusCode::METHOD_NOT_ALLOWED,
                "OPTIONS {template} reached a handler"
            );
            let allow = res
                .headers()
                .get(header::ALLOW)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default();
            let mut methods: Vec<&str> = allow
                .split(',')
                .map(str::trim)
                .filter(|m| !m.is_empty() && *m != "HEAD")
                .collect();
            methods.sort();
            for method in methods {
                let method = Method::from_bytes(method.as_bytes()).unwrap();
                // host_guard answers every unauthenticated API request before routing, except the
                // public status (reduced to an allowlist; its own test covers the body).
                let unauth = if method == Method::GET && template == "/api/status" {
                    "public".to_string()
                } else {
                    let res = full.clone().oneshot(request(method.clone(), &uri)).await.unwrap();
                    res.status().as_u16().to_string()
                };
                let token = crate::api_tokens::describe_need(&method, &uri);
                let activity = crate::activity::recorded_kind(&method, &template).unwrap_or("-");
                lines.push(format!(
                    "{template:<52} {:<6} unauth={unauth:<6} token={token:<16} activity={activity}",
                    method.as_str()
                ));
            }
        }
        if !lines.is_empty() {
            lines.sort();
            table.insert(module, lines);
        }
    }
    let _ = std::fs::remove_dir_all(&root);
    table
}

/// A snapshot's path for a module.
fn snapshot_path(module: &str) -> PathBuf {
    Path::new(SNAPSHOT_DIR).join(format!("{module}.snap"))
}

/// A snapshot's contents: the lines, newline-terminated.
fn snapshot_body(lines: &[String]) -> String {
    format!("{}\n", lines.join("\n"))
}

/// The modules with a snapshot on disk.
fn snapshotted_modules() -> BTreeSet<String> {
    std::fs::read_dir(SNAPSHOT_DIR)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter_map(|name| name.strip_suffix(".snap").map(str::to_string))
        .collect()
}

#[tokio::test]
async fn the_route_table_matches_its_snapshot() {
    let table = route_table().await;
    let total: usize = table.values().map(Vec::len).sum();
    assert!(
        total > 100,
        "the probe found too few routes to be the real table:\n{table:#?}"
    );
    let dir = Path::new(SNAPSHOT_DIR);
    if std::env::var_os("UPDATE_ROUTE_SNAPSHOT").is_some() {
        std::fs::create_dir_all(dir).unwrap();
        for module in snapshotted_modules() {
            if !table.contains_key(&module) {
                std::fs::remove_file(snapshot_path(&module)).unwrap();
            }
        }
        for (module, lines) in &table {
            std::fs::write(snapshot_path(module), snapshot_body(lines)).unwrap();
        }
        return;
    }
    let mut broken = Vec::new();
    for (module, lines) in &table {
        let want = snapshot_body(lines);
        let got = std::fs::read_to_string(snapshot_path(module)).unwrap_or_default();
        if got != want {
            let old: BTreeSet<&str> = got.lines().collect();
            let new: BTreeSet<&str> = lines.iter().map(String::as_str).collect();
            let gone: Vec<_> = old.difference(&new).collect();
            let added: Vec<_> = new.difference(&old).collect();
            broken.push(format!("{module}.snap\ngone:\n{gone:#?}\nadded:\n{added:#?}"));
        }
    }
    for module in snapshotted_modules() {
        if !table.contains_key(&module) {
            broken.push(format!("{module}.snap is stale"));
        }
    }
    if !broken.is_empty() {
        panic!(
            "the route table changed; if that is intended, regenerate the snapshots with \
             UPDATE_ROUTE_SNAPSHOT=1 cargo test -p colonizer-harness route_table\n{}",
            broken.join("\n")
        );
    }
}
