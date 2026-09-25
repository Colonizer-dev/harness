//! The shared anti-spam ledger (issue #311): one record per mothership, at `<data_dir>/ledger.json`,
//! of every outbound proactive action, consulted by notify before it announces and by the autonomous
//! judge before it answers — the watchdog joins them later. Proactive messages all leave through
//! one front door, so the rate the operator lives with is one rate, and nothing outside — a colony
//! asking, a provider flapping, ten more colonies arriving at once — can flood the person watching.
//!
//! The split is the same one `notify::decide` uses: [`check`] is a pure gate that reads the state and
//! rules, [`record`] is the write half that counts what actually happened. Every candidate is counted
//! — delivered, held for the digest, or dropped — so nothing goes out unaccounted: what the soft
//! layers held lands in the hour's digest line, and every hold and drop stands in the tallies the
//! status poll reports. Nothing identifiable leaves the ledger: keys name kinds and counters, never
//! question text (the privacy rule notify already holds) and never raw repository content.

use crate::util::write_atomic;
use chrono::{DateTime, Duration, Timelike, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Mutex,
};
use tokio::sync::Mutex as AsyncMutex;

/// The two claimants the ledger serves today. The watchdog's nudges are the third, when they move
/// behind the same gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// An announcement on its way to the desktop or the webhook.
    Notify,
    /// An autonomous answer about to be sent to a colony's open question.
    Judge,
}

/// The three-way outcome of one candidate: out the door, held for the hour's digest line, or dropped
/// outright. The `&'static str` is the short reason — `duplicate`, `cooldown`, and the rest — carried
/// for logs and tests, never persisted per entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Deliver,
    /// Soft layer: withheld now, summarised later. `Digest("quiet_hours")` and friends.
    Digest(&'static str),
    /// Hard layer: not this hour, not worth holding either. `Drop("duplicate")` and friends.
    Drop(&'static str),
}

/// One outbound action asking to leave the mothership. The string keys are bounded on purpose:
/// `topic` buckets the counter (`"provider:<id>"`, `"question:<session>"`), `class` is the event
/// name the digest counts by, and `fact` is the underlying fact behind the message so two claimants
/// that saw the same thing collapse into one delivery — a key, never text.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub kind: Kind,
    pub topic: String,
    pub class: String,
    pub fact: Option<String>,
    /// The colony the action is about, if any — a provider event has none. Counted, never sent.
    pub colony: Option<String>,
    /// The candidate blocks a colony (a question nobody answered): it bypasses the soft layers,
    /// never the hard ones.
    pub priority: bool,
}

/// The rules one kind lives under. Pure data: [`Limits::for_kind`] carries the defaults, and nothing
/// reads configuration yet — the point of this module is that the defaults are sane enough to ship.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The UTC hour window `[start, end)` in which nothing delivers; `22..6` wraps midnight. `None`
    /// is no quiet hours.
    pub quiet: Option<(u32, u32)>,
    /// Delivered messages per kind per rolling hour; past it, hold for the digest.
    pub per_hour: usize,
    /// Delivered messages per kind per rolling day; past it, stop outright.
    pub per_day: usize,
    /// How long after a delivery the same topic holds again.
    pub topic_cooldown: Duration,
    /// How long the same fact stays claimed by its one delivery.
    pub dedup_window: Duration,
    /// Delivered messages per kind per topic per rolling day — the hard per-topic cap.
    pub topic_daily_cap: usize,
}

impl Limits {
    pub fn for_kind(kind: Kind) -> Self {
        match kind {
            // Notify defaults: a colony's life can be eventful, an operator's afternoon is not.
            Kind::Notify => Self {
                quiet: None,
                per_hour: 12,
                per_day: 60,
                topic_cooldown: Duration::minutes(10),
                dedup_window: Duration::hours(1),
                topic_daily_cap: 10,
            },
            // The judge answers real work: looser quotas, no cooldown — but at most 20 judged
            // answers per colony per day, so a loop of questions cannot burn a provider's key all
            // day, and one fact (one question) is claimed for 24 h.
            Kind::Judge => Self {
                quiet: None,
                per_hour: 30,
                per_day: 100,
                topic_cooldown: Duration::zero(),
                dedup_window: Duration::hours(24),
                topic_daily_cap: 20,
            },
        }
    }
}

/// How long the raw entries are kept: quotas read rolling windows of at most a day, so two days of
/// history is plenty. Entries past this, or past [`MAX_ENTRIES`] in count, are pruned on every record.
const KEEP: Duration = Duration::hours(48);
const MAX_ENTRIES: usize = 5000;

const DELIVERED: &str = "delivered";
const DIGESTED: &str = "digested";
const DROPPED: &str = "dropped";

/// One recorded candidate, exactly as it was counted. The reason a candidate was held or dropped is
/// not persisted — the counters say what happened, the entries say when.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Entry {
    pub at: DateTime<Utc>,
    pub kind: Kind,
    pub topic: String,
    pub class: String,
    pub fact: Option<String>,
    pub colony: Option<String>,
    /// One of `delivered`, `digested`, `dropped`.
    pub outcome: String,
}

impl Default for Entry {
    fn default() -> Self {
        Self {
            at: DateTime::<Utc>::UNIX_EPOCH,
            kind: Kind::Notify,
            topic: String::new(),
            class: String::new(),
            fact: None,
            colony: None,
            outcome: DELIVERED.to_string(),
        }
    }
}

/// A class's running tally: the three outcomes sum to everything the class ever sent through.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counters {
    pub delivered: u64,
    pub digested: u64,
    pub dropped: u64,
}

/// Everything the ledger persists. Every field defaults, so a file an earlier or later build wrote
/// still loads — the same forward tolerance `Session` and the red-team runs carry.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LedgerState {
    pub entries: Vec<Entry>,
    /// Per class (`"question"`, `"judge"`, …): how many were delivered, held and dropped. Ever.
    pub counters: BTreeMap<String, Counters>,
    /// Per class: candidates held since the last digest line went out.
    pub pending_digest: BTreeMap<String, u64>,
    pub last_digest: Option<DateTime<Utc>>,
}

/// Whether `now` sits in the quiet-hours window. An empty window (`22..22`) is no window at all.
fn in_quiet_hours(quiet: Option<(u32, u32)>, now: DateTime<Utc>) -> bool {
    let Some((start, end)) = quiet else { return false };
    if start == end {
        return false;
    }
    let hour = now.hour();
    if start < end {
        hour >= start && hour < end
    } else {
        // Wrapping midnight: 22..6 holds 23:00 and 05:00 alike.
        hour >= start || hour < end
    }
}

/// Counts this candidate's kind has already spent in the window ending at `now` — the only entries
/// that count are delivered ones: a message that was held or dropped spent nobody's patience.
fn spent(state: &LedgerState, kind: Kind, topic: Option<&str>, window: Duration, now: DateTime<Utc>) -> usize {
    let since = now - window;
    state
        .entries
        .iter()
        .filter(|e| e.kind == kind && e.outcome == DELIVERED && e.at > since && topic.is_none_or(|t| e.topic == t))
        .count()
}

/// The pure gate: should this candidate deliver, wait for the digest, or be dropped? Reads `state`
/// and never touches it. The layers run hard-first — dedup, the per-topic cap, the daily quota, the
/// ones a `priority` candidate cannot argue with — then the soft ones a candidate that blocks a
/// colony skips, then the door.
pub fn check(state: &LedgerState, limits: &Limits, c: &Candidate, now: DateTime<Utc>) -> Verdict {
    // (a) Hard layers. The same fact, already delivered by any claimant inside the dedup window, is
    // one fact told once — the key namespaces it, so kinds share the rule.
    if let Some(fact) = &c.fact {
        let claimed = state
            .entries
            .iter()
            .rev()
            .find(|e| e.outcome == DELIVERED && e.fact.as_deref() == Some(fact.as_str()));
        if let Some(e) = claimed
            && now - e.at < limits.dedup_window
        {
            return Verdict::Drop("duplicate");
        }
    }
    if spent(state, c.kind, Some(&c.topic), Duration::hours(24), now) >= limits.topic_daily_cap {
        return Verdict::Drop("topic_cap");
    }
    if spent(state, c.kind, None, Duration::hours(24), now) >= limits.per_day {
        return Verdict::Drop("daily_quota");
    }
    // (b) Soft layers, skipped by a candidate whose colony is blocked until a person answers.
    if !c.priority {
        if in_quiet_hours(limits.quiet, now) {
            return Verdict::Digest("quiet_hours");
        }
        if limits.topic_cooldown > Duration::zero() {
            let last = state
                .entries
                .iter()
                .rev()
                .find(|e| e.kind == c.kind && e.outcome == DELIVERED && e.topic == c.topic);
            if let Some(e) = last
                && now - e.at < limits.topic_cooldown
            {
                return Verdict::Digest("cooldown");
            }
        }
        if spent(state, c.kind, None, Duration::hours(1), now) >= limits.per_hour {
            return Verdict::Digest("hourly_quota");
        }
    }
    // (c) The door.
    Verdict::Deliver
}

/// The write half: append the entry, bump the counters, and let a held candidate swell the pending
/// digest. Callers record a `Deliver` only after the delivery actually went out, and a `Digest`/`Drop`
/// immediately, so every held or dropped candidate is counted, never silent.
pub fn record(state: &mut LedgerState, c: &Candidate, verdict: &Verdict, now: DateTime<Utc>) {
    let outcome = match verdict {
        Verdict::Deliver => DELIVERED,
        Verdict::Digest(_) => DIGESTED,
        Verdict::Drop(_) => DROPPED,
    };
    if let Verdict::Digest(_) = verdict {
        *state.pending_digest.entry(c.class.clone()).or_default() += 1;
    }
    let tally = state.counters.entry(c.class.clone()).or_default();
    match verdict {
        Verdict::Deliver => tally.delivered += 1,
        Verdict::Digest(_) => tally.digested += 1,
        Verdict::Drop(_) => tally.dropped += 1,
    }
    state.entries.push(Entry {
        at: now,
        kind: c.kind,
        topic: c.topic.clone(),
        class: c.class.clone(),
        fact: c.fact.clone(),
        colony: c.colony.clone(),
        outcome: outcome.to_string(),
    });
    // Two days of entries is all any quota reads; a flood prunes to the cap, not to disk.
    let since = now - KEEP;
    state.entries.retain(|e| e.at > since);
    if state.entries.len() > MAX_ENTRIES {
        let excess = state.entries.len() - MAX_ENTRIES;
        state.entries.drain(..excess);
    }
}

/// The read-only half of the digest: the one line to deliver, if one is due — something held, and at
/// least an hour since the last line — with the counts the line was built from, so the commit can
/// subtract exactly what went out. Counts by class only; no identities, no question text.
pub fn digest_due(state: &LedgerState, now: DateTime<Utc>) -> Option<(String, BTreeMap<String, u64>)> {
    if state.pending_digest.is_empty() {
        return None;
    }
    if let Some(last) = state.last_digest
        && now - last < Duration::hours(1)
    {
        return None;
    }
    Some((digest_line(&state.pending_digest), state.pending_digest.clone()))
}

fn digest_line(pending: &BTreeMap<String, u64>) -> String {
    let total: u64 = pending.values().sum();
    let parts: Vec<String> = pending.iter().map(|(class, n)| format!("{class} ×{n}")).collect();
    format!("Colonizer: {total} held announcements — {}", parts.join(", "))
}

/// The commit half: the digest line went out, so subtract the counts it carried — candidates held
/// while the line was in flight stay pending, and a class counts down to nothing rather than being
/// cleared wholesale. Split from [`digest_due`] so a digest that failed to deliver is still due on
/// the next tick.
pub fn commit_digest(state: &mut LedgerState, delivered: &BTreeMap<String, u64>, now: DateTime<Utc>) {
    for (class, n) in delivered {
        let left = state.pending_digest.entry(class.clone()).or_default();
        *left = left.saturating_sub(*n);
        if *left == 0 {
            state.pending_digest.remove(class);
        }
    }
    state.last_digest = Some(now);
}

/// Whether any entry already carries this fact — delivered, held or dropped. A held or dropped
/// verdict is counted once for its fact, not once per tick, and the entries prune at [`KEEP`], so
/// the lookup stays bounded and survives a restart.
pub fn has_fact(state: &LedgerState, fact: &str) -> bool {
    state.entries.iter().any(|e| e.fact.as_deref() == Some(fact))
}

/// The loaded ledger, shared on [`crate::App`] like the red-team store: state behind a std mutex
/// (short, no awaits inside), saves serialised behind their own lock so two writers cannot
/// interleave temp files.
pub struct LedgerStore {
    path: PathBuf,
    state: Mutex<LedgerState>,
    write: AsyncMutex<()>,
    /// How many times a corrupt file was quarantined at load: 0 or 1, set once, never cleared.
    quarantined: u64,
}

impl LedgerStore {
    /// `<data_dir>/ledger.json` → the state it holds. A missing file is a first run; a corrupt one is
    /// moved aside whole (`ledger.json.corrupt-<unix-ts>`), its bytes kept, and the store starts
    /// empty — the sessions.json rule, never a wipe.
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join("ledger.json");
        let (state, quarantined) = match std::fs::read(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (LedgerState::default(), 0),
            Err(e) => {
                eprintln!("ledger: could not read {}: {e}; starting empty", path.display());
                (LedgerState::default(), 0)
            }
            Ok(bytes) => match serde_json::from_slice::<LedgerState>(&bytes) {
                Ok(state) => (state, 0),
                Err(e) => {
                    let note = match crate::move_corrupt_aside(&path) {
                        Ok(saved) => format!("the bytes were saved as {}", saved.display()),
                        Err(move_error) => format!("starting empty, and {move_error:#}"),
                    };
                    eprintln!(
                        "ledger: {} is not a ledger the harness understands ({e}); {note}",
                        path.display()
                    );
                    (LedgerState::default(), 1)
                }
            },
        };
        Self {
            path,
            state: Mutex::new(state),
            write: AsyncMutex::new(()),
            quarantined,
        }
    }

    /// Serialises the state and puts it on disk, atomically. A failed save is a line in the log, not
    /// a fault: the ledger is bookkeeping, and the next record tries again.
    async fn save(&self) {
        let data = {
            let state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            serde_json::to_vec_pretty(&*state)
        };
        let data = match data {
            Ok(data) => data,
            Err(e) => {
                eprintln!("ledger: could not serialise {}: {e}", self.path.display());
                return;
            }
        };
        let _guard = self.write.lock().await;
        if let Err(e) = write_atomic(&self.path, &data).await {
            eprintln!("ledger: could not save {}: {e:#}", self.path.display());
        }
    }

    /// The gate against this store's state and the candidate's own kind's limits.
    pub fn check(&self, c: &Candidate, now: DateTime<Utc>) -> Verdict {
        let state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        check(&state, &Limits::for_kind(c.kind), c, now)
    }

    /// Counts a candidate's verdict and saves. Called after a successful delivery for
    /// [`Verdict::Deliver`], and immediately for [`Verdict::Digest`] / [`Verdict::Drop`].
    pub async fn record(&self, c: &Candidate, verdict: &Verdict, now: DateTime<Utc>) {
        {
            let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            record(&mut state, c, verdict, now);
        }
        self.save().await;
    }

    /// The digest line to deliver now, if any, with the counts it carries. Read-only: nothing is
    /// subtracted until [`Self::commit_digest`] says the line went out.
    pub fn digest_due(&self, now: DateTime<Utc>) -> Option<(String, BTreeMap<String, u64>)> {
        let state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        digest_due(&state, now)
    }

    /// Subtracts the counts the delivered line carried and stamps the cadence, then saves.
    pub async fn commit_digest(&self, delivered: &BTreeMap<String, u64>, now: DateTime<Utc>) {
        {
            let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            commit_digest(&mut state, delivered, now);
        }
        self.save().await;
    }

    /// Whether the ledger already carries an entry for this fact — a held or dropped verdict is
    /// counted once, not once per tick.
    pub fn has_fact(&self, fact: &str) -> bool {
        let state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        has_fact(&state, fact)
    }

    /// The status-poll view: counters and limits by class. No colony ids, no facts, no question
    /// text — this is the authenticated `/api/status`, and it still gets nothing it could name
    /// anyone with.
    pub fn snapshot(&self) -> Value {
        let state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let counters: BTreeMap<&String, &Counters> = state.counters.iter().collect();
        json!({
            "counters": counters,
            "pending_digest": state.pending_digest,
            "last_digest": state.last_digest.map(|at| at.to_rfc3339()),
            "quarantined": self.quarantined,
            "limits": {
                "notify": limits_json(Kind::Notify),
                "judge": limits_json(Kind::Judge),
            },
        })
    }
}

fn limits_json(kind: Kind) -> Value {
    let l = Limits::for_kind(kind);
    json!({
        "quiet_hours": l.quiet.map(|(start, end)| json!({"start": start, "end": end})),
        "per_hour": l.per_hour,
        "per_day": l.per_day,
        "topic_cooldown_minutes": l.topic_cooldown.num_minutes(),
        "dedup_window_hours": l.dedup_window.num_hours(),
        "topic_daily_cap": l.topic_daily_cap,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ledger state, fresh: no history, nothing pending.
    fn state() -> LedgerState {
        LedgerState::default()
    }

    fn candidate(kind: Kind, topic: &str, class: &str, fact: Option<&str>) -> Candidate {
        Candidate {
            kind,
            topic: topic.into(),
            class: class.into(),
            fact: fact.map(String::from),
            colony: None,
            priority: false,
        }
    }

    /// A question candidate: the one that blocks a colony, so it bypasses the soft layers.
    fn question(session: &str) -> Candidate {
        let mut c = candidate(
            Kind::Notify,
            &format!("question:{session}"),
            "question",
            Some(&format!("question:{session}")),
        );
        c.priority = true;
        c
    }

    fn degraded(id: &str) -> Candidate {
        candidate(
            Kind::Notify,
            &format!("provider:{id}"),
            "provider_degraded",
            Some(&format!("provider:{id}")),
        )
    }

    fn notify_limits() -> Limits {
        Limits::for_kind(Kind::Notify)
    }

    fn at(minute: u64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_789_000_000 + (minute * 60) as i64, 0).unwrap()
    }

    /// Records `c` as delivered at `at`, the way a caller that really sent it would.
    fn deliver(state: &mut LedgerState, c: &Candidate, at: DateTime<Utc>) {
        record(state, c, &Verdict::Deliver, at);
    }

    fn outcome_of<'a>(state: &'a LedgerState, kind: Kind, topic: &str) -> &'a str {
        state
            .entries
            .iter()
            .rev()
            .find(|e| e.kind == kind && e.topic == topic)
            .map(|e| e.outcome.as_str())
            .expect("an entry for the topic")
    }

    fn total(state: &LedgerState, outcome: &str) -> usize {
        state.entries.iter().filter(|e| e.outcome == outcome).count()
    }

    #[test]
    fn quiet_hours_hold_into_the_digest_priority_bypasses_and_a_wrapped_window_holds_both_sides() {
        let limits = Limits {
            quiet: Some((22, 6)),
            ..notify_limits()
        };
        // An ordinary announcement, one the soft layers may hold. 23:00 and 05:00 sit inside a
        // window that wraps midnight; 12:00 does not.
        let c = degraded("p1");
        let on_the_hour = |hour: u32| Utc::now().with_hour(hour).unwrap().with_minute(0).unwrap();
        let evening = on_the_hour(23);
        assert_eq!(check(&state(), &limits, &c, evening), Verdict::Digest("quiet_hours"));
        assert_eq!(check(&state(), &limits, &c, on_the_hour(5)), Verdict::Digest("quiet_hours"));
        assert_eq!(check(&state(), &limits, &c, on_the_hour(12)), Verdict::Deliver);
        // An empty window is no window at all, and `None` is none either.
        let empty = Limits {
            quiet: Some((22, 22)),
            ..limits.clone()
        };
        assert_eq!(check(&state(), &empty, &c, evening), Verdict::Deliver);
        assert_eq!(check(&state(), &notify_limits(), &c, evening), Verdict::Deliver);
        // A question blocks its colony: the soft layer gives way, the colony gets its answer.
        assert_eq!(check(&state(), &limits, &question("abc123"), evening), Verdict::Deliver);
    }

    #[test]
    fn the_hourly_quota_holds_into_the_digest_and_lifts_once_the_hour_passes() {
        let limits = notify_limits();
        let mut st = state();
        for i in 0..12 {
            deliver(&mut st, &degraded(&format!("p{i}")), at(5));
        }
        let thirteenth = degraded("p12");
        assert_eq!(check(&st, &limits, &thirteenth, at(6)), Verdict::Digest("hourly_quota"));
        // An hour on, the spent deliveries have fallen out of the window.
        assert_eq!(check(&st, &limits, &thirteenth, at(66)), Verdict::Deliver);
    }

    #[test]
    fn the_daily_quota_drops_and_hard_layers_run_before_soft_ones() {
        let limits = notify_limits();
        let mut st = state();
        for i in 0..60 {
            deliver(&mut st, &question(&format!("c{i}")), at(10));
        }
        // Sixty-one events in a day: dropped outright, even a priority one, even though the hourly
        // quota (a soft layer) would merely have held it — hard layers answer first.
        let sixty_first = question("c60");
        assert_eq!(check(&st, &limits, &sixty_first, at(11)), Verdict::Drop("daily_quota"));
        // A day later the quota is spent no longer.
        assert_eq!(check(&st, &limits, &sixty_first, at(10 + 24 * 60 + 1)), Verdict::Deliver);
    }

    #[test]
    fn the_topic_cooldown_holds_then_delivers_once_it_passes() {
        let limits = notify_limits();
        let mut st = state();
        deliver(&mut st, &degraded("p1"), at(0));
        // Inside ten minutes the same topic waits for the digest instead of announcing again — a
        // re-crossing carrying a new fact, since the same fact is the hard layer's business first.
        let recrossing = candidate(Kind::Notify, "provider:p1", "provider_degraded", Some("p1-recrossing"));
        assert_eq!(check(&st, &limits, &recrossing, at(3)), Verdict::Digest("cooldown"));
        // A different topic is not cooled down by p1's delivery.
        assert_eq!(check(&st, &limits, &degraded("p2"), at(3)), Verdict::Deliver);
        // Past the cooldown the topic delivers again.
        assert_eq!(check(&st, &limits, &recrossing, at(11)), Verdict::Deliver);
    }

    #[test]
    fn the_same_fact_drops_as_a_duplicate_but_a_different_fact_delivers() {
        let limits = notify_limits();
        let mut st = state();
        deliver(&mut st, &degraded("p1"), at(0));
        // Same fact, any claimant, inside the window: one fact is told once.
        assert_eq!(check(&st, &limits, &degraded("p1"), at(5)), Verdict::Drop("duplicate"));
        let judge_same_fact = candidate(Kind::Judge, "judge:s1", "judge", Some("provider:p1"));
        assert_eq!(check(&st, &limits, &judge_same_fact, at(5)), Verdict::Drop("duplicate"));
        // A different fact is its own message.
        assert_eq!(check(&st, &limits, &degraded("p2"), at(5)), Verdict::Deliver);
        // Past the window the fact is claimable again.
        assert_eq!(check(&st, &limits, &degraded("p1"), at(61)), Verdict::Deliver);
    }

    #[test]
    fn the_topic_daily_cap_drops_even_a_priority_candidate() {
        let limits = notify_limits();
        let mut st = state();
        // Ten delivered for one topic, spaced past the cooldown, inside one day.
        for minute in 0..10 {
            deliver(
                &mut st,
                &candidate(
                    Kind::Notify,
                    "provider:p1",
                    "provider_degraded",
                    Some(&format!("p1-crossing-{minute}")),
                ),
                at(minute * 11),
            );
        }
        let eleventh = candidate(Kind::Notify, "provider:p1", "provider_degraded", Some("p1-crossing-new"));
        assert_eq!(check(&st, &limits, &eleventh, at(111)), Verdict::Drop("topic_cap"));
        let mut urgent = eleventh.clone();
        urgent.priority = true;
        assert_eq!(
            check(&st, &limits, &urgent, at(111)),
            Verdict::Drop("topic_cap"),
            "priority never passes a hard layer"
        );
        // A fresh topic is its own cap.
        assert_eq!(check(&st, &limits, &degraded("p2"), at(111)), Verdict::Deliver);
    }

    #[test]
    fn only_delivered_entries_count_toward_the_quotas() {
        let limits = notify_limits();
        let mut st = state();
        // Twelve drops and twelve holds inside the hour: nothing was spent, so nothing is limited.
        for i in 0..12 {
            record(&mut st, &question(&format!("c{i}")), &Verdict::Drop("daily_quota"), at(5));
            record(&mut st, &question(&format!("d{i}")), &Verdict::Digest("quiet_hours"), at(5));
        }
        assert_eq!(check(&st, &limits, &question("fresh"), at(6)), Verdict::Deliver);
    }

    #[test]
    fn check_reads_the_ledger_without_changing_it() {
        let limits = notify_limits();
        let mut st = state();
        deliver(&mut st, &degraded("p1"), at(0));
        let before = st.clone();
        assert_eq!(check(&st, &limits, &degraded("p1"), at(1)), Verdict::Drop("duplicate"));
        assert_eq!(st, before, "check is read-only");
    }

    #[test]
    fn the_digest_summary_counts_by_class_and_cadences_once_an_hour() {
        let mut st = state();
        for _ in 0..3 {
            record(&mut st, &question("s1"), &Verdict::Digest("quiet_hours"), at(0));
        }
        for _ in 0..2 {
            record(&mut st, &degraded("p1"), &Verdict::Digest("cooldown"), at(0));
        }
        let (line, sent) = digest_due(&st, at(1)).expect("something held");
        assert_eq!(
            line, "Colonizer: 5 held announcements — provider_degraded ×2, question ×3",
            "counts by class, no identities"
        );
        // Nothing held, nothing due.
        assert_eq!(digest_due(&state(), at(1)), None);
        // The line goes out, and for the next hour it does not go out again — whatever else is held.
        commit_digest(&mut st, &sent, at(2));
        record(&mut st, &question("s2"), &Verdict::Digest("quiet_hours"), at(30));
        assert_eq!(digest_due(&st, at(35)), None, "too soon after the last one");
        assert_eq!(
            digest_due(&st, at(63)).map(|(line, _)| line).as_deref(),
            Some("Colonizer: 1 held announcements — question ×1")
        );
    }

    #[test]
    fn the_commit_subtracts_what_the_line_carried_so_candidates_held_in_flight_stay_due() {
        let mut st = state();
        for _ in 0..3 {
            record(&mut st, &question("s1"), &Verdict::Digest("quiet_hours"), at(0));
        }
        let (_, sent) = digest_due(&st, at(1)).expect("something held");
        // The line is in flight and two more candidates are held behind it — a deliver() can take
        // seconds, and the tick keeps recording meanwhile.
        for _ in 0..2 {
            record(&mut st, &question("s2"), &Verdict::Digest("quiet_hours"), at(5));
        }
        commit_digest(&mut st, &sent, at(6));
        assert_eq!(
            st.pending_digest.get("question"),
            Some(&2),
            "only what the line carried is subtracted"
        );
        // The cadence holds — the remainder is not a second line within the hour.
        assert_eq!(digest_due(&st, at(30)), None);
        assert_eq!(
            digest_due(&st, at(67)).map(|(line, _)| line).as_deref(),
            Some("Colonizer: 2 held announcements — question ×2")
        );
        // And a class counts down to gone, not to zero.
        let (_, sent) = digest_due(&st, at(67)).unwrap();
        commit_digest(&mut st, &sent, at(68));
        assert_eq!(st.pending_digest.get("question"), None);
    }

    #[tokio::test]
    async fn a_saved_ledger_loads_back_with_its_counters_entries_and_pending_digest() {
        let dir = std::env::temp_dir().join(format!("colonizer-ledger-roundtrip-{}", crate::util::short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = LedgerStore::load(&dir);
        assert_eq!(store.snapshot()["quarantined"], json!(0), "a missing file is a first run");
        store.record(&question("abc123"), &Verdict::Deliver, at(0)).await;
        store.record(&degraded("p1"), &Verdict::Digest("cooldown"), at(1)).await;
        store.record(&question("zzz"), &Verdict::Drop("daily_quota"), at(2)).await;
        // A restarted mothership reads the same book.
        let snapshot = LedgerStore::load(&dir).snapshot();
        assert_eq!(
            snapshot["counters"]["question"],
            json!({"delivered": 1, "digested": 0, "dropped": 1})
        );
        assert_eq!(snapshot["counters"]["provider_degraded"]["digested"], json!(1));
        assert_eq!(snapshot["pending_digest"]["provider_degraded"], json!(1));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_corrupt_ledger_file_is_quarantined_aside_and_the_store_starts_empty() {
        let dir = std::env::temp_dir().join(format!("colonizer-ledger-corrupt-{}", crate::util::short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ledger.json"), b"{ half a ledger").unwrap();
        let store = LedgerStore::load(&dir);
        assert_eq!(store.snapshot()["quarantined"], json!(1));
        assert!(store.state.lock().unwrap().entries.is_empty(), "never salvaged into lies");
        // The bytes survive, renamed aside — never wiped.
        assert!(!dir.join("ledger.json").exists());
        let mut aside = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("ledger.json.corrupt-"))
            .collect::<Vec<_>>();
        assert_eq!(aside.len(), 1, "exactly one quarantine copy: {aside:?}");
        assert_eq!(std::fs::read(dir.join(aside.remove(0))).unwrap(), b"{ half a ledger");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn one_fact_claimed_by_three_claimants_delivers_exactly_once() {
        let mut st = state();
        // Notify saw the question first and delivered it.
        let notify_claim = question("abc123");
        assert_eq!(check(&st, &notify_limits(), &notify_claim, at(0)), Verdict::Deliver);
        deliver(&mut st, &notify_claim, at(0));
        // The judge reaches for the same question — the same fact, its own kind — and is told no.
        let judge_claim = candidate(Kind::Judge, "judge:abc123", "judge", Some("question:abc123"));
        assert_eq!(check(&st, &notify_limits(), &judge_claim, at(1)), Verdict::Drop("duplicate"));
        record(&mut st, &judge_claim, &Verdict::Drop("duplicate"), at(1));
        // A third claimant, a third topic: the fact, not the claimant, is what is already told.
        let mut third = candidate(Kind::Notify, "question:abc123:again", "question", Some("question:abc123"));
        third.priority = true;
        assert_eq!(check(&st, &notify_limits(), &third, at(2)), Verdict::Drop("duplicate"));
        record(&mut st, &third, &Verdict::Drop("duplicate"), at(2));
        assert_eq!(total(&st, DELIVERED), 1, "one fact, one delivery");
        assert_eq!(total(&st, DROPPED), 2, "the other claimants are counted, not silent");
        assert_eq!(outcome_of(&st, Kind::Notify, "question:abc123"), DELIVERED);
        assert_eq!(outcome_of(&st, Kind::Judge, "judge:abc123"), DROPPED);
        // The lookup a caller uses to count a hold or a drop once per question.
        assert!(has_fact(&st, "question:abc123"));
        assert!(!has_fact(&st, "question:nobody"));
    }

    #[test]
    fn ten_colonies_and_a_flapping_provider_stay_bounded_and_every_candidate_is_counted() {
        let limits = notify_limits();
        let mut st = state();
        let mut candidates = 0;
        // Ten colonies ask at once: ten priority questions, all delivered. One provider flaps ten
        // times in the hour: the first crossing announces, the rest are one fact already told.
        for i in 0..10 {
            deliver(&mut st, &question(&format!("c{i}")), at(0));
            candidates += 1;
        }
        for minute in 1..=10u64 {
            let flap = degraded("p1");
            let verdict = check(&st, &limits, &flap, at(minute * 5));
            record(&mut st, &flap, &verdict, at(minute * 5));
            assert_eq!(
                verdict,
                if minute == 1 {
                    Verdict::Deliver
                } else {
                    Verdict::Drop("duplicate")
                },
                "flap {minute}"
            );
            candidates += 1;
        }
        // Two hours on, fourteen attention events in one hour: twelve go out, the last two wait for
        // the digest line instead of a thirteenth and fourteenth popup.
        for i in 0..14u64 {
            let c = candidate(
                Kind::Notify,
                &format!("attention:c{i}"),
                "attention",
                Some(&format!("attention:c{i}")),
            );
            let verdict = check(&st, &limits, &c, at(120 + i));
            record(&mut st, &c, &verdict, at(120 + i));
            assert_eq!(
                verdict,
                if i < 12 {
                    Verdict::Deliver
                } else {
                    Verdict::Digest("hourly_quota")
                },
                "attention {i}"
            );
            candidates += 1;
        }
        // Bounded, and conserved: every candidate is somewhere.
        assert_eq!(total(&st, DELIVERED), 23);
        assert_eq!(total(&st, DIGESTED), 2);
        assert_eq!(total(&st, DROPPED), 9);
        let counted: usize = st
            .counters
            .values()
            .map(|c| (c.delivered + c.digested + c.dropped) as usize)
            .sum();
        assert_eq!(counted, candidates, "delivered + digested + dropped == total candidates");
        assert_eq!(st.entries.len(), candidates, "every candidate is an entry too");
    }

    #[test]
    fn record_prunes_entries_older_than_two_days_and_caps_the_list() {
        let mut st = state();
        for minute in 0..30 {
            deliver(&mut st, &question(&format!("c{minute}")), at(minute));
        }
        // A record two days later prunes everything past the keep window in the same stroke.
        deliver(&mut st, &question("late"), at(50 * 60));
        assert!(
            st.entries.iter().all(|e| e.at > at(50 * 60) - KEEP),
            "only fresh entries remain"
        );
        assert_eq!(st.entries.len(), 1);
        // And the cap, not the flood, bounds the list: MAX_ENTRIES + 1 records keep MAX_ENTRIES.
        let mut flood = state();
        for i in 0..=MAX_ENTRIES {
            deliver(&mut flood, &question(&format!("f{i}")), at(0));
        }
        assert_eq!(flood.entries.len(), MAX_ENTRIES);
        assert_eq!(
            flood.entries.first().unwrap().topic,
            "question:f1",
            "the oldest fell out first"
        );
    }

    #[test]
    fn the_entry_schema_round_trips_and_defaults_what_an_older_build_left_out() {
        let entry = Entry {
            at: at(0),
            kind: Kind::Judge,
            topic: "judge:s1".into(),
            class: "judge".into(),
            fact: Some("judge:s1:q1".into()),
            colony: Some("s1".into()),
            outcome: DIGESTED.into(),
        };
        let written = serde_json::to_string(&entry).unwrap();
        assert!(written.contains("\"judge\""), "the kind serialises snake_case: {written}");
        assert_eq!(serde_json::from_str::<Entry>(&written).unwrap(), entry);
        // A file an earlier build wrote — no entries, no counters — still loads.
        assert_eq!(serde_json::from_str::<LedgerState>("{}").unwrap(), LedgerState::default());
    }

    #[test]
    fn the_snapshot_carries_counters_and_limits_and_no_colony_ids() {
        let mut st = state();
        deliver(&mut st, &question("secret-colony-id"), at(0));
        let store = LedgerStore {
            path: "unused".into(),
            state: Mutex::new(st),
            write: AsyncMutex::new(()),
            quarantined: 0,
        };
        let snapshot = store.snapshot();
        assert_eq!(snapshot["counters"]["question"]["delivered"], json!(1));
        assert_eq!(snapshot["limits"]["notify"]["per_hour"], json!(12));
        assert_eq!(snapshot["limits"]["judge"]["topic_daily_cap"], json!(20));
        assert!(
            !snapshot.to_string().contains("secret-colony-id"),
            "no colony id leaves the ledger: {snapshot}"
        );
    }

    #[test]
    fn the_judge_limits_cap_a_colony_at_twenty_answers_a_day() {
        let limits = Limits::for_kind(Kind::Judge);
        assert_eq!(limits.topic_daily_cap, 20);
        assert_eq!(limits.dedup_window, Duration::hours(24));
        assert_eq!(limits.topic_cooldown, Duration::zero());
        let mut st = state();
        for i in 0..20 {
            deliver(
                &mut st,
                &candidate(Kind::Judge, "judge:s1", "judge", Some(&format!("judge:s1:q{i}"))),
                at(i),
            );
        }
        // The twenty-first answer to the same colony's day: dropped, even though no soft layer
        // would have held it.
        assert_eq!(
            check(
                &st,
                &limits,
                &candidate(Kind::Judge, "judge:s1", "judge", Some("judge:s1:q21")),
                at(21)
            ),
            Verdict::Drop("topic_cap")
        );
    }
}
