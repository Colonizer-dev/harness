//! The backfill's send pacing (#858): the token bucket that holds it to `--rate` records a
//! second, and the jittered backoff a retried delivery waits. A module of the engine's own
//! ([`super`]); the live exporter paces itself with tailer.rs's bucket and its own backoff.

use std::time::Duration;

/// The floor a server's `Retry-After` is raised to.
pub(crate) const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_CAP: Duration = Duration::from_secs(60);

/// A token bucket over records or bytes: refilled at `rate` a second, holding at most one second's
/// worth, going into debt for an oversized first line. Over `tokio::time`, so a paused test clock
/// drives it (tailer.rs's own bucket, in miniature).
#[derive(Debug, Default)]
pub(crate) struct Bucket {
    tokens: f64,
    last: Option<tokio::time::Instant>,
}

impl Bucket {
    pub(crate) fn available(&mut self, rate: u64) -> u64 {
        let now = tokio::time::Instant::now();
        let cap = rate as f64;
        self.tokens = match self.last {
            None => cap,
            Some(last) => (self.tokens + now.duration_since(last).as_secs_f64() * cap).min(cap),
        };
        self.last = Some(now);
        self.tokens.max(0.0) as u64
    }

    pub(crate) fn spend(&mut self, n: u64) {
        self.tokens -= n as f64;
    }
}

/// A jittered next delay: the current one doubled, capped, ±20 % (the exporter's own).
pub(crate) fn next_backoff(current: Option<Duration>) -> Duration {
    let base = match current {
        None => BACKOFF_START,
        Some(d) => (d * 2).min(BACKOFF_CAP),
    };
    let mut r = [0u8; 1];
    let _ = ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut r);
    let factor = 0.8 + 0.4 * (f64::from(r[0]) / 255.0);
    base.mul_f64(factor)
}
