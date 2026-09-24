//! When something recurring fires next: the cadence shared by red-team schedules (`redteam.rs`) and
//! loops (`loops.rs`). Every time is UTC; the cockpit converts the operator's local choice before
//! saving. Pure, so the month-end, week-wrap and interval rules are tested with fixed dates.

use chrono::{DateTime, Datelike, Duration as ChronoDuration, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};

/// The shortest interval a loop may run at: anything tighter mostly burns tokens on colonies that
/// find nothing new.
pub const MIN_INTERVAL_MINUTES: u32 = 15;
/// The longest interval: past a week, a weekly or monthly cadence says it better.
pub const MAX_INTERVAL_MINUTES: u32 = 7 * 24 * 60;
/// A self-paced loop whose colony never says when to run next runs again after this long.
pub const SELF_PACED_FALLBACK_MINUTES: u32 = 24 * 60;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "every", rename_all = "snake_case")]
pub enum Cadence {
    /// Every `minutes` minutes after the previous firing.
    Interval { minutes: u32 },
    /// Every day at `hour:minute` UTC.
    Daily { hour: u32, minute: u32 },
    /// `weekday` 0 = Monday … 6 = Sunday.
    Weekly { weekday: u32, hour: u32, minute: u32 },
    /// `day` 1–31; a day past the month's end fires on its last day (31 → 30 April, 28/29 February).
    Monthly { day: u32, hour: u32, minute: u32 },
    /// The colony chooses: it names its next run (loop_next), else [`SELF_PACED_FALLBACK_MINUTES`].
    SelfPaced {},
}

impl Cadence {
    pub(crate) fn check(&self) -> Result<(), String> {
        let (hour, minute) = match self {
            Cadence::Interval { minutes } => {
                if !(MIN_INTERVAL_MINUTES..=MAX_INTERVAL_MINUTES).contains(minutes) {
                    return Err(format!(
                        "an interval must be {MIN_INTERVAL_MINUTES} minutes to 7 days, got {minutes} minutes"
                    ));
                }
                return Ok(());
            }
            Cadence::SelfPaced {} => return Ok(()),
            Cadence::Daily { hour, minute } => (*hour, *minute),
            Cadence::Weekly { weekday, hour, minute } => {
                if *weekday > 6 {
                    return Err(format!("weekday must be 0 (Monday) to 6 (Sunday), got {weekday}"));
                }
                (*hour, *minute)
            }
            Cadence::Monthly { day, hour, minute } => {
                if !(1..=31).contains(day) {
                    return Err(format!("day must be 1 to 31, got {day}"));
                }
                (*hour, *minute)
            }
        };
        if hour > 23 || minute > 59 {
            return Err(format!("time must be 00:00 to 23:59 UTC, got {hour:02}:{minute:02}"));
        }
        Ok(())
    }
}

fn last_day_of_month(year: i32, month: u32) -> u32 {
    let (next_year, next_month) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    NaiveDate::from_ymd_opt(next_year, next_month, 1)
        .and_then(|d| d.pred_opt())
        .map(|d| d.day())
        .unwrap_or(28)
}

fn at(date: NaiveDate, hour: u32, minute: u32) -> Option<DateTime<Utc>> {
    date.and_hms_opt(hour, minute, 0).map(|t| Utc.from_utc_datetime(&t))
}

/// The first time the cadence fires strictly after `after`.
pub fn next_run_after(cadence: &Cadence, after: DateTime<Utc>) -> DateTime<Utc> {
    let today = after.date_naive();
    match *cadence {
        Cadence::Interval { minutes } => after + ChronoDuration::minutes(i64::from(minutes.max(1))),
        Cadence::SelfPaced {} => after + ChronoDuration::minutes(i64::from(SELF_PACED_FALLBACK_MINUTES)),
        Cadence::Daily { hour, minute } => {
            for extra in [0, 1] {
                if let Some(when) = at(today + ChronoDuration::days(extra), hour, minute)
                    && when > after
                {
                    return when;
                }
            }
            after + ChronoDuration::days(1)
        }
        Cadence::Weekly { weekday, hour, minute } => {
            let ahead = (weekday + 7 - today.weekday().num_days_from_monday()) % 7;
            for extra in [0, 7] {
                if let Some(when) = at(today + ChronoDuration::days(i64::from(ahead + extra)), hour, minute)
                    && when > after
                {
                    return when;
                }
            }
            after + ChronoDuration::days(7)
        }
        Cadence::Monthly { day, hour, minute } => {
            let (mut year, mut month) = (today.year(), today.month());
            for _ in 0..3 {
                let d = day.min(last_day_of_month(year, month));
                if let Some(when) = NaiveDate::from_ymd_opt(year, month, d).and_then(|date| at(date, hour, minute))
                    && when > after
                {
                    return when;
                }
                (year, month) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
            }
            after + ChronoDuration::days(28)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }

    #[test]
    fn intervals_add_their_minutes_and_self_paced_falls_back_to_a_day() {
        let now = utc(2026, 9, 24, 10, 7);
        assert_eq!(
            next_run_after(&Cadence::Interval { minutes: 90 }, now),
            utc(2026, 9, 24, 11, 37)
        );
        assert_eq!(next_run_after(&Cadence::SelfPaced {}, now), utc(2026, 9, 25, 10, 7));
    }

    #[test]
    fn daily_fires_later_today_or_tomorrow_never_exactly_now() {
        let nine = Cadence::Daily { hour: 9, minute: 0 };
        assert_eq!(next_run_after(&nine, utc(2026, 9, 24, 8, 59)), utc(2026, 9, 24, 9, 0));
        assert_eq!(next_run_after(&nine, utc(2026, 9, 24, 9, 0)), utc(2026, 9, 25, 9, 0));
        assert_eq!(
            next_run_after(&nine, utc(2026, 12, 31, 23, 0)),
            utc(2027, 1, 1, 9, 0),
            "year wrap"
        );
    }

    #[test]
    fn checks_bound_intervals_and_times() {
        assert!(Cadence::Interval { minutes: 15 }.check().is_ok());
        assert!(Cadence::Interval { minutes: 14 }.check().is_err(), "under 15 minutes");
        assert!(
            Cadence::Interval {
                minutes: 7 * 24 * 60 + 1
            }
            .check()
            .is_err(),
            "over a week"
        );
        assert!(Cadence::Daily { hour: 24, minute: 0 }.check().is_err());
        assert!(Cadence::SelfPaced {}.check().is_ok());
    }
}
