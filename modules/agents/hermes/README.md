# Hermes agent module

Runs [Nous Research's Hermes Agent CLI](https://github.com/NousResearch/hermes-agent) and speaks the
Colonizer runner contract (`docs/protocol.md` §2): commands as JSON lines on stdin, events as JSON
lines on stdout, diagnostics on stderr.

Each `user_message` becomes one headless turn: `hermes chat -q <text> --format stream-json
--provider colonizer-<id> -m <model>` in the workspace, with `--resume <session_id>` from the second
turn on (the id is persisted to `$HERMES_HOME/colonizer-session-id`, so a restarted runner resumes).
Text deltas stream as `assistant_text_delta`; `tool_use`/`tool_result` pair FIFO by tool name, since
Hermes names the tool rather than the call; the final `result` becomes `assistant_text` plus
`turn_end` with `model_usage` from Hermes' token counts and `cost_usd: null` — the provider gateway
prices and budgets every routed request, so the runner never reports cost. Non-JSON lines on Hermes'
stdout (the tirith scanner prints some) become `warn` logs; tirith itself stays on. Messages arriving
mid-turn queue and run in order.

Headless drivability was verified live against real Hermes v0.21.5 (tag `v2026.9.24`) through a fake
Anthropic-wire gateway: a full tool round-trip, `--resume` across turns and across a runner restart,
interrupt via SIGTERM with no orphan processes, the colony header present on every request, and the
memory, skills, delegation, cronjob, clarify and tts toolsets absent from the tools Hermes offered.
Two things to expect against the real provider gateway: Hermes probes about ten model-catalogue
endpoints per turn (for example `/api/v1/models`, `/v1/props`) and tolerates 404s there, and tirith
prints one non-JSON banner line per turn, which this runner surfaces as a `warn` log.

## Configuration

| Variable | Default | Meaning |
| :--- | :--- | :--- |
| `COLONIZER_HERMES_BIN` | `hermes` | The hermes command (split on spaces) |
| `COLONIZER_HERMES_HOME` | `/tmp/colonizer-hermes` | `HERMES_HOME`; the runner writes `config.yaml` (as JSON, valid YAML) and the session-id file here. The config carries a `model:` block naming the resolved provider and default, rewritten before every turn so it always matches the CLI flags — without it Hermes' first-run guard sees "no API keys or providers found" (it ignores the top-level `providers:` map) and exits |
| `COLONIZER_MODEL` | none — required | `<provider>/<model>`, which must match a gateway route; anything else refuses the turn |
| `COLONIZER_MODEL_ROUTES` | none | JSON provider routes from the mothership (`docs/protocol.md` §6.1); one becomes a Hermes provider `colonizer-<id>` on the Anthropic Messages wire with the colony header |
| `COLONIZER_HERMES_TURN_TIMEOUT_SECS` | 3600 | Per-turn cap; exceeding it SIGTERMs Hermes and ends the turn with an error |

Models go only through the mothership's provider gateway, which holds provider keys host-side and
does pricing and budget accounting; the colony VM needs no provider secret and no provider egress.
The gateway expects the bare model (the claude-code router strips the same `<id>/` prefix), so `-m`
gets the remainder after the route prefix. Nous Portal models (`nous/…`) are refused: Portal credits
are a prepaid pool the gateway cannot account for ([#199](https://github.com/Colonizer-dev/harness/issues/199)).

## Recorded decisions

- **Sandbox.** `terminal.backend` is pinned to `local` in the config and `TERMINAL_ENV=local` in the
  child env; the startup probe refuses to run when the inherited `TERMINAL_ENV` names any other
  backend (docker, ssh, singularity, modal, daytona, vercel_sandbox), because Hermes silently falls
  back to local when one is unusable — there is no Docker-in-microVM here. SIGINT does not cancel a
  Hermes turn, so `interrupt` and the turn timeout SIGTERM the process; the runner needs its own
  timeout because `--run-budget` does not cap a hung call.
- **Memory and skills.** Hermes' memory (both flags), the skills toolset (with `write_approval` on)
  and background review are off, so Colonizer's shared memory and skillsets stay the system of
  record. The `clarify`, `tts`, `cronjob` and `delegation` toolsets are disabled too — delegation
  until the subagent-inheritance contract covers it.
- **Scheduling and messaging.** `hermes gateway` (cron, messaging) is never started by this runner;
  the cronjob toolset is off.
- **Questions.** None: `clarify` is a toolset for interactive questions and headless `-q` mode has no
  channel to answer one, so an `answer` command gets a `warn` log. A future ACP driver (`hermes acp`)
  is the gap-filler.
- **Credentials.** Only gateway routes, above: no provider secret ever enters the colony.

## Gaps

- Nothing stages the `hermes` binary into the colony VM yet; the runner's preflight fails loudly
  (`status error`, non-zero exit, the pinned install command in the message) when it is missing, so
  a colony that picks this module stops there.
- No ACP question channel, so the module cannot ask you anything.
- No end-to-end colony run: the live verification above used a fake Anthropic-wire gateway and no
  microVM, so the real gateway's pricing and budget path has not yet seen Hermes traffic, and the
  in-VM preflight has only run against the stub. Not `SHIPPING`.
