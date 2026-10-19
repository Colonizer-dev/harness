//! The backfill's send pacing (#858): the token bucket that holds it to `--rate` records a
//! second. A module of the engine's own ([`super`]); a retried delivery waits on the shared
//! `delivery` module's backoff, and the live exporter paces itself with tailer.rs's bucket.

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
