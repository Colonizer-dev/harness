//! The remote session store against an in-process S3-compatible fake that checks every request's
//! signature, and — when `COLONIZER_TEST_S3` names one — against a real bucket.

use super::*;
use crate::config::Settings;
use crate::store::tests::{cleanup, contract, derived, event, lines, put, refuses_host_paths, seed, temp_root};
use crate::store::{MemoryObjectStore, migrate};
use axum::{
    Router,
    body::{Body, Bytes},
    extract::{Path as AxumPath, RawQuery, State},
    http::{HeaderMap, Method, StatusCode, Uri},
    response::Response,
    routing::any,
};
use std::sync::atomic::{AtomicUsize, Ordering};

const BUCKET: &str = "colonies";
const KEY_ID: &str = "TESTKEY";
const SECRET: &str = "test/secret+key";

fn creds() -> Credentials {
    Credentials {
        access_key_id: KEY_ID.into(),
        secret_access_key: SECRET.into(),
    }
}

/// One stored object: its bytes, an ETag that changes on every put, and when it was put.
struct Object {
    bytes: Vec<u8>,
    etag: String,
    modified: chrono::DateTime<chrono::Utc>,
}

/// The fake bucket's state, and the knobs the tests turn.
#[derive(Default)]
struct Fake {
    objects: Mutex<BTreeMap<String, Object>>,
    puts: AtomicUsize,
    versions: AtomicUsize,
    /// Answer this many requests with 503 before serving again.
    fail_next: AtomicUsize,
    /// Refuse (403) a put whose key contains this, once.
    deny_once: Mutex<Option<String>>,
}

/// An S3-compatible server on a loopback port: path-style objects, ListObjectsV2 (two keys a page,
/// so paging is exercised), suffix ranges, conditional puts, and SigV4 checked against the request
/// as it arrived on the wire.
async fn serve() -> (String, Arc<Fake>) {
    let fake = Arc::new(Fake::default());
    let app = Router::new()
        .route("/{bucket}", any(bucket_root))
        .route("/{bucket}/{*key}", any(object))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), fake)
}

fn answer(status: StatusCode, body: impl Into<Body>) -> Response {
    Response::builder().status(status).body(body.into()).unwrap()
}

/// Recomputes the request's signature from what arrived and compares it with the one sent.
fn check_signature(method: &Method, uri: &Uri, headers: &HeaderMap, body: &[u8]) -> Result<(), Box<Response>> {
    let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or_default();
    let prefix = format!("AWS4-HMAC-SHA256 Credential={KEY_ID}/");
    if !auth.starts_with(&prefix) {
        return Err(Box::new(answer(StatusCode::FORBIDDEN, "InvalidAccessKeyId")));
    }
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string()
    };
    if header("x-amz-content-sha256") != sha256_hex(body) {
        return Err(Box::new(answer(StatusCode::BAD_REQUEST, "XAmzContentSHA256Mismatch")));
    }
    let signed: Vec<String> = auth
        .split("SignedHeaders=")
        .nth(1)
        .and_then(|rest| rest.split(',').next())
        .unwrap_or_default()
        .split(';')
        .map(str::to_string)
        .collect();
    let signed_headers: Vec<(String, String)> = signed.iter().map(|name| (name.clone(), header(name))).collect();
    let query: Vec<(String, String)> = uri
        .query()
        .map(|q| {
            reqwest::Url::parse(&format!("http://x/?{q}"))
                .unwrap()
                .query_pairs()
                .into_owned()
                .collect()
        })
        .unwrap_or_default();
    let region = auth.split('/').nth(2).unwrap_or_default().to_string();
    let expected = authorization(
        &creds(),
        &region,
        &header("x-amz-date"),
        &ToSign {
            method: method.as_str(),
            path: uri.path(),
            query: &query,
            headers: &signed_headers,
            payload_hash: &header("x-amz-content-sha256"),
        },
    );
    if expected != auth {
        return Err(Box::new(answer(StatusCode::FORBIDDEN, "SignatureDoesNotMatch")));
    }
    Ok(())
}

/// The failure knobs, then the signature: what every request goes through first.
fn admit(fake: &Fake, method: &Method, uri: &Uri, headers: &HeaderMap, body: &[u8]) -> Result<(), Box<Response>> {
    if fake.fail_next.load(Ordering::SeqCst) > 0 {
        fake.fail_next.fetch_sub(1, Ordering::SeqCst);
        return Err(Box::new(answer(StatusCode::SERVICE_UNAVAILABLE, "SlowDown")));
    }
    check_signature(method, uri, headers, body)
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

async fn bucket_root(
    State(fake): State<Arc<Fake>>,
    AxumPath(bucket): AxumPath<String>,
    method: Method,
    uri: Uri,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(refused) = admit(&fake, &method, &uri, &headers, &body) {
        return *refused;
    }
    if bucket != BUCKET {
        return answer(StatusCode::NOT_FOUND, "NoSuchBucket");
    }
    let params: BTreeMap<String, String> = raw
        .map(|q| {
            reqwest::Url::parse(&format!("http://x/?{q}"))
                .unwrap()
                .query_pairs()
                .into_owned()
                .collect()
        })
        .unwrap_or_default();
    let prefix = params.get("prefix").cloned().unwrap_or_default();
    let delimiter = params.contains_key("delimiter");
    let after = params.get("continuation-token").cloned().unwrap_or_default();
    let objects = fake.objects.lock().unwrap();
    // Every entry the listing would show, keys or common prefixes, in order and deduplicated.
    let mut entries: Vec<(bool, String)> = Vec::new();
    for key in objects.keys().filter(|k| k.starts_with(&prefix)) {
        let rest = &key[prefix.len()..];
        let entry = match rest.find('/') {
            Some(at) if delimiter => (true, format!("{prefix}{}", &rest[..=at])),
            _ => (false, key.clone()),
        };
        if entries.last() != Some(&entry) {
            entries.push(entry);
        }
    }
    let page: Vec<&(bool, String)> = entries.iter().filter(|(_, e)| *e > after).take(2).collect();
    let truncated = entries.iter().filter(|(_, e)| *e > after).count() > page.len();
    let mut xml = String::from("<ListBucketResult>");
    for (common, entry) in &page {
        if *common {
            xml.push_str(&format!(
                "<CommonPrefixes><Prefix>{}</Prefix></CommonPrefixes>",
                escape(entry)
            ));
        } else {
            xml.push_str(&format!("<Contents><Key>{}</Key></Contents>", escape(entry)));
        }
    }
    xml.push_str(&format!("<IsTruncated>{truncated}</IsTruncated>"));
    if truncated && let Some((_, last)) = page.last() {
        xml.push_str(&format!("<NextContinuationToken>{}</NextContinuationToken>", escape(last)));
    }
    xml.push_str("</ListBucketResult>");
    answer(StatusCode::OK, xml)
}

async fn object(
    State(fake): State<Arc<Fake>>,
    AxumPath((bucket, key)): AxumPath<(String, String)>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(refused) = admit(&fake, &method, &uri, &headers, &body) {
        return *refused;
    }
    if bucket != BUCKET {
        return answer(StatusCode::NOT_FOUND, "NoSuchBucket");
    }
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
    let mut objects = fake.objects.lock().unwrap();
    match method {
        Method::PUT => {
            {
                let mut deny = fake.deny_once.lock().unwrap();
                if deny.as_ref().is_some_and(|d| key.contains(d.as_str())) {
                    *deny = None;
                    return answer(StatusCode::FORBIDDEN, "AccessDenied");
                }
            }
            let current = objects.get(&key).map(|o| o.etag.clone());
            let refused = match (header("if-none-match"), header("if-match")) {
                (Some(_), _) => current.is_some(),
                (_, Some(want)) => current.as_deref() != Some(want.as_str()),
                _ => false,
            };
            if refused {
                return answer(StatusCode::PRECONDITION_FAILED, "PreconditionFailed");
            }
            fake.puts.fetch_add(1, Ordering::SeqCst);
            let etag = format!("\"v{}\"", fake.versions.fetch_add(1, Ordering::SeqCst));
            objects.insert(
                key,
                Object {
                    bytes: body.to_vec(),
                    etag: etag.clone(),
                    modified: chrono::Utc::now(),
                },
            );
            Response::builder()
                .status(StatusCode::OK)
                .header("etag", etag)
                .body(Body::empty())
                .unwrap()
        }
        Method::DELETE => {
            objects.remove(&key);
            answer(StatusCode::NO_CONTENT, Body::empty())
        }
        Method::GET | Method::HEAD => {
            let Some(found) = objects.get(&key) else {
                return answer(StatusCode::NOT_FOUND, "NoSuchKey");
            };
            let mut bytes = found.bytes.clone();
            let mut status = StatusCode::OK;
            if let Some(range) = header("range") {
                let want: usize = range.strip_prefix("bytes=-").unwrap().parse().unwrap();
                if bytes.is_empty() {
                    return answer(StatusCode::RANGE_NOT_SATISFIABLE, "InvalidRange");
                }
                bytes = bytes[bytes.len().saturating_sub(want)..].to_vec();
                status = StatusCode::PARTIAL_CONTENT;
            }
            let response = Response::builder()
                .status(status)
                .header("etag", &found.etag)
                .header(
                    "last-modified",
                    found.modified.format("%a, %d %b %Y %H:%M:%S GMT").to_string(),
                )
                .header("content-length", bytes.len());
            let body = if method == Method::HEAD {
                Body::empty()
            } else {
                Body::from(bytes)
            };
            response.body(body).unwrap()
        }
        _ => answer(StatusCode::METHOD_NOT_ALLOWED, ""),
    }
}

/// A store on the fake, under its own prefix, retrying fast.
fn store_on(endpoint: &str, prefix: &str) -> S3Store {
    let config = S3Config {
        endpoint: endpoint.into(),
        bucket: BUCKET.into(),
        prefix: prefix.into(),
        region: "us-east-1".into(),
    };
    S3Store::new(config, creds()).unwrap().with_backoff(Duration::from_millis(1))
}

/// The two signing examples in AWS's SigV4 documentation for S3 (GET Object with a range, and
/// ListObjects), so the signer is checked against something other than itself.
#[test]
fn the_signer_reproduces_the_published_sigv4_examples() {
    let creds = Credentials {
        access_key_id: "AKIAIOSFODNN7EXAMPLE".into(),
        secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
    };
    let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    let date = "20130524T000000Z";
    let header = |k: &str, v: &str| (k.to_string(), v.to_string());
    let get = authorization(
        &creds,
        "us-east-1",
        date,
        &ToSign {
            method: "GET",
            path: "/test.txt",
            query: &[],
            headers: &[
                header("host", "examplebucket.s3.amazonaws.com"),
                header("range", "bytes=0-9"),
                header("x-amz-content-sha256", empty),
                header("x-amz-date", date),
            ],
            payload_hash: empty,
        },
    );
    assert!(
        get.ends_with("SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"),
        "{get}"
    );
    assert!(get.starts_with("AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request"));
    let list = authorization(
        &creds,
        "us-east-1",
        date,
        &ToSign {
            method: "GET",
            path: "/",
            query: &[("max-keys".into(), "2".into()), ("prefix".into(), "J".into())],
            headers: &[
                header("host", "examplebucket.s3.amazonaws.com"),
                header("x-amz-content-sha256", empty),
                header("x-amz-date", date),
            ],
            payload_hash: empty,
        },
    );
    assert!(
        list.ends_with("Signature=34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7"),
        "{list}"
    );
}

#[test]
fn a_bucket_is_named_by_an_s3_url() {
    let config = S3Config::parse("s3://colonies/team/a?endpoint=http://127.0.0.1:9000/&region=us-east-1").unwrap();
    assert_eq!(
        config,
        S3Config {
            endpoint: "http://127.0.0.1:9000".into(),
            bucket: "colonies".into(),
            prefix: "team/a".into(),
            region: "us-east-1".into(),
        }
    );
    let aws = S3Config::parse("s3://colonies?region=eu-west-1").unwrap();
    assert_eq!(aws.endpoint, "https://s3.eu-west-1.amazonaws.com");
    assert_eq!(aws.prefix, "");
    assert!(S3Config::parse("s3://colonies").is_err(), "R2 and MinIO need an endpoint");
    assert!(S3Config::parse("s3://colonies?endpoint=ftp://x").is_err());
    let dotted = S3Config {
        prefix: "a/../b".into(),
        ..config.clone()
    };
    assert!(dotted.check().is_err(), "a prefix is plain names");
    assert!(
        S3Config::parse("s3://colonies?endpoint=http://x&secret=y").is_err(),
        "credentials never ride the URL"
    );
}

/// The conformance suite every backend runs, against the bucket spoken to directly.
#[tokio::test]
async fn the_contract_holds_for_the_s3_store() {
    let (endpoint, _fake) = serve().await;
    contract(&store_on(&endpoint, "contract"), "s3").await;
    refuses_host_paths(&store_on(&endpoint, "refuse"), "s3").await;
    // The bucket root, no prefix, holds the same promises.
    contract(&store_on(&endpoint, ""), "s3 at the root").await;
}

/// The same suite against the mirror, then the bucket checked against the working copy: after a
/// flush they hold the same sessions, files and bytes.
#[tokio::test]
async fn the_contract_holds_for_the_mirror_and_a_flush_makes_the_bucket_match() {
    let (endpoint, _fake) = serve().await;
    let root = temp_root("mirror-contract");
    let remote: Arc<dyn SessionStore> = Arc::new(store_on(&endpoint, "mirror"));
    let mirror = MirroredStore::open(root.clone(), remote.clone()).await.unwrap();
    contract(mirror.as_ref(), "mirror").await;
    let refusing_root = temp_root("mirror-refuse");
    let refusing = MirroredStore::open(refusing_root.clone(), Arc::new(store_on(&endpoint, "mirror-refuse")))
        .await
        .unwrap();
    refuses_host_paths(refusing.as_ref(), "mirror").await;
    cleanup(&refusing_root);
    mirror.flush().await.unwrap();
    assert_eq!(remote.list_sessions().await.unwrap(), mirror.list_sessions().await.unwrap());
    for id in mirror.list_sessions().await.unwrap() {
        let names = mirror.list_files(&id).await.unwrap();
        assert_eq!(remote.list_files(&id).await.unwrap(), names, "{id}");
        for name in names {
            let local = mirror.read_file(&id, &name).await.unwrap();
            assert_eq!(remote.read_file(&id, &name).await.unwrap(), local, "{id}/{name}");
        }
    }
    // The contract quarantined the index last, so the bucket holds none either.
    assert_eq!(remote.read_index().await.unwrap(), None);
    cleanup(&root);
}

/// Throttling and server errors are retried with backoff; a bucket that keeps failing is an error
/// that says so, not a hang.
#[tokio::test]
async fn requests_retry_through_server_errors_and_give_up_with_the_reason() {
    let (endpoint, fake) = serve().await;
    let s3 = store_on(&endpoint, "retry");
    fake.fail_next.store(ATTEMPTS as usize - 1, Ordering::SeqCst);
    s3.write_file("a", "events.jsonl", b"{}\n").await.unwrap();
    assert_eq!(s3.read_file("a", "events.jsonl").await.unwrap().unwrap(), b"{}\n");
    fake.fail_next.store(ATTEMPTS as usize, Ordering::SeqCst);
    let err = s3.read_file("a", "events.jsonl").await.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::TimedOut, "{err}");
    assert!(err.to_string().contains("HTTP 503"), "{err}");
    // A wrong key is refused at once, not retried.
    let wrong = S3Store::new(
        S3Config {
            endpoint: endpoint.clone(),
            bucket: BUCKET.into(),
            prefix: "retry".into(),
            region: "us-east-1".into(),
        },
        Credentials {
            access_key_id: "SOMEONE".into(),
            secret_access_key: "else".into(),
        },
    )
    .unwrap();
    let err = wrong.read_file("a", "events.jsonl").await.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
}

/// Appends race on a conditional put: concurrent writers each retry until their line lands, so no
/// line is lost to a read-modify-write overlap.
#[tokio::test]
async fn concurrent_appends_lose_no_line() {
    let (endpoint, _fake) = serve().await;
    let s3 = Arc::new(store_on(&endpoint, "race"));
    let writers: Vec<_> = (1..=6)
        .map(|seq| {
            let s3 = s3.clone();
            tokio::spawn(async move { s3.append("a", "events.jsonl", &event(seq)).await })
        })
        .collect();
    for writer in writers {
        writer.await.unwrap().unwrap();
    }
    let log = s3.read_file("a", "events.jsonl").await.unwrap().unwrap();
    let mut seqs: Vec<u64> = String::from_utf8(log)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()["seq"].as_u64().unwrap())
        .collect();
    seqs.sort();
    assert_eq!(seqs, [1, 2, 3, 4, 5, 6]);
}

/// A fresh host: its working copy is empty, so the mirror fills it from the bucket — index last,
/// credentials owner-only — and a later start with the working copy in place uploads nothing.
#[tokio::test]
async fn a_fresh_host_hydrates_its_working_copy_from_the_bucket() {
    use std::os::unix::fs::PermissionsExt;
    let (endpoint, fake) = serve().await;
    let remote: Arc<dyn SessionStore> = Arc::new(store_on(&endpoint, "hydrate"));
    let ids = seed(remote.as_ref()).await;
    remote.write_file(&ids[0], "vm/token", b"tok").await.unwrap();

    let root = temp_root("hydrate");
    let mirror = MirroredStore::open(root.clone(), remote.clone()).await.unwrap();
    assert_eq!(mirror.read_index().await.unwrap(), remote.read_index().await.unwrap());
    for id in &ids {
        assert_eq!(mirror.list_files(id).await.unwrap(), remote.list_files(id).await.unwrap());
    }
    let token = crate::store::local_session_dir(&root, &ids[0]).join("vm/token");
    assert_eq!(std::fs::metadata(token).unwrap().permissions().mode() & 0o777, 0o600);

    let puts = fake.puts.load(Ordering::SeqCst);
    drop(mirror);
    let again = MirroredStore::open(root.clone(), remote.clone()).await.unwrap();
    again.flush().await.unwrap();
    assert_eq!(fake.puts.load(Ordering::SeqCst), puts, "nothing was owed");
    cleanup(&root);
}

/// The write-ahead half: a write that reached the working copy but not the bucket — a crash in
/// between, or a file the microVM wrote itself — is found by the sweep on the next start and
/// uploaded; an upload that fails stays owed until one succeeds.
#[tokio::test]
async fn what_the_bucket_missed_is_swept_up_and_retried() {
    let (endpoint, fake) = serve().await;
    let remote: Arc<dyn SessionStore> = Arc::new(store_on(&endpoint, "sweep"));
    let root = temp_root("sweep");
    let mirror = MirroredStore::open(root.clone(), remote.clone()).await.unwrap();
    mirror.write_index(br#"[{"id":"a1b2c3d4"}]"#).await.unwrap();
    mirror.append("a1b2c3d4", "events.jsonl", &event(1)).await.unwrap();
    mirror.flush().await.unwrap();

    // The microVM writes its pull request description straight into the working copy.
    let out = crate::store::local_session_dir(&root, "a1b2c3d4").join("out");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("pr.md"), "# Title\n").unwrap();
    drop(mirror);

    let mirror = MirroredStore::open(root.clone(), remote.clone()).await.unwrap();
    *fake.deny_once.lock().unwrap() = Some("out/pr.md".into());
    mirror.flush().await.unwrap_err();
    assert_eq!(
        remote.read_file("a1b2c3d4", "out/pr.md").await.unwrap(),
        None,
        "the refused put"
    );
    mirror.flush().await.unwrap();
    assert_eq!(
        remote.read_file("a1b2c3d4", "out/pr.md").await.unwrap().unwrap(),
        b"# Title\n"
    );

    // A removed session goes from the bucket too.
    mirror.remove_session("a1b2c3d4").await.unwrap();
    mirror.flush().await.unwrap();
    assert!(remote.list_files("a1b2c3d4").await.unwrap().is_empty());
    cleanup(&root);
}

/// Settings for a local command, with a data dir and config dir of the test's own and the bucket
/// credentials saved where the secrets mechanism reads them.
fn settings(root: &Path) -> Settings {
    let cfg = Settings {
        bind: "127.0.0.1:0".into(),
        data_dir: root.join("data"),
        config_dir: root.join("config"),
        runtime_dir: PathBuf::new(),
        assets: None,
        msb: "msb".into(),
        claude_bin: None,
        gateway_bind: "127.0.0.1:0".parse().unwrap(),
        allowed_hosts: Vec::new(),
        fleet_peers: Vec::new(),
        bench_pool: None,
    };
    crate::util::write_file_secret(&cfg.config_dir.join(ACCESS_KEY_FILE), KEY_ID).unwrap();
    crate::util::write_file_secret(&cfg.config_dir.join(SECRET_KEY_FILE), SECRET).unwrap();
    cfg
}

/// The whole move, end to end: `sessions migrate` from the local store into a bucket — dry run,
/// an interrupted run, the resumed one — switches `session-store.json` only once the copy verified;
/// the mothership's store then opens on the bucket without uploading the copy again; and migrating
/// back to `local` switches back.
#[tokio::test]
async fn sessions_migrate_moves_to_a_bucket_switches_and_moves_back() {
    use crate::store_config::{Backend, CONFIG_FILE, load, migrate_command};
    let (endpoint, fake) = serve().await;
    let root = temp_root("to-bucket");
    let cfg = settings(&root);
    let local = LocalDirStore::new(cfg.data_dir.clone());
    let ids = seed(&local).await;
    let to = format!("s3://{BUCKET}/install?endpoint={endpoint}&region=us-east-1");

    migrate_command(&cfg, None, &to, true, false).await.unwrap();
    assert_eq!(fake.puts.load(Ordering::SeqCst), 0, "a dry run writes nothing");
    assert_eq!(load(&cfg.config_dir).unwrap(), Backend::Local);

    *fake.deny_once.lock().unwrap() = Some(format!("{}/", ids[1]));
    migrate_command(&cfg, None, &to, false, false).await.unwrap_err();
    assert_eq!(
        load(&cfg.config_dir).unwrap(),
        Backend::Local,
        "no switch without a verified copy"
    );

    migrate_command(&cfg, None, &to, false, false).await.unwrap();
    let Backend::S3(config) = load(&cfg.config_dir).unwrap() else {
        panic!(
            "the setting names the bucket: {}",
            std::fs::read_to_string(cfg.config_dir.join(CONFIG_FILE)).unwrap()
        );
    };
    assert_eq!(config.prefix, "install");
    let remote = store_on(&endpoint, "install");
    assert_eq!(remote.read_index().await.unwrap(), local.read_index().await.unwrap());

    // The mothership's open: the mirror over the data dir, which already holds every byte.
    let puts = fake.puts.load(Ordering::SeqCst);
    let store = Backend::S3(config).open(&cfg).await.unwrap();
    assert_eq!(store.list_sessions().await.unwrap(), ids);
    assert_eq!(
        fake.puts.load(Ordering::SeqCst),
        puts,
        "the migrated copy is not uploaded again"
    );
    drop(store);

    // And back: the data dir already holds the same colonies, so the copy re-verifies and switches.
    migrate_command(&cfg, None, "local", false, false).await.unwrap();
    assert_eq!(load(&cfg.config_dir).unwrap(), Backend::Local);
    cleanup(&root);
}

/// The memory reference store mirrors the same way: the mirror does not depend on S3 at all.
#[tokio::test]
async fn the_mirror_works_over_any_store_of_record() {
    let remote: Arc<dyn SessionStore> = Arc::new(MemoryObjectStore::new());
    let root = temp_root("mirror-memory");
    let mirror = MirroredStore::open(root.clone(), remote.clone()).await.unwrap();
    derived(mirror.as_ref(), "mirror over memory").await;
    put(mirror.as_ref(), "a1b2c3d4", "events.jsonl", &lines(&[event(1)])).await;
    mirror.flush().await.unwrap();
    assert_eq!(
        remote.read_file("a1b2c3d4", "events.jsonl").await.unwrap().unwrap(),
        lines(&[event(1)])
    );
    let migrated = migrate(remote.as_ref(), &MemoryObjectStore::new(), true).await.unwrap();
    assert_eq!(migrated.files, 1);
    cleanup(&root);
}

/// Against a real bucket when one is named: `COLONIZER_TEST_S3` is an `s3://` URL, and the key pair
/// comes from the same environment variables the mothership reads. Each run uses a fresh prefix.
#[tokio::test]
async fn the_contract_holds_against_a_real_bucket_when_one_is_named() {
    let Some(spec) = crate::util::env_nonempty("COLONIZER_TEST_S3") else {
        return;
    };
    let mut config = S3Config::parse(&spec).unwrap();
    config.prefix = format!("{}/colonizer-test-{}", config.prefix, crate::util::short_id())
        .trim_start_matches('/')
        .to_string();
    let creds = Credentials::resolve(Path::new("/nonexistent")).unwrap();
    let s3 = S3Store::new(config, creds).unwrap();
    contract(&s3, "real bucket").await;
    for id in s3.list_sessions().await.unwrap() {
        s3.remove_session(&id).await.unwrap();
    }
}
