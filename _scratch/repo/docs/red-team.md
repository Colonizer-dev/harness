# Red-team raids

A red-team run sends N hunters with distinct briefs to raid one repository at the same time.
Each hunter is an ordinary colony — same validation, same queue, same microVM — with one
assignment and one rule: find and report bugs, don't change the code. When every hunter is
done, the run is done, and its tally says what the swarm found.

## The empty-nest rule

A run launches only while no colony is live — the nest is empty. There are two ways to ask:

- **Start now.** Refused with a 409 while any colony is live, naming the live count
  ("2 colonies are live — a red-team run can only start when the nest is empty"), so
  starting costs nothing when the nest is occupied.
- **Arm.** The run waits (`armed`, with a `gate_reason` saying what it waits for) and a
  background tick launches it the next time the nest empties.

One active run per repository at a time; a second repo can raid in parallel (its run arms
while the first runs, since the gate is global). Stopping a run is idempotent: live hunters
are stopped and their microVMs removed, queued hunters leave the queue, finished hunters
are not touched.

## The swarm

Swarm size is 1 to 8, default 3. Hunters draw their briefs from eight focus areas, cycling
`i % 8`, and each brief names its own focus and lists the other seven, so the swarm keeps
out of one another's way:

1. error handling and edge cases
2. concurrency and race conditions
3. input validation and injection
4. resource leaks and exhaustion
5. auth and permission boundaries
6. core-flow logic errors
7. silent failures and swallowed errors
8. API and contract mismatches

The module a hunter runs on defaults to `general` and can be set per run. Hunters are
titled `Red-team hunter i/N: <focus>` and numbered from 1, matching what the UI shows.

## The brief

With autofix off — the default — the brief is explicit: hunt aggressively, reproduce each
bug before reporting it, report with the findings tool, and **never open, merge or
autofix anything**. Autopilot stays off. With autofix on, the brief instead expects the
fix itself, and autopilot runs.

## Findings and the tally

Each hunter writes `findings.jsonl` in its session directory (protocol §6.6), one line
per stage a finding reaches: `validated` or `rejected` by the orchestrator, then `filed`
or `duplicate` (an open issue already had the title), and with autofix the fix colony,
its review and merge. The run's tally groups a hunter's lines by the finding's title and
counts each finding once: every finding counts as found, and as validated, rejected or
filed (`filed` or `duplicate`) when it reached that stage. Lines written before stages
existed carry no `state` and are read by what they carry: an `issue` or `duplicate_of`
counts as filed.

## States

`armed` → `waiting` / `running` → `draining` → `done`, plus `stopped`:

- `armed`: created to arm, waiting for the gate; `gate_reason` says why.
- `waiting`: hunters launched but all still queued for a parallel slot (hunters
  consume parallel slots like any colony).
- `running`: at least one hunter starting or live.
- `draining`: no hunter live, but some still in flight (publishing, PR open) or queued.
- `done` / `stopped`: terminal; the tick never touches them again.

A crash mid-launch cannot duplicate the swarm: the launch is recorded before the
first hunter exists, a half-launched run drains to `done`, and hunter sessions missing
after a restart count as ended. Runs persist in `data/redteam.json`, load-tolerant the
same way `sessions.json` is.

## Operating it

Overview → the RED TEAM card: repository, swarm size, comma-separated modules,
autofix checkbox, then Start now or Arm. The card lists runs newest first with
live counts, per-run hunters, and a stop button. API: `GET`/`POST`
`/api/redteam/runs`, `GET /api/redteam/runs/{id}`, `POST
/api/redteam/runs/{id}/stop`.
