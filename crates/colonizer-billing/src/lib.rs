//! Plan and dispute state for a hosted Colonizer account (issue #941, paid plans through Polar as
//! Merchant of Record).
//!
//! This is the billing core and nothing else: a pure state machine over normalized webhook events,
//! with no IO, no clock and no network. Events arrive already verified — the signature check and the
//! `PolarEvent` → [`Event`] mapping come from Cratefield's Payments port
//! (`cratefield-adapter-polar`) when that adapter is wired in, so the wiring is a name-for-name
//! rename between [`EventKind`] and the adapter's change names.
//!
//! Two rules hold throughout: non-payment only ever changes limits — this crate never deletes
//! anything, so a lapsed, unpaid or lost-dispute account keeps its colonies, its history and its
//! files — and every event is audited, [`Account::apply`] returning the [`AuditEntry`] values the
//! caller appends to the activity log (`crates/colonizer/src/activity.rs`), so "why is this account
//! on the free limits?" is answerable from the log. Paid limits come from a caller-owned
//! [`Catalog`]: a product it does not know gets the free limits rather than a guess.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

/// How many event ids an [`Account`] remembers. Polar redelivers a webhook that was not acknowledged
/// in time, so a redelivery must be a no-op; the ring is bounded at [`SEEN_EVENTS`], so a redelivery
/// older than that window is applied again rather than detected.
const SEEN_EVENTS: usize = 256;

/// The plan's billing period. Annual is the default: Polar charges a fixed fee per transaction, so
/// one annual charge a year costs a customer less than twelve monthly ones.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub enum Interval {
    Monthly,
    #[default]
    Annual,
}

/// What a plan allows, right now. Paid plans differ from the free plan in how many colonies may run
/// at once.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Limits {
    pub max_parallel: u32,
}

/// The free plan: 32 parallel colonies, the `max_parallel` ceiling of the free mothership's module
/// schema (`crates/colonizer/src/modules.rs`).
pub const FREE_LIMITS: Limits = Limits { max_parallel: 32 };

/// The product ids this deployment sells, with the limits each buys. An empty catalog means nothing
/// is paid, the right answer for a self-hosted mothership.
#[derive(Clone, Copy, Debug, Default)]
pub struct Catalog<'a>(&'a [(&'a str, Limits)]);

impl<'a> Catalog<'a> {
    pub const fn new(products: &'a [(&'a str, Limits)]) -> Self {
        Catalog(products)
    }

    /// The limits `product_id` buys, or `None` if this deployment does not sell it.
    pub fn get(&self, product_id: &str) -> Option<Limits> {
        self.0.iter().find(|(id, _)| *id == product_id).map(|(_, limits)| *limits)
    }
}

/// What an open dispute does while it is open. Pausing paid features by default is the safe reading
/// of a chargeback: the money is on its way back to the cardholder.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct DisputePolicy {
    pub pause_paid_features: bool,
}

impl Default for DisputePolicy {
    fn default() -> Self {
        DisputePolicy {
            pause_paid_features: true,
        }
    }
}

/// Where a subscription stands. A cancellation is not a revocation: Polar cancels at the end of the
/// period the customer already paid for, so `Canceling` keeps the paid limits until a revocation
/// takes the plan away.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub enum Status {
    Active,
    Canceling,
    /// A renewal charge failed. The plan is still owed, not held, but the account falls back to the
    /// free limits until an `OrderPaid` arrives.
    PastDue,
}

/// The plan a subscription change moves an account to, shared by the three subscription changes.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Plan {
    pub subscription_id: String,
    pub product_id: String,
    pub interval: Interval,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Subscription {
    pub plan: Plan,
    pub status: Status,
}

/// One change to a subscription, order or dispute, named after the adapter's change.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub enum EventKind {
    SubscriptionCreated(Plan),
    SubscriptionActive(Plan),
    /// Any change to an existing subscription: a different product, a different interval, a
    /// restored cancellation.
    SubscriptionUpdated(Plan),
    /// Cancelled at the period end; still paid until the period is over and the subscription is
    /// revoked.
    SubscriptionCanceled {
        subscription_id: String,
    },
    SubscriptionUncanceled {
        subscription_id: String,
    },
    SubscriptionPastDue {
        subscription_id: String,
    },
    SubscriptionRevoked {
        subscription_id: String,
    },
    OrderPaid {
        order_id: String,
    },
    OrderRefunded {
        order_id: String,
        full: bool,
    },
    DisputeOpened {
        dispute_id: String,
        order_id: String,
    },
    DisputeWon {
        dispute_id: String,
    },
    DisputeLost {
        dispute_id: String,
    },
}

/// A webhook delivery: the id Polar assigns it, which makes the delivery idempotent, and what it
/// says.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Event {
    pub id: String,
    pub kind: EventKind,
}

/// One line of explanation for the activity log; the caller appends these.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct AuditEntry {
    pub event_id: String,
    /// A dotted, greppable name such as `billing.plan_activated` or `billing.downgraded_free`.
    pub action: &'static str,
    pub detail: String,
}

/// Everything this crate knows about one account's billing, serialized so the caller can persist it
/// and keep idempotency across restarts.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct Account {
    subscription: Option<Subscription>,
    /// The disputes still open, keyed by dispute id. Several can be open at once — a customer can
    /// chargeback an order and then another — and each resolves on its own event.
    disputes: BTreeMap<String, String>,
    seen: VecDeque<String>,
}

impl Account {
    pub fn new() -> Self {
        Account::default()
    }

    pub fn subscription(&self) -> Option<&Subscription> {
        self.subscription.as_ref()
    }

    /// The disputes still open: dispute id to the order each is against.
    pub fn disputes(&self) -> &BTreeMap<String, String> {
        &self.disputes
    }

    /// Whether any dispute is open. Any one of them pauses the paid features, whichever order it is
    /// against: until the last resolves, the account is the one being charged back.
    pub fn flagged(&self) -> bool {
        !self.disputes.is_empty()
    }

    /// Whether the account's own state entitles it to paid limits. [`Account::limits`] asks this
    /// and then looks the product up, so an entitled account on an unsold product still gets the
    /// free limits.
    pub fn is_paid(&self, policy: &DisputePolicy) -> bool {
        let Some(subscription) = &self.subscription else {
            return false;
        };
        if !matches!(subscription.status, Status::Active | Status::Canceling) {
            return false;
        }
        !(self.flagged() && policy.pause_paid_features)
    }

    /// What this account may do right now.
    pub fn limits(&self, catalog: &Catalog<'_>, policy: &DisputePolicy) -> Limits {
        self.subscription
            .as_ref()
            .filter(|_| self.is_paid(policy))
            .and_then(|subscription| catalog.get(&subscription.plan.product_id))
            .unwrap_or(FREE_LIMITS)
    }

    /// Folds one event into the account and returns the audit entries it produced, oldest first. A
    /// redelivered event id returns nothing at all, so replaying a webhook batch is safe.
    pub fn apply(&mut self, event: &Event, policy: &DisputePolicy) -> Vec<AuditEntry> {
        if self.seen.contains(&event.id) {
            return Vec::new();
        }
        self.seen.push_back(event.id.clone());
        while self.seen.len() > SEEN_EVENTS {
            self.seen.pop_front();
        }
        let mut entries = Vec::new();
        let mut entry = |action: &'static str, detail: String| {
            entries.push(AuditEntry {
                event_id: event.id.clone(),
                action,
                detail,
            });
        };
        match &event.kind {
            EventKind::SubscriptionCreated(plan) | EventKind::SubscriptionActive(plan) | EventKind::SubscriptionUpdated(plan) => {
                let previous = self.subscription.as_ref().map(|s| (s.plan.product_id.clone(), s.status));
                let detail = format!("{} on {:?} ({})", plan.product_id, plan.interval, plan.subscription_id);
                self.subscription = Some(Subscription {
                    plan: plan.clone(),
                    status: Status::Active,
                });
                match previous {
                    None => entry("billing.plan_activated", detail),
                    Some((from, _)) if from != plan.product_id => entry("billing.plan_changed", format!("{from} -> {detail}")),
                    Some((_, Status::Active)) => entry("billing.plan_unchanged", detail),
                    // A past-due or cancelling subscription is paid again by the same event.
                    Some(_) => entry("billing.plan_activated", format!("restored, {detail}")),
                }
            }
            EventKind::SubscriptionCanceled { subscription_id } => {
                self.on_subscription(subscription_id, &mut entry, "billing.cancel_scheduled", |s| {
                    s.status = Status::Canceling
                });
            }
            EventKind::SubscriptionUncanceled { subscription_id } => {
                self.on_subscription(subscription_id, &mut entry, "billing.uncanceled", |s| {
                    s.status = Status::Active
                });
            }
            EventKind::SubscriptionPastDue { subscription_id } => {
                self.on_subscription(subscription_id, &mut entry, "billing.past_due", |s| {
                    s.status = Status::PastDue
                });
            }
            EventKind::SubscriptionRevoked { subscription_id } => {
                if self.take_subscription(subscription_id) {
                    entry("billing.downgraded_free", format!("subscription {subscription_id} revoked"));
                } else {
                    entry("billing.event_ignored", format!("no subscription {subscription_id}"));
                }
            }
            EventKind::OrderPaid { order_id } => match self.subscription.as_mut() {
                Some(subscription) if subscription.status == Status::PastDue => {
                    subscription.status = Status::Active;
                    entry("billing.restored", format!("order {order_id} paid"));
                }
                _ => entry("billing.order_paid", format!("order {order_id}")),
            },
            EventKind::OrderRefunded { order_id, full: true } => {
                let action = if self.subscription.take().is_some() {
                    "billing.downgraded_free"
                } else {
                    "billing.order_refunded"
                };
                entry(action, format!("order {order_id} refunded in full"));
            }
            // A partial refund is money back for something, not for the plan.
            EventKind::OrderRefunded { order_id, full: false } => {
                entry("billing.refund_partial", format!("order {order_id} partially refunded"));
            }
            EventKind::DisputeOpened { dispute_id, order_id } => {
                let was_paid = self.is_paid(policy);
                match self.disputes.insert(dispute_id.clone(), order_id.clone()) {
                    // Already open — re-reported, or redelivered under a new delivery id.
                    Some(order) => entry(
                        "billing.dispute_opened",
                        format!("dispute {dispute_id} already open on order {order}"),
                    ),
                    None => {
                        entry("billing.dispute_opened", format!("dispute {dispute_id} on order {order_id}"));
                        if was_paid && !self.is_paid(policy) {
                            entry("billing.downgraded_free", format!("dispute {dispute_id} is open"));
                        }
                    }
                }
            }
            EventKind::DisputeWon { dispute_id } => match self.disputes.remove(dispute_id) {
                Some(order) => {
                    entry("billing.dispute_won", format!("dispute {dispute_id} on order {order} won"));
                    if !self.flagged() {
                        entry("billing.restored", "no dispute left open".to_string());
                    }
                }
                None => entry("billing.dispute_mismatch", format!("dispute {dispute_id} won, none open")),
            },
            EventKind::DisputeLost { dispute_id } => match self.disputes.remove(dispute_id) {
                Some(order) => {
                    entry("billing.dispute_lost", format!("dispute {dispute_id} on order {order} lost"));
                    // The plan goes with the money; the account does not. Whether Polar sells to
                    // this customer again is Polar's call, and a later subscription event pays.
                    if self.subscription.take().is_some() {
                        entry("billing.downgraded_free", format!("dispute {dispute_id} lost"));
                    }
                }
                None => entry("billing.dispute_mismatch", format!("dispute {dispute_id} lost, none open")),
            },
        }
        debug_assert!(!entries.is_empty(), "every event produces at least one audit entry");
        entries
    }

    /// Applies `change` when the event names this account's subscription, and otherwise records
    /// that it was about someone else's — worth seeing in the log, not an error worth refusing.
    fn on_subscription(
        &mut self,
        subscription_id: &str,
        entry: &mut impl FnMut(&'static str, String),
        action: &'static str,
        change: impl FnOnce(&mut Subscription),
    ) {
        match self
            .subscription
            .as_mut()
            .filter(|s| s.plan.subscription_id == subscription_id)
        {
            Some(subscription) => change(subscription),
            None => return entry("billing.event_ignored", format!("no subscription {subscription_id}")),
        }
        entry(action, format!("subscription {subscription_id}"));
    }

    /// Drops the subscription if the event names it, so a webhook about someone else's subscription
    /// changes nothing. Returns whether it did.
    fn take_subscription(&mut self, subscription_id: &str) -> bool {
        if self
            .subscription
            .as_ref()
            .is_some_and(|s| s.plan.subscription_id == subscription_id)
        {
            self.subscription = None;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUB: &str = "sub_1";
    const PRO: &str = "prod_team";
    const ORDER: &str = "order_1";
    const PAID: u32 = 128;
    const FREE: u32 = FREE_LIMITS.max_parallel;
    const CATALOG: Catalog<'static> = Catalog::new(&[(PRO, Limits { max_parallel: PAID })]);

    fn plan(product: &str) -> Plan {
        Plan {
            subscription_id: SUB.into(),
            product_id: product.into(),
            interval: Interval::Annual,
        }
    }

    /// One event, named as a test names it: `"canceled"`, or `"dispute:d1"` for the kinds that
    /// need a dispute id. A test reads as the sequence of deliveries it stands for.
    fn event(name: &str) -> EventKind {
        let (kind, id) = name.split_once(':').unwrap_or((name, ""));
        match kind {
            "created" => EventKind::SubscriptionCreated(plan(PRO)),
            "updated" => EventKind::SubscriptionUpdated(plan("prod_org")),
            "canceled" => EventKind::SubscriptionCanceled {
                subscription_id: SUB.into(),
            },
            "uncanceled" => EventKind::SubscriptionUncanceled {
                subscription_id: SUB.into(),
            },
            "past_due" => EventKind::SubscriptionPastDue {
                subscription_id: SUB.into(),
            },
            "revoked" => EventKind::SubscriptionRevoked {
                subscription_id: SUB.into(),
            },
            // A webhook naming a subscription this account does not have.
            "foreign" => EventKind::SubscriptionRevoked {
                subscription_id: "sub_other".into(),
            },
            "paid" => EventKind::OrderPaid { order_id: ORDER.into() },
            "refunded" => EventKind::OrderRefunded {
                order_id: ORDER.into(),
                full: true,
            },
            "partial" => EventKind::OrderRefunded {
                order_id: ORDER.into(),
                full: false,
            },
            "dispute" => EventKind::DisputeOpened {
                dispute_id: id.into(),
                order_id: ORDER.into(),
            },
            "won" => EventKind::DisputeWon { dispute_id: id.into() },
            "lost" => EventKind::DisputeLost { dispute_id: id.into() },
            other => panic!("no event named {other}"),
        }
    }

    /// Applies `events` in order and returns the account, the action each produced, and the
    /// account's parallel-colony limit. Ids are numbered; a test that cares about a specific one
    /// uses [`deliver`] instead.
    fn run_with(policy: &DisputePolicy, events: &[&str]) -> (Account, Vec<&'static str>, u32) {
        let mut account = Account::new();
        let mut actions = Vec::new();
        for (n, name) in events.iter().enumerate() {
            actions.extend(actions_of(&deliver(&mut account, &format!("e{n}"), name)));
        }
        let max_parallel = account.limits(&CATALOG, policy).max_parallel;
        (account, actions, max_parallel)
    }

    /// Applies one named event under a chosen delivery id, returning its audit entries.
    fn deliver(account: &mut Account, id: &str, name: &str) -> Vec<AuditEntry> {
        let event = Event {
            id: id.to_string(),
            kind: event(name),
        };
        account.apply(&event, &DisputePolicy::default())
    }

    fn actions_of(entries: &[AuditEntry]) -> Vec<&'static str> {
        entries.iter().map(|e| e.action).collect()
    }

    fn run(events: &[&str]) -> (Account, Vec<&'static str>, u32) {
        run_with(&DisputePolicy::default(), events)
    }

    #[test]
    fn checkout_upgrades_limits() {
        let (account, actions, max_parallel) = run(&["created"]);
        assert!(account.is_paid(&DisputePolicy::default()));
        assert_eq!(max_parallel, PAID);
        assert_eq!(actions, ["billing.plan_activated"]);
        assert_eq!(Interval::default(), Interval::Annual, "annual is the default interval");
        let policy = DisputePolicy::default();
        assert_eq!(
            account.limits(&Catalog::default(), &policy),
            FREE_LIMITS,
            "an unsold product is free"
        );
    }

    #[test]
    fn cancel_keeps_paid_until_the_period_ends() {
        let (canceling, actions, max_parallel) = run(&["created", "canceled"]);
        assert_eq!(canceling.subscription().map(|s| s.status), Some(Status::Canceling));
        // Still paid for the period the customer already paid for.
        assert_eq!(max_parallel, PAID);
        assert_eq!(actions, ["billing.plan_activated", "billing.cancel_scheduled"]);
        let (ended, actions, max_parallel) = run(&["created", "canceled", "revoked"]);
        assert!(ended.subscription().is_none());
        assert_eq!(max_parallel, FREE);
        assert_eq!(actions[2], "billing.downgraded_free");
        let (kept, _, max_parallel) = run(&["created", "canceled", "uncanceled"]);
        assert_eq!(kept.subscription().map(|s| s.status), Some(Status::Active));
        assert_eq!(max_parallel, PAID);
    }

    #[test]
    fn past_due_falls_back_then_a_paid_order_restores() {
        let (account, actions, max_parallel) = run(&["created", "past_due", "paid"]);
        assert_eq!(max_parallel, PAID);
        assert_eq!(actions, ["billing.plan_activated", "billing.past_due", "billing.restored"]);
        assert_eq!(account.subscription().map(|s| s.status), Some(Status::Active));
    }

    #[test]
    fn a_full_refund_falls_back_to_free_and_a_partial_one_does_not() {
        assert_eq!(run(&["created", "refunded"]).2, FREE);
        let (_, actions, max_parallel) = run(&["created", "partial"]);
        assert_eq!(max_parallel, PAID);
        assert_eq!(actions[1], "billing.refund_partial");
    }

    #[test]
    fn an_open_dispute_pauses_the_paid_features() {
        let (account, _, max_parallel) = run(&["created", "dispute:d1"]);
        assert!(account.flagged());
        assert_eq!(account.disputes().get("d1").map(String::as_str), Some(ORDER));
        assert_eq!(max_parallel, FREE);
        let keep_paying = DisputePolicy {
            pause_paid_features: false,
        };
        assert_eq!(
            run_with(&keep_paying, &["created", "dispute:d1"]).2,
            PAID,
            "the policy can keep them on"
        );
    }

    #[test]
    fn a_dispute_opened_then_lost_flags_then_downgrades() {
        let (account, actions, max_parallel) = run(&["created", "dispute:d1", "lost:d1"]);
        assert_eq!(
            actions,
            [
                "billing.plan_activated",
                "billing.dispute_opened",
                "billing.downgraded_free",
                "billing.dispute_lost",
                "billing.downgraded_free"
            ]
        );
        assert!(!account.flagged(), "a resolved dispute is not open any more");
        assert!(account.disputes().is_empty());
        assert_eq!(max_parallel, FREE);
        // The account itself is unharmed: it may buy the plan again.
        assert_eq!(run(&["created", "dispute:d1", "lost:d1", "created"]).2, PAID);
    }

    #[test]
    fn a_dispute_opened_then_won_restores() {
        let (account, actions, max_parallel) = run(&["created", "dispute:d1", "won:d1"]);
        assert_eq!(actions.last(), Some(&"billing.restored"));
        assert!(!account.flagged());
        assert_eq!(max_parallel, PAID);
    }

    #[test]
    fn two_disputes_open_resolve_one_at_a_time() {
        let (both, _, max_parallel) = run(&["created", "dispute:d1", "dispute:d2"]);
        assert_eq!(both.disputes().len(), 2);
        assert_eq!(max_parallel, FREE);

        // Winning one leaves the other open, so the account stays flagged and on the free limits.
        let (one, actions, max_parallel) = run(&["created", "dispute:d1", "dispute:d2", "won:d1"]);
        assert_eq!(one.disputes().keys().collect::<Vec<_>>(), ["d2"]);
        assert!(one.flagged());
        assert_eq!(max_parallel, FREE);
        assert!(!actions.contains(&"billing.restored"), "a dispute is still open");

        // Resolving the last one restores the account.
        let (none, actions, max_parallel) = run(&["created", "dispute:d1", "dispute:d2", "won:d1", "won:d2"]);
        assert!(none.disputes().is_empty() && !none.flagged());
        assert_eq!(max_parallel, PAID);
        assert_eq!(actions.last(), Some(&"billing.restored"));

        // Losing one drops the plan but leaves the other dispute to resolve.
        let (one, _, max_parallel) = run(&["created", "dispute:d1", "dispute:d2", "lost:d2"]);
        assert_eq!(one.disputes().keys().collect::<Vec<_>>(), ["d1"]);
        assert!(one.flagged());
        assert_eq!(max_parallel, FREE);
    }

    #[test]
    fn a_dispute_opened_twice_is_one_dispute() {
        let mut account = Account::new();
        deliver(&mut account, "e0", "created");
        deliver(&mut account, "e1", "dispute:d1");
        // Re-reported under a new delivery id: the dispute is open, so this one adds nothing.
        assert_eq!(deliver(&mut account, "e2", "dispute:d1").len(), 1);
        assert_eq!(account.disputes().len(), 1);
        // And it still resolves once.
        assert_eq!(deliver(&mut account, "e3", "won:d1").len(), 2);
        assert!(account.disputes().is_empty());
    }

    #[test]
    fn an_event_naming_another_subscription_or_dispute_is_recorded_and_ignored() {
        let (account, actions, max_parallel) = run(&["created", "foreign"]);
        assert_eq!(actions[1], "billing.event_ignored");
        assert!(account.subscription().is_some(), "someone else's revocation took nothing");
        assert_eq!(max_parallel, PAID);
        let (account, actions, max_parallel) = run(&["created", "dispute:d1", "won:other"]);
        assert_eq!(actions[3], "billing.dispute_mismatch");
        assert_eq!(account.disputes().keys().collect::<Vec<_>>(), ["d1"], "d1 is still open");
        assert_eq!(max_parallel, FREE);
    }

    #[test]
    fn a_redelivered_event_is_a_no_op() {
        let mut account = Account::new();
        assert_eq!(deliver(&mut account, "e1", "created").len(), 1);
        assert!(deliver(&mut account, "e1", "created").is_empty(), "the same id again");
        assert_eq!(account.limits(&CATALOG, &DisputePolicy::default()).max_parallel, PAID);
    }

    #[test]
    fn the_seen_ring_forgets_the_oldest_ids() {
        let mut account = Account::new();
        for n in 0..=SEEN_EVENTS {
            deliver(&mut account, &format!("e{n}"), "paid");
        }
        assert!(account.seen.len() <= SEEN_EVENTS);
        assert!(!account.seen.contains(&"e0".to_string()));
        assert_eq!(
            deliver(&mut account, "e0", "created").len(),
            1,
            "an id past the ring applies again"
        );
    }

    #[test]
    fn an_account_round_trips_through_serde() {
        let (account, _, _) = run(&["created", "dispute:d1"]);
        let json = serde_json::to_string(&account).expect("serializes");
        let mut reloaded: Account = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(reloaded, account);
        assert_eq!(reloaded.disputes().get("d1").map(String::as_str), Some(ORDER));
        // The seen-id ring came back too, so a webhook redelivered after the restart is still a
        // no-op rather than a second state change.
        assert!(deliver(&mut reloaded, "e1", "dispute:d1").is_empty());
        assert_eq!(reloaded.disputes().len(), 1);
    }

    #[test]
    fn changing_plan_is_audited_as_a_change() {
        let (account, actions, _) = run(&["created", "updated"]);
        assert_eq!(actions[1], "billing.plan_changed");
        assert_eq!(account.subscription().map(|s| s.plan.product_id.as_str()), Some("prod_org"));
    }
}
