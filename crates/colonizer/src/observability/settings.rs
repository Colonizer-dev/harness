//! Providers, settings schema, save-time rules and the parsed [`ExporterConfig`] for the
//! `observability` module kind (#840).
//!
//! The module sends nothing in this build: the schema, the rules and the parsed shape are what a
//! save validates against and what the exporter of a later issue (#851 gRPC, #852 the headers
//! secret) will read. The one non-obvious rule is the content confirmation — turning on
//! `conversation_content` or `conversation_thinking` needs the same PUT to also carry
//! `"confirm_content": true`, which is a request field and is never written to `modules.json`.

use crate::config::ModuleChoice;
use crate::modules::Provider;
use serde_json::{Map, Value, json};

/// The provider that ships events over OTLP to a backend.
pub const OTLP: &str = "otlp";
/// The provider that writes events to capped local files and sends nothing.
pub const FILE: &str = "file";

fn provider(id: &str, name: &str, description: &str, schema: Value) -> Provider {
    Provider {
        id: id.into(),
        name: name.into(),
        description: description.into(),
        schema,
        loop_tools: false,
    }
}

/// The settings both providers share, appended to each provider's own schema so the two agree by
/// construction rather than by a copy a later edit forgets.
fn shared_properties() -> Map<String, Value> {
    let mut m = Map::new();
    m.insert(
        "stream_operational".into(),
        json!({"type": "boolean", "title": "Operational logs", "description": "The harness's own lines: boots, parks, quota pauses, watchdog nudges.", "default": true}),
    );
    m.insert(
        "stream_activity".into(),
        json!({"type": "boolean", "title": "Colony activity", "description": "What each colony is doing: stages, tool use and status changes.", "default": true}),
    );
    m.insert(
        "stream_traces".into(),
        json!({"type": "boolean", "title": "Traces", "description": "One trace per colony, its turns, tool calls and subagents as spans, so a slow step can be seen in context.", "default": true}),
    );
    m.insert(
        "stream_metrics".into(),
        json!({"type": "boolean", "title": "Metrics", "description": "Counters and durations: colonies, tokens and requests.", "default": true}),
    );
    m.insert(
        "conversation_content".into(),
        json!({"type": "boolean", "title": "Conversation content", "description": "Off by default, and the loudest switch here: it sends the words you and the agent exchanged. Switching it on needs the same save to carry \"confirm_content\": true.", "default": false}),
    );
    m.insert(
        "conversation_thinking".into(),
        json!({"type": "boolean", "title": "Agent thinking", "description": "Off by default: the agent's reasoning, not just its messages. Switching it on needs the same save to carry \"confirm_content\": true.", "default": false}),
    );
    m.insert(
        "trace_sample_ratio".into(),
        json!({"type": "number", "title": "Trace sample ratio", "description": "The share of colonies traced, 0 to 1, chosen per colony so a trace is always whole. 1 traces every colony. Logs and metrics are never sampled.", "minimum": 0, "maximum": 1, "default": 1}),
    );
    m.insert(
        "max_attribute_bytes".into(),
        json!({"type": "integer", "title": "Max attribute size (bytes)", "description": "A single attribute longer than this is truncated.", "minimum": 128, "maximum": 8192, "default": 1024}),
    );
    m.insert(
        "max_content_bytes".into(),
        json!({"type": "integer", "title": "Max content size (bytes)", "description": "A single message, log body or trace span longer than this is truncated.", "minimum": 1024, "maximum": 196608, "default": 32768}),
    );
    m.insert(
        "max_trace_bytes".into(),
        json!({"type": "integer", "title": "Max trace size (bytes)", "description": "One colony trace's budget, kept under Tempo's 5 MB per trace. Past 90% of it, tool, subagent, question, gateway and host-step spans are counted on their turn and the root instead of sent; turns and the root always are.", "minimum": 1024, "default": 4194304}),
    );
    m.insert(
        "repo_names".into(),
        json!({"type": "string", "title": "Repository names", "enum": ["plain", "hashed"], "default": "plain", "description": "Plain sends owner/repo as it is. Hashed sends a salted hash instead, so a backend never learns which repository an event came from."}),
    );
    m.insert(
        "prometheus".into(),
        json!({"type": "boolean", "title": "Expose Prometheus metrics", "description": "Serve the metrics on the harness's own metrics endpoint for scraping, in addition to any exporter above.", "default": false}),
    );
    m.insert(
        "max_backlog_days".into(),
        json!({"type": "integer", "title": "Max backlog (days)", "description": "Events older than this are dropped rather than exported late; 0 keeps everything.", "minimum": 0, "default": 7}),
    );
    m.insert(
        "max_read_mib_per_sec".into(),
        json!({"type": "integer", "title": "Max read rate (MiB/s)", "description": "A ceiling on how fast the exporter reads its backlog, so it never crowds a running colony.", "minimum": 1, "default": 8}),
    );
    m.insert(
        "start_from".into(),
        json!({"type": "string", "title": "Start from", "enum": ["now", "backlog"], "default": "now", "description": "Where a new destination starts. now sends only what is written from the moment it is configured; backlog also sends what the ledgers already hold, back to Max backlog (days). Settled once per endpoint: changing it later does not move the read position."}),
    );
    m
}

/// A provider's schema: its own properties first (the ones an operator reaches for), then the
/// shared ones.
fn schema(own: Vec<(&str, Value)>) -> Value {
    let mut props = Map::new();
    for (key, spec) in own {
        props.insert(key.into(), spec);
    }
    for (key, spec) in shared_properties() {
        props.insert(key, spec);
    }
    json!({"type": "object", "properties": props})
}

/// The two providers of this kind, each with the settings both share.
pub fn providers() -> Vec<Provider> {
    vec![
        provider(
            OTLP,
            "OTLP endpoint",
            "Send logs, traces and metrics over OTLP (HTTP) to Grafana Cloud, a local collector, Datadog, Honeycomb, Elastic, SigNoz, New Relic or any OpenTelemetry backend",
            schema(vec![
                (
                    "endpoint",
                    json!({"type": "string", "title": "Endpoint URL", "description": "http:// or https://. It must carry no credentials (user:pass@) and no query string — those belong in the observability-headers secret, never in the URL.", "default": ""}),
                ),
                (
                    "protocol",
                    json!({"type": "string", "title": "Protocol", "enum": ["http/protobuf", "http/json", "grpc"], "default": "http/protobuf", "description": "http/protobuf suits almost every backend. grpc is refused in this build (no gRPC transport yet); see the observability docs."}),
                ),
                (
                    "preset",
                    json!({"type": "string", "title": "Backend", "enum": ["custom", "grafana_cloud", "local_collector", "datadog", "honeycomb", "elastic", "signoz", "new_relic"], "default": "custom", "description": "Which backend this endpoint is, so the right header name is offered. The header's value lives in the observability-headers secret, never here: Honeycomb wants x-honeycomb-team, Datadog dd-api-key, New Relic api-key, Grafana Cloud Authorization (Basic), Elastic Authorization (ApiKey), SigNoz and a local collector usually want none."}),
                ),
                (
                    "compression",
                    json!({"type": "string", "title": "Compression", "enum": ["gzip", "none"], "default": "gzip"}),
                ),
                (
                    "timeout_secs",
                    json!({"type": "integer", "title": "Request timeout (seconds)", "minimum": 1, "maximum": 120, "default": 10}),
                ),
                (
                    "allow_insecure",
                    json!({"type": "boolean", "title": "Allow plain http to a public host", "description": "Off: a plain http:// endpoint is accepted only for loopback and private addresses. On: send anyway — the payload then travels unencrypted.", "default": false}),
                ),
            ]),
        ),
        provider(
            FILE,
            "Local file",
            "Write the same events to capped files under the data dir, with nothing leaving the machine",
            schema(vec![
                (
                    "dir",
                    json!({"type": "string", "title": "Directory", "description": "Where the local exporter writes, relative to the data dir. Empty means the default, <data>/observability/otlp.", "default": ""}),
                ),
                (
                    "max_mb",
                    json!({"type": "integer", "title": "Total size cap (MB)", "description": "Total size across the files; the oldest is trimmed first.", "minimum": 1, "default": 512}),
                ),
            ]),
        ),
    ]
}

/// The parsed settings of one observability module, with every default applied.
///
/// The exporter (#851, #852) reads this; nothing in this build does yet, so the whole type is
/// allowed to be unused until then.
#[allow(dead_code)]
#[derive(Clone)]
pub struct ExporterConfig {
    pub provider: String,
    pub endpoint: String,
    pub protocol: String,
    pub preset: String,
    pub compression: String,
    pub timeout_secs: u64,
    pub allow_insecure: bool,
    pub dir: String,
    pub max_mb: u64,
    pub stream_operational: bool,
    pub stream_activity: bool,
    pub stream_traces: bool,
    pub stream_metrics: bool,
    pub conversation_content: bool,
    pub conversation_thinking: bool,
    pub trace_sample_ratio: f64,
    pub max_attribute_bytes: u64,
    pub max_content_bytes: u64,
    pub max_trace_bytes: u64,
    pub repo_names: String,
    pub prometheus: bool,
    pub max_backlog_days: u64,
    pub max_read_mib_per_sec: u64,
    /// Where a new destination starts: `now` or `backlog`.
    pub start_from: String,
    /// The OTLP headers, filled by a later issue from the `observability-headers` secret (#852).
    /// Empty in this build — nothing here reads a secret or an environment variable.
    pub headers: Vec<(String, String)>,
}

impl ExporterConfig {
    /// Parse a saved [`ModuleChoice`] for this kind, or `None` when the module is switched off.
    ///
    /// An *absent* module is the caller's `Option`, not this function's: read
    /// `modules.observability.as_ref()` and call this only when it is `Some`, so "never configured"
    /// and "switched off" both read as no exporter.
    #[allow(dead_code)] // called by the exporter (#851, #852), which this issue does not land yet.
    pub fn from_module(choice: &ModuleChoice) -> Option<ExporterConfig> {
        if !choice.enabled {
            return None;
        }
        let s = &choice.settings;
        Some(ExporterConfig {
            provider: choice.provider.clone(),
            endpoint: str_at(s, "endpoint", ""),
            protocol: str_at(s, "protocol", "http/protobuf"),
            preset: str_at(s, "preset", "custom"),
            compression: str_at(s, "compression", "gzip"),
            timeout_secs: u64_at(s, "timeout_secs", 10),
            allow_insecure: bool_at(s, "allow_insecure", false),
            dir: str_at(s, "dir", ""),
            max_mb: u64_at(s, "max_mb", 512),
            stream_operational: bool_at(s, "stream_operational", true),
            stream_activity: bool_at(s, "stream_activity", true),
            stream_traces: bool_at(s, "stream_traces", true),
            stream_metrics: bool_at(s, "stream_metrics", true),
            conversation_content: bool_at(s, "conversation_content", false),
            conversation_thinking: bool_at(s, "conversation_thinking", false),
            trace_sample_ratio: f64_at(s, "trace_sample_ratio", 1.0),
            max_attribute_bytes: u64_at(s, "max_attribute_bytes", 1024),
            max_content_bytes: u64_at(s, "max_content_bytes", 32768),
            max_trace_bytes: u64_at(s, "max_trace_bytes", 4_194_304),
            repo_names: str_at(s, "repo_names", "plain"),
            prometheus: bool_at(s, "prometheus", false),
            max_backlog_days: u64_at(s, "max_backlog_days", 7),
            max_read_mib_per_sec: u64_at(s, "max_read_mib_per_sec", 8),
            start_from: str_at(s, "start_from", "now"),
            headers: Vec::new(),
        })
    }
}

/// Prints header *names*, never their values: a log line or a panic message must not leak the
/// credential a header carries.
impl std::fmt::Debug for ExporterConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let headers = self
            .headers
            .iter()
            .map(|(name, _)| format!("{name}=<redacted>"))
            .collect::<Vec<_>>()
            .join(", ");
        f.debug_struct("ExporterConfig")
            .field("provider", &self.provider)
            .field("endpoint", &self.endpoint)
            .field("protocol", &self.protocol)
            .field("preset", &self.preset)
            .field("compression", &self.compression)
            .field("timeout_secs", &self.timeout_secs)
            .field("allow_insecure", &self.allow_insecure)
            .field("dir", &self.dir)
            .field("max_mb", &self.max_mb)
            .field("stream_operational", &self.stream_operational)
            .field("stream_activity", &self.stream_activity)
            .field("stream_traces", &self.stream_traces)
            .field("stream_metrics", &self.stream_metrics)
            .field("conversation_content", &self.conversation_content)
            .field("conversation_thinking", &self.conversation_thinking)
            .field("trace_sample_ratio", &self.trace_sample_ratio)
            .field("max_attribute_bytes", &self.max_attribute_bytes)
            .field("max_content_bytes", &self.max_content_bytes)
            .field("max_trace_bytes", &self.max_trace_bytes)
            .field("repo_names", &self.repo_names)
            .field("prometheus", &self.prometheus)
            .field("max_backlog_days", &self.max_backlog_days)
            .field("max_read_mib_per_sec", &self.max_read_mib_per_sec)
            .field("start_from", &self.start_from)
            .field("headers", &headers)
            .finish()
    }
}

/// The rules a save of this kind must pass, called from `modules::update` with two views of the
/// settings: `stored`, what the module held before the save, and `effective`, what it would hold
/// after (the request's settings over that). `enabled` is the module's state after the save.
/// `confirm_content` is the request's `confirm_content` field, which is never persisted.
///
/// The content confirmation is required only for a switch that this save *turns on*, not for every
/// later save while it stays on: the cockpit saves back everything it holds, so a rule that
/// demanded the word again would refuse every unrelated edit once a switch was on.
pub fn validate(
    provider: &str,
    stored: &Map<String, Value>,
    effective: &Map<String, Value>,
    enabled: bool,
    confirm_content: bool,
) -> Result<(), String> {
    // Per switch: turning `conversation_thinking` on while `conversation_content` is already on is
    // still a new stream of what the agent said, so it needs the word too.
    for key in ["conversation_content", "conversation_thinking"] {
        if turns_on(stored, effective, key) && !confirm_content {
            return Err(
                "conversation content or thinking is being turned on, which sends what you and the agent actually said; \
                 confirm it by sending \"confirm_content\": true in the same save"
                    .into(),
            );
        }
    }
    let settings = effective;
    if provider != OTLP {
        return Ok(());
    }
    let protocol = str_at(settings, "protocol", "http/protobuf");
    let preset = str_at(settings, "preset", "custom");
    if protocol == "grpc" {
        if preset == "grafana_cloud" {
            return Err("Grafana Cloud's OTLP gateway does not take gRPC from this build; use http/protobuf".into());
        }
        // #851 adds the `otlp-grpc` feature and a transport to build on; until it lands there is
        // nothing to send gRPC with, whatever the endpoint.
        return Err("this build has no gRPC; use http/protobuf".into());
    }
    let endpoint = str_at(settings, "endpoint", "");
    if endpoint.is_empty() {
        // An enabled exporter with nowhere to send is a silent no-op; refuse it, but let a
        // disabled module keep an empty endpoint so the first save of the settings works.
        if enabled {
            return Err("an enabled OTLP module needs an endpoint; set the endpoint or switch the module off".into());
        }
    } else {
        check_endpoint(&endpoint, bool_at(settings, "allow_insecure", false))?;
    }
    Ok(())
}

/// The endpoint rules: a http(s) URL, no credentials in it, no query string, and a plain `http://`
/// only to a loopback or private host unless `allow_insecure` says otherwise.
pub(crate) fn check_endpoint(endpoint: &str, allow_insecure: bool) -> Result<(), String> {
    let (scheme, rest) = if let Some(r) = endpoint.strip_prefix("https://") {
        ("https", r)
    } else if let Some(r) = endpoint.strip_prefix("http://") {
        ("http", r)
    } else {
        return Err("endpoint must be a http:// or https:// URL".into());
    };
    // The authority runs from after the scheme to the first `/`, `?` or `#`; a query is refused
    // outright, so its own `?` never has to be reasoned about.
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return Err(
            "endpoint must carry no credentials (user:pass@ or user@); put them in the observability-headers secret".into(),
        );
    }
    if endpoint.contains('?') {
        return Err("endpoint must carry no query string; credentials belong in the observability-headers secret".into());
    }
    if authority.is_empty() {
        return Err("endpoint must name a host".into());
    }
    let host = if let Some(v6) = authority.strip_prefix('[') {
        v6.split(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or("")
    };
    if scheme == "http" && !allow_insecure && !is_private_host(host) {
        return Err(format!(
            "a plain http:// endpoint to {host} is neither loopback nor private, so the export would travel unencrypted; \
             use https:// or set allow_insecure"
        ));
    }
    Ok(())
}

/// Whether a switch on an IPv4-mapped IPv6 host counts as private: unwrap `::ffff:` to the IPv4
/// rules rather than treating the address as a plain (public) v6 one.
fn is_private_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v4)) => is_private_v4(v4),
        Ok(std::net::IpAddr::V6(v6)) => match v6.to_ipv4_mapped() {
            Some(v4) => is_private_v4(v4),
            // fc00::/7, unique local addresses: no std predicate, so the prefix is checked directly.
            None => v6.is_loopback() || (v6.segments()[0] & 0xfe00) == 0xfc00,
        },
        Err(_) => false,
    }
}

fn is_private_v4(v4: std::net::Ipv4Addr) -> bool {
    v4.is_loopback() || v4.is_private() || v4.is_link_local()
}

/// Whether a save turns `key` from off to on: `effective` has it true where `stored` had it false
/// or absent.
fn turns_on(stored: &Map<String, Value>, effective: &Map<String, Value>, key: &str) -> bool {
    bool_at(effective, key, false) && !bool_at(stored, key, false)
}

fn bool_at(settings: &Map<String, Value>, key: &str, default: bool) -> bool {
    settings.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn str_at(settings: &Map<String, Value>, key: &str, default: &str) -> String {
    settings.get(key).and_then(Value::as_str).unwrap_or(default).to_string()
}

fn u64_at(settings: &Map<String, Value>, key: &str, default: u64) -> u64 {
    settings.get(key).and_then(Value::as_u64).unwrap_or(default)
}

fn f64_at(settings: &Map<String, Value>, key: &str, default: f64) -> f64 {
    settings.get(key).and_then(Value::as_f64).unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(pairs: &[(&str, Value)]) -> Map<String, Value> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect()
    }

    #[test]
    fn both_providers_declare_the_shared_settings_with_their_defaults() {
        let ids: Vec<_> = providers().into_iter().map(|p| p.id).collect();
        assert_eq!(ids, vec!["otlp".to_string(), "file".to_string()]);
        for provider in providers() {
            let props = &provider.schema["properties"];
            for (key, default) in [
                ("stream_operational", json!(true)),
                ("stream_activity", json!(true)),
                ("stream_traces", json!(true)),
                ("stream_metrics", json!(true)),
                ("conversation_content", json!(false)),
                ("conversation_thinking", json!(false)),
                ("trace_sample_ratio", json!(1)),
                ("max_attribute_bytes", json!(1024)),
                ("max_content_bytes", json!(32768)),
                ("max_trace_bytes", json!(4194304)),
                ("repo_names", json!("plain")),
                ("prometheus", json!(false)),
                ("max_backlog_days", json!(7)),
                ("max_read_mib_per_sec", json!(8)),
                ("start_from", json!("now")),
            ] {
                assert_eq!(props[key]["default"], default, "{}: {key}", provider.id);
            }
        }
        let otlp = &providers().remove(0).schema["properties"];
        assert_eq!(otlp["protocol"]["default"], json!("http/protobuf"));
        assert_eq!(otlp["preset"]["default"], json!("custom"));
        assert_eq!(otlp["allow_insecure"]["default"], json!(false));
        let file = &providers().remove(1).schema["properties"];
        assert_eq!(file["max_mb"]["default"], json!(512));
    }

    #[test]
    fn a_plain_http_endpoint_is_private_only_unless_insecure_is_allowed() {
        assert!(check_endpoint("http://localhost:4318", false).is_ok());
        assert!(check_endpoint("http://127.0.0.1:4318", false).is_ok());
        assert!(check_endpoint("http://10.0.0.5:4318", false).is_ok());
        assert!(check_endpoint("http://172.16.4.4:4318", false).is_ok());
        assert!(check_endpoint("http://192.168.1.9:4318", false).is_ok());
        assert!(check_endpoint("http://[::1]:4318", false).is_ok());
        assert!(check_endpoint("http://[fd00::1]:4318", false).is_ok());
        assert!(check_endpoint("http://169.254.1.1:4318", false).is_ok());
        // An IPv4-mapped IPv6 address takes the IPv4 rules, not the v6 (public) ones.
        assert!(check_endpoint("http://[::ffff:127.0.0.1]:4318", false).is_ok());
        assert!(check_endpoint("http://[::ffff:10.0.0.5]:4318", false).is_ok());
        assert!(
            check_endpoint("https://otlp.example.com", false).is_ok(),
            "https to a public host is fine"
        );

        let err = check_endpoint("http://otlp.example.com:4318", false).unwrap_err();
        assert!(err.contains("allow_insecure"), "{err}");
        let err = check_endpoint("http://[::ffff:8.8.8.8]:4318", false).unwrap_err();
        assert!(err.contains("allow_insecure"), "a mapped public v4 is still public: {err}");
        assert!(
            check_endpoint("http://otlp.example.com:4318", true).is_ok(),
            "the switch opens it"
        );

        assert!(check_endpoint("ftp://example.com", false).is_err());
        let err = check_endpoint("https://user:pass@example.com", false).unwrap_err();
        assert!(err.contains("observability-headers"), "{err}");
        let err = check_endpoint("https://example.com?token=abc", false).unwrap_err();
        assert!(err.contains("observability-headers"), "{err}");
    }

    #[test]
    fn grpc_is_refused_and_grafana_cloud_has_its_own_message() {
        let stored = Map::new();
        let err = validate(OTLP, &stored, &settings(&[("protocol", json!("grpc"))]), true, false).unwrap_err();
        assert!(err.contains("no gRPC"), "{err}");
        let err = validate(
            OTLP,
            &stored,
            &settings(&[("preset", json!("grafana_cloud")), ("protocol", json!("grpc"))]),
            true,
            false,
        )
        .unwrap_err();
        assert!(err.contains("Grafana Cloud"), "{err}");
        assert!(
            !err.contains("this build has no gRPC"),
            "the preset's message comes first: {err}"
        );
    }

    #[test]
    fn an_enabled_otlp_module_needs_an_endpoint() {
        let stored = Map::new();
        let err = validate(OTLP, &stored, &Map::new(), true, false).unwrap_err();
        assert!(err.contains("endpoint"), "{err}");
        // Disabled with no endpoint is fine: the first save of the settings writes the module before
        // the operator has an endpoint, and nothing is exported while it is off.
        assert!(validate(OTLP, &stored, &Map::new(), false, false).is_ok());
        // The file provider has no endpoint to need.
        assert!(validate(FILE, &stored, &settings(&[("max_mb", json!(128))]), true, false).is_ok());
    }

    #[test]
    fn content_needs_a_confirmation_only_for_the_save_that_turns_it_on() {
        let endpoint = ("endpoint", json!("https://otlp.example.com"));
        let off = settings(std::slice::from_ref(&endpoint));
        let on = settings(&[endpoint.clone(), ("conversation_content", json!(true))]);
        let err = validate(OTLP, &off, &on, true, false).unwrap_err();
        assert!(err.contains("confirm_content"), "{err}");
        assert!(validate(OTLP, &off, &on, true, true).is_ok(), "the word turns it on");
        // Already on and edited without the word: the cockpit saves back what it holds, so this
        // must pass rather than brick every later save.
        assert!(validate(OTLP, &on, &on, true, false).is_ok());
        // Turning it off never needs the word.
        assert!(validate(OTLP, &on, &off, true, false).is_ok());

        let thinking = settings(&[endpoint.clone(), ("conversation_thinking", json!(true))]);
        assert!(validate(OTLP, &off, &thinking, true, false).is_err());
        assert!(validate(OTLP, &off, &thinking, true, true).is_ok());

        // The word is per switch: content already on, thinking turned on now, still needs it.
        let content_then_thinking = settings(&[
            endpoint.clone(),
            ("conversation_content", json!(true)),
            ("conversation_thinking", json!(true)),
        ]);
        assert!(validate(OTLP, &on, &content_then_thinking, true, false).is_err());
        assert!(validate(OTLP, &on, &content_then_thinking, true, true).is_ok());
    }

    #[test]
    fn from_module_applies_defaults_and_honours_the_switch() {
        let off = ModuleChoice {
            provider: OTLP.into(),
            enabled: false,
            settings: Map::new(),
        };
        assert!(
            ExporterConfig::from_module(&off).is_none(),
            "a switched-off module exports nothing"
        );

        let on = ModuleChoice {
            provider: OTLP.into(),
            enabled: true,
            settings: settings(&[("endpoint", json!("https://otlp.example.com")), ("timeout_secs", json!(3))]),
        };
        let cfg = ExporterConfig::from_module(&on).unwrap();
        assert_eq!(cfg.endpoint, "https://otlp.example.com");
        assert_eq!(cfg.timeout_secs, 3);
        assert_eq!(cfg.protocol, "http/protobuf", "an absent key takes its default");
        assert!(cfg.stream_operational && !cfg.conversation_content);
        assert_eq!(cfg.max_content_bytes, 32768);
        assert!(cfg.headers.is_empty(), "this build reads no secret");
    }

    #[test]
    fn debug_names_headers_but_never_their_values() {
        let choice = ModuleChoice {
            provider: OTLP.into(),
            enabled: true,
            settings: Map::new(),
        };
        let mut cfg = ExporterConfig::from_module(&choice).unwrap();
        // A value built at runtime: no credential-looking literal lives in the source.
        let secret = format!("canary-{}", std::process::id());
        cfg.headers.push(("x-honeycomb-team".into(), secret.clone()));
        let shown = format!("{cfg:?}");
        assert!(!shown.contains(&secret), "the header value must not appear: {shown}");
        assert!(shown.contains("x-honeycomb-team=<redacted>"), "{shown}");
    }
}
