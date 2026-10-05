//! The exec policy's shape, checked where an operator saves one (issue #924).
//!
//! The policy itself is applied in the agent runners (`modules/agents/*/execpolicy.mjs`), which
//! drop a malformed layer — or a rule they cannot use — with a warning and keep enforcing the rest.
//! That is right in a running colony, but wrong at a save: a rule silently dropped is a rule the
//! operator believes holds. So a policy the mothership stores (an org's `exec_policy`) must be one
//! the runner's `parsePolicy` keeps whole: JSON text of at most 64 KiB, an object with a `rules`
//! array, and every rule an object with a `deny`/`ask`/`allow` decision and only usable
//! predicates, at least one of them. The shared fixture
//! `modules/agents/claude-code/test/fixtures/execpolicy-valid.json` is run against this port and
//! against `parsePolicy` (crates/repo-contracts, execpolicy.test.mjs), so the two cannot drift.
//!
//! Not ported: whether a `command`/`script` pattern compiles as a JavaScript regular expression.
//! The runner owns that grammar; the mothership has no JavaScript regex engine to ask.

use serde_json::Value;

/// The runner's `EXEC_POLICY_MAX_BYTES`, counted the way it counts: JavaScript string length.
const MAX_LENGTH: usize = 64 * 1024;
/// The runner's `REGEX_MAX_CHARS`, likewise in UTF-16 units.
const REGEX_MAX_CHARS: usize = 500;
const DECISIONS: [&str; 3] = ["deny", "ask", "allow"];

/// Checks one exec policy's JSON text, naming the first problem in words an operator can act on.
pub(crate) fn validate(text: &str) -> Result<(), String> {
    if text.encode_utf16().count() > MAX_LENGTH {
        return Err("the exec policy is larger than 64 KiB".into());
    }
    let policy: Value = serde_json::from_str(text).map_err(|e| format!("the exec policy is not valid JSON: {e}"))?;
    let Some(rules) = policy.as_object().and_then(|o| o.get("rules")).and_then(Value::as_array) else {
        return Err(r#"the exec policy must be an object with a "rules" array"#.into());
    };
    for (i, rule) in rules.iter().enumerate() {
        let name = rule
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map_or_else(|| format!("rule {}", i + 1), |id| format!("rule {:?}", id));
        validate_rule(rule).map_err(|problem| format!("exec policy {name}: {problem}"))?;
    }
    Ok(())
}

fn validate_rule(rule: &Value) -> Result<(), String> {
    let Some(rule) = rule.as_object() else {
        return Err("a rule must be an object".into());
    };
    let decision = rule.get("decision").and_then(Value::as_str).map(str::to_lowercase);
    if !decision.is_some_and(|d| DECISIONS.contains(&d.as_str())) {
        return Err(r#""decision" must be "deny", "ask" or "allow""#.into());
    }
    let mut predicates = 0;
    for key in ["command", "script"] {
        if let Some(value) = rule.get(key) {
            patterns(value).map_err(|problem| format!("{key:?} {problem}"))?;
            predicates += 1;
        }
    }
    if let Some(touches) = rule.get("touches") {
        let globs = touches
            .as_array()
            .filter(|list| list.iter().all(|g| g.as_str().is_some_and(|g| !g.trim().is_empty())))
            .ok_or(r#""touches" must be a list of path globs"#)?;
        // A `!` entry only excludes; with nothing included the runner drops the predicate.
        if !globs.iter().filter_map(Value::as_str).any(|g| !g.starts_with('!')) {
            return Err(r#""touches" needs at least one path glob that is not a "!" exclusion"#.into());
        }
        predicates += 1;
    }
    match rule.get("writes_outside") {
        None | Some(Value::Bool(false)) => {}
        Some(Value::Bool(true)) => predicates += 1,
        Some(Value::String(s)) if s == "strict" => predicates += 1,
        Some(_) => return Err(r#""writes_outside" must be true or "strict""#.into()),
    }
    if predicates == 0 {
        return Err(r#"a rule needs "command", "script", "touches" or "writes_outside", or it would match every command"#.into());
    }
    Ok(())
}

/// A `command` or `script` value: one pattern or a non-empty list of them, not all empty.
fn patterns(value: &Value) -> Result<(), String> {
    let list = match value {
        Value::Array(list) => list.iter().collect::<Vec<_>>(),
        one => vec![one],
    };
    let strings = list.iter().map(|p| p.as_str()).collect::<Option<Vec<_>>>();
    let Some(strings) = strings.filter(|s| !s.is_empty()) else {
        return Err("must be a pattern or a list of patterns".into());
    };
    if strings.iter().any(|p| p.encode_utf16().count() > REGEX_MAX_CHARS) {
        return Err(format!("patterns are at most {REGEX_MAX_CHARS} characters"));
    }
    if strings.iter().all(|p| p.is_empty()) {
        return Err(r#"is empty; a deliberate catch-all is ".""#.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_usable_policy_saves() {
        validate(r#"{"rules": []}"#).unwrap();
        validate(r#"{"rules": [{"id": "no-rm", "decision": "DENY", "command": ["\\brm\\b", "shred"]}]}"#).unwrap();
        validate(r#"{"rules": [{"decision": "ask", "writes_outside": "strict"}, {"decision": "allow", "touches": ["docs/**", "!docs/secret"]}]}"#).unwrap();
    }

    #[test]
    fn a_policy_the_runner_would_drop_or_trim_is_refused_with_its_reason() {
        let refused = |text: &str| validate(text).unwrap_err();
        assert!(refused("{not json").starts_with("the exec policy is not valid JSON"));
        assert_eq!(refused("[]"), r#"the exec policy must be an object with a "rules" array"#);
        assert_eq!(
            refused(r#"{"rules": [{"id": "x", "decision": "maybe", "command": "ls"}]}"#),
            r#"exec policy rule "x": "decision" must be "deny", "ask" or "allow""#
        );
        assert_eq!(
            refused(r#"{"rules": [{"decision": "deny"}]}"#),
            r#"exec policy rule 1: a rule needs "command", "script", "touches" or "writes_outside", or it would match every command"#
        );
        assert!(refused(r#"{"rules": [{"decision": "deny", "writes_outside": "loose"}]}"#).contains("writes_outside"));
        assert!(
            refused(&format!(
                r#"{{"rules": [{{"decision": "deny", "command": "{}"}}]}}"#,
                "a".repeat(501)
            ))
            .contains("500")
        );
        assert!(refused(&format!(r#"{{"rules": [], "pad": "{}"}}"#, "a".repeat(MAX_LENGTH))).contains("64 KiB"));
    }
}
