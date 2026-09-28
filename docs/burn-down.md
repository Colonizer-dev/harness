# Burn-down mode

Burn-down spends a weekly token plan before it resets: near the reset, the scheduler
launches bug-hunt colonies — paced across the window, not burst — until the estimated
allowance is down to the reserve the operator set, then stops. It is a `burn_down`
module (Settings → Modules), off until configured, with a card on the Overview and
two API routes.

## Measured spend, estimated allowance

The budget is measured, not granted: what colonies have actually cost since the last reset anchor.
The login only ever proves the subscription's identity, never its limits or reset schedule, so the
allowance is the operator's estimate and the status says so (`estimate: true`). No allowance set
means burn-down launches nothing — it never invents a number.

"Spent" is the recorded cost of every colony created since the last reset — burn-down's own and
everyone else's, in every org and on every agent module: the agent's own estimate plus what the
provider gateway priced on routed providers. A colony created before the reset does not count, even
if it is still spending. A provider without `pricing` adds $0, so a plan billed that way is not seen
here at all.

## Settings

Settings → Modules → Burn down. All times are UTC.

| Setting | Default | Meaning |
| :--- | :--- | :--- |
| `enabled` | off | The module's on/off switch. Never configured reads as off. |
| `reset_weekday` | `Monday` | `Monday` … `Sunday`. Anything else parses to nothing and no window ever opens. |
| `reset_time` | `00:00` | `HH:MM`. Anything else parses to nothing and no window ever opens. |
| `lead_hours` | 48 | How many hours before the reset the window starts, 1 to 168. |
| `reserve_pct` | 5 | Percent of the allowance to keep unspent, 0 to 90. Spending stops at the reserve. |
| `allowance_usd` | none | The operator's estimate of the weekly plan allowance. Unset launches nothing. |
| `spend_usd_per_colony` | 5 | Expected cost of one hunt colony; sizes the plan. The form's minimum is 0.5; a hand-edited zero or negative value holds forever — never a burst. |
| `max_live` | 2 | How many burn-down colonies may be out at once, on top of the pacing, 1 to 8. |
| `repos` | empty | Comma-separated `owner/repo` list to hunt, round-robined. Empty means unconfigured. |
| `instructions` | empty | What hunt colonies run on. Empty falls back to the built-in prompt below. |

Numbers outside those ranges in a hand-edited `modules.json` are clamped when read.

The built-in prompt: find real bugs and fix them, one small pull request per verified
bug — correctness defects, security gaps, crashes, data loss, deadlocks, clear
regressions. Verify each is genuine (reproduce it, confirm with a fresh subagent),
never file style or hypotheticals, keep every pull request small with what is wrong,
how it was verified, and what changed.

## The pace rule

Over the whole window the operator wants `ceil(remaining-above-reserve /
spend-per-colony)` launches. By fraction `f` of the window, `floor(f × needed)`
should have gone out already: a tick that has fallen behind launches another (the
scheduler ticks once a minute, so at most one launch a minute), one on pace or ahead
holds. The decision is a pure function of settings, clock and session list, so UI
and scheduler can never disagree.

Launches go through the ordinary admission path: a burn-down colony queues past the
parallel limit like any other. A launch that is refused (a workspace switched off, a
missing login) is dropped, and the next tick decides again. Every auto-launched colony is tagged
`origin: "burn_down"`, runs on autopilot, and works the whole repository rather than
one issue, so it never waits on another colony's claim.

## States

`disabled`, `unconfigured` (no repos), `unknown_allowance` (no estimate),
`outside_window`, `at_reserve`, `burning`. `GET /api/burn-down` reports the state
plus now, next reset, window start, spent/allowance/remaining/reserve dollars,
live/queued/total colonies, and launches needed vs done.

## Stopping

`POST /api/burn-down/stop` (and the card's Stop button, with a confirm) switches
the module off persistently, then stops every colony it launched — live ones
through the ordinary stop, queued ones out of the queue; one already publishing is
left to finish — and leaves every other colony alone. Idempotent, and safe before burn-down was ever configured.

The Overview card shows the same numbers. It polls `/api/burn-down` every 10 seconds
and renders nothing unless the module is enabled or launched colonies in the current
window, so a mothership that never configured burn-down shows no trace of it.
