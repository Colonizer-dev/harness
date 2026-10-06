//! Boot-failure classification and the transient-retry schedule (issue #881).
//!
//! A microVM boot that dies on a blip should not cost the colony: a transient failure is retried on
//! a fixed backoff before the colony is failed for real, while a permanent one — a bad
//! configuration, a missing credential, a policy refusal — fails at once. [`classify`] is pure, so
//! the rule is tested apart from the boot and queue machinery that applies it, the way
//! `github::classify` is.

use chrono::Duration;
use serde::{Deserialize, Serialize};

/// The kind of failure a failed boot is. Stored on the colony (`Session::failure_class`) so the
/// cockpit and any later reader can tell a blip that was retried from a verdict that was not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    /// Infrastructure that may well come back on its own: a runtime or image hiccup, a timeout, a
    /// dropped or refused connection, an HTTP 5xx. Retried on [`BOOT_RETRY_DELAYS`].
    TransientInfra,
    /// Everything else — a bad or missing configuration, missing credentials, a policy refusal.
    /// Retrying would only fail the same way, so the colony is failed at once.
    Permanent,
}

impl FailureClass {
    /// The name the API serialises and messages name back to a person, e.g. in a loop's last-run
    /// outcome (`failed (transient_infra)`, issue #881).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TransientInfra => "transient_infra",
            Self::Permanent => "permanent",
        }
    }
}

/// How long to wait before each transient retry, longest last. Three delays, so a boot is attempted
/// four times in all before the colony is failed for real.
pub const BOOT_RETRY_DELAYS: [Duration; 3] = [Duration::minutes(1), Duration::minutes(5), Duration::minutes(15)];

/// The wait before retry number `retries + 1` (0-based: the first retry is `retries == 0`), or
/// `None` once the budget is spent and the next failure should be permanent.
pub fn retry_delay(retries: u32) -> Option<Duration> {
    BOOT_RETRY_DELAYS.get(retries as usize).copied()
}

/// Classifies a failed boot's message. The default is [`FailureClass::Permanent`]: failing fast on
/// a message nobody recognised is the safe reading, and only a shape that is known to be a blip
/// earns a retry.
pub fn classify(message: &str) -> FailureClass {
    // GitHub's own verdicts — a repository the account cannot see, a refused or missing
    // credential, a suspended account — are permanent, and `github::classify` already names them.
    // Asked first, so its `is_transient` (asked below) never runs on one and so a message like
    // "connection refused" is not mistaken for a refused credential.
    if crate::github::classify(message).is_some() {
        return FailureClass::Permanent;
    }
    let text = message.to_ascii_lowercase();
    // The permanent shapes that are not GitHub's: a refusal by policy or authorization, a bad or
    // missing configuration, a wrong shape. Checked before the transient list so a config error
    // that happens to name a timeout is not retried; the markers are specific enough that the
    // transient "connection refused" does not match any of them.
    const PERMANENT: &[&str] = &[
        "not authorized",
        "unauthorized",
        "forbidden",
        "permission denied",
        "resume refused",
        "policy",
        "not installed",
        "no worktree to resume",
        "configuration",
        "misconfigur",
        "invalid",
        "malformed",
        "unsupported",
        "missing secret",
    ];
    if PERMANENT.iter().any(|m| text.contains(m)) {
        return FailureClass::Permanent;
    }
    // The transient shapes, each naming the specific failure it is: the microVM runtime and image
    // pull, a boot that failed or timed out, a dropped or refused connection, an HTTP 5xx (529 and
    // "overloaded" are Anthropic's and the gateway's extra 5xx), and a boot interrupted by a
    // harness restart. Deliberately not the bare "runtime" or "sandbox", which also appear in
    // permanent messages (a missing node runtime, a bad sandbox configuration). `github::is_transient`
    // already knows the network and 5xx shapes the boot's GitHub steps hit, so it is asked too.
    const TRANSIENT: &[&str] = &[
        "microvm",
        "microsandbox",
        "msb ",
        "failed to boot",
        "boot failed",
        "image pull",
        "failed to pull",
        "pull failed",
        "deadline exceeded",
        "interrupted by restart",
        "overloaded",
        "http 500",
        "http 502",
        "http 503",
        "http 504",
        "http 529",
        "connection reset",
        "connection refused",
        "connection timed out",
        "timed out",
        "timeout",
        // A provider the router cannot reach is a blip, whatever 5xx it came in on: the agent
        // runner's own wording for it is "502 model router: <provider> is unreachable" (issue #980),
        // which carries no "network" and so is not `github::is_transient`'s "network is unreachable".
        "unreachable",
    ];
    if TRANSIENT.iter().any(|m| text.contains(m)) || crate::github::is_transient(message) {
        return FailureClass::TransientInfra;
    }
    FailureClass::Permanent
}

/// Classifies the error a colony's turn ended with (issue #1093), read off the runner's free-text
/// result ("API Error: 502 model router: the connection to Anthropic failed (UND_ERR_SOCKET)").
///
/// The boot classifier ([`classify`]) is the fallback, but a turn's error has shapes a boot's never
/// does, so three rules run first. A refusal of the request itself — sign-in, permission, policy —
/// is permanent: retrying only fails the same way. A model gateway status from the 5xx family (529
/// is Anthropic's "overloaded") is transient. And a connection that was reset or closed under the
/// request (undici's `UND_ERR_SOCKET`, `ECONNRESET`, a gateway that restarted) is transient whatever
/// status it came on, since the router names a reset "the connection to <provider> failed".
pub fn classify_turn_error(message: &str) -> FailureClass {
    let text = message.to_ascii_lowercase();
    // A connection that was refused is a gateway that is not listening (yet); a request that was
    // refused is a verdict. Everything else carrying these words is a refusal of the request.
    const REFUSED: &[&str] = &[
        "authentication",
        "unauthorized",
        "not authorized",
        "forbidden",
        "permission",
        "policy",
        "invalid api key",
        "invalid x-api-key",
        "oauth token",
    ];
    let request_refused = text.contains("refused") && !text.contains("connection refused") && !text.contains("econnrefused");
    if request_refused || REFUSED.iter().any(|m| text.contains(m)) {
        return FailureClass::Permanent;
    }
    match turn_error_status(&text) {
        Some(500 | 502 | 503 | 504 | 529) => return FailureClass::TransientInfra,
        Some(400..=499) => return classify(message),
        _ => {}
    }
    const RESET: &[&str] = &[
        "und_err_socket",
        "und_err_closed",
        "econnreset",
        "econnrefused",
        "epipe",
        "socket hang up",
        "other side closed",
        "the connection to",
        "fetch failed",
        "gateway restart",
    ];
    if RESET.iter().any(|m| text.contains(m)) {
        return FailureClass::TransientInfra;
    }
    classify(message)
}

/// The HTTP status a turn's error names, if it names one where the runner puts it: right after
/// "API Error:", or as the message's first word ("502 model router: ..."). A number elsewhere — a
/// timeout's seconds, a token count — is not a status.
fn turn_error_status(lowercase: &str) -> Option<u16> {
    let rest = lowercase
        .find("api error:")
        .map(|at| &lowercase[at + "api error:".len()..])
        .unwrap_or(lowercase)
        .trim_start();
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let next = rest[digits.len()..].chars().next();
    if digits.len() != 3 || next.is_some_and(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    digits.parse().ok().filter(|code| (100..600).contains(code))
}

/// A short name for what a transient turn error was, for the colony's attention card and log
/// (issue #1093): the status and the failure, e.g. "502, connection to Anthropic" for the router's
/// "502 model router: the connection to Anthropic failed (UND_ERR_SOCKET)". Empty when the message
/// names neither, and the card then says only "a model gateway error".
pub fn turn_error_cause(message: &str) -> String {
    let text = message.to_ascii_lowercase();
    let mut parts: Vec<String> = Vec::new();
    if let Some(status) = turn_error_status(&text) {
        parts.push(status.to_string());
    }
    // The provider's name as the message spells it, so "Anthropic" keeps its capital.
    let named = |prefix: &str, suffix: &str| -> Option<String> {
        let start = text.find(prefix)? + prefix.len();
        let len = text[start..].find(suffix)?;
        let name = message.get(start..start + len)?.trim();
        (!name.is_empty() && name.len() <= 40).then(|| name.to_string())
    };
    let what = if let Some(provider) = named("the connection to ", " failed") {
        Some(format!("connection to {provider}"))
    } else if let Some(provider) = named("model router: ", " is unreachable") {
        Some(format!("{provider} unreachable"))
    } else if text.contains("overloaded") || text.contains("529") {
        Some("overloaded".to_string())
    } else if text.contains("timed out") || text.contains("timeout") {
        Some("timed out".to_string())
    } else if [
        "und_err_socket",
        "econnreset",
        "socket hang up",
        "other side closed",
        "connection reset",
    ]
    .iter()
    .any(|m| text.contains(m))
    {
        Some("connection reset".to_string())
    } else if text.contains("connection refused") || text.contains("econnrefused") {
        Some("connection refused".to_string())
    } else {
        None
    };
    parts.extend(what);
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The blips a boot should ride out: the runtime and image pull, timeouts, dropped and refused
    /// connections, the HTTP 5xx family, a restart mid-boot, and the network shapes the GitHub
    /// steps hit (`github::is_transient` is folded in, so a DNS or 429 failure counts too).
    #[test]
    fn a_transient_infrastructure_failure_is_retried() {
        for message in [
            "microVM colonizer-abc failed to start: msb: exit status 1",
            "microsandbox: could not start the sandbox",
            "msb run: image pull failed for rust:1-bookworm",
            "failed to pull the image: connection reset by peer",
            "booting microVM timed out after 90s",
            "runtime error: context deadline exceeded",
            "connection refused (os error 111)",
            "server returned HTTP 502 bad gateway",
            "HTTP 503 service unavailable",
            "upstream gateway returned HTTP 529",
            "the model is overloaded, please try again",
            // Issue #980: the agent runner's own wording for a provider the router could not reach.
            "API Error: 502 model router: Anthropic is unreachable",
            "boot interrupted by restart",
            // `github::is_transient`'s own network shapes.
            "error connecting to api.github.com",
            "Could not resolve host: github.com",
            "http 429 too many requests",
        ] {
            assert_eq!(classify(message), FailureClass::TransientInfra, "{message:?}");
        }
    }

    /// The verdicts a retry cannot fix: credentials, configuration and policy fail at once, and an
    /// unrecognised message is failed fast rather than retried on a guess.
    #[test]
    fn a_permanent_failure_is_not_retried() {
        for message in [
            "git asked for a GitHub credential and none was available",
            "bad credentials (HTTP 401)",
            "authentication failed",
            "GitHub has suspended the account signed in on this machine",
            "resume refused: not authorized (no grant)",
            "the egress policy forbids this host",
            "the agent module is not installed",
            "this colony has no worktree to resume",
            "invalid sandbox configuration: cpus must be >= 1",
            "unsupported image format",
            // The bare "runtime"/"sandbox" words must not earn a retry on their own (issue #881).
            "node runtime bin/node-guest is missing or unusable; run scripts/install.sh",
            "some failure nobody has seen before",
        ] {
            assert_eq!(classify(message), FailureClass::Permanent, "{message:?}");
        }
    }

    /// A refusal that reads like a transient one is still permanent: "connection refused" is a
    /// blip, but a refused credential or authorization is a verdict, and GitHub's own wording
    /// wins over the transient list.
    #[test]
    fn a_refusal_is_told_from_a_dropped_connection() {
        assert_eq!(classify("connection refused"), FailureClass::TransientInfra);
        assert_eq!(
            classify("GitHub refused the credentials for acme/repo"),
            FailureClass::Permanent
        );
        assert_eq!(classify("resume refused: not authorized"), FailureClass::Permanent);
    }

    /// The backoff runs 1, 5, 15 minutes and then runs out — three retries, four attempts.
    #[test]
    fn the_backoff_runs_out_after_three_retries() {
        assert_eq!(retry_delay(0), Some(Duration::minutes(1)));
        assert_eq!(retry_delay(1), Some(Duration::minutes(5)));
        assert_eq!(retry_delay(2), Some(Duration::minutes(15)));
        assert_eq!(retry_delay(3), None, "the budget is spent");
        assert_eq!(retry_delay(9), None);
    }

    /// The class rides `sessions.json` as its snake_case name and survives the round trip.
    #[test]
    fn the_failure_class_round_trips_through_the_wire() {
        assert_eq!(
            serde_json::to_value(FailureClass::TransientInfra).unwrap(),
            serde_json::json!("transient_infra")
        );
        assert_eq!(
            serde_json::to_value(FailureClass::Permanent).unwrap(),
            serde_json::json!("permanent")
        );
        assert_eq!(
            serde_json::from_value::<FailureClass>(serde_json::json!("transient_infra")).unwrap(),
            FailureClass::TransientInfra
        );
        // The name messages use must be the name the wire uses, or a loop's last-run outcome would
        // read differently from the colony it describes.
        for class in [FailureClass::TransientInfra, FailureClass::Permanent] {
            assert_eq!(serde_json::to_value(class).unwrap().as_str(), Some(class.as_str()));
        }
    }

    /// Issue #1093: the gateway errors a turn dies on are transient — the router's reset wording
    /// (which carries no "unreachable"), a bare 5xx after "API Error:", 529 overloaded, and a socket
    /// that a restarting gateway dropped.
    #[test]
    fn a_turn_that_died_on_the_gateway_is_transient() {
        for message in [
            "API Error: 502 model router: the connection to Anthropic failed (UND_ERR_SOCKET)",
            "API Error: 502 model router: Anthropic is unreachable (connection failed: ECONNREFUSED)",
            "API Error: 503 service temporarily down",
            "API Error: 529 {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\"}}",
            "API Error: 500 internal error",
            "request failed: read ECONNRESET",
            "socket hang up",
            "UND_ERR_SOCKET other side closed",
        ] {
            assert_eq!(classify_turn_error(message), FailureClass::TransientInfra, "{message:?}");
        }
    }

    /// Issue #1093: a turn refused for who is asking or what is asked holds at once.
    #[test]
    fn a_turn_refused_for_auth_or_policy_is_permanent() {
        for message in [
            "API Error: 401 {\"type\":\"authentication_error\",\"message\":\"invalid x-api-key\"}",
            "API Error: 403 forbidden",
            "API Error: 400 the request was refused by the usage policy",
            "API Error: 502 model router: request refused",
            "OAuth token has expired",
            "some failure nobody has seen before",
        ] {
            assert_eq!(classify_turn_error(message), FailureClass::Permanent, "{message:?}");
        }
    }

    /// The cause the card names: the status and the failure, the provider spelled as the message
    /// spells it, and nothing invented for a message that names neither.
    #[test]
    fn the_turn_error_cause_names_the_status_and_the_failure() {
        assert_eq!(
            turn_error_cause("API Error: 502 model router: the connection to Anthropic failed (UND_ERR_SOCKET)"),
            "502, connection to Anthropic"
        );
        assert_eq!(
            turn_error_cause("API Error: 502 model router: Anthropic is unreachable (DNS lookup failed: ENOTFOUND)"),
            "502, Anthropic unreachable"
        );
        assert_eq!(turn_error_cause("API Error: 529 overloaded_error"), "529, overloaded");
        assert_eq!(turn_error_cause("read ECONNRESET"), "connection reset");
        assert_eq!(turn_error_cause("model timed out after 600 s"), "timed out");
        assert_eq!(turn_error_cause("something else"), "");
    }
}
