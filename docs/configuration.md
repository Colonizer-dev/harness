# Configuration

Every environment variable, with its full meaning, is in [install.md](install.md#settings); this page is
where settings live and what bounds a colony.

## Where settings live

Module settings live in `~/.config/colonizer/modules.json` and are edited in the UI. The answers to
the [live map](telemetry.md) and [usage data](usage-data.md) questions live beside it, in
`telemetry.json` and `usage.json`, and `usage-last.json` beside those keeps the last usage batch
built. The built-in [dependencies and supply-chain loop](loops.md#dependencies--supply-chain) keeps its settings
in `supply-chain-loop.json`; it is off, with an empty allowlist, until you opt a repository or org
in. Process settings come from the environment. There is one more for usage data:
`COLONIZER_TELEMETRY_ENDPOINT` names the collector it is posted to, at most once a day — with no
default, so unset means nothing is ever sent ([docs/usage-data.md](usage-data.md)). The rest:

| Variable | Default | Meaning |
| :--- | :--- | :--- |
| `COLONIZER_BIND` | `127.0.0.1:7878` | Listen address |
| `COLONIZER_NO_BROWSER` | – | Set to skip auto-opening the cockpit sign-in link |
| `COLONIZER_ALLOWED_HOSTS` | – | Extra `Host` names to accept, comma separated |
| `COLONIZER_GATEWAY_BIND` | `127.0.0.1:41750` | Provider gateway; colonies reach it through `host.microsandbox.internal` |
| `COLONIZER_DATA_DIR` | `~/.local/share/colonizer` | Clones, worktrees, colonies, mesh state |
| `COLONIZER_CONFIG_DIR` | `~/.config/colonizer` | Module config and saved tokens |
| `COLONIZER_CLAUDE_BIN` | auto-detected | Native Claude Code binary to mount |
| `COLONIZER_HOME` | next to the binary, or `dist/` | Bundled app assets |
| `COLONIZER_APP` | `~/.local/share/colonizer/app` | The symlink an install moves; what an [update](updates.md) follows |
| `COLONIZER_UPDATE_CHECK` | on | `0` keeps the update check off whatever Settings says — then no request is made at all |
| `COLONIZER_RELEASES_URL` | GitHub's latest release for this repo | Where the update check looks |
| `DO_NOT_TRACK`, `COLONIZER_TELEMETRY=off` | – | Keep the [live map](telemetry.md) and [usage data](usage-data.md) off whatever Settings says |
| `CI=true` | – | Also keeps [usage data](usage-data.md) off; the live map does not read it |
| `COLONIZER_TELEMETRY_URL` | `https://telemetry.colonizer.dev` | Where live map heartbeats go |
| `COLONIZER_MASTER_KEY` | – (secrets saved in plaintext, 0600) | Encrypts the secrets the mothership saves, at rest; see below |

## `COLONIZER_MASTER_KEY`

`COLONIZER_MASTER_KEY` encrypts the secrets saved under the config directory (the GitHub and Claude
tokens, the model provider keys, the mem0 key and the webhook signing secret) with ChaCha20-Poly1305, keyed by
the SHA-256 of its value, and writes each one as a `.enc` file beside where the plaintext would be, removing
the plaintext. Unset or blank, secrets are written in plaintext (0600) and any stale `.enc` is removed. A
`.enc` file with content is read with the key or not at all: without the key, or with the wrong one, the
secret counts as missing, never falls back to an old plaintext copy. It protects a copied, synced or
backed-up config directory, not a machine where something runs as you, since that can read the variable
too. Use a long random value (32 or more random bytes): the single SHA-256 does no key stretching. Another
machine with a copy of the directory needs the same value. To rotate, set a new value and save each secret
again; if the key is lost, delete the `.enc` files and enter the secrets again.

## The system keychain

When the macOS Keychain or the Linux Secret Service answers a startup probe, newly saved secrets go
there instead of to files (existing files stay until you move them on the cockpit's Secrets page, which
also shows where each one lives). On macOS the Keychain ties an item to the binary that wrote it, so
build with `COLONIZER_CODESIGN_IDENTITY` set (see `scripts/install.sh`) to keep access across rebuilds.

## Per-colony limits

Three limits bound one colony, all sandbox module settings (Settings → Modules → sandbox); `budget_usd`
and `host_disk` take an override per org, the token budget does not. All default to `0` — unlimited — on
purpose: there is no dollar figure, token count or byte count that suits every deployment, and a default
that silently stopped running colonies on upgrade would be a surprise.

- **`budget_usd`** is the most one colony may spend on models, in dollars: Claude's own estimate plus what
  the provider gateway priced on routed providers. When the recorded spend passes it, the mothership stops
  the colony, and a routed request arriving past it is refused with `403`. Claude traffic does not go through
  the gateway — microsandbox swaps the credential for `api.anthropic.com` at its TLS edge — so Claude's
  spend is only seen when a turn ends, and both halves of the total are estimates. The worktree is kept:
  raise the budget and press Resume to continue.
- **`budget_tokens`** is the most one colony may route through the provider gateway, in tokens — counted
  for every routed request, whether or not the provider prices it. A prepaid token or coding plan prices
  nothing, so its colonies spend $0 and `budget_usd` can never trip; this is the budget that holds them.
  Enforcement is the dollar budget's: past it the colony is stopped, a routed request arriving past it is
  refused with `403`, and the worktree is kept — raise the budget and press Resume to continue.
- **`host_disk`** is the most one colony may leave on the host, a size like `16G`: its worktree plus its
  session directory and logs, measured every five minutes. It is not the microVM's root disk, which the
  `root_disk` setting bounds. Past the quota the colony is stopped with its worktree kept, because
  removing a colony's work is your call: clean up or raise the quota and press Resume to continue.

**Disk cleanup** is a built-in loop every install has, off until you switch it on (Loops → Disk
cleanup, or `colonizer loop enable disk-cleanup`). It runs hourly, and early whenever free space
falls under 15%, removing build output (`target/`, `node_modules/`, `.next/`, `dist/`) from finished
colonies, reclaimable worktrees and orphan microVMs — never live colonies, uncommitted or unpushed
work, `.git`, `~/.cargo` or caches. Its preview lists what a run would remove, with sizes, before
you turn it on; archives and your own build directories are opt-in categories
([docs/loops.md](loops.md#disk-cleanup)).

An org's own budget or quota beats the sandbox default, and an org set to `0` opts out of a global limit. Routed
providers need `pricing` — dollars per million tokens for input, output, cached read, cache write and
thinking — to count toward the budget; an unpriced provider still counts its tokens but contributes $0,
so a colony that spends only through one never reaches `budget_usd` and is never stopped for spend —
`budget_tokens` is what holds it.

## The spend journal

Every colony-scoped row of the spend journal (`<data>/spend.jsonl`) names the colony (`session`) and the
agent module that ran it (`agent`), and splits its spend by who measured it: the agent's own turn-end
estimate (first-party traffic never passes the gateway) against the gateway's metered price for routed
providers. `node scripts/colony-report.mjs --costs` reads the journal back grouped per colony and per
harness × model, by default over the same last-30-days window the spend history answers
([docs/protocol.md](protocol.md) §6.8).

## `colonizer.toml`

A few things belong in neither the UI nor the environment. They live in `~/.config/colonizer/colonizer.toml`,
which you write and Colonizer only reads — a missing file means the defaults:

```toml
[publish]
# Who the commit and the pull request body name as co-author: `true` (the default) is the
# github.com/colonizer-settlers account, `co_author = { name = "…", email = "…" }` names
# someone else, and `false` turns the commit trailer, the PR-body trailer and the findings
# credit off. The address must belong to the account or GitHub shows it as plain text —
# for a user account that is the ID-prefixed noreply form. An unreadable or malformed file
# falls back to the defaults, so co-author stays on.
co_author = true
```
