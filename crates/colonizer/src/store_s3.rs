//! The remote session store (issue #610): an S3-compatible object store — Cloudflare R2, MinIO,
//! AWS S3, anything that speaks the S3 REST API with Signature Version 4 — behind
//! [`SessionStore`], laid out like the reference [`crate::store::MemoryObjectStore`]:
//! `<prefix>/sessions.json` and `<prefix>/sessions/<id>/<name>`.
//!
//! Two layers:
//!
//! - [`S3Store`] speaks to the bucket directly: whole-object puts for every replace, an append as a
//!   read-modify-write under a conditional put on the object's ETag (the single-writer rule, enforced
//!   rather than assumed), retries with exponential backoff on throttling, server errors and dropped
//!   connections. `colonizer sessions migrate` copies into and out of it.
//! - [`MirroredStore`] is what a mothership runs on: a write-ahead local cache with upload. Every
//!   write lands in the data dir's working copy first — the same bytes and layout as the local store,
//!   which a microVM mounts — and is uploaded shortly after; a periodic sweep also uploads what the
//!   microVM wrote into the working copy itself. Reads are served locally. On a host whose working
//!   copy has no index, startup hydrates it from the bucket, which is what lets a fresh mothership
//!   pick up another host's colonies. The upload state is kept beside the working copy, so a crash
//!   between a local write and its upload is caught up on the next start.
//!
//! The contract and its limits are in docs/session-store.md.

use crate::store::{
    self, FileStat, INDEX, LocalDirStore, SESSIONS, SessionStore, StoreFuture, check_id, check_name, whole_line_tail,
};
use ring::{digest, hmac};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

/// How many times one request is tried before its error is returned.
const ATTEMPTS: u32 = 5;
/// The first retry's wait; each later one doubles it, up to [`BACKOFF_CAP`], plus jitter.
const BACKOFF_BASE: Duration = Duration::from_millis(200);
const BACKOFF_CAP: Duration = Duration::from_secs(5);
/// One request's deadline, connection and body included.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// How many times an append retries a conditional put another writer won.
const APPEND_RACES: u32 = 8;

/// Where a mirrored store waits after a write before uploading, so a burst of appends to one log
/// uploads once. With [`SWEEP_INTERVAL`], the read-lag bound the contract asks a remote backend to
/// state: a write through the store reaches the bucket within this delay while the bucket answers,
/// and a file a microVM wrote into its working copy by itself within the sweep interval.
pub(crate) const UPLOAD_DELAY: Duration = Duration::from_secs(1);
pub(crate) const SWEEP_INTERVAL: Duration = Duration::from_secs(30);
/// The mirrored store's upload state, beside the working copy's index.
const SYNC_STATE: &str = "session-store-sync.json";

/// An S3-compatible bucket, as `session-store.json` records it. Credentials are not here: they come
/// from the secrets mechanism ([`Credentials::resolve`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct S3Config {
    /// The service's base URL: `https://<account>.r2.cloudflarestorage.com`,
    /// `http://127.0.0.1:9000` for MinIO, `https://s3.<region>.amazonaws.com`. Requests are
    /// path-style, `<endpoint>/<bucket>/<key>`, which every one of them serves.
    pub endpoint: String,
    pub bucket: String,
    /// The key prefix the store lives under (no leading or trailing `/`); empty for the bucket root.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prefix: String,
    /// The signing region: `auto` for R2, `us-east-1` for MinIO's default, the bucket's region on AWS.
    #[serde(default = "default_region")]
    pub region: String,
}

fn default_region() -> String {
    "auto".into()
}

impl S3Config {
    /// Parses `s3://<bucket>[/<prefix>]?endpoint=<url>[&region=<region>]`.
    pub(crate) fn parse(spec: &str) -> anyhow::Result<S3Config> {
        let url = reqwest::Url::parse(spec).map_err(|e| anyhow::anyhow!("{spec:?} is not an s3:// URL: {e}"))?;
        anyhow::ensure!(url.scheme() == "s3", "{spec:?} is not an s3:// URL");
        let bucket = url.host_str().unwrap_or_default().to_string();
        anyhow::ensure!(
            !bucket.is_empty(),
            "{spec:?} names no bucket: s3://<bucket>/<prefix>?endpoint=<url>"
        );
        let mut endpoint = None;
        let mut region = None;
        for (key, value) in url.query_pairs() {
            match &*key {
                "endpoint" => endpoint = Some(value.into_owned()),
                "region" => region = Some(value.into_owned()),
                other => anyhow::bail!("{spec:?}: unknown parameter {other:?} (endpoint and region are known)"),
            }
        }
        let region = region.unwrap_or_else(default_region);
        let endpoint = match endpoint {
            Some(endpoint) => endpoint,
            None if region != "auto" => format!("https://s3.{region}.amazonaws.com"),
            None => anyhow::bail!("{spec:?} needs ?endpoint=<url> (or a region, for AWS)"),
        };
        let config = S3Config {
            endpoint: endpoint.trim_end_matches('/').to_string(),
            bucket,
            prefix: url.path().trim_matches('/').to_string(),
            region,
        };
        config.check()?;
        Ok(config)
    }

    /// Refuses an endpoint that is not an http(s) URL, or a prefix with an empty or dot segment.
    pub(crate) fn check(&self) -> anyhow::Result<()> {
        let endpoint = reqwest::Url::parse(&self.endpoint).map_err(|e| anyhow::anyhow!("endpoint {:?}: {e}", self.endpoint))?;
        anyhow::ensure!(
            matches!(endpoint.scheme(), "http" | "https") && endpoint.host_str().is_some(),
            "endpoint {:?} must be an http(s) URL",
            self.endpoint
        );
        anyhow::ensure!(
            !self.bucket.is_empty() && !self.bucket.contains('/'),
            "bucket {:?} is not one name",
            self.bucket
        );
        if !self.prefix.is_empty() {
            anyhow::ensure!(
                self.prefix
                    .split('/')
                    .all(|part| !part.is_empty() && part != "." && part != ".."),
                "prefix {:?} must be plain `/`-separated names",
                self.prefix
            );
        }
        Ok(())
    }

    /// One line naming the store, for messages; never carries a credential.
    pub(crate) fn describe(&self) -> String {
        let prefix = if self.prefix.is_empty() {
            String::new()
        } else {
            format!("/{}", self.prefix)
        };
        format!("s3://{}{prefix} at {}", self.bucket, self.endpoint)
    }
}

/// An access key pair for the bucket.
#[derive(Clone)]
pub(crate) struct Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("access_key_id", &self.access_key_id)
            .finish_non_exhaustive()
    }
}

/// The environment variables that supply the key pair, ahead of the saved secrets.
pub(crate) const ACCESS_KEY_ENV: &str = "COLONIZER_SESSION_STORE_ACCESS_KEY_ID";
pub(crate) const SECRET_KEY_ENV: &str = "COLONIZER_SESSION_STORE_SECRET_ACCESS_KEY";
/// The saved secrets that supply it otherwise, under the config dir (keychain or 0600 file, the
/// same mechanism every provider key uses).
pub(crate) const ACCESS_KEY_FILE: &str = "session-store-access-key-id";
pub(crate) const SECRET_KEY_FILE: &str = "session-store-secret-access-key";

impl Credentials {
    /// The key pair from the environment, else from the saved secrets.
    pub(crate) fn resolve(config_dir: &Path) -> anyhow::Result<Credentials> {
        let read =
            |env: &str, file: &str| crate::util::env_nonempty(env).or_else(|| crate::util::read_secret(&config_dir.join(file)));
        match (read(ACCESS_KEY_ENV, ACCESS_KEY_FILE), read(SECRET_KEY_ENV, SECRET_KEY_FILE)) {
            (Some(access_key_id), Some(secret_access_key)) => Ok(Credentials {
                access_key_id: access_key_id.trim().to_string(),
                secret_access_key: secret_access_key.trim().to_string(),
            }),
            _ => anyhow::bail!(
                "the session store's bucket credentials are not set: set {ACCESS_KEY_ENV} and {SECRET_KEY_ENV}, or save \
                 them as the secrets {ACCESS_KEY_FILE} and {SECRET_KEY_FILE} in the config dir"
            ),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Signature Version 4.
// ---------------------------------------------------------------------------------------------

/// Percent-encodes per SigV4: unreserved characters stay, everything else is `%XX`; `/` stays only
/// when `keep_slash` (an object path, not a query value).
fn uri_encode(value: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            b'/' if keep_slash => out.push('/'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn sha256_hex(bytes: &[u8]) -> String {
    store::hex(digest::digest(&digest::SHA256, bytes).as_ref())
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), data).as_ref().to_vec()
}

/// One request to sign: `path` is the canonical (already encoded) URI, `headers` every header that
/// is sent and signed — `host`, `x-amz-*`, and any `range`/`if-*` — lowercase names.
struct ToSign<'a> {
    method: &'a str,
    path: &'a str,
    query: &'a [(String, String)],
    headers: &'a [(String, String)],
    payload_hash: &'a str,
}

/// The `Authorization` header for `request` (AWS Signature Version 4, service `s3`).
fn authorization(creds: &Credentials, region: &str, amz_date: &str, request: &ToSign<'_>) -> String {
    let mut query: Vec<(String, String)> = request
        .query
        .iter()
        .map(|(k, v)| (uri_encode(k, false), uri_encode(v, false)))
        .collect();
    query.sort();
    let canonical_query = query.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&");
    let mut headers: Vec<(String, String)> = request
        .headers
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    headers.sort();
    let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed_headers = headers.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");
    let canonical = format!(
        "{}\n{}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{}",
        request.method, request.path, request.payload_hash
    );
    let date = &amz_date[..8];
    let scope = format!("{date}/{region}/s3/aws4_request");
    let to_sign = format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}", sha256_hex(canonical.as_bytes()));
    let mut key = hmac_sha256(format!("AWS4{}", creds.secret_access_key).as_bytes(), date.as_bytes());
    for part in [region, "s3", "aws4_request"] {
        key = hmac_sha256(&key, part.as_bytes());
    }
    let signature = store::hex(&hmac_sha256(&key, to_sign.as_bytes()));
    format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        creds.access_key_id
    )
}

// ---------------------------------------------------------------------------------------------
// The direct store.
// ---------------------------------------------------------------------------------------------

/// One answer from the bucket.
struct Answer {
    status: u16,
    etag: Option<String>,
    length: Option<u64>,
    modified: Option<SystemTime>,
    body: Vec<u8>,
}

/// What a conditional put asks of the object it replaces.
enum Condition {
    None,
    /// The object must not exist yet.
    Absent,
    /// The object must still carry this ETag.
    Matches(String),
}

/// The session store in an S3-compatible bucket, spoken to directly. See the module docs.
pub(crate) struct S3Store {
    config: S3Config,
    creds: Credentials,
    http: reqwest::Client,
    /// The first retry's wait ([`BACKOFF_BASE`]; the tests shorten it).
    backoff_base: Duration,
}

impl S3Store {
    pub(crate) fn new(config: S3Config, creds: Credentials) -> anyhow::Result<S3Store> {
        config.check()?;
        let http = reqwest::Client::builder().timeout(REQUEST_TIMEOUT).build()?;
        Ok(S3Store {
            config,
            creds,
            http,
            backoff_base: BACKOFF_BASE,
        })
    }

    /// The same store with another first retry wait, for the tests.
    #[cfg(test)]
    pub(crate) fn with_backoff(mut self, base: Duration) -> S3Store {
        self.backoff_base = base;
        self
    }

    /// An object's key under the prefix.
    fn key(&self, rel: &str) -> String {
        if self.config.prefix.is_empty() {
            rel.to_string()
        } else {
            format!("{}/{rel}", self.config.prefix)
        }
    }

    fn file_key(&self, id: &str, name: &str) -> io::Result<String> {
        check_id(id)?;
        check_name(name)?;
        Ok(self.key(&format!("{SESSIONS}/{id}/{name}")))
    }

    /// Sends one request, retrying throttling (429), server errors (5xx) and transport failures with
    /// exponential backoff and jitter. Any other answer — success or a client error — is returned
    /// for the caller to read.
    async fn send(
        &self,
        method: reqwest::Method,
        key: Option<&str>,
        query: &[(String, String)],
        extra: &[(&str, String)],
        body: &[u8],
    ) -> io::Result<Answer> {
        let mut path = format!("/{}", uri_encode(&self.config.bucket, false));
        if let Some(key) = key {
            path.push('/');
            path.push_str(&uri_encode(key, true));
        }
        let base = reqwest::Url::parse(&self.config.endpoint).map_err(io::Error::other)?;
        let base_path = base.path().trim_end_matches('/');
        let path = format!("{base_path}{path}");
        let host = match base.port() {
            Some(port) => format!("{}:{port}", base.host_str().unwrap_or_default()),
            None => base.host_str().unwrap_or_default().to_string(),
        };
        let mut url = base.clone();
        url.set_path(&path);
        // Encoded exactly as the signature's canonical query encodes it.
        let encoded: Vec<String> = query
            .iter()
            .map(|(k, v)| format!("{}={}", uri_encode(k, false), uri_encode(v, false)))
            .collect();
        url.set_query((!encoded.is_empty()).then(|| encoded.join("&")).as_deref());
        let payload_hash = sha256_hex(body);
        let mut last_error = String::new();
        for attempt in 0..ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(backoff(self.backoff_base, attempt)).await;
            }
            let amz_date = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
            let mut headers: Vec<(String, String)> = vec![
                ("host".into(), host.clone()),
                ("x-amz-content-sha256".into(), payload_hash.clone()),
                ("x-amz-date".into(), amz_date.clone()),
            ];
            headers.extend(extra.iter().map(|(k, v)| (k.to_string(), v.clone())));
            let auth = authorization(
                &self.creds,
                &self.config.region,
                &amz_date,
                &ToSign {
                    method: method.as_str(),
                    path: &path,
                    query,
                    headers: &headers,
                    payload_hash: &payload_hash,
                },
            );
            let mut request = self.http.request(method.clone(), url.clone()).header("authorization", auth);
            for (name, value) in headers.iter().filter(|(name, _)| name != "host") {
                request = request.header(name.as_str(), value.as_str());
            }
            if !body.is_empty() || method == reqwest::Method::PUT {
                request = request.body(body.to_vec());
            }
            match request.send().await {
                Ok(response) => {
                    let status = response.status().as_u16();
                    let header = |name: &str| response.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
                    let etag = header("etag");
                    let length = header("content-length").and_then(|v| v.parse().ok());
                    let modified = header("last-modified")
                        .and_then(|v| chrono::DateTime::parse_from_rfc2822(&v).ok())
                        .map(SystemTime::from);
                    let body = match response.bytes().await {
                        Ok(bytes) => bytes.to_vec(),
                        Err(e) => {
                            last_error = format!("reading the answer: {e}");
                            continue;
                        }
                    };
                    if status == 429 || status >= 500 {
                        last_error = format!("HTTP {status}: {}", excerpt(&body));
                        continue;
                    }
                    return Ok(Answer {
                        status,
                        etag,
                        length,
                        modified,
                        body,
                    });
                }
                Err(e) => last_error = e.to_string(),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "the session store bucket ({}) did not answer {} {path} after {ATTEMPTS} attempts: {last_error}",
                self.config.describe(),
                method
            ),
        ))
    }

    /// The error for an answer the caller did not expect.
    fn refusal(&self, what: &str, answer: &Answer) -> io::Error {
        let kind = match answer.status {
            401 | 403 => io::ErrorKind::PermissionDenied,
            404 => io::ErrorKind::NotFound,
            412 => io::ErrorKind::AlreadyExists,
            _ => io::ErrorKind::Other,
        };
        io::Error::new(
            kind,
            format!(
                "{what} in {}: HTTP {}: {}",
                self.config.describe(),
                answer.status,
                excerpt(&answer.body)
            ),
        )
    }

    async fn get(&self, key: &str) -> io::Result<Option<(Vec<u8>, Option<String>)>> {
        let answer = self.send(reqwest::Method::GET, Some(key), &[], &[], &[]).await?;
        match answer.status {
            200 => Ok(Some((answer.body, answer.etag))),
            404 => Ok(None),
            _ => Err(self.refusal(&format!("reading {key}"), &answer)),
        }
    }

    async fn put(&self, key: &str, bytes: &[u8], condition: Condition) -> io::Result<()> {
        let extra: Vec<(&str, String)> = match condition {
            Condition::None => Vec::new(),
            Condition::Absent => vec![("if-none-match", "*".into())],
            Condition::Matches(etag) => vec![("if-match", etag)],
        };
        let answer = self.send(reqwest::Method::PUT, Some(key), &[], &extra, bytes).await?;
        match answer.status {
            200..=299 => Ok(()),
            _ => Err(self.refusal(&format!("writing {key}"), &answer)),
        }
    }

    async fn delete(&self, key: &str) -> io::Result<()> {
        let answer = self.send(reqwest::Method::DELETE, Some(key), &[], &[], &[]).await?;
        match answer.status {
            200..=299 | 404 => Ok(()),
            _ => Err(self.refusal(&format!("removing {key}"), &answer)),
        }
    }

    /// Every key under `prefix` (ListObjectsV2, every page), or with `delimiter` the common prefixes
    /// one level down instead.
    async fn list(&self, prefix: &str, delimiter: bool) -> io::Result<Vec<String>> {
        let mut found = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut query = vec![
                ("list-type".to_string(), "2".to_string()),
                ("prefix".to_string(), prefix.to_string()),
            ];
            if delimiter {
                query.push(("delimiter".into(), "/".into()));
            }
            if let Some(token) = &token {
                query.push(("continuation-token".into(), token.clone()));
            }
            let answer = self.send(reqwest::Method::GET, None, &query, &[], &[]).await?;
            if answer.status != 200 {
                return Err(self.refusal(&format!("listing {prefix}"), &answer));
            }
            let xml = String::from_utf8_lossy(&answer.body);
            if delimiter {
                for block in xml_blocks(&xml, "CommonPrefixes") {
                    found.extend(xml_blocks(block, "Prefix").into_iter().map(xml_unescape));
                }
            } else {
                for block in xml_blocks(&xml, "Contents") {
                    found.extend(xml_blocks(block, "Key").into_iter().map(xml_unescape));
                }
            }
            let truncated = xml_blocks(&xml, "IsTruncated").first().is_some_and(|v| v.trim() == "true");
            token = xml_blocks(&xml, "NextContinuationToken").first().map(|t| xml_unescape(t));
            if !truncated || token.is_none() {
                return Ok(found);
            }
        }
    }
}

/// The retry wait before `attempt` (1-based): doubling from [`BACKOFF_BASE`], capped, plus up to a
/// quarter of it again as jitter so retries from many tasks do not land together.
fn backoff(base: Duration, attempt: u32) -> Duration {
    let wait = base.saturating_mul(1 << attempt.min(16)).min(BACKOFF_CAP);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    wait + wait / 4 * (nanos % 1000) / 1000
}

/// The first bytes of an error body, for a message.
fn excerpt(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    crate::util::truncate(text.trim(), 300)
}

/// Every `<tag>…</tag>` body in `xml`, outermost first; enough for ListObjectsV2's flat answer.
fn xml_blocks<'a>(xml: &'a str, tag: &str) -> Vec<&'a str> {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start + open.len()..];
        let Some(end) = after.find(&close) else { break };
        out.push(&after[..end]);
        rest = &after[end + close.len()..];
    }
    out
}

fn xml_unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

impl SessionStore for S3Store {
    fn read_index(&self) -> StoreFuture<'_, Option<Vec<u8>>> {
        Box::pin(async move { Ok(self.get(&self.key(INDEX)).await?.map(|(bytes, _)| bytes)) })
    }

    fn write_index<'a>(&'a self, bytes: &'a [u8]) -> StoreFuture<'a, ()> {
        Box::pin(async move { self.put(&self.key(INDEX), bytes, Condition::None).await })
    }

    fn list_sessions(&self) -> StoreFuture<'_, Vec<String>> {
        Box::pin(async move {
            let root = self.key(&format!("{SESSIONS}/"));
            let mut ids: Vec<String> = self
                .list(&root, true)
                .await?
                .iter()
                .filter_map(|p| p.strip_prefix(&root)?.strip_suffix('/').map(str::to_string))
                .filter(|id| check_id(id).is_ok())
                .collect();
            ids.sort();
            ids.dedup();
            Ok(ids)
        })
    }

    fn read_file<'a>(&'a self, id: &'a str, name: &'a str) -> StoreFuture<'a, Option<Vec<u8>>> {
        Box::pin(async move { Ok(self.get(&self.file_key(id, name)?).await?.map(|(bytes, _)| bytes)) })
    }

    fn write_file<'a>(&'a self, id: &'a str, name: &'a str, bytes: &'a [u8]) -> StoreFuture<'a, ()> {
        Box::pin(async move { self.put(&self.file_key(id, name)?, bytes, Condition::None).await })
    }

    fn append<'a>(&'a self, id: &'a str, name: &'a str, line: &'a [u8]) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let key = self.file_key(id, name)?;
            // #761: every backend stores an appended line redacted (`store::ledger_line`).
            let line = crate::store::ledger_line(line);
            std::str::from_utf8(&line)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, format!("an appended line must be UTF-8: {key}")))?;
            // Read-modify-write under a conditional put: the put lands only if the object is still
            // the one read, so a second writer cannot silently drop this line or have it drop theirs.
            for _ in 0..APPEND_RACES {
                let (mut bytes, condition) = match self.get(&key).await? {
                    Some((bytes, Some(etag))) => (bytes, Condition::Matches(etag)),
                    Some((bytes, None)) => (bytes, Condition::None),
                    None => (Vec::new(), Condition::Absent),
                };
                bytes.extend_from_slice(&line);
                bytes.push(b'\n');
                match self.put(&key, &bytes, condition).await {
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                    other => return other,
                }
            }
            Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{key} kept changing under this append: another writer is appending to colony {id}"),
            ))
        })
    }

    fn list_files<'a>(&'a self, id: &'a str) -> StoreFuture<'a, Vec<String>> {
        Box::pin(async move {
            check_id(id)?;
            let root = self.key(&format!("{SESSIONS}/{id}/"));
            let mut files: Vec<String> = self
                .list(&root, false)
                .await?
                .iter()
                .filter_map(|k| k.strip_prefix(&root).map(str::to_string))
                .filter(|name| check_name(name).is_ok())
                .collect();
            files.sort();
            Ok(files)
        })
    }

    fn remove_session<'a>(&'a self, id: &'a str) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            for name in self.list_files(id).await? {
                self.delete(&self.file_key(id, &name)?).await?;
            }
            Ok(())
        })
    }

    fn quarantine_index(&self) -> StoreFuture<'_, Option<String>> {
        Box::pin(async move {
            let Some((bytes, _)) = self.get(&self.key(INDEX)).await? else {
                return Ok(None);
            };
            // Copy, then delete: there is no move on an object store, and a failure between the two
            // leaves both, never neither.
            let name = format!("{INDEX}.corrupt-{}", store::quarantine_stamp());
            self.put(&self.key(&name), &bytes, Condition::None).await?;
            self.delete(&self.key(INDEX)).await?;
            Ok(Some(name))
        })
    }

    fn read_tail<'a>(&'a self, id: &'a str, name: &'a str, max: u64) -> StoreFuture<'a, Option<Vec<u8>>> {
        Box::pin(async move {
            let key = self.file_key(id, name)?;
            // One byte past the budget, so the tail can tell a cut on a line start from a mid-line one.
            let range = format!("bytes=-{}", max.saturating_add(1));
            let answer = self
                .send(reqwest::Method::GET, Some(&key), &[], &[("range", range)], &[])
                .await?;
            match answer.status {
                200 | 206 => Ok(Some(whole_line_tail(&answer.body, max))),
                // An empty object has no byte range to serve.
                416 => Ok(Some(Vec::new())),
                404 => Ok(None),
                _ => Err(self.refusal(&format!("reading the end of {key}"), &answer)),
            }
        })
    }

    fn stat<'a>(&'a self, id: &'a str, name: &'a str) -> StoreFuture<'a, Option<FileStat>> {
        Box::pin(async move {
            let key = self.file_key(id, name)?;
            let answer = self.send(reqwest::Method::HEAD, Some(&key), &[], &[], &[]).await?;
            match answer.status {
                200 => Ok(Some(FileStat {
                    len: answer.length.unwrap_or_default(),
                    modified: answer.modified,
                })),
                404 => Ok(None),
                _ => Err(self.refusal(&format!("looking up {key}"), &answer)),
            }
        })
    }

    fn remove_file<'a>(&'a self, id: &'a str, name: &'a str) -> StoreFuture<'a, ()> {
        Box::pin(async move { self.delete(&self.file_key(id, name)?).await })
    }
}

// ---------------------------------------------------------------------------------------------
// The mirrored store: a write-ahead local cache with upload.
// ---------------------------------------------------------------------------------------------

/// One object the mirror owes the bucket.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Pending {
    /// The index: uploaded as it is locally, or quarantined in the bucket too when the local one was.
    Index,
    /// One session file.
    File { id: String, name: String },
    /// A whole session, removed.
    Session { id: String },
}

/// What the bucket was last given for one local file: its length and modified time when uploaded,
/// so the sweep can tell what changed since.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Mark {
    len: u64,
    modified_ns: u128,
}

/// The upload state, persisted beside the working copy: what is owed and what was last uploaded.
#[derive(Default, Serialize, Deserialize)]
struct SyncState {
    pending: BTreeSet<Pending>,
    uploaded: BTreeMap<String, Mark>,
    /// The last upload failure, for the operator; cleared by the next success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_error: Option<String>,
}

/// The store a mothership runs on when `session-store.json` names a bucket. See the module docs.
pub(crate) struct MirroredStore {
    local: LocalDirStore,
    root: PathBuf,
    remote: Arc<dyn SessionStore>,
    state: Mutex<SyncState>,
    wake: tokio::sync::Notify,
    /// Serializes uploads, so the sweep and a flush never race on one object.
    uploading: tokio::sync::Mutex<()>,
}

/// The key a local file is marked under in the upload state.
fn mark_key(id: &str, name: &str) -> String {
    format!("{SESSIONS}/{id}/{name}")
}

fn modified_ns(stat: &FileStat) -> u128 {
    stat.modified
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos())
}

impl MirroredStore {
    /// Opens the mirror over the working copy at `root`, with `remote` as the store of record:
    /// hydrates an empty working copy from the bucket, reloads the upload state, and sweeps for
    /// anything written and not yet uploaded (a crash between the two). Nothing uploads until
    /// [`MirroredStore::flush`] or the task [`MirroredStore::spawn_uploader`] starts.
    pub(crate) async fn open(root: PathBuf, remote: Arc<dyn SessionStore>) -> io::Result<Arc<MirroredStore>> {
        let local = LocalDirStore::new(root.clone());
        let state: SyncState = match std::fs::read(root.join(SYNC_STATE)) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => SyncState::default(),
        };
        let mirror = Arc::new(MirroredStore {
            local,
            root,
            remote,
            state: Mutex::new(state),
            wake: tokio::sync::Notify::new(),
            uploading: tokio::sync::Mutex::new(()),
        });
        if mirror.local.read_index().await?.is_none() && mirror.remote.read_index().await?.is_some() {
            mirror.hydrate().await?;
        }
        mirror.sweep().await?;
        Ok(mirror)
    }

    /// Copies the bucket into an empty working copy — a fresh host taking over — and marks every
    /// file as already uploaded. The index lands last, as in a migration.
    async fn hydrate(&self) -> io::Result<()> {
        for id in self.remote.list_sessions().await? {
            for name in self.remote.list_files(&id).await? {
                let Some(bytes) = self.remote.read_file(&id, &name).await? else {
                    continue;
                };
                if crate::archive::is_credential_file(&name) || name == "issue.json" {
                    self.local.write_private(&id, &name, &bytes).await?;
                } else {
                    self.local.write_file(&id, &name, &bytes).await?;
                }
                self.note_uploaded(&id, &name).await?;
            }
        }
        if let Some(index) = self.remote.read_index().await? {
            self.local.write_index(&index).await?;
        }
        self.save_state()
    }

    /// Records a local file's current length and time as what the bucket holds.
    async fn note_uploaded(&self, id: &str, name: &str) -> io::Result<()> {
        if let Some(stat) = self.local.stat(id, name).await? {
            let mark = Mark {
                len: stat.len,
                modified_ns: modified_ns(&stat),
            };
            self.lock().uploaded.insert(mark_key(id, name), mark);
        }
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SyncState> {
        self.state.lock().expect("poisoned")
    }

    fn save_state(&self) -> io::Result<()> {
        let bytes = serde_json::to_vec(&*self.lock()).map_err(io::Error::other)?;
        let path = self.root.join(SYNC_STATE);
        let tmp = self.root.join(format!("{SYNC_STATE}.tmp"));
        std::fs::create_dir_all(&self.root)?;
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(tmp, path)
    }

    /// Owes the bucket `pending` and wakes the uploader. The state is saved before this returns, so
    /// a crash before the upload still owes it on the next start.
    fn owe(&self, pending: Pending) -> io::Result<()> {
        // Saved only when the object was not owed already: a burst of appends to one log costs one
        // state write, not one per line.
        let fresh = self.lock().pending.insert(pending);
        if fresh {
            self.save_state()?;
        }
        self.wake.notify_one();
        Ok(())
    }

    /// Owes the bucket every local file whose length or time differs from its last upload — what
    /// a microVM wrote into the working copy itself, or a write a crash kept from uploading — and
    /// the index when there is one. A file never uploaded whose bucket copy already has the same
    /// length (a migration just copied it) is marked rather than sent again.
    pub(crate) async fn sweep(&self) -> io::Result<()> {
        for id in self.local.list_sessions().await? {
            for name in self.local.list_files(&id).await? {
                let Some(stat) = self.local.stat(&id, &name).await? else {
                    continue;
                };
                let mark = Mark {
                    len: stat.len,
                    modified_ns: modified_ns(&stat),
                };
                let known = self.lock().uploaded.get(&mark_key(&id, &name)).copied();
                match known {
                    Some(known) if known == mark => {}
                    None if self.remote.stat(&id, &name).await?.is_some_and(|r| r.len == stat.len) => {
                        self.lock().uploaded.insert(mark_key(&id, &name), mark);
                    }
                    _ => {
                        self.lock().pending.insert(Pending::File { id: id.clone(), name });
                    }
                }
            }
        }
        if let Some(index) = self.local.read_index().await?
            && self.remote.read_index().await?.as_deref() != Some(index.as_slice())
        {
            self.lock().pending.insert(Pending::Index);
        }
        self.save_state()
    }

    /// Uploads everything owed, once. What fails stays owed, with the error kept for the operator
    /// and returned; the next call (or the uploader's next round) tries it again.
    pub(crate) async fn flush(&self) -> io::Result<()> {
        let _serial = self.uploading.lock().await;
        let owed: Vec<Pending> = self.lock().pending.iter().cloned().collect();
        let mut failure = None;
        for item in owed {
            match self.upload(&item).await {
                Ok(()) => {
                    self.lock().pending.remove(&item);
                }
                Err(e) => {
                    eprintln!("session store: could not upload {item:?}: {e}");
                    failure = Some(e);
                }
            }
        }
        self.lock().last_error = failure.as_ref().map(|e| e.to_string());
        self.save_state()?;
        match failure {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Sends one owed object: the local bytes as they are now (so a burst of appends uploads once),
    /// or a removal when the local file is gone.
    async fn upload(&self, item: &Pending) -> io::Result<()> {
        match item {
            Pending::Session { id } => self.remote.remove_session(id).await,
            Pending::Index => match self.local.read_index().await? {
                Some(bytes) => self.remote.write_index(&bytes).await,
                // The local index was quarantined: the bucket's goes aside the same way, keeping its
                // bytes, and the local aside stays where the operator will look for it.
                None => self.remote.quarantine_index().await.map(|_| ()),
            },
            Pending::File { id, name } => {
                let stat = self.local.stat(id, name).await?;
                match self.local.read_file(id, name).await? {
                    Some(bytes) => {
                        self.remote.write_file(id, name, &bytes).await?;
                        if let Some(stat) = stat {
                            let mark = Mark {
                                len: stat.len,
                                modified_ns: modified_ns(&stat),
                            };
                            self.lock().uploaded.insert(mark_key(id, name), mark);
                        }
                        Ok(())
                    }
                    None => {
                        self.lock().uploaded.remove(&mark_key(id, name));
                        self.remote.remove_file(id, name).await
                    }
                }
            }
        }
    }

    /// The uploader: after each write, waits [`UPLOAD_DELAY`] and uploads what is owed; every
    /// [`SWEEP_INTERVAL`] it also sweeps the working copy. A failed round backs off and retries.
    pub(crate) fn spawn_uploader(self: &Arc<Self>) {
        let mirror = Arc::clone(self);
        tokio::spawn(async move {
            let mut failures: u32 = 0;
            let mut last_sweep = tokio::time::Instant::now();
            loop {
                let wait = if failures > 0 {
                    backoff(BACKOFF_BASE, failures).max(UPLOAD_DELAY)
                } else {
                    SWEEP_INTERVAL
                };
                let _ = tokio::time::timeout(wait, mirror.wake.notified()).await;
                tokio::time::sleep(UPLOAD_DELAY).await;
                if last_sweep.elapsed() >= SWEEP_INTERVAL {
                    last_sweep = tokio::time::Instant::now();
                    if let Err(e) = mirror.sweep().await {
                        eprintln!("session store: the sweep failed: {e}");
                    }
                }
                failures = match mirror.flush().await {
                    Ok(()) => 0,
                    Err(_) => (failures + 1).min(8),
                };
            }
        });
    }
}

impl SessionStore for MirroredStore {
    fn read_index(&self) -> StoreFuture<'_, Option<Vec<u8>>> {
        self.local.read_index()
    }

    fn write_index<'a>(&'a self, bytes: &'a [u8]) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            self.local.write_index(bytes).await?;
            self.owe(Pending::Index)
        })
    }

    fn list_sessions(&self) -> StoreFuture<'_, Vec<String>> {
        self.local.list_sessions()
    }

    fn read_file<'a>(&'a self, id: &'a str, name: &'a str) -> StoreFuture<'a, Option<Vec<u8>>> {
        self.local.read_file(id, name)
    }

    fn write_file<'a>(&'a self, id: &'a str, name: &'a str, bytes: &'a [u8]) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            self.local.write_file(id, name, bytes).await?;
            self.owe(Pending::File {
                id: id.into(),
                name: name.into(),
            })
        })
    }

    fn append<'a>(&'a self, id: &'a str, name: &'a str, line: &'a [u8]) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            self.local.append(id, name, line).await?;
            self.owe(Pending::File {
                id: id.into(),
                name: name.into(),
            })
        })
    }

    fn list_files<'a>(&'a self, id: &'a str) -> StoreFuture<'a, Vec<String>> {
        self.local.list_files(id)
    }

    fn remove_session<'a>(&'a self, id: &'a str) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            self.local.remove_session(id).await?;
            {
                let mut state = self.lock();
                state
                    .pending
                    .retain(|p| !matches!(p, Pending::File { id: owner, .. } if owner == id));
                let prefix = format!("{SESSIONS}/{id}/");
                state.uploaded.retain(|key, _| !key.starts_with(&prefix));
            }
            self.owe(Pending::Session { id: id.into() })
        })
    }

    fn quarantine_index(&self) -> StoreFuture<'_, Option<String>> {
        Box::pin(async move {
            let aside = self.local.quarantine_index().await?;
            if aside.is_some() {
                self.owe(Pending::Index)?;
            }
            Ok(aside)
        })
    }

    fn read_tail<'a>(&'a self, id: &'a str, name: &'a str, max: u64) -> StoreFuture<'a, Option<Vec<u8>>> {
        self.local.read_tail(id, name, max)
    }

    fn stat<'a>(&'a self, id: &'a str, name: &'a str) -> StoreFuture<'a, Option<FileStat>> {
        self.local.stat(id, name)
    }

    fn write_private<'a>(&'a self, id: &'a str, name: &'a str, bytes: &'a [u8]) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            self.local.write_private(id, name, bytes).await?;
            self.owe(Pending::File {
                id: id.into(),
                name: name.into(),
            })
        })
    }

    fn remove_file<'a>(&'a self, id: &'a str, name: &'a str) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            self.local.remove_file(id, name).await?;
            self.owe(Pending::File {
                id: id.into(),
                name: name.into(),
            })
        })
    }

    fn rename_file<'a>(&'a self, id: &'a str, from: &'a str, to: &'a str) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            self.local.rename_file(id, from, to).await?;
            self.owe(Pending::File {
                id: id.into(),
                name: from.into(),
            })?;
            self.owe(Pending::File {
                id: id.into(),
                name: to.into(),
            })
        })
    }
}

#[cfg(test)]
mod tests;
