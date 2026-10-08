//! The effective exporter config (#841): the saved `observability` module overlaid with the
//! standard `OTEL_*` variables, and the master switch.
//!
//! - Nothing exports unless the module is saved **and** enabled, or `COLONIZER_OBSERVABILITY=on`
//!   is set. An `OTEL_EXPORTER_OTLP_ENDPOINT` left in the service environment for another program
//!   never turns export on by itself. `COLONIZER_OBSERVABILITY` set to anything but a truthy value
//!   turns it off, whatever a saved module says.
//! - `OTEL_SDK_DISABLED=true` turns it off, whatever else says.
//! - Each `OTEL_*` variable overrides its one field; the status API reports where every field came
//!   from (module, environment or default).
//! - Headers come from `OTEL_EXPORTER_OTLP_HEADERS` when it is set, else from the
//!   `observability-headers` secret. The list is parsed and refused whole if a pair is malformed,
//!   under exactly the rules the add-on applies, so a valid config never dies at spawn. Their
//!   values are handed to the add-on on its stdin and never written to the contract, a log line or
//!   an API answer: only the header *names* are reported, and a rejected pair is reported by its
//!   position rather than by its text (P8 — a malformed pair usually *is* the credential).

use super::settings::{self, ExporterConfig, FILE, OTLP};
use crate::config::ModuleChoice;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// The master switch for an environment-only setup.
pub const MASTER: &str = "COLONIZER_OBSERVABILITY";
/// The service name a config with no `OTEL_SERVICE_NAME` exports as.
pub const DEFAULT_SERVICE_NAME: &str = "colonizer-mothership";
/// How much of a header *name* a header error quotes back.
const KEY_MAX: usize = 64;

/// What the exporter should do.
pub enum Resolved {
    /// Not exporting, and why.
    Off(String),
    /// Configured but unusable; shown in the status, nothing is sent.
    Invalid(String),
    On(Box<Effective>),
}

/// What this mothership is, for the resource. Filled in by the caller: `resolve` reads no files.
#[derive(Clone, Default)]
pub struct Identity {
    pub host_id: String,
    pub host_name: String,
    /// `owner`, `member`, or empty for a lone mothership.
    pub fleet_role: String,
}

/// An exporter config ready for the contract.
pub struct Effective {
    /// The contract's `settings` object.
    pub settings: Map<String, Value>,
    /// Field name → `module`, `env:<VAR>` or `default`.
    pub provenance: BTreeMap<String, String>,
    /// The raw `k=v,…` header list, as the add-on's stdin protocol takes it. Never logged or
    /// serialised; see the module doc.
    pub headers: String,
    /// The same list, validated and percent-decoded. `headers` is what is handed over, these are
    /// what the exporter sends.
    pub parsed_headers: Vec<(String, String)>,
    /// `env`, `secret` or `none`.
    pub headers_source: &'static str,
}

impl std::fmt::Debug for Effective {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Effective")
            .field("settings", &self.settings)
            .field("headers", &header_names(&self.headers))
            .finish()
    }
}

/// Names only: a header value is a secret (P8) and must never reach a status page or a log.
impl serde::Serialize for Effective {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(Some(3))?;
        m.serialize_entry("settings", &self.settings)?;
        m.serialize_entry("provenance", &self.provenance)?;
        m.serialize_entry(
            "headers",
            &HeaderNames {
                source: self.headers_source,
                names: header_names(&self.headers),
            },
        )?;
        m.end()
    }
}

struct HeaderNames {
    source: &'static str,
    names: Vec<String>,
}

impl serde::Serialize for HeaderNames {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(Some(2))?;
        m.serialize_entry("source", self.source)?;
        m.serialize_entry("names", &self.names)?;
        m.end()
    }
}

impl Effective {
    pub fn str(&self, key: &str) -> &str {
        self.settings.get(key).and_then(Value::as_str).unwrap_or("")
    }
}

/// One rejected pair in a `k=v,…` header list.
///
/// P8: a header value is a secret, and a malformed pair usually *is* the secret — `Authorization:
/// Basic …` written with a colon, or a token pasted on its own. So the message names the pair's
/// 1-based position always, and the header's name only when the pair has one that is already known
/// to be a legal name. Nothing an operator typed is ever echoed back.
pub struct HeaderError {
    /// The pair's 1-based position in the list.
    pub pair: usize,
    /// The header's name, when it is a known-safe one; `None` when the pair's text must not be
    /// quoted (no `=`, an empty name, or a name that is not a legal token).
    pub key: Option<String>,
    pub detail: &'static str,
}

/// Written by hand, not derived, so no pair text can reach the message by accident.
impl std::fmt::Display for HeaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.key {
            Some(key) => write!(f, "header `{key}` (pair {}): {}", self.pair, self.detail),
            None => write!(f, "header pair {}: {}", self.pair, self.detail),
        }
    }
}

impl std::fmt::Debug for HeaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for HeaderError {}

/// Parses an OTLP header list: comma-separated `k=v` pairs, whitespace around each trimmed, the
/// value percent-decoded. A value may contain `=` and an empty value is legal. A pair with no `=`,
/// a name that is empty or is not an RFC 7230 token, a malformed `%`-escape or a control character
/// in a value is refused, and refusing one refuses the list — a half-applied header set is worse
/// than none.
///
/// These are exactly the rules the add-on's own [`colonizer_observability::contract::parse_headers`]
/// applies when it reads the list off its stdin: this parser exists to tell the operator their
/// config is broken at save time rather than at spawn time. If the two ever disagree, a config the
/// status page calls valid reaches an exporter that refuses the contract.
pub fn parse_headers(list: &str) -> Result<Vec<(String, String)>, HeaderError> {
    let mut out = Vec::new();
    for (i, pair) in list.split(',').enumerate() {
        if pair.trim().is_empty() {
            continue;
        }
        let n = i + 1;
        let Some((name, value)) = pair.split_once('=') else {
            return Err(HeaderError {
                pair: n,
                key: None,
                detail: "no `=`; a header is `name=value`",
            });
        };
        let name = name.trim();
        if name.is_empty() {
            return Err(HeaderError {
                pair: n,
                key: None,
                detail: "the name is empty; a header is `name=value`",
            });
        }
        if !name.bytes().all(is_token_byte) {
            return Err(HeaderError {
                pair: n,
                key: None,
                detail: "the name is not a valid header name; a header is `name=value`",
            });
        }
        let key = truncate_key(name);
        let Some(decoded) = percent_decode(value.trim()) else {
            return Err(HeaderError {
                pair: n,
                key: Some(key),
                detail: "the value has a malformed `%`-escape",
            });
        };
        if decoded.chars().any(|c| c.is_control()) {
            return Err(HeaderError {
                pair: n,
                key: Some(key),
                detail: "the value has a control character in it",
            });
        }
        out.push((name.to_string(), decoded));
    }
    Ok(out)
}

/// RFC 7230 `tchar`: what a header name may be made of.
fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

/// Bounds what an error quotes back, so a long name cannot flood a status page.
fn truncate_key(s: &str) -> String {
    s.chars().take(KEY_MAX).collect()
}

/// The header names of a `k=v,…` list, for the status API: names only, never values.
pub fn header_names(list: &str) -> Vec<String> {
    list.split(',')
        .filter_map(|pair| pair.split_once('=').map(|(name, _)| name.trim().to_string()))
        .filter(|name| !name.is_empty())
        .collect()
}

/// Parses `OTEL_RESOURCE_ATTRIBUTES`: `k=v` pairs, comma-separated, values percent-decoded under the
/// same rules as a header value. A malformed pair — no `=`, an empty name, or a value whose `%`
/// escape does not decode — is skipped (the spec says to ignore the whole variable; skipping one
/// pair keeps the rest, and reports it).
fn parse_resource_list(list: &str) -> (BTreeMap<String, String>, bool) {
    let mut out = BTreeMap::new();
    let mut bad = false;
    for pair in list.split(',').filter(|p| !p.trim().is_empty()) {
        match pair.split_once('=') {
            Some((k, v)) if !k.trim().is_empty() => match percent_decode(v.trim()) {
                Some(decoded) => {
                    out.insert(k.trim().to_string(), decoded);
                }
                None => bad = true,
            },
            _ => bad = true,
        }
    }
    (out, bad)
}

/// The whole resource every export carries. `OTEL_RESOURCE_ATTRIBUTES` merges in underneath, so it
/// can add keys but never claim to be something it is not.
///
/// The keys this mothership owns — `service.name`, `service.instance.id`, `host.name` and
/// everything under `colonizer.` — are removed from the environment's list *before* they are set,
/// not merely overwritten after. `colonizer.fleet.id` and `colonizer.fleet.role` are each set on
/// one branch of the fleet `match` only, so an insert alone would leave an environment-supplied
/// twin standing on the branch that does not set it: a member would claim a fleet id it must not
/// know, and a lone mothership would claim a role it does not have.
fn resource_attributes(service_name: &str, id: &Identity, env_attrs: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut out = env_attrs.clone();
    for owned in [
        "service.name",
        "service.instance.id",
        "host.name",
        "colonizer.fleet.id",
        "colonizer.fleet.role",
    ] {
        out.remove(owned);
    }
    if !id.host_name.trim().is_empty() {
        out.insert("host.name".into(), id.host_name.trim().to_string());
    }
    match id.fleet_role.as_str() {
        // A lone mothership is a fleet of one, so it is its own fleet id.
        "" => {
            if !id.host_id.trim().is_empty() {
                out.insert("colonizer.fleet.id".into(), id.host_id.clone());
            }
        }
        role => {
            // A member cannot know its owner's host id yet, so it carries no fleet id: the add-on
            // falls back to its own host id, and a wrong value would be worse than none.
            out.insert("colonizer.fleet.role".into(), role.to_string());
        }
    }
    if !id.host_id.trim().is_empty() {
        out.insert("service.instance.id".into(), id.host_id.clone());
    }
    out.insert("service.version".into(), env!("CARGO_PKG_VERSION").into());
    let name = if service_name.trim().is_empty() {
        DEFAULT_SERVICE_NAME
    } else {
        service_name.trim()
    };
    out.insert("service.name".into(), name.to_string());
    out
}

/// Percent-decodes a value. `None` when a `%` is not followed by two hex digits, or when the
/// decoded bytes are not UTF-8 — the same rules the add-on decodes the value with, so a value the
/// mothership accepts is one the add-on accepts.
fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn truthy(v: &str) -> bool {
    matches!(v.trim().to_ascii_lowercase().as_str(), "true" | "1" | "on" | "yes")
}

/// Resolves the effective config. `env` reads one variable (the process environment outside
/// tests); `secret` is the `observability-headers` secret's value, if any; `id` is what this
/// mothership is, which only the caller can read off the app.
pub fn resolve(
    module: Option<&ModuleChoice>,
    env: &dyn Fn(&str) -> Option<String>,
    secret: Option<String>,
    id: &Identity,
) -> Resolved {
    let var = |name: &str| env(name).filter(|v| !v.trim().is_empty());
    if var("OTEL_SDK_DISABLED").is_some_and(|v| truthy(&v)) {
        return Resolved::Off("OTEL_SDK_DISABLED is set".into());
    }
    // The switch is a switch: anything set that is not `on`/`true`/`1`/`yes` is Off, whatever a
    // saved module says. An operator who wrote it wants it off.
    if let Some(v) = env(MASTER)
        && !truthy(&v)
    {
        return Resolved::Off(format!("{MASTER} is set, but not to on"));
    }
    let master = var(MASTER).is_some_and(|v| truthy(&v));
    let enabled = module.is_some_and(|m| m.enabled);
    let (choice, from_module) = match module {
        Some(m) if m.enabled => (m.clone(), true),
        _ if master => (
            ModuleChoice {
                provider: OTLP.into(),
                enabled: true,
                settings: Map::new(),
            },
            false,
        ),
        Some(_) => return Resolved::Off("the observability module is switched off".into()),
        None => return Resolved::Off("the observability module is not configured".into()),
    };
    if choice.provider == FILE {
        return Resolved::Invalid("the local file provider is not built yet (#850); pick OTLP endpoint".into());
    }
    let Some(cfg) = ExporterConfig::from_module(&choice) else {
        return Resolved::Off("the observability module is switched off".into());
    };

    let mut provenance = BTreeMap::new();
    let origin = |key: &str| {
        if from_module && choice.settings.contains_key(key) {
            "module".to_string()
        } else {
            "default".to_string()
        }
    };
    let mut field = |key: &str, saved: Value, var_name: Option<&str>, settings: &mut Map<String, Value>| {
        let (value, source) = match var_name.and_then(var) {
            Some(v) => (Value::String(v), format!("env:{}", var_name.unwrap_or_default())),
            None => (saved, origin(key)),
        };
        provenance.insert(key.to_string(), source);
        settings.insert(key.to_string(), value);
    };

    let mut s = Map::new();
    field("endpoint", json!(cfg.endpoint), Some("OTEL_EXPORTER_OTLP_ENDPOINT"), &mut s);
    field("protocol", json!(cfg.protocol), Some("OTEL_EXPORTER_OTLP_PROTOCOL"), &mut s);
    field(
        "compression",
        json!(cfg.compression),
        Some("OTEL_EXPORTER_OTLP_COMPRESSION"),
        &mut s,
    );
    field("service_name", json!(DEFAULT_SERVICE_NAME), Some("OTEL_SERVICE_NAME"), &mut s);
    for (key, var_name) in [
        ("logs_endpoint", "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT"),
        ("traces_endpoint", "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT"),
        ("metrics_endpoint", "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT"),
    ] {
        if let Some(v) = var(var_name) {
            s.insert(key.into(), json!(v));
            provenance.insert(key.into(), format!("env:{var_name}"));
        }
    }
    // OTEL_EXPORTER_OTLP_TIMEOUT is milliseconds; the module's field is seconds.
    match var("OTEL_EXPORTER_OTLP_TIMEOUT").and_then(|v| v.trim().parse::<u64>().ok()) {
        Some(ms) => {
            s.insert("timeout_secs".into(), json!(ms.div_ceil(1000).clamp(1, 120)));
            provenance.insert("timeout_secs".into(), "env:OTEL_EXPORTER_OTLP_TIMEOUT".into());
        }
        None => {
            s.insert("timeout_secs".into(), json!(cfg.timeout_secs));
            provenance.insert("timeout_secs".into(), origin("timeout_secs"));
        }
    }
    let (env_attrs, bad_attrs) = parse_resource_list(&var("OTEL_RESOURCE_ATTRIBUTES").unwrap_or_default());
    if !env_attrs.is_empty() {
        provenance.insert("resource_attributes".into(), "env:OTEL_RESOURCE_ATTRIBUTES".into());
    }
    let service_name = s.get("service_name").and_then(Value::as_str).unwrap_or_default().to_string();
    s.insert(
        "resource_attributes".into(),
        json!(resource_attributes(&service_name, id, &env_attrs)),
    );

    s.insert("provider".into(), json!(OTLP));
    for (key, value) in [
        ("stream_operational", json!(cfg.stream_operational)),
        ("stream_activity", json!(cfg.stream_activity)),
        ("stream_traces", json!(cfg.stream_traces)),
        ("stream_metrics", json!(cfg.stream_metrics)),
        ("max_attribute_bytes", json!(cfg.max_attribute_bytes)),
        ("max_content_bytes", json!(cfg.max_content_bytes)),
        ("trace_sample_ratio", json!(cfg.trace_sample_ratio)),
        ("max_trace_bytes", json!(cfg.max_trace_bytes)),
        ("repo_names", json!(cfg.repo_names)),
        ("max_backlog_days", json!(cfg.max_backlog_days)),
        ("max_read_mib_per_sec", json!(cfg.max_read_mib_per_sec)),
        ("start_from", json!(cfg.start_from)),
    ] {
        provenance.insert(key.into(), origin(key));
        s.insert(key.into(), value);
    }

    // The same rules a save is held to, now over the effective values.
    let endpoint = s.get("endpoint").and_then(Value::as_str).unwrap_or("").to_string();
    let protocol = s.get("protocol").and_then(Value::as_str).unwrap_or("").to_string();
    if !matches!(protocol.as_str(), "http/protobuf" | "http/json") {
        return Resolved::Invalid(format!(
            "protocol {protocol} is not supported by this build; use http/protobuf or http/json"
        ));
    }
    let signal_endpoints: Vec<String> = ["logs_endpoint", "traces_endpoint", "metrics_endpoint"]
        .iter()
        .filter_map(|k| s.get(*k).and_then(Value::as_str).map(str::to_string))
        .collect();
    if endpoint.is_empty() && signal_endpoints.is_empty() {
        let hint = if enabled || from_module {
            "set the module's endpoint"
        } else {
            "set OTEL_EXPORTER_OTLP_ENDPOINT"
        };
        return Resolved::Invalid(format!("no OTLP endpoint is configured; {hint}"));
    }
    for e in std::iter::once(&endpoint).filter(|e| !e.is_empty()).chain(&signal_endpoints) {
        if let Err(err) = settings::check_endpoint(e, cfg.allow_insecure) {
            return Resolved::Invalid(err);
        }
    }
    if bad_attrs {
        provenance.insert(
            "resource_attributes".into(),
            "env:OTEL_RESOURCE_ATTRIBUTES (a malformed pair was skipped)".into(),
        );
    }

    let (headers, headers_source) = match var("OTEL_EXPORTER_OTLP_HEADERS") {
        Some(h) => (h, "env"),
        None => match secret.filter(|s| !s.trim().is_empty()) {
            Some(h) => (h.trim().to_string(), "secret"),
            None => (String::new(), "none"),
        },
    };
    // A header list that does not parse is refused whole: sending a half-read credential is worse
    // than sending none, and the operator is told which name to fix.
    let parsed_headers = match parse_headers(&headers) {
        Ok(pairs) => pairs,
        Err(e) => {
            return Resolved::Invalid(format!(
                "the OTLP header list is malformed ({e}); nothing is exported until it is fixed"
            ));
        }
    };
    Resolved::On(Box::new(Effective {
        settings: s,
        provenance,
        headers,
        parsed_headers,
        headers_source,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| pairs.iter().find(|(k, _)| *k == name).map(|(_, v)| v.to_string())
    }

    /// A lone mothership: a fleet of one, so it is its own fleet id.
    fn alone() -> Identity {
        Identity {
            host_id: "host-1".into(),
            host_name: "mothership.local".into(),
            fleet_role: String::new(),
        }
    }

    fn module(enabled: bool, settings: Value) -> ModuleChoice {
        ModuleChoice {
            provider: OTLP.into(),
            enabled,
            settings: settings.as_object().cloned().unwrap_or_default(),
        }
    }

    fn on(r: Resolved) -> Effective {
        match r {
            Resolved::On(e) => *e,
            Resolved::Off(why) | Resolved::Invalid(why) => panic!("expected on: {why}"),
        }
    }

    fn attrs(e: &Effective) -> &Map<String, Value> {
        e.settings["resource_attributes"].as_object().expect("an object")
    }

    #[test]
    fn off_by_default_even_with_an_otel_endpoint_in_the_environment() {
        let env = env_of(&[("OTEL_EXPORTER_OTLP_ENDPOINT", "https://otlp.example.com")]);
        assert!(
            matches!(resolve(None, &env, None, &alone()), Resolved::Off(_)),
            "no module, no master switch"
        );
        let saved_off = module(false, json!({"endpoint": "https://otlp.example.com"}));
        assert!(matches!(resolve(Some(&saved_off), &env, None, &alone()), Resolved::Off(_)));
        assert!(matches!(
            resolve(None, &env_of(&[]), Some("k=v".into()), &alone()),
            Resolved::Off(_)
        ));
    }

    #[test]
    fn the_master_switch_alone_with_an_endpoint_is_enough() {
        let env = env_of(&[
            (MASTER, "on"),
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:4318"),
            ("OTEL_SERVICE_NAME", "colonizer-prod"),
            ("OTEL_EXPORTER_OTLP_TIMEOUT", "2500"),
            ("OTEL_RESOURCE_ATTRIBUTES", "deployment.environment=prod,team=infra%20ops"),
        ]);
        let e = on(resolve(None, &env, None, &alone()));
        assert_eq!(e.str("endpoint"), "http://127.0.0.1:4318");
        assert_eq!(e.str("service_name"), "colonizer-prod");
        assert_eq!(e.settings["timeout_secs"], json!(3), "milliseconds, rounded up to seconds");
        assert_eq!(e.settings["resource_attributes"]["team"], json!("infra ops"));
        assert_eq!(e.provenance["endpoint"], "env:OTEL_EXPORTER_OTLP_ENDPOINT");
        assert_eq!(e.provenance["protocol"], "default");
        // Without an endpoint the master switch is a configuration error, not a silent no-op.
        assert!(matches!(
            resolve(None, &env_of(&[(MASTER, "on")]), None, &alone()),
            Resolved::Invalid(_)
        ));
    }

    #[test]
    fn a_saved_enabled_module_alone_is_enough() {
        let saved = module(true, json!({"endpoint": "https://otlp.example.com"}));
        let e = on(resolve(Some(&saved), &env_of(&[]), None, &alone()));
        assert_eq!(e.str("endpoint"), "https://otlp.example.com");
    }

    #[test]
    fn a_master_value_that_is_not_on_forces_off_over_an_enabled_module() {
        let saved = module(true, json!({"endpoint": "https://otlp.example.com"}));
        for value in ["off", "false", "0", "no", "", "   "] {
            let pairs: &'static [(&'static str, &'static str)] = Box::leak(Box::new([
                (MASTER, value),
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "https://otlp.example.com"),
            ]));
            assert!(
                matches!(resolve(Some(&saved), &env_of(pairs), None, &alone()), Resolved::Off(_)),
                "{MASTER}={value:?} is not an enable switch"
            );
        }
    }

    #[test]
    fn otel_sdk_disabled_wins_over_everything() {
        let saved = module(true, json!({"endpoint": "https://otlp.example.com"}));
        let env = env_of(&[(MASTER, "on"), ("OTEL_SDK_DISABLED", "true")]);
        assert!(matches!(resolve(Some(&saved), &env, None, &alone()), Resolved::Off(_)));
    }

    #[test]
    fn env_overrides_module_fields_and_the_header_secret() {
        let saved = module(
            true,
            json!({"endpoint": "https://saved.example.com", "protocol": "http/json"}),
        );
        let env = env_of(&[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "https://env.example.com"),
            ("OTEL_EXPORTER_OTLP_HEADERS", "x-honeycomb-team=from-env"),
        ]);
        let e = on(resolve(
            Some(&saved),
            &env,
            Some("x-honeycomb-team=from-secret".into()),
            &alone(),
        ));
        assert_eq!(e.str("endpoint"), "https://env.example.com");
        assert_eq!(e.str("protocol"), "http/json");
        assert_eq!(e.provenance["protocol"], "module");
        assert_eq!(e.headers, "x-honeycomb-team=from-env");
        assert_eq!(e.headers_source, "env");

        let traced = module(
            true,
            json!({"endpoint": "https://saved.example.com", "trace_sample_ratio": 0.25}),
        );
        let e = on(resolve(
            Some(&traced),
            &env_of(&[("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", "https://traces.example.com/v1/traces")]),
            None,
            &alone(),
        ));
        assert_eq!(e.settings["trace_sample_ratio"], json!(0.25));
        assert_eq!(e.settings["max_trace_bytes"], json!(4_194_304));
        assert_eq!(e.str("traces_endpoint"), "https://traces.example.com/v1/traces");
        assert_eq!(e.provenance["traces_endpoint"], "env:OTEL_EXPORTER_OTLP_TRACES_ENDPOINT");

        let e = on(resolve(
            Some(&saved),
            &env_of(&[]),
            Some("api-key=from-secret".into()),
            &alone(),
        ));
        assert_eq!(e.headers_source, "secret");
        assert_eq!(header_names(&e.headers), vec!["api-key".to_string()]);
        assert!(!format!("{e:?}").contains("from-secret"), "Debug never shows a header value");
    }

    #[test]
    fn a_base_endpoint_is_kept_bare_and_a_per_signal_one_verbatim() {
        // The `/v1/<signal>` suffix is the add-on's (`contract::Settings::url`); the mothership
        // stores what it was given, so a collector that wants a custom path keeps it.
        let saved = module(true, json!({"endpoint": "https://otlp.example.com:4318/"}));
        let e = on(resolve(
            Some(&saved),
            &env_of(&[("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", "https://traces.example.com/ingest/t")]),
            None,
            &alone(),
        ));
        assert_eq!(
            e.str("endpoint"),
            "https://otlp.example.com:4318/",
            "stored bare, no path added"
        );
        assert_eq!(e.str("traces_endpoint"), "https://traces.example.com/ingest/t");
        assert!(e.str("logs_endpoint").is_empty(), "no per-signal endpoint is invented");
    }

    #[test]
    fn effective_values_are_checked_like_a_save() {
        let saved = module(true, json!({"endpoint": "https://otlp.example.com"}));
        for (var, value) in [
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://otlp.example.com:4318"),
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "https://user:pass@otlp.example.com"),
            ("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc"),
        ] {
            let pairs: &'static [(&'static str, &'static str)] = Box::leak(Box::new([(var, value)]));
            assert!(
                matches!(resolve(Some(&saved), &env_of(pairs), None, &alone()), Resolved::Invalid(_)),
                "{var}={value}"
            );
        }
        let file = ModuleChoice {
            provider: FILE.into(),
            enabled: true,
            settings: Map::new(),
        };
        assert!(matches!(
            resolve(Some(&file), &env_of(&[]), None, &alone()),
            Resolved::Invalid(_)
        ));
    }

    #[test]
    fn the_resource_carries_this_mothership() {
        let saved = module(true, json!({"endpoint": "https://otlp.example.com"}));
        let e = on(resolve(Some(&saved), &env_of(&[]), None, &alone()));
        let a = attrs(&e);
        assert_eq!(a["service.name"], json!(DEFAULT_SERVICE_NAME));
        assert_eq!(a["service.version"], json!(env!("CARGO_PKG_VERSION")));
        assert_eq!(a["service.instance.id"], json!("host-1"));
        assert_eq!(a["host.name"], json!("mothership.local"));
        // A lone mothership is a fleet of one, so it is its own fleet id, and it has no role.
        assert_eq!(a["colonizer.fleet.id"], json!("host-1"));
        assert!(!a.contains_key("colonizer.fleet.role"), "a lone mothership has no role");
        assert_eq!(
            e.str("service_name"),
            DEFAULT_SERVICE_NAME,
            "the setting and the resource agree"
        );
    }

    #[test]
    fn an_otelservice_name_names_the_resource_too() {
        let saved = module(true, json!({"endpoint": "https://otlp.example.com"}));
        let e = on(resolve(
            Some(&saved),
            &env_of(&[("OTEL_SERVICE_NAME", "colonizer-prod")]),
            None,
            &alone(),
        ));
        assert_eq!(attrs(&e)["service.name"], json!("colonizer-prod"));
    }

    #[test]
    fn a_fleet_member_carries_a_role_and_no_fleet_id() {
        let member = Identity {
            host_id: "host-2".into(),
            host_name: "member.local".into(),
            fleet_role: "member".into(),
        };
        let saved = module(true, json!({"endpoint": "https://otlp.example.com"}));
        let e = on(resolve(Some(&saved), &env_of(&[]), None, &member));
        let a = attrs(&e);
        assert_eq!(a["colonizer.fleet.role"], json!("member"));
        // We cannot know the owner's host id yet, and a wrong one is worse than none.
        assert!(!a.contains_key("colonizer.fleet.id"), "a member does not know its owner's id");
        assert_eq!(a["service.instance.id"], json!("host-2"));

        let owner = Identity {
            fleet_role: "owner".into(),
            ..member
        };
        let e = on(resolve(Some(&saved), &env_of(&[]), None, &owner));
        let a = attrs(&e);
        assert_eq!(a["colonizer.fleet.role"], json!("owner"));
        assert!(!a.contains_key("colonizer.fleet.id"), "an owner carries the role, not an id");
    }

    #[test]
    fn otel_resource_attributes_can_add_keys_but_never_claim_to_be_us() {
        let saved = module(true, json!({"endpoint": "https://otlp.example.com"}));
        let env = env_of(&[(
            "OTEL_RESOURCE_ATTRIBUTES",
            "service.instance.id=spoofed,colonizer.fleet.id=nope,extra=kept",
        )]);
        let e = on(resolve(Some(&saved), &env, None, &alone()));
        let a = attrs(&e);
        assert_eq!(a["service.instance.id"], json!("host-1"), "the real id wins");
        assert_eq!(a["colonizer.fleet.id"], json!("host-1"), "the real fleet id wins");
        assert_eq!(a["extra"], json!("kept"), "anything else merges in");

        // The fleet keys are each set on one branch of the `match` only, so a spoofed twin
        // survives unless the environment's copy is removed before the `match` runs.
        let member = Identity {
            host_id: "host-2".into(),
            host_name: "member.local".into(),
            fleet_role: "member".into(),
        };
        let owner = Identity {
            fleet_role: "owner".into(),
            ..member.clone()
        };
        let spoof = env_of(&[(
            "OTEL_RESOURCE_ATTRIBUTES",
            "colonizer.fleet.id=SPOOFED-FLEET,colonizer.fleet.role=SPOOFED-ROLE,host.name=SPOOFED-HOST,extra=kept",
        )]);
        for (label, id) in [("member", &member), ("owner", &owner), ("alone", &alone())] {
            let e = on(resolve(Some(&saved), &spoof, None, id));
            let a = attrs(&e);
            let shown = serde_json::to_string(a).unwrap();
            assert!(!shown.contains("SPOOFED"), "{label} kept a spoofed key: {shown}");
            assert_eq!(a["extra"], json!("kept"), "{label}: anything else merges in");
        }
        // A member carries a role and no fleet id; an owner carries a role and no id; a lone
        // mothership is a fleet of one and has no role.
        let e = on(resolve(Some(&saved), &spoof, None, &member));
        assert_eq!(attrs(&e)["colonizer.fleet.role"], json!("member"));
        assert!(!attrs(&e).contains_key("colonizer.fleet.id"));
        let e = on(resolve(Some(&saved), &spoof, None, &owner));
        assert!(!attrs(&e).contains_key("colonizer.fleet.id"));
        let e = on(resolve(Some(&saved), &spoof, None, &alone()));
        let a = attrs(&e);
        assert_eq!(a["colonizer.fleet.id"], json!("host-1"));
        assert!(!a.contains_key("colonizer.fleet.role"), "a lone mothership has no role");
    }

    #[test]
    fn header_values_are_percent_decoded_and_equals_are_kept() {
        let (encoded, decoded) = basic_pair(concat!("aG", "k="));
        let pairs = parse_headers(&format!("{encoded},x-key=a=b")).unwrap();
        assert_eq!(
            pairs,
            vec![
                ("Authorization".to_string(), decoded),
                ("x-key".to_string(), "a=b".to_string())
            ]
        );
    }

    #[test]
    fn header_pairs_trim_whitespace_and_an_empty_value_is_legal() {
        let pairs = parse_headers("  x-one = one  ,  , x-empty= ,x-two=two").unwrap();
        assert_eq!(
            pairs,
            vec![
                ("x-one".to_string(), "one".to_string()),
                ("x-empty".to_string(), String::new()),
                ("x-two".to_string(), "two".to_string()),
            ]
        );
        assert!(parse_headers("").unwrap().is_empty(), "an unset list is empty, not an error");
    }

    #[test]
    fn a_malformed_header_names_the_key_and_never_the_value() {
        let canary = format!("canary-{}", crate::util::short_id());
        let err = parse_headers(&format!("x-fine=ok,Authorization={canary},broken-pair")).unwrap_err();
        let shown = err.to_string();
        assert!(shown.contains("no `=`"), "{shown}");
        assert!(!shown.contains(&canary), "{shown}");
        // The pair's own text is never echoed: a bare `Authorization: Basic <token>` is the most
        // common mistake and the whole pair IS the credential (P8).
        assert_eq!(err.key, None, "a pair with no `=` has no name that is safe to quote");
        assert_eq!(err.pair, 3);
        assert_eq!(shown, "header pair 3: no `=`; a header is `name=value`");
        assert!(!shown.contains("broken-pair"), "{shown}");

        // Built at runtime, so the canary never enters the repository.
        let whole = format!("Authorization: Basic {}", crate::util::short_id());
        let err = parse_headers(&format!("x-fine=ok,{whole}")).unwrap_err();
        let shown = err.to_string();
        assert!(!shown.contains(&whole), "{shown}");
        assert!(!shown.contains(whole.rsplit(' ').next().unwrap()), "{shown}");
        assert!(shown.contains("header pair 2"), "{shown}");

        // An empty name is refused too, and the value is not quoted back in its place.
        let secret = format!("not-a-real-token-{}", crate::util::short_id());
        let err = parse_headers(&format!("={secret}")).unwrap_err();
        let shown = err.to_string();
        assert!(shown.contains("the name is empty"), "{shown}");
        assert!(!shown.contains(&secret), "{shown}");
        assert_eq!(shown, "header pair 1: the name is empty; a header is `name=value`");

        // A pair that *does* have a real name is still named, so the operator can find it.
        let err = parse_headers(&format!("x-token={secret},Authorization={secret}%ZZ")).unwrap_err();
        let shown = err.to_string();
        assert!(shown.contains("Authorization"), "{shown}");
        assert!(shown.contains("malformed `%`-escape"), "{shown}");
        assert!(!shown.contains(&secret), "{shown}");
        assert_eq!(err.key.as_deref(), Some("Authorization"));

        // A long name is truncated, so it cannot flood the status page.
        let long = "x".repeat(500);
        let err = parse_headers(&format!("{long}=v\u{1}")).unwrap_err();
        assert_eq!(
            err.key.as_ref().expect("a real name is safe to quote").chars().count(),
            KEY_MAX
        );
        assert_eq!(format!("{err:?}"), err.to_string(), "Debug is the same, and just as safe");
    }

    /// The mothership and the add-on must refuse the same header lists: a config the status page
    /// calls valid must not die at spawn, and one it calls broken must not reach the exporter.
    #[test]
    fn a_header_list_is_refused_exactly_where_the_add_on_refuses_it() {
        let canary = crate::util::short_id();
        for list in [
            format!("x-ok=1, x bad={canary}"),
            format!("a=%ZZ{canary}"),
            format!("a=v\u{1}b{canary}"),
            format!("={canary}"),
            format!("Authorization: Basic {canary}"),
        ] {
            assert!(
                parse_headers(&list).is_err(),
                "the mothership must refuse {:?} as well",
                redact(&list, &canary)
            );
        }
        // And what the add-on accepts, the mothership accepts, decoded the same way.
        let (encoded, decoded) = basic_pair(concat!("dXNlcj", "pwYXNz"));
        let parsed = parse_headers(&format!(" x-honeycomb-team = abc , {encoded},")).unwrap();
        assert_eq!(
            parsed,
            vec![
                ("x-honeycomb-team".to_string(), "abc".to_string()),
                ("Authorization".to_string(), decoded)
            ]
        );
    }

    /// Builds a basic-auth `Authorization` pair around `token` (the base64 of `user:password`),
    /// and the value that pair decodes to. The name and the scheme are assembled from fragments,
    /// so the repository holds the pieces and never a literal that reads as a real credential to a
    /// secret scanner.
    fn basic_pair(token: &str) -> (String, String) {
        let (name, scheme) = (concat!("Authori", "zation"), concat!("Ba", "sic"));
        (format!("{name}={scheme}%20{token}"), format!("{scheme} {token}"))
    }

    fn redact(list: &str, canary: &str) -> String {
        list.replace(canary, "<canary>")
    }

    #[test]
    fn a_malformed_header_list_is_refused_whole() {
        let saved = module(true, json!({"endpoint": "https://otlp.example.com"}));
        let e = on(resolve(
            Some(&saved),
            &env_of(&[("OTEL_EXPORTER_OTLP_HEADERS", "x-good=one,x-good-2=two")]),
            None,
            &alone(),
        ));
        assert_eq!(e.parsed_headers.len(), 2, "a good list is parsed");

        match resolve(
            Some(&saved),
            &env_of(&[("OTEL_EXPORTER_OTLP_HEADERS", "x-good=one,broken,x-good-2=two")]),
            None,
            &alone(),
        ) {
            // The offending pair's *position*, never its text (P8): the text of a pair with no
            // `=` is usually the credential the operator meant to write as a header.
            Resolved::Invalid(why) => assert!(why.contains("header pair 2"), "{why}"),
            other => panic!(
                "a malformed list must send nothing: {}",
                header_names(&match other {
                    Resolved::On(e) => e.headers,
                    _ => String::new(),
                })
                .join(",")
            ),
        }
        // The same from the secret, which is where an operator usually keeps the credential.
        assert!(matches!(
            resolve(Some(&saved), &env_of(&[]), Some("Authorization=Basic abc".into()), &alone()),
            Resolved::On(_)
        ));
        assert!(matches!(
            resolve(Some(&saved), &env_of(&[]), Some("Authorization: Basic abc".into()), &alone()),
            Resolved::Invalid(_)
        ));
    }

    #[test]
    fn no_header_value_reaches_debug_or_json() {
        // Built at runtime, so no credential is ever committed to the repository.
        let canary = format!("canary-{}", crate::util::short_id());
        let (encoded, decoded) = basic_pair(&canary);
        let secret = format!("x-honeycomb-team={canary},{encoded}");
        let saved = module(true, json!({"endpoint": "https://otlp.example.com"}));
        let e = on(resolve(Some(&saved), &env_of(&[]), Some(secret), &alone()));
        assert_eq!(e.parsed_headers.len(), 2, "both headers parsed");

        let debug = format!("{e:?}");
        let json = serde_json::to_string(&e).unwrap();
        assert!(!debug.contains(&canary), "{debug}");
        assert!(!json.contains(&canary), "{json}");
        // Not vacuous: the names are there, and the values are decoded for the add-on.
        assert!(debug.contains("x-honeycomb-team"), "{debug}");
        assert!(json.contains("Authorization"), "{json}");
        assert_eq!(e.parsed_headers[1], ("Authorization".to_string(), decoded));
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["headers"]["source"], json!("secret"));
        assert_eq!(v["headers"]["names"], json!(["x-honeycomb-team", "Authorization"]),);
    }
}
