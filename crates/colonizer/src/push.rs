//! The push module: Web Push notifications to the phones and desktops an operator actually
//! carries, the third channel beside the desktop popup and the webhook (issue #516).
//!
//! Two protocols do the work. RFC 8291 (`aes128gcm`) seals each message to the subscription's
//! key with a fresh ephemeral ECDH key, so the push service that relays it — a browser maker's,
//! not ours — carries bytes it cannot open. RFC 8292 (VAPID) signs every request with a key
//! generated on first use and kept beside the other secrets, so the push service knows the sender
//! and the browser knows who was allowed to wake it. Subscribing is the opt-in: there is no
//! module setting to forget, and removing a subscription in Settings ends the channel for that
//! device alone.
//!
//! Like the other channels, what leaves is one short line about the colony and a link — never
//! question text, agent output, or any repository content.

use crate::{
    ApiResult, App, AppError, Shared, client_error,
    protocol::Origin,
    util::{b64_decode, b64_encode, read_secret, short_id, truncate, write_private, write_secret},
};
use anyhow::{Result, anyhow};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::Utc;
use ring::{
    aead::{AES_128_GCM, Aad, LessSafeKey, Nonce, UnboundKey},
    agreement, hkdf,
    rand::{SecureRandom as _, SystemRandom},
    signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair as _},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path as FsPath, PathBuf};

/// ring's errors are deliberately unspecifiable — no details, no cause — so each call site names
/// the step that failed instead.
fn unspecified(what: &'static str) -> impl Fn(ring::error::Unspecified) -> anyhow::Error {
    move |_| anyhow!("{what}")
}

/// The VAPID `sub` claim: who runs the sender. A URL, not a mailto — Apple's push service
/// rejects mailto addresses on unclaimed origins.
const DEFAULT_SUBJECT: &str = "https://github.com/Colonizer-dev/harness";

/// How long a VAPID JWT stays fresh. A push service must reject one older than its `exp`, so this
/// wants to be long enough that a clock a little off is no problem and short enough that a stolen
/// token dies on its own.
const JWT_TTL_SECS: i64 = 12 * 60 * 60;

/// The most a notification body carries: repository, issue number and a few words — the same one
/// line the desktop popup shows.
const MAX_BODY: usize = 120;

/// The most characters a device label takes, so a pasted paragraph cannot sprawl across Settings.
const MAX_LABEL: usize = 60;

/// RFC 8291: one record, and the header says so — a receiver may only have to handle one.
const RECORD_SIZE: u32 = 4096;

// ---------------------------------------------------------------------------
// base64url — the alphabet the browser hands these keys around in
// ---------------------------------------------------------------------------

fn b64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// The inverse, tolerating the `=` padding browsers are allowed to send.
fn un_b64url(text: &str) -> Result<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(text.trim_end_matches('='))
        .map_err(|_| anyhow!("not base64url"))
}

// ---------------------------------------------------------------------------
// The VAPID key, stored like the webhook signing secret
// ---------------------------------------------------------------------------

fn key_file(app: &App) -> PathBuf {
    app.cfg.config_dir.join("push-vapid-key")
}

/// The VAPID signing key, generated on first use and kept like every other secret — base64 PKCS#8
/// through `write_secret`, so the system keychain when it is available and a 0600 file otherwise,
/// never in modules.json and never out through the API. One key signs for every device: it is this
/// mothership's identity, and the browser shows it as the subscription's origin permission.
pub fn signing_key(app: &App) -> Result<EcdsaKeyPair> {
    let rng = SystemRandom::new();
    if let Some(saved) = read_secret(&key_file(app)) {
        let der = b64_decode(saved.trim()).ok_or_else(|| anyhow!("the saved VAPID key is not base64"))?;
        return EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &der, &rng)
            .map_err(|e| anyhow!("the saved VAPID key could not be used ({e})"));
    }
    let generated = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
        .map_err(unspecified("a new VAPID key could not be generated"))?;
    write_secret(&key_file(app), &b64_encode(generated.as_ref()))?;
    EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, generated.as_ref(), &rng)
        .map_err(|e| anyhow!("the generated VAPID key could not be read back ({e})"))
}

/// The RFC 8292 authorization for one endpoint: a JWT whose audience is the endpoint's origin,
/// signed with the mothership's key and sent as `vapid t=…, k=…` next to the public key a push
/// service needs to check it.
fn vapid_token(key: &EcdsaKeyPair, audience: &str, now: i64, subject: &str) -> Result<String> {
    let header = b64url(br#"{"typ":"JWT","alg":"ES256"}"#);
    let claims = serde_json::to_vec(&json!({ "aud": audience, "exp": now + JWT_TTL_SECS, "sub": subject }))?;
    let signing_input = format!("{header}.{}", b64url(&claims));
    // ES256: ring signs the fixed 64-byte r||s form the JWT expects.
    let signature = key
        .sign(&SystemRandom::new(), signing_input.as_bytes())
        .map_err(unspecified("the VAPID JWT could not be signed"))?;
    Ok(format!("{signing_input}.{}", b64url(signature.as_ref())))
}

/// The origin a JWT may name as its audience: `https://` and the authority, nothing more — a push
/// service compares it against the endpoint's own origin, so a path or query would be rejected.
fn audience(endpoint: &str) -> Option<String> {
    let authority = endpoint.strip_prefix("https://")?.split(['/', '?', '#']).next()?;
    (!authority.is_empty()).then(|| format!("https://{authority}"))
}

/// Who runs the sender, as the JWT's `sub`: the operator's override or the project's home.
fn subject() -> String {
    std::env::var("COLONIZER_VAPID_SUBJECT")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_SUBJECT.to_string())
}

// ---------------------------------------------------------------------------
// RFC 8291 encryption
// ---------------------------------------------------------------------------

/// The `aes128gcm` content coding (RFC 8291 with RFC 8188), given every input the random path
/// supplies, so the arithmetic itself is checkable against the RFC's own worked example. The body
/// is the 86-byte header — salt, one-record size, the ephemeral public key — followed by the
/// plaintext, a 0x02 padding delimiter and the AEAD tag, all under one key: CEK and nonce are
/// HKDF of the ECDH secret combined with the subscription's auth secret.
fn seal(
    ecdh_secret: &[u8],
    as_public: &[u8],
    ua_public: &[u8],
    auth_secret: &[u8],
    salt: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    // RFC 8291 §3.3: combine the ECDH and authentication secrets into the RFC 8188 input keying
    // material — Extract with the auth secret, Expand with the WebPush info — then §3.4 re-extracts
    // with the record salt and expands the record's key and nonce out of that.
    let mut info = Vec::with_capacity(14 + ua_public.len() + as_public.len());
    info.extend_from_slice(b"WebPush: info");
    info.push(0);
    info.extend_from_slice(ua_public);
    info.extend_from_slice(as_public);
    let mut ikm = [0u8; 32];
    hkdf::Salt::new(hkdf::HKDF_SHA256, auth_secret)
        .extract(ecdh_secret)
        .expand(&[&info], hkdf::HKDF_SHA256)
        .map_err(unspecified("the WebPush keying material could not be derived"))?
        .fill(&mut ikm)
        .map_err(unspecified("the WebPush keying material could not be derived"))?;
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(&ikm);
    let mut cek = [0u8; 32];
    prk.expand(&[b"Content-Encoding: aes128gcm\0"], hkdf::HKDF_SHA256)
        .map_err(unspecified("the content encryption key could not be derived"))?
        .fill(&mut cek)
        .map_err(unspecified("the content encryption key could not be derived"))?;
    let mut nonce = [0u8; 32];
    prk.expand(&[b"Content-Encoding: nonce\0"], hkdf::HKDF_SHA256)
        .map_err(unspecified("the record nonce could not be derived"))?
        .fill(&mut nonce)
        .map_err(unspecified("the record nonce could not be derived"))?;

    // The header: salt(16) || rs(4, big-endian) || keyid length(1) || keyid, the ephemeral point.
    // It stays in the clear; only the record after it is sealed.
    let mut body = Vec::with_capacity(86 + plaintext.len() + 1 + 16);
    body.extend_from_slice(salt);
    body.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    body.push(as_public.len() as u8);
    body.extend_from_slice(as_public);
    // The record: the plaintext, a 0x02 delimiter ending it, and the AEAD tag. One record, the
    // only shape a push service must take.
    let mut record = plaintext.to_vec();
    record.push(0x02);
    let key = LessSafeKey::new(UnboundKey::new(&AES_128_GCM, &cek[..16]).map_err(unspecified("the record key is unusable"))?);
    let nonce = Nonce::try_assume_unique_for_key(&nonce[..12]).map_err(unspecified("the record nonce is unusable"))?;
    key.seal_in_place_append_tag(nonce, Aad::empty(), &mut record)
        .map_err(unspecified("the record could not be encrypted"))?;
    body.extend_from_slice(&record);
    Ok(body)
}

/// [`seal`]'s production half: a fresh ephemeral ECDH key and salt per message, sealed to a
/// subscription's `p256dh` key and auth secret. The ephemeral key is discarded — each message
/// names its own key in the header.
fn encrypt(ua_public: &[u8], auth_secret: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
    let rng = SystemRandom::new();
    let ephemeral = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng)
        .map_err(unspecified("an ephemeral key could not be generated"))?;
    let as_public = ephemeral
        .compute_public_key()
        .map_err(unspecified("the ephemeral public key could not be computed"))?;
    let mut salt = [0u8; 16];
    rng.fill(&mut salt)
        .map_err(unspecified("a record salt could not be generated"))?;
    // A peer key that is not a P-256 point fails the agreement, which is the one thing to say
    // about it; seal's own errors come through the inner result.
    match agreement::agree_ephemeral(
        ephemeral,
        &agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, ua_public),
        |ecdh_secret| seal(ecdh_secret, as_public.as_ref(), ua_public, auth_secret, &salt, plaintext),
    ) {
        Ok(Ok(body)) => Ok(body),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(anyhow!("the subscription's p256dh key is not a valid P-256 point")),
    }
}

// ---------------------------------------------------------------------------
// The payload
// ---------------------------------------------------------------------------

/// The four-key push payload. `text` is the same one line the desktop popup and the webhook carry
/// — the only content this function is handed, so repository content cannot travel through it —
/// and `session` only names the colony, for the deep link and the tag. Provider and digest events
/// have no colony and link to the cockpit's front page.
pub fn payload(event: &str, text: &str, session: Option<&str>) -> Value {
    let (url, tag) = match session {
        Some(id) => (format!("/?colony={id}"), format!("{event}-{id}")),
        None => ("/".to_string(), event.to_string()),
    };
    json!({
        "title": title(event),
        "body": one_line(text),
        "url": url,
        "tag": tag,
    })
}

/// The short title per event. A notification shade has room for one line of sense, not a sentence.
fn title(event: &str) -> &'static str {
    match event {
        "question" => "Colony asks a question",
        "attention" => "Colony stalled",
        "failed" => "Colony failed",
        "pull_request" => "Pull request opened",
        "provider_degraded" => "Provider degraded",
        "needs_rebase" => "Needs rebase",
        "digest" => "Colony digest",
        _ => "Colonizer",
    }
}

/// The body is one line, hard-stopped: a notification shade folds on newlines, and nothing that
/// long was one thought anyway.
fn one_line(text: &str) -> String {
    let flat: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    truncate(flat.trim(), MAX_BODY)
}

// ---------------------------------------------------------------------------
// The subscription store
// ---------------------------------------------------------------------------

/// One device's subscription, as the browser's `PushSubscription` gave it to us. The endpoint is a
/// capability URL — anyone holding it can wake that device — so the list is written 0600 and the
/// API hands out only each host's name, never the URL or the keys.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Subscription {
    pub id: String,
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
    pub label: String,
    pub created_at: i64,
}

fn subscriptions_file(config_dir: &FsPath) -> PathBuf {
    config_dir.join("push-subscriptions.json")
}

/// The list, newest last. A missing file is an empty list; a damaged one is an error, so a save
/// never overwrites it with nothing — the same rule colony-secrets follows.
fn load(config_dir: &FsPath) -> Result<Vec<Subscription>> {
    let path = subscriptions_file(config_dir);
    match std::fs::read(&path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|e| anyhow!("{} could not be parsed ({e}); fix or remove it", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(anyhow!("{} could not be read ({e})", path.display())),
    }
}

fn save(config_dir: &FsPath, list: &[Subscription]) -> Result<()> {
    write_private(&subscriptions_file(config_dir), &serde_json::to_vec_pretty(list)?)
}

/// What POST /api/push/subscriptions takes: the `PushSubscription.toJSON()` shape the browser
/// hands over, plus an optional label.
#[derive(Deserialize)]
pub struct NewSubscription {
    endpoint: String,
    keys: SubscriptionKeys,
    label: Option<String>,
}

#[derive(Deserialize)]
pub struct SubscriptionKeys {
    p256dh: String,
    auth: String,
}

/// A subscription as `check` left it: the fields to store, normalised.
struct Checked {
    endpoint: String,
    p256dh: String,
    auth: String,
    label: String,
}

/// Checks a subscription before it is stored. The endpoint must be an `https://` URL (we POST
/// nowhere else, and the endpoint is a capability that must not travel in the clear); the p256dh
/// key must decode to a 65-byte uncompressed P-256 point (RFC 8291's exact form) and the auth
/// secret to its 16 bytes — refusing them here beats a failure on every future send.
fn check(new: NewSubscription) -> Result<Checked, String> {
    let endpoint = new.endpoint.trim().to_string();
    if !endpoint.starts_with("https://") {
        return Err("the endpoint must be an https:// push service URL".into());
    }
    if endpoint.len() > 2048 {
        return Err("the endpoint URL is too long".into());
    }
    let point = un_b64url(&new.keys.p256dh).map_err(|_| "the p256dh key is not valid base64url".to_string())?;
    if point.len() != 65 || point[0] != 0x04 {
        return Err("the p256dh key must be a 65-byte uncompressed P-256 point".into());
    }
    let auth = un_b64url(&new.keys.auth).map_err(|_| "the auth secret is not valid base64url".to_string())?;
    if auth.len() != 16 {
        return Err("the auth secret must be 16 bytes".into());
    }
    let label = match new.label.as_deref().map(str::trim) {
        None | Some("") => "This device".to_string(),
        Some(label) => truncate(label, MAX_LABEL),
    };
    Ok(Checked {
        endpoint,
        p256dh: new.keys.p256dh,
        auth: new.keys.auth,
        label,
    })
}

/// Adds a subscription, or refreshes an endpoint's keys — a browser re-subscribing in place
/// rotates them and keeps the device's row, id and age.
fn upsert(config_dir: &FsPath, checked: Checked) -> Result<Subscription> {
    let mut list = load(config_dir)?;
    let entry = match list.iter_mut().find(|s| s.endpoint == checked.endpoint) {
        Some(existing) => {
            existing.p256dh = checked.p256dh;
            existing.auth = checked.auth;
            existing.label = checked.label;
            existing.clone()
        }
        None => {
            let entry = Subscription {
                id: short_id(),
                endpoint: checked.endpoint,
                p256dh: checked.p256dh,
                auth: checked.auth,
                label: checked.label,
                created_at: Utc::now().timestamp(),
            };
            list.push(entry.clone());
            entry
        }
    };
    save(config_dir, &list)?;
    Ok(entry)
}

/// Removes one subscription by id; answers whether it was there.
fn delete(config_dir: &FsPath, id: &str) -> Result<bool> {
    let mut list = load(config_dir)?;
    let before = list.len();
    list.retain(|s| s.id != id);
    if list.len() == before {
        return Ok(false);
    }
    save(config_dir, &list)?;
    Ok(true)
}

/// What the API says about one subscription: the host's name, the label and the age — never the
/// endpoint URL (a capability) or the keys.
fn summary(subscription: &Subscription) -> Value {
    json!({
        "id": subscription.id,
        "label": subscription.label,
        "created_at": subscription.created_at,
        "endpoint_host": endpoint_host(&subscription.endpoint),
    })
}

/// The host part of an endpoint URL, for showing `fcm.googleapis.com` without the capability path.
fn endpoint_host(endpoint: &str) -> String {
    let rest = endpoint.split("://").nth(1).unwrap_or(endpoint);
    rest.split(['/', '?', '#'])
        .next()
        .unwrap_or(rest)
        .rsplit('@')
        .next()
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default()
        .to_string()
}

// ---------------------------------------------------------------------------
// API
// ---------------------------------------------------------------------------

/// `GET /api/push/key`: the VAPID public key the browser subscribes with, creating the key on
/// first use.
pub async fn public_key(State(app): State<Shared>) -> ApiResult<Value> {
    let key = signing_key(&app)?;
    Ok(Json(json!({ "public_key": b64url(key.public_key().as_ref()) })))
}

/// `GET /api/push/subscriptions`: the saved devices, summaries only.
pub async fn list_subscriptions(State(app): State<Shared>) -> ApiResult<Value> {
    let list = load(&app.cfg.config_dir)?;
    Ok(Json(Value::Array(list.iter().map(summary).collect())))
}

/// `POST /api/push/subscriptions`: saves a device. The same endpoint again refreshes its keys.
pub async fn add_subscription(State(app): State<Shared>, Json(body): Json<NewSubscription>) -> ApiResult<Value> {
    let checked = check(body).map_err(|m| client_error(StatusCode::BAD_REQUEST, &m))?;
    let entry = upsert(&app.cfg.config_dir, checked)?;
    Ok(Json(summary(&entry)))
}

/// `DELETE /api/push/subscriptions/{id}`: forgets one device.
pub async fn delete_subscription(State(app): State<Shared>, Path(id): Path<String>) -> Result<StatusCode, AppError> {
    if delete(&app.cfg.config_dir, &id)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(client_error(StatusCode::NOT_FOUND, "no such subscription"))
    }
}

// ---------------------------------------------------------------------------
// Delivery
// ---------------------------------------------------------------------------

/// Why one send did not happen. Gone is not a fault: the push service is telling us the device
/// unsubscribed in a way the browser could not report (cleared site data, an expired capability),
/// so the subscription is dropped instead of retried forever.
enum NotSent {
    Gone(String),
    Failed(String),
}

/// The push channel of [`crate::notify`]'s `deliver`: one announcement to every subscribed
/// device, behind the same ledger and event switches as the other channels. Answers whether at
/// least one device took it, like they do; each failure is a line in the colony's log (or stderr,
/// for the events with no colony), never a fault in the others.
pub async fn deliver(app: &App, client: &reqwest::Client, event: &str, text: &str, session: Option<&str>) -> bool {
    let list = match load(&app.cfg.config_dir) {
        Ok(list) => list,
        Err(e) => {
            eprintln!("push: the subscription list could not be read ({e:#}); nothing sent");
            return false;
        }
    };
    if list.is_empty() {
        return false;
    }
    let key = match signing_key(app) {
        Ok(key) => key,
        Err(e) => {
            eprintln!("push: the VAPID key is unavailable ({e:#}); nothing sent");
            return false;
        }
    };
    let Ok(body) = serde_json::to_vec(&payload(event, text, session)) else {
        return false;
    };
    // A question blocks a colony, so the phone buzzes like it matters; the rest can wait for the
    // device's own idea of a good moment.
    let urgency = if event == "question" { "high" } else { "normal" };
    let mut sent = false;
    let mut gone: Vec<String> = Vec::new();
    for subscription in &list {
        match send(client, &key, subscription, &body, urgency).await {
            Ok(()) => sent = true,
            Err(NotSent::Gone(why)) => {
                report(
                    app,
                    session,
                    format!(
                        "push: subscription {} ({}) dropped: {why}",
                        subscription.label, subscription.id
                    ),
                    "info",
                )
                .await;
                gone.push(subscription.id.clone());
            }
            Err(NotSent::Failed(why)) => {
                report(app, session, format!("push: {} failed: {why}", subscription.label), "warn").await;
            }
        }
    }
    if !gone.is_empty() {
        // Prune out of a list re-read now, not the one loaded before the sends: a device that
        // subscribed while the push services above were being reached must not be lost with them.
        match load(&app.cfg.config_dir) {
            Ok(mut remaining) => {
                remaining.retain(|s| !gone.contains(&s.id));
                if let Err(e) = save(&app.cfg.config_dir, &remaining) {
                    eprintln!("push: could not save the subscription list ({e:#})");
                }
            }
            Err(e) => eprintln!("push: could not re-read the subscription list to prune it ({e:#}); the gone ones stay"),
        }
    }
    sent
}

/// One encrypted POST to one push service: RFC 8291 body, RFC 8292 authorization, a day to
/// deliver and the urgency to say whether that matters. No retries — the next event tries again.
async fn send(
    client: &reqwest::Client,
    key: &EcdsaKeyPair,
    subscription: &Subscription,
    body: &[u8],
    urgency: &str,
) -> Result<(), NotSent> {
    let failed = |e: anyhow::Error| NotSent::Failed(format!("{e:#}"));
    let ua_public =
        un_b64url(&subscription.p256dh).map_err(|e| NotSent::Failed(format!("the saved p256dh key is unusable ({e})")))?;
    let auth = un_b64url(&subscription.auth).map_err(|e| NotSent::Failed(format!("the saved auth secret is unusable ({e})")))?;
    let encrypted = encrypt(&ua_public, &auth, body).map_err(failed)?;
    let audience = audience(&subscription.endpoint).ok_or_else(|| NotSent::Failed("the endpoint names no origin".into()))?;
    let token = vapid_token(key, &audience, Utc::now().timestamp(), &subject()).map_err(failed)?;
    let response = client
        .post(&subscription.endpoint)
        .header("Content-Encoding", "aes128gcm")
        .header("Content-Type", "application/octet-stream")
        .header("TTL", "86400")
        .header("Urgency", urgency)
        .header(
            "Authorization",
            format!("vapid t={token}, k={}", b64url(key.public_key().as_ref())),
        )
        .body(encrypted)
        .send()
        .await
        .map_err(|e| NotSent::Failed(format!("{e}")))?;
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    if status == StatusCode::NOT_FOUND || status == StatusCode::GONE {
        return Err(NotSent::Gone(format!("the push service answered {status}")));
    }
    Err(NotSent::Failed(format!("the push service answered {status}")))
}

/// Where a failed send's line goes: the colony's log when the event is about a colony, stderr when
/// it is not — the same rule notify's own channel failures follow.
async fn report(app: &App, session: Option<&str>, what: String, level: &str) {
    match session {
        Some(id) => app.session_log_as(Origin::Notify, id, level, what).await,
        None => eprintln!("{what}"),
    }
}

/// The API routes this module serves. `server::api_routes` merges them into the cockpit's router,
/// behind the activity log's route layer and `host_guard`.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new()
        .route("/api/push/key", routing::get(public_key))
        .route(
            "/api/push/subscriptions",
            routing::get(list_subscriptions).post(add_subscription),
        )
        .route("/api/push/subscriptions/{id}", routing::delete(delete_subscription))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::Event;

    /// A colony as `sessions.json` holds one, with a sentinel in every free-text field: none of it
    /// may reach a push payload, because [`payload`] is only ever handed the one line and the id.
    fn colony() -> crate::sessions::Session {
        serde_json::from_value(json!({
            "id": "abc123",
            "repo": "acme/webshop",
            "org": "acme",
            "issue": 42,
            "issue_title": "SENTINEL-issue-title",
            "instructions": "fix the bug. sk-ant-api03-SENTINEL",
            "status": "waiting_for_answer",
            "branch": "colonizer/SENTINEL-branch",
            "worktree": "/colonizer/worktrees/wt",
            "sandbox": "colonizer-abc123",
            "agent": "claude",
            "pr_url": "https://github.com/acme/webshop/pull/7",
            "error": "SENTINEL-error",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
        }))
        .unwrap()
    }

    #[test]
    fn encryption_matches_the_rfc_8291_example_byte_for_byte() {
        // Appendix A's inputs and the exact body Section 5's example sends, pins the whole scheme:
        // the HKDF combination, the 86-byte header, the 0x02 delimiter and the AEAD tag.
        let ecdh_secret = un_b64url("kyrL1jIIOHEzg3sM2ZWRHDRB62YACZhhSlknJ672kSs").unwrap();
        let as_public =
            un_b64url("BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8").unwrap();
        let ua_public =
            un_b64url("BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4").unwrap();
        let auth = un_b64url("BTBZMqHH6r4Tts7J_aSIgg").unwrap();
        let salt = un_b64url("DGv6ra1nlYgDCS1FRnbzlw").unwrap();
        let body = seal(
            &ecdh_secret,
            &as_public,
            &ua_public,
            &auth,
            &salt,
            b"When I grow up, I want to be a watermelon",
        )
        .unwrap();
        assert_eq!(
            b64url(&body),
            "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN"
        );
    }

    #[test]
    fn the_production_path_wraps_one_record_around_a_fresh_key_and_salt() {
        let rng = SystemRandom::new();
        let ua = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng).unwrap();
        let ua_public = ua.compute_public_key().unwrap();
        let body = encrypt(ua_public.as_ref(), &[9u8; 16], b"hello").unwrap();
        assert_eq!(body.len(), 86 + 5 + 17, "header, plaintext, delimiter, tag");
        assert_eq!(&body[16..20], &RECORD_SIZE.to_be_bytes());
        assert_eq!(body[20], 65);
        assert_eq!(body[21], 0x04, "the keyid is an uncompressed point");
        let other = encrypt(ua_public.as_ref(), &[9u8; 16], b"hello").unwrap();
        assert_ne!(&body[..16], &other[..16], "a fresh salt every time");
    }

    #[test]
    fn the_vapid_token_is_a_jwt_the_browser_can_check() {
        let rng = SystemRandom::new();
        let generated = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, generated.as_ref(), &rng).unwrap();
        let token = vapid_token(&key, "https://fcm.googleapis.com", 1_789_000_000, "https://colonizer.dev").unwrap();
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3);
        for part in &parts {
            assert!(!part.contains('='), "JWT parts are unpadded base64url");
        }
        let header_bytes = un_b64url(parts[0]).unwrap();
        let header = std::str::from_utf8(&header_bytes).unwrap();
        assert_eq!(header, r#"{"typ":"JWT","alg":"ES256"}"#);
        let claims: Value = serde_json::from_slice(&un_b64url(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["aud"], "https://fcm.googleapis.com");
        assert_eq!(claims["exp"], 1_789_000_000 + JWT_TTL_SECS);
        assert_eq!(claims["sub"], "https://colonizer.dev");
        // The signature is the fixed 64-byte r||s form, and verifies against the key's public half.
        let signature_bytes = un_b64url(parts[2]).unwrap();
        assert_eq!(signature_bytes.len(), 64);
        ring::signature::UnparsedPublicKey::new(&ring::signature::ECDSA_P256_SHA256_FIXED, key.public_key().as_ref())
            .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature_bytes)
            .expect("the JWT signature verifies");
    }

    #[test]
    fn the_audience_is_the_endpoints_origin_and_the_host_is_its_name() {
        assert_eq!(
            audience("https://fcm.googleapis.com/fcm/send/abc").as_deref(),
            Some("https://fcm.googleapis.com")
        );
        assert_eq!(
            audience("https://push.apple.com:443/3/abc").as_deref(),
            Some("https://push.apple.com:443"),
            "a port is part of the origin"
        );
        assert_eq!(audience("http://fcm.googleapis.com/x"), None, "we only ever POST to https");
        assert_eq!(audience("https://"), None);
        assert_eq!(endpoint_host("https://fcm.googleapis.com/fcm/send/abc"), "fcm.googleapis.com");
        assert_eq!(endpoint_host("not a url"), "not a url");
    }

    #[test]
    fn each_event_has_a_title_and_coloniless_events_link_to_the_front_page() {
        for (event, want) in [
            ("question", "Colony asks a question"),
            ("attention", "Colony stalled"),
            ("failed", "Colony failed"),
            ("pull_request", "Pull request opened"),
            ("provider_degraded", "Provider degraded"),
            ("needs_rebase", "Needs rebase"),
            ("digest", "Colony digest"),
        ] {
            assert_eq!(title(event), want);
        }
        assert_eq!(title("anything-else"), "Colonizer");
        let provider = payload("provider_degraded", "zai is failing 29.4% of its requests", None);
        assert_eq!(provider["url"], "/");
        assert_eq!(provider["tag"], "provider_degraded");
    }

    #[test]
    fn the_push_payload_carries_four_keys_and_no_session_content() {
        let session = colony();
        let text = Event::Question.text(&session.repo, session.issue);
        let body = serde_json::to_string(&payload("question", &text, Some(&session.id))).unwrap();
        for sentinel in [
            "SENTINEL-issue-title",
            "SENTINEL-branch",
            "SENTINEL-error",
            "sk-ant-api03-SENTINEL",
        ] {
            assert!(!body.contains(sentinel), "the push payload leaked {sentinel}: {body}");
        }
        let value: Value = serde_json::from_str(&body).unwrap();
        let mut keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["body", "tag", "title", "url"],
            "exactly four keys, one shape for the service worker"
        );
        assert_eq!(value["title"], "Colony asks a question");
        assert_eq!(value["body"], "acme/webshop #42 needs an answer");
        assert_eq!(value["url"], "/?colony=abc123");
        assert_eq!(value["tag"], "question-abc123");
    }

    #[test]
    fn the_body_is_one_bounded_line() {
        assert_eq!(one_line("first\nsecond\r\nthird"), "first second  third");
        let long: String = "x".repeat(MAX_BODY + 40);
        assert_eq!(one_line(&long).chars().count(), MAX_BODY + 1, "capped, ellipsis included");
    }

    /// The keys of a subscription `check` accepts: a 65-byte uncompressed point, a 16-byte secret.
    fn good_keys() -> SubscriptionKeys {
        let mut point = vec![0x04u8];
        point.resize(65, 1);
        SubscriptionKeys {
            p256dh: b64url(&point),
            auth: b64url(&[9u8; 16]),
        }
    }

    /// A subscription POST body whose keys pass `check`.
    fn new_subscription(endpoint: &str, label: Option<&str>) -> NewSubscription {
        NewSubscription {
            endpoint: endpoint.into(),
            keys: good_keys(),
            label: label.map(String::from),
        }
    }

    #[test]
    fn subscriptions_round_trip_upsert_and_delete() {
        let dir = std::env::temp_dir().join(format!("colonizer-push-{}", short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = upsert(
            &dir,
            check(new_subscription(
                "  https://fcm.googleapis.com/fcm/send/abc ",
                Some(" Pixel "),
            ))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(first.label, "Pixel", "the label is trimmed");
        assert_eq!(first.id.len(), 8);
        // The same endpoint again: new keys and label, same row.
        let again = upsert(
            &dir,
            check(new_subscription("https://fcm.googleapis.com/fcm/send/abc", None)).unwrap(),
        )
        .unwrap();
        assert_eq!(again.id, first.id, "the same endpoint keeps its row and id");
        assert_eq!(again.label, "This device", "the label is replaced too");
        assert_eq!(load(&dir).unwrap().len(), 1);
        // A second device lands beside it, with the default label and its own row.
        let other = upsert(&dir, check(new_subscription("https://web.push.apple.com/3/x", None)).unwrap()).unwrap();
        assert_eq!(other.label, "This device");
        assert_eq!(load(&dir).unwrap(), vec![again, other.clone()]);
        // Deleting is by id, and says so when there is nothing to delete.
        assert!(delete(&dir, &first.id).unwrap());
        assert!(!delete(&dir, &first.id).unwrap());
        assert_eq!(load(&dir).unwrap(), vec![other]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn subscriptions_are_validated_before_they_are_stored() {
        let ok = check(new_subscription("https://fcm.googleapis.com/fcm/send/x", None)).unwrap();
        assert_eq!(ok.endpoint, "https://fcm.googleapis.com/fcm/send/x");
        assert_eq!(ok.label, "This device");
        let mut short = vec![0x04u8];
        short.resize(64, 1);
        let mut compressed = vec![0x03u8];
        compressed.resize(65, 1);
        let keys = |p256dh: String, auth: String| SubscriptionKeys { p256dh, auth };
        let at = "https://fcm.googleapis.com/x";
        for (why, bad) in [
            ("an http endpoint", new_subscription("http://fcm.googleapis.com/x", None)),
            ("not a URL at all", new_subscription("fcm.googleapis.com/x", None)),
            (
                "a p256dh that is not base64url",
                NewSubscription {
                    endpoint: at.into(),
                    keys: keys("not base64".into(), b64url(&[9u8; 16])),
                    label: None,
                },
            ),
            (
                "a p256dh that is not a point",
                NewSubscription {
                    endpoint: at.into(),
                    keys: keys(b64url(&short), b64url(&[9u8; 16])),
                    label: None,
                },
            ),
            (
                "a compressed p256dh",
                NewSubscription {
                    endpoint: at.into(),
                    keys: keys(b64url(&compressed), b64url(&[9u8; 16])),
                    label: None,
                },
            ),
            (
                "an auth secret of the wrong length",
                NewSubscription {
                    endpoint: at.into(),
                    keys: keys(b64url(&[0x04; 65]), b64url(&[9u8; 15])),
                    label: None,
                },
            ),
        ] {
            assert!(check(bad).is_err(), "{why} should be refused");
        }
        // Padding, which browsers are allowed to send, still decodes.
        let padded = NewSubscription {
            endpoint: at.into(),
            keys: keys(format!("{}=", b64url(&[0x04; 65])), format!("{}==", b64url(&[9u8; 16]))),
            label: None,
        };
        assert!(check(padded).is_ok());
        // A browser's PushSubscription.toJSON() may carry extras (expirationTime); they are ignored.
        let with_extras: NewSubscription = serde_json::from_value(json!({
            "endpoint": at,
            "expirationTime": None::<Value>,
            "keys": { "p256dh": b64url(&[0x04; 65]), "auth": b64url(&[9u8; 16]) },
        }))
        .unwrap();
        assert!(check(with_extras).is_ok());
    }
}
