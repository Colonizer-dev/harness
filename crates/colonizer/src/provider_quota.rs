//! Provider quota exhaustion (issue #225): telling "the plan ran out" apart from transport failure
//! and model refusal, so an exhausted provider parks colonies and pauses the queue instead of
//! retrying against an empty plan. Pure, so the gateway error path and the colony-side turn-end
//! scan share it. Parking reuses `Stopped` plus this module's attention reason until #213 adds
//! `Parked`.

use chrono::{DateTime, Datelike, TimeZone, Utc};

/// The attention reason a quota-parked colony carries, shared with #230.
pub const QUOTA_EXHAUSTED_REASON: &str = "provider_quota_exhausted";
/// The gateway fallback marker for a quota answer, read by the colony router like 502/503/504.
pub const QUOTA_FALLBACK: &str = "provider_quota_exhausted";
/// How long a reset-less exhaustion record counts: without a reset the provider named no recovery,
/// so the record lapses after 15 minutes and the queue re-probes (resume → re-hit → re-park)
/// instead of parking forever.
pub const QUOTA_DEFAULT_TTL_SECS: i64 = 15 * 60;

/// What the classifier found: when the plan refills, as the provider worded it and as a timestamp,
/// and whether the cap is the Claude account's own (a session/usage limit names no provider id, so
/// the colony-side park marks every provider instead of none).
pub struct QuotaExhaustion {
    pub reset_at: Option<String>,
    pub reset_unix: Option<i64>,
    pub account_wide: bool,
}

/// Whether `message` says the provider's plan ran out — never a bare rate limit, a refused model
/// or a transport failure. Only an eligible status counts: 429/403, or 0 (unknown — the colony-side
/// scan only has the turn text). Any other nonzero status is not exhaustion, whatever it says.
pub fn classify_quota_exhaustion(status: u16, kind: &str, message: &str) -> Option<QuotaExhaustion> {
    classify_quota_at(status, kind, message, Utc::now())
}

fn classify_quota_at(status: u16, kind: &str, message: &str, now: DateTime<Utc>) -> Option<QuotaExhaustion> {
    if status != 0 && status != 429 && status != 403 {
        return None;
    }
    let text = message.to_lowercase();
    let has = |p: &str| text.contains(p);
    // "usage limit" is the weak one: a bare mention with no limit/reset phrasing is a generic 429.
    let reset_words = ["reset", "until", "renew", "reach"].iter().any(|w| has(w));
    // A bare plan/billing + quota mention is advice ("check your plan to manage your quota"), not
    // exhaustion: it only counts beside an exhaustion verb. The strong phrases stand alone.
    let exhaustion_verb = has("exhaust")
        || has("deplet")
        || has("expir")
        || has("exceed")
        || has("out of")
        || has("used up")
        || (has("no ") && (has("remaining") || has("balance")))
        || reached_limit(&text);
    let exhausted = has("insufficient_quota")
        || kind.to_lowercase().contains("insufficient_quota")
        || has("quota has been exhausted")
        || has("quota exhausted")
        || has("quota_exhausted")
        || ((has("token-plan") || has("token plan")) && (has("limit") || has("reset")))
        || has("weekly limit")
        || ((has("billing") || has("plan")) && has("quota") && exhaustion_verb)
        || (has("usage limit") && reset_words)
        // "session limit" is the Claude account's own cap (`You've hit your session limit · resets
        // 7am`), not one routed provider's plan: the phrase itself is rarely advisory, but a bare
        // mention with neither reset phrasing nor an exhaustion verb ("Session limit: 10 concurrent
        // runs") still reads as a dashboard line, so it takes the same reset guard as "usage limit" —
        // widened with the verb, since "reached your session limit" names exhaustion without naming a
        // reset. A bare "hit your session limit" with no reset stays out: "hit" alone is too common
        // to promote on.
        || ((has("session limit") || has("session_limit")) && (reset_words || exhaustion_verb));
    if !exhausted {
        return None;
    }
    let account_wide = has("session limit") || has("session_limit");
    let (reset_at, reset_unix) = extract_reset(message, now);
    Some(QuotaExhaustion {
        reset_at,
        reset_unix,
        account_wide,
    })
}

/// `reached ... limit` in order: "you have reached your weekly limit", "reached the plan's limit".
fn reached_limit(text: &str) -> bool {
    text.find("reach").is_some_and(|r| text[r..].contains("limit"))
}

/// A reset instant out of provider prose, as raw words and unix time: ISO8601, then `Mon DD
/// [HH:MM[am]]`, then `MM-DD HH:MM[:SS]`, then a dateless clock time, all UTC. A month and day land
/// on this year, or next when only just passed; more than ~24h stale, or unrepresentable (Feb 29),
/// reads as reset-less. `None` when the message names no reset.
fn extract_reset(message: &str, now: DateTime<Utc>) -> (Option<String>, Option<i64>) {
    if let Some(hit) = iso_reset(message) {
        return hit;
    }
    if let Some(hit) = named_reset(message, now) {
        return hit;
    }
    if let Some(hit) = numeric_reset(message, now) {
        return hit;
    }
    if let Some(hit) = time_only_reset(message, now) {
        return hit;
    }
    (None, None)
}

/// An RFC3339 instant anywhere in the message, e.g. `2026-09-23T07:54:00Z`.
fn iso_reset(message: &str) -> Option<(Option<String>, Option<i64>)> {
    for token in message.split_whitespace() {
        let raw = token.trim_matches(|c: char| matches!(c, ',' | ')' | '(' | '"' | '\'' | '.'));
        if raw.len() > 10
            && raw.as_bytes()[4] == b'-'
            && raw.contains('T')
            && let Ok(parsed) = DateTime::parse_from_rfc3339(raw)
        {
            return Some((Some(raw.to_string()), Some(parsed.timestamp())));
        }
    }
    None
}

const MONTHS: [(&str, u32); 12] = [
    ("january", 1),
    ("february", 2),
    ("march", 3),
    ("april", 4),
    ("may", 5),
    ("june", 6),
    ("july", 7),
    ("august", 8),
    ("september", 9),
    ("october", 10),
    ("november", 11),
    ("december", 12),
];

fn month_number(word: &str) -> Option<u32> {
    let word = word.to_lowercase();
    MONTHS.iter().find(|(name, _)| word.starts_with(&name[..3])).map(|(_, n)| *n)
}

/// `Sep 23, 5am (UTC)` style: a month name, a day, and an optional time.
fn named_reset(message: &str, now: DateTime<Utc>) -> Option<(Option<String>, Option<i64>)> {
    let bytes = message.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_alphabetic() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                i += 1;
            }
            if let Some(month) = month_number(&message[start..i]) {
                // A bare month, or one followed by a longer number (a year), is not a reset: keep
                // scanning instead of bailing, so a later real reset still parses.
                let mut j = i;
                let Some(day) = skip_to_number(bytes, &mut j) else { continue };
                if bytes.get(j).is_some_and(|b| b.is_ascii_digit()) {
                    continue;
                }
                let (hour, min) = read_time(bytes, &mut j).unwrap_or((0, 0));
                skip_utc(bytes, &mut j);
                let raw = message[start..j].trim().to_string();
                let unix = place(month, day, hour, min, now);
                return Some((Some(raw), unix));
            }
            continue;
        }
        i += 1;
    }
    None
}

/// `09-23 07:54:00 UTC` style: a numeric month and day with a clock time.
fn numeric_reset(message: &str, now: DateTime<Utc>) -> Option<(Option<String>, Option<i64>)> {
    let bytes = message.as_bytes();
    let digit = |at: usize| bytes.get(at).is_some_and(|b| b.is_ascii_digit());
    let mut i = 0;
    while i + 11 <= bytes.len() {
        // All slices below are ASCII digits, so the byte indices are char boundaries.
        if digit(i)
            && digit(i + 1)
            && bytes[i + 2] == b'-'
            && digit(i + 3)
            && digit(i + 4)
            && bytes[i + 5] == b' '
            && digit(i + 6)
            && digit(i + 7)
            && bytes[i + 8] == b':'
            && digit(i + 9)
            && digit(i + 10)
        {
            let month: u32 = message[i..i + 2].parse().unwrap_or(0);
            let day: u32 = message[i + 3..i + 5].parse().unwrap_or(0);
            let hour: u32 = message[i + 6..i + 8].parse().unwrap_or(99);
            let min: u32 = message[i + 9..i + 11].parse().unwrap_or(99);
            if matches!(month, 1..=12) && matches!(day, 1..=31) && hour < 24 && min < 60 {
                let mut j = i + 11;
                if j + 3 <= bytes.len() && bytes[j] == b':' && digit(j + 1) && digit(j + 2) {
                    j += 3; // seconds ride along; the minute is what the queue waits on
                }
                skip_utc(bytes, &mut j);
                let raw = message[i..j].trim().to_string();
                return Some((Some(raw), place(month, day, hour, min, now)));
            }
        }
        i += 1;
    }
    None
}

/// `resets 7am` style: a clock time with no date — the session-limit class names only an hour. The
/// next future occurrence in UTC (later today when still ahead, else tomorrow), so a `7am` refill
/// parks until morning instead of lapsing on the 15-minute TTL and re-hitting all night. Only a
/// time that carries its own clock evidence counts: an am/pm marker, or an `HH:MM` pinned to UTC.
/// A bare hour or a zoneless `HH:MM` stays reset-less — counts, durations and stray stamps ("retry
/// in 30 minutes", "backoff 07:54") are not refills.
fn time_only_reset(message: &str, now: DateTime<Utc>) -> Option<(Option<String>, Option<i64>)> {
    let bytes = message.as_bytes();
    let digit = |at: usize| bytes.get(at).is_some_and(|b| b.is_ascii_digit());
    let mut i = 0;
    while i < bytes.len() {
        // A clock starts its own token: a digit the prose before it did not start.
        if !bytes[i].is_ascii_digit() || (i > 0 && bytes[i - 1].is_ascii_alphanumeric()) {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i - start > 2 {
            continue; // a longer run is a count or a year, not an hour
        }
        let mut hour: u32 = std::str::from_utf8(&bytes[start..i]).ok()?.parse().ok()?;
        let mut min = 0;
        let clock = bytes.get(i) == Some(&b':') && digit(i + 1) && digit(i + 2);
        if clock {
            min = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?.parse().ok()?;
            if min > 59 {
                continue;
            }
            i += 3;
            if bytes.get(i) == Some(&b':') && digit(i + 1) && digit(i + 2) {
                i += 3; // seconds ride along, as in the numeric arm
            }
        }
        while i < bytes.len() && bytes[i] == b' ' {
            i += 1;
        }
        let mut meridiem: Option<bool> = None; // Some(is_pm)
        if i + 2 <= bytes.len() {
            meridiem = match &bytes[i..i + 2] {
                [b'a', b'm'] | [b'A', b'M'] => Some(false),
                [b'p', b'm'] | [b'P', b'M'] => Some(true),
                _ => None,
            };
            if meridiem.is_some() {
                // `7amazing` is prose, not a clock: the marker needs a word edge after it.
                if bytes.get(i + 2).is_some_and(|b| b.is_ascii_alphanumeric()) {
                    continue;
                }
                i += 2;
            }
        }
        match meridiem {
            Some(pm) => {
                if hour == 0 || hour > 12 {
                    continue;
                }
                hour = hour % 12 + u32::from(pm) * 12;
                // A trailing zone belongs to the raw reset words, as in the other arms.
                take_utc(bytes, &mut i);
            }
            None => {
                if !clock || hour > 23 {
                    continue;
                }
                if !take_utc(bytes, &mut i) {
                    continue; // a zoneless `HH:MM` is a stray stamp, not a refill
                }
            }
        }
        let raw = message[start..i].trim().to_string();
        return Some((Some(raw), Some(place_time(hour, min, now))));
    }
    None
}

/// Today at `hour:min` UTC when still ahead, else tomorrow: a dateless clock time always names a
/// future refill, and every `HH:MM` exists every day, so unlike [`place`] this never reads reset-less.
fn place_time(hour: u32, min: u32, now: DateTime<Utc>) -> i64 {
    let today = now
        .date_naive()
        .and_hms_opt(hour, min, 0)
        .expect("the time-only arm validates its clock");
    let noon = Utc.from_utc_datetime(&today);
    if noon > now {
        noon.timestamp()
    } else {
        (noon + chrono::Duration::days(1)).timestamp()
    }
}

/// Skips spaces, commas and brackets, then reads a 1-2 digit number.
fn skip_to_number(bytes: &[u8], j: &mut usize) -> Option<u32> {
    while *j < bytes.len() && (bytes[*j] == b' ' || bytes[*j] == b',' || bytes[*j] == b'(') {
        *j += 1;
    }
    let start = *j;
    while *j < bytes.len() && bytes[*j].is_ascii_digit() {
        *j += 1;
    }
    if start == *j || *j - start > 2 {
        return None;
    }
    Some(bytes[start..*j].iter().fold(0, |n, b| n * 10 + u32::from(*b - b'0')))
}

/// An optional `5am` / `07:54` after the day; midnight when the day stands alone.
fn read_time(bytes: &[u8], j: &mut usize) -> Option<(u32, u32)> {
    while *j < bytes.len() && (bytes[*j] == b' ' || bytes[*j] == b',') {
        *j += 1;
    }
    let start = *j;
    while *j < bytes.len() && bytes[*j].is_ascii_digit() {
        *j += 1;
    }
    if start == *j || *j - start > 2 {
        return None;
    }
    let mut hour: u32 = std::str::from_utf8(&bytes[start..*j]).ok()?.parse().ok()?;
    let mut min = 0;
    if *j < bytes.len() && bytes[*j] == b':' && *j + 3 <= bytes.len() {
        min = std::str::from_utf8(&bytes[*j + 1..*j + 3]).ok()?.parse().ok()?;
        if min > 59 {
            return None;
        }
        *j += 3;
    }
    while *j < bytes.len() && bytes[*j] == b' ' {
        *j += 1;
    }
    if *j + 2 <= bytes.len() {
        match &bytes[*j..*j + 2] {
            [b'a', b'm'] | [b'A', b'M'] => {
                hour %= 12;
                *j += 2;
            }
            [b'p', b'm'] | [b'P', b'M'] => {
                hour = hour % 12 + 12;
                *j += 2;
            }
            _ => {}
        }
    }
    (hour < 24).then_some((hour, min))
}

/// Consumes a trailing `UTC` or `(UTC)` when one is really there; otherwise leaves `j` alone, so
/// a parenthetical that is not a zone ("07:54 (see dashboard)") neither parses nor pollutes the raw
/// words. Stricter than [`skip_utc`], which the date-bearing arms use.
fn take_utc(bytes: &[u8], j: &mut usize) -> bool {
    let mut k = *j;
    while k < bytes.len() && bytes[k] == b' ' {
        k += 1;
    }
    let paren = bytes.get(k) == Some(&b'(');
    if paren {
        k += 1;
    }
    if k + 3 > bytes.len() || !bytes[k..k + 3].eq_ignore_ascii_case(b"utc") {
        return false;
    }
    k += 3;
    if paren && bytes.get(k) == Some(&b')') {
        k += 1;
    }
    *j = k;
    true
}

/// A trailing `UTC` or `(UTC)` belongs to the raw reset words.
fn skip_utc(bytes: &[u8], j: &mut usize) {
    while *j < bytes.len() && (bytes[*j] == b' ' || bytes[*j] == b'(') {
        *j += 1;
    }
    if *j + 3 <= bytes.len() && bytes[*j..*j + 3].eq_ignore_ascii_case(b"utc") {
        *j += 3;
        if *j < bytes.len() && bytes[*j] == b')' {
            *j += 1;
        }
    }
}

/// This year when still ahead; next year when the date only just passed (clock skew, a slow
/// retry). A reset more than ~24h in the past is stale words, not next year's plan — reset-less
/// (None), so the record expires on TTL instead of parking the queue for a year. Feb 29 on a
/// non-leap year is unrepresentable: reset-less too.
fn place(month: u32, day: u32, hour: u32, min: u32, now: DateTime<Utc>) -> Option<i64> {
    match Utc.with_ymd_and_hms(now.year(), month, day, hour, min, 0).single() {
        Some(date) if date > now => Some(date.timestamp()),
        Some(date) if now.signed_duration_since(date) <= chrono::Duration::hours(24) => Utc
            .with_ymd_and_hms(now.year() + 1, month, day, hour, min, 0)
            .single()
            .map(|d| d.timestamp()),
        _ => None,
    }
}

/// Which provider `text` names, case-insensitively: how turn text and parked errors are tied back
/// to a provider id. Ids win over display names (a prose "Claude" must not claim a provider merely
/// named after the model), and a candidate only matches on word boundaries — a short id inside a
/// longer word is prose, not attribution.
pub fn mentioned_provider(text: &str, ids: &[String], names: &[String]) -> Option<String> {
    let lower = text.to_lowercase();
    ids.iter()
        .find(|c| contains_word(&lower, &c.to_lowercase()))
        .or_else(|| names.iter().find(|c| contains_word(&lower, &c.to_lowercase())))
        .cloned()
}

/// Whether `needle` occurs in already-lowercased `haystack` delimited by non-alphanumerics (or the
/// string edges) on both sides. Byte-wise over `as_bytes`, so multibyte prose can never panic it.
fn contains_word(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    let (h, n) = (haystack.as_bytes(), needle.as_bytes());
    let mut i = 0;
    while i + n.len() <= h.len() {
        if &h[i..i + n.len()] == n
            && (i == 0 || !h[i - 1].is_ascii_alphanumeric())
            && (i + n.len() == h.len() || !h[i + n.len()].is_ascii_alphanumeric())
        {
            return true;
        }
        i += 1;
    }
    false
}

/// True while a record with this reset and `since` still counts as exhausted at `now`: a named
/// reset ahead, or — reset-less — a mark younger than [`QUOTA_DEFAULT_TTL_SECS`]. What the
/// gateway's verdicts and the queue's admission share, so TTL-expired reads as recovered
/// everywhere at once.
pub fn quota_active(reset_unix: Option<i64>, since: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    match reset_unix {
        Some(t) => t > now.timestamp(),
        None => now.signed_duration_since(since).num_seconds() < QUOTA_DEFAULT_TTL_SECS,
    }
}

/// One provider's quota state for the queue's admission decision.
pub struct ProviderQuota {
    pub id: String,
    pub exhausted: bool,
    pub reset_at: Option<String>,
    pub reset_unix: Option<i64>,
    /// Referenced by a model role (`used_by` non-empty): traffic actually routes here.
    pub routable: bool,
}

/// Why the queue holds every colony, for `/api/status` and the overview banner.
pub struct QuotaPause {
    pub reason: String,
    pub reset_at: Option<String>,
    pub reset_unix: Option<i64>,
    pub providers: Vec<String>,
}

/// The queue holds when every routable provider is exhausted and at least one is: with no routable
/// provider at all, every provider counts instead, so a fleet of spares still pauses together.
/// `None` means admit as usual — including when nothing is exhausted, and when no provider exists.
pub fn quota_pause(states: &[ProviderQuota], waiting: usize) -> Option<QuotaPause> {
    if states.is_empty() {
        return None;
    }
    let routable: Vec<&ProviderQuota> = {
        let used: Vec<&ProviderQuota> = states.iter().filter(|s| s.routable).collect();
        if used.is_empty() { states.iter().collect() } else { used }
    };
    let exhausted: Vec<&ProviderQuota> = routable.iter().filter(|s| s.exhausted).copied().collect();
    if exhausted.is_empty() || exhausted.len() < routable.len() {
        return None;
    }
    let reset_unix = exhausted.iter().filter_map(|s| s.reset_unix).min();
    let reset_at = reset_unix.and_then(|t| {
        exhausted
            .iter()
            .find(|s| s.reset_unix == Some(t))
            .and_then(|s| s.reset_at.clone())
    });
    let providers: Vec<String> = exhausted.iter().map(|s| s.id.clone()).collect();
    let reason = match &reset_at {
        Some(reset) => format!(
            "queue paused — {} quota exhausted, resets {reset} ({waiting} waiting)",
            providers.join(", ")
        ),
        None => format!("queue paused — {} quota exhausted ({waiting} waiting)", providers.join(", ")),
    };
    Some(QuotaPause {
        reason,
        reset_at,
        reset_unix,
        providers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        // Just before the resets below, so they land this year, not next.
        chrono::DateTime::parse_from_rfc3339("2026-09-21T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn state(id: &str, exhausted: bool, reset_unix: Option<i64>, routable: bool) -> ProviderQuota {
        ProviderQuota {
            id: id.into(),
            exhausted,
            reset_at: reset_unix.map(|_| "reset".into()),
            reset_unix,
            routable,
        }
    }

    #[test]
    fn the_bailian_quota_text_parks_with_its_reset() {
        let message = "Your quota has been exhausted. Your quota will reset on 09-23 07:54:00 UTC. \
            Please reduce your request frequency or go to the console to increase your quota.";
        let hit = classify_quota_at(429, "rate_limit_error", message, now()).expect("quota wording is a hit");
        assert_eq!(hit.reset_at.as_deref(), Some("09-23 07:54:00 UTC"));
        assert_eq!(
            hit.reset_unix,
            Some(Utc.with_ymd_and_hms(2026, 9, 23, 7, 54, 0).unwrap().timestamp())
        );
    }

    #[test]
    fn the_claude_weekly_limit_text_parks_with_its_reset() {
        let message = "This account has reached its weekly limit for Claude. Your limit resets Sep 23, 5am (UTC). \
            Upgrade or wait for the reset.";
        let hit = classify_quota_at(403, "permission_error", message, now()).expect("weekly limit is a hit");
        assert_eq!(hit.reset_at.as_deref(), Some("Sep 23, 5am (UTC)"));
        assert_eq!(
            hit.reset_unix,
            Some(Utc.with_ymd_and_hms(2026, 9, 23, 5, 0, 0).unwrap().timestamp())
        );
    }

    #[test]
    fn a_generic_429_without_quota_words_is_not_exhaustion() {
        assert!(
            classify_quota_at(
                429,
                "rate_limit_error",
                "Rate limit exceeded. Please retry after 30 seconds.",
                now()
            )
            .is_none()
        );
    }

    #[test]
    fn only_429_403_and_unknown_statuses_can_be_exhaustion() {
        let wording = "quota exhausted, retry later";
        assert!(
            classify_quota_at(500, "", wording, now()).is_none(),
            "a 500 is transport failure"
        );
        assert!(classify_quota_at(400, "", wording, now()).is_none(), "a 400 is a refusal");
        assert!(
            classify_quota_at(502, "insufficient_quota", wording, now()).is_none(),
            "status gates the kind too"
        );
        let bailian = "Your quota has been exhausted. Your quota will reset on 09-23 07:54:00 UTC.";
        assert!(
            classify_quota_at(0, "", bailian, now()).is_some(),
            "the text-only scan has no status"
        );
    }

    #[test]
    fn plan_and_billing_quota_needs_an_exhaustion_verb() {
        assert!(classify_quota_at(429, "", "your plan's rate quota was exceeded, retry shortly", now()).is_some());
        assert!(
            classify_quota_at(
                429,
                "",
                "You exceeded your current quota, please check your plan and billing details",
                now()
            )
            .is_some(),
            "the OpenAI classic still parks"
        );
        assert!(
            classify_quota_at(429, "", "check your plan and billing details to manage your quota", now()).is_none(),
            "advice without an exhaustion verb is not exhaustion"
        );
    }

    #[test]
    fn a_stale_reset_is_reset_less_not_next_year() {
        // Sep 10 is 11 days past: stale words, not a year of exhaustion.
        let hit = classify_quota_at(429, "", "quota exhausted, resets Sep 10.", now()).expect("hit");
        assert_eq!(hit.reset_at.as_deref(), Some("Sep 10"));
        assert!(hit.reset_unix.is_none(), "stale resets expire on TTL");
        // An hour past is clock skew, not staleness: still rolls forward.
        let hit = classify_quota_at(429, "", "quota exhausted, resets Sep 20, 23:00.", now()).expect("hit");
        assert_eq!(
            hit.reset_unix,
            Some(Utc.with_ymd_and_hms(2027, 9, 20, 23, 0, 0).unwrap().timestamp())
        );
    }

    #[test]
    fn feb_29_on_a_non_leap_year_is_reset_less() {
        let hit = classify_quota_at(429, "", "quota exhausted, resets Feb 29.", now()).expect("hit");
        assert!(hit.reset_unix.is_none(), "2026 has no Feb 29");
    }

    #[test]
    fn a_reset_less_record_lapses_on_ttl() {
        let since = now();
        assert!(quota_active(None, since, since), "a fresh mark counts");
        assert!(
            !quota_active(None, since, since + chrono::Duration::seconds(QUOTA_DEFAULT_TTL_SECS + 1)),
            "past TTL reads as recovered"
        );
        assert!(
            quota_active(Some(since.timestamp() + 60), since, since),
            "a future reset counts"
        );
        assert!(
            !quota_active(Some(since.timestamp() - 1), since, since),
            "a passed reset reads as recovered"
        );
    }

    #[test]
    fn attribution_needs_whole_words_and_prefers_ids() {
        let ids = vec!["zai".to_string()];
        let names = vec!["Claude Relay".to_string()];
        assert_eq!(
            mentioned_provider("retry on Claude", &ids, &names),
            None,
            "neither the id nor the full name is named"
        );
        assert_eq!(
            mentioned_provider("provider quota exhausted (zai, resets 09-23 07:54 UTC)", &ids, &names),
            Some("zai".to_string()),
            "the id as a whole word attributes"
        );
        assert_eq!(
            mentioned_provider("the pizzaiolo special", &ids, &names),
            None,
            "a short id inside a word is prose"
        );
        let ids = vec!["zai".to_string(), "bailian".to_string()];
        let names = vec!["ZAI".to_string(), "Bailian Cloud".to_string()];
        assert_eq!(
            mentioned_provider("Bailian Cloud exhausted; see zai docs", &ids, &names),
            Some("zai".to_string()),
            "an id match wins over a display-name match"
        );
    }

    #[test]
    fn insufficient_quota_is_exhaustion_even_without_a_reset() {
        let hit = classify_quota_at(
            403,
            "permission_error",
            "You exceeded your current quota, please check your plan and billing details. insufficient_quota",
            now(),
        )
        .expect("insufficient_quota is a hit");
        assert!(hit.reset_at.is_none() && hit.reset_unix.is_none());
    }

    #[test]
    fn a_model_refusal_is_not_exhaustion() {
        assert!(
            classify_quota_at(
                400,
                "invalid_request_error",
                "Invalid request: the model 'frobnicate-9' does not exist.",
                now()
            )
            .is_none()
        );
    }

    #[test]
    fn usage_limit_counts_only_with_reset_phrasing() {
        assert!(classify_quota_at(429, "rate_limit_error", "Usage limit exceeded for this key.", now()).is_none());
        assert!(
            classify_quota_at(
                429,
                "rate_limit_error",
                "You have reached your usage limit. Your quota resets tomorrow at 07:54 UTC.",
                now()
            )
            .is_some()
        );
    }

    #[test]
    fn an_iso_reset_parses_with_its_year() {
        let hit = classify_quota_at(429, "", "quota exhausted; resets 2026-09-23T07:54:00Z.", now()).expect("hit");
        assert_eq!(
            hit.reset_unix,
            Some(Utc.with_ymd_and_hms(2026, 9, 23, 7, 54, 0).unwrap().timestamp())
        );
    }

    #[test]
    fn the_queue_pauses_only_when_nothing_routable_is_healthy() {
        let future = Some(1_789_000_000);
        let all_out = vec![state("bailian", true, future, true), state("zai", true, future, true)];
        let pause = quota_pause(&all_out, 3).expect("every routable provider exhausted");
        assert!(
            pause.reason.contains("bailian") && pause.reason.contains("3 waiting"),
            "{}",
            pause.reason
        );
        assert_eq!(pause.providers.len(), 2);

        let one_healthy = vec![state("bailian", true, future, true), state("zai", false, None, true)];
        assert!(quota_pause(&one_healthy, 3).is_none(), "a healthy provider admits");

        let idle_spare = vec![state("bailian", true, future, true), state("spare", true, future, false)];
        assert!(
            quota_pause(&idle_spare, 0).is_some(),
            "an unused spare does not unpause the queue"
        );

        assert!(quota_pause(&[], 0).is_none(), "no providers, no pause");
        assert!(
            quota_pause(&[state("bailian", false, None, true)], 1).is_none(),
            "nothing exhausted, no pause"
        );
    }

    #[test]
    fn the_claude_session_limit_text_parks_account_wide_with_its_hour() {
        let message = "You've hit your session limit · resets 7am (UTC)";
        let hit = classify_quota_at(0, "", message, now()).expect("session limit is a hit");
        assert!(hit.account_wide, "no provider id can own the account cap");
        assert_eq!(hit.reset_at.as_deref(), Some("7am (UTC)"));
        assert_eq!(
            hit.reset_unix,
            Some(Utc.with_ymd_and_hms(2026, 9, 21, 7, 0, 0).unwrap().timestamp()),
            "later today, still ahead of midnight"
        );
    }

    #[test]
    fn session_limit_times_parse_to_the_next_future_hour() {
        // An evening now rolls a morning reset to tomorrow.
        let evening = Utc.with_ymd_and_hms(2026, 9, 21, 20, 0, 0).unwrap();
        let hit = classify_quota_at(0, "", "session limit reached, resets 7am", evening).expect("hit");
        assert_eq!(
            hit.reset_unix,
            Some(Utc.with_ymd_and_hms(2026, 9, 22, 7, 0, 0).unwrap().timestamp())
        );
        assert!(hit.account_wide);
        // `HH:MM UTC` and `H:MMpm` shapes parse too.
        let hit = classify_quota_at(0, "", "session limit exceeded; resets 07:00 UTC", now()).expect("hit");
        assert_eq!(hit.reset_at.as_deref(), Some("07:00 UTC"));
        assert_eq!(
            hit.reset_unix,
            Some(Utc.with_ymd_and_hms(2026, 9, 21, 7, 0, 0).unwrap().timestamp())
        );
        let hit = classify_quota_at(0, "", "session limit exceeded; resets 7:30pm", now()).expect("hit");
        assert_eq!(hit.reset_at.as_deref(), Some("7:30pm"));
        assert_eq!(
            hit.reset_unix,
            Some(Utc.with_ymd_and_hms(2026, 9, 21, 19, 30, 0).unwrap().timestamp())
        );
        // A reached form with no clock still parks, reset-less on TTL.
        let hit = classify_quota_at(0, "", "You have reached your session limit.", now()).expect("hit");
        assert!(hit.reset_unix.is_none() && hit.account_wide);
    }

    #[test]
    fn a_bare_session_limit_mention_is_not_exhaustion() {
        assert!(
            classify_quota_at(429, "", "Session limit: 10 concurrent runs per account.", now()).is_none(),
            "a dashboard line with no reset and no exhaustion verb"
        );
        assert!(
            classify_quota_at(0, "", "You've hit your session limit", now()).is_none(),
            "bare 'hit' with no reset stays out — too common to promote on"
        );
        assert!(
            classify_quota_at(500, "", "You've hit your session limit · resets 7am (UTC)", now()).is_none(),
            "status still gates the account class"
        );
    }

    #[test]
    fn durations_and_zoneless_stamps_are_not_resets() {
        let hit = classify_quota_at(429, "", "quota exhausted, retry in 30 seconds", now()).expect("hit");
        assert!(
            hit.reset_at.is_none() && hit.reset_unix.is_none(),
            "a duration is not a refill"
        );
        let hit = classify_quota_at(429, "", "quota exhausted; backoff until 07:54 then retry", now()).expect("hit");
        assert!(hit.reset_unix.is_none(), "a zoneless stamp stays reset-less");
        assert!(!hit.account_wide, "the provider class is not account-wide");
        let bailian = "Your quota has been exhausted. Your quota will reset on 09-23 07:54:00 UTC.";
        let hit = classify_quota_at(429, "rate_limit_error", bailian, now()).expect("hit");
        assert!(!hit.account_wide, "a named plan stays provider-scoped");
    }
}
