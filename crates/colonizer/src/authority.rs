//! Control-plane governance for external effects (issue #98).
//!
//! Every effect that leaves the harness — committing, pushing, opening a PR,
//! filing an issue, any other external call, and resume/recover after a restart —
//! needs its own authorization, bound to the exact candidate (tree sha / content
//! hash) it was granted for. The checks here fail closed: anything missing,
//! expired, mismatched, or not independently reviewed is denied.
//!
//! Candidate-binding semantics: `bind_candidate(parts)` is the lowercase hex of
//! SHA-256 over the concatenation of the parts in order, each part prefixed with
//! its length as 8 little-endian bytes (so `["ab","c"]` and `["a","bc"]` bind
//! differently). An approval names one hash; `authorize` grants exactly that
//! candidate and nothing else.

use ring::digest;

/// One external effect. Each variant needs its own authorization — there is no
/// blanket "may publish" that covers commit, push, and PR creation together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    Execute,
    Commit,
    Push,
    OpenPr,
    FileIssue,
    External,
    Resume,
    Recover,
}

/// The scope an approval is granted for: one colony, one task, one nonce, one
/// expiry, one list of effects, and the candidate hash the approval is bound to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Authority {
    pub colony: String,
    pub task: String,
    pub nonce: String,
    pub expires_unix: u64,
    pub effects: Vec<Effect>,
    pub candidate: Option<String>,
}

/// A concrete grant: the authority plus who built the candidate and who
/// independently reviewed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grant {
    pub authority: Authority,
    pub candidate_hash: String,
    pub reviewer: String,
    pub builder: String,
}

/// Why an authorization was refused. Every fallible path here returns one of
/// these; there is no default-allow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Deny {
    pub reason: &'static str,
}

/// Whether the effect needs a reviewer independent of the builder. Commits,
/// pushes, PRs, issues, and other external calls do; plain execution and the
/// restart paths (resume/recover) re-authenticate instead of re-reviewing.
pub fn needs_independent_review(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::Commit | Effect::Push | Effect::OpenPr | Effect::FileIssue | Effect::External
    )
}

/// Resume after a harness restart always needs fresh authorization: anything
/// preserved across the restart (see `lifecycle::recover`) is fenced until a
/// new grant arrives. Returns `true`, unconditionally, so callers reference —
/// and cannot silently drop — the re-auth requirement.
pub fn requires_reauth_after_restart() -> bool {
    true
}

/// Binds a candidate from its parts (e.g. the pr.md bytes, the tree sha).
/// Deterministic: same parts in the same order always give the same hash.
pub fn bind_candidate(parts: &[&[u8]]) -> String {
    let mut ctx = digest::Context::new(&digest::SHA256);
    for part in parts {
        ctx.update(&(part.len() as u64).to_le_bytes());
        ctx.update(part);
    }
    let hash = ctx.finish();
    let bytes = hash.as_ref();
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from_digit((b >> 4) as u32, 16).unwrap_or('?'));
        out.push(char::from_digit((b & 0xf) as u32, 16).unwrap_or('?'));
    }
    out
}

/// Authorizes one effect against one candidate at one moment, fail-closed.
/// Checks run in this order: missing binding, expiry, effect grant, candidate
/// match, reviewer independence. The first failure denies.
pub fn authorize(grant: &Grant, want_effect: &Effect, want_candidate: &str, now_unix: u64) -> Result<(), Deny> {
    if grant.candidate_hash.is_empty()
        || want_candidate.is_empty()
        || grant.reviewer.is_empty()
        || grant.builder.is_empty()
        || grant.authority.candidate.as_deref().unwrap_or("").is_empty()
    {
        return Err(Deny {
            reason: "missing-binding",
        });
    }
    if now_unix > grant.authority.expires_unix {
        return Err(Deny { reason: "expired" });
    }
    if !grant.authority.effects.contains(want_effect) {
        return Err(Deny {
            reason: "effect-not-granted",
        });
    }
    if grant.candidate_hash != want_candidate || grant.authority.candidate.as_deref() != Some(want_candidate) {
        return Err(Deny {
            reason: "candidate-mismatch",
        });
    }
    if needs_independent_review(want_effect) && grant.reviewer == grant.builder {
        return Err(Deny {
            reason: "reviewer-not-independent",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant() -> Grant {
        let candidate = bind_candidate(&[b"tree-sha", b"pr body"]);
        Grant {
            authority: Authority {
                colony: "abc".into(),
                task: "issue-1".into(),
                nonce: "n1".into(),
                expires_unix: 1_000_000,
                effects: vec![Effect::Commit, Effect::Push, Effect::OpenPr],
                candidate: Some(candidate.clone()),
            },
            candidate_hash: candidate,
            reviewer: "r1".into(),
            builder: "b1".into(),
        }
    }

    /// The denial reason for one authorization attempt: every test below denies.
    fn denied(g: &Grant, effect: &Effect, candidate: &str, now: u64) -> &'static str {
        authorize(g, effect, candidate, now).unwrap_err().reason
    }

    #[test]
    fn a_fresh_grant_for_the_bound_candidate_is_authorized() {
        let g = grant();
        let hash = g.candidate_hash.clone();
        assert!(authorize(&g, &Effect::Commit, &hash, 999_999).is_ok());
        assert!(authorize(&g, &Effect::OpenPr, &hash, 1_000_000).is_ok());
    }

    #[test]
    fn an_expired_grant_is_denied() {
        let g = grant();
        let hash = g.candidate_hash.clone();
        assert_eq!(denied(&g, &Effect::Commit, &hash, 1_000_001), "expired");
    }

    #[test]
    fn an_effect_outside_the_grant_is_denied() {
        let g = grant();
        let hash = g.candidate_hash.clone();
        assert_eq!(denied(&g, &Effect::FileIssue, &hash, 999_999), "effect-not-granted");
    }

    #[test]
    fn a_different_candidate_is_denied() {
        let g = grant();
        assert_eq!(
            denied(&g, &Effect::Push, &bind_candidate(&[b"other"]), 999_999),
            "candidate-mismatch"
        );
    }

    #[test]
    fn a_reviewer_who_is_also_the_builder_is_denied() {
        let mut g = grant();
        g.reviewer = g.builder.clone();
        let hash = g.candidate_hash.clone();
        assert_eq!(denied(&g, &Effect::OpenPr, &hash, 999_999), "reviewer-not-independent");
    }

    #[test]
    fn empty_bindings_are_denied_not_default_allowed() {
        let g = grant();
        let hash = g.candidate_hash.clone();
        let mut no_reviewer = g.clone();
        no_reviewer.reviewer.clear();
        assert_eq!(denied(&no_reviewer, &Effect::Commit, &hash, 0), "missing-binding");
        let mut no_candidate = g.clone();
        no_candidate.authority.candidate = None;
        assert_eq!(denied(&no_candidate, &Effect::Commit, &hash, 0), "missing-binding");
        assert_eq!(denied(&g, &Effect::Commit, "", 0), "missing-binding");
    }

    #[test]
    fn binding_is_deterministic_and_sensitive_to_input_and_order() {
        assert_eq!(bind_candidate(&[b"a", b"b"]), bind_candidate(&[b"a", b"b"]));
        assert_ne!(bind_candidate(&[b"a"]), bind_candidate(&[b"b"]));
        assert_ne!(bind_candidate(&[b"ab", b"c"]), bind_candidate(&[b"a", b"bc"]));
        assert_eq!(bind_candidate(&[b"x"]).len(), 64, "hex sha256");
    }

    #[test]
    fn only_external_effects_need_independent_review() {
        for e in [
            Effect::Commit,
            Effect::Push,
            Effect::OpenPr,
            Effect::FileIssue,
            Effect::External,
        ] {
            assert!(needs_independent_review(&e), "{e:?}");
        }
        for e in [Effect::Execute, Effect::Resume, Effect::Recover] {
            assert!(!needs_independent_review(&e), "{e:?}");
        }
        assert!(requires_reauth_after_restart(), "restarts always re-authorize");
    }
}
