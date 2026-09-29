//! The Unified Harness Protocol wire helpers (docs/protocol.md §7): the spec version every
//! `/uhp/…` reply advertises and the §7.7 error envelope both route families answer with. The
//! routes themselves live with their feature (`sessions/files.rs` has the §7.5 artifacts); this
//! module only holds what more than one surface would otherwise copy.

use axum::{
    Json,
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse as _, Response},
};
use serde_json::{Value, json};

/// The spec version §7.1 makes every `/uhp/v1/…` reply send.
pub(crate) const VERSION: &str = "2026-09-12";

/// The request header a client sets to ask for UHP shapes, and the reply header the `/uhp`
/// surface always sends (§7.1).
pub(crate) const VERSION_HEADER: HeaderName = HeaderName::from_static("uhp-version");

/// Stamps a reply with [`VERSION`]: what every `/uhp/…` route wraps its answer in.
pub(crate) fn stamped(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(VERSION_HEADER, HeaderValue::from_static(VERSION));
    response
}

/// Whether a request asked an `/api/…` route for UHP shapes (§7.1): errors then come in the
/// §7.7 envelope instead of Colonizer's own.
fn speaks_uhp(headers: &HeaderMap) -> bool {
    headers.contains_key(VERSION_HEADER)
}

/// The §7.7 envelope: the shape OpenAI's Responses API established, with the code in UHP's
/// vocabulary where it has one. A caller's mistake — a bad id, a bad name, an oversize read —
/// is the request-error type; a failure on our side is the server one.
fn envelope(status: StatusCode, code: &str, message: String, detail: Value) -> Response {
    let kind = if status.is_server_error() {
        "server_error"
    } else {
        "invalid_request_error"
    };
    (
        status,
        Json(json!({
            "error": {
                "type": kind,
                "code": code,
                "message": message,
                "param": Value::Null,
                "detail": detail,
            }
        })),
    )
        .into_response()
}

/// Colonizer's own error body with the code as a sibling (§7.7): what an `/api/…` route answers
/// when the request does not speak UHP. The detail is envelope-only and is dropped here.
fn coded(status: StatusCode, code: &str, message: String) -> Response {
    (status, Json(json!({"error": message, "code": code}))).into_response()
}

/// A `/uhp/…` route's refusal: always the §7.7 envelope, with `detail` riding along when the
/// code carries one (`file_too_large`'s `max_bytes`, §7.5).
pub(crate) fn uhp_error(status: StatusCode, code: &str, message: impl std::fmt::Display, detail: Option<Value>) -> Response {
    envelope(status, code, message.to_string(), detail.unwrap_or(Value::Null))
}

/// An `/api/…` route's refusal: the §7.7 envelope when the request sent `UHP-Version`,
/// Colonizer's string error with a `code` sibling otherwise (§7.7). The detail is envelope-only
/// and is dropped with the envelope.
pub(crate) fn api_error(
    headers: &HeaderMap,
    status: StatusCode,
    code: &str,
    message: impl std::fmt::Display,
    detail: Option<Value>,
) -> Response {
    if speaks_uhp(headers) {
        envelope(status, code, message.to_string(), detail.unwrap_or(Value::Null))
    } else {
        coded(status, code, message.to_string())
    }
}

/// The one refusal both routes of a `/api`/`/uhp` pair answer with: the shared helpers take the
/// surface from their caller and land here.
pub(crate) fn error_for(
    uhp: bool,
    headers: &HeaderMap,
    status: StatusCode,
    code: &str,
    message: impl std::fmt::Display,
    detail: Option<Value>,
) -> Response {
    if uhp {
        uhp_error(status, code, message, detail)
    } else {
        api_error(headers, status, code, message, detail)
    }
}

/// The JSON 404 every `/uhp` path no route claims answers with (`#641`'s answer, carried over to
/// the protocol's paths): a miss must read as an error, never as the cockpit's page.
pub(crate) fn unknown_route() -> Response {
    stamped(uhp_error(StatusCode::NOT_FOUND, "not_found", "no such UHP route", None))
}
