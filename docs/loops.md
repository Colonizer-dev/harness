# Loops

A loop is a saved prompt on a repository that launches a colony on a schedule — the mothership's
version of Claude Code's `/loop`. Use one for work that recurs: triaging new issues every morning,
keeping dependencies current every week, fixing last night's flaky tests, or writing yesterday's
CHANGELOG entries.

## Making one

- **Loops** in the sidebar → **New loop**: pick a repository, write what each run does (or start
  from a template), choose when, and optionally a model, a subagent model, autopilot and a run limit.
- **Composer (⌘K)**: `/loop 1h check CI on main and fix flakes` creates a loop on the composer's
  repository that runs every hour. `/loop 14d …` runs every 14 days at the current time of day — a
  whole-day interval past a week becomes an every-N-days cadence. `/loop <task>` without an
  interval is self-paced.

## When it runs

| Cadence | Runs |
|---|---|
| Every N minutes | N minutes after the previous firing (15 minutes to 7 days) |
| Daily | every day at a time you pick, in your local time |
| Weekly | on a weekday at a time |
| Monthly | on a day of the month at a time; days 29–31 fire on a shorter month's last day |
| Every N days | every N days (1–365) at a time you pick, anchored in UTC so a run that fires late never moves the schedule |
| Self-paced | each run names the next with `loop_next` (15 minutes to 24 hours); without one, 24 hours later |

Times are stored in UTC; the cockpit converts your local choice when you save.

**One run at a time.** A tick that finds the loop's previous run still live — queued, working,
waiting for an answer, or publishing — skips, and the loop's note says so. A fixed loop tries again
at its next slot; a self-paced one 15 minutes later. **Run now** starts a run immediately, and is
refused while the previous one is live.

## What the colony can do

Every run is an ordinary colony (its origin is `loop:<id>`, and colony lists badge it ↻ loop). It
gets the loop's prompt plus a note saying which run it is, and two tools only the orchestrator may
call:

- `loop_next(delay_minutes, reason)` — self-paced loops only: when the next run starts, and why.
  The value is clamped to 15 minutes – 24 hours and shown in the loop's note.
- `loop_stop(reason)` — ends the loop ("stopped by the colony: …"). Re-enable it on the Loops page.

A loop also ends by itself after its **max runs**, or when its next run would fall past its **end
date**.

## Map refresh

A map loop keeps architecture maps fresh instead of running a prompt: each firing launches the same
mapping colony as the Map view — same instructions, same `archify` skillset, reuse of a colony
already drawing — so its runs are the map, exactly as if it had been drawn by hand. Its prompt is
not used and may be left empty. Choose a scope:

- **This repository** — maps the one repository on its cadence.
- **All repositories in the org** (`owner/*`) — maps them one at a time, ten minutes apart, and
  re-lists the org at the start of every cycle, so repositories added later are included.

Launches go through the ordinary admission path, so parallel limits, org budgets and the archify
skillset rule the loop in exactly as they rule the Map view. A firing that is refused says so in the
loop's note and tries again at its next slot; org-wide, one repository's failure is noted and the
cycle moves on to the next — and a workspace that is switched off altogether skips its whole cycle
with that single note rather than one per repository. When a refresh ends, History records it as
`map.refresh` — and a repository whose refresh failed keeps its old map.

**Keep this map up to date?** — the Map view asks once per repository when its first map is drawn.
The answer defaults to every 14 days, with presets 7/14/30/60/90 or a custom number; **Not now** is
remembered in that browser for 30 days. Once a refresh loop exists, the map shows
"Refreshed every N days · edit". Map loops can also be made from the Loops page.

## Cost

Each run is a full colony with its own microVM and model spend. A frequent loop on a large
repository adds up: start daily, and let the loop stop itself when its goal is met. The loop's
history lists every run with its status, pull request and cost.

## API

`GET/POST /api/loops`, `PUT/DELETE /api/loops/{id}`, `POST /api/loops/{id}/run-now`,
`GET /api/loops/{id}/runs` — see [protocol.md](protocol.md#loops). Loops are saved in
`<config_dir>/loops.json`.
