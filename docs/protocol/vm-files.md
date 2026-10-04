# 1. Files inside the VM

Part of the [Colonizer protocol](../protocol.md).

| Path | Mode | Content |
| --- | --- | --- |
| `/colonizer/session.json` | ro | Session config (below) |
| `/colonizer/token` | ro | Bearer token for agentd (single line). agentd seals the file once it has read it, so it reads empty for the rest of the boot |
| `/colonizer/boot.sh` | ro | Boot script (image command) |
| `/colonizer/mesh-authkey` | ro | Headscale pre-auth key (absent when mesh disabled) |
| `/colonizer/path-policy` | ro | The path policy the boot script enforces before the agent starts: files masked or pinned read-only in the worktree ([path-policy.md](../path-policy.md)). A missing list stops the boot |
| `/colonizer/memory/{global,org,repo}/` | ro | Approved shared-memory notes (§6.2). Absent when memory is off |
| `<data>/repos/<owner>/<name>.git` | ro | The mothership's bare clone, mounted at its host path; `GIT_DIR` points at the worktree's admin directory inside it |
| `/opt/colonizer/bin/colonizer-agentd` | ro | Static agentd binary |
| `/opt/colonizer/tailscale/{tailscale,tailscaled}` | ro | Static tailscale binaries |
| `/opt/colonizer/agent/` | ro | Active agent module directory |
| `/opt/colonizer/plugins/<name>/` | ro | Claude Code plugin directories, one per entry in `COLONIZER_PLUGIN_DIRS`. Absent when none are configured |
| `/opt/colonizer/{caveman,headroom,jev-compaction}/`, `/opt/colonizer/bin/rtk` | ro | The token-saving payloads (Token savings, §4), each mounted only when its switch is on and the payload is installed |
| `/opt/claude/bin/claude` | ro | Claude Code binary (claude-code module only) |
| `/root/.claude/projects` | rw | The agent's session transcripts, a host directory (`<session dir>/transcripts`) mounted writable so they outlive the microVM. Path from the module's `session_resume.dir` (claude-code: `/root/.claude/projects`, codex: `/root/.codex`, acp: `/root/.gemini`) |
| `/colonizer/services` | rw | Service records the guest's writers keep ([colonies.md](../colonies.md#services-that-come-back-after-a-resume)), one JSON file per service (`<name>.json`). A host directory (`<session dir>/services`) mounted writable, so they outlive the microVM like the transcripts above |
| `/opt/node/bin/node` | ro | Vendored Node runtime for the agent runner, pinned in `vendor/node.lock` and fetched at install by `scripts/fetch-node-binary.sh`, mounted read-only beside agentd |
| `/workspace` | rw | Git worktree |
| `/harness/out` | rw | Files the agent hands to the host (e.g. `pr.md`) |
| `/var/lib/colonizer/events.jsonl` | VM-local | agentd event log (replay source) |

`session.json`:

```json
{
  "session_id": "ab12cd34",
  "workspace": "/workspace",
  "listen": "0.0.0.0:7070",
  "agent": {
    "module": "claude-code",
    "command": ["node", "/opt/colonizer/agent/runner.mjs"],
    "env": { "COLONIZER_MODEL": "" }
  },
  "initial_prompt": "You are resolving GitHub issue #12 ..."
}
```

One more variable reaches `agent.env` on a boot that delivers an answer held while the colony was
suspended ([#562]): `COLONIZER_RESUME_SESSION` carries the `agent_session` id the runner reported
last run, for it to continue that conversation (the Claude Code runner passes it to the SDK's
`resume` option, codex to `codex exec … resume`, ACP to `session/load`); the held answer is the
`initial_prompt`. Absent means a fresh conversation.

`COLONIZER_SERVICES_DIR=/colonizer/services` reaches `agent.env` on every boot (#700): the
directory the guest's service writers (`colonizer-svc`, a Claude Code background-Bash hook) record
started services in, one JSON file per service. On a resume boot, `session.json` also gains a
top-level `restore` key — absent on other boots, which is how the guest tells a restore from a
fresh start:

```json
"restore": {
  "suspended": true,
  "services": [
    { "name": "web", "cmd": "npm run dev -- --port 5173", "cwd": "web", "ready": "5173",
      "env": ["VITE_API_URL"], "timeout_secs": 30, "restart": true, "source": "manifest" }
  ]
}
```

`suspended` says the colony was suspended when the resume claimed it. `services` lists the
repository's `.colonizer/services.toml` declarations first, then the records the previous run left
in `COLONIZER_SERVICES_DIR`; background records (`restart: false`) are listed once and deleted, so
they are reported lost exactly once. `env` holds names only and every `cmd` is scrubbed of the
colony's secret values before any of this is written. The guest relaunches each restartable
service, waits it out to `ready` or `timeout_secs` (default 60), and opens the resumed turn saying
what came back and what was lost.

## Where the agent runtime comes from

Every colony ships two things: its stack toolchain image (the preset's image — Node, Python,
Rust or Go, chosen per repository as §4's `POST /api/sandbox/pull` describes) *plus* a vendored
Node binary. No custom images, no per-boot download: the Node runtime is pinned in
`vendor/node.lock`, fetched once at install by `scripts/fetch-node-binary.sh` into
`dist/bin/node-guest`, and mounted read-only at `/opt/node/bin/node`, beside agentd.

The runner command `["node", "/opt/colonizer/agent/runner.mjs"]` resolves `node` via `PATH`,
with `/opt/node/bin` first — so the agent entry runs on the vendored runtime even on a non-Node
stack image. A Rust colony boots `rust:1-bookworm` for its toolchain and still runs its runner
on Node; the colony keeps its own toolchain and the brief (§6.1's `COLONIZER_IMAGE`) names the
resolved image, so the agent knows which toolchains are native and which need installing.

A host missing `bin/node-guest` fails the boot fast instead of launching a colony whose runner
cannot start; a runner that fails to start sets attention `agent_failed` (the boot half of this,
owned by the sessions/runner side). Both hosts fetch it at install time, never at runtime — a Mac
alongside the guest Claude Code build (which a Linux host skips in favour of its native install),
a Linux host for node alone, since there is no host Node binary a colony can reuse.

---
