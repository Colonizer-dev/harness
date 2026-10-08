//! The boot's wait for agentd (`GET /v1/health`), with each attempt bounded.
//!
//! The first dial through the mesh's SOCKS proxy right after a microVM joins can stall for the
//! proxy's own ~5 s before it fails, because the WireGuard path to the new peer is not up yet
//! (issue #1143). Waiting that out made every boot's `agentd` phase 5.5 s. Each attempt is cut off
//! after a short limit instead, and the next one starts at once.

use std::{future::Future, time::Duration};

use anyhow::Result;
use tokio::time::Instant;

/// How long the first readiness attempt may take. Each later attempt gets twice the last, up to
/// [`READY_ATTEMPT_CAP`].
pub(crate) const READY_ATTEMPT_FIRST: Duration = Duration::from_millis(750);
/// The longest one readiness attempt may take.
pub(crate) const READY_ATTEMPT_CAP: Duration = Duration::from_secs(3);
/// The pause after an attempt that failed fast (a refused connection while agentd is still
/// starting). An attempt that hit its limit already waited, so it is followed by none.
pub(crate) const READY_RETRY_PAUSE: Duration = Duration::from_millis(500);

/// The time limit of the `attempt`th readiness try, counting from 0: 750 ms, 1.5 s, 3 s, 3 s, ...
pub(crate) fn ready_attempt_limit(attempt: u32) -> Duration {
    READY_ATTEMPT_FIRST
        .saturating_mul(1u32.checked_shl(attempt).unwrap_or(u32::MAX))
        .min(READY_ATTEMPT_CAP)
}

/// Calls `dial` until it answers `200`, giving each call [`ready_attempt_limit`] and starting the
/// next one at once when a call hits its limit. `on_attempt` hears each attempt's number (from 1),
/// how long it took and how it ended. Returns `false` once an attempt has failed after `deadline`;
/// like the loop it replaced, it never abandons an attempt for the deadline alone.
pub(crate) async fn wait_until_ready<D, DF, L, LF>(deadline: Instant, mut dial: D, mut on_attempt: L) -> bool
where
    D: FnMut() -> DF,
    DF: Future<Output = Result<(u16, String)>>,
    L: FnMut(u32, Duration, String) -> LF,
    LF: Future<Output = ()>,
{
    let mut attempt = 0u32;
    loop {
        let limit = ready_attempt_limit(attempt);
        attempt += 1;
        let started = Instant::now();
        let answer = tokio::time::timeout(limit, dial()).await;
        let took = started.elapsed();
        let (ready, timed_out, outcome) = match &answer {
            Ok(Ok((200, _))) => (true, false, "ready".to_string()),
            Ok(Ok((status, _))) => (false, false, format!("answered {status}")),
            Ok(Err(e)) => (false, false, format!("failed: {e:#}")),
            Err(_) => (false, true, format!("no answer within {limit:?}")),
        };
        on_attempt(attempt, took, outcome).await;
        if ready {
            return true;
        }
        if Instant::now() > deadline {
            return false;
        }
        if !timed_out {
            tokio::time::sleep(READY_RETRY_PAUSE).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use std::cell::{Cell, RefCell};

    #[test]
    fn the_attempt_limit_grows_from_750_ms_to_a_3_s_cap() {
        let limits: Vec<_> = (0..5).map(ready_attempt_limit).collect();
        let ms = |n| Duration::from_millis(n);
        assert_eq!(limits, [ms(750), ms(1500), ms(3000), ms(3000), ms(3000)]);
        // A count far past any real boot does not overflow.
        assert_eq!(ready_attempt_limit(500), READY_ATTEMPT_CAP);
    }

    #[tokio::test(start_paused = true)]
    async fn a_first_attempt_that_hangs_is_retried_after_its_limit_not_the_proxys_five_seconds() {
        let started = Instant::now();
        let calls = Cell::new(0);
        let seen = RefCell::new(Vec::new());
        let ready = wait_until_ready(
            started + Duration::from_secs(90),
            || async {
                calls.set(calls.get() + 1);
                if calls.get() == 1 {
                    // The SOCKS proxy sitting on the SYN: the future never completes on its own.
                    std::future::pending().await
                } else {
                    Ok((200, "{}".to_string()))
                }
            },
            |n, took, outcome| {
                seen.borrow_mut().push((n, took, outcome));
                async {}
            },
        )
        .await;
        assert!(ready);
        assert_eq!(calls.get(), 2);
        // Retried right after the 750 ms limit: no 500 ms pause, and nowhere near 5 s.
        assert_eq!(started.elapsed(), READY_ATTEMPT_FIRST);
        let seen = seen.into_inner();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].1, READY_ATTEMPT_FIRST);
        assert!(seen[0].2.starts_with("no answer within"));
        assert_eq!(seen[1].2, "ready");
    }

    #[tokio::test(start_paused = true)]
    async fn a_fast_failure_waits_the_retry_pause_before_the_next_attempt() {
        let started = Instant::now();
        let calls = Cell::new(0);
        let ready = wait_until_ready(
            started + Duration::from_secs(90),
            || async {
                calls.set(calls.get() + 1);
                if calls.get() < 3 {
                    Err(anyhow!("connection refused"))
                } else {
                    Ok((200, String::new()))
                }
            },
            |_, _, _| async {},
        )
        .await;
        assert!(ready);
        assert_eq!(started.elapsed(), READY_RETRY_PAUSE * 2);
    }

    #[tokio::test(start_paused = true)]
    async fn an_agentd_that_never_answers_ends_the_wait_at_the_overall_deadline() {
        let started = Instant::now();
        let tooks = RefCell::new(Vec::new());
        let ready = wait_until_ready(
            started + Duration::from_secs(90),
            || async { std::future::pending().await },
            |_, took, _| {
                tooks.borrow_mut().push(took);
                async {}
            },
        )
        .await;
        assert!(!ready);
        let tooks = tooks.into_inner();
        assert_eq!(
            &tooks[..4],
            [
                READY_ATTEMPT_FIRST,
                Duration::from_millis(1500),
                READY_ATTEMPT_CAP,
                READY_ATTEMPT_CAP
            ]
        );
        // Every attempt was bounded, so the wait ends within one cap of the 90 s deadline.
        let elapsed = started.elapsed();
        assert!(
            elapsed > Duration::from_secs(90) && elapsed <= Duration::from_secs(90) + READY_ATTEMPT_CAP,
            "{elapsed:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_non_200_answer_is_not_ready() {
        let started = Instant::now();
        let calls = Cell::new(0);
        let ready = wait_until_ready(
            started + Duration::from_secs(90),
            || async {
                calls.set(calls.get() + 1);
                Ok((if calls.get() == 1 { 503 } else { 200 }, String::new()))
            },
            |_, _, _| async {},
        )
        .await;
        assert!(ready);
        assert_eq!(calls.get(), 2);
    }
}
