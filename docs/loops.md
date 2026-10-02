# Loops

A loop is a saved prompt on a repository that launches a colony on a schedule — the mothership's
version of Claude Code's `/loop`. Use one for work that recurs: triaging new issues every morning,
keeping dependencies current every week, fixing last night's flaky tests, or writing yesterday's
changelog entries (as `changelog.d/` fragments in a repository that keeps them, like this one).

## Making one

- **Loops** in the sidebar → **New loop**: choose what each run does (a colony from a prompt, or
  refresh the architecture map), pick a repository, write the prompt (or start from a template:
  triage new issues, keep dependencies current, fix last night's flaky tests, write changelog
  entries), choose when, and optionally a model, a subagent model, autopilot (on by default) and a
  run limit.
- **Colonize (⌘K) or the composer**: `/loop 1h check CI on main and fix flakes` creates a loop on
  the repository picked there that runs every hour. The interval takes `m`, `h` or `d`. `/loop 14d …`
  runs every 14 days at the current time of day: a whole-day interval past a week becomes an
  every-N-days cadence (up to 365 days). `/loop <task>` without an interval is self-paced.
- **The CLI**: `colonizer loop create acme/app --name "Triage" --prompt "Triage new issues"
  daily@09:00`, with `loop list`, `run`, `stop`, `start` and `delete` beside it — the grammar is in
  [cli.md](cli.md#loops).
- **The Map view** offers a map-refresh loop once a repository has a map ([Map refresh](#map-refresh)).

An end date (`end_at`) can be set through the API; the cockpit's form keeps one that is already set
but has no field for it.

## When it runs

| Cadence | Runs |
|---|---|
| Every N minutes | N minutes after the previous firing (15 minutes to 7 days) |
| Daily | every day at a time you pick, in your local time |
| Weekly | on a weekday at a time |
| Monthly | on a day of the month at a time; days 29–31 fire on a shorter month's last day |
| Every N days | every N days (1–365) at a time you pick; a run that fires a little late does not push the next one later |
| Self-paced | each run names the next with `loop_next` (15 minutes to 24 hours); without one, 24 hours later |

Times are stored in UTC; the cockpit converts your local choice when you save. The scheduler checks
for due loops once a minute.

**One run at a time.** A tick that finds the loop's previous run still live — queued, starting,
working, idle, waiting for an answer (suspended included), or publishing — skips, and the loop's note says so. A fixed loop tries again
at its next slot; a self-paced one 15 minutes later. **Run now** starts a run immediately, and is
refused (409) while the previous one is live.

## What the colony can do

Every run is an ordinary colony (its origin is `loop:<id>`, and colony lists badge it ↻ loop). It
goes through the same admission path as any launch: parallel limits, budgets and a switched-off
workspace apply. A run that cannot start says why in the loop's note and tries again at the next
slot. The colony gets the loop's prompt plus a note saying which run it is, and two tools only the
orchestrator may call:

- `loop_next(delay_minutes, reason)` — self-paced loops only: when the next run starts, and why.
  The value is clamped to 15 minutes – 24 hours and shown in the loop's note.
- `loop_stop(reason)` — ends the loop ("stopped by the colony: …"). Re-enable it on the Loops page.

The Claude Code, Codex, Grok Build and OpenCode agent modules serve these two tools. A loop whose
colonies run on a module without them — Pi, Hermes and ACP today — still runs on its schedule, but
its brief never mentions the tools, the loop form warns when you pick self-paced, and a self-paced
one simply runs again every 24 hours: its colonies can neither pace the loop nor stop it.

A loop also ends by itself after its **max runs**, when its next run would fall past its **end
date**, or — at its next slot — when its API token has been revoked.

## Tokens

A loop can be created by a scoped API token as well as by the owner ([Scoped API
tokens](cli.md#scoped-api-tokens)): the loop records the token, and every run is admitted against
the token's org/repo limits, concurrency cap and daily budget and marked as external input,
exactly like a colony the token launched by hand. Revoking the token ends the loop the next time
it would run (a run-now answers 409), so nothing launches after revocation. The token lists every
loop inside its limits, but edits and runs only the loops it created — an owner's loop reads as
unknown to it — and map loops stay with the owner.

## Map refresh

A map loop keeps architecture maps fresh instead of running a prompt: each firing launches the same
mapping colony as the Map view — same instructions, same `archify` skillset, reuse of a colony
already drawing — so its runs are the map, exactly as if it had been drawn by hand. Its colonies
carry the origin `map:loop:<id>`. Its prompt is not used and may be left empty. Choose a scope:

- **This repository** — maps the one repository on its cadence.
- **All repositories in the org** (`owner/*`) — maps them one at a time, ten minutes apart, and
  re-lists the org at the start of every cycle, so repositories added later are included. The next
  repository starts even while the previous one is still drawing.

Launches go through the ordinary admission path, so parallel limits, org budgets and the archify
skillset rule the loop in exactly as they rule the Map view. A firing that is refused says so in the
loop's note and tries again at its next slot; org-wide, one repository's failure is noted and the
cycle moves on to the next — and a workspace that is switched off altogether skips its whole cycle
with that single note rather than one per repository. When a refresh ends, History records it as
`map.refresh` — and a repository whose refresh failed keeps its old map.

**Keep this map up to date?** — the Map view asks this once a repository has a map and no enabled
map loop covers it. The answer defaults to every 14 days, with presets 7/14/30/60/90 or a custom
number of days (up to 365), for this repository or all repositories in its org; the loop runs at
03:00 your local time. **Not now** is remembered in that browser for 30 days, and the map bar's
**Keep fresh…** link asks again. Once a refresh loop covers the repository, the map shows
"Refreshed every N days · edit". Map loops can also be made from the Loops page.

## Merge train

The Loops page opens with one built-in loop, **Merge train** (issue #754): the careful way to run
the [merge train](architecture.md#merge-train) — merge colony pull requests, and rebase them when
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

## Cost

Each run is a full colony with its own microVM and model spend. A frequent loop on a large
repository adds up: start daily, and let the loop stop itself when its goal is met. The loop's
history lists every run with its status, pull request and cost.

## API

`GET/POST /api/loops`, `PUT/DELETE /api/loops/{id}`, `POST /api/loops/{id}/run-now`,
`GET /api/loops/{id}/runs` — see [protocol.md](protocol.md#loops). Loops are saved in
`<config_dir>/loops.json`. The merge-train loop is `GET/PUT /api/merge-train/loop` and
`POST /api/merge-train/loop/run[?dry_run=true]`, saved with its history in
`<config_dir>/merge-train-loop.json`.
