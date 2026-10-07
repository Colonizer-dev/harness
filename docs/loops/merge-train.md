# Merge train

Part of the [Colonizer loops](../loops.md).

The Loops page opens with one built-in loop, **Merge train** (issue #754): the careful way to run
the [merge train](../architecture.md#merge-train) — merge colony pull requests, and rebase them when
needed — on a schedule instead of the train's own two-minute tick. It is **off by default**, runs
**hourly** once switched on (any interval from 15 minutes), and merges **only in repositories you
opt in**: the allowlist (`owner/repo`, or `owner` for a whole org) starts empty. A `never` list —
an upstream-review-only fork, say — beats the allowlist, and so does the train's own
`merge_train_deny_orgs`. While the loop drives a repository, the train's two-minute tick leaves it
alone.

Each run follows the rules an operator would follow by hand:

1. **Keep main green.** Before any merge, the default branch's latest CI on its tip must be
   completed and successful. A run still going means wait; a cancelled run — re-queued or not — is
   a wait too, never red. A main that is not green merges nothing.
2. **Merge only on fresh CI.** A pull request merges only when it is behind its base by 0 and every
   check on that exact head is green. GitHub's CLEAN is not enough: it can reflect CI that ran on an
   older base. The head must also have been quiet — unchanged for the publish module's
   `merge_train_quiet_minutes` (default 10) — and the merge is pinned to it, so a commit pushed after
   the first green run is never squashed away; a branch that still gets commits after the merged head
   is kept and raised as "commits not merged" ([merge train](../architecture.md#merge-train), issue
   #1075). A remaining quiet wait no longer than `ci_wait_minutes` is sat out once in the run.
3. **One at a time.** After a merge, every candidate still waiting in the run that shares a file
   with the one merged is brought onto the new base at once (`update-branch`, or the host's
   mechanical rebase — or `needs_redo` — when it now conflicts), so a file the train just touched
   does not leave the candidates behind it stale for their turn — and a candidate updated after one
   merge is updated again after the next. This stops at the merge cap: a merge that reaches
   `max_merges` ends the run, so nothing more is fanned out. The run waits up to `ci_wait_minutes`
   (default 20) for the head candidate's CI and main's before re-checking. At most `max_merges`
   merges per repository per run (default 4, overridable per repository), at least `cooldown_secs`
   (default 120) between two merges in a repository, even across runs. Every GitHub call is paced
   (`min_call_gap_ms`, default 1 s) and
   budgeted (`max_api_calls`, default 400 per run); any 403 or 429, abuse-detection or
   secondary-rate-limit answer **stops the run** there and then, and nothing is retried — the rest
   waits for the next run.
4. **Eligibility.** Only colony pull requests (`colonizer/*` branches the mothership published);
   never drafts, never a HOLD, WIP or do-not-merge label or title, never a colony held with
   `loop merge-train hold`, never a colony superseded by its redo or by a newer pull request on the
   same issue; and the train's own author and attribution guards apply.
5. **Rebase only when mechanical.** A conflicted (DIRTY) pull request gets the host's mechanical
   rebase, and then needs fresh CI before it can merge. If that rebase conflicts, nothing is guessed:
   the pull request is marked `needs_redo`, and — only with `redo_on_conflict` on — one redo colony
   is dispatched for it, ever, with the pull request as its reference (`git fetch origin
   pull/N/head`, `allow_duplicate`). The redo supersedes the original. With `resolve_conflicts`
   on, a conflicted pull request is [resolved by its own colony](#resolving-conflicts-with-a-colony)
   instead — a merge, never a rebase.
6. **Self-heal main — off by default.** When main goes red and its tip is the train's own merge, the
   repository is **paused** until main is green again. With `self_heal` on, the failed jobs are
   re-run once (a flake), and if main is still red on the next run a small fix colony is sent with
   the failing job's log and instructions to fix main minimally. `revert_on_red` sends a colony that
   reverts the train's own last merge instead — never anything else. A red main whose tip is not the
   train's merge is left to people.
7. **Red pull request CI** is re-run once when every failing check is on `flaky_checks` (a trailing
   `*` matches a prefix); otherwise it is left red and reported.
8. **No attribution.** Merges are squashes titled `<pull request title> (#N)`, pinned to the head
   that was read (the merge API's `sha`; a push during the merge makes GitHub refuse it, and the pull
   request waits for checks on its new head); a pull request whose commits carry AI attribution is
   refused.
9. **A report every run** — merged (with the head it merged), updated (CI running), red (why), redo
   dispatched, skipped (why), any "commits not merged", plus what was done about a red main — kept in the loop's history (the last 20 runs), written to
   the activity log (one `publish.merge_train` line per repository) and to each colony's own log.
   The card shows the last report.

### Resolving conflicts with a colony

With dozens of colonies on one repository, most pull requests go DIRTY within the hour. With
`resolve_conflicts` on (`colonizer loop merge-train set --resolve on`; off by default), the loop
does not rebase a conflicted pull request (issue #968). It merges the base into the colony's own
kept worktree on the host — `git merge origin/<base>`, a merge commit; nothing is rewritten and
nothing is ever force-pushed:

- **A clean merge** is pushed to the branch as it is (a plain push: anything but a fast-forward is
  refused) and the pull request merges after fresh CI.
- **Conflicts** resume the pull request's colony on that worktree with a one-shot brief: resolve
  every conflict keeping both sides' intent, rerun the generators the repository documents instead
  of hand-merging their output (route snapshots, lockfiles, compatibility or error docs, translation
  catalogs), run the checks, and rewrite `pr.md`. Its publish commits the merge and pushes it to the
  same pull request; the report shows **resolving conflicts** meanwhile. A conflict that needs a
  product or security decision is asked with choices, the normal question flow, and the pull
  request is labelled `needs-human` while it waits.
- **One at a time**: one resolve per pull request (it is the colony's own), one per repository at
  once, one attempt per base commit, and at most `resolve_attempts` (default 3) per pull request.
  A resolve that stops or fails without publishing has its merge aborted and its pull request put
  back in the train. Past the limit — or when the colony's worktree is gone — the pull request is
  labelled `needs-human` and left open with the reason.

The repository can steer it from its base branch's `.colonizer/merge.toml`:

```toml
[resolve]
never = ["migrations/**", "SECURITY.md"]          # a conflict here goes to a person, never a colony

[[resolve.generators]]
files = "crates/colonizer/routes/*.snap"         # when this conflicts, rerun its generator
run = "UPDATE_ROUTE_SNAPSHOT=1 cargo test -p colonizer-harness route_table"
```

A resolved colony keeps autopilot on afterwards, so its later fixes publish to the same pull request.

### When GitHub CI cannot run

When CI cannot run at all — the org's Actions billing failed or its spending limit was reached, no
runner picked the jobs up, or Actions is switched off — nothing would ever merge, so the loop can
run the repository's checks itself (issue #969). It tells **could not run** from **ran and failed**
by GitHub's own words on the refused jobs ("The job was not started because…", "…not acquired by
Runner…"), or, for a pull request with no checks at all, by Actions or every workflow being
disabled. One check that ran and failed, or one still running, and the pull request is not
"unavailable": a real CI failure is never merged this way.

It is **opt-in per repository**, read from the base branch, never from the pull request:

- `.colonizer/merge.toml` with `local_checks = ["npm ci", "npx tsc --noEmit", "npx vitest run"]`
  turns it on with those commands; `local_checks = []` keeps it off whatever the loop says.
- The loop's `local_checks` list (`owner` or `owner/repo`;
  `colonizer loop merge-train set --local-checks acme,acme/web`) turns it on with commands detected
  from the stack: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`
  and `cargo test --workspace` for a root `Cargo.toml`; for a root `package.json`, the install and
  test command the [verifier](../colonies.md#verifying-done) would run, then its `typecheck`, `lint` and `build`
  scripts; `make test` otherwise.

For a pull request that would merge but for its refused CI, the loop merges the head with the
current base on the host (`git merge-tree`: objects only, nothing runs), runs the commands one after
another in a one-shot microVM from the colony image, and posts the result as the
`colonizer/local-checks` commit status on the head — `pending`, then `success`, `failure` (naming the
command) or `error`. It merges only when every command passed **and** the base has not moved since,
pinned to the head that was checked; a failure is not run again until the head or the base moves. A
main whose CI could not run is not red — no re-run, no fix colony — and without local checks it
holds the repository with the reason. Branch protection still applies: if the refused jobs are
required checks, GitHub refuses the merge and the report says so.

When a repository flips into this mode — the first run that sees refused CI there — the owner is
notified once through the notify module's channels (desktop, webhook, Web Push), with GitHub's
reason, and once more when main's CI runs green again (issue #972). The mode is remembered in
`merge-train-loop.json` (`ci_unavailable`), so a restart does not announce it again.

**Dry run** reads everything and writes nothing: it lists what the loop would merge, update, rebase
and skip, and why. With `COLONIZER_NO_EXTERNAL_EFFECTS` set, every run — scheduled or not — is a
dry run, and its report says so.

From a terminal:

```sh
colonizer loop merge-train show                    # settings, next run, paused repositories, last report
colonizer loop merge-train allow acme/web          # opt a repository in (or `allow acme` for the org)
colonizer loop merge-train never acme/upstream     # never merge here
colonizer loop merge-train set --every 120 --max-merges 2 --repo-cap acme/web=1 --flaky 'e2e*,lint'
colonizer loop merge-train set --self-heal on --redo on
colonizer loop merge-train set --local-checks acme # run acme's checks locally when GitHub CI cannot run
colonizer loop merge-train set --resolve on --resolve-attempts 3   # resolve DIRTY pull requests with their colony
colonizer loop merge-train run --dry-run           # what it would do, and why
colonizer loop merge-train on                      # switch it on; `off` switches it off
```
