//! Model routing: choose the model tier a colony runs on for one task, so a one-line README fix
//! does not pay for the same model as a nine-file refactor. The rule is deliberately heuristic —
//! labels, body sizes and a path count, with no model and no network behind it — and it is a pure
//! function of the task's signals and the colony's settings, so it can be tested apart from the
//! boot path that applies it.

use crate::providers;
use serde::Serialize;
use std::collections::BTreeSet;

/// Labels that mark a task as small: each match takes two points off the score. Matched
/// case-insensitively against trimmed label names, and a label matches only when it equals one of
/// these exactly.
pub(crate) const LOW_LABELS: [&str; 5] = ["chore", "copy", "docs", "documentation", "typo"];

/// Labels that mark a task as large: each match adds three points to the score, and a high label
/// wins when an issue carries both kinds.
pub(crate) const HIGH_LABELS: [&str; 4] = ["breaking-change", "epic", "migration", "refactor"];

/// Extensions that let a token count as a path on its ending alone, matched case-sensitively: a
/// bare filename needs one, and so does a path with a single slash, which has no deeper segment to
/// prove itself with.
const EXTENSIONS: [&str; 18] = [
    ".rs", ".toml", ".json", ".md", ".ts", ".tsx", ".js", ".mjs", ".jsx", ".py", ".go", ".sh", ".css", ".html", ".yml", ".yaml",
    ".sql", ".lock",
];

/// How much reasoning a task needs. Ordered by capability and price, cheapest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Low,
    Medium,
    High,
}

impl Tier {
    /// The tier's name as it is spelled in settings, tables and log lines.
    pub fn as_str(&self) -> &'static str {
        match self {
            Tier::Low => "low",
            Tier::Medium => "medium",
            Tier::High => "high",
        }
    }

    /// Read a tier from a setting or a request: case-insensitive, tolerant of stray whitespace, and
    /// `None` for anything that is not a tier.
    pub fn parse(s: &str) -> Option<Tier> {
        match s.trim().to_ascii_lowercase().as_str() {
            "low" => Some(Tier::Low),
            "medium" => Some(Tier::Medium),
            "high" => Some(Tier::High),
            _ => None,
        }
    }
}

/// What the rule knows about a task. Everything here is read off the issue, so the decision costs nothing.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Signals {
    /// The strongest tier-bearing label on the issue, if it carries one.
    pub label: Option<&'static str>,
    /// Characters of task text, after trimming.
    pub body_chars: usize,
    /// Markdown checklist items in the text.
    pub checklist_items: usize,
    /// Distinct file paths the text names.
    pub paths: usize,
    /// Whether every path it names sits in one directory. True when it names none.
    pub one_directory: bool,
    /// Whether the colony boots on a sandbox preset the harness knows. A custom stack means the
    /// harness supplies no defaults for it, so such a colony never routes down to the cheapest tier.
    pub known_preset: bool,
    /// An optional second opinion from the external Jev classifier (`jev.rs`), fetched by the boot
    /// path before `decide` runs. Recorded only — it never changes the rule's score or `decide`'s
    /// output tier.
    pub jev: Option<JevOpinion>,
}

/// A recorded-but-not-applied second opinion from an optional external classifier ("Jev"), fetched
/// by the boot path before `decide` runs. `decide` copies it through unchanged into `Decision` — it
/// never affects `tier`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct JevOpinion {
    pub tier: Tier,
    pub model: String,
    pub confidence: f64,
    pub estimated_cost_usd: f64,
}

/// Per-task routing as configured. Resolved by the caller from module settings and the colony record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoutingSettings {
    /// Whether to route per task at all. Off: a colony with no tier of its own runs on the module's
    /// `model`.
    pub enabled: bool,
    /// A tier this colony was started with, which wins over the rule — honoured even when routing is
    /// off.
    pub chosen: Option<Tier>,
}

/// What decided the tier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Off,
    Rule,
    Override,
}

/// The tier a colony will run on, with everything that chose it, so the choice can be logged and
/// later measured against what it should have been.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Decision {
    /// The tier in force.
    pub tier: Tier,
    /// The tier the rule alone would have chosen, whatever else overrode it.
    pub rule: Tier,
    /// What settled it.
    pub source: Source,
    /// The rule's score, kept so a recorded decision can be re-scored later.
    pub score: i32,
    /// One clause naming the tier and the signals behind it, for the session log.
    pub reason: String,
    /// Jev's second opinion, copied through from `Signals` unchanged. Shadow mode only: never read
    /// by `decide` to pick `tier`.
    pub jev: Option<JevOpinion>,
}

impl Decision {
    /// Whether the colony ended up somewhere the rule did not want. A human overriding the rule is
    /// the cheapest misroute label there is: everything else the rule can get wrong is the rule's
    /// own error, but an override against its advice is worth counting on its own.
    pub fn misroute(&self) -> bool {
        self.source == Source::Override && self.tier != self.rule
    }

    /// Whether Jev's tier agrees with the rule's own tier — `None` when no opinion was recorded.
    /// Shadow-mode comparison only: this never feeds back into `tier`.
    pub fn jev_agrees(&self) -> Option<bool> {
        Some(self.jev.as_ref()?.tier == self.rule)
    }
}

/// Read the signals off a task: its title and text, the issue labels, and whether the colony's
/// sandbox preset is a known one.
pub fn signals(title: &str, text: &str, labels: &[String], known_preset: bool) -> Signals {
    let body_chars = text.trim().chars().count();
    let checklist_items = text.lines().filter(|line| is_checklist_item(line)).count();
    let named = paths_in(title, text);
    let mut directories = named.iter().map(|path| directory_of(path));
    let one_directory = match directories.next() {
        None => true,
        Some(first) => directories.all(|directory| directory == first),
    };
    Signals {
        label: tier_label(labels),
        body_chars,
        checklist_items,
        paths: named.len(),
        one_directory,
        known_preset,
        jev: None,
    }
}

/// The tier a colony runs on. Pure: the same task always routes the same way.
pub fn decide(settings: &RoutingSettings, signals: &Signals) -> Decision {
    let score = score(signals);
    let rule = rule_tier(signals, score);
    let detail = score_detail(signals, score);
    // An explicit tier is an operator instruction, so it is honoured whether or not the rule is on.
    if let Some(chosen) = settings.chosen {
        return Decision {
            tier: chosen,
            rule,
            source: Source::Override,
            score,
            reason: format!(
                "{} tier, set for this colony; the rule says {} at {detail}",
                chosen.as_str(),
                rule.as_str()
            ),
            jev: signals.jev.clone(),
        };
    }
    if !settings.enabled {
        return Decision {
            tier: Tier::Medium,
            rule,
            source: Source::Off,
            score,
            reason: "per-task routing is off, so the colony runs on the module's model".to_string(),
            jev: signals.jev.clone(),
        };
    }
    Decision {
        tier: rule,
        rule,
        source: Source::Rule,
        score,
        reason: format!("{} tier, {detail}", rule.as_str()),
        jev: signals.jev.clone(),
    }
}

/// The model a tier runs on: the tier's own setting when it has one, otherwise the module's `model`.
/// An empty result means leave the model the agent module already chose alone. A setting that is
/// empty once trimmed counts as unset, so a blank value never wins.
pub fn model_for<'a>(tier: Tier, low: &'a str, model: &'a str, high: &'a str) -> &'a str {
    let own = match tier {
        Tier::Low => low,
        Tier::Medium => model,
        Tier::High => high,
    };
    // Low and high fall back to the module model when their own setting is blank; medium's own
    // setting is that model already, so the fallback cannot change its answer.
    let resolved = if own.trim().is_empty() { model } else { own };
    if resolved.trim().is_empty() { "" } else { resolved }
}

/// Token volumes behind a routed subtask's cost estimate: X (context the cheaper model must load
/// before it can start), Y (its output), and Z (extra tokens the direct model re-reads afterward to
/// pick up what changed). All three are operator-supplied estimates for now — there is no live
/// per-colony measurement of them yet (see the token-breakdown work planned for #469).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct RoutingCostTokens {
    pub context_tokens: u64,
    pub output_tokens: u64,
    pub reread_tokens: u64,
}

/// What routing a subtask down is estimated to cost, in dollars, against doing it directly on the
/// module's own model.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct RoutingCostEstimate {
    pub direct_usd: f64,
    pub routed_usd: f64,
}

impl RoutingCostEstimate {
    /// Whether the estimate favors routing down.
    pub fn worth_routing(&self) -> bool {
        self.routed_usd < self.direct_usd
    }
}

/// Prices a routed subtask against doing it directly. The direct model pays for the output and for
/// re-reading what changed afterward (`Z`); routing down additionally makes the cheaper model pay to
/// load the context (`X`) the direct model already held, on top of its own output. This is a
/// simplification of the note behind #470 — it does not split `Y`/`Z` between the two models the way
/// the note's own worked numbers do, for lack of a measured split — but it keeps the one term the
/// issue is about: a routed subtask pays for context the direct model would not have had to reload.
pub fn estimate_cost(direct: providers::Pricing, routed: providers::Pricing, tokens: RoutingCostTokens) -> RoutingCostEstimate {
    let direct_usd = direct.cost_usd(providers::Usage {
        output_tokens: tokens.output_tokens,
        input_tokens: tokens.reread_tokens,
        ..Default::default()
    });
    let routed_usd = routed.cost_usd(providers::Usage {
        input_tokens: tokens.context_tokens,
        output_tokens: tokens.output_tokens,
        ..Default::default()
    }) + direct.cost_usd(providers::Usage {
        input_tokens: tokens.reread_tokens,
        ..Default::default()
    });
    RoutingCostEstimate { direct_usd, routed_usd }
}

/// The rule's score for a task: one point per unit of bulk, plus one for crossing directories, plus
/// the label's pull in either direction. It can dip below zero when a low label outweighs everything
/// else, and a score of one or less is what makes a task low.
fn score(signals: &Signals) -> i32 {
    let body = match signals.body_chars {
        0..=400 => 0,
        401..=2_000 => 1,
        2_001..=6_000 => 2,
        _ => 3,
    };
    let checklist = match signals.checklist_items {
        0..=1 => 0,
        2..=5 => 1,
        _ => 2,
    };
    let paths = match signals.paths {
        0..=1 => 0,
        2..=4 => 1,
        _ => 2,
    };
    let crossing = i32::from(signals.paths > 1 && !signals.one_directory);
    let label = match signals.label {
        Some(name) if HIGH_LABELS.contains(&name) => 3,
        Some(name) if LOW_LABELS.contains(&name) => -2,
        _ => 0,
    };
    body + checklist + paths + crossing + label
}

/// The tier a score lands on: one point or fewer is low, up to four is medium, anything above is high.
fn tier_for_score(score: i32) -> Tier {
    if score <= 1 {
        Tier::Low
    } else if score <= 4 {
        Tier::Medium
    } else {
        Tier::High
    }
}

/// The rule's tier for a task: the score's tier, then the clamp — a colony the harness cannot supply
/// defaults for is never routed down to the cheapest tier, however tiny the task looks.
fn rule_tier(signals: &Signals, score: i32) -> Tier {
    if !signals.known_preset && tier_for_score(score) == Tier::Low {
        Tier::Medium
    } else {
        tier_for_score(score)
    }
}

/// The signals behind a score, as one clause for the log — `score 0: a 180-character body, no
/// checklist items, 1 path named` — with the label and the preset clamp appended when they took part.
fn score_detail(signals: &Signals, score: i32) -> String {
    let checklist = match signals.checklist_items {
        0 => "no checklist items".to_string(),
        1 => "1 checklist item".to_string(),
        n => format!("{n} checklist items"),
    };
    let paths = match signals.paths {
        0 => "no paths named".to_string(),
        1 => "1 path named".to_string(),
        n => format!("{n} paths named"),
    };
    let paths = if signals.paths > 1 {
        let spread = if signals.one_directory {
            " in one directory"
        } else {
            " across directories"
        };
        format!("{paths}{spread}")
    } else {
        paths
    };
    let mut detail = format!("score {score}: a {}-character body, {checklist}, {paths}", signals.body_chars);
    if let Some(label) = signals.label {
        detail.push_str(&format!(", and the {label} label"));
    }
    if !signals.known_preset && tier_for_score(score) == Tier::Low {
        detail.push_str(", kept off low because the sandbox preset is unknown");
    }
    detail
}

/// The strongest tier-bearing label on an issue: a high label wins when both kinds are present, and
/// within a kind the table order decides. The name comes back from the table, so it is one of a
/// closed set, like `watchdog::WATCHDOG_REASONS`.
fn tier_label(labels: &[String]) -> Option<&'static str> {
    let from = |table: &[&'static str]| {
        table.iter().find_map(|name| {
            labels
                .iter()
                .any(|label| label.trim().eq_ignore_ascii_case(name))
                .then_some(*name)
        })
    };
    from(&HIGH_LABELS).or_else(|| from(&LOW_LABELS))
}

/// A Markdown checklist item: a list line (`-` or `*`) whose first thing after it is a task box.
fn is_checklist_item(line: &str) -> bool {
    let line = line.trim();
    ["- [ ]", "- [x]", "- [X]", "* [ ]", "* [x]", "* [X]"]
        .iter()
        .any(|box_mark| line.starts_with(box_mark))
}

/// Every distinct file path named across the title and the text. A `BTreeSet`, so the count and the
/// directory comparison never depend on the order the paths appeared in.
fn paths_in<'a>(title: &'a str, text: &'a str) -> BTreeSet<&'a str> {
    title
        .split_whitespace()
        .chain(text.split_whitespace())
        .filter_map(as_path)
        .collect()
}

/// Decide whether one whitespace-free token names a file path, after stripping the punctuation and
/// markup prose wraps paths in. Whitespace cannot survive the split, so a token that passes the
/// length bound is a single word; it counts when it ends in a known extension — a bare filename or a
/// two-segment path like `docs/protocol.md` — or when it carries three or more non-empty segments
/// (`crates/colonizer/src`), since one slash without an extension is as much `and/or` as `web/src`.
/// A quote surviving the trim means the token glued two pieces of prose together, so it does not count.
fn as_path(token: &str) -> Option<&str> {
    let token = trim_markup(token);
    let starts_like_a_word = token
        .chars()
        .next()
        .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.');
    if !(3..=200).contains(&token.len())
        || token.contains("://")
        || token.starts_with("http")
        || token.starts_with('#')
        || token.starts_with('@')
        || token.ends_with('/')
        || !starts_like_a_word
        || token.contains(['`', '\'', '"'])
        || token.chars().all(|c| c == '/' || c == '.')
    {
        return None;
    }
    let mut segments = token.split('/');
    let deep = segments.clone().count() >= 3 && segments.all(|segment| !segment.is_empty());
    let extension = EXTENSIONS.iter().any(|ext| token.ends_with(ext));
    (deep || extension).then_some(token)
}

/// The characters prose wraps paths in: markup quotes and brackets on either side, and the sentence
/// punctuation that only makes sense at the end. A leading `.` is left alone, since dotfiles start with one.
fn trim_markup(token: &str) -> &str {
    token
        .trim_matches(['`', '\'', '"', '(', ')', '[', ']', '<', '>', ',', ';', ':'])
        .trim_end_matches(['.', '!', '?'])
}

/// The directory part of a path: everything before its last `/`, empty for a bare filename.
fn directory_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signals_with(
        body_chars: usize,
        checklist_items: usize,
        paths: usize,
        one_directory: bool,
        label: Option<&'static str>,
    ) -> Signals {
        Signals {
            label,
            body_chars,
            checklist_items,
            paths,
            one_directory,
            known_preset: true,
            jev: None,
        }
    }

    fn routed(task: &Signals) -> Decision {
        decide(
            &RoutingSettings {
                enabled: true,
                chosen: None,
            },
            task,
        )
    }

    #[test]
    fn a_typo_label_pulls_a_small_multi_path_task_to_low_and_the_same_task_without_it_stays_medium() {
        let text = "fix the typo in src/main.rs src/lib.rs src/util.rs web/app.ts web/index.html";
        let typo = signals("fix a typo", text, &["typo".to_string()], true);
        assert_eq!(typo.label, Some("typo"));
        assert_eq!(typo.paths, 5);
        assert!(!typo.one_directory);
        assert_eq!(
            decide(&RoutingSettings { enabled: true, chosen: None }, &typo),
            Decision {
                tier: Tier::Low,
                rule: Tier::Low,
                source: Source::Rule,
                score: 1,
                reason: "low tier, score 1: a 76-character body, no checklist items, 5 paths named across directories, and the typo label"
                    .to_string(),
                jev: None,
            }
        );
        let plain = signals("fix a typo", text, &[], true);
        let without = routed(&plain);
        assert_eq!(without.tier, Tier::Medium);
        assert_eq!(without.score, 3);
    }

    #[test]
    fn a_docs_label_does_not_drag_a_long_checklist_heavy_many_path_issue_down_to_low() {
        let docs = routed(&signals_with(5_000, 6, 5, false, Some("docs")));
        assert_eq!(docs.score, 5);
        assert_eq!(docs.tier, Tier::High);
        assert_eq!(docs.rule, Tier::High);
        // Without the label the same issue scores two higher, so the label's pull is exactly -2.
        let unlabelled = routed(&signals_with(5_000, 6, 5, false, None));
        assert_eq!(unlabelled.score, 7);
        assert_eq!(unlabelled.tier, Tier::High);
    }

    #[test]
    fn a_score_of_one_or_less_routes_low_five_or_more_routes_high_and_between_them_medium() {
        let one = routed(&signals_with(401, 0, 0, true, None));
        assert_eq!(one.score, 1);
        assert_eq!(one.tier, Tier::Low);

        let two = routed(&signals_with(401, 2, 0, true, None));
        assert_eq!(two.score, 2);
        assert_eq!(two.tier, Tier::Medium);

        let four = routed(&signals_with(2_001, 2, 2, true, None));
        assert_eq!(four.score, 4);
        assert_eq!(four.tier, Tier::Medium);

        let five = routed(&signals_with(2_001, 2, 2, false, None));
        assert_eq!(five.score, 5);
        assert_eq!(five.tier, Tier::High);
    }

    #[test]
    fn body_length_checklist_and_path_thresholds_each_flip_the_score_at_their_boundary() {
        // Body length at 400/401, with one checklist point underneath: 401 crosses into medium, 400 does not.
        assert_eq!(routed(&signals_with(400, 2, 0, true, None)).score, 1);
        assert_eq!(routed(&signals_with(400, 2, 0, true, None)).tier, Tier::Low);
        assert_eq!(routed(&signals_with(401, 2, 0, true, None)).score, 2);
        assert_eq!(routed(&signals_with(401, 2, 0, true, None)).tier, Tier::Medium);

        // Body length at 2000/2001, with a path and a crossing point alongside: 2001 reaches high, 2000 stays medium.
        assert_eq!(routed(&signals_with(2_000, 2, 2, false, None)).score, 4);
        assert_eq!(routed(&signals_with(2_000, 2, 2, false, None)).tier, Tier::Medium);
        assert_eq!(routed(&signals_with(2_001, 2, 2, false, None)).score, 5);
        assert_eq!(routed(&signals_with(2_001, 2, 2, false, None)).tier, Tier::High);

        // Body length at 6000/6001, one body point away from high on its own.
        assert_eq!(routed(&signals_with(6_000, 2, 2, true, None)).score, 4);
        assert_eq!(routed(&signals_with(6_000, 2, 2, true, None)).tier, Tier::Medium);
        assert_eq!(routed(&signals_with(6_001, 2, 2, true, None)).score, 5);
        assert_eq!(routed(&signals_with(6_001, 2, 2, true, None)).tier, Tier::High);

        // Checklist items at 1/2 and at 5/6.
        assert_eq!(routed(&signals_with(401, 1, 0, true, None)).score, 1);
        assert_eq!(routed(&signals_with(401, 1, 0, true, None)).tier, Tier::Low);
        assert_eq!(routed(&signals_with(401, 2, 0, true, None)).score, 2);
        assert_eq!(routed(&signals_with(401, 2, 0, true, None)).tier, Tier::Medium);
        assert_eq!(routed(&signals_with(6_001, 5, 2, true, None)).score, 5);
        assert_eq!(routed(&signals_with(6_001, 6, 2, true, None)).score, 6);

        // Paths at 1/2 and at 4/5.
        assert_eq!(routed(&signals_with(401, 0, 1, true, None)).score, 1);
        assert_eq!(routed(&signals_with(401, 0, 1, true, None)).tier, Tier::Low);
        assert_eq!(routed(&signals_with(401, 0, 2, true, None)).score, 2);
        assert_eq!(routed(&signals_with(401, 0, 2, true, None)).tier, Tier::Medium);
        assert_eq!(routed(&signals_with(6_001, 0, 4, true, None)).score, 4);
        assert_eq!(routed(&signals_with(6_001, 0, 4, true, None)).tier, Tier::Medium);
        assert_eq!(routed(&signals_with(6_001, 0, 5, true, None)).score, 5);
        assert_eq!(routed(&signals_with(6_001, 0, 5, true, None)).tier, Tier::High);
    }

    #[test]
    fn paths_across_directories_score_a_point_more_than_the_same_paths_in_one_directory() {
        let together = routed(&signals_with(10, 0, 3, true, None));
        assert_eq!(together.score, 1);
        assert_eq!(together.tier, Tier::Low);
        let spread_out = routed(&signals_with(10, 0, 3, false, None));
        assert_eq!(spread_out.score, 2);
        assert_eq!(spread_out.tier, Tier::Medium);
    }

    #[test]
    fn a_high_label_beats_a_low_label_when_the_issue_carries_both() {
        let both = signals("", "tidy up", &["typo".to_string(), "epic".to_string()], true);
        assert_eq!(both.label, Some("epic"));
        let decision = routed(&both);
        assert_eq!(decision.score, 3);
        assert_eq!(decision.tier, Tier::Medium);
        // The same issue carrying only the low label would have gone the other way.
        let low_only = signals("", "tidy up", &["typo".to_string()], true);
        assert_eq!(low_only.label, Some("typo"));
        assert_eq!(routed(&low_only).tier, Tier::Low);
    }

    #[test]
    fn an_unknown_preset_keeps_a_tiny_task_off_the_cheapest_tier_and_leaves_medium_and_high_alone() {
        let tiny = Signals {
            label: Some("typo"),
            body_chars: 12,
            checklist_items: 0,
            paths: 0,
            one_directory: true,
            known_preset: false,
            jev: None,
        };
        assert_eq!(
            routed(&tiny),
            Decision {
                tier: Tier::Medium,
                rule: Tier::Medium,
                source: Source::Rule,
                score: -2,
                reason: "medium tier, score -2: a 12-character body, no checklist items, no paths named, and the typo label, kept off low because the sandbox preset is unknown"
                    .to_string(),
                jev: None,
            }
        );

        let medium = routed(&Signals {
            known_preset: false,
            ..signals_with(401, 2, 0, true, None)
        });
        assert_eq!(medium.score, 2);
        assert_eq!(medium.tier, Tier::Medium);
        assert_eq!(medium.rule, Tier::Medium);

        let high = routed(&Signals {
            known_preset: false,
            ..signals_with(6_001, 0, 2, false, None)
        });
        assert_eq!(high.score, 5);
        assert_eq!(high.tier, Tier::High);
        assert_eq!(high.rule, Tier::High);
    }

    #[test]
    fn routing_turned_off_returns_medium_and_still_reports_what_the_rule_would_have_chosen() {
        let big = Signals {
            label: Some("epic"),
            body_chars: 5_000,
            checklist_items: 6,
            paths: 5,
            one_directory: false,
            known_preset: true,
            jev: None,
        };
        assert_eq!(
            decide(
                &RoutingSettings {
                    enabled: false,
                    chosen: None
                },
                &big
            ),
            Decision {
                tier: Tier::Medium,
                rule: Tier::High,
                source: Source::Off,
                score: 10,
                reason: "per-task routing is off, so the colony runs on the module's model".to_string(),
                jev: None,
            }
        );
    }

    #[test]
    fn an_explicit_tier_is_honoured_even_with_routing_off_and_still_reports_what_the_rule_would_have_chosen() {
        let big = Signals {
            label: Some("epic"),
            body_chars: 5_000,
            checklist_items: 6,
            paths: 5,
            one_directory: false,
            known_preset: true,
            jev: None,
        };
        let decided = decide(
            &RoutingSettings {
                enabled: false,
                chosen: Some(Tier::Low),
            },
            &big,
        );
        assert_eq!(
            decided,
            Decision {
                tier: Tier::Low,
                rule: Tier::High,
                source: Source::Override,
                score: 10,
                reason: "low tier, set for this colony; the rule says high at score 10: a 5000-character body, 6 checklist items, 5 paths named across directories, and the epic label"
                    .to_string(),
                jev: None,
            }
        );
        // The rule is off, but an override against its recorded tier is still a misroute label.
        assert!(decided.misroute());
        // An override that lands where the rule would have gone anyway is not.
        let agreed = decide(
            &RoutingSettings {
                enabled: false,
                chosen: Some(Tier::High),
            },
            &big,
        );
        assert_eq!(agreed.tier, Tier::High);
        assert_eq!(agreed.rule, Tier::High);
        assert!(!agreed.misroute());
    }

    #[test]
    fn a_colony_override_wins_records_the_rule_and_flags_only_real_misroutes() {
        let small = signals_with(180, 0, 1, true, None);
        let agreed = decide(
            &RoutingSettings {
                enabled: true,
                chosen: Some(Tier::Low),
            },
            &small,
        );
        assert_eq!(
            agreed,
            Decision {
                tier: Tier::Low,
                rule: Tier::Low,
                source: Source::Override,
                score: 0,
                reason: "low tier, set for this colony; the rule says low at score 0: a 180-character body, no checklist items, 1 path named"
                    .to_string(),
                jev: None,
            }
        );
        assert!(!agreed.misroute());

        let overruled = decide(
            &RoutingSettings {
                enabled: true,
                chosen: Some(Tier::High),
            },
            &small,
        );
        assert_eq!(
            overruled,
            Decision {
                tier: Tier::High,
                rule: Tier::Low,
                source: Source::Override,
                score: 0,
                reason: "high tier, set for this colony; the rule says low at score 0: a 180-character body, no checklist items, 1 path named"
                    .to_string(),
                jev: None,
            }
        );
        assert!(overruled.misroute());

        // Whatever the rule itself lands on is never a misroute.
        assert!(!routed(&signals_with(6_000, 6, 5, false, Some("epic"))).misroute());
    }

    #[test]
    fn signals_find_paths_in_backticks_and_prose_ignore_urls_mentions_and_duplicates_and_judge_one_directory() {
        let title = "Fix `src/main.rs` and src/lib.rs.";
        let text = "The bug is in `src/lib.rs` (see src/util.rs), not in web/app.ts.\nRefs #123, cc @handle, docs at https://example.com/guide.md.\n";
        let s = signals(title, text, &[], true);
        assert_eq!(s.paths, 4);
        assert!(!s.one_directory);
        assert_eq!(s.checklist_items, 0);

        let same_directory = signals("Fix `src/main.rs`", "and `src/lib.rs` too", &[], true);
        assert_eq!(same_directory.paths, 2);
        assert!(same_directory.one_directory);
    }

    #[test]
    fn slash_paired_prose_and_tokens_with_inner_quotes_contribute_no_paths() {
        let s = signals(
            "",
            "the input/output mapping is read/write support, offered 24/7, and/or on request",
            &[],
            true,
        );
        assert_eq!(s.paths, 0);

        // `authorization`/`x-api-key` trims to a token with the backticks still inside: two pieces of
        // prose glued together, not a directory named `authorization`. The real file next to it counts.
        let quoted = signals(
            "",
            "replace `authorization`/`x-api-key` with the key from `settings.json`",
            &[],
            true,
        );
        assert_eq!(quoted.paths, 1);

        let double_quoted = signals("", r#"the "web/src"/"main" pair came up"#, &[], true);
        assert_eq!(double_quoted.paths, 0);
    }

    #[test]
    fn a_three_segment_directory_reference_still_counts_where_a_two_segment_one_without_an_extension_does_not() {
        let deep = signals("", "move the retry loop under crates/colonizer/src and rebuild", &[], true);
        assert_eq!(deep.paths, 1);
        assert!(deep.one_directory);

        // Accepted trade: `web/src` is indistinguishable from `and/or` without a filesystem, so a
        // two-segment extensionless reference no longer counts.
        let shallow = signals("", "the fix belongs in web/src somewhere", &[], true);
        assert_eq!(shallow.paths, 0);
    }

    #[test]
    fn a_two_segment_path_with_an_extension_and_a_bare_manifest_still_count() {
        let s = signals(
            "",
            "update the flow in docs/protocol.md and the fields in `Cargo.toml`",
            &[],
            true,
        );
        assert_eq!(s.paths, 2);
        assert!(!s.one_directory);
    }

    #[test]
    fn signals_count_both_checkbox_forms_and_ignore_a_checkbox_line_that_is_not_a_list_item() {
        let text = "- [ ] first\n* [x] second\n- [X] third\n* [ ] fourth\n[ ] not a list item\n1. [ ] numbered either\n";
        let s = signals("", text, &[], true);
        assert_eq!(s.checklist_items, 4);
    }

    #[test]
    fn a_tier_without_its_own_model_falls_back_to_the_module_model_and_nothing_set_means_empty() {
        assert_eq!(model_for(Tier::Low, "low-model", "module-model", ""), "low-model");
        assert_eq!(
            model_for(Tier::Medium, "low-model", "module-model", "high-model"),
            "module-model"
        );
        assert_eq!(model_for(Tier::High, "", "module-model", "high-model"), "high-model");
        assert_eq!(model_for(Tier::Low, "", "module-model", "high-model"), "module-model");
        assert_eq!(model_for(Tier::High, "low-model", "module-model", ""), "module-model");
        // A value that is empty once trimmed counts as unset, so low and high fall back to the module
        // model; the module model itself empty means nothing is set, and the result is empty.
        assert_eq!(model_for(Tier::Low, "   ", "module-model", "high-model"), "module-model");
        assert_eq!(model_for(Tier::High, "low-model", "module-model", "  "), "module-model");
        assert_eq!(model_for(Tier::Medium, "low-model", "", "high-model"), "");
        assert_eq!(model_for(Tier::Low, "", "", "high-model"), "");
    }

    #[test]
    fn tier_parse_round_trips_as_str_and_rejects_unknown_names() {
        for tier in [Tier::Low, Tier::Medium, Tier::High] {
            assert_eq!(Tier::parse(tier.as_str()), Some(tier));
        }
        assert_eq!(Tier::parse("  HIGH "), Some(Tier::High));
        assert_eq!(Tier::parse("Medium"), Some(Tier::Medium));
        assert_eq!(Tier::parse("cheap"), None);
        assert_eq!(Tier::parse(""), None);
    }

    fn jev(tier: Tier) -> JevOpinion {
        JevOpinion {
            tier,
            model: "jev-1.13.0".to_string(),
            confidence: 0.9,
            estimated_cost_usd: 0.0001,
        }
    }

    #[test]
    fn a_jev_opinion_on_signals_is_copied_into_the_rule_based_decision_and_never_changes_its_tier() {
        let with_opinion = Signals {
            jev: Some(jev(Tier::High)),
            ..signals_with(180, 0, 1, true, None)
        };
        let decision = routed(&with_opinion);
        // The rule alone would put this small task at low; a disagreeing Jev opinion is recorded but
        // changes nothing about the tier actually chosen.
        assert_eq!(decision.tier, Tier::Low);
        assert_eq!(decision.jev, Some(jev(Tier::High)));
    }

    #[test]
    fn a_jev_opinion_on_signals_is_copied_into_the_off_decision_and_never_changes_its_tier() {
        let with_opinion = Signals {
            jev: Some(jev(Tier::Low)),
            ..signals_with(180, 0, 1, true, None)
        };
        let decision = decide(
            &RoutingSettings {
                enabled: false,
                chosen: None,
            },
            &with_opinion,
        );
        assert_eq!(decision.tier, Tier::Medium);
        assert_eq!(decision.jev, Some(jev(Tier::Low)));
    }

    #[test]
    fn a_jev_opinion_on_signals_is_copied_into_the_override_decision_and_never_changes_its_tier() {
        let with_opinion = Signals {
            jev: Some(jev(Tier::Medium)),
            ..signals_with(180, 0, 1, true, None)
        };
        let decision = decide(
            &RoutingSettings {
                enabled: true,
                chosen: Some(Tier::High),
            },
            &with_opinion,
        );
        assert_eq!(decision.tier, Tier::High);
        assert_eq!(decision.jev, Some(jev(Tier::Medium)));
    }

    #[test]
    fn jev_agrees_compares_the_opinion_against_the_rule_tier_not_the_chosen_one() {
        // No opinion recorded: nothing to compare.
        assert_eq!(routed(&signals_with(180, 0, 1, true, None)).jev_agrees(), None);

        // The rule lands on low for this task; a matching opinion agrees...
        let matching = Signals {
            jev: Some(jev(Tier::Low)),
            ..signals_with(180, 0, 1, true, None)
        };
        assert_eq!(routed(&matching).jev_agrees(), Some(true));

        // ...and a disagreeing one does not, even once an override changes the tier actually chosen.
        let disagreeing = Signals {
            jev: Some(jev(Tier::High)),
            ..signals_with(180, 0, 1, true, None)
        };
        let overridden = decide(
            &RoutingSettings {
                enabled: true,
                chosen: Some(Tier::Medium),
            },
            &disagreeing,
        );
        assert_eq!(overridden.rule, Tier::Low);
        assert_eq!(overridden.jev_agrees(), Some(false));
    }

    #[test]
    fn a_small_context_and_a_big_output_price_gap_make_routing_down_worth_it() {
        // A task that is nearly all output, on a routed model whose output price is far below the
        // direct one: the context reload is a rounding error next to what the output saves.
        let direct = providers::Pricing {
            input_per_mtok: 5.0,
            output_per_mtok: 60.0,
            ..Default::default()
        };
        let routed = providers::Pricing {
            input_per_mtok: 3.0,
            output_per_mtok: 5.0,
            ..Default::default()
        };
        let tokens = RoutingCostTokens {
            context_tokens: 10,
            output_tokens: 1_000,
            reread_tokens: 20,
        };
        let estimate = estimate_cost(direct, routed, tokens);
        assert!(estimate.routed_usd < estimate.direct_usd);
        assert!(estimate.worth_routing());
    }

    #[test]
    fn routing_down_pays_for_the_context_reload_and_the_issues_own_numbers_land_on_not_worth_it() {
        // The issue's worked shape (X=0.65, Y=0.12, Z=0.23) as whole-number tokens — same ratios, so
        // the arithmetic reads directly. The direct model pays for the output plus re-reading what
        // changed; routing down also pays to load the context the direct model already held, and
        // that extra term tips it over. This is the issue's central point: the cheaper model is not
        // always the cheaper run.
        let direct = providers::Pricing {
            input_per_mtok: 5.0,
            output_per_mtok: 25.0,
            ..Default::default()
        };
        let routed = providers::Pricing {
            input_per_mtok: 3.0,
            output_per_mtok: 15.0,
            ..Default::default()
        };
        let tokens = RoutingCostTokens {
            context_tokens: 65,
            output_tokens: 12,
            reread_tokens: 23,
        };
        let estimate = estimate_cost(direct, routed, tokens);
        // Direct: 12 output at 25 + 23 re-read at 5. Routed: 65 context at 3 + 12 output at 15, plus
        // the direct model's 23 re-read at 5 afterwards.
        assert!(
            (estimate.direct_usd - 415.0 / 1_000_000.0).abs() < 1e-12,
            "{}",
            estimate.direct_usd
        );
        assert!(
            (estimate.routed_usd - 490.0 / 1_000_000.0).abs() < 1e-12,
            "{}",
            estimate.routed_usd
        );
        assert!(
            !estimate.worth_routing(),
            "the context reload makes routing down the dearer run"
        );
    }

    #[test]
    fn zero_pricing_estimates_nothing_and_equal_costs_are_never_worth_routing() {
        let tokens = RoutingCostTokens {
            context_tokens: 100,
            output_tokens: 50,
            reread_tokens: 25,
        };
        let estimate = estimate_cost(providers::Pricing::default(), providers::Pricing::default(), tokens);
        assert_eq!(estimate.direct_usd, 0.0);
        assert_eq!(estimate.routed_usd, 0.0);
        assert!(
            !estimate.worth_routing(),
            "a tie keeps the colony on its own model: only strictly cheaper routes"
        );
    }
}
