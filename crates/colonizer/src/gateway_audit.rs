//! The gateway's per-request audit log (issue #302): one JSON line per authenticated request in
//! `<session_dir>/gateway.jsonl`, saying what was asked for, where it went and how it ended. The
//! record is a fixed serde struct — the struct is the allowlist, so no string copied from a body or
//! a header can turn a secret into a log line.

use crate::{App, providers::Usage, providers::Wire};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::{
    fs::OpenOptions,
    io::Write,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

/// The file a colony's gateway requests are audited into, under its session directory.
pub(crate) fn log_path(app: &App, colony: &str) -> PathBuf {
    app.session_dir(colony).join("gateway.jsonl")
}

/// Every way a gateway request can fail, one code each: the audit log's `failure` value and the
/// provider's `last_failure`, so every surface reads one vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GatewayFailure {
    /// The colony token named a provider this mothership has not configured.
    UnknownProvider,
    /// Configured, but this colony's model settings never routed to it (issue #409).
    NotRouted,
    /// The colony's task touches restricted paths and the provider is not trusted (issue #472).
    Restricted,
    /// A keyed provider with no saved key: refused before any upstream call.
    MissingKey,
    /// The colony passed its spend budget, or this request's estimate would tip it over.
    Budget,
    /// A path or body the wire cannot serve: an unsupported path or an untranslatable request.
    BadRequest,
    /// No free per-provider request slot within `queue_timeout_secs`.
    QueueFull,
    /// The upstream could not be reached at all (connect or send error).
    Unreachable,
    /// The upstream sent no answer within `timeout_secs`.
    Timeout,
    /// The upstream answered 4xx/5xx with an error that is not quota exhaustion.
    UpstreamError,
    /// The upstream says the plan is out (issue #225).
    QuotaExhausted,
    /// The response body failed — or would not translate — after the headers were in.
    BodyReadFailed,
}

impl GatewayFailure {
    pub fn code(self) -> &'static str {
        match self {
            Self::UnknownProvider => "unknown_provider",
            Self::NotRouted => "not_routed",
            Self::Restricted => "restricted",
            Self::MissingKey => "missing_key",
            Self::Budget => "budget",
            Self::BadRequest => "bad_request",
            Self::QueueFull => "queue_full",
            Self::Unreachable => "unreachable",
            Self::Timeout => "timeout",
            Self::UpstreamError => "upstream_error",
            Self::QuotaExhausted => "quota_exhausted",
            Self::BodyReadFailed => "body_read_failed",
        }
    }

    /// Whether the plain answer tells the colony's router it may fall back to Claude: the three
    /// gateway-level failures answer 502/503/504 with `x-colonizer-fallback`, and quota exhaustion
    /// names the same header — but only when the provider has a fallback model and failover is on,
    /// a decision the caller makes, passing it to [`GatewayAudit::fail_with`].
    pub fn fallback(self) -> bool {
        matches!(
            self,
            Self::QueueFull | Self::Unreachable | Self::Timeout | Self::QuotaExhausted
        )
    }
}

/// The wire's name as the record carries it.
pub(crate) fn wire_name(wire: Wire) -> &'static str {
    match wire {
        Wire::Anthropic => "anthropic",
        Wire::Openai => "openai",
    }
}

/// One request's audit record, exactly as the log line serializes it: every field fixed and typed,
/// model strings only ever landing here through the model-id validator.
#[derive(Clone, Debug, Serialize)]
pub struct GatewayAuditRecord {
    /// Always `gateway_request`.
    #[serde(rename = "type")]
    kind: &'static str,
    /// When the request was accepted, RFC3339 like the harness log's `ts`.
    ts: DateTime<Utc>,
    colony: String,
    /// The provider id the request named, even when no such provider exists.
    provider: String,
    /// The wire the provider speaks; `null` only when there was no such provider.
    wire: Option<&'static str>,
    /// The requested model, only when it passes the model-id validator — never raw body text.
    model: Option<String>,
    /// The model sent upstream, same validator.
    wire_model: Option<String>,
    method: String,
    /// The provider-relative path, query string dropped.
    path: String,
    status: u16,
    /// One of [`GatewayFailure`]'s codes; `null` when the request did not fail.
    failure: Option<&'static str>,
    /// Whether the answer signalled the router it may fall back to Claude.
    fallback: bool,
    queue_ms: u64,
    /// Filled in when the line is written, so a streamed response counts its whole body.
    duration_ms: u64,
    request_bytes: usize,
    response_bytes: usize,
    /// From the usage tap, when it managed to count the body.
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

/// One request's audit guard: created the moment a colony token authenticates, updated as the
/// request proceeds, and appending its line when it — or the streamed body holding a clone — ends.
/// Appends are best-effort: a failed log write never fails the request.
#[derive(Clone)]
pub(crate) struct GatewayAudit {
    inner: Arc<Inner>,
}

struct Inner {
    log: PathBuf,
    start: Instant,
    record: Mutex<GatewayAuditRecord>,
    /// The line is written once, whichever of the guard and the body clone ends first.
    emitted: AtomicBool,
}

impl GatewayAudit {
    /// Starts the record; the requested model lands later, from the body's one parse.
    pub(crate) fn start(log: PathBuf, colony: &str, provider: &str, method: &str, path: &str, request_bytes: usize) -> Self {
        let record = GatewayAuditRecord {
            kind: "gateway_request",
            ts: Utc::now(),
            colony: colony.to_string(),
            provider: provider.to_string(),
            wire: None,
            model: None,
            wire_model: None,
            method: method.to_string(),
            path: path.to_string(),
            status: 0,
            failure: None,
            fallback: false,
            queue_ms: 0,
            duration_ms: 0,
            request_bytes,
            response_bytes: 0,
            input_tokens: None,
            output_tokens: None,
        };
        Self {
            inner: Arc::new(Inner {
                log,
                start: Instant::now(),
                record: Mutex::new(record),
                emitted: AtomicBool::new(false),
            }),
        }
    }

    fn edit(&self, f: impl FnOnce(&mut GatewayAuditRecord)) {
        f(&mut self.inner.record.lock().unwrap());
    }

    /// Names the outcome of a failed request; `fallback` follows the failure's plain answer.
    pub(crate) fn fail(&self, status: u16, failure: GatewayFailure) {
        self.fail_with(status, failure, failure.fallback());
    }

    /// The same, where the wire's answer declines the fallback: quota without a fallback model
    /// answers 429/403 with no `x-colonizer-fallback`.
    pub(crate) fn fail_with(&self, status: u16, failure: GatewayFailure, fallback: bool) {
        self.edit(|r| {
            r.status = status;
            r.failure = Some(failure.code());
            r.fallback = fallback;
        });
    }

    pub(crate) fn set_wire(&self, wire: &'static str) {
        self.edit(|r| r.wire = Some(wire));
    }

    /// The requested model, validator applied by the caller.
    pub(crate) fn set_model(&self, model: Option<String>) {
        self.edit(|r| r.model = model);
    }

    /// The model actually sent upstream, validator applied.
    pub(crate) fn set_wire_model(&self, model: Option<String>) {
        self.edit(|r| r.wire_model = model);
    }

    pub(crate) fn set_status(&self, status: u16) {
        self.edit(|r| r.status = status);
    }

    pub(crate) fn set_queue_ms(&self, ms: u64) {
        self.edit(|r| r.queue_ms = ms);
    }

    pub(crate) fn add_bytes(&self, n: usize) {
        self.edit(|r| r.response_bytes += n);
    }

    /// A body that errored after the headers were in: whatever status the headers carried, never a
    /// fallback signal.
    pub(crate) fn note_body_error(&self) {
        self.edit(|r| {
            r.failure = Some(GatewayFailure::BodyReadFailed.code());
            r.fallback = false;
        });
    }

    /// Fills in the usage tap's token counts, when it counted any, and writes the line.
    pub(crate) fn finish(&self, usage: Usage) {
        if usage.total_tokens() > 0 {
            self.edit(|r| {
                r.input_tokens = Some(usage.input_tokens);
                r.output_tokens = Some(usage.output_tokens);
            });
        }
        self.emit();
    }

    /// Appends the one line. Idempotent: the guard and the streamed body each call it, first wins.
    fn emit(&self) {
        if self.inner.emitted.swap(true, Ordering::SeqCst) {
            return;
        }
        let mut record = self.inner.record.lock().unwrap();
        record.duration_ms = self.inner.start.elapsed().as_millis() as u64;
        if let Ok(line) = serde_json::to_string(&*record)
            && let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&self.inner.log)
        {
            let _ = writeln!(file, "{line}");
        }
    }
}

impl Drop for GatewayAudit {
    fn drop(&mut self) {
        self.emit();
    }
}

#[cfg(test)]
mod tests {
    use super::{GatewayFailure::*, *};
    use serde_json::Value;
    use std::{collections::HashSet, path::Path};

    fn started() -> (GatewayAudit, PathBuf) {
        let dir = std::env::temp_dir().join(format!("colonizer-audit-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("gateway.jsonl");
        let audit = GatewayAudit::start(path.clone(), "c1", "deepseek", "POST", "/v1/messages", 2);
        (audit, path)
    }

    fn lines(path: &Path) -> Vec<Value> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn failure_codes_are_distinct_and_fallback_names_what_the_router_acts_on() {
        let all = [
            UnknownProvider,
            NotRouted,
            Restricted,
            MissingKey,
            Budget,
            BadRequest,
            QueueFull,
            Unreachable,
            Timeout,
            UpstreamError,
            QuotaExhausted,
            BodyReadFailed,
        ];
        let codes: HashSet<_> = all.iter().map(|f| f.code()).collect();
        assert_eq!(codes.len(), all.len(), "one code each");
        for failure in all {
            assert_eq!(
                failure.fallback(),
                matches!(failure, QueueFull | Unreachable | Timeout | QuotaExhausted),
                "{}",
                failure.code()
            );
        }
    }

    #[test]
    fn one_shaped_line_per_request() {
        let (audit, path) = started();
        audit.set_wire("anthropic");
        audit.set_model(Some("claude-sonnet-5".into()));
        audit.set_wire_model(Some("claude-sonnet-5".into()));
        audit.set_queue_ms(7);
        audit.set_status(200);
        audit.add_bytes(11);
        audit.finish(Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Default::default()
        });
        let lines = lines(&path);
        assert_eq!(lines.len(), 1, "exactly one line");
        assert_eq!(
            lines[0],
            serde_json::json!({
                "type": "gateway_request",
                "ts": lines[0]["ts"],
                "colony": "c1",
                "provider": "deepseek",
                "wire": "anthropic",
                "model": "claude-sonnet-5",
                "wire_model": "claude-sonnet-5",
                "method": "POST",
                "path": "/v1/messages",
                "status": 200,
                "failure": null,
                "fallback": false,
                "queue_ms": 7,
                "duration_ms": lines[0]["duration_ms"],
                "request_bytes": 2,
                "response_bytes": 11,
                "input_tokens": 10,
                "output_tokens": 5,
            })
        );
        assert!(lines[0]["ts"].as_str().unwrap().contains('T'), "ts is an RFC3339 timestamp");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn the_body_clone_never_writes_a_second_line_and_a_failure_names_its_code() {
        let (audit, path) = started();
        let body = audit.clone();
        audit.fail(503, QueueFull);
        body.finish(Usage::default());
        drop(body);
        drop(audit);
        let lines = lines(&path);
        assert_eq!(lines.len(), 1, "one line however many guards end");
        assert_eq!(lines[0]["failure"], "queue_full");
        assert_eq!(lines[0]["fallback"], true, "the plain answer carries the fallback header");
        assert_eq!(lines[0]["status"], 503);
        assert_eq!(lines[0]["input_tokens"], Value::Null, "an uncounted body names no tokens");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_write_that_cannot_open_is_silent_and_never_panics() {
        let audit = GatewayAudit::start(
            PathBuf::from("/nonexistent-dir/gateway.jsonl"),
            "c1",
            "p",
            "POST",
            "/v1/messages",
            0,
        );
        audit.set_status(200);
        audit.finish(Usage::default());
    }
}
