//! What Colonizer was, and who it was, on a commit or a pull request it published (#908).
//!
//! A colonizer commit reads like any other in a repository's history: the title says what changed,
//! the trailer says who to thank. Nothing says a machine wrote it, with which version of the
//! harness, through which settler (the agent module) and on which model. A maintainer triaging a
//! busy repository has to open each pull request to find out, and cannot filter for the automated
//! ones at all.
//!
//! So the publish names itself: `Colonizer-Version`, `Colonizer-Settler` and `Colonizer-Model`
//! trailers on the commit, and the same three facts as a small table in the pull request body.
//! They are ordinary git trailers, which is the point — a repository can search for them, a
//! `git log --grep` finds them, and GitHub's own trailer UI reads them without Colonizer's help.
//! `publish.label_provenance = false` in `colonizer.toml` turns them off for a repository that does
//! not want them (config.rs).
//!
//! Everything here is pure: no git, no GitHub, no I/O. The wiring in `github.rs` decides whether
//! the trailers go out at all and asks this module only what to write.

use crate::sessions::Session;
use serde_json::Value;

/// The trailer names. Fixed strings rather than a loop over a list, because the order is the order
/// a reader scans them in and the model line is the one that may be absent.
const VERSION_TRAILER: &str = "Colonizer-Version";
const SETTLER_TRAILER: &str = "Colonizer-Settler";
const MODEL_TRAILER: &str = "Colonizer-Model";

/// How long a value may be before it is cut. Every value here is short by nature (a version, a
/// module id, a model alias), so the cap is a floor under the damage a hostile one could do, not a
/// limit anyone should reach.
const MAX_VALUE: usize = 120;

/// Marks a value that hit [`MAX_VALUE`], so a reader sees it was cut rather than that it was that
/// long to begin with.
const ELLIPSIS: char = '…';

/// The three facts, as they will be written down.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provenance {
    version: String,
    settler: String,
    model: Option<String>,
}

impl Provenance {
    /// The one place a value enters a [`Provenance`], and the one funnel every caller goes through:
    /// each value is redacted and then sanitised on the way in, so nothing built from a
    /// [`Provenance`] can carry a secret, a line break or a control character into a commit message
    /// or a pull request body. The pull request path redacts the colony's own title and body
    /// (`github.rs`, `read_pr_description`), but this block is appended after that, so a secret
    /// among the three values would otherwise be published verbatim.
    pub fn new(version: String, settler: String, model: Option<String>) -> Provenance {
        Provenance {
            version: sanitize(&version),
            settler: sanitize(&settler),
            model: model.map(|m| sanitize(&m)),
        }
    }

    /// What Colonizer was, and who it was, on a colony: this build of the mothership, the agent
    /// module that ran, and — best effort — the model it ran on.
    ///
    /// The model is a preference order rather than one field, because a colony records it in
    /// different places depending on how it started: what the operator named at launch
    /// ([`Session::model_override`]) is the most direct statement of intent, the routing record's
    /// `model` is what boot actually resolved, and the tier is what it would have chosen. `None`
    /// when none of them says anything usable — a Claude-only colony whose routing record left the
    /// model null — because a guessed name on a published commit is worse than none.
    pub fn from_session(s: &Session) -> Provenance {
        Provenance::new(crate::version::build().line(), s.agent.clone(), model_for(s))
    }

    /// The commit trailers: newline-joined, no trailing newline, so the caller can place them
    /// wherever git expects a paragraph. Any value that came out empty is left out rather than
    /// written blank — an empty trailer reads as a value, and filters would match it. That applies
    /// to all three equally: a launch record with no model, and a session whose `agent` is unset,
    /// are the same shape of "Colonizer has nothing to say here".
    pub fn commit_trailers(&self) -> String {
        self.rows()
            .into_iter()
            .map(|(name, value)| format!("{name}: {value}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The same three facts as Markdown, for a pull request body — where a trailer block would
    /// render as three lines of plain text with no labels. Small and factual on purpose: this is a
    /// stamp for a reviewer skimming, not a report.
    pub fn body_block(&self) -> String {
        let mut out = String::from("## Provenance\n\n| | |\n| --- | --- |\n");
        for (name, value) in self.rows() {
            // Only here, never in a commit trailer: a backslash is literal in a trailer value, but
            // in a table cell `|` would end it and a backtick would start a code span, either of
            // which lets a value break the shape of the table it is printed in.
            out.push_str(&format!("| {name} | {} |\n", cell(value)));
        }
        out
    }

    /// The `(name, value)` pairs that have something to say, in reading order. The single rule for
    /// a value that is not written is here, so the commit and the pull request always agree.
    fn rows(&self) -> Vec<(&'static str, &str)> {
        [
            (VERSION_TRAILER, Some(self.version.as_str())),
            (SETTLER_TRAILER, Some(self.settler.as_str())),
            (MODEL_TRAILER, self.model.as_deref()),
        ]
        .into_iter()
        .filter_map(|(name, value)| value.filter(|v| !v.is_empty()).map(|v| (name, v)))
        .collect()
    }
}

/// One Markdown table cell: `|` and a backtick escaped so the value cannot leave the cell it is
/// written in. Markdown has no other escape for these inside a table.
fn cell(value: &str) -> String {
    value.replace('\\', "\\\\").replace('|', "\\|").replace('`', "\\`")
}

/// The model's name, or `None` when nothing usable is recorded.
///
/// Every step here is `Option`-returning rather than panicking: `model_routing` is a free-form
/// JSON value written by boot, so it can be absent, null, or not an object at all, and a colony
/// that boots on a provider with no model recorded is ordinary, not a failure.
fn model_for(s: &Session) -> Option<String> {
    let usable = |value: Option<&str>| value.map(str::trim).filter(|v| !v.is_empty()).map(String::from);
    usable(s.model_override.as_deref())
        .or_else(|| usable(routed_model(s.model_routing.as_ref())))
        .or_else(|| usable(s.model_tier.as_deref()))
}

/// The `model` key of a routing record, when it is one. Boot writes `Value::Null` there when the
/// routing rule did not change the model, so a null is the normal answer, not an error.
fn routed_model(routing: Option<&Value>) -> Option<&str> {
    routing?.get("model")?.as_str()
}

/// One trailer value or one table cell, made safe to write.
///
/// This is the security-relevant part of the module, and it is here because of where the values
/// come from: the settler is the colony record's `agent`, the model is a launch record, and both
/// are operator- or launch-supplied strings that Colonizer copies rather than chooses. Without
/// this, a settler of `claude-code\nColonizer-Model: fake` publishes a commit whose trailer block
/// says a model Colonizer never ran on — a value a reviewer filters on. So a line break is removed
/// like any other control character, and it leaves a space rather than joining two words together.
/// Whitespace runs collapse for the same reason, and the cap stops a value pushing the rest of the
/// commit message off the end.
///
/// Redaction (`crate::redact`) is the other half of the same job and happens first, here rather
/// than at the call site so that the commit-trailer and pull-request-body paths cannot drift apart.
/// The pull request path redacts the colony's own title and body before `compose_pr_body` appends
/// this block, so a value was the only way a secret could reach a public body: `model_override` is
/// validated on its `provider/` prefix and nothing else, so a launch-scoped caller can put anything
/// after the slash, and `agent` comes from a launch record too. Redaction can only replace a span,
/// never add one, so sanitising after it cannot put a secret back.
fn sanitize(value: &str) -> String {
    let value = crate::redact::redact_text(value);
    let mut out = String::with_capacity(value.len());
    let mut gap = false;
    for ch in value.chars() {
        if ch.is_control() || ch.is_whitespace() {
            // Leading and trailing whitespace is dropped rather than turned into a space, so a
            // padded value does not read as a different one.
            gap = !out.is_empty();
            continue;
        }
        if gap {
            out.push(' ');
            gap = false;
        }
        out.push(ch);
    }
    if out.chars().count() <= MAX_VALUE {
        return out;
    }
    let mut cut: String = out.chars().take(MAX_VALUE - 1).collect();
    cut.push(ELLIPSIS);
    cut
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn a_settler() -> Provenance {
        Provenance::new("v0.2.14 (61ace11)".into(), "claude-code".into(), Some("claude-opus-5".into()))
    }

    #[test]
    fn every_fact_lands_on_the_commit_as_a_trailer() {
        assert_eq!(
            a_settler().commit_trailers(),
            "Colonizer-Version: v0.2.14 (61ace11)\nColonizer-Settler: claude-code\nColonizer-Model: claude-opus-5"
        );
        // The whole block is one paragraph, so git reads all three as trailers of the commit
        // rather than the last line as one and the rest as prose.
        assert!(
            !a_settler().commit_trailers().ends_with('\n'),
            "{:?}",
            a_settler().commit_trailers()
        );
    }

    #[test]
    fn an_unknown_model_leaves_the_line_out_entirely() {
        let p = Provenance::new("v0.2.14".into(), "codex".into(), None);
        let trailers = p.commit_trailers();
        assert!(!trailers.contains(MODEL_TRAILER), "{trailers}");
        assert!(!p.body_block().contains(MODEL_TRAILER), "{}", p.body_block());
        assert!(trailers.contains("Colonizer-Settler: codex"), "{trailers}");
    }

    #[test]
    fn the_body_block_is_a_labelled_table_a_reviewer_can_read() {
        let block = a_settler().body_block();
        assert!(block.starts_with("## Provenance\n\n"), "{block}");
        for row in [
            "Colonizer-Version | v0.2.14 (61ace11)",
            "Colonizer-Settler | claude-code",
            "Colonizer-Model | claude-opus-5",
        ] {
            assert!(block.contains(row), "missing {row}: {block}");
        }
    }

    #[test]
    fn a_value_cannot_forge_another_trailer() {
        // The whole reason `sanitize` exists (issue #908): a settler from a launch record that
        // carries a newline would otherwise write a trailer Colonizer never chose.
        let p = Provenance::new("v0.2.14".into(), "claude-code\nColonizer-Model: fake".into(), None);
        let trailers = p.commit_trailers();
        assert_eq!(trailers.lines().count(), 2, "{trailers}");
        assert!(
            !trailers.lines().any(|l| l.starts_with(MODEL_TRAILER)),
            "a settler forged a model trailer: {trailers}"
        );
        assert!(
            trailers.contains("Colonizer-Settler: claude-code Colonizer-Model: fake"),
            "{trailers}"
        );
        // And in the body block, where the same value would break the table out of its cell.
        let block = p.body_block();
        assert_eq!(block.lines().filter(|l| l.starts_with('|')).count(), 4, "{block}");
    }

    #[test]
    fn control_characters_go_and_runs_of_whitespace_collapse() {
        assert_eq!(sanitize("claude\r\ncode"), "claude code");
        // A NUL is a control character, and it leaves the same single space a newline does rather
        // than fusing the two words — there is no way to tell, from the result, where it was.
        assert_eq!(sanitize("  claude\u{0}-code\t "), "claude -code");
        assert_eq!(sanitize("a\u{7}b"), "a b");
        // A run of them is still one space, like a run of spaces.
        assert_eq!(sanitize("a \r\n\tb"), "a b");
        // Empty and whitespace-only values stay empty rather than becoming a bare trailer value.
        assert_eq!(sanitize("\n\r"), "");
        let p = Provenance::new("v0.2.14".into(), "\n".into(), Some(" ".into()));
        assert_eq!(p.settler, "");
        assert_eq!(p.model.as_deref(), Some(""));
    }

    /// A value that says nothing is left out of both renderings, whichever fact it was. The reason
    /// is the one the model line already had — an empty trailer reads as a value, and filters would
    /// match it — and `Session::default()`'s empty `agent` makes an empty `Colonizer-Settler:`
    /// reachable, so the rule cannot be about the model alone.
    #[test]
    fn an_empty_value_of_any_kind_leaves_its_line_out_entirely() {
        // `Session::default()`'s empty `agent` and a blank `model_override` both land here.
        let p = Provenance::new("v0.2.14".into(), "\n".into(), Some(" ".into()));
        assert_eq!(p.commit_trailers(), "Colonizer-Version: v0.2.14");
        for name in [SETTLER_TRAILER, MODEL_TRAILER] {
            assert!(!p.commit_trailers().contains(name), "{name}: {}", p.commit_trailers());
            assert!(!p.body_block().contains(name), "{name}: {}", p.body_block());
        }

        // The same rule for an empty version, which `crate::version::build()` never produces but
        // which `Provenance::new` accepts.
        let no_version = Provenance::new("".into(), "codex".into(), None);
        assert_eq!(no_version.commit_trailers(), "Colonizer-Settler: codex");
        assert!(
            !no_version.body_block().contains(VERSION_TRAILER),
            "{}",
            no_version.body_block()
        );

        // A value that is there still appears, in both renderings.
        let full = a_settler();
        for name in [VERSION_TRAILER, SETTLER_TRAILER, MODEL_TRAILER] {
            assert!(full.commit_trailers().contains(name), "{name}: {}", full.commit_trailers());
            assert!(full.body_block().contains(name), "{name}: {}", full.body_block());
        }
    }

    /// A secret among the three values must not reach a published commit or pull request body. The
    /// pull request path redacts the colony's own title and body, but this block is appended after
    /// that, so redaction has to happen here to close the gap (issue #908 follow-up).
    #[test]
    fn a_secret_shaped_value_is_redacted_out_of_both_publish_paths() {
        // Split so no single line of this file holds a token-shaped literal; the value is the
        // same one the other modules use, and still redacts as `github_token`.
        let secret = concat!("gh", "p_aB3dE5gH7jK9mN1pQ3sT5vX7zA9cE1gH3jK5");
        for (version, settler, model) in [
            (secret.to_string(), "claude-code".to_string(), None),
            ("v0.2.14".to_string(), secret.to_string(), None),
            ("v0.2.14".to_string(), "claude-code".to_string(), Some(secret.to_string())),
        ] {
            let p = Provenance::new(version, settler, model);
            for published in [p.commit_trailers(), p.body_block()] {
                assert!(
                    !published.contains(secret),
                    "a secret reached a published body: {published:?}"
                );
                assert!(published.contains("[REDACTED:"), "{published:?}");
            }
        }
        // Redaction composes with sanitising rather than replacing it: the surrounding value keeps
        // its shape, and a newline still cannot smuggle in a forged trailer.
        let p = Provenance::new("v0.2.14".into(), format!("claude\ncode {secret}"), None);
        assert_eq!(p.settler, "claude code [REDACTED:github_token]");
        assert_eq!(p.commit_trailers().lines().count(), 2, "{}", p.commit_trailers());
    }

    /// A value cannot break out of the Markdown cell it is printed in: `|` ends a cell and a
    /// backtick opens a code span, either of which would give a hostile settler a row of its own.
    #[test]
    fn a_table_cell_cannot_break_out_of_its_column() {
        let p = Provenance::new("v0.2.14".into(), "a|b|c".into(), Some("`code`".into()));
        let block = p.body_block();
        assert!(block.contains("| Colonizer-Settler | a\\|b\\|c |"), "{block}");
        assert!(block.contains("| Colonizer-Model | \\`code\\` |"), "{block}");
        // Every row is still the header's two columns: the unescaped `|`s are exactly the three
        // that delimit them, whatever the value did.
        for line in block.lines().filter(|l| l.starts_with('|')) {
            let unescaped = line.chars().filter(|c| *c == '|').count() - line.matches("\\|").count();
            assert_eq!(unescaped, 3, "a row left its column: {line:?}");
        }
        // And a commit trailer, where a backslash would be literal, keeps the value as it was.
        let trailers = p.commit_trailers();
        assert!(trailers.contains("Colonizer-Settler: a|b|c"), "{trailers}");
        assert!(trailers.contains("Colonizer-Model: `code`"), "{trailers}");
    }

    #[test]
    fn a_very_long_value_is_cut_and_marked() {
        let long = "m".repeat(400);
        let p = Provenance::new(long.clone(), long.clone(), Some(long));
        assert_eq!(p.settler.chars().count(), MAX_VALUE);
        assert!(p.settler.ends_with(ELLIPSIS), "{}", p.settler);
        assert!(
            p.commit_trailers().lines().all(|l| l.chars().count() <= MAX_VALUE + 20),
            "the trailer name adds to the cap"
        );
        // Exactly at the cap is not truncated: the marker means "there was more".
        assert_eq!(sanitize(&"m".repeat(MAX_VALUE)), "m".repeat(MAX_VALUE));
    }

    #[test]
    fn the_model_is_read_from_the_launch_record_before_the_routing_decision() {
        let mut s = Session {
            agent: "claude-code".into(),
            model_override: Some("claude-opus-5".into()),
            model_tier: Some("balanced".into()),
            ..Default::default()
        };
        s.model_routing = Some(json!({"tier": "high", "model": "claude-sonnet-5"}));
        assert_eq!(model_for(&s).as_deref(), Some("claude-opus-5"));

        // Without the override, the routing record's resolved model — what boot actually used.
        s.model_override = None;
        assert_eq!(model_for(&s).as_deref(), Some("claude-sonnet-5"));

        // Without a routing record either, the tier is what is left to say.
        s.model_routing = None;
        assert_eq!(model_for(&s).as_deref(), Some("balanced"));

        // And nothing recorded at all is no model, not a guess.
        s.model_tier = None;
        assert_eq!(model_for(&s), None);
    }

    #[test]
    fn a_malformed_routing_record_reads_as_no_model_and_nothing_more() {
        for routing in [
            json!(null),
            json!("claude-sonnet-5"),
            json!({"model": null}), // boot writes this when the rule changed nothing
            json!({"model": 42}),
            json!({"model": {"name": "claude-sonnet-5"}}),
            json!([1, 2, 3]),
        ] {
            let s = Session {
                model_routing: Some(routing.clone()),
                ..Default::default()
            };
            assert_eq!(model_for(&s), None, "{routing}");
        }
        // A blank value is no value: it must not reach a published commit as an empty trailer.
        let blank = Session {
            model_override: Some("   ".into()),
            model_tier: Some("".into()),
            ..Default::default()
        };
        assert_eq!(model_for(&blank), None);
    }

    #[test]
    fn a_sessions_provenance_names_this_build_and_the_agent_that_ran_it() {
        let s = Session {
            agent: "codex".into(),
            model_override: Some("gpt-5-codex".into()),
            ..Default::default()
        };
        let p = Provenance::from_session(&s);
        assert_eq!(p.settler, "codex");
        assert_eq!(p.model.as_deref(), Some("gpt-5-codex"));
        assert_eq!(p.version, crate::version::build().line());
        assert!(
            p.commit_trailers().starts_with("Colonizer-Version: "),
            "{}",
            p.commit_trailers()
        );
    }
}
