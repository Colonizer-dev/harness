//! Status classification and `Retry-After` parsing.

use super::*;

#[test]
fn only_record_level_refusals_are_refused() {
    for status in [401, 403, 407] {
        assert_eq!(classify(status), Class::Unauthorized, "{status}");
    }
    for status in [400, 413, 422] {
        assert_eq!(classify(status), Class::Refused, "{status}");
    }
    for status in [404, 405, 408, 415, 429, 500, 502, 503, 504] {
        assert_eq!(classify(status), Class::Retry, "{status} must keep the batch");
    }
}

#[test]
fn retry_after_takes_seconds_and_http_dates_and_is_capped() {
    // 1994-11-06T08:49:37Z, the RFC's own example.
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(784_111_777);
    assert_eq!(retry_after("3", now), Some(Duration::from_secs(3)));
    assert_eq!(retry_after(" 0 ", now), Some(Duration::ZERO));
    assert_eq!(retry_after("86400", now), Some(MAX_RETRY_AFTER));
    assert_eq!(
        retry_after("Sun, 06 Nov 1994 08:49:47 GMT", now),
        Some(Duration::from_secs(10))
    );
    assert_eq!(
        retry_after("Sunday, 06-Nov-94 08:50:37 GMT", now),
        Some(Duration::from_secs(60))
    );
    assert_eq!(retry_after("Sun Nov  6 08:49:57 1994", now), Some(Duration::from_secs(20)));
    assert_eq!(
        retry_after("Sun, 06 Nov 1994 08:00:00 GMT", now),
        Some(Duration::ZERO),
        "a date in the past"
    );
    assert_eq!(retry_after("Mon, 06 Nov 1995 08:49:37 GMT", now), Some(MAX_RETRY_AFTER));
    assert_eq!(retry_after("soon", now), None);
    assert_eq!(retry_after("-5", now), None);
}
