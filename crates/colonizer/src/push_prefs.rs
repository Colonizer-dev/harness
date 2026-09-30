//! Per-device push preferences (issue #743): the one place that decides what reaches a device.
//!
//! Every Web Push subscription carries a [`Prefs`] blob, edited from Settings → Notifications and
//! checked by the mothership before anything is sent to that device. Everything that sends a push
//! asks this module, and nothing else, whether and how:
//!
//! - [`allows`]: whether an event reaches the device at all — its event switch, its org/repo scope,
//!   its quiet hours (in the device's own time zone, with an optional question break-through) and
//!   whether a focused cockpit tab on the device is already showing that colony;
//! - [`silent`]: whether the notification may sound (only a question, and only when allowed);
//! - [`answer_actions`]: whether a question's push may carry answer buttons (issue #742's hook);
//! - [`wants_resolved`] and [`badge`]: whether a device takes the silent "resolved" push, and the
//!   app-badge count it is sent (issue #744's hooks).
//!
//! The functions are pure over the preferences, the colony and the clock, so the rules are tested
//! here without a push service. What the payload itself carries is push.rs's business, and stays
//! labels only: nothing here widens it.

use crate::sessions::Session;
use chrono::{DateTime, Offset as _, TimeZone as _, Timelike as _};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::{LazyLock, Mutex};

// ---------------------------------------------------------------------------
// The event catalogue
// ---------------------------------------------------------------------------

/// A question is waiting ([`crate::notify::Event::Question`]).
pub const QUESTION: &str = "question";
/// The colony-less hourly digest of what the soft layers held (notify.rs).
pub const DIGEST: &str = "digest";

/// Every event a device can switch, by the name [`crate::notify`] sends, with its default. The
/// act-on events are on; the ambient ones (a provider's health, the hourly digest) are off. A new
/// event pushes only once it is added here: an unknown name is never sent.
pub const EVENTS: &[(&str, bool)] = &[
    (QUESTION, true),
    ("pull_request", true),
    ("needs_rebase", true),
    ("failed", true),
    ("attention", true),
    ("provider_degraded", false),
    (DIGEST, false),
];

fn event_default(event: &str) -> Option<bool> {
    EVENTS.iter().find(|(name, _)| *name == event).map(|(_, on)| *on)
}

// ---------------------------------------------------------------------------
// The preferences
// ---------------------------------------------------------------------------

/// Quiet hours in minutes since the device's local midnight; `start > end` wraps midnight.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct QuietHours {
    pub start: i32,
    pub end: i32,
}

/// One device's notification preferences. Every field defaults, so a subscription stored before a
/// field existed keeps loading, on the defaults.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// Per-event overrides by event name; a missing key means the event's default in [`EVENTS`].
    pub events: BTreeMap<String, bool>,
    /// Whether a question may make a sound: the one push ever sent with `silent` false.
    pub question_sound: bool,
    /// Whether a question's notification may offer answer buttons (issue #742).
    pub answer_actions: bool,
    /// Whether pushes set the installed app's badge to the needs-you count (issue #744).
    pub badge: bool,
    /// The orgs and repos this device hears about, `org` or `org/repo` entries; empty for all.
    pub scope: Vec<String>,
    pub quiet: Option<QuietHours>,
    pub questions_break_quiet: bool,
    /// The device's IANA time zone. When it names a zone quiet hours follow it, daylight saving
    /// included; otherwise the offset below does.
    pub tz: Option<String>,
    /// Minutes east of UTC the device last reported, the fallback for a zone this build does not know.
    pub utc_offset: i32,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            events: BTreeMap::new(),
            question_sound: true,
            answer_actions: true,
            badge: true,
            scope: Vec::new(),
            quiet: None,
            questions_break_quiet: false,
            tz: None,
            utc_offset: 0,
        }
    }
}

pub const MAX_SCOPE: usize = 50;
pub const MAX_SCOPE_ENTRY: usize = 200;
/// How far from UTC a device may claim to be: ±14 hours is the real-world extreme.
pub const MAX_OFFSET: i32 = 14 * 60;
pub const TZ_RULE: &str = "the time zone name is at most 64 characters and carries no control characters";

/// Whether a time-zone name is storable: short and control-character-free. An unknown name is
/// kept (a newer browser may know zones this build does not) and quiet hours fall back to the offset.
pub fn tz_ok(tz: &str) -> bool {
    tz.chars().count() <= 64 && !tz.chars().any(char::is_control)
}

impl Prefs {
    /// Whether these preferences are storable, and the short line to send when they are not.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(name) = self.events.keys().find(|name| event_default(name).is_none()) {
            let known: Vec<&str> = EVENTS.iter().map(|(name, _)| *name).collect();
            return Err(format!("unknown event {name:?}; known events are {}", known.join(", ")));
        }
        if self.scope.len() > MAX_SCOPE {
            return Err(format!("the scope names more than {MAX_SCOPE} entries"));
        }
        for entry in &self.scope {
            if entry.is_empty() || entry.chars().count() > MAX_SCOPE_ENTRY || entry.contains(char::is_whitespace) {
                return Err(format!(
                    "a scope entry must be 1..={MAX_SCOPE_ENTRY} whitespace-free characters"
                ));
            }
            if entry.matches('/').count() > 1 || entry.starts_with('/') || entry.ends_with('/') {
                return Err(format!("a scope entry is an org or an org/repo, not {entry:?}"));
            }
        }
        if self.tz.as_deref().is_some_and(|tz| !tz_ok(tz)) {
            return Err(TZ_RULE.into());
        }
        if let Some(quiet) = &self.quiet
            && (quiet.start == quiet.end || !(0..1440).contains(&quiet.start) || !(0..1440).contains(&quiet.end))
        {
            return Err("quiet hours are minutes since midnight, 0..1440, and start and end differ".into());
        }
        if self.utc_offset.abs() > MAX_OFFSET {
            return Err(format!("the utc offset is more than {MAX_OFFSET} minutes"));
        }
        Ok(())
    }

    /// Whether the device's switch for this event is on; an event missing from [`EVENTS`] is off.
    pub fn event_on(&self, event: &str) -> bool {
        match event_default(event) {
            Some(default) => self.events.get(event).copied().unwrap_or(default),
            None => false,
        }
    }

    /// Whether a colony is inside the device's scope: an empty scope takes every colony; an entry
    /// matches the colony's org or its `org/repo`, case-insensitively.
    pub fn in_scope(&self, session: &Session) -> bool {
        self.scope.is_empty()
            || self
                .scope
                .iter()
                .any(|entry| entry.eq_ignore_ascii_case(&session.org) || entry.eq_ignore_ascii_case(&session.repo))
    }

    /// The device's minute of its local day at `now` (unix seconds): its IANA zone when it names
    /// one, daylight saving included, else its last reported offset.
    pub fn local_minute(&self, now: i64) -> i32 {
        let utc = DateTime::from_timestamp(now, 0).unwrap_or_default();
        let offset_secs = match self.tz.as_deref().and_then(|tz| tz.parse::<chrono_tz::Tz>().ok()) {
            Some(zone) => zone.offset_from_utc_datetime(&utc.naive_utc()).fix().local_minus_utc(),
            None => self.utc_offset * 60,
        };
        let local = utc + chrono::Duration::seconds(i64::from(offset_secs));
        (local.hour() * 60 + local.minute()) as i32
    }

    /// Whether `now` falls inside the device's quiet hours; the start minute is quiet, the end is not.
    pub fn in_quiet(&self, now: i64) -> bool {
        let Some(QuietHours { start, end }) = self.quiet else {
            return false;
        };
        let minute = self.local_minute(now);
        if start < end {
            (start..end).contains(&minute)
        } else {
            minute >= start || minute < end
        }
    }
}

// ---------------------------------------------------------------------------
// Presence: what a device's cockpit tab is showing
// ---------------------------------------------------------------------------

/// How fresh a focus report must be to hold a push back: the cockpit reports every half minute
/// while focused, so anything older means the tab is effectively gone and the phone should speak.
pub const FRESH_SECS: i64 = 75;

/// What a device last said about itself: the colony its tab is on, whether it has focus, and when.
#[derive(Clone, Debug, PartialEq)]
pub struct Presence {
    pub colony: Option<String>,
    pub focused: bool,
    pub at: i64,
}

/// What each device last reported, by subscription id. In memory only: a restart forgets it and
/// the next heartbeat refills it, and a forgotten report errs toward the phone buzzing.
static PRESENCE: LazyLock<Mutex<HashMap<String, Presence>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn record_presence(id: &str, presence: Presence) {
    if let Ok(mut map) = PRESENCE.lock() {
        map.insert(id.to_string(), presence);
    }
}

pub fn presence_of(id: &str) -> Option<Presence> {
    PRESENCE.lock().ok().and_then(|map| map.get(id).cloned())
}

pub fn forget_presence(id: &str) {
    if let Ok(mut map) = PRESENCE.lock() {
        map.remove(id);
    }
}

// ---------------------------------------------------------------------------
// The gates
// ---------------------------------------------------------------------------

/// Whether one device takes one event: its switch, its scope, its quiet hours, then its focus.
/// `session` is the colony the event is about; a colony-less event (provider, digest) is nobody's
/// repo and nobody's open tab, so neither the scope nor presence can hold it back.
pub fn allows(prefs: &Prefs, event: &str, session: Option<&Session>, now: i64, presence: Option<&Presence>) -> bool {
    if !prefs.event_on(event) {
        return false;
    }
    if session.is_some_and(|session| !prefs.in_scope(session)) {
        return false;
    }
    if prefs.in_quiet(now) && !(event == QUESTION && prefs.questions_break_quiet) {
        return false;
    }
    // A device already looking at this colony, and that said so recently, needs no push about it.
    let watching = presence.is_some_and(|p| {
        p.focused && now - p.at <= FRESH_SECS && session.is_some_and(|s| p.colony.as_deref() == Some(s.id.as_str()))
    });
    !watching
}

/// Whether the push for this event goes out silent: everything but a question on a device that
/// lets questions sound.
pub fn silent(prefs: &Prefs, event: &str) -> bool {
    event != QUESTION || !prefs.question_sound
}

/// Issue #742's hook: whether a question's push to this device may carry answer buttons (the option
/// labels and a one-shot token). Asked per device after [`allows`]; off means the plain question push.
#[cfg_attr(not(test), expect(dead_code, reason = "a hook for #742; drop this line once it calls it"))]
pub fn answer_actions(prefs: &Prefs, event: &str) -> bool {
    event == QUESTION && prefs.answer_actions
}

/// Issue #744's hook: whether this device takes the silent "resolved" push for a colony — only
/// when it could have been told about that colony at all (in scope, and at least one colony event
/// on). Quiet hours and presence do not apply: the push only clears what is already there.
#[cfg_attr(not(test), expect(dead_code, reason = "a hook for #744; drop this line once it calls it"))]
pub fn wants_resolved(prefs: &Prefs, session: &Session) -> bool {
    prefs.in_scope(session)
        && EVENTS
            .iter()
            .any(|(name, _)| *name != DIGEST && *name != "provider_degraded" && prefs.event_on(name))
}

/// Issue #744's hook: the app-badge count to put in a push to this device, or `None` to leave the
/// badge key out when the device turned the badge off.
#[cfg_attr(not(test), expect(dead_code, reason = "a hook for #744; drop this line once it calls it"))]
pub fn badge(prefs: &Prefs, count: usize) -> Option<usize> {
    prefs.badge.then_some(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn colony() -> Session {
        serde_json::from_value(json!({
            "id": "abc123", "repo": "acme/webshop", "org": "acme", "issue": 42, "status": "waiting_for_answer",
            "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z",
        }))
        .unwrap()
    }

    /// The default prefs with one opinion of their own.
    fn with<R>(f: impl FnOnce(&mut Prefs) -> R) -> Prefs {
        let mut prefs = Prefs::default();
        f(&mut prefs);
        prefs
    }

    /// 2026-09-25 00:26:40 UTC: minute 26 of the day at offset zero.
    const NOW: i64 = 1_789_000_000;

    /// A unix time from a UTC date and time, for the time-zone cases.
    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        chrono::Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap().timestamp()
    }

    fn seen(colony: Option<&str>, focused: bool, age: i64) -> Presence {
        Presence {
            colony: colony.map(String::from),
            focused,
            at: NOW - age,
        }
    }

    #[test]
    fn each_event_defaults_as_designed_and_the_device_overrides() {
        let session = colony();
        for (event, want) in [
            ("question", true),
            ("attention", true),
            ("failed", true),
            ("pull_request", true),
            ("needs_rebase", true),
            ("provider_degraded", false),
            ("digest", false),
            ("not_an_event", false),
        ] {
            assert_eq!(allows(&Prefs::default(), event, Some(&session), NOW, None), want, "{event}");
        }
        let muted = with(|p| p.events.insert("question".into(), false));
        assert!(!allows(&muted, "question", Some(&session), NOW, None));
        let digest = with(|p| p.events.insert("digest".into(), true));
        assert!(allows(&digest, "digest", None, NOW, None));
        // Only a question may sound, and only where the device lets it.
        assert!(!silent(&Prefs::default(), "question"));
        assert!(silent(&with(|p| p.question_sound = false), "question"));
        assert!(silent(&Prefs::default(), "failed"));
    }

    #[test]
    fn the_scope_narrows_colony_events_and_cannot_refuse_coloniless_ones() {
        let session = colony();
        let device = |scope: &[&str]| with(|p| p.scope = scope.iter().map(|e| (*e).to_string()).collect());
        for (scope, want) in [
            (&[][..], true),
            (&["acme"][..], true),
            (&["ACME"][..], true),
            (&["acme/webshop"][..], true),
            (&["Acme/WebShop"][..], true),
            (&["acme/other"][..], false),
            (&["other"][..], false),
            (&["webshop"][..], false),
            (&["other", "acme/webshop"][..], true),
        ] {
            assert_eq!(allows(&device(scope), "failed", Some(&session), NOW, None), want, "{scope:?}");
        }
        let provider = with(|p| {
            p.scope = vec!["other".into()];
            p.events.insert("provider_degraded".into(), true);
        });
        assert!(allows(&provider, "provider_degraded", None, NOW, None));
    }

    #[test]
    fn quiet_hours_hold_everything_including_across_midnight() {
        let session = colony();
        let device = |start: i32, end: i32| with(|p| p.quiet = Some(QuietHours { start, end }));
        // NOW is minute 26 at UTC: the start minute is quiet, the end minute is not.
        assert!(!allows(&device(20, 40), "failed", Some(&session), NOW, None));
        assert!(!allows(&device(26, 40), "failed", Some(&session), NOW, None));
        assert!(allows(&device(0, 26), "failed", Some(&session), NOW, None));
        assert!(allows(&device(40, 60), "failed", Some(&session), NOW, None));
        // 23:00–01:00 wraps midnight and holds 00:26; 00:40–00:20 wraps too but 00:26 is its day.
        assert!(!allows(&device(1380, 60), "failed", Some(&session), NOW, None));
        assert!(allows(&device(40, 20), "failed", Some(&session), NOW, None));
        // 22:00–07:00: 23:59 and 06:59 are quiet, 07:00 and 21:59 are not.
        let night = device(1320, 420);
        for (h, m, quiet) in [(23, 59, true), (6, 59, true), (7, 0, false), (21, 59, false), (22, 0, true)] {
            assert_eq!(night.in_quiet(at(2026, 9, 25, h, m)), quiet, "{h}:{m}");
        }
    }

    #[test]
    fn a_question_breaks_through_quiet_hours_only_when_allowed() {
        let session = colony();
        let held = Some(QuietHours { start: 20, end: 40 });
        let strict = with(|p| p.quiet = held);
        let lenient = with(|p| {
            p.quiet = held;
            p.questions_break_quiet = true;
        });
        assert!(!allows(&strict, "question", Some(&session), NOW, None));
        assert!(allows(&lenient, "question", Some(&session), NOW, None));
        assert!(
            !allows(&lenient, "failed", Some(&session), NOW, None),
            "only questions break through"
        );
    }

    #[test]
    fn quiet_hours_follow_the_devices_own_time_zone() {
        // 22:00–07:00 on a device in Berlin: 21:30 UTC in September is 23:30 there (CEST, +2).
        let berlin = with(|p| {
            p.quiet = Some(QuietHours { start: 1320, end: 420 });
            p.tz = Some("Europe/Berlin".into());
        });
        assert!(berlin.in_quiet(at(2026, 9, 25, 21, 30)));
        assert!(!berlin.in_quiet(at(2026, 9, 25, 5, 30)), "07:30 in Berlin");
        // In January Berlin is +1: 05:30 UTC is 06:30 there, still quiet.
        assert!(berlin.in_quiet(at(2026, 1, 15, 5, 30)));
        // Daylight saving comes from the zone, not the stored offset, which may be stale.
        let stale = with(|p| {
            p.tz = Some("America/New_York".into());
            p.utc_offset = -240;
        });
        assert_eq!(stale.local_minute(at(2026, 1, 15, 12, 0)), 7 * 60, "EST is -5 in January");
        assert_eq!(stale.local_minute(at(2026, 7, 15, 12, 0)), 8 * 60, "EDT is -4 in July");
        // A half-hour zone, and one a day ahead of UTC that crosses midnight.
        let kolkata = with(|p| p.tz = Some("Asia/Kolkata".into()));
        assert_eq!(kolkata.local_minute(at(2026, 9, 25, 20, 0)), 90, "01:30 the next day");
        let auckland = with(|p| {
            p.quiet = Some(QuietHours { start: 1380, end: 360 });
            p.tz = Some("Pacific/Auckland".into());
        });
        assert!(auckland.in_quiet(at(2026, 9, 25, 12, 0)), "00:00 NZST next day");
        assert!(!auckland.in_quiet(at(2026, 9, 25, 2, 0)), "14:00 in Auckland");
        // An unknown zone name falls back to the reported offset.
        let unknown = with(|p| {
            p.tz = Some("Mars/Olympus".into());
            p.utc_offset = -150;
        });
        assert_eq!(unknown.local_minute(at(2026, 9, 25, 1, 0)), 22 * 60 + 30);
        assert_eq!(Prefs::default().local_minute(NOW), 26, "no zone, no offset: UTC");
    }

    #[test]
    fn a_fresh_focused_report_on_the_same_colony_holds_the_push_back() {
        let session = colony();
        for (on_screen, focused, age, want) in [
            (Some("abc123"), true, 10, false),
            (Some("abc123"), true, FRESH_SECS, false),
            (Some("abc123"), true, FRESH_SECS + 1, true),
            (Some("abc123"), false, 10, true),
            (Some("def456"), true, 10, true),
            (None, true, 10, true),
        ] {
            let report = seen(on_screen, focused, age);
            assert_eq!(
                allows(&Prefs::default(), "failed", Some(&session), NOW, Some(&report)),
                want,
                "{on_screen:?} {focused} {age}"
            );
        }
        let digest = with(|p| p.events.insert("digest".into(), true));
        assert!(allows(&digest, "digest", None, NOW, Some(&seen(Some("abc123"), true, 10))));
    }

    #[test]
    fn the_hooks_for_answer_buttons_resolved_pushes_and_the_badge() {
        let session = colony();
        assert!(answer_actions(&Prefs::default(), "question"));
        assert!(!answer_actions(&Prefs::default(), "failed"));
        assert!(!answer_actions(&with(|p| p.answer_actions = false), "question"));

        assert!(wants_resolved(&Prefs::default(), &session));
        assert!(!wants_resolved(&with(|p| p.scope = vec!["other".into()]), &session));
        let ambient_only = with(|p| {
            for (name, _) in EVENTS {
                p.events
                    .insert((*name).into(), matches!(*name, "digest" | "provider_degraded"));
            }
        });
        assert!(
            !wants_resolved(&ambient_only, &session),
            "nothing about colonies was ever sent"
        );
        let quiet = with(|p| p.quiet = Some(QuietHours { start: 0, end: 1439 }));
        assert!(wants_resolved(&quiet, &session), "clearing is not held by quiet hours");

        assert_eq!(badge(&Prefs::default(), 3), Some(3));
        assert_eq!(badge(&with(|p| p.badge = false), 3), None);
    }

    #[test]
    fn prefs_are_validated_before_they_are_stored() {
        let flooded = (0..=MAX_SCOPE).map(|i| format!("org{i}")).collect::<Vec<_>>();
        let cases = [
            ("the defaults", Prefs::default(), true),
            ("an unknown event", with(|p| p.events.insert("nope".into(), true)), false),
            ("more entries than the cap", with(|p| p.scope = flooded.clone()), false),
            ("an empty entry", with(|p| p.scope = vec![String::new()]), false),
            ("whitespace", with(|p| p.scope = vec!["acme x".into()]), false),
            ("two slashes", with(|p| p.scope = vec!["a/b/c".into()]), false),
            ("a dangling slash", with(|p| p.scope = vec!["acme/".into()]), false),
            (
                "an overlong entry",
                with(|p| p.scope = vec!["x".repeat(MAX_SCOPE_ENTRY + 1)]),
                false,
            ),
            ("an overlong zone", with(|p| p.tz = Some("x".repeat(65))), false),
            ("a control character", with(|p| p.tz = Some("a\nb".into())), false),
            ("an offset past the extreme", with(|p| p.utc_offset = MAX_OFFSET + 1), false),
            (
                "start equals end",
                with(|p| p.quiet = Some(QuietHours { start: 9, end: 9 })),
                false,
            ),
            (
                "a negative minute",
                with(|p| p.quiet = Some(QuietHours { start: -1, end: 60 })),
                false,
            ),
            (
                "minute 1440",
                with(|p| p.quiet = Some(QuietHours { start: 0, end: 1440 })),
                false,
            ),
            (
                "a wrapping window",
                with(|p| p.quiet = Some(QuietHours { start: 1320, end: 420 })),
                true,
            ),
            ("a known event", with(|p| p.events.insert("question".into(), false)), true),
            ("the extreme offset", with(|p| p.utc_offset = -MAX_OFFSET), true),
            ("an unknown zone", with(|p| p.tz = Some("Mars/Olympus".into())), true),
        ];
        for (why, prefs, ok) in &cases {
            assert_eq!(prefs.validate().is_ok(), *ok, "{why}");
        }
    }

    #[test]
    fn missing_fields_load_as_the_defaults() {
        let prefs: Prefs = serde_json::from_value(json!({ "scope": ["acme"] })).unwrap();
        assert_eq!(prefs, with(|p| p.scope = vec!["acme".into()]));
        assert!(prefs.question_sound && prefs.answer_actions && prefs.badge);
    }
}
