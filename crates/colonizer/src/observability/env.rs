//! The effective exporter config (#841): the saved `observability` module overlaid with the
//! standard `OTEL_*` variables, and the master switch.
//!
//! - Nothing exports unless the module is saved **and** enabled, or `COLONIZER_OBSERVABILITY=on`
//!   is set. An `OTEL_EXPORTER_OTLP_ENDPOINT` left in the service environment for another program
//!   never turns export on by itself.
//! - `OTEL_SDK_DISABLED=true` turns it off, whatever else says.
//! - Each `OTEL_*` variable overrides its one field; the status API reports where every field came
//!   from (module, environment or default).
//! - Headers come from `OTEL_EXPORTER_OTLP_HEADERS` when it is set, else from the
//!   `observability-headers` secret. Their values are handed to the add-on on its stdin and never
//!   written to the contract, a log line or an API answer: only the header *names* are reported.

use super::settings::{self, ExporterConfig, FILE, OTLP};
use crate::config::ModuleChoice;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// The master switch for an environment-only setup.
pub const MASTER: &str = "COLONIZER_OBSERVABILITY";

/// What the exporter should do.
pub enum Resolved {
    /// Not exporting, and why.
    Off(String),
    /// Configured but unusable; shown in the status, nothing is sent.
    Invalid(String),
    On(Box<Effective>),
}

/// An exporter config ready for the contract.
pub struct Effective {
    /// The contract's `settings` object.
    pub settings: Map<String, Value>,
    /// Field name → `module`, `env:<VAR>` or `default`.
    pub provenance: BTreeMap<String, String>,
    /// The raw `k=v,…` header list. Never logged or serialised; see the module doc.
    pub headers: String,
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

impl Effective {
    pub fn str(&self, key: &str) -> &str {
        self.settings.get(key).and_then(Value::as_str).unwrap_or("")
    }
}

/// The header names of a `k=v,…` list, for the status API: names only, never values.
pub fn header_names(list: &str) -> Vec<String> {
    list.split(',')
        .filter_map(|pair| pair.split_once('=').map(|(name, _)| name.trim().to_string()))
        .filter(|name| !name.is_empty())
        .collect()
}

/// Parses `OTEL_RESOURCE_ATTRIBUTES`: `k=v` pairs, comma-separated, values percent-decoded. A
/// malformed pair is skipped (the spec says to ignore the whole variable; skipping one pair keeps
/// the rest, and reports it).
fn resource_attributes(list: &str) -> (BTreeMap<String, String>, bool) {
    let mut out = BTreeMap::new();
    let mut bad = false;
    for pair in list.split(',').filter(|p| !p.trim().is_empty()) {
        match pair.split_once('=') {
            Some((k, v)) if !k.trim().is_empty() => {
                out.insert(k.trim().to_string(), percent_decode(v.trim()));
            }
            _ => bad = true,
        }
    }
    (out, bad)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = s.get(i + 1..i + 3)
            && let Ok(b) = u8::from_str_radix(hex, 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn truthy(v: &str) -> bool {
    matches!(v.trim().to_ascii_lowercase().as_str(), "true" | "1" | "on" | "yes")
}

/// Resolves the effective config. `env` reads one variable (the process environment outside
/// tests); `secret` is the `observability-headers` secret's value, if any.
pub fn resolve(module: Option<&ModuleChoice>, env: &dyn Fn(&str) -> Option<String>, secret: Option<String>) -> Resolved {
    let var = |name: &str| env(name).filter(|v| !v.trim().is_empty());
    if var("OTEL_SDK_DISABLED").is_some_and(|v| truthy(&v)) {
        return Resolved::Off("OTEL_SDK_DISABLED is set".into());
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
    field("service_name", json!("colonizer"), Some("OTEL_SERVICE_NAME"), &mut s);
    for (key, var_name) in [
        ("logs_endpoint", "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT"),
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
    let (attrs, bad_attrs) = resource_attributes(&var("OTEL_RESOURCE_ATTRIBUTES").unwrap_or_default());
    if !attrs.is_empty() {
        provenance.insert("resource_attributes".into(), "env:OTEL_RESOURCE_ATTRIBUTES".into());
    }
    s.insert("resource_attributes".into(), json!(attrs));

    s.insert("provider".into(), json!(OTLP));
    for (key, value) in [
        ("stream_operational", json!(cfg.stream_operational)),
        ("stream_activity", json!(cfg.stream_activity)),
        ("stream_traces", json!(cfg.stream_traces)),
        ("stream_metrics", json!(cfg.stream_metrics)),
        ("max_attribute_bytes", json!(cfg.max_attribute_bytes)),
        ("max_content_bytes", json!(cfg.max_content_bytes)),
        ("repo_names", json!(cfg.repo_names)),
        ("max_backlog_days", json!(cfg.max_backlog_days)),
        ("max_read_mib_per_sec", json!(cfg.max_read_mib_per_sec)),
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
    let signal_endpoints: Vec<String> = ["logs_endpoint", "metrics_endpoint"]
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
    Resolved::On(Box::new(Effective {
        settings: s,
        provenance,
        headers,
        headers_source,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| pairs.iter().find(|(k, _)| *k == name).map(|(_, v)| v.to_string())
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

    #[test]
    fn off_by_default_even_with_an_otel_endpoint_in_the_environment() {
        let env = env_of(&[("OTEL_EXPORTER_OTLP_ENDPOINT", "https://otlp.example.com")]);
        assert!(
            matches!(resolve(None, &env, None), Resolved::Off(_)),
            "no module, no master switch"
        );
        let saved_off = module(false, json!({"endpoint": "https://otlp.example.com"}));
        assert!(matches!(resolve(Some(&saved_off), &env, None), Resolved::Off(_)));
        assert!(matches!(resolve(None, &env_of(&[]), Some("k=v".into())), Resolved::Off(_)));
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
        let e = on(resolve(None, &env, None));
        assert_eq!(e.str("endpoint"), "http://127.0.0.1:4318");
        assert_eq!(e.str("service_name"), "colonizer-prod");
        assert_eq!(e.settings["timeout_secs"], json!(3), "milliseconds, rounded up to seconds");
        assert_eq!(e.settings["resource_attributes"]["team"], json!("infra ops"));
        assert_eq!(e.provenance["endpoint"], "env:OTEL_EXPORTER_OTLP_ENDPOINT");
        assert_eq!(e.provenance["protocol"], "default");
        // Without an endpoint the master switch is a configuration error, not a silent no-op.
        assert!(matches!(
            resolve(None, &env_of(&[(MASTER, "on")]), None),
            Resolved::Invalid(_)
        ));
    }

    #[test]
    fn otel_sdk_disabled_wins_over_everything() {
        let saved = module(true, json!({"endpoint": "https://otlp.example.com"}));
        let env = env_of(&[(MASTER, "on"), ("OTEL_SDK_DISABLED", "true")]);
        assert!(matches!(resolve(Some(&saved), &env, None), Resolved::Off(_)));
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
        let e = on(resolve(Some(&saved), &env, Some("x-honeycomb-team=from-secret".into())));
        assert_eq!(e.str("endpoint"), "https://env.example.com");
        assert_eq!(e.str("protocol"), "http/json");
        assert_eq!(e.provenance["protocol"], "module");
        assert_eq!(e.headers, "x-honeycomb-team=from-env");
        assert_eq!(e.headers_source, "env");

        let e = on(resolve(Some(&saved), &env_of(&[]), Some("api-key=from-secret".into())));
        assert_eq!(e.headers_source, "secret");
        assert_eq!(header_names(&e.headers), vec!["api-key".to_string()]);
        assert!(!format!("{e:?}").contains("from-secret"), "Debug never shows a header value");
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
                matches!(resolve(Some(&saved), &env_of(pairs), None), Resolved::Invalid(_)),
                "{var}={value}"
            );
        }
        let file = ModuleChoice {
            provider: FILE.into(),
            enabled: true,
            settings: Map::new(),
        };
        assert!(matches!(resolve(Some(&file), &env_of(&[]), None), Resolved::Invalid(_)));
    }
}
