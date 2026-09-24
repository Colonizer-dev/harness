//! `GET /api/img?u=<url>`: GitHub avatars through the mothership, kept on disk for a week, so org
//! and contributor faces load from the cockpit's own origin and survive a GitHub outage.
//!
//! Not an open proxy: only `https://avatars.githubusercontent.com/...` and
//! `https://github.com/<login>.png` are fetched, a redirect is followed only to another allowed
//! URL (at most three), and the answer must be a raster image (`image/png`, `jpeg`, `gif`, `webp`,
//! `avif` — never SVG, which can carry script) of at most 5 MB. The cached copy is served with its
//! ETag; past a week it is revalidated conditionally, and when GitHub cannot be reached the old
//! copy is served rather than a broken image.

use crate::{
    Shared,
    cache_store::{self, DiskCache},
};
use anyhow::{Context, Result, bail};
use axum::{
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// The largest image fetched.
pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;
/// How long a cached image is served without asking GitHub again.
pub const IMAGE_FRESH: Duration = Duration::from_secs(7 * 24 * 3600);
const MAX_REDIRECTS: usize = 3;
const ALLOWED_TYPES: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp", "image/avif"];

/// Whether `url` is one the proxy may fetch: an https GitHub avatar, no credentials, no port.
pub fn allowed(url: &Url) -> bool {
    if url.scheme() != "https" || url.port().is_some() || !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    match url.host_str() {
        Some("avatars.githubusercontent.com") => true,
        Some("github.com") => {
            let path = url.path();
            path.strip_prefix('/')
                .and_then(|p| p.strip_suffix(".png"))
                .is_some_and(|login| {
                    !login.is_empty() && login.len() <= 39 && login.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                })
        }
        _ => false,
    }
}

/// A raster image type; parameters (`; charset=...`) are ignored.
pub fn acceptable_type(content_type: &str) -> bool {
    let base = content_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    ALLOWED_TYPES.contains(&base.as_str())
}

#[derive(Deserialize)]
pub struct ImgQuery {
    u: String,
}

/// What is kept for one image: the header line of its cache file (the bytes follow it).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ImgMeta {
    key: String,
    fetched_at: u64,
    content_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    etag: Option<String>,
}

fn encode(meta: &ImgMeta, bytes: &[u8]) -> Result<Vec<u8>> {
    let mut out = serde_json::to_vec(meta)?;
    out.push(b'\n');
    out.extend_from_slice(bytes);
    Ok(out)
}

fn decode(key: &str, file: &[u8]) -> Option<(ImgMeta, Vec<u8>)> {
    let split = file.iter().position(|b| *b == b'\n')?;
    let meta: ImgMeta = serde_json::from_slice(&file[..split]).ok()?;
    (meta.key == key && acceptable_type(&meta.content_type)).then(|| (meta, file[split + 1..].to_vec()))
}

fn load(cache: &DiskCache, key: &str) -> Option<(ImgMeta, Vec<u8>)> {
    let file = cache.load_bytes(key)?;
    let decoded = decode(key, &file);
    if decoded.is_none() {
        cache.remove(key);
    }
    decoded
}

/// What one fetch came to.
#[derive(Debug)]
pub enum Fetched {
    NotModified,
    Image {
        content_type: String,
        etag: Option<String>,
        bytes: Vec<u8>,
    },
}

/// A client that follows redirects only to URLs `allow` accepts.
pub fn client(allow: fn(&Url) -> bool) -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .user_agent(concat!("colonizer/", env!("CARGO_PKG_VERSION"), " (+https://colonizer.dev)"))
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() > MAX_REDIRECTS {
                attempt.error("too many redirects")
            } else if allow(attempt.url()) {
                attempt.follow()
            } else {
                attempt.error("redirect to a host the image proxy does not fetch")
            }
        }))
        .build()?)
}

/// Fetches one image, conditionally when `etag` is known, enforcing the type and size limits.
pub async fn fetch(client: &reqwest::Client, url: &Url, etag: Option<&str>) -> Result<Fetched> {
    let mut req = client.get(url.clone()).header(header::ACCEPT, "image/*");
    if let Some(etag) = etag {
        req = req.header(header::IF_NONE_MATCH, etag);
    }
    let mut res = req.send().await.context("could not reach the image host")?;
    if res.status() == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(Fetched::NotModified);
    }
    if !res.status().is_success() {
        bail!("the image host answered {}", res.status());
    }
    let content_type = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    if !acceptable_type(&content_type) {
        bail!("not an image: {content_type:?}");
    }
    if res.content_length().is_some_and(|n| n as usize > MAX_IMAGE_BYTES) {
        bail!("image larger than {MAX_IMAGE_BYTES} bytes");
    }
    let etag = res
        .headers()
        .get(header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let mut bytes = Vec::new();
    while let Some(chunk) = res.chunk().await? {
        if bytes.len() + chunk.len() > MAX_IMAGE_BYTES {
            bail!("image larger than {MAX_IMAGE_BYTES} bytes");
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(Fetched::Image {
        content_type,
        etag,
        bytes,
    })
}

fn respond(meta: &ImgMeta, bytes: Vec<u8>, request: &HeaderMap) -> Response {
    // The browser's own validator is the cache file's identity: the upstream ETag when there is
    // one, else when it was fetched.
    let tag = format!(
        "\"{}\"",
        &cache_store::key_hash(&format!("{}{}", meta.etag.as_deref().unwrap_or_default(), meta.fetched_at))[..16]
    );
    let not_modified = request
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == tag));
    let mut response = if not_modified {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        bytes.into_response()
    };
    let headers = response.headers_mut();
    if !not_modified && let Ok(ct) = HeaderValue::from_str(&meta.content_type) {
        headers.insert(header::CONTENT_TYPE, ct);
    }
    if let Ok(tag) = HeaderValue::from_str(&tag) {
        headers.insert(header::ETAG, tag);
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=604800"));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; sandbox"),
    );
    response
}

/// GET /api/img?u=<url>
pub async fn image(State(app): State<Shared>, Query(q): Query<ImgQuery>, headers: HeaderMap) -> Response {
    let Some(url) = Url::parse(&q.u).ok().filter(allowed) else {
        return (StatusCode::BAD_REQUEST, "only GitHub avatars are proxied").into_response();
    };
    let key = url.to_string();
    let cached = load(&app.img_cache, &key);
    let age = |m: &ImgMeta| Duration::from_millis(cache_store::now_ms().saturating_sub(m.fetched_at));
    if let Some((meta, bytes)) = cached.as_ref().filter(|(m, _)| age(m) < IMAGE_FRESH) {
        return respond(meta, bytes.clone(), &headers);
    }
    let fetched = match client(allowed) {
        Ok(c) => fetch(&c, &url, cached.as_ref().and_then(|(m, _)| m.etag.as_deref())).await,
        Err(e) => Err(e),
    };
    let (meta, bytes) = match (fetched, cached) {
        (Ok(Fetched::NotModified), Some((mut meta, bytes))) => {
            meta.fetched_at = cache_store::now_ms();
            (meta, bytes)
        }
        (
            Ok(Fetched::Image {
                content_type,
                etag,
                bytes,
            }),
            _,
        ) => (
            ImgMeta {
                key: key.clone(),
                fetched_at: cache_store::now_ms(),
                content_type,
                etag,
            },
            bytes,
        ),
        // GitHub unreachable or odd: the old face beats a broken image.
        (_, Some((meta, bytes))) => return respond(&meta, bytes, &headers),
        (Ok(Fetched::NotModified), None) => {
            return (StatusCode::BAD_GATEWAY, "the image host answered 304 to a plain request").into_response();
        }
        (Err(e), None) => return (StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    };
    match encode(&meta, &bytes) {
        Ok(file) => {
            if let Err(e) = app.img_cache.store_bytes(&key, &file) {
                eprintln!("img: could not keep {key}: {e:#}");
            }
        }
        Err(e) => eprintln!("img: {e:#}"),
    }
    respond(&meta, bytes, &headers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, routing::get};

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn only_github_avatars_are_allowed() {
        assert!(allowed(&url("https://avatars.githubusercontent.com/u/583231?v=4&s=64")));
        assert!(allowed(&url("https://github.com/octocat.png")));
        assert!(allowed(&url("https://github.com/octo-cat.png?size=80")));
        for bad in [
            "http://avatars.githubusercontent.com/u/1",
            "https://avatars.githubusercontent.com:8443/u/1",
            "https://user:pw@avatars.githubusercontent.com/u/1",
            "https://evil.example/u/1",
            "https://avatars.githubusercontent.com.evil.example/u/1",
            "https://github.com/octocat",
            "https://github.com/octocat/repo.png",
            "https://github.com/login/oauth.png",
            "https://github.com/.png",
            "https://raw.githubusercontent.com/a/b/main/x.png",
            "file:///etc/passwd",
        ] {
            assert!(!allowed(&url(bad)), "{bad} must not be proxied");
        }
    }

    #[test]
    fn only_raster_images_are_accepted() {
        assert!(acceptable_type("image/png"));
        assert!(acceptable_type("image/jpeg; charset=binary"));
        assert!(acceptable_type("IMAGE/WEBP"));
        assert!(!acceptable_type("image/svg+xml"));
        assert!(!acceptable_type("text/html"));
        assert!(!acceptable_type(""));
    }

    #[test]
    fn a_cache_file_round_trips_and_a_foreign_or_bad_one_is_refused() {
        let meta = ImgMeta {
            key: "https://github.com/a.png".into(),
            fetched_at: 1,
            content_type: "image/png".into(),
            etag: Some("\"e\"".into()),
        };
        let file = encode(&meta, b"\x89PNG\nbytes").unwrap();
        assert_eq!(decode(&meta.key, &file), Some((meta.clone(), b"\x89PNG\nbytes".to_vec())));
        assert_eq!(decode("https://github.com/b.png", &file), None);
        assert_eq!(decode(&meta.key, b"garbage"), None);
        let svg = ImgMeta {
            content_type: "image/svg+xml".into(),
            ..meta.clone()
        };
        assert_eq!(decode(&meta.key, &encode(&svg, b"<svg/>").unwrap()), None);
    }

    /// A local image host: the allowlist in these tests is "127.0.0.1 only", standing in for the
    /// GitHub hosts, so redirects to anywhere else are refused the same way.
    async fn host() -> String {
        let big = vec![0u8; MAX_IMAGE_BYTES + 1];
        let app = Router::new()
            .route(
                "/ok.png",
                get(|| async {
                    (
                        [(header::CONTENT_TYPE, "image/png"), (header::ETAG, "\"v1\"")],
                        b"PNG".to_vec(),
                    )
                }),
            )
            .route("/page", get(|| async { ([(header::CONTENT_TYPE, "text/html")], "<script>") }))
            .route(
                "/big.png",
                get(move || async move { ([(header::CONTENT_TYPE, "image/png")], big.clone()) }),
            )
            .route(
                "/to-ok",
                get(|| async { (StatusCode::FOUND, [(header::LOCATION, "/ok.png")]) }),
            )
            .route(
                "/to-elsewhere",
                get(|| async { (StatusCode::FOUND, [(header::LOCATION, "http://localhost.invalid/x.png")]) }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    fn local_only(u: &Url) -> bool {
        u.host_str() == Some("127.0.0.1")
    }

    #[tokio::test]
    async fn fetch_enforces_redirects_types_and_size() {
        let base = host().await;
        let c = client(local_only).unwrap();
        match fetch(&c, &url(&format!("{base}/ok.png")), None).await.unwrap() {
            Fetched::Image {
                content_type,
                etag,
                bytes,
            } => {
                assert_eq!(content_type, "image/png");
                assert_eq!(etag.as_deref(), Some("\"v1\""));
                assert_eq!(bytes, b"PNG");
            }
            other => panic!("expected an image, got {other:?}"),
        }
        assert!(matches!(
            fetch(&c, &url(&format!("{base}/to-ok")), None).await.unwrap(),
            Fetched::Image { .. }
        ));
        let off = fetch(&c, &url(&format!("{base}/to-elsewhere")), None).await.unwrap_err();
        assert!(format!("{off:#}").contains("does not fetch"), "{off:#}");
        let html = fetch(&c, &url(&format!("{base}/page")), None).await.unwrap_err();
        assert!(format!("{html:#}").contains("not an image"), "{html:#}");
        let big = fetch(&c, &url(&format!("{base}/big.png")), None).await.unwrap_err();
        assert!(format!("{big:#}").contains("larger than"), "{big:#}");
    }
}
