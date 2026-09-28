//! The colony's artifacts (docs/protocol.md §7.5, issue #651): the files a colony's agent left
//! in `<session dir>/out/`, read back over `/api/sessions/{id}/files…` and, in UHP shapes with
//! the `UHP-Version` header, over the `/uhp/v1` aliases. Read-only, top-level regular files
//! only, with the same no-symlink checks `pr.md` is read under. The microVM can write `out`
//! while the colony is live, so the final path component is never trusted twice: the listing
//! lstats it, the reads open it with `O_NOFOLLOW` and fstat before the first byte. The
//! directories on the way there, `out` itself included, are followed as they are.

use super::*;
use axum::{
    extract::Request,
    http::{HeaderMap, HeaderValue},
    middleware::Next,
};

/// The largest artifact one read answers with (§7.5's `file_too_large` cap): these answers are
/// read into memory whole, and agent output past 16 MiB belongs in the repository anyway.
pub(crate) const ARTIFACT_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// The most artifact bytes one archive request reads into memory: the files a tar would hold
/// are capped in sum, not only each on its own, or an `out/` could pin unbounded memory a file
/// at a time under the per-file cap.
pub(crate) const ARCHIVE_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// The routes of this module, merged into the session routes: the `/api` originals and the UHP
/// aliases §7.1 serves beside them, one stamping layer over the aliases so no handler can
/// forget `UHP-Version`. Each read registers once per surface on the same handler — the path
/// prefix decides the error shapes.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing::get;
    let api = axum::Router::new()
        .route("/api/sessions/{id}/files", get(artifacts))
        .route("/api/sessions/{id}/files/archive", get(artifacts_archive))
        .route("/api/sessions/{id}/files/{name}/content", get(artifact_content));
    let uhp = axum::Router::new()
        .route("/uhp/v1/sessions", get(uhp_sessions))
        .route("/uhp/v1/sessions/{id}/files", get(artifacts))
        .route("/uhp/v1/sessions/{id}/files/archive", get(artifacts_archive))
        .route("/uhp/v1/sessions/{id}/files/{name}/content", get(artifact_content))
        .route("/uhp/v1/containers/{cid}/files/{fid}/content", get(uhp_container_content))
        .route_layer(axum::middleware::from_fn(stamp));
    api.merge(uhp)
}

/// The stamping layer over the `/uhp` aliases (§7.1): `UHP-Version` on every answer.
async fn stamp(req: Request, next: Next) -> Response {
    crate::uhp::stamped(next.run(req).await)
}

/// The container id §7.5 puts in every artifact row: the colony's id in a `cntr_` wrapper.
fn container_id(id: &str) -> String {
    format!("cntr_{id}")
}

/// The colony a files route names, or `None` when there is none for this caller — the handler
/// answers [`no_such_session`] then. An unknown id and one outside a scoped token's org/repo
/// limits read the same, exactly as the colony routes hide them (issue #508).
async fn visible_session(
    app: &App,
    id: &str,
    scoped: Option<&axum::Extension<crate::api_tokens::ScopedToken>>,
) -> Option<Session> {
    match app.session(id).await {
        Some(session) if scoped.is_none_or(|token| token.covers(&session.org, &session.repo)) => Some(session),
        _ => None,
    }
}

/// The **404** `session_not_found` a files route answers when [`visible_session`] found nothing.
fn no_such_session(uhp: bool, headers: &HeaderMap) -> Response {
    crate::uhp::error_for(
        uhp,
        headers,
        StatusCode::NOT_FOUND,
        "session_not_found",
        "no such session",
        None,
    )
}

/// One artifact row (§7.5): the UHP file object over a name in the colony's `out/`.
fn artifact_row(id: &str, name: &str, meta: &std::fs::Metadata) -> Value {
    json!({
        "id": name,
        "object": "file",
        "container_id": container_id(id),
        "filename": name,
        "bytes": meta.len(),
        "created_at": mtime(meta),
    })
}

/// The artifacts of one colony's `out/` directory: top-level regular files, sorted by name. A
/// symlink, a directory, any other file type and a non-UTF-8 name are not artifacts and are left
/// out; a colony with no `out/` yet simply has none.
fn artifact_files(app: &App, id: &str) -> Vec<(String, std::fs::Metadata)> {
    let Ok(entries) = std::fs::read_dir(app.session_dir(id).join("out")) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        // The name is the artifact id (§7.5), so it has to survive the JSON row: a name the VM
        // wrote that is not UTF-8 stays unreadable rather than being mangled into another file.
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else { continue };
        // The link's own metadata, never a target's: a symlinked artifact would name a host
        // file the microVM never wrote, so it is skipped, not followed.
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        files.push((name.to_string(), meta));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}

/// The §7.5 listing both surfaces answer with: the colony's artifact rows.
fn listing(app: &App, id: &str) -> Response {
    let rows: Vec<Value> = artifact_files(app, id)
        .iter()
        .map(|(name, meta)| artifact_row(id, name, meta))
        .collect();
    Json(json!({ "files": rows })).into_response()
}

/// Whether the request named the `/uhp` alias (§7.1): one handler per resource serves both
/// names, and the path prefix decides the error shapes — the envelope always under `/uhp`,
/// beside `/api` only when the request itself speaks UHP.
fn uhp_surface(path: &str) -> bool {
    path.starts_with("/uhp/")
}

/// `GET …/sessions/{id}/files` (§7.5), both names for it: the colony's artifacts,
/// `{"files": [file…]}`.
async fn artifacts(
    State(app): State<Shared>,
    Path(id): Path<String>,
    uri: axum::http::Uri,
    headers: HeaderMap,
    scoped: Option<axum::Extension<crate::api_tokens::ScopedToken>>,
) -> Response {
    let uhp = uhp_surface(uri.path());
    if visible_session(&app, &id, scoped.as_ref()).await.is_none() {
        return no_such_session(uhp, &headers);
    }
    listing(&app, &id)
}

/// Whether a path segment can name an artifact: exactly one component, neither `.` nor `..`, no
/// separator of either flavour and no NUL (§7.5: read-only over `out/`, never through it). The
/// router has already percent-decoded the segment, so an encoded `%2f` arrives here as `/` and
/// is refused by the same check.
fn is_artifact_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', '\0'])
}

/// An attachment filename a header line can carry: the validated name with the bytes that would
/// break the header — quotes, backslashes, control characters — flattened to `_`.
fn header_filename(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '"' | '\\' => '_',
            _ if c.is_ascii_graphic() || c == ' ' => c,
            _ => '_',
        })
        .collect()
}

/// The attachment headers every artifact answer carries (§7.5): saved to disk, and never
/// sniffed into something the browser would run.
fn attachment_headers(response: &mut Response, content_type: &'static str, filename: &str) {
    let headers = response.headers_mut();
    headers.insert(axum::http::header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(
        axum::http::header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{}\"", header_filename(filename)))
            .unwrap_or(HeaderValue::from_static("attachment")),
    );
    headers.insert(
        axum::http::header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
}

/// One artifact's bytes: opened through the single no-follow handle, read within
/// [`ARTIFACT_MAX_BYTES`]. Past the cap answers **413** `file_too_large` with `detail.max_bytes`
/// (§7.5); anything else — a name not in `out/`, a symlink swapped in after the listing, a file
/// that vanished — reads as a file that is not there.
fn artifact_bytes(path: &std::path::Path, name: &str, uhp: bool, headers: &HeaderMap) -> Response {
    match crate::github::read_regular_file_bytes(path, ARTIFACT_MAX_BYTES) {
        Ok(bytes) => {
            let mut response = (StatusCode::OK, bytes).into_response();
            attachment_headers(&mut response, "application/octet-stream", name);
            response
        }
        Err(crate::github::RegularFileRead::TooLarge(cap)) => crate::uhp::error_for(
            uhp,
            headers,
            StatusCode::PAYLOAD_TOO_LARGE,
            "file_too_large",
            format!("\"{name}\" is larger than the {cap}-byte artifact cap"),
            Some(json!({ "max_bytes": cap })),
        ),
        Err(crate::github::RegularFileRead::Unreadable(_)) => crate::uhp::error_for(
            uhp,
            headers,
            StatusCode::NOT_FOUND,
            "file_not_found",
            format!("no artifact named \"{name}\""),
            None,
        ),
    }
}

/// The file a content route names, or the **404** `file_not_found` it answers when the name
/// cannot name one.
fn refused_name(name: &str, uhp: bool, headers: &HeaderMap) -> Option<Response> {
    if is_artifact_name(name) {
        return None;
    }
    Some(crate::uhp::error_for(
        uhp,
        headers,
        StatusCode::NOT_FOUND,
        "file_not_found",
        format!("\"{name}\" does not name an artifact"),
        None,
    ))
}

/// `GET …/sessions/{id}/files/{name}/content` (§7.5), both names: the raw artifact.
async fn artifact_content(
    State(app): State<Shared>,
    Path((id, name)): Path<(String, String)>,
    uri: axum::http::Uri,
    headers: HeaderMap,
    scoped: Option<axum::Extension<crate::api_tokens::ScopedToken>>,
) -> Response {
    let uhp = uhp_surface(uri.path());
    if visible_session(&app, &id, scoped.as_ref()).await.is_none() {
        return no_such_session(uhp, &headers);
    }
    if let Some(response) = refused_name(&name, uhp, &headers) {
        return response;
    }
    artifact_bytes(&app.session_dir(&id).join("out").join(&name), &name, uhp, &headers)
}

/// `GET …/sessions/{id}/files/archive` (§7.5), both names: every listed artifact as one plain
/// tar, named for the colony — or the refusals, artifacts adding up past [`ARCHIVE_MAX_BYTES`]
/// reading **413** `file_too_large` and an `out/` that stopped being readable mid-build a
/// server error.
async fn artifacts_archive(
    State(app): State<Shared>,
    Path(id): Path<String>,
    uri: axum::http::Uri,
    headers: HeaderMap,
    scoped: Option<axum::Extension<crate::api_tokens::ScopedToken>>,
) -> Response {
    let uhp = uhp_surface(uri.path());
    if visible_session(&app, &id, scoped.as_ref()).await.is_none() {
        return no_such_session(uhp, &headers);
    }
    match artifact_tar(&app, &id, ARCHIVE_MAX_BYTES) {
        Ok(Archive::Tar(bytes)) => {
            let mut response = (StatusCode::OK, bytes).into_response();
            attachment_headers(&mut response, "application/x-tar", &format!("{id}-files.tar"));
            response
        }
        Ok(Archive::TooLarge(cap)) => crate::uhp::error_for(
            uhp,
            &headers,
            StatusCode::PAYLOAD_TOO_LARGE,
            "file_too_large",
            format!("the colony's artifacts add up past the {cap}-byte archive cap"),
            Some(json!({ "max_bytes": cap })),
        ),
        Err(_) => crate::uhp::error_for(
            uhp,
            &headers,
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "could not read the colony's artifacts",
            None,
        ),
    }
}

/// What building the archive ends in: the tar, or the caller's mistake — the artifacts that
/// would go in add up past the cap, refused before a byte is read.
enum Archive {
    Tar(Vec<u8>),
    TooLarge(u64),
}

/// The tar itself, in memory like the log archive's bundles (`archive.rs`): each file through
/// its own no-follow handle, so a symlink swapped in between listing and read is refused, not
/// followed, and a file that grew past the per-file cap between the two looks is skipped. The
/// tests build a small `cap`; production passes [`ARCHIVE_MAX_BYTES`].
fn artifact_tar(app: &App, id: &str, cap: u64) -> std::io::Result<Archive> {
    let files = artifact_files(app, id);
    // What one request would read in full: the files the tar would actually hold (each already
    // within the per-file cap). Past the cap the archive is refused, not built short.
    let archivable: u64 = files
        .iter()
        .filter(|(_, meta)| meta.len() <= ARTIFACT_MAX_BYTES)
        .map(|(_, meta)| meta.len())
        .sum();
    if archivable > cap {
        return Ok(Archive::TooLarge(cap));
    }
    let mut tar = tar::Builder::new(Vec::new());
    for (name, meta) in files {
        let bytes = match crate::github::read_regular_file_bytes(&app.session_dir(id).join("out").join(&name), ARTIFACT_MAX_BYTES)
        {
            Ok(bytes) => bytes,
            Err(crate::github::RegularFileRead::TooLarge(_) | crate::github::RegularFileRead::Unreadable(_)) => continue,
        };
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(mtime(&meta));
        header.set_cksum();
        tar.append_data(&mut header, &name, bytes.as_slice())?;
    }
    tar.finish()?;
    Ok(Archive::Tar(tar.into_inner()?))
}

/// A file's mtime in unix seconds, 0 when the clock cannot say.
fn mtime(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|age| age.as_secs())
        .unwrap_or(0)
}

// -- the UHP-only routes (§7.1): the session page and the container id ---------------------------

/// `GET /uhp/v1/sessions`: the colony list as a page, always — §7 wants the `sessions` array and
/// the `next_cursor` marker on every answer, so a client never guesses the end of the list.
async fn uhp_sessions(
    State(app): State<Shared>,
    Query(query): Query<ListQuery>,
    headers: HeaderMap,
    scoped: Option<axum::Extension<crate::api_tokens::ScopedToken>>,
) -> Response {
    let visible = visible_sessions(&app, scoped.as_ref()).await;
    paged_list(&app, visible, &query, true, &headers).await
}

/// `GET /uhp/v1/containers/{cid}/files/{fid}/content` (§7.5): the download by container id —
/// the colony's id in its `cntr_` wrapper, anything else reading as no colony at all. The same
/// checks as [`artifact_content`] once the wrapper resolves.
async fn uhp_container_content(
    State(app): State<Shared>,
    Path((cid, fid)): Path<(String, String)>,
    headers: HeaderMap,
    scoped: Option<axum::Extension<crate::api_tokens::ScopedToken>>,
) -> Response {
    let Some(id) = cid.strip_prefix("cntr_").filter(|id| !id.is_empty()) else {
        return crate::uhp::uhp_error(StatusCode::NOT_FOUND, "session_not_found", "no such container", None);
    };
    if visible_session(&app, id, scoped.as_ref()).await.is_none() {
        return no_such_session(true, &headers);
    }
    if let Some(response) = refused_name(&fid, true, &headers) {
        return response;
    }
    artifact_bytes(&app.session_dir(id).join("out").join(&fid), &fid, true, &headers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::tests::*;

    /// A read-scoped token, unlimited (empty limits mean no limit of that kind).
    fn scoped_token() -> axum::Extension<crate::api_tokens::ScopedToken> {
        axum::Extension(crate::api_tokens::ScopedToken {
            id: "tok_test".into(),
            name: "watcher".into(),
            scope: crate::api_tokens::Scope::Read,
            orgs: Vec::new(),
            repos: Vec::new(),
            max_concurrent: None,
            budget_usd_per_day: None,
        })
    }

    /// A colony with an `out/` holding `hello.txt`, a subdirectory and a symlink out of it —
    /// the shapes a listing must tell apart.
    async fn colony_with_out(id: &str) -> (Shared, PathBuf) {
        let (app, root) = app_with_colony(id, SessionStatus::Stopped).await;
        let out = app.session_dir(id).join("out");
        tokio::fs::create_dir_all(&out).await.unwrap();
        tokio::fs::write(out.join("hello.txt"), b"hello artifact").await.unwrap();
        tokio::fs::create_dir_all(out.join("nested")).await.unwrap();
        tokio::fs::write(out.join("nested").join("buried.txt"), b"not an artifact")
            .await
            .unwrap();
        tokio::fs::symlink("/etc/hostname", out.join("link.txt")).await.unwrap();
        (app, root)
    }

    async fn body(response: Response) -> Value {
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()))
    }

    /// The handlers read the surface off the request path (§7.1's prefix), so the tests name it
    /// the same way: `uhp = false` asks `/api/…`, `uhp = true` its `/uhp/v1` alias.
    fn surface(path: String) -> axum::http::Uri {
        path.parse().unwrap()
    }

    fn prefix(uhp: bool) -> &'static str {
        if uhp { "/uhp/v1/sessions" } else { "/api/sessions" }
    }

    async fn get_files(app: &Shared, id: &str, uhp: bool) -> Response {
        artifacts(
            State(app.clone()),
            Path(id.into()),
            surface(format!("{}/{id}/files", prefix(uhp))),
            HeaderMap::new(),
            None,
        )
        .await
    }

    async fn get_content(app: &Shared, id: &str, name: &str, uhp: bool) -> Response {
        artifact_content(
            State(app.clone()),
            Path((id.into(), name.into())),
            surface(format!("{}/{id}/files/{name}/content", prefix(uhp))),
            HeaderMap::new(),
            None,
        )
        .await
    }

    async fn get_archive(app: &Shared, id: &str, uhp: bool) -> Response {
        artifacts_archive(
            State(app.clone()),
            Path(id.into()),
            surface(format!("{}/{id}/files/archive", prefix(uhp))),
            HeaderMap::new(),
            None,
        )
        .await
    }

    async fn get_container(app: &Shared, cid: &str, name: &str) -> Response {
        uhp_container_content(State(app.clone()), Path((cid.into(), name.into())), HeaderMap::new(), None).await
    }

    #[tokio::test]
    async fn the_listing_answers_regular_files_only() {
        let (app, root) = colony_with_out("abc").await;
        let response = get_files(&app, "abc", false).await;
        assert_eq!(response.status(), StatusCode::OK);
        let files = body(response).await["files"].as_array().unwrap().clone();
        assert_eq!(files.len(), 1, "dirs and symlinks are not artifacts: {files:?}");
        assert_eq!(files[0]["id"], "hello.txt");
        assert_eq!(files[0]["filename"], "hello.txt");
        assert_eq!(files[0]["object"], "file");
        assert_eq!(files[0]["container_id"], "cntr_abc");
        assert_eq!(files[0]["bytes"], 14);
        assert!(
            files[0]["created_at"].as_u64().unwrap_or(0) > 0,
            "the row carries the file's mtime: {files:?}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// Several names come back sorted, and a colony with no `out/` at all lists empty.
    #[tokio::test]
    async fn the_listing_sorts_by_name_and_answers_empty_without_an_out_dir() {
        let (app, root) = app_with_colony("abc", SessionStatus::Stopped).await;
        let response = get_files(&app, "abc", false).await;
        assert_eq!(
            body(response).await["files"].as_array().unwrap().len(),
            0,
            "no out dir, no artifacts"
        );

        let out = app.session_dir("abc").join("out");
        tokio::fs::create_dir_all(&out).await.unwrap();
        tokio::fs::write(out.join("b.bin"), b"b").await.unwrap();
        tokio::fs::write(out.join("a.bin"), b"a").await.unwrap();
        let names: Vec<String> = body(get_files(&app, "abc", false).await).await["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names, ["a.bin", "b.bin"]);
        let _ = std::fs::remove_dir_all(root);
    }

    /// An unknown colony and one outside a scoped token's limits read the same **404**, with the
    /// code either shape answers with.
    #[tokio::test]
    async fn an_unknown_or_uncovered_colony_is_session_not_found() {
        let (app, root) = colony_with_out("abc").await;
        let response = get_files(&app, "zzz", false).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(body(response).await["code"], "session_not_found");

        let mut token = scoped_token();
        token.0.repos = vec!["other/repo".into()];
        let response = artifacts(
            State(app.clone()),
            Path("abc".into()),
            surface("/api/sessions/abc/files".into()),
            HeaderMap::new(),
            Some(token),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "outside the limits reads as unknown"
        );
        assert_eq!(body(response).await["code"], "session_not_found");

        // The same colony through the UHP alias answers the envelope (the version header is the
        // router layer's, covered by the stamp test below).
        let response = get_files(&app, "zzz", true).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let error = body(response).await["error"].clone();
        assert_eq!(error["code"], "session_not_found");
        assert_eq!(error["type"], "invalid_request_error");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn the_content_route_answers_the_raw_bytes_with_download_headers() {
        let (app, root) = colony_with_out("abc").await;
        let payload: Vec<u8> = (0..=255u8).cycle().take(600).collect();
        tokio::fs::write(app.session_dir("abc").join("out").join("blob.bin"), &payload)
            .await
            .unwrap();

        let response = get_content(&app, "abc", "blob.bin", false).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(axum::http::header::CONTENT_TYPE).unwrap(),
            "application/octet-stream"
        );
        assert_eq!(
            response.headers().get(axum::http::header::CONTENT_DISPOSITION).unwrap(),
            "attachment; filename=\"blob.bin\""
        );
        assert_eq!(
            response.headers().get(axum::http::header::X_CONTENT_TYPE_OPTIONS).unwrap(),
            "nosniff"
        );
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(bytes.to_vec(), payload, "binary artifacts survive byte for byte");

        let response = get_content(&app, "abc", "hello.txt", true).await;
        assert_eq!(response.status(), StatusCode::OK);
        let _ = std::fs::remove_dir_all(root);
    }

    /// Traversal in the artifact name is refused wherever it hides: plain dots, a Windows
    /// separator, and the encoded slash the router decodes before the handler sees it.
    #[tokio::test]
    async fn traversal_names_are_file_not_found() {
        let (app, root) = colony_with_out("abc").await;
        for name in ["../../etc/passwd", "..", ".", "sub\\..", "a/b", "..%2f..%2fetc%2fpasswd"] {
            let response = get_content(&app, "abc", name, false).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{name}");
            assert_eq!(body(response).await["code"], "file_not_found", "{name}");
        }
        // A name carrying NUL is one the wire cannot either: it reaches the handler only as a
        // decoded path segment, so the check is called the same way, name verbatim.
        let response = artifact_content(
            State(app.clone()),
            Path(("abc".into(), "with\0nul".into())),
            surface("/api/sessions/abc/files/x/content".into()),
            HeaderMap::new(),
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(body(response).await["code"], "file_not_found");
        // The same refusals through the container alias, envelope-shaped.
        let response = get_container(&app, "cntr_abc", "../../etc/passwd").await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(body(response).await["error"]["code"], "file_not_found");
        let _ = std::fs::remove_dir_all(root);
    }

    /// A file past the artifact cap answers 413 `file_too_large` with `detail.max_bytes`, and
    /// the tar skips it instead of failing.
    #[tokio::test]
    async fn an_oversize_artifact_is_too_large_and_is_skipped_by_the_archive() {
        let (app, root) = colony_with_out("abc").await;
        let out = app.session_dir("abc").join("out");
        let big = out.join("big.bin");
        std::fs::File::create(&big).unwrap().set_len(ARTIFACT_MAX_BYTES + 1).unwrap();

        let response = get_content(&app, "abc", "big.bin", false).await;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let parsed = body(response).await;
        assert_eq!(parsed["code"], "file_too_large");
        assert_eq!(parsed["detail"], Value::Null, "the string shape carries no detail: {parsed}");

        let response = get_content(&app, "abc", "big.bin", true).await;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(body(response).await["error"]["detail"]["max_bytes"], ARTIFACT_MAX_BYTES);

        // The archive skips the oversize file and still carries the readable one.
        let response = get_archive(&app, "abc", false).await;
        assert_eq!(response.status(), StatusCode::OK);
        let names = tar_names(response).await;
        assert_eq!(names, ["hello.txt"], "oversize skipped: {names:?}");
        let _ = std::fs::remove_dir_all(root);
    }

    /// The tar is a plain one (`application/x-tar`) holding exactly the listed artifacts.
    #[tokio::test]
    async fn the_archive_holds_the_listed_files() {
        let (app, root) = colony_with_out("abc").await;
        let response = get_archive(&app, "abc", false).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(axum::http::header::CONTENT_TYPE).unwrap(),
            "application/x-tar"
        );
        assert_eq!(
            response.headers().get(axum::http::header::CONTENT_DISPOSITION).unwrap(),
            "attachment; filename=\"abc-files.tar\""
        );
        assert_eq!(tar_names(response).await, ["hello.txt"]);
        let _ = std::fs::remove_dir_all(root);
    }

    /// The archive is capped in sum, not only per file: artifacts that would go in adding up
    /// past [`ARCHIVE_MAX_BYTES`] answer **413** `file_too_large` with `detail.max_bytes`
    /// instead of a tar. Sparse files keep the fixture cheap — the cap is checked on the
    /// listing's metadata before a byte is read.
    #[tokio::test]
    async fn an_archive_past_the_aggregate_cap_is_file_too_large() {
        let (app, root) = colony_with_out("abc").await;
        let out = app.session_dir("abc").join("out");
        for name in ["a.bin", "b.bin", "c.bin", "d.bin", "e.bin"] {
            std::fs::File::create(out.join(name))
                .unwrap()
                .set_len(15 * 1024 * 1024)
                .unwrap();
        }

        let response = get_archive(&app, "abc", false).await;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let parsed = body(response).await;
        assert_eq!(parsed["code"], "file_too_large");
        assert_eq!(parsed["detail"], Value::Null, "the string shape carries no detail: {parsed}");

        let response = get_archive(&app, "abc", true).await;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(body(response).await["error"]["detail"]["max_bytes"], ARCHIVE_MAX_BYTES);
        let _ = std::fs::remove_dir_all(root);
    }

    /// The cap is a parameter of the build, so a refusal is testable without the bytes: `out/`
    /// holds 14 archivable bytes here, and a cap under that refuses before the tar exists.
    #[tokio::test]
    async fn the_archive_cap_is_checked_before_any_file_is_read() {
        let (app, root) = colony_with_out("abc").await;
        assert!(matches!(artifact_tar(&app, "abc", 8), Ok(Archive::TooLarge(8))));
        match artifact_tar(&app, "abc", ARCHIVE_MAX_BYTES) {
            Ok(Archive::Tar(bytes)) => assert!(bytes.len() > 14, "hello.txt is in the tar"),
            Ok(Archive::TooLarge(cap)) => panic!("the production cap refused 14 bytes of artifacts at {cap}"),
            Err(e) => panic!("the tar build failed: {e}"),
        }
        let _ = std::fs::remove_dir_all(root);
    }

    /// Reads the body back as a tar and lists its member names.
    async fn tar_names(response: Response) -> Vec<String> {
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024 * 1024).await.unwrap();
        let mut archive = tar::Archive::new(&bytes[..]);
        let mut names = Vec::new();
        for entry in archive.entries().unwrap() {
            names.push(String::from_utf8(entry.unwrap().path_bytes().to_vec()).unwrap());
        }
        names
    }

    /// The container alias resolves `cntr_<id>` and nothing else, in either direction.
    #[tokio::test]
    async fn the_container_alias_needs_the_cntr_wrapper() {
        let (app, root) = colony_with_out("abc").await;
        let response = get_container(&app, "abc", "hello.txt").await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "a bare session id is no container");
        assert_eq!(body(response).await["error"]["code"], "session_not_found");

        for cid in ["cntr_zzz", "cntr_", "container_abc"] {
            let response = get_container(&app, cid, "hello.txt").await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{cid}");
        }

        let response = uhp_container_content(
            State(app.clone()),
            Path(("cntr_abc".into(), "hello.txt".into())),
            HeaderMap::new(),
            Some(scoped_token()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK, "the wrapper resolves to the colony");
        let _ = std::fs::remove_dir_all(root);
    }

    /// A scoped token may read a colony it covers, on both surfaces.
    #[tokio::test]
    async fn a_covering_scoped_token_reads_the_artifacts() {
        let (app, root) = colony_with_out("abc").await;
        let token = scoped_token();
        for uri in ["/api/sessions/abc/files", "/uhp/v1/sessions/abc/files"] {
            let list = artifacts(
                State(app.clone()),
                Path("abc".into()),
                surface(uri.into()),
                HeaderMap::new(),
                Some(token.clone()),
            )
            .await;
            assert_eq!(list.status(), StatusCode::OK, "{uri}");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    /// `UHP-Version` is the router layer's job, so it is tested through the router: every `/uhp`
    /// answer carries it — a success, a refusal, and the container alias — and no `/api` answer
    /// does (§7.1: the header names the `/uhp` surface, not the mothership's own routes).
    #[tokio::test]
    async fn every_uhp_answer_stamps_the_version_and_no_api_answer_does() {
        use tower::ServiceExt as _;

        let (app, root) = colony_with_out("abc").await;
        let router = routes().with_state(app.clone());
        for uri in [
            "/uhp/v1/sessions",
            "/uhp/v1/sessions/abc/files",
            "/uhp/v1/sessions/abc/files/hello.txt/content",
            "/uhp/v1/sessions/abc/files/archive",
            "/uhp/v1/containers/cntr_abc/files/hello.txt/content",
        ] {
            let request = axum::http::Request::builder()
                .uri(uri)
                .body(axum::body::Body::empty())
                .unwrap();
            let response = router.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{uri}");
            assert_eq!(
                response.headers().get(crate::uhp::VERSION_HEADER).unwrap(),
                crate::uhp::VERSION,
                "{uri}"
            );
        }
        // A refusal carries the stamp too.
        let request = axum::http::Request::builder()
            .uri("/uhp/v1/sessions/zzz/files")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response.headers().get(crate::uhp::VERSION_HEADER).unwrap(),
            crate::uhp::VERSION
        );
        let request = axum::http::Request::builder()
            .uri("/api/sessions/abc/files")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers().get(crate::uhp::VERSION_HEADER).is_none(),
            "the /api surface does not advertise the version"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
