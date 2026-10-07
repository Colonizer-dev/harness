//! The secret-canary kit: one fake credential per redactor detector kind, made fresh at run time
//! (split literals, random bodies — no fake secret ever sits in the source), and
//! [`assert_absent`], which looks for them in exported bytes in every form a leak could take.

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE};
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::Value;
use std::io::Read;

/// One canary: the text it hides in, the parts that must never be exported, and the
/// `[REDACTED:<kind>]` kinds redaction must leave in their place.
#[derive(Clone, Debug)]
pub(crate) struct Canary {
    pub name: &'static str,
    pub text: String,
    pub secrets: Vec<String>,
    pub marks: Vec<&'static str>,
}

/// Every canary, and a content canary: not a secret, a marker for text that only the content tier
/// may carry.
#[derive(Clone, Debug)]
pub(crate) struct Canaries {
    pub all: Vec<Canary>,
    pub content: String,
}

const ALNUM: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
const UPPER_DIGIT: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

fn random_bytes(n: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; n];
    SystemRandom::new().fill(&mut bytes).expect("system randomness");
    bytes
}

/// `n` distinct characters of `alphabet` in random order, with at least one upper-case letter, one
/// digit and (when the alphabet has them) one lower-case letter: distinct characters keep the
/// entropy detector's verdict certain, the mix keeps every detector's character-class rules met.
fn token_from(alphabet: &[u8], n: usize) -> String {
    assert!(n <= alphabet.len());
    let wants_lower = alphabet.iter().any(u8::is_ascii_lowercase);
    loop {
        let mut chars = alphabet.to_vec();
        let random = random_bytes(chars.len() * 2);
        for i in (1..chars.len()).rev() {
            let j = usize::from(u16::from_le_bytes([random[2 * i], random[2 * i + 1]])) % (i + 1);
            chars.swap(i, j);
        }
        chars.truncate(n);
        let ok = chars.iter().any(u8::is_ascii_uppercase)
            && chars.iter().any(u8::is_ascii_digit)
            && (!wants_lower || chars.iter().any(u8::is_ascii_lowercase));
        if ok {
            return String::from_utf8(chars).expect("ASCII");
        }
    }
}

fn token(n: usize) -> String {
    token_from(ALNUM, n)
}

/// A short random hex id for a test's temp directory.
pub(crate) fn unique() -> String {
    hex(&random_bytes(6))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl Canaries {
    pub(crate) fn new() -> Canaries {
        let canary = |name, text: String, secrets: Vec<String>, marks: Vec<&'static str>| Canary {
            name,
            text,
            secrets,
            marks,
        };
        let github = format!("{}_{}", "ghp", token(36));
        let anthropic = format!("{}-{}-api03-{}", "sk", "ant", token(40));
        let openai = format!("{}-{}-{}", "sk", "proj", token(40));
        let aws_id = format!("{}{}{}", "AK", "IA", token_from(UPPER_DIGIT, 16));
        let aws_secret = token(40);
        let stripe = format!("{}_{}_{}", "sk", "live", token(24));
        let digits: String = random_bytes(12).iter().map(|b| char::from(b'0' + b % 10)).collect();
        let slack = format!("{}-{digits}-{}", "xoxb", token(24));
        let (pem_a, pem_b) = (token(48), token(44));
        let pem = format!(
            "-----{} RSA {} KEY-----\n{pem_a}\n{pem_b}\n-----{} RSA {} KEY-----",
            "BEGIN", "PRIVATE", "END", "PRIVATE"
        );
        let (jwt_payload, jwt_sig) = (format!("eyJ{}", token(30)), token(43));
        let jwt = format!("eyJ{}.{jwt_payload}.{jwt_sig}", token(20));
        let bearer = token(32);
        let basic = token(24);
        let uri_pw = token(16);
        let pg_pw = token(16);
        let password = token(20);
        let entropy = token(40);
        let (header_key, header_bearer) = (token(32), token(32));
        let all = vec![
            canary(
                "github",
                format!("git push with {github} now"),
                vec![github],
                vec!["github_token"],
            ),
            canary(
                "anthropic",
                format!("key {anthropic} in env"),
                vec![anthropic],
                vec!["anthropic_key"],
            ),
            canary("openai", format!("OPENAI {openai} set"), vec![openai], vec!["openai_key"]),
            canary(
                "aws",
                format!("aws {aws_id} {aws_secret} creds"),
                vec![aws_id, aws_secret],
                vec!["aws_access_key", "aws_secret_key"],
            ),
            canary("stripe", format!("stripe {stripe} live"), vec![stripe], vec!["stripe_key"]),
            canary("slack", format!("slack {slack} bot"), vec![slack], vec!["slack_token"]),
            canary(
                "pem",
                format!("key file:\n{pem}\nend"),
                vec![pem_a, pem_b],
                vec!["private_key"],
            ),
            canary("jwt", format!("session {jwt} ok"), vec![jwt_payload, jwt_sig], vec!["jwt"]),
            canary(
                "bearer",
                format!("Authorization: Bearer {bearer}"),
                vec![bearer],
                vec!["bearer_token"],
            ),
            canary(
                "basic",
                format!("Authorization: Basic {basic}"),
                vec![basic],
                vec!["basic_auth"],
            ),
            canary(
                "uri",
                format!("clone https://nick:{uri_pw}@example.com/repo.git"),
                vec![uri_pw],
                vec!["uri_credentials"],
            ),
            canary(
                "postgres",
                format!("DATABASE_URL is postgres://app:{pg_pw}@db:5432/app"),
                vec![pg_pw],
                vec!["connection_string"],
            ),
            canary(
                "password",
                format!("DB_PASSWORD={password}"),
                vec![password],
                vec!["password"],
            ),
            canary(
                "entropy",
                format!("opaque {entropy} here"),
                vec![entropy],
                vec!["high_entropy"],
            ),
            // The `observability-headers` secret's own value, as OTEL_EXPORTER_OTLP_HEADERS spells it.
            canary(
                "otlp_headers",
                format!("x-api-key={header_key},authorization=Bearer {header_bearer}"),
                vec![header_key, header_bearer],
                vec![],
            ),
        ];
        Canaries {
            all,
            content: format!("colonizer-content-canary-{}", hex(&random_bytes(8))),
        }
    }

    /// Every secret of every canary.
    pub(crate) fn secrets(&self) -> impl Iterator<Item = &str> {
        self.all.iter().flat_map(|c| c.secrets.iter().map(String::as_str))
    }
}

/// The forms a leaked `secret` could take in exported bytes: itself, percent-encoded, and base64
/// (standard and URL-safe) at each of the three alignments it could start at inside a longer
/// encoded run — the characters that depend on the secret's bytes alone.
fn needles(secret: &str) -> Vec<String> {
    let mut out = vec![secret.to_string()];
    let percent: String = secret
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => char::from(b).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    if percent != secret {
        out.push(percent);
    }
    for shift in 0..3 {
        let mut bytes = vec![0u8; shift];
        bytes.extend_from_slice(secret.as_bytes());
        for engine in [&STANDARD, &URL_SAFE] {
            let encoded = engine.encode(&bytes);
            let start = if shift == 0 { 0 } else { 4 };
            if encoded.len() > start + 4 + 8 {
                out.push(encoded[start..encoded.len() - 4].to_string());
            }
        }
    }
    out
}

/// `bytes` ungzipped when they are gzip.
fn gunzip(bytes: &[u8]) -> Vec<u8> {
    if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(bytes).read_to_end(&mut out).expect("valid gzip");
        out
    } else {
        bytes.to_vec()
    }
}

/// Every string in an exported request (gzip, OTLP/JSON or OTLP protobuf of any signal), keys
/// included, plus the raw bytes read as text.
pub(crate) fn strings(bytes: &[u8]) -> Vec<String> {
    use crate::proto::collector::{logs::v1 as logs, metrics::v1 as metrics, trace::v1 as trace};
    use prost::Message;
    let bytes = gunzip(bytes);
    let mut out = vec![String::from_utf8_lossy(&bytes).into_owned()];
    let mut decoded = Vec::new();
    if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
        decoded.push(value);
    } else {
        if let Ok(r) = logs::ExportLogsServiceRequest::decode(bytes.as_slice()) {
            decoded.push(serde_json::to_value(r).expect("serializes"));
        }
        if let Ok(r) = trace::ExportTraceServiceRequest::decode(bytes.as_slice()) {
            decoded.push(serde_json::to_value(r).expect("serializes"));
        }
        if let Ok(r) = metrics::ExportMetricsServiceRequest::decode(bytes.as_slice()) {
            decoded.push(serde_json::to_value(r).expect("serializes"));
        }
        assert!(!decoded.is_empty(), "bytes are neither OTLP/JSON nor OTLP protobuf");
    }
    for value in &decoded {
        collect(value, &mut out);
    }
    out
}

fn collect(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) => out.push(s.clone()),
        Value::Array(items) => items.iter().for_each(|v| collect(v, out)),
        Value::Object(map) => {
            for (k, v) in map {
                out.push(k.clone());
                collect(v, out);
            }
        }
        _ => {}
    }
}

/// Panics if any of `secrets`, in any of its encoded forms, is anywhere in `bytes`.
pub(crate) fn assert_absent<'a>(bytes: &[u8], secrets: impl IntoIterator<Item = &'a str>, what: &str) {
    let haystack = strings(bytes);
    for secret in secrets {
        for needle in needles(secret) {
            if let Some(found) = haystack.iter().find(|s| s.contains(&needle)) {
                let at = found.find(&needle).unwrap_or(0);
                let context = &found[at.saturating_sub(40).min(at)..];
                let context: String = context.chars().take(120).collect();
                panic!("{what}: a canary leaked (form {needle:?}) near {context:?}");
            }
        }
    }
}

#[cfg(test)]
mod tests;
