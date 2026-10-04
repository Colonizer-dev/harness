# Autonomous and burn-down modes

Part of the [Colonizer protocol](../protocol.md).

## 6.2b Autonomous mode (Mothership)

The `autonomy` module decides who answers a colony's questions. `off` (the default) means they wait
for a person. `judge` means a model answers one the person has not.

| Setting | Default | |
| --- | --- | --- |
| `model` | none | `provider/model` goes to that configured provider with the key saved for it, on the `anthropic` or `openai` wire. A plain id (`fable`, `opus`) resolves to a configured provider whose base URL host is Anthropic's API (`api.anthropic.com`) and spends that provider's key; with none configured the judge does not answer — it logs why and the question waits for a person. The Mothership's own Claude login is never spent on judging. A frontier model judges best |
| `after_minutes` | 10 | How long a question waits for a person first; `0` answers as soon as it is seen |
| `max_answers` | 5 | Judged answers per colony, after which it is left for the person |
| `free_text` | false | Whether a question with no options may be answered |
| `risk_ceiling` | `workspace_write` | The highest risk class (§2) the judge may answer. A question above the ceiling is never answered: the judge logs once per question that it has left the question for the person, and waits however long — without spending the colony's judged answers, so a later question within the ceiling is still judged |

Every thirty seconds the Mothership looks for colonies in `waiting_for_answer` whose question has
waited long enough. Suspended colonies ([#562]) are left out — the question they parked on waits
for a person. It sends the model the task, the question with its options, and the last few
colony events as context, and expects
`{"answers": {"<question>": "<label>"}, "reason": "<sentence>"}` back.

The judge chooses **only among the labels the agent offered**, and the reply is checked against them
before anything is sent. A label that was not offered, a question left out, a reply that is not that
JSON, or free text while `free_text` is off: each leaves the question for the person rather than
guessing, and stops this colony being judged again. This is the boundary that keeps a colony's own
output (which can carry repository content, which can carry instructions) from becoming an
instruction to the Mothership.

Risk bounds it further. Every question carries a risk class (§2) and `risk_ceiling` is the highest
class the judge may answer, `workspace_write` by default. A question above the ceiling —
publish-affecting or credential-adjacent, unless the ceiling is raised — degrades to notify-only:
the judge never answers it, logs once per question that it has left the question for the person,
and waits however long it takes — without spending the colony's answer budget, so a later
question within the ceiling is judged as usual.

Not reaching an answer is different from refusing one. Any failure short of a refusal — a provider
that cannot be reached, an HTTP error status (a 401 from a stale key as much as a 5xx), a reply
that is not JSON at all, no provider configured for the id — is retried on a later tick rather
than taken as final, so a brief outage does not silence the judge for the colony; only a refusal
as above, or three consecutive failures, hands the colony back to the person.

An accepted answer travels the ordinary path (§2's `answer` command), so the colony cannot tell it
apart from a person's, except that its `response` says so in words, and the session log records the
model and its reason. A pull request that came out of autonomous mode reads as one afterwards.

## 6.2c Burn-down mode (Mothership)

The `burn_down` module (provider `default`) maxes out the weekly plan: near the weekly reset it
deliberately launches bug-hunt colonies — paced across the window, not a burst — until the estimated
allowance is down to whatever reserve you set, then stops. Every colony it launches carries
`"origin": "burn_down"`, and a global stop kills the scheduler and every colony it launched. Like
`autonomy` and `notify`, it is absent from `modules.json` until first configured.

| Setting | Default | |
| --- | --- | --- |
| `reset_weekday` | `Monday` | The weekday the allowance resets, UTC (an enum). A value no day matches leaves `next_reset` null and the scheduler dormant |
| `reset_time` | `00:00` | The 24-hour UTC time of the reset (`HH:MM`); a value that never parses means the same dormancy |
| `lead_hours` | 48 | How long before the reset the burn window opens |
| `reserve_pct` | 5 | Percent of the allowance left untouched when the reset lands |
| `allowance_usd` | *none* | Your **estimate** of the weekly allowance in USD. The scheduler never launches without it — an invented number would be worse than no number |
| `spend_usd_per_colony` | 5 | What one bug-hunt colony roughly burns; paces the launches |
| `max_live` | 2 | Cap on concurrent live burn-down colonies |
| `repos` | `""` | Comma-separated `owner/repo` list to hunt in. Empty means burn-down is unconfigured and launches nothing |
| `instructions` | `""` | Custom hunt prompt; empty uses a built-in bug-hunt prompt |

Once a minute, inside the window (`reset − lead_hours` to `reset`), the scheduler plans
`ceil((allowance − spent − reserve) / spend_usd_per_colony)` launches in total. By fraction `f` of
the window, `floor(f × needed)` should already be out; a tick that is behind launches another,
round-robin over `repos`, one that is on pace or ahead holds, and `max_live` caps how many run at
once. When spend brings the allowance down to the reserve it stops. Launches go through the ordinary
admission path, so past the parallel limit a burn-down colony queues like any other.

`spent_usd` is **measured, not read from the plan**: `claude_login` exposes only the subscription's
identity, never its usage limits or reset schedules, so the authoritative number is what sessions
have actually cost since the previous reset anchor. `allowance_usd` is your estimate, and
`GET /api/burn-down` says so with `"estimate": true`.

`POST /api/burn-down/stop` persistently switches the module off — it stays off until re-enabled in
Settings — then stops every `origin: "burn_down"` colony: live ones through the ordinary stop
(worktree kept), queued ones out of the queue. It is idempotent, and safe before the module was ever
configured.

`GET /api/burn-down`'s `state` is `disabled`, `unconfigured` (no `repos`), `unknown_allowance`,
`outside_window`, `at_reserve` or `burning`.

Failures are quiet and never spike: unknown `allowance_usd` → `state: "unknown_allowance"` and
nothing launches; an unparseable `reset_time`/`reset_weekday` → `next_reset` null and the window
never opens; a launch that fails is logged and retried on the next tick.

Hunt colonies currently run a generic bug-hunt prompt — find real bugs, verify before filing, keep
pull requests small; they will adopt the red-team runs of
[#212](https://github.com/Colonizer-dev/harness/issues/212) when those land.
