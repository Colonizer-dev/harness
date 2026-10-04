# 6.7 Red-team runs

Part of the [Colonizer protocol](../protocol.md).

A red-team run raids one repository with up to 8 hunter colonies (default 3). Each hunter gets a
distinct focus — error handling, concurrency, input validation, resource exhaustion, auth
boundaries, core-flow logic, silent failures, API contract mismatches — and an explicit non-overlap
clause listing the other focuses, so the swarm does not duplicate one another's work. Hunters report
what they find with the findings tool (§6.6); with `autofix` unset they are briefed never to open,
merge or autofix anything, and run with autopilot off. They go through the normal `POST /api/sessions`
path, so the parallel limit applies: a hunter may sit `queued` until a slot frees.

`RedTeamRun`:

```jsonc
{"id": "rt_ab12cd34", "repo": "owner/repo", "org": "owner",
 "state": "armed|waiting|running|draining|done|stopped",
 "hunter": "swarm", "model": null, "subagent_model": null, "schedule_id": null,
 "swarm_size": 3, "modules": ["general"], "autofix": false,
 "hunters": [{"session_id": "ab12cd34", "title": "Red-team hunter 1/3: …", "module": "general",
              "version": null, "focus": "error handling and edge cases"}],
 "counts": {"found": 0, "validated": 0, "rejected": 0, "filed": 0, "merged": null},
 "synthesis": null, "preset": "general", "prescan": null,
 "created_at": "…", "started_at": null, "ended_at": null, "gate_reason": null}
```

`preset` is `general` or `security`. A security run records its pre-scan in `prescan` before the
first hunter launches: `{"ran_at", "commit", "secret_scanner": "gitleaks|builtin|", "notes": [..],
"leads": [{"id": "P1", "check", "focus": 0-7, "path", "line", "commit", "message"}], "checklist":
[{"id", "title", "status": "needs_review|not_verifiable", "evidence"}]}` — a checklist item is never
passed ([red-team.md](../red-team.md#the-pre-scan)).

The gate. A run may only *launch* while no colony is live (a ``SessionStatus::is_live()`` state
anywhere, whatever the org). `POST` with `arm` unset/`false` ("start now") launches its hunters
inside the handler and draws a **409** while the gate is closed, e.g. `2 colonies are live — a
red-team run can only start when the nest is empty`. With `arm: true` the run is created `armed` and
the background tick launches it on the next empty-nest pass; while colonies are live it waits with
`gate_reason` set (e.g. `2 colonies are live — waiting for the nest to empty`). Only one run per
repository may be active at a time (**409**).

States. `armed` → `running` when the hunters launch — or `waiting` while every launched hunter is
still queued for a parallel slot — with `waiting` ⇄ `running` as hunters queue and start. When no
hunter is live but some are still in flight (publishing, PR open, queued) the run is `draining`,
and lands `done` once every hunter is `merged`, `closed`, `no_changes`, `stopped` or `failed` — or
its session is gone (a restart's hunt). `counts` are read from the hunters' findings ledgers (§6.6)
and count distinct findings (by title, per hunter) by the stages they reached: `found` every
finding, `validated`/`rejected` the orchestrator's verdicts, `filed` those filed or matched to an
open issue.
`POST /api/redteam/runs/{id}/stop` lands the run `stopped` from any non-terminal state: hunters that
are live or queued are stopped, while one already publishing or with its pull request open settles on
its own — the run is still marked `stopped`. Stops are idempotent once the run is terminal. Runs
persist to `data/redteam.json` and survive a restart, where the tick re-derives their state from the
hunter sessions it finds: hunters whose sessions are gone count as ended, so a run interrupted
mid-launch drains to `done` rather than re-launching a duplicate swarm.

Synthesis. A run with findings that lands `done` launches one more colony — the synthesis judge —
which merges the hunters' findings into one report and publishes nothing (autopilot, autofix and
automerge off; the brief orders it to write only that one file). It fires exactly once, at the
transition into `done` — never mid-run, never on a `stopped` run, never automatically for a run
already done (`POST /api/redteam/runs/{id}/synthesize` re-runs it by hand, §4).
Colonies cannot mount host files, so the brief carries the hunters' ledgers inline (latest ledger
state per finding, plus the body and evidence from the raw `finding` event, each cut to an equal
share of the brief) and cites the host paths. The report is
`sessions/<synthesis id>/out/redteam-report.jsonl`, JSON lines, one object per distinct defect,
most severe first: `{"defect", "severity": "critical|high|medium|low", "reproduction":
"reproduced|unconfirmed", "steps", "files": [..], "hunters": [<hunter session ids>], "merged_from":
n, "validation": "validated|rejected|unvalidated"}`, and on a security run also `"proof"` and
`"prescan_leads": ["P3"]` (the pre-scan leads the defect confirmed) — a defect several hunters reported is one
line naming all of them, and `validation` carries the ledger verdicts (§6.6): `validated` when any
merged finding was validated, `rejected` when all were, else `unvalidated`. `synthesis` tracks the
judge independently of the run's own state, which stays `done`: `null` | `{"state":
"pending|running|done|failed", "session_id": <newest synthesis colony>, "report": <host path of
the newest report, null until one finishes>, "reason": <why it failed>, "superseded": [<earlier
synthesis colony ids, oldest first>]}`; `pending` → `running` → `done` (the report is linked and
`counts.merged` is its parsed line count) or `failed` with `reason` (failed launch; colony failed,
stopped or gone; ended without a report) — any previous report and `merged` stay in place.
