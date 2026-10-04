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
}
