//! The push module: Web Push notifications to the phones and desktops an operator actually
//! carries, the third channel beside the desktop popup and the webhook (issue #516).
//!
//! Two protocols do the work. RFC 8291 (`aes128gcm`) seals each message to the subscription's
//! key with a fresh ephemeral ECDH key, so the push service that relays it — a browser maker's,
//! not ours — carries bytes it cannot open. RFC 8292 (VAPID) signs every request with a key
//! generated on first use and kept beside the other secrets, so the push service knows the sender
//! and the browser knows who was allowed to wake it. Subscribing is the opt-in, and each
//! subscription's own preferences (push_prefs.rs, issue #743) decide what reaches that device;
//! removing a subscription in Settings ends the channel for that device alone.
//!
//! Like the other channels, what leaves is one short line about the colony and a link — never
//! question text, agent output, or any repository content. The one addition (issue #742): a
//! question the notification itself can answer also carries its option labels and a one-shot
//! answer token (answer_tokens.rs) — the labels, never the question or its header.

use crate::{
    ApiResult, App, AppError, Shared, client_error,
    phone::PhoneDevice,
    protocol::Origin,
    push_prefs::{self, MAX_OFFSET, Prefs, Presence, TZ_RULE, forget_presence, tz_ok},
    sessions::{Session, SessionStatus},
    util::{b64_decode, b64_encode, read_secret, short_id, truncate, write_private, write_secret},
};
use anyhow::{Result, anyhow};
use axum::{
    Extension, Json,
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
use std::{
    path::{Path as FsPath, PathBuf},
    sync::{LazyLock, Mutex},
    time::Duration,
};

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

/// Serialises the key's first use: two requests arriving together must settle on one key, or the
/// loser's browser subscribes with a public key the stored private key no longer matches. Never an await.
static VAPID_KEY: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// The VAPID signing key, generated on first use and kept like every other secret — base64 PKCS#8
/// through `write_secret`, so the system keychain when it is available and a 0600 file otherwise,
/// never in modules.json and never out through the API. One key signs for every device: it is this
/// mothership's identity, and the browser shows it as the subscription's origin permission.
pub fn signing_key(app: &App) -> Result<EcdsaKeyPair> {
    // Held across the check, the generate and the write: the read and the write are one decision.
    let _key = VAPID_KEY.lock().expect("the VAPID key lock");
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
    let (scheme, rest) = endpoint.split_once("://")?;
    // Production mints tokens for https origins only. In tests the http loopback stands in for a
    // push service end to end, certificate and all.
    let https_only = scheme == "https" || (cfg!(test) && scheme == "http" && rest.starts_with("127.0.0.1:"));
    if !https_only {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    (!authority.is_empty()).then(|| format!("{scheme}://{authority}"))
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
// The badge: how many colonies need a person right now
// ---------------------------------------------------------------------------

/// Whether this colony needs a person right now — the one definition the app badge counts
/// (issue #744), mirroring `needsYou` in web/src/notifications.ts; keep the two in step. A
/// failure nobody has looked at yet counts even though `failed` is terminal, so this check comes
/// before the terminal early-return.
pub fn needs_you(session: &Session) -> bool {
    if session.status == SessionStatus::Failed && session.unseen_failure {
        return true;
    }
    if session.status.is_terminal() {
        return false;
    }
    if session.status == SessionStatus::WaitingForAnswer {
        // One that answered while suspended (issue #667) is not waiting on a person any more —
        // its answer is stored and it is queued for a parallelism slot. Nothing here is left to
        // do.
        return !(session.suspended.is_some() && session.pending_answer.is_some());
    }
    let Some(attention) = &session.attention else {
        return false;
    };
    // A provider error on a colony that is still working is not yours to act on yet: its next
    // request may succeed (the gateway then lifts the flag). It needs you once the turn has
    // stopped.
    if attention["reason"].as_str() == Some("model_error")
        && matches!(session.status, SessionStatus::Running | SessionStatus::Starting)
    {
        return false;
    }
    // A colony parked for sitting idle (issue #1140) only waits to be resumed: nobody has to act.
    if attention["reason"].as_str() == Some(crate::idle_park::IDLE_PARK_REASON) {
        return false;
    }
    true
}

/// The app badge: how many rows Needs you has ([`needs_you`] per colony, less the noise
/// `needs_feed` removes: superseded and cascade failures, one entry per issue, old questions folded).
pub fn attention_count(sessions: &[Session]) -> usize {
    crate::needs_feed::rows(sessions, chrono::Utc::now())
}

/// [`attention_count`] with one colony left out — the one a resolution just closed (see
/// [`resolved`], which sends the count as the badge).
fn attention_count_without(sessions: &[Session], id: &str) -> usize {
    let rest: Vec<Session> = sessions.iter().filter(|s| s.id != id).cloned().collect();
    attention_count(&rest)
}

// ---------------------------------------------------------------------------
// The payload
// ---------------------------------------------------------------------------

/// The push payload: `text` is the same one line the desktop popup and the webhook carry — the
/// only content this function is handed, so repository content cannot travel through it — while
/// `session` names the colony for the deep link and the tag (one notification per colony, issue
/// #744). Only a question on a device that lets questions sound is loud. `badge` is the app badge
/// at send time, its key left out on `None`. Events with no colony have no `colony` key on the wire
/// and link to the front page.
pub fn payload(event: &str, text: &str, session: Option<&str>, prefs: &Prefs, badge: Option<usize>) -> Value {
    let (url, tag) = match session {
        Some(id) => (format!("/?colony={id}"), format!("colony-{id}")),
        None => ("/".to_string(), event.to_string()),
    };
    let mut payload = json!({
        "title": title(event),
        "body": one_line(text),
        "url": url,
        "tag": tag,
        "silent": push_prefs::silent(prefs, event),
    });
    if let Some(badge) = badge {
        payload["badge"] = json!(badge);
    }
    if let Some(id) = session {
        payload["colony"] = json!(id);
    }
    payload
}

/// The question push's payload: the five keys plus `answer` (issue #742) — the one-shot token that
/// `POST /api/push/answer` consumes and the option labels a notification can offer, in order. The
/// question's text and header are not parameters: a push never carries them, only the one line and
/// the labels. A question that cannot be answered from a notification (no minted token) still gets
/// the `answer` key, as `{"choices": []}`, so the service worker sees one question shape.
pub fn question_payload(
    text: &str,
    session: &str,
    answer: Option<(&str, &[String])>,
    prefs: &Prefs,
    badge: Option<usize>,
) -> Value {
    let mut value = payload("question", text, Some(session), prefs, badge);
    let answer = match answer {
        Some((token, labels)) => json!({ "token": token, "choices": labels }),
        None => json!({ "choices": [] }),
    };
    value
        .as_object_mut()
        .expect("payload is an object")
        .insert("answer".into(), answer);
    value
}

/// Where an out-of-quota push opens (issue #767): the Inbox, where the provider's card is.
pub const QUOTA_URL: &str = "/?view=inbox";

/// The out-of-quota push (issue #767): the five keys of [`payload`], linking to the Inbox that
/// shows the card and tagged per provider, so a second push about the same provider replaces the
/// first on the device instead of stacking. No `colony` key: the card is about many.
pub fn quota_payload(provider: &str, text: &str, prefs: &Prefs, badge: Option<usize>) -> Value {
    let mut value = payload(push_prefs::QUOTA, text, None, prefs, badge);
    value["url"] = json!(QUOTA_URL);
    value["tag"] = json!(format!("quota-{provider}"));
    value
}

/// The short title per event. A notification shade has room for one line of sense, not a sentence.
fn title(event: &str) -> &'static str {
    match event {
        "question" => "Colony asks a question",
        "attention" => "Colony stalled",
        "failed" => "Colony failed",
        "pull_request" => "Pull request opened",
        "provider_degraded" => "Provider degraded",
        push_prefs::QUOTA => "Provider out of quota",
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

/// How far the written `last_seen` may lag a heartbeat: pings come every half minute; the file need not move each time.
const LAST_SEEN_SAVE_SECS: i64 = 5 * 60;

/// Serialises every read-modify-write of the subscription file: a heartbeat, a settings save and a
/// prune can land together, and the last writer would otherwise resurrect stale state. Never an await.
static STORE: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

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
    /// The device's notification preferences; a file from before the field existed loads defaults.
    #[serde(default)]
    pub prefs: Prefs,
    /// When the device last pinged presence, unix seconds; absent until the first ping.
    #[serde(default)]
    pub last_seen: Option<i64>,
    /// The paired phone (phone.rs, issue #746) that subscribed this device, when a phone credential
    /// did: revoking that phone drops this subscription with it, and the phone may change this
    /// subscription's settings and no other device's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone: Option<String>,
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

pub(crate) fn save(config_dir: &FsPath, list: &[Subscription]) -> Result<()> {
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
    /// The paired phone subscribing, if a phone credential made the request; `check` leaves it unset.
    phone: Option<String>,
}

/// The stored form of a device label: trimmed, capped, defaulted — the same rule on create and PATCH.
fn label_of(label: Option<&str>) -> String {
    match label.map(str::trim) {
        None | Some("") => "This device".to_string(),
        Some(label) => truncate(label, MAX_LABEL),
    }
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
    let label = label_of(new.label.as_deref());
    Ok(Checked {
        endpoint,
        p256dh: new.keys.p256dh,
        auth: new.keys.auth,
        label,
        phone: None,
    })
}

/// Adds a subscription, or refreshes an endpoint's keys — a browser re-subscribing in place
/// rotates them and keeps the device's row, id and age.
fn upsert(config_dir: &FsPath, checked: Checked) -> Result<Subscription> {
    let _store = STORE.lock().expect("the subscription store lock");
    let mut list = load(config_dir)?;
    let entry = match list.iter_mut().find(|s| s.endpoint == checked.endpoint) {
        Some(existing) => {
            existing.p256dh = checked.p256dh;
            existing.auth = checked.auth;
            existing.label = checked.label;
            existing.phone = checked.phone;
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
                prefs: Prefs::default(),
                last_seen: None,
                phone: checked.phone,
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
    let _store = STORE.lock().expect("the subscription store lock");
    let mut list = load(config_dir)?;
    let before = list.len();
    list.retain(|s| s.id != id);
    if list.len() == before {
        return Ok(false);
    }
    save(config_dir, &list)?;
    forget_presence(id);
    Ok(true)
}

/// Removes every subscription a paired phone made (issue #746): revoking the phone ends its push
/// channel with its credential, so a lost phone stops receiving notifications — and answer
/// buttons — at once. Answers how many went.
pub(crate) fn drop_phone(config_dir: &FsPath, phone: &str) -> Result<usize> {
    let _store = STORE.lock().expect("the subscription store lock");
    let mut list = load(config_dir)?;
    let (gone, kept): (Vec<Subscription>, Vec<Subscription>) = list.drain(..).partition(|s| s.phone.as_deref() == Some(phone));
    if gone.is_empty() {
        return Ok(0);
    }
    save(config_dir, &kept)?;
    for subscription in &gone {
        forget_presence(&subscription.id);
    }
    Ok(gone.len())
}

/// A paired phone manages its own device's subscription and no other: its per-device settings
/// (issue #743) are its own to change, the owner's desktop's are not. The owner's credentials
/// reach every subscription.
fn may_manage(phone: Option<&Extension<PhoneDevice>>, subscription: &Subscription) -> Result<(), AppError> {
    match phone {
        Some(Extension(phone)) if subscription.phone.as_deref() != Some(phone.id.as_str()) => Err(client_error(
            StatusCode::FORBIDDEN,
            "a phone can change only its own notification settings",
        )),
        _ => Ok(()),
    }
}

/// What the API says about one subscription — never the endpoint URL (a capability) or the keys.
fn summary(subscription: &Subscription) -> Value {
    json!({
        "id": subscription.id,
        "label": subscription.label,
        "created_at": subscription.created_at,
        "endpoint_host": endpoint_host(&subscription.endpoint),
        "last_seen": subscription.last_seen,
        "prefs": subscription.prefs,
        "phone": subscription.phone,
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
pub async fn add_subscription(
    State(app): State<Shared>,
    phone: Option<Extension<PhoneDevice>>,
    Json(body): Json<NewSubscription>,
) -> ApiResult<Value> {
    let unlabelled = body.label.as_deref().is_none_or(|label| label.trim().is_empty());
    let mut checked = check(body).map_err(|m| client_error(StatusCode::BAD_REQUEST, &m))?;
    // A paired phone's subscription belongs to that phone (issue #746), and without a label of
    // its own it takes the phone's name from Settings → Add your phone.
    if let Some(Extension(phone)) = phone {
        if unlabelled && let Some(label) = app.phones.label(&phone.id) {
            checked.label = label_of(Some(&label));
        }
        checked.phone = Some(phone.id);
    }
    let entry = upsert(&app.cfg.config_dir, checked)?;
    Ok(Json(summary(&entry)))
}

/// `DELETE /api/push/subscriptions/{id}`: forgets one device.
pub async fn delete_subscription(
    State(app): State<Shared>,
    Path(id): Path<String>,
    phone: Option<Extension<PhoneDevice>>,
) -> Result<StatusCode, AppError> {
    if phone.is_some()
        && let Some(subscription) = load(&app.cfg.config_dir)?.iter().find(|s| s.id == id)
    {
        may_manage(phone.as_ref(), subscription)?;
    }
    if delete(&app.cfg.config_dir, &id)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(client_error(StatusCode::NOT_FOUND, "no such subscription"))
    }
}

/// What PATCH /api/push/subscriptions/{id} takes: a new label, new preferences, or both — `None` leaves a field as it is.
#[derive(Deserialize)]
pub struct PatchSubscription {
    label: Option<String>,
    prefs: Option<Prefs>,
}

/// `PATCH /api/push/subscriptions/{id}`: renames a device or edits its preferences; the label follows the create rules.
pub async fn patch_subscription(
    State(app): State<Shared>,
    Path(id): Path<String>,
    phone: Option<Extension<PhoneDevice>>,
    Json(body): Json<PatchSubscription>,
) -> ApiResult<Value> {
    if let Err(m) = body.prefs.as_ref().map(Prefs::validate).transpose() {
        return Err(client_error(StatusCode::BAD_REQUEST, &m));
    }
    let label = body.label.as_deref().map(|label| label_of(Some(label)));
    let _store = STORE.lock().expect("the subscription store lock");
    let mut list = load(&app.cfg.config_dir)?;
    let Some(entry) = list.iter_mut().find(|s| s.id == id) else {
        return Err(client_error(StatusCode::NOT_FOUND, "no such subscription"));
    };
    may_manage(phone.as_ref(), entry)?;
    if let Some(label) = label {
        entry.label = label;
    }
    if let Some(prefs) = body.prefs {
        entry.prefs = prefs;
    }
    let updated = entry.clone();
    save(&app.cfg.config_dir, &list)?;
    Ok(Json(summary(&updated)))
}

/// `POST /api/push/subscriptions/{id}/test`: one test push to that device alone, through the same
/// encryption and prune-on-Gone as a real event but past every preference — proof the pipe works.
pub async fn test_subscription(
    State(app): State<Shared>,
    Path(id): Path<String>,
    phone: Option<Extension<PhoneDevice>>,
) -> ApiResult<Value> {
    let list = load(&app.cfg.config_dir)?;
    let Some(subscription) = list.iter().find(|s| s.id == id) else {
        return Err(client_error(StatusCode::NOT_FOUND, "no such subscription"));
    };
    may_manage(phone.as_ref(), subscription)?;
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(15))
        .user_agent(concat!("colonizer/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| anyhow!("an HTTP client could not be built ({e})"))?;
    let key = signing_key(&app)?;
    // The "test" event has no title of its own, so the payload falls back to "Colonizer".
    let body = serde_json::to_vec(&payload("test", "Test notification", None, &Prefs::default(), None))?;
    let sent = send_one(&app, &client, &key, subscription, &body, "normal", None).await;
    Ok(Json(json!({ "sent": sent })))
}

#[derive(Deserialize)]
pub struct PresencePing {
    endpoint: String,
    colony: Option<String>,
    focused: bool,
    tz: Option<String>,
    utc_offset: Option<i32>,
}

/// `POST /api/push/presence`: the cockpit's heartbeat — what the tab is looking at, and where in the day it is.
pub async fn ping(State(app): State<Shared>, Json(body): Json<PresencePing>) -> Result<StatusCode, AppError> {
    if body.utc_offset.is_some_and(|offset| offset.abs() > MAX_OFFSET) {
        return Err(client_error(StatusCode::BAD_REQUEST, "the utc offset is out of range"));
    }
    let tz = body.tz.as_deref().map(str::trim).filter(|tz| !tz.is_empty());
    if tz.is_some_and(|tz| !tz_ok(tz)) {
        return Err(client_error(StatusCode::BAD_REQUEST, TZ_RULE));
    }
    if heartbeat(&app.cfg.config_dir, &body, tz, Utc::now().timestamp())? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(client_error(StatusCode::NOT_FOUND, "no such subscription"))
    }
}

/// One heartbeat, as one read-modify-write of the subscription file: find the device by endpoint,
/// record its last seen and any time-zone move, and write only when something changed or the record went stale.
fn heartbeat(config_dir: &FsPath, ping: &PresencePing, tz: Option<&str>, now: i64) -> Result<bool> {
    let _store = STORE.lock().expect("the subscription store lock");
    let mut list = load(config_dir)?;
    let Some(entry) = list.iter_mut().find(|s| s.endpoint == ping.endpoint) else {
        return Ok(false);
    };
    let stale = entry.last_seen.is_none_or(|seen| now - seen >= LAST_SEEN_SAVE_SECS);
    let mut changed = false;
    if let Some(tz) = tz.filter(|tz| entry.prefs.tz.as_deref() != Some(tz)) {
        entry.prefs.tz = Some(tz.to_string());
        changed = true;
    }
    if let Some(offset) = ping.utc_offset.filter(|offset| entry.prefs.utc_offset != *offset) {
        entry.prefs.utc_offset = offset;
        changed = true;
    }
    push_prefs::record_presence(
        &entry.id,
        Presence {
            colony: ping.colony.clone(),
            focused: ping.focused,
            at: now,
        },
    );
    entry.last_seen = Some(now);
    if changed || stale {
        save(config_dir, &list)?;
    }
    Ok(true)
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

/// Sends one already-built payload to one device and answers whether it took; a Gone reports and
/// prunes the device, and any failure is a line in the log, never a fault in the other sends.
async fn send_one(
    app: &App,
    client: &reqwest::Client,
    key: &EcdsaKeyPair,
    subscription: &Subscription,
    body: &[u8],
    urgency: &str,
    session: Option<&str>,
) -> bool {
    match send(client, key, subscription, body, urgency).await {
        Ok(()) => true,
        Err(NotSent::Gone(why)) => {
            let what = format!(
                "push: subscription {} ({}) dropped: {why}",
                subscription.label, subscription.id
            );
            report(app, session, what, "info").await;
            prune(app, std::slice::from_ref(&subscription.id));
            false
        }
        Err(NotSent::Failed(why)) => {
            report(app, session, format!("push: {} failed: {why}", subscription.label), "warn").await;
            false
        }
    }
}

/// The push channel of [`crate::notify`]'s `deliver`: one announcement to every subscribed device
/// its own preferences let through, behind the same ledger and event switches as the other channels.
pub async fn deliver(app: &App, client: &reqwest::Client, event: &str, text: &str, session: Option<&Session>) -> bool {
    let list = match load(&app.cfg.config_dir) {
        Ok(list) => list,
        Err(e) => {
            tracing::error!( error = %format!("{e:#}"), "push: the subscription list could not be read ({e:#}); nothing sent" );
            return false;
        }
    };
    if list.is_empty() {
        return false;
    }
    let key = match signing_key(app) {
        Ok(key) => key,
        Err(e) => {
            tracing::error!( error = %format!("{e:#}"), "push: the VAPID key is unavailable ({e:#}); nothing sent" );
            return false;
        }
    };
    let now = Utc::now().timestamp();
    let colony = session.map(|s| s.id.as_str());
    let recipients = recipients(&list, event, session, now);
    let badge = attention_count(&app.sessions.read().await);
    // The one push that can be answered in place (issue #742): mint a token once per deliver, and
    // only when at least one receiving device shows answer buttons (push_prefs::answer_actions).
    // A question that cannot be answered from a notification mints nothing.
    let wants_answer = recipients
        .iter()
        .any(|subscription| push_prefs::answer_actions(&subscription.prefs, event));
    let answer = match (event, colony) {
        ("question", Some(id)) if wants_answer => {
            // The paired phones this token reaches (issue #746): revoking one of them burns it.
            let phones: Vec<String> = recipients
                .iter()
                .filter(|subscription| push_prefs::answer_actions(&subscription.prefs, event))
                .filter_map(|subscription| subscription.phone.clone())
                .collect();
            app.answer_tokens.for_push(app, id, &phones).await
        }
        _ => None,
    };
    let answer = answer.as_ref().map(|(token, labels)| (token.as_str(), labels.as_slice()));
    // A question blocks a colony, so the phone buzzes like it matters; the rest can wait for the
    // device's own idea of a good moment.
    let urgency = if event == "question" { "high" } else { "normal" };
    let mut sent = false;
    for subscription in recipients {
        let Ok(body) = serde_json::to_vec(&device_payload(
            event,
            text,
            colony,
            &subscription.prefs,
            answer,
            push_prefs::badge(&subscription.prefs, badge),
        )) else {
            continue;
        };
        sent |= send_one(app, client, &key, subscription, &body, urgency, colony).await;
    }
    sent
}

/// The push channel for an out-of-quota card (issue #767): one push per provider, to every device
/// whose own preferences take the event for at least one of the card's colonies
/// ([`push_prefs::allows_any`]). Carries the one line notify built — provider name and reset time,
/// never anything a colony said.
pub async fn deliver_quota(app: &App, client: &reqwest::Client, provider: &str, text: &str, colonies: &[Session]) -> bool {
    let list = match load(&app.cfg.config_dir) {
        Ok(list) => list,
        Err(e) => {
            tracing::error!( error = %format!("{e:#}"), "push: the subscription list could not be read ({e:#}); nothing sent" );
            return false;
        }
    };
    if list.is_empty() {
        return false;
    }
    let key = match signing_key(app) {
        Ok(key) => key,
        Err(e) => {
            tracing::error!( error = %format!("{e:#}"), "push: the VAPID key is unavailable ({e:#}); nothing sent" );
            return false;
        }
    };
    let badge = attention_count(&app.sessions.read().await);
    let mut sent = false;
    for subscription in quota_recipients(&list, colonies, Utc::now().timestamp()) {
        let body = quota_payload(
            provider,
            text,
            &subscription.prefs,
            push_prefs::badge(&subscription.prefs, badge),
        );
        let Ok(body) = serde_json::to_vec(&body) else { continue };
        sent |= send_one(app, client, &key, subscription, &body, "normal", None).await;
    }
    sent
}

/// The devices an out-of-quota card reaches: [`recipients`] over several colonies at once.
fn quota_recipients<'a>(list: &'a [Subscription], colonies: &[Session], now: i64) -> Vec<&'a Subscription> {
    list.iter()
        .filter(|subscription| {
            let seen = push_prefs::presence_of(&subscription.id);
            push_prefs::allows_any(&subscription.prefs, push_prefs::QUOTA, colonies, now, seen.as_ref())
        })
        .collect()
}

/// The devices one event reaches: each subscription's own [`push_prefs::allows`] — its switch,
/// scope, quiet hours and presence — decides for that device alone.
fn recipients<'a>(list: &'a [Subscription], event: &str, session: Option<&Session>, now: i64) -> Vec<&'a Subscription> {
    list.iter()
        .filter(|subscription| {
            let seen = push_prefs::presence_of(&subscription.id);
            push_prefs::allows(&subscription.prefs, event, session, now, seen.as_ref())
        })
        .collect()
}

/// One device's payload: its prefs decide whether it sounds (push_prefs::silent) and, for a
/// question, whether it carries the answer buttons (push_prefs::answer_actions). A device with
/// the buttons off gets the question's empty answer, which opens the cockpit instead. `badge` is
/// already this device's own (push_prefs::badge): `None` leaves the key out.
fn device_payload(
    event: &str,
    text: &str,
    colony: Option<&str>,
    prefs: &Prefs,
    answer: Option<(&str, &[String])>,
    badge: Option<usize>,
) -> Value {
    match colony {
        Some(id) if event == "question" => question_payload(
            text,
            id,
            answer.filter(|_| push_prefs::answer_actions(prefs, event)),
            prefs,
            badge,
        ),
        _ => payload(event, text, colony, prefs, badge),
    }
}

/// Drops the gone subscriptions out of a list re-read now, not the one loaded before the sends: a
/// device that subscribed meanwhile must not be lost with them.
fn prune(app: &App, gone: &[String]) {
    let _store = STORE.lock().expect("the subscription store lock");
    match load(&app.cfg.config_dir) {
        Ok(mut remaining) => {
            remaining.retain(|s| !gone.contains(&s.id));
            if let Err(e) = save(&app.cfg.config_dir, &remaining) {
                tracing::error!( error = %format!("{e:#}"), "push: could not save the subscription list ({e:#})" );
            }
        }
        Err(e) => {
            tracing::warn!( error = %format!("{e:#}"), "push: could not re-read the subscription list to prune it ({e:#}); the gone ones stay" )
        }
    }
    for id in gone {
        forget_presence(id);
    }
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

/// The silent "resolved" push (issue #744): an answer or a look on one device means every other
/// device closes this colony's notification and sets its badge to the count sent here. Nothing
/// may be shown, so it runs outside notify's ledger — a resolution is the retraction of spam, not
/// spam to be cooled down. The badge is the attention with this colony left out: the colony still
/// reads as needing a person until its runner takes the question down, so leaving it out is the
/// count the receiver shows once the notification is closed.
pub async fn resolved(app: &App, session: &str) {
    // With the notify module off nothing was announced, so there is nothing to retract — and an
    // invisible push for nothing only spends the browser's silent-push budget.
    if !app.modules.read().await.notify.as_ref().is_some_and(|c| c.enabled) {
        return;
    }
    let list = match load(&app.cfg.config_dir) {
        Ok(list) => list,
        Err(e) => {
            tracing::error!( error = %format!("{e:#}"), "push: the subscription list could not be read ({e:#}); nothing sent" );
            return;
        }
    };
    if list.is_empty() {
        return;
    }
    let key = match signing_key(app) {
        Ok(key) => key,
        Err(e) => {
            tracing::error!( error = %format!("{e:#}"), "push: the VAPID key is unavailable ({e:#}); nothing sent" );
            return;
        }
    };
    // The badge and each device's scope both read the colony list, so it is read once.
    let (count, colony) = {
        let sessions = app.sessions.read().await;
        let colony = sessions.iter().find(|s| s.id == session).cloned();
        (attention_count_without(&sessions, session), colony)
    };
    // A device only hears about a colony its preferences could have announced; with the colony
    // gone there is nothing left to scope against, and nothing is sent.
    let Some(colony) = colony else {
        return;
    };
    let Some(client) = push_client() else {
        tracing::error!("push: could not build an HTTP client; the resolution was not sent");
        return;
    };
    for subscription in resolved_recipients(&list, &colony) {
        let Ok(body) = serde_json::to_vec(&resolved_payload(session, &subscription.prefs, count)) else {
            continue;
        };
        // The same report-and-prune `deliver` does: 404/410 is an unsubscription the browser
        // could not report.
        send_one(app, &client, &key, subscription, &body, "normal", Some(session)).await;
    }
}

/// The devices a colony's "resolved" push goes to: never an Apple endpoint — Safari/iOS revoke a
/// web push subscription that receives a push without a visible notification, and this one is
/// never visible, so that device's badge catches up on its next regular push instead — and only
/// the devices [`push_prefs::wants_resolved`] lets hear about this colony. Quiet hours and presence
/// deliberately do not apply: the push only clears what is already there.
fn resolved_recipients<'a>(list: &'a [Subscription], colony: &Session) -> Vec<&'a Subscription> {
    list.iter()
        .filter(|subscription| !is_apple(&subscription.endpoint))
        .filter(|subscription| push_prefs::wants_resolved(&subscription.prefs, colony))
        .collect()
}

/// The silent "resolved" payload for one device: the colony to close and, where the device keeps
/// the badge on ([`push_prefs::badge`]), the count to set it to.
fn resolved_payload(session: &str, prefs: &Prefs, count: usize) -> Value {
    let mut value = json!({ "type": "resolved", "colony": session });
    if let Some(badge) = push_prefs::badge(prefs, count) {
        value["badge"] = json!(badge);
    }
    value
}

/// Whether the endpoint is Apple's push service, which [`resolved`] must not wake (see the skip
/// above for why).
fn is_apple(endpoint: &str) -> bool {
    let host = endpoint_host(endpoint);
    host == "push.apple.com" || host.ends_with(".push.apple.com")
}

/// The HTTP client the resolved pushes go out with: built like notify's own, same timeouts and UA.
fn push_client() -> Option<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(15))
        .user_agent(concat!("colonizer/", env!("CARGO_PKG_VERSION")))
        .build()
        .ok()
}

/// Where a failed send's line goes: the colony's log when the event is about a colony, stderr when
/// it is not — the same rule notify's own channel failures follow. The session log takes the level
/// as a string; this arm maps the same two levels onto `tracing`.
async fn report(app: &App, session: Option<&str>, what: String, level: &str) {
    match session {
        Some(id) => app.session_log_as(Origin::Notify, id, level, what).await,
        None if level == "info" => tracing::info!("{what}"),
        None => tracing::warn!("{what}"),
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
        .route(
            "/api/push/subscriptions/{id}",
            routing::delete(delete_subscription).patch(patch_subscription),
        )
        .route("/api/push/subscriptions/{id}/test", routing::post(test_subscription))
        .route("/api/push/presence", routing::post(ping))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::notify::Event;
    use std::sync::{Arc, Barrier};

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

    /// Appendix A's message, sealed in both RFC checks.
    const WATERMELON: &[u8] = b"When I grow up, I want to be a watermelon";

    /// Appendix A sealed to the RFC's own keys — the shared stand-in for the two checks, one
    /// pinning `seal`'s bytes, the other reading them back through `unseal`. Returns the receiver
    /// side's three inputs and the body they open.
    fn rfc_8291_body() -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
        let ecdh_secret = un_b64url("kyrL1jIIOHEzg3sM2ZWRHDRB62YACZhhSlknJ672kSs").unwrap();
        let as_public =
            un_b64url("BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8").unwrap();
        let ua_public =
            un_b64url("BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4").unwrap();
        let auth = un_b64url("BTBZMqHH6r4Tts7J_aSIgg").unwrap();
        let salt = un_b64url("DGv6ra1nlYgDCS1FRnbzlw").unwrap();
        let body = seal(&ecdh_secret, &as_public, &ua_public, &auth, &salt, WATERMELON).unwrap();
        (ecdh_secret, ua_public, auth, body)
    }

    #[test]
    fn encryption_matches_the_rfc_8291_example_byte_for_byte() {
        // The exact body Section 5's example sends, pinning the whole scheme: the HKDF combination,
        // the 86-byte header, the 0x02 delimiter and the AEAD tag.
        let (_, _, _, body) = rfc_8291_body();
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
        assert_eq!(
            audience("http://127.0.0.1:42251/phone").as_deref(),
            Some("http://127.0.0.1:42251"),
            "the loopback stands in for a push service in tests"
        );
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
        let provider = payload(
            "provider_degraded",
            "zai is failing 29.4% of its requests",
            None,
            &Prefs::default(),
            Some(2),
        );
        assert_eq!(provider["url"], "/");
        assert_eq!(provider["tag"], "provider_degraded");
        assert_eq!(
            provider.as_object().unwrap().keys().map(String::as_str).collect::<Vec<_>>(),
            ["title", "body", "url", "tag", "silent", "badge"],
            "no colony key for an event with no colony"
        );
    }

    #[test]
    fn the_push_payload_names_the_colony_and_the_badge_and_no_session_content() {
        let session = colony();
        let body = serde_json::to_string(&payload(
            "failed",
            "acme/webshop #42 failed",
            Some(&session.id),
            &Prefs::default(),
            Some(3),
        ))
        .unwrap();
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
            ["badge", "body", "colony", "silent", "tag", "title", "url"],
            "one shape for the service worker"
        );
        assert_eq!(value["silent"], true, "only a question sounds");
        assert_eq!(value["title"], "Colony failed");
        assert_eq!(value["body"], "acme/webshop #42 failed");
        assert_eq!(value["url"], "/?colony=abc123");
        // One notification per colony (issue #744): the tag names the colony alone, so the next
        // event for it replaces this one in place instead of stacking.
        assert_eq!(value["tag"], "colony-abc123");
        assert_eq!(value["colony"], "abc123");
        assert_eq!(value["badge"], 3);
    }

    /// The question push is the one payload with an `answer` key (issue #742): a token and the option
    /// labels — labels only, never the question text or header, so nothing of the ask itself leaves
    /// the install.
    #[test]
    fn a_question_push_carries_a_token_and_labels_but_never_the_question() {
        let session = colony();
        let labels = vec!["Push now".to_string(), "Wait".to_string()];
        let text = Event::Question.text(&session.repo, session.issue);
        let body = serde_json::to_string(&question_payload(
            &text,
            &session.id,
            Some(("tok123", &labels)),
            &Prefs::default(),
            Some(1),
        ))
        .unwrap();
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
            ["answer", "badge", "body", "colony", "silent", "tag", "title", "url"],
            "the question shape: the colony shape plus answer"
        );
        assert_eq!(value["title"], "Colony asks a question");
        assert_eq!(value["body"], "acme/webshop #42 needs an answer");
        assert_eq!(value["url"], "/?colony=abc123");
        assert_eq!(value["tag"], "colony-abc123");
        assert_eq!(value["silent"], false, "a sounding question is the one loud payload");
        assert_eq!(value["answer"], json!({"token": "tok123", "choices": ["Push now", "Wait"]}));
    }

    /// A question that cannot be answered from a notification pushes the empty answer: the same
    /// keys, no token in it.
    #[test]
    fn an_unanswerable_question_push_carries_an_empty_answer_and_no_token() {
        let value = question_payload("acme/webshop #42 needs an answer", "abc123", None, &Prefs::default(), Some(1));
        assert_eq!(value["answer"], json!({"choices": []}));
        assert!(value["answer"].get("token").is_none(), "no token, no credential");
    }

    /// A device with answer buttons off gets the question's empty answer — no token, no labels —
    /// while a device with them on gets both; other events never carry an answer.
    #[test]
    fn answer_buttons_follow_each_devices_answer_actions() {
        let labels = vec!["Push now".to_string(), "Wait".to_string()];
        let answer = Some(("tok123", labels.as_slice()));
        let text = "acme/webshop #42 needs an answer";
        let on = device_payload("question", text, Some("abc123"), &Prefs::default(), answer, Some(1));
        assert_eq!(on["answer"], json!({"token": "tok123", "choices": ["Push now", "Wait"]}));
        let off_prefs = Prefs {
            answer_actions: false,
            ..Prefs::default()
        };
        let off = device_payload("question", text, Some("abc123"), &off_prefs, answer, Some(1));
        assert_eq!(off["answer"], json!({"choices": []}));
        let failed = device_payload(
            "failed",
            "acme/webshop #42 failed",
            Some("abc123"),
            &Prefs::default(),
            answer,
            Some(1),
        );
        assert!(failed.get("answer").is_none());
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

    /// Every concurrent first subscribe lands: `upsert` is a read-modify-write of one JSON file, so
    /// without the store lock the last writer would drop the rows the others added.
    #[test]
    fn concurrent_subscriptions_all_persist() {
        let dir = std::env::temp_dir().join(format!("colonizer-push-{}", short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let start = Barrier::new(16);
        std::thread::scope(|scope| {
            for i in 0..16 {
                let endpoint = format!("https://fcm.googleapis.com/fcm/send/{i}");
                let dir = &dir;
                let start = &start;
                scope.spawn(move || {
                    start.wait();
                    upsert(dir, check(new_subscription(&endpoint, None)).unwrap()).unwrap();
                });
            }
        });
        assert_eq!(load(&dir).unwrap().len(), 16, "a subscribe was lost");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every concurrent first key request settles on one key: without the lock two callers could
    /// each generate and store one, leaving a browser subscribed with a public key the stored
    /// private key no longer matches.
    #[test]
    fn concurrent_first_key_calls_return_one_key() {
        let root = std::env::temp_dir().join(format!("colonizer-push-{}", short_id()));
        let app = crate::tests::test_app(&root);
        let start = Barrier::new(16);
        let keys: Vec<String> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..16)
                .map(|_| {
                    let app = &app;
                    let start = &start;
                    scope.spawn(move || {
                        start.wait();
                        b64url(signing_key(app).unwrap().public_key().as_ref())
                    })
                })
                .collect();
            handles.into_iter().map(|handle| handle.join().unwrap()).collect()
        });
        assert!(
            keys.iter().all(|key| *key == keys[0]),
            "one key for every first caller: {keys:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
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

    #[test]
    fn a_file_from_before_the_prefs_existed_loads_with_defaults() {
        let old: Subscription = serde_json::from_value(json!({
            "id": "dev1", "endpoint": "https://fcm.googleapis.com/x", "p256dh": "k", "auth": "a", "label": "Pixel", "created_at": 1_789_000_000,
        }))
        .unwrap();
        assert_eq!(old.prefs, Prefs::default(), "no preferences means the defaults");
        assert_eq!(old.last_seen, None);
        // And the full new shape round-trips.
        let sub: Subscription = serde_json::from_value(json!({
            "id": "dev1", "endpoint": "https://fcm.googleapis.com/x", "p256dh": "k", "auth": "a", "label": "Pixel", "created_at": 1_789_000_000,
            "prefs": { "quiet": { "start": 1320, "end": 420 }, "scope": ["acme"] },
            "last_seen": 1_789_000_000,
        }))
        .unwrap();
        let back: Subscription = serde_json::from_str(&serde_json::to_string(&sub).unwrap()).unwrap();
        assert_eq!(back, sub);
    }

    // -- the badge (issue #744) -----------------------------------------------------------------

    /// The fixture the cockpit pins its own `needsYou` against, run through [`needs_you`]: one
    /// definition of "needs a person", two implementations, no drift.
    #[test]
    fn needs_you_matches_the_fixture_the_cockpit_pins() {
        let cases: Value = serde_json::from_str(include_str!("../tests/fixtures/needs_you.json")).unwrap();
        let base = serde_json::to_value(colony()).unwrap();
        for case in cases.as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let mut merged = base.clone();
            merged
                .as_object_mut()
                .unwrap()
                .extend(case["session"].as_object().unwrap().clone());
            let session: Session = serde_json::from_value(merged).unwrap();
            assert_eq!(needs_you(&session), case["needs_you"].as_bool().unwrap(), "case {name}");
        }
    }

    /// The badge is a count over the whole list, and a resolution reads its colony out of it.
    #[test]
    fn the_badge_counts_the_colonies_that_need_a_person_and_resolved_leaves_its_own_out() {
        let with = |id: &str, status: SessionStatus, unseen: bool| {
            let mut s = crate::sessions::tests::colony("acme", status);
            s.id = id.into();
            s.unseen_failure = unseen;
            s
        };
        let waiting = with("waiting", SessionStatus::WaitingForAnswer, false);
        let failed = with("failed", SessionStatus::Failed, true);
        let seen = with("seen", SessionStatus::Failed, false);
        let idle = with("idle", SessionStatus::Idle, false);
        // Waiting on a question and one unseen failure; the seen failure and the idle do not count.
        assert_eq!(attention_count(&[waiting.clone(), failed, seen, idle]), 2);
        let other = with("other", SessionStatus::WaitingForAnswer, false);
        assert_eq!(attention_count_without(&[waiting.clone(), other], "waiting"), 1);
        assert_eq!(attention_count_without(&[waiting], "waiting"), 0, "the colony itself");
    }

    // -- the receiver's side, test-only ---------------------------------------------------------

    /// One fake device: the subscription as `save` stores it, plus the private half of its key and
    /// the auth secret needed to decrypt what arrives. Ring never hands out private key bytes, so
    /// the `EphemeralPrivateKey` itself is kept — good for exactly the one decryption a capture gets.
    pub(crate) struct Device {
        pub(crate) subscription: Subscription,
        private: Option<agreement::EphemeralPrivateKey>,
        auth: [u8; 16],
    }

    pub(crate) fn device(endpoint: &str, auth: [u8; 16]) -> Device {
        let private = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &SystemRandom::new()).unwrap();
        let subscription = Subscription {
            id: short_id(),
            endpoint: endpoint.into(),
            p256dh: b64url(private.compute_public_key().unwrap().as_ref()),
            auth: b64url(&auth),
            label: "test device".into(),
            created_at: Utc::now().timestamp(),
            prefs: Prefs::default(),
            last_seen: None,
            phone: None,
        };
        Device {
            subscription,
            private: Some(private),
            auth,
        }
    }

    /// A device's public key bytes, as the sender encrypted to them.
    pub(crate) fn ua_public(device: &Device) -> Vec<u8> {
        un_b64url(&device.subscription.p256dh).unwrap()
    }

    /// The receiver's ECDH: the shared secret with a sender's ephemeral point, under the
    /// subscription's private half.
    pub(crate) fn agree(device: &mut Device, as_public: &[u8]) -> Vec<u8> {
        agreement::agree_ephemeral(
            device.private.take().unwrap(),
            &agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, as_public),
            |secret| secret.to_vec(),
        )
        .unwrap()
    }

    /// The RFC 8291 receiver, the inverse of [`seal`]: the key schedule run back out of the body's
    /// own header, one AES-GCM open, the 0x02 delimiter stripped. Pinned against the RFC's worked
    /// example, and the end-to-end test reads its captures through it.
    pub(crate) fn unseal(ecdh_secret: &[u8], ua_public: &[u8], auth_secret: &[u8], body: &[u8]) -> Vec<u8> {
        // The header: salt(16) || rs(4) || keyid length(1) || keyid, the sender's ephemeral point.
        let salt = &body[..16];
        let keyid_len = body[20] as usize;
        let as_public = &body[21..21 + keyid_len];
        let mut info = b"WebPush: info\0".to_vec();
        info.extend_from_slice(ua_public);
        info.extend_from_slice(as_public);
        let mut ikm = [0u8; 32];
        hkdf::Salt::new(hkdf::HKDF_SHA256, auth_secret)
            .extract(ecdh_secret)
            .expand(&[&info], hkdf::HKDF_SHA256)
            .unwrap()
            .fill(&mut ikm)
            .unwrap();
        let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(&ikm);
        // ring's Okm::fill wants the full SHA-256 block, so both keys derive into 32 bytes and the
        // AES key and nonce take their prefixes.
        let okm = |info: &[&[u8]]| {
            let mut out = [0u8; 32];
            prk.expand(info, hkdf::HKDF_SHA256).unwrap().fill(&mut out).unwrap();
            out
        };
        let cek = okm(&[b"Content-Encoding: aes128gcm\0"]);
        let nonce = okm(&[b"Content-Encoding: nonce\0"]);
        let mut record = body[21 + keyid_len..].to_vec();
        let key = LessSafeKey::new(UnboundKey::new(&AES_128_GCM, &cek[..16]).unwrap());
        let plaintext = key
            .open_in_place(
                Nonce::try_assume_unique_for_key(&nonce[..12]).unwrap(),
                Aad::empty(),
                &mut record,
            )
            .unwrap();
        let end = plaintext.iter().rposition(|&b| b == 0x02).unwrap();
        plaintext[..end].to_vec()
    }

    #[test]
    fn unseal_decodes_the_rfc_8291_example() {
        // The same worked example `encryption_matches_the_rfc_8291_example_byte_for_byte` pins.
        let (ecdh_secret, ua_public, auth, body) = rfc_8291_body();
        assert_eq!(
            unseal(&ecdh_secret, &ua_public, &auth, &body),
            WATERMELON,
            "the receiver's side reads seal's work back"
        );
    }

    /// Switches the notify module on, as an operator would: `resolved` retracts only what notify
    /// could have announced.
    pub(crate) async fn notify_on(app: &App) {
        app.modules.write().await.notify = Some(crate::config::ModuleChoice {
            provider: "default".into(),
            enabled: true,
            settings: Default::default(),
        });
    }

    /// What a capture server records per POST: the path POSTed, the headers and the encrypted body.
    pub(crate) type Captures = Arc<std::sync::Mutex<Vec<(String, axum::http::HeaderMap, Vec<u8>)>>>;

    /// A push service that records what it is handed, serving any path, one capture per POST.
    pub(crate) async fn capture_server(captures: Captures) -> std::net::SocketAddr {
        let router = axum::Router::new().route(
            "/{device}",
            axum::routing::post(
                move |axum::extract::Path(device): axum::extract::Path<String>,
                      headers: axum::http::HeaderMap,
                      body: axum::body::Bytes| {
                    let captures = captures.clone();
                    async move {
                        captures.lock().unwrap().push((device, headers, body.to_vec()));
                        axum::http::StatusCode::CREATED
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        addr
    }

    /// Waits, bounded, for `want` captures.
    pub(crate) async fn await_captures(captures: &Captures, want: usize) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while captures.lock().unwrap().len() < want {
            assert!(
                std::time::Instant::now() < deadline,
                "only {} of {want} pushes arrived",
                captures.lock().unwrap().len()
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// The whole path, end to end (issue #744): a colony waiting on a question, two subscribed
    /// devices and an Apple watch, the real answer handler. Both web devices decrypt the same
    /// `{"type": "resolved", "colony", "badge"}`, urgency normal, with the badge already past the
    /// colony that just resolved; the Apple endpoint is neither woken nor pruned.
    #[tokio::test]
    async fn answering_a_question_resolves_it_on_every_subscribed_device() {
        let captures: Captures = Arc::default();
        let addr = capture_server(captures.clone()).await;
        let (app, root) = crate::sessions::tests::app_with_colony("abc", SessionStatus::WaitingForAnswer).await;
        let rt = app.runtime("abc").await;
        *rt.open_question.lock().await = Some(("q1".into(), Vec::new(), crate::protocol::QuestionRisk::ReadOnly));
        let mut laptop = device(&format!("http://{addr}/laptop"), [7u8; 16]);
        let mut phone = device(&format!("http://{addr}/phone"), [8u8; 16]);
        let watch = Subscription {
            id: short_id(),
            endpoint: "https://api.push.apple.com/3/device/abc".into(),
            p256dh: b64url(&[0x04; 65]),
            auth: b64url(&[9u8; 16]),
            label: "watch".into(),
            created_at: Utc::now().timestamp(),
            prefs: Prefs::default(),
            last_seen: None,
            phone: None,
        };
        notify_on(&app).await;
        std::fs::create_dir_all(&app.cfg.config_dir).unwrap();
        save(
            &app.cfg.config_dir,
            &[laptop.subscription.clone(), phone.subscription.clone(), watch],
        )
        .unwrap();

        // The colony is waiting on the person, so the badge it resolves from is 1 — and the push
        // must carry the count after the resolution, 0, not the stale one.
        assert_eq!(attention_count(&app.sessions.read().await), 1);
        let response = crate::sessions::answer(
            State(app.clone()),
            Path("abc".into()),
            None,
            Json(json!({"question_id": "q1", "answers": {}, "response": null})),
        )
        .await
        .unwrap();
        assert_eq!(response, StatusCode::NO_CONTENT);
        await_captures(&captures, 2).await;

        for name in ["laptop", "phone"] {
            let (path, headers, body) = {
                let captures = captures.lock().unwrap();
                captures.iter().find(|(p, ..)| p == name).unwrap().clone()
            };
            assert_eq!(path, name);
            assert_eq!(headers.get("Urgency").unwrap(), "normal", "{name}");
            let device = if name == "laptop" { &mut laptop } else { &mut phone };
            let ua_public = un_b64url(&device.subscription.p256dh).unwrap();
            let sender = &body[21..21 + body[20] as usize]; // the header's keyid, the sender's point
            let plaintext = unseal(&agree(device, sender), &ua_public, &device.auth, &body);
            let payload: Value = serde_json::from_slice(&plaintext).unwrap();
            assert_eq!(payload, json!({"type": "resolved", "colony": "abc", "badge": 0}), "{name}");
        }
        // The Apple endpoint stayed asleep and stayed stored: see the skip in `resolved`.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(captures.lock().unwrap().len(), 2, "the Apple endpoint was woken");
        assert_eq!(
            load(&app.cfg.config_dir).unwrap().len(),
            3,
            "the Apple subscription was pruned"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// With the notify module off nothing was announced, so a resolution has nothing to retract
    /// and wakes no device.
    #[tokio::test]
    async fn resolved_stays_silent_while_notify_is_off() {
        let captures: Captures = Arc::default();
        let addr = capture_server(captures.clone()).await;
        let (app, root) = crate::sessions::tests::app_with_colony("abc", SessionStatus::Failed).await;
        let phone = device(&format!("http://{addr}/phone"), [8u8; 16]);
        std::fs::create_dir_all(&app.cfg.config_dir).unwrap();
        save(&app.cfg.config_dir, &[phone.subscription]).unwrap();
        resolved(&app, "abc").await;
        assert!(captures.lock().unwrap().is_empty(), "notify is off: nothing to retract");
        notify_on(&app).await;
        resolved(&app, "abc").await;
        assert_eq!(captures.lock().unwrap().len(), 1, "notify on: the device hears it");
        let _ = std::fs::remove_dir_all(root);
    }

    // -- the three together: #743's preferences shaping #742's buttons and #744's badge/resolved ----

    /// One question, then its resolution, across five devices with their own preferences: answer
    /// buttons only where `answer_actions` allows, quiet hours holding the question back unless
    /// questions break through while the resolution still sets the badge where the badge is on, and
    /// the resolution skipped wherever `wants_resolved` says the device never heard of the colony.
    #[test]
    fn preferences_shape_answer_buttons_quiet_hours_badge_and_resolved_per_device() {
        use crate::push_prefs::QuietHours;
        let colony = colony();
        let sub = |label: &str, f: fn(&mut Prefs)| {
            let mut subscription = device(&format!("https://fcm.googleapis.com/{label}"), [1u8; 16]).subscription;
            subscription.label = label.into();
            f(&mut subscription.prefs);
            subscription
        };
        // 03:00 UTC; every quiet device sleeps 22:00–07:00 at UTC+0.
        let now = 1_767_236_400;
        const NIGHT: Option<QuietHours> = Some(QuietHours {
            start: 22 * 60,
            end: 7 * 60,
        });
        let list = [
            sub("buttons", |_| {}),
            sub("no-buttons", |p| p.answer_actions = false),
            sub("asleep", |p| p.quiet = NIGHT),
            sub("asleep-no-badge", |p| {
                p.quiet = NIGHT;
                p.badge = false;
            }),
            sub("asleep-breakthrough", |p| {
                p.quiet = NIGHT;
                p.questions_break_quiet = true;
            }),
            sub("other-org", |p| p.scope = vec!["globex".into()]),
        ];
        let labels = vec!["Push now".to_string(), "Wait".to_string()];
        let answer = Some(("tok123", labels.as_slice()));
        let text = "acme/webshop #42 needs an answer";

        // The question push: quiet hours hold back the sleeping devices, except the one that lets
        // questions break through, and scope holds back the other org.
        let asked = recipients(&list, "question", Some(&colony), now);
        let names: Vec<&str> = asked.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(names, ["buttons", "no-buttons", "asleep-breakthrough"]);
        let pushed: Vec<Value> = asked
            .iter()
            .map(|s| {
                device_payload(
                    "question",
                    text,
                    Some(&colony.id),
                    &s.prefs,
                    answer,
                    push_prefs::badge(&s.prefs, 2),
                )
            })
            .collect();
        // Buttons on: the labels with the token. Buttons off: the same notification, its labels
        // (title and body) intact, but an empty answer — no token, so no action buttons.
        assert_eq!(
            pushed[0]["answer"],
            json!({"token": "tok123", "choices": ["Push now", "Wait"]})
        );
        assert_eq!(pushed[1]["answer"], json!({"choices": []}));
        assert_eq!(pushed[1]["title"], "Colony asks a question");
        assert_eq!(pushed[1]["body"], text);
        assert_eq!(pushed[2]["answer"], pushed[0]["answer"]);
        for payload in &pushed {
            assert_eq!(payload["badge"], 2);
        }

        // The resolution: quiet hours do not apply, so the sleeping devices' badges catch up —
        // but only where the badge is on; the other org never heard of the colony, so it hears
        // nothing now either.
        let resolved: Vec<(&str, Value)> = resolved_recipients(&list, &colony)
            .into_iter()
            .map(|s| (s.label.as_str(), resolved_payload(&colony.id, &s.prefs, 1)))
            .collect();
        let names: Vec<&str> = resolved.iter().map(|(name, _)| *name).collect();
        assert_eq!(
            names,
            ["buttons", "no-buttons", "asleep", "asleep-no-badge", "asleep-breakthrough"]
        );
        let of = |name: &str| resolved.iter().find(|(n, _)| *n == name).unwrap().1.clone();
        assert_eq!(of("asleep"), json!({"type": "resolved", "colony": "abc123", "badge": 1}));
        assert_eq!(
            of("asleep-no-badge"),
            json!({"type": "resolved", "colony": "abc123"}),
            "badge off: no badge key"
        );

        // A device with every colony event off could never have been told about the colony, so
        // wants_resolved is false and the resolution skips it, like an Apple endpoint.
        let mut muted = sub("muted", |_| {});
        for (name, _) in crate::push_prefs::EVENTS {
            muted.prefs.events.insert((*name).to_string(), false);
        }
        let mut apple = sub("apple", |_| {});
        apple.endpoint = "https://web.push.apple.com/abc".into();
        assert!(!push_prefs::wants_resolved(&muted.prefs, &colony));
        assert!(resolved_recipients(&[muted, apple], &colony).is_empty());
    }
}
