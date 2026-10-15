# Jev: the external second opinion

Jev is an optional, default-off external classifier the harness may consult at a *decision point* — a
place where it would otherwise apply a fixed rule. The harness asks one `choice` question with a
closed set of options (never free text) and gets back a pick and a confidence; it may then ignore the
pick, record it, or act on it, depending on the point's mode.

Every point runs through one shared layer (`crates/colonizer/src/decide.rs`), so the guarantees below
hold for all of them. Jev is not required to boot: with no `JEV_API_KEY` secret and every point off,
the harness makes zero network calls and every rule behaves exactly as before.

## Modes

- **off** — nothing is asked. The default, and the mode every install starts in.
- **shadow** — the question is asked and the answer recorded, but never applied. This is how a point
  is measured before it is trusted.
- **act** — the answer is applied when it is confident enough. For routing the threshold is
  `jev_routing_act_confidence` (default `0.8`); below it, or with no answer, the rule decides. A
  routing answer is also clamped to the floor `routing::decide` derives (a sensitive task or an
  unknown sandbox preset can raise it, never lower it).

## Hard limits

- **Enumerated options only.** A pick outside the options a point declared is a miss, never acted on.
- **No reason text.** Jev returns a choice and a confidence; it is never asked for prose.
- **Never destructive, security or publish decisions.** A point whose id begins with `publish.`,
  `security.`, `delete.` or `destroy.` is refused before any network call.
- **A budget, then the rule.** An ask past its point's budget is a miss; the caller stays on its rule.
- **Metadata only.** A point's context is built by the harness — titles (credential-looking tokens
  redacted), labels and bucketed counts — never secrets, file contents or code. Jev is US-hosted.
- **Per-org switch.** An org setting `jev: false` turns every point off for that org's colonies, with
  no network call.

## Points

| Point | Settings | Budget |
| --- | --- | --- |
| `routing.tier` | `jev_shadow_mode`, `jev_routing_act`, `jev_routing_act_confidence` | 1800 ms |
| `recovery.path` | `jev_shadow_mode`, `jev_recovery_act`, `jev_recovery_act_confidence`, `jev_recovery_cap` | 1800 ms |
| `verify.focus` | `verify_focus` (`off`/`shadow`/`act`, on the publish module) | 1800 ms |

### `recovery.path`

What to do when a step fails: a provider error, a tool failure, a watchdog stall, or
`autopilot_held`. The closed option set is `retry_same`, `retry_other_provider`, `narrow_task`
(split the work into smaller subagent tasks), `nudge_agent`, `ask_human` and `stop`. `ask_human` and
`stop` are always offered; `retry_other_provider` only where a second provider exists (today neither
call site can switch a running colony cheaply, so it is never offered). Nothing in the set pushes,
publishes or deletes — those are not options.

- The harness's own rule is the fallback and the shadow-mode action: the watchdog's nudge (`Stall`)
  and autopilot's hold (`AutopilotHeld`, or `ProviderError` when the colony is already flagged as a
  provider error).
- In **act** mode a confident pick is carried out: a `retry_same`/`narrow_task`/`nudge_agent` sends
  the agent a short message, while `ask_human` and `stop` leave the colony for you (no nudge or
  message). At the watchdog site `stop` also interrupts the current turn; at the autopilot site the
  turn has already ended, so `stop` only raises the "needs you" flag like `ask_human`.
- **Caps.** At most `jev_recovery_cap` (default 2) machine-chosen recoveries per colony; recovering
  past the cap is `ask_human`. Recoveries that ask a person or stop never count against it. The
  count is per colony, in memory — a mothership restart starts it over.

### `verify.focus`

Which focused check the verifier runs first, before the full suite (#584). The options are the
candidate checks the diff offers — one per check it would run, labelled `dir: command` — plus `full`
for "full suite only". The context is metadata only: how many files the diff touches, the changed
files' extensions and counts, and per candidate how many of the changed files it owns.

- **Shadow only.** The pick is recorded and graded, never applied: the verifier's own rule (the check
  owning the most changed files, first on a tie) keeps deciding the order, and a confirmed verdict
  still needs every check green. Jev's answer can never shorten the suite or change a verdict.
- **Detached.** The ask is spawned after the checks ran, so a verification pays nothing for it; the
  row lands in the ledger while the turn moves on.
- **Grading.** One `decisions.jsonl` row per verification that had two or more candidates, `did`
  always `rule`, and `outcome` filled in the same row:
  `{"would_catch": bool, "actual_first_failure_ms": n, "focused_first_failure_ms": n, "total_ms": n}`
  — whether Jev's pick would have caught the failure the suite actually found, and the
  time-to-first-failure Jev's order would have meant versus the order that ran. A miss leaves the
  outcome null; the rule's own per-verification measurement is in `jev_focus.jsonl`
  (`crates/colonizer/src/verify_focus.rs`).

## The ledger

Every ask at a point that is not off appends one row to `decisions.jsonl` in the data dir:

`{ts, kind:"decision", point, session, repo, issue, mode, options, pick, confidence, latency_ms, miss, did, outcome}`

- `pick` and `confidence` are null on a miss; `miss` names why (`no_key`, `org_off`, `forbidden`,
  `timeout`, `error`, `outside_options`).
- `did` is what the harness did in the end — for routing, `jev` when the tier came from Jev's pick
  and `rule` otherwise; for recovery, `jev`, `rule`, or `cap` when the per-colony cap turned an
  automatic pick into `ask_human`.
- `outcome` grades a decision against what happened. `recovery.path` fills it: ten minutes after each
  decision (shadow and act alike) a second row is appended with the same point, session and `ts`,
  `kind: "outcome"`, and `outcome` = `{"progressed": bool, "window_min": 10}` — whether the colony's
  activity shows progress after the decision. A grade measures only, so it writes no activity line. A
  restart before the window elapses loses that grade. `verify.focus` fills it in the decision row
  itself, as soon as the checks have run (above). The other points leave it null today.

Each ask also writes one activity line (`GET /api/activity`): `decision.shadow` for a shadow ask,
`decision.act` for a used act pick, and `decision.fallback` for an act ask on the rule or any miss.

## Not this

Jev **compaction** (the token-saving pass over a transcript) is a separate feature with its own switch
(`jev_compaction`) and ledger (`jev_ladder.jsonl`); see
[colonies.md, Measuring Jev compaction](colonies.md#measuring-jev-compaction). Jev **brief picks**
(`jev_brief_shadow`, `crates/colonizer/src/brief_pick.rs`) are also separate: a shadow-only
measurement of which shared-memory notes and skill packs a colony would need, logged to
`brief_picks.jsonl`, that does not go through the decision layer yet. A Settings UI showing
each point's agreement rate is a follow-up.
