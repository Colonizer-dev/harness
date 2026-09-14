# Colonizer

Turn a task into a pull request by running a coding agent inside a disposable microVM, and follow
along in the browser:

- **Sessions** — start from a GitHub issue or just a repository. Each session gets a fresh git
  worktree on a `colonizer/…` branch and its own microVM.
- **Chat** — watch the agent work; its questions always arrive as **multiple-choice cards** (with an
  "Other…" answer), never as buried plain-text questions. Send follow-ups at any time.
- **Terminal** — a shell inside the same microVM, right next to the chat.
- **Private mesh** — every microVM joins a private Tailscale-compatible network with the harness,
  automatically. It runs on bundled Headscale and never touches your own tailnet.
- **Create PR** — the host commits, pushes and opens the pull request (or let autopilot do it when the
  agent finishes).

Everything is a **module** you choose in Settings → Modules: source (GitHub), sandbox (microsandbox),
mesh (private mesh / loopback), agent (Claude Code), interfaces (chat, terminal) and publish (GitHub PR).

See [docs/architecture.md](docs/architecture.md) and [docs/protocol.md](docs/protocol.md).

## Requirements

- Linux x86_64 with KVM (`/dev/kvm` readable and writable by your user)
- [microsandbox](https://docs.microsandbox.dev) (`curl -fsSL https://get.microsandbox.dev | sh`)
- `git`, `gh`, Node.js ≥ 20 + npm, a Rust toolchain, and a native Claude Code install (its binary is
  mounted read-only into microVMs)

## Install

```sh
scripts/install.sh --install     # builds dist/ and installs ~/.local/share/colonizer/app
colonizer                   # open http://127.0.0.1:7878
```

`install.sh` bundles everything the app needs, so nothing is downloaded at runtime:

| Piece | How it's built |
| --- | --- |
| Headscale, Tailscale | Pinned in `vendor/vendor.lock`, sha256-verified (`scripts/fetch-vendor.sh`) |
| `colonizer-agentd` | Static musl binary built inside a `rust:alpine` microVM (`scripts/build-agentd.sh`) |
| Agent modules | `modules/agents/*` with production `node_modules` |
| Web UI | `web/` (React + assistant-ui + xterm.js) |
| Harness | `crates/colonizer` |

Then open **Settings**:

- **GitHub** – uses your `gh auth login` session automatically, or paste a token.
- **Claude** – **Log in with Claude subscription** runs the official `claude setup-token` flow; the
  token stays on the host.

## Trust model

| What | Where it lives |
| --- | --- |
| GitHub token | Host only. Commit, push and `gh pr create` run on the host after the VM is gone. |
| Claude token | Host only. The guest sees a placeholder; microsandbox's TLS proxy swaps in the real value for `api.anthropic.com` only. |
| Worktree | Mounted read-write at `/workspace`. |
| Git objects & worktree metadata | Mounted read-only (`git status/diff/log` work in the VM, commits don't). |
| VM output | Untrusted: `.git` is rewritten, nested `.git` dirs removed, host git runs without hooks/fsmonitor, `pr.md` must be a regular file. |
| Mesh | Separate Headscale + userspace tailscaled (own state and socket, `--no-logs-no-support`). The harness may reach VMs; VMs can't reach each other. VMs get one extra network rule: UDP to the harness node's port, for direct WireGuard. |
| agentd | Per-session bearer token, even inside the mesh. |
| Web API | Loopback by default; rejects unexpected `Host` headers and cross-origin writes and WebSocket upgrades. |

microVMs are detached: they keep running when the harness restarts, and sessions reconnect.

## Configuration

Module settings live in `~/.config/colonizer/modules.json` (edit them in the UI). Process
settings come from the environment:

| Variable | Default | Meaning |
| --- | --- | --- |
| `COLONIZER_BIND` | `127.0.0.1:7878` | Listen address |
| `COLONIZER_ALLOWED_HOSTS` | – | Extra `Host` names to accept, comma separated |
| `COLONIZER_DATA_DIR` | `~/.local/share/colonizer` | Bare clones, worktrees, sessions, mesh state |
| `COLONIZER_CONFIG_DIR` | `~/.config/colonizer` | Module config and saved tokens (0600) |
| `COLONIZER_CLAUDE_BIN` | auto-detected | Native Claude Code binary to mount |
| `COLONIZER_HOME` | next to the binary / `dist/` | Bundled app assets |

## Development

```sh
scripts/install.sh                         # build dist/ in the checkout
cargo test --workspace                     # harness + agentd tests
(cd modules/agents/claude-code && node --test test/)
(cd web && npm run dev)                    # UI dev server, proxies /api to 127.0.0.1:7878
open 'http://127.0.0.1:5173/?mock=1'       # UI against an in-browser mock backend
```

## Run as a user service

```ini
# ~/.config/systemd/user/colonizer.service
[Unit]
Description=Colonizer

[Service]
ExecStart=%h/.local/share/colonizer/app/bin/colonizer
Environment=PATH=%h/.local/bin:%h/.local/share/mise/installs/claude/latest:/usr/bin
Restart=on-failure

[Install]
WantedBy=default.target
```

```sh
systemctl --user daemon-reload && systemctl --user enable --now colonizer
```
