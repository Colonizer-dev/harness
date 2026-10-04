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
   older base.
3. **One at a time.** After a merge, the next candidate is updated onto the new base
   (`update-branch`, once per run) and the run waits up to `ci_wait_minutes` (default 20) for its
   CI and main's before re-checking. At most `max_merges` merges per repository per run (default 4,
   overridable per repository), at least `cooldown_secs` (default 120) between two merges in a
   repository, even across runs. Every GitHub call is paced (`min_call_gap_ms`, default 1 s) and
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
   pull/N/head`, `allow_duplicate`). The redo supersedes the original.
6. **Self-heal main — off by default.** When main goes red and its tip is the train's own merge, the
   repository is **paused** until main is green again. With `self_heal` on, the failed jobs are
   re-run once (a flake), and if main is still red on the next run a small fix colony is sent with
   the failing job's log and instructions to fix main minimally. `revert_on_red` sends a colony that
   reverts the train's own last merge instead — never anything else. A red main whose tip is not the
   train's merge is left to people.
7. **Red pull request CI** is re-run once when every failing check is on `flaky_checks` (a trailing
   `*` matches a prefix); otherwise it is left red and reported.
8. **No attribution.** Merges are squashes titled `<pull request title> (#N)`, pinned to the head
   that was read; a pull request whose commits carry AI attribution is refused.
9. **A report every run** — merged, updated (CI running), red (why), redo dispatched, skipped (why),
   plus what was done about a red main — kept in the loop's history (the last 20 runs), written to
   the activity log (one `publish.merge_train` line per repository) and to each colony's own log.
   The card shows the last report.

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
colonizer loop merge-train run --dry-run           # what it would do, and why
colonizer loop merge-train on                      # switch it on; `off` switches it off
```
