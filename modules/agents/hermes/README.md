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
| `COLONIZER_HERMES_BIN` | unset | The hermes command (split on spaces). Unset, the runner takes `hermes` from the `PATH`, then the pinned build it stages on first boot (below) |
| `COLONIZER_HERMES_HOME` | `/tmp/colonizer-hermes` | `HERMES_HOME`; the runner writes `config.yaml` (as JSON, valid YAML) and the session-id file here. The config carries a `model:` block naming the resolved provider and default, rewritten before every turn so it always matches the CLI flags — without it Hermes' first-run guard sees "no API keys or providers found" (it ignores the top-level `providers:` map) and exits |
| `COLONIZER_MODEL` | none — required | `<provider>/<model>`, which must match a gateway route; anything else refuses the turn |
| `COLONIZER_DISABLED_TOOLS` | empty | The module's Disabled tools setting: comma-separated Hermes toolset names appended (deduplicated) to the always-off list in `agent.disabled_toolsets`. Whole toolsets only — a single tool inside one (only `write_file` within `file`, say) cannot be turned off; `memory`, `skills`, `delegation`, `cronjob`, `tts` and `clarify` are always off already |
| `COLONIZER_MODEL_ROUTES` | none | JSON provider routes from the mothership (`docs/protocol.md` §6.1); one becomes a Hermes provider `colonizer-<id>` on the Anthropic Messages wire with the colony header |
| `COLONIZER_HERMES_TURN_TIMEOUT_SECS` | 3600 | Per-turn cap; exceeding it SIGTERMs Hermes and ends the turn with an error |

Models go only through the mothership's provider gateway, which holds provider keys host-side and
does pricing and budget accounting; the colony VM needs no provider secret and no provider egress.
The gateway expects the bare model (the claude-code router strips the same `<id>/` prefix), so `-m`
gets the remainder after the route prefix. Nous Portal models (`nous/…`) are refused: Portal credits
are a prepaid pool the gateway cannot account for ([#199](https://github.com/Colonizer-dev/harness/issues/199)).

## Staging on first boot

Hermes ships no release binaries, and PyPI stops at 0.19.0, so the runner builds the pinned
hermes-agent itself the first time a colony boots without one (`stage.mjs`). Every byte that runs is
checked against a hash in this module:

| File | Pins |
| :--- | :--- |
| `hermes.lock` | the GitHub source tarball of tag `v2026.9.24`'s exact commit (`f97608f1…`, also `requires.pins.hermes.source_rev` in `module.json`), uv 0.12.23 per architecture, a python-build-standalone CPython 3.13 per architecture, and the sha256 of `hermes-requirements.lock` |
| `hermes-requirements.lock` | every Python dependency of `hermes-agent[mcp]`, exported from upstream's own `uv.lock` with all of its PyPI hashes, plus the `setuptools` and `wheel` build backend |

The steps: download uv for this architecture and check its sha256; use the image's `python3.11`–`3.13`
if it has one (the colony toolbox images carry Debian's 3.11 or the python preset's 3.13), otherwise
download the pinned CPython and check its sha256; download the source tarball and check its sha256;
`uv venv`; `uv pip install --require-hashes --only-binary :all: -r hermes-requirements.lock`, so
every dependency is a wheel whose hash is listed; then `uv pip install --no-deps
--no-build-isolation --no-index -e` on the source tree, editable as upstream's installer does it,
built by the hash-pinned setuptools. The `[mcp]` extra is in the lock, so the `ask_user` tool is
always there. uv runs with `UV_NO_CONFIG` and `UV_PYTHON_DOWNLOADS=never`, so neither a stray config
nor a managed-Python download changes what is installed.

Any mismatch fails closed: a download whose sha256 differs is deleted unextracted, a requirements
file that differs from its pinned sha256 or lists a requirement without a hash is refused before any
download, uv refuses a wheel whose hash is not listed, and the runner then ends with `status error`
naming the step. Nothing is left half-trusted: the build lives in
`~/.cache/colonizer/hermes/<commit>-<lock digest>/<platform>/`, beside the other fetched CLIs, and a
`staged.json` marker written last is what later boots look for. Without it the directory is wiped and
rebuilt. With it, staging is skipped (a rebuild also happens when the interpreter the venv was made
from is gone). A new pin removes the previous build.

**Cost**, measured in a `node:24-bookworm-slim` container: about 40 seconds on a fast link, on
linux-arm64 with the pinned CPython and on linux-x64 with Debian's Python 3.11 alike. It takes
420–490 MB of disk: the source tree 217 MB (the editable install keeps it), the venv 143 MB, uv
40 MB, and the CPython 90 MB when it is downloaded. The downloads are about 75 MB of source, 20 MB of
uv, 30–35 MB of CPython when needed, and the wheels. The colony needs `codeload.github.com`,
`github.com` and its release-asset hosts, `pypi.org` and `files.pythonhosted.org`; the module
declares them under `egress.extra`.

To move the pin, run `node scripts/pin-hermes.mjs <tag>` (add `--uv <version>` or `--python
<version>+<release>` to move those too). It resolves the tag's commit with `git ls-remote`, hashes
each download, checks uv's and CPython's against the `.sha256` and `SHA256SUMS` files their releases
publish, regenerates `hermes-requirements.lock` with the pinned uv, proves it with a wheels-only
`--require-hashes` dry run for Python 3.11, 3.12 and 3.13 on x86_64 and aarch64, and only then
rewrites both locks and the `module.json` pin.

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
- **Questions.** The runner registers a vendored MCP server under `mcp_servers.colonizer` in the
  config (the module's `mcp.mjs`, byte-identical to the codex module's) whose `ask_user` tool POSTs
  to a loopback bridge in the runner: the call becomes a `question` event for the cockpit, and the
  `answer` command resolves it and returns `{answers, response}` to the tool call. The server's
  tool-call `timeout` is an hour and `mcp.mjs` holds a long ask open with progress notes; an
  interrupt or a turn end cancels the ask. A question is never also a `tool_call`/`tool_result`
  (§2). `clarify` stays disabled — the MCP tool replaces it. Hermes only loads MCP servers when the
  optional `mcp` Python extra is installed (`pip install -e ".[mcp]"`); without it the tool is
  silently absent. The config's server env carries the bridge coordinates and no findings or memory
  switch, so mcp.mjs's own gating leaves those tools unoffered here.
- **Loop tools.** A loop colony's server env also carries `COLONIZER_LOOP` and
  `COLONIZER_LOOP_SELF_PACED` (issue #643, [loops.md](../../../docs/loops.md)), so `mcp.mjs` offers
  `loop_stop`, and `loop_next` on a self-paced loop. Both POST to the same bridge, which emits a
  `loop_next` or `loop_stop` event as the codex module's does; `module.json` declares
  `"loop_tools": true`, so a self-paced loop is briefed with `loop_next` instead of running every
  24 hours. Like `ask_user`, they need the `[mcp]` extra.
- **Credentials.** Only gateway routes, above: no provider secret ever enters the colony.

## Gaps

- First boot pays for staging (above). A colony whose cache does not persist pays it every boot.
- A custom image with its own `hermes` on the `PATH` is used as is, without the pinned build. It
  is the operator's word, still probed with `hermes --version` by the runner's preflight.
- Questions need the `[mcp]` extra. The staged build always has it, but a custom image whose
  hermes-agent install skipped it silently has no `ask_user` tool, and the model then has no
  channel to ask anything.
- The [exec policy](../claude-code/README.md#exec-policy) is not applied: the harness refuses to
  launch a Hermes colony while one is set (the install's `exec_policy` setting, or a repo
  `.colonizer/exec-policy.json`).
- No end-to-end colony run: the live verification above used a fake Anthropic-wire gateway and no
  microVM, so the real gateway's pricing and budget path has not yet seen Hermes traffic. The
  first-boot staging has run for real only in Linux containers (both architectures), not in a
  colony microVM. Not `SHIPPING`.
