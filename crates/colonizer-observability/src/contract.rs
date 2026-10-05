//! The contract between the mothership and this add-on (docs/design/observability.md, Contract):
//! `<data>/observability/exporter.json`, written by the mothership, and the one JSON line of secrets
//! it writes on the child's stdin. Nothing else configures the add-on: it reads no `OTEL_*`
//! variable itself, since the mothership clears the child's environment and resolves every override.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};

/// The contract version this build speaks. A mothership that writes another is refused.
pub const CONTRACT: u32 = 1;
/// The exit code of a refused contract or version: the mothership never restarts on it.
pub const EXIT_REFUSED: i32 = 78;
/// `start_from = now`: a new destination skips what the ledgers already hold.
pub const START_NOW: &str = "now";
/// `start_from = backlog`: a new destination gets everything within `max_backlog_days`.
pub const START_BACKLOG: &str = "backlog";
/// The contract file's name, under `<data>/observability/`.
pub const FILE: &str = "exporter.json";

/// `exporter.json`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Contract {
    pub contract: u32,
    pub mothership_version: String,
    pub host_id: String,
    pub fleet_id: String,
    pub data_dir: PathBuf,
    pub settings: Settings,
    /// Every colony the mothership knows, by id. A colony missing here is structure-only.
    pub policy: BTreeMap<String, ColonyPolicy>,
}

/// The effective exporter settings, every override already applied by the mothership.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub provider: String,
    /// The base OTLP/HTTP endpoint; `/v1/logs`, `/v1/traces` and `/v1/metrics` are appended to it.
    pub endpoint: String,
    /// Per-signal endpoints (`OTEL_EXPORTER_OTLP_LOGS_ENDPOINT`, …), used as they are.
    pub logs_endpoint: Option<String>,
    pub traces_endpoint: Option<String>,
    pub metrics_endpoint: Option<String>,
    pub protocol: String,
    pub compression: String,
    pub timeout_secs: u64,
    pub service_name: String,
    /// `OTEL_RESOURCE_ATTRIBUTES`, parsed.
    pub resource_attributes: BTreeMap<String, String>,
    pub stream_operational: bool,
    pub stream_activity: bool,
    pub stream_traces: bool,
    pub stream_metrics: bool,
    pub max_attribute_bytes: u64,
    pub max_content_bytes: u64,
    /// The share of colonies whose trace is exported, 0 to 1, decided per colony by its trace id.
    pub trace_sample_ratio: f64,
    pub repo_names: String,
    pub max_backlog_days: u64,
    pub max_read_mib_per_sec: u64,
    /// Where a destination with no read position yet starts: `now` (the end of every file that
    /// exists when it is first configured) or `backlog` (the start of every file, within
    /// `max_backlog_days`).
    pub start_from: String,
    /// How often metrics are pushed.
    pub metrics_interval_secs: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            provider: "otlp".into(),
            endpoint: String::new(),
            logs_endpoint: None,
            traces_endpoint: None,
            metrics_endpoint: None,
            protocol: "http/protobuf".into(),
            compression: "gzip".into(),
            timeout_secs: 10,
            service_name: "colonizer".into(),
            resource_attributes: BTreeMap::new(),
            stream_operational: true,
            stream_activity: true,
            stream_traces: true,
            stream_metrics: true,
            max_attribute_bytes: 1024,
            max_content_bytes: 32_768,
            trace_sample_ratio: 1.0,
            repo_names: "plain".into(),
            max_backlog_days: 7,
            max_read_mib_per_sec: 8,
            start_from: START_NOW.into(),
            metrics_interval_secs: 30,
        }
    }
}

impl Settings {
    /// Where a signal's requests go: its own endpoint if set, else the base with the OTLP path.
    pub fn url(&self, path: &str) -> String {
        let own = match path {
            "/v1/logs" => self.logs_endpoint.as_deref(),
            "/v1/traces" => self.traces_endpoint.as_deref(),
            "/v1/metrics" => self.metrics_endpoint.as_deref(),
            _ => None,
        };
        match own.filter(|u| !u.is_empty()) {
            Some(url) => url.to_string(),
            None => format!("{}{path}", self.endpoint.trim_end_matches('/')),
        }
    }
}

/// What the mothership resolved for one colony.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ColonyPolicy {
    pub org: String,
    pub repo: String,
    pub sensitivity: Option<String>,
    /// Content export for this colony (#848). Always false until that lands; the policy's content
    /// gate stays closed regardless.
    pub content: bool,
    pub thinking: bool,
    /// The colony's status, for the colonies-by-status metric.
    pub status: Option<String>,
    /// The agent module the colony runs (`claude_code`): its trace's `gen_ai.agent.name`.
    pub agent: String,
    /// When the colony was created (RFC 3339): its root span's start.
    pub created_at: Option<String>,
    /// What launched it (`burn_down`, `redteam`, …), when not a person.
    pub origin: Option<String>,
    /// Its pull request, once it has one.
    pub pr_url: Option<String>,
}

/// `<data>/observability/exporter.json`.
pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("observability").join(FILE)
}

/// Reads and checks a contract file. A version this build does not speak is an error the caller
/// turns into [`EXIT_REFUSED`].
pub fn load(path: &Path) -> Result<Contract, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let contract: Contract =
        serde_json::from_slice(&bytes).map_err(|e| format!("{} is not a valid contract: {e}", path.display()))?;
    if contract.contract != CONTRACT {
        return Err(format!(
            "refused: the mothership speaks contract {}, this add-on speaks {CONTRACT}",
            contract.contract
        ));
    }
    Ok(contract)
}

/// The secrets line on stdin: `{"headers": "k=v,k2=v2"}`.
#[derive(Default, Deserialize)]
struct SecretsLine {
    #[serde(default)]
    headers: String,
}

/// Reads the one secrets line from `reader` and parses its headers. An empty input means no
/// headers. The error never quotes the line: it may hold a credential.
pub fn read_secrets(reader: &mut impl BufRead) -> Result<Vec<(String, String)>, String> {
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| format!("cannot read the secrets line: {e}"))?;
    if line.trim().is_empty() {
        return Ok(Vec::new());
    }
    let secrets: SecretsLine =
        serde_json::from_str(&line).map_err(|_| "the secrets line is not the expected JSON object".to_string())?;
    parse_headers(&secrets.headers)
}

/// Parses an `OTEL_EXPORTER_OTLP_HEADERS` list: `k=v` pairs separated by commas, whitespace around
/// each trimmed, values percent-decoded (the OpenTelemetry environment variable spec). A pair with
/// no `=`, an empty or non-token name, or a value with a control character is refused, naming the
/// pair's position and never its value.
pub fn parse_headers(list: &str) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    for (i, pair) in list.split(',').enumerate() {
        if pair.trim().is_empty() {
            continue;
        }
        let Some((name, value)) = pair.split_once('=') else {
            return Err(format!("header {} has no `=`", i + 1));
        };
        let name = name.trim();
        if name.is_empty() || !name.bytes().all(is_token_byte) {
            return Err(format!("header {} has an invalid name", i + 1));
        }
        let value = percent_decode(value.trim()).ok_or_else(|| format!("header {} has a malformed %-escape", i + 1))?;
        if value.chars().any(|c| c.is_control()) {
            return Err(format!("header {} ({name}) has a control character in its value", i + 1));
        }
        out.push((name.to_string(), value));
    }
    Ok(out)
}

/// RFC 7230 `tchar`.
fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3)?;
            let hex = std::str::from_utf8(hex).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_parse_trim_and_percent_decode() {
        let parsed = parse_headers(" x-honeycomb-team = abc , Authorization=Basic%20dXNlcjpwYXNz,").unwrap();
        assert_eq!(
            parsed,
            vec![
                ("x-honeycomb-team".to_string(), "abc".to_string()),
                ("Authorization".to_string(), "Basic dXNlcjpwYXNz".to_string()),
            ]
        );
        assert!(parse_headers("").unwrap().is_empty());
    }

    #[test]
    fn a_bad_header_is_refused_without_quoting_its_value() {
        let value = format!("canary{}", std::process::id());
        for list in [
            format!("novalue{value}"),
            format!("bad name={value}"),
            format!("k={value}%zz"),
            format!("k={value}%0A"),
        ] {
            let err = parse_headers(&list).unwrap_err();
            assert!(!err.contains(&value), "{err}");
        }
    }

    #[test]
    fn the_secrets_line_is_one_json_object() {
        let mut input: &[u8] = b"{\"headers\":\"api-key=k1\"}\n";
        assert_eq!(read_secrets(&mut input).unwrap(), vec![("api-key".into(), "k1".into())]);
        let mut empty: &[u8] = b"";
        assert!(read_secrets(&mut empty).unwrap().is_empty());
        let mut junk: &[u8] = b"api-key=k1\n";
        let err = read_secrets(&mut junk).unwrap_err();
        assert!(!err.contains("k1"), "{err}");
    }

    #[test]
    fn signal_urls_append_the_otlp_path_unless_a_signal_has_its_own() {
        let mut s = Settings {
            endpoint: "https://otlp.example.com/otlp/".into(),
            ..Settings::default()
        };
        assert_eq!(s.url("/v1/logs"), "https://otlp.example.com/otlp/v1/logs");
        s.metrics_endpoint = Some("https://metrics.example.com/custom".into());
        assert_eq!(s.url("/v1/metrics"), "https://metrics.example.com/custom");
        assert_eq!(s.url("/v1/logs"), "https://otlp.example.com/otlp/v1/logs");
    }

    #[test]
    fn another_contract_version_is_refused() {
        let dir = std::env::temp_dir().join(format!("colonizer-contract-{}", crate::testkit::unique()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(FILE);
        std::fs::write(&file, br#"{"contract": 2, "host_id": "h"}"#).unwrap();
        assert!(load(&file).unwrap_err().starts_with("refused"));
        std::fs::write(&file, br#"{"contract": 1, "host_id": "h"}"#).unwrap();
        let c = load(&file).unwrap();
        assert_eq!(c.settings.service_name, "colonizer", "absent fields take their defaults");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
