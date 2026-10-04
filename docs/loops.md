# Loops

A loop is a saved prompt on a repository that launches a colony on a schedule — the mothership's
version of Claude Code's `/loop`. One loop is built in: [Disk cleanup](loops/disk-cleanup.md), off until you
switch it on. Use one for work that recurs: triaging new issues every morning,
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
- **The Map view** offers a map-refresh loop once a repository has a map ([Map refresh](loops/map-refresh.md)).

An optional **Ends** date and time in the loop form (your local time, stored as `end_at`) stops the
loop when its next run would fall past it; a value that is not after now is refused in the form. The
Loops list shows the day a dated loop ends.

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

**A run that fails for an infrastructure reason is re-run once** (issue #881): a run that ends failed
with a transient class — a runtime or image hiccup, a timeout, a dropped connection, an HTTP 5xx — is
launched again within `retry_failed_runs` minutes of the run's start (60 by default, `0` to switch the
re-run off). The re-run does not count as a run and the next slot does not move, so the schedule is
untouched; the Loops page and `loop list` show the last run's outcome, with the failure class when it
failed.

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

**Loops that need GitHub.** Triaging issues, fixing CI flakes and writing changelog entries all read
GitHub, and a colony has no GitHub token. Turn on **Needs GitHub** on a colony loop (the templates
that need it set it for you) and two things change. First, the loop launches nothing until the
mothership can read its repository; if it cannot — no token, no access — no colony boots, and the
loop's note says so and how to fix it. Second, the run gets the inputs it cannot fetch itself, under
the read-only `/colonizer/github`: `issues.json` (open issues touched since the last run),
`ci-failures.json` (failed runs on the default branch) and `merged-prs.json` (pull requests merged
since the last run), each with the `since` timestamp it was narrowed to, plus tools the
orchestrator may call — `issue_label`, `issue_comment`, `issue_close_duplicate`, `pr_comment`,
`pr_label` and `pr_merge` — which the mothership makes on the loop's own repository (`pr_merge`
only once merges are switched on for it, see [protocol/loops.md](protocol/loops.md#612-github-loops-issue-778-pr-actions-issue-807)).
The loop's brief names all of it. A loop without the
switch behaves exactly as before.

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

## Built-in loops

Each built-in loop is documented on its own page:

Generated by `node scripts/doc-index.mjs write`: add a file under docs/loops/ and rerun; don't edit by hand.
<!-- doc-index:start -->
- [Dependencies & supply chain](loops/dependencies.md)
- [Disk cleanup](loops/disk-cleanup.md)
- [Docs & README](loops/docs-readme.md)
- [Map refresh](loops/map-refresh.md)
- [Merge train](loops/merge-train.md)
- [TypeScript: remove any](loops/typescript-remove-any.md)
<!-- doc-index:end -->

Links to the old per-loop anchors still resolve on this page:
<a id="map-refresh"></a><a id="merge-train"></a><a id="disk-cleanup"></a><a id="docs--readme"></a><a id="dependencies--supply-chain"></a><a id="typescript-remove-any"></a>

## Cost

Each run is a full colony with its own microVM and model spend. A frequent loop on a large
repository adds up: start daily, and let the loop stop itself when its goal is met. The loop's
history lists every run with its status, pull request and cost.

## API

`GET/POST /api/loops`, `PUT/DELETE /api/loops/{id}`, `POST /api/loops/{id}/run-now`,
`GET /api/loops/{id}/runs` — see [protocol.md](protocol/loops.md). Loops are saved in
`<config_dir>/loops.json`; the built-in disk cleanup in `<config_dir>/disk-cleanup.json`. The
merge-train loop is `GET/PUT /api/merge-train/loop` and
`POST /api/merge-train/loop/run[?dry_run=true]`, saved with its history in
`<config_dir>/merge-train-loop.json`. The supply-chain loop has its own routes:
`GET/PUT /api/supply-chain-loop` and `POST /api/supply-chain-loop/run`.
