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
//!
//! Grants are values, minted at a real approval event and passed down the call
//! chain to the effect site; they are never persisted, so nothing approved before
//! a harness restart survives it (`requires_reauth_after_restart`). Wired today:
//! the publish path mints at the operator's Create PR press and at autopilot's
//! confirmed verdict and checks `Commit`/`Push`/`OpenPr` at their effect sites in
//! `github::run_publish_with`; finding filing mints `FileIssue` after the
//! independent validation and checks it in `findings::file` before any `gh` call;
//! the resume paths mint `Resume` per boot and check it in `boot`, and
//! `lifecycle::recover` denies `Recover` against the grant a restart necessarily
//! destroyed. Every path also consults the `external_writes_blocked` kill-switch,
//! which fails closed when it is set.

use ring::digest;

/// One external effect. Each variant needs its own authorization — there is no
/// blanket "may publish" that covers commit, push, and PR creation together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Agent command execution. Checked at the execution sink, not wired yet.
    #[allow(dead_code)]
    Execute,
    Commit,
    Push,
    OpenPr,
    FileIssue,
    /// Any other external call. Checked at its sink, not wired yet.
    #[allow(dead_code)]
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

/// How long a freshly minted grant stays valid: fifteen minutes. An approval is
/// meant to cover the work it reviewed while the trail is hot — a publish, a
/// filing, a resume — not to outlive it; anything still unspent after that needs
/// approving again, and the expiry check (`authorize`) denies the rest.
pub const GRANT_TTL_SECS: u64 = 15 * 60;

/// The harness's one wall clock, in seconds since the epoch. `authorize` compares
/// `now_unix` against a grant's `expires_unix`; centralised so every check and
/// every expiry test read the same shape of time.
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Grant {
    /// Mints a grant at a real approval event: a fresh nonce, an expiry `ttl_secs`
    /// seconds out, and the candidate hash the approval reviewed, carried twice —
    /// in the authority (what was approved) and as `candidate_hash` (what
    /// `authorize` compares against). `reviewer` and `builder` must name different
    /// parties for anything `needs_independent_review` covers; both must be
    /// non-empty either way.
    pub fn mint(
        colony: &str,
        task: &str,
        effects: Vec<Effect>,
        candidate: String,
        reviewer: &str,
        builder: &str,
        ttl_secs: u64,
    ) -> Grant {
        Grant {
            authority: Authority {
                colony: colony.to_string(),
                task: task.to_string(),
                nonce: crate::util::random_token(),
                expires_unix: now_unix() + ttl_secs,
                effects,
                candidate: Some(candidate.clone()),
            },
            candidate_hash: candidate,
            reviewer: reviewer.to_string(),
            builder: builder.to_string(),
        }
    }
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

/// The operator's kill-switch for every write that leaves the harness (issue #84): commits, pushes,
/// pull requests, PR merges and comments, and filed issues. Set `COLONIZER_NO_EXTERNAL_EFFECTS` (or
/// `COLONIZER_NO_WRITE`) to anything but `0`/`false`/`off`/`no` and each of those refuses to run.
pub fn external_writes_blocked() -> bool {
    // Tests never read the real environment, so a developer's own kill-switch cannot fail the suite;
    // a test blocks writes for its own thread with `test_block_external_writes`.
    #[cfg(test)]
    return TEST_BLOCKED.with(std::cell::Cell::get);
    #[cfg(not(test))]
    blocked_by_env(&["COLONIZER_NO_EXTERNAL_EFFECTS", "COLONIZER_NO_WRITE"])
}

/// Whether any of `keys` is set to a value that switches writes off: anything non-empty except the
/// usual spellings of "off".
fn blocked_by_env(keys: &[&str]) -> bool {
    keys.iter().any(|key| {
        crate::util::env_nonempty(key)
            .is_some_and(|v| !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "off" | "no"))
    })
}

#[cfg(test)]
thread_local! {
    static TEST_BLOCKED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Blocks external writes for the calling test's thread until the guard drops. Thread-local on
/// purpose: tests run in parallel, and a process-wide env var would leak into every other test.
#[cfg(test)]
pub(crate) fn test_block_external_writes() -> impl Drop {
    struct Unblock;
    impl Drop for Unblock {
        fn drop(&mut self) {
            TEST_BLOCKED.with(|b| b.set(false));
        }
    }
    TEST_BLOCKED.with(|b| b.set(true));
    Unblock
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

/// [`authorize`] for the call sites that carry the grant down an async chain as an
/// `Option`: `None` — no mint, or a grant a restart destroyed — is the first and
/// hardest denial, not a silent pass.
pub fn authorize_opt(grant: Option<&Grant>, want_effect: &Effect, want_candidate: &str, now_unix: u64) -> Result<(), Deny> {
    let grant = grant.ok_or(Deny { reason: "missing-grant" })?;
    authorize(grant, want_effect, want_candidate, now_unix)
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

    #[test]
    fn a_minted_grant_authorizes_its_own_candidate_and_nothing_else() {
        let candidate = bind_candidate(&[b"tree", b"body"]);
        let g = Grant::mint(
            "colony",
            "issue-1",
            vec![Effect::Commit, Effect::Push, Effect::OpenPr],
            candidate.clone(),
            "reviewer",
            "builder",
            GRANT_TTL_SECS,
        );
        assert!(!g.authority.nonce.is_empty(), "every grant carries a fresh nonce");
        assert_eq!(g.authority.candidate.as_deref(), Some(candidate.as_str()));
        assert!(authorize(&g, &Effect::Commit, &candidate, now_unix()).is_ok());
        assert_eq!(
            authorize(&g, &Effect::Commit, &bind_candidate(&[b"other"]), now_unix())
                .unwrap_err()
                .reason,
            "candidate-mismatch"
        );
    }

    #[test]
    fn a_missing_grant_is_denied_as_missing_not_passed_over() {
        assert_eq!(
            authorize_opt(None, &Effect::Commit, &bind_candidate(&[b"tree"]), now_unix())
                .unwrap_err()
                .reason,
            "missing-grant"
        );
        let g = grant();
        let hash = g.candidate_hash.clone();
        assert!(authorize_opt(Some(&g), &Effect::Push, &hash, 999_999).is_ok());
    }

    #[test]
    fn the_write_kill_switch_reads_any_non_off_value_as_blocked() {
        // Keys only this test reads, so setting them cannot leak into a parallel test.
        const A: &str = "COLONIZER_TEST_84_NO_EXTERNAL_EFFECTS";
        const B: &str = "COLONIZER_TEST_84_NO_WRITE";
        let set = |k: &str, v: Option<&str>| match v {
            // SAFETY: no other test touches these keys.
            Some(v) => unsafe { std::env::set_var(k, v) },
            None => unsafe { std::env::remove_var(k) },
        };
        set(A, None);
        set(B, None);
        assert!(!blocked_by_env(&[A, B]), "unset means writes are allowed");
        for off in ["", "  ", "0", "false", "OFF", "No"] {
            set(A, Some(off));
            assert!(!blocked_by_env(&[A, B]), "{off:?} does not block");
        }
        for on in ["1", "true", "yes", "anything"] {
            set(A, Some(on));
            assert!(blocked_by_env(&[A, B]), "{on:?} blocks");
        }
        set(A, None);
        set(B, Some("1"));
        assert!(blocked_by_env(&[A, B]), "either key blocks on its own");
        set(B, None);
    }

    #[test]
    fn a_test_can_block_external_writes_for_its_own_thread_only() {
        assert!(!external_writes_blocked(), "the test build ignores the real environment");
        {
            let _blocked = test_block_external_writes();
            assert!(external_writes_blocked());
            std::thread::spawn(|| assert!(!external_writes_blocked(), "other threads are unaffected"))
                .join()
                .unwrap();
        }
        assert!(!external_writes_blocked(), "the guard unblocks on drop");
    }
}
