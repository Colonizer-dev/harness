# OpenCode agent module

Runs [OpenCode](https://opencode.ai) (`opencode run --format json`) and speaks the Colonizer
runner contract (`docs/protocol.md` §2): commands as JSON lines on stdin, events as JSON lines
on stdout, diagnostics on stderr.

- One turn per `user_message`: `opencode run` with the prompt on stdin, resumed with `--session`
  from the second turn on. A turn with no output for 120 s is SIGINTed and retried once.
- Questions: OpenCode's native question tool is unavailable in `run` mode, so the model asks
  only through `colonizer_ask_user` (own MCP server, `mcp.mjs`), surfaced as `question` events;
  the matching `answer` command resolves them. Long asks survive: the MCP `timeout` is set to
  an hour and progress notes hold the call open.
- Text arrives per part as `assistant_text` (`reasoning` as `thinking`); tool completions become
  `tool_call`/`tool_result` (capped at 20 000 characters); `turn_end` carries cumulative
  `model_usage` per `<provider>/<model>`.

## Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `COLONIZER_MODEL` | required | Agent model as `<provider>/<model>` from Settings → Providers, e.g. `local/deepseek-v4-flash` |
| `COLONIZER_SMALL_MODEL` | main model | Model for titles and summaries, same `<provider>/<model>` form |
| `COLONIZER_MODEL_ROUTES` | none | JSON provider routes (`docs/protocol.md` §6.5) |
| `COLONIZER_FINDINGS` | off | `true` emits `finding` events for `colonizer_finding_file` calls |
| `COLONIZER_MEMORY_DIR` | unset | Shared memory the model may read (`{repo,org,global}/notes/*.md`) |
| `COLONIZER_OPENCODE_BIN` | none | Use this binary instead of downloading the pinned one |

Models are reached through the mothership's provider gateway, which holds the keys (§6.5): for a
LAN/tailnet endpoint add the `local` preset in Settings → Providers (`base_url`
`http://<tailnet-ip>:8000`, `auth` none), pick the OpenCode agent module, and set the model to
`local/<model>`. The mothership host reaches the tailnet; the colony only reaches the gateway.

## Binary

At boot the runner uses `COLONIZER_OPENCODE_BIN`, then `opencode` on `PATH`, else downloads the
pinned build for its architecture from registry.npmjs.org, checks it against `opencode.lock`
(sha256, before extraction), and reuses it on later boots. The tarball and binary are cached on
disk under `$XDG_CACHE_HOME/colonizer/opencode` (or `~/.cache/…`), not in `/tmp`: the colony's
`/tmp` is a small tmpfs and the two together are ~245 MB. `linux-x64-baseline` serves x64 CPUs
without AVX2.

## Limitations

- No Claude subscription models: the Claude login belongs to the Claude Code module.
- Streaming is per part, not per token; resume starts a fresh OpenCode session per turn series.
- The native `question` tool is unavailable in `run` mode; questions use `colonizer_ask_user`.

## Develop

```sh
node --test test/*.test.mjs   # fake-child tests, no network, no binary
```
