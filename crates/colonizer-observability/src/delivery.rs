//! The delivery loop the backfill (#858) and the fleet relay (#861) share: send a queue of
//! requests in order, bisect a record-level refusal down to the record at fault, and say how long
//! to wait when the backend asks for a retry.

use crate::encode::Request;
use crate::exporter::{BACKOFF_START, halves, next_backoff};
use crate::transport::{Outcome, Transport};
use std::time::Duration;

/// A delivery that did not finish.
pub(crate) enum Held {
    /// Retry after a backoff, or the server's `Retry-After` when it gave one.
    Retry(Option<Duration>),
    /// The credential was refused: nothing in the queue is dropped.
    Unauthorized(String),
}

/// One acknowledged request, for whoever is counting: its records, how many of those the backend
/// declined (`rejected_log_records` and friends), and its bytes.
#[derive(Clone, Copy)]
pub(crate) struct Ack {
    pub(crate) records: u64,
    pub(crate) rejected: u64,
    pub(crate) bytes: u64,
}

/// Sends `requests` in order, reporting each acknowledged one through `on_sent` and returning the
/// records dropped to record-level refusals: a refusal is bisected with the exporter's own halves
/// down to the record at fault, which is dropped rather than retried forever. On the first outcome
/// worth retrying the rest stays queued and the hold comes back.
pub(crate) async fn deliver(
    transport: &Transport,
    requests: &mut Vec<Request>,
    mut on_sent: impl FnMut(Ack),
) -> Result<u64, Held> {
    let mut refused = 0;
    while let Some(request) = requests.first().cloned() {
        match transport.send(&request).await {
            Outcome::Sent { rejected, bytes, .. } => {
                requests.remove(0);
                on_sent(Ack {
                    records: request.len() as u64,
                    rejected,
                    bytes,
                });
            }
            Outcome::Refused { .. } => {
                requests.remove(0);
                match halves(&request) {
                    Some((a, b)) => {
                        requests.insert(0, b);
                        requests.insert(0, a);
                    }
                    None => refused += request.len() as u64,
                }
            }
            Outcome::Unauthorized { message, .. } => return Err(Held::Unauthorized(message)),
            Outcome::Retry { after, .. } => return Err(Held::Retry(after)),
        }
    }
    Ok(refused)
}

/// A retry hold's delay: the server's `Retry-After` when it gave one (at least a full backoff
/// step), else the next doubling one after `current`.
pub(crate) fn retry_delay(after: Option<Duration>, current: Option<Duration>) -> Duration {
    match after {
        Some(after) => after.max(BACKOFF_START),
        None => next_backoff(current),
    }
}
