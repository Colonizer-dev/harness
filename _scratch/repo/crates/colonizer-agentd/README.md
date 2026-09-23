<p align="center">
  <a href="https://github.com/Colonizer-dev/harness">
    <img src="https://raw.githubusercontent.com/Colonizer-dev/harness/main/assets/banners/colonizer-agentd.png" alt="colonizer-agentd — Inside every colony." width="100%">
  </a>
</p>

<p align="center">
  <a href="https://crates.io/crates/colonizer-agentd"><img src="https://img.shields.io/crates/v/colonizer-agentd.svg?style=flat-square&labelColor=0A0A0B&color=FF6B35" alt="colonizer-agentd on crates.io"></a>
  <a href="https://github.com/Colonizer-dev/harness/releases/latest"><img src="https://img.shields.io/github/v/release/Colonizer-dev/harness?style=flat-square&labelColor=0A0A0B&color=EDEBE6&label=RELEASE" alt="Latest release"></a>
  <a href="https://colonizer.dev/docs/protocol"><img src="https://img.shields.io/badge/PROTOCOL-COLONIZER.DEV-EDEBE6?style=flat-square&labelColor=0A0A0B" alt="Protocol"></a>
  <a href="https://github.com/Colonizer-dev/harness/blob/main/LICENSE"><img src="https://img.shields.io/badge/LICENSE-MIT-FF6B35?style=flat-square&labelColor=0A0A0B" alt="MIT"></a>
</p>

# `colonizer-agentd`

The daemon inside every [Colonizer](https://colonizer.dev) colony.

A colony is a KVM microVM with a git worktree and a coding agent. `colonizer-agentd` is what the
mothership ([`colonizer-harness`](https://crates.io/crates/colonizer-harness)) talks to in there. It runs
the agent module, keeps the colony's event log and serves its terminal. Colonizer builds it as a static musl
binary and mounts it read-only into every colony, whatever the image, so there is nothing to install by
hand.

## What it does

- **Bridges the agent.** It starts the agent module and speaks its runner contract: commands as JSON Lines
  on stdin, events as JSON Lines on stdout.
- **Keeps the event log.** It numbers each event, appends it to `/var/lib/colonizer/events.jsonl` and
  broadcasts it, so the mothership can reconnect and replay from any point.
- **Serves the terminal.** A login shell in `/workspace` over a WebSocket PTY.
- **Answers only the mothership.** It listens on port 7070 in the colony, and every request needs the
  colony's own bearer token, even inside the private mesh.

| Endpoint | What it does |
| :--- | :--- |
| `GET /v1/health` | Version and the agent's state |
| `GET /v1/events?since=<seq>` | WebSocket: every event after `seq`, then live ones; commands go the other way |
| `GET /v1/pty?cols=<n>&rows=<n>` | WebSocket: a shell in `/workspace` |
| `POST /v1/shutdown` | Shuts the agent down |

The full contract is in the [protocol](https://colonizer.dev/docs/protocol).

## Install

```sh
curl -fsSL https://colonizer.dev/install.sh | sh
```

That installs Colonizer, with this daemon built for the colonies' architecture. The
[install guide](https://colonizer.dev/docs/install) covers building from source. Code, issues and releases:
[Colonizer-dev/harness](https://github.com/Colonizer-dev/harness).

MIT.
