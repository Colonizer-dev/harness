//! The Living Brain API, as the mothership calls it — the same routes and bodies the brain's own
//! CLI uses (`crates/livingbrain-cli/src/api.rs`):
//!
//! | route | body | answer |
//! |---|---|---|
//! | `POST /v1/device-auth/code` | `{"client_id":"livingbrain-cli"}` | `{device_code, user_code, verification_uri, verification_uri_complete?, interval, expires_in}` |
//! | `POST /v1/device-auth/token` | form `grant_type=…device_code&device_code=…&client_id=livingbrain-cli` | `{access_token}`, or `400 {error}` (RFC 8628 §3.5) |
//! | `GET /v1/tokens` | — | `{tokens:[{prefix, name, scopes, created_at}]}` (bearer-readable) |
//! | `DELETE /v1/tokens/{prefix}` | — | `204` |
//! | `POST /v1/pages/sources` | `{kind, path, body, scope, project?}` | `{id, created, …}` |
//! | `POST /v1/notes` | `{body, title, slug, project?}` | `{id, url, unchanged, …}` |
//!
//! The token only ever travels in the `Authorization` header to the configured server. Nothing in
//! this file formats it: [`Client`]'s `Debug` withholds it, and an error carries the server's
//! words (redacted) and the status, never the request.

use serde_json::{Value, json};
use std::{sync::LazyLock, time::Duration};

/// The brain's one public device-flow client.
pub(crate) const CLIENT_ID: &str = "livingbrain-cli";
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

static HTTP: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .user_agent(concat!("colonizer/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .expect("a reqwest client with a user agent and timeouts always builds")
});

/// Why a call did not do its job: the HTTP status when there was one, and the server's message.
#[derive(Debug, Clone)]
pub(crate) struct LbError {
    pub(crate) status: Option<u16>,
    pub(crate) message: String,
}

impl LbError {
    fn transport(e: reqwest::Error) -> Self {
        // reqwest's Display names the URL, never a header, so it is safe to keep.
        LbError {
            status: None,
            message: format!(
                "Living Brain could not be reached ({})",
                colonizer_redact::redact_text(&e.to_string())
            ),
        }
    }

    /// A refusal the same request would get again: retrying it only spends the rate limit.
    pub(crate) fn permanent(&self) -> bool {
        matches!(self.status, Some(400 | 403 | 404 | 409 | 413 | 422))
    }

    pub(crate) fn unauthorized(&self) -> bool {
        self.status == Some(401)
    }
}

impl std::fmt::Display for LbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// What one poll of the token endpoint ended as (RFC 8628 §3.5).
pub(crate) enum Poll {
    Issued(String),
    Pending,
    SlowDown,
    /// `expired_token`, `access_denied`, or anything else final, in the server's word.
    Refused(String),
}

pub(crate) struct Client {
    base: String,
    token: Option<String>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("base", &self.base)
            .field("token", &self.token.as_ref().map(|_| "<withheld>"))
            .finish()
    }
}

/// `application/x-www-form-urlencoded` for one value (RFC 3986 unreserved kept).
fn form_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A path segment, percent-encoded.
fn segment(value: &str) -> String {
    form_encode(value)
}

/// The server's own words for a refusal: `{error}`, `{message}`, or the RFC 9457 problem's
/// `title`/`detail`; the status line otherwise. Redacted, and cut to a line a person can read.
fn refusal(status: u16, body: &[u8]) -> String {
    let parsed: Option<Value> = serde_json::from_slice(body).ok();
    let words = parsed.as_ref().and_then(|v| {
        let text = |k: &str| v.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());
        match (text("title"), text("detail")) {
            (Some(t), Some(d)) => Some(format!("{t}: {d}")),
            (Some(t), None) => Some(t.to_owned()),
            _ => text("error").or_else(|| text("message")).map(str::to_owned),
        }
    });
    let mut message = format!(
        "Living Brain answered {status}{}",
        words.map(|w| format!(": {w}")).unwrap_or_default()
    );
    if message.chars().count() > 300 {
        message = message.chars().take(300).collect::<String>() + "…";
    }
    colonizer_redact::redact_text(&message).into_owned()
}

impl Client {
    pub(crate) fn new(base: &str, token: Option<String>) -> Self {
        Client {
            base: base.trim_end_matches('/').to_owned(),
            token,
        }
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut request = HTTP.request(method, format!("{}{path}", self.base));
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        request
    }

    async fn send(&self, request: reqwest::RequestBuilder) -> Result<(u16, Value), LbError> {
        let response = request.send().await.map_err(LbError::transport)?;
        let status = response.status().as_u16();
        let body = response.bytes().await.map_err(LbError::transport)?;
        if (200..300).contains(&status) {
            let value = if body.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&body).map_err(|_| LbError {
                    status: Some(status),
                    message: format!("Living Brain answered {status} with a body that is not JSON"),
                })?
            };
            return Ok((status, value));
        }
        Err(LbError {
            status: Some(status),
            message: refusal(status, &body[..body.len().min(4096)]),
        })
    }

    /// `POST /v1/device-auth/code`.
    pub(crate) async fn device_code(&self) -> Result<Value, LbError> {
        let request = self
            .request(reqwest::Method::POST, "/v1/device-auth/code")
            .json(&json!({ "client_id": CLIENT_ID }));
        self.send(request).await.map(|(_, v)| v)
    }

    /// One poll of `POST /v1/device-auth/token`.
    pub(crate) async fn device_token(&self, device_code: &str) -> Result<Poll, LbError> {
        let form = format!(
            "grant_type={}&device_code={}&client_id={}",
            form_encode(DEVICE_GRANT),
            form_encode(device_code),
            form_encode(CLIENT_ID)
        );
        let request = self
            .request(reqwest::Method::POST, "/v1/device-auth/token")
            .header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(form);
        let response = request.send().await.map_err(LbError::transport)?;
        let status = response.status().as_u16();
        let body = response.bytes().await.map_err(LbError::transport)?;
        let value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        if (200..300).contains(&status) {
            return match value.get("access_token").and_then(Value::as_str) {
                Some(token) if !token.trim().is_empty() => Ok(Poll::Issued(token.trim().to_owned())),
                _ => Err(LbError {
                    status: Some(status),
                    message: "Living Brain approved the code but sent no token".into(),
                }),
            };
        }
        match value.get("error").and_then(Value::as_str) {
            Some("authorization_pending") => Ok(Poll::Pending),
            Some("slow_down") => Ok(Poll::SlowDown),
            Some(other) if status == 400 => Ok(Poll::Refused(other.to_owned())),
            _ => Err(LbError {
                status: Some(status),
                message: refusal(status, &body[..body.len().min(4096)]),
            }),
        }
    }

    /// `GET /v1/tokens`: the caller's own tokens — the one bearer-readable identity the brain has.
    pub(crate) async fn tokens(&self) -> Result<Value, LbError> {
        self.send(self.request(reqwest::Method::GET, "/v1/tokens"))
            .await
            .map(|(_, v)| v)
    }

    /// `DELETE /v1/tokens/{prefix}`: revokes the token server-side.
    pub(crate) async fn revoke(&self, prefix: &str) -> Result<(), LbError> {
        let path = format!("/v1/tokens/{}", segment(prefix));
        self.send(self.request(reqwest::Method::DELETE, &path)).await.map(|_| ())
    }

    /// `POST /v1/pages/sources` — one `agent_log` source in the personal scope.
    pub(crate) async fn post_source(&self, path: &str, body: &str, project: Option<&str>) -> Result<Value, LbError> {
        let mut payload = json!({ "kind": "agent_log", "path": path, "body": body, "scope": "personal" });
        if let Some(project) = project {
            payload["project"] = json!(project);
        }
        let request = self.request(reqwest::Method::POST, "/v1/pages/sources").json(&payload);
        self.send(request).await.map(|(_, v)| v)
    }

    /// `POST /v1/notes` keyed by `slug`, so a re-send updates the one page in place.
    pub(crate) async fn keyed_note(&self, slug: &str, title: &str, body: &str, project: Option<&str>) -> Result<Value, LbError> {
        let mut payload = json!({ "body": body, "title": title, "slug": slug });
        if let Some(project) = project {
            payload["project"] = json!(project);
        }
        let request = self.request(reqwest::Method::POST, "/v1/notes").json(&payload);
        self.send(request).await.map(|(_, v)| v)
    }

    /// `GET /v1/graph?limit=N`: the brain's map (nodes and links). Read chunk by chunk and refused
    /// past `cap` bytes, so a brain cannot make the mothership hold an unbounded answer.
    pub(crate) async fn graph(&self, limit: usize, cap: usize) -> Result<Value, LbError> {
        let path = format!("/v1/graph?limit={limit}");
        let mut response = self
            .request(reqwest::Method::GET, &path)
            .send()
            .await
            .map_err(LbError::transport)?;
        let status = response.status().as_u16();
        let mut body: Vec<u8> = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(LbError::transport)? {
            if body.len() + chunk.len() > cap {
                return Err(LbError {
                    status: Some(status),
                    message: "Living Brain's map is larger than the brain map reads".into(),
                });
            }
            body.extend_from_slice(&chunk);
        }
        if !(200..300).contains(&status) {
            return Err(LbError {
                status: Some(status),
                message: refusal(status, &body[..body.len().min(4096)]),
            });
        }
        serde_json::from_slice(&body).map_err(|_| LbError {
            status: Some(status),
            message: format!("Living Brain answered {status} with a body that is not JSON"),
        })
    }
}

/// The public half of a personal access token, `lbp_<id>` out of `lbp_<id>_<secret>` — what the
/// brain lists and revokes by. `None` for anything not shaped like one.
pub(crate) fn token_prefix(token: &str) -> Option<String> {
    let (prefix, secret) = token.trim().rsplit_once('_')?;
    let id = prefix.strip_prefix("lbp_")?;
    (!id.is_empty() && !secret.is_empty() && !id.contains('_')).then(|| prefix.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prefix_is_the_public_half() {
        assert_eq!(token_prefix("lbp_01ABC_s3cr3t").as_deref(), Some("lbp_01ABC"));
        assert_eq!(token_prefix("  lbp_x_y \n").as_deref(), Some("lbp_x"));
        assert_eq!(token_prefix("ghp_abc_def"), None);
        assert_eq!(token_prefix("lbp__secret"), None);
        assert_eq!(token_prefix("lbp_a_b_c"), None, "an id never carries an underscore");
        assert_eq!(token_prefix("lbp_abc"), None);
    }

    #[test]
    fn a_client_never_debugs_its_token() {
        let client = Client::new("https://app.example/", Some("lbp_id_SENTINEL".into()));
        let shown = format!("{client:?}");
        assert!(!shown.contains("SENTINEL"), "{shown}");
        assert!(shown.contains("https://app.example"), "{shown}");
    }

    #[test]
    fn a_refusal_reads_the_servers_words() {
        assert_eq!(
            refusal(401, br#"{"title":"Sign in","detail":"no token"}"#),
            "Living Brain answered 401: Sign in: no token"
        );
        assert_eq!(
            refusal(400, br#"{"error":"expired_token"}"#),
            "Living Brain answered 400: expired_token"
        );
        assert_eq!(refusal(502, b"<html>"), "Living Brain answered 502");
    }

    #[test]
    fn form_values_are_percent_encoded() {
        assert_eq!(form_encode("urn:a b/c"), "urn%3Aa%20b%2Fc");
        assert_eq!(form_encode("Ab-9._~"), "Ab-9._~");
    }
}
