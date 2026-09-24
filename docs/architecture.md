# Colonizer architecture

Colonizer turns a task (a GitHub issue today) into a pull request by running a coding agent
inside a disposable microVM, with a web UI to watch, answer the agent's questions, and open a
terminal in the VM.

Colonizer runs on Linux x86_64 with `/dev/kvm` readable and writable, or an Apple Silicon Mac. An
Intel Mac can't run it, because microsandbox's libkrun backend is aarch64-only. On Linux the host
also needs glibc 2.28 or newer, which the pinned microsandbox binary requires.
[docs/install.md](install.md) has the rest.

```mermaid
flowchart TB
  browser["browser"]

  subgraph mothership["mothership"]
    direction TB
    host["colonizer (Rust)<br/>the mothership process"]
    hs["headscale<br/>127.0.0.1 · control plane"]
    ts["tailscaled --tun=userspace<br/>the harness's own mesh node"]
    gw["provider gateway<br/>127.0.0.1:41750"]
  end

  mesh{{"private mesh<br/>never your own tailnet"}}

  subgraph colony["colony · one microVM"]
    direction TB
    vmts["tailscaled (static)<br/>joins the mesh at boot"]
    agentd["colonizer-agentd :7070<br/>events · pty · shutdown"]
    runner["agent runner (module)<br/>Claude Code via the Agent SDK"]
    ws["/workspace<br/>git worktree (rw)"]
  end

  browser -->|HTTP/WS| host
  host --> hs
  host --> ts
  host --> gw
  ts -->|SOCKS5| mesh
  hs -.->|control| mesh
  mesh --> vmts
  vmts --> agentd
  agentd --> runner
  runner --> ws
  runner -.->|"&lt;provider&gt;/&lt;model&gt;"| gw

  classDef box fill:#12151d,stroke:#2a3040,color:#e7e9ef
  classDef edge fill:#0f1218,stroke:#ff7b2c,color:#ff7b2c
  class browser,host,hs,ts,gw,vmts,agentd,runner,ws box
  class mesh edge
```

## Modules

Every moving part is a module selected and configured in the harness (`~/.config/colonizer/modules.json`,
editable in Settings → Modules). A module kind has one active provider:

| Kind | Providers (v1) | Responsibility |
| --- | --- | --- |
| `source` | `github` | List repositories and issues, fetch an issue for the prompt |
| `sandbox` | `microsandbox` | Boot/stop/remove microVMs with mounts, secrets and network rules. A `preset` picks the image (pinned by digest from `crates/colonizer/images.lock`) and machine size; `auto`, the default, reads the stack off the repository's marker files when the colony's worktree is checked out and falls back to Node when a repository names none; explicit settings override it |
| `mesh` | `headscale` (or `none`) | Private Tailscale-compatible network between harness and VMs |
| `agent` | `claude-code` | Runner that speaks the Colonizer agent protocol inside the VM |
| `interfaces` | `default` | Panels in the session view; `chat` and `terminal` are its settings |
| `publish` | `github-pr` | Commit, push and open the pull request on the host, each only when not already done |
| `memory` | `files`, `mem0` | Shared notes per repository, org and globally; agents propose, the user approves. `mem0` stores approved notes in a mem0 project and writes each colony's copy at boot |
| `watchdog` | `default` | Nudges colonies that stop making progress and flags the ones that need the user |
| `autonomy` | `off`, `judge` | A model answers a colony's questions when nobody does, among the options the agent offered; off by default |
| `notify` | `default` | Announces a colony asking a question, stalling, failing or opening a pull request, or a model provider starting to fail, to the desktop or a webhook. Absent from `modules.json` until first configured; what leaves the mothership is one short line about the colony, never repository content |
| `burn_down` | `default` | Spends a weekly token plan before it resets: launches bug-hunt colonies paced across the window down to a reserve, then stops. Off until configured; see [burn-down](burn-down.md) |
| `voice` | `browser`, `openai`, `groq`, `deepgram`, `elevenlabs`, `openai_compatible` | Speech-to-text for the cockpit composer's microphone. `browser` (the default, also what an absent entry reads as) is the browser's own recogniser and nothing server-side. Any other provider is a transcription API the mothership calls: the browser records a clip, posts it to `/api/voice/transcribe`, and the mothership forwards it with the key (`voice-keys/<provider>`, 0600, or the provider's env var, or a configured OpenAI / Groq model provider's key) and returns the text. Audio goes browser → mothership → service and is not kept; nothing reaches a colony but the words you send |

Two settings layers sit next to the modules:

- **Model providers** (`providers.json`, keys in the system keychain when it works, else `provider-keys/`, 0600): Anthropic-compatible endpoints,
  or OpenAI Chat Completions endpoints the gateway translates, that the agent can route models to as
  `<provider>/<model>`. The Claude Code runner starts a router inside
  the colony that sends those requests to the mothership's provider gateway
  (`host.microsandbox.internal:41750`). The gateway reaches loopback, LAN and tailnet providers, adds the
  key, queues requests per provider (`max_concurrent`), applies long timeouts, and marks the colony busy
  for the watchdog; the runner falls back to a Claude model when the gateway reports the provider
  unreachable, timed out or full.
- **Org workspaces** (`orgs.json`, `known-orgs.json`): per-GitHub-org overrides for agent models, the parallel limit, the
  per-colony budget and host-disk quota, the sandbox stack, memory, the watchdog and notifications, plus an on/off
  switch per org. `known-orgs.json` records the orgs seen on the signed-in GitHub account, so an org that appears for
  the first time asks instead of being adopted silently. A colony belongs to its repository
  owner's org.

## Session lifecycle

```mermaid
stateDiagram-v2
  direction LR
  [*] --> Queued: past the parallel limit
  [*] --> Create
  Queued --> Create: a slot frees a launch
  Queued --> Boot: a slot frees a resume
  Queued --> Stopped: left the queue
  Create --> Boot: worktree, session dir, mesh key
  Boot --> Connect: agentd up on the mesh
  Connect --> Interact: prompt sent
  Interact --> Interact: questions, follow-ups, terminals
  Interact --> Publish: autopilot, or "Create PR"
  Interact --> Stopped: microVM gone, or a limit passed
  Stopped --> Queued: Resume past the limit
  Stopped --> Boot: Resume, same worktree
  Publish --> [*]: VM removed, then the host publishes the branch
```

0. **Queued** – a colony launched past the parallel limit (global, the org's own, or per repository) is created
   `queued`: no worktree, no microVM, nothing claimed. A resume that lands on a full limit queues too,
   keeping its worktree while it waits. Every five seconds the harness starts the oldest
   queued colony that fits, so a queue drains on its own as colonies finish.
   An org or repository at its own limit doesn't hold up the colonies behind it, and leaving the queue is just Stop.
1. **Create** – source module fetches the issue; the host creates a bare clone + git worktree on a
   `colonizer/issue-<n>-<id>` branch; the harness writes the session directory (`session.json`, `token`,
   `prompt.md`, `boot.sh`, mesh auth key).
2. **Boot** – sandbox module runs `msb run -d` with the image command `sh /colonizer/boot.sh`. The boot
   script starts `tailscaled`, joins the mesh (`--accept-dns=false`, so microsandbox's DNS-based
   secret injection keeps working), then `exec`s `colonizer-agentd`.
3. **Connect** – the harness waits until headscale reports the node online, then connects to
   `ws://<mesh-ip>:7070/v1/events` through its SOCKS5 proxy, persists events, and fans them out to
   browsers. agentd sends the initial prompt to the agent runner.
4. **Interact** – the user watches the chat, answers questions (always multiple choice + "Other"),
   sends follow-ups, and opens terminals (`/v1/pty`), all over the mesh.
5. **Publish** – "Create PR", or autopilot (the `publish` module's `autopilot` setting, on by default)
   when a turn ends without an error or open question and the agent wrote or updated `pr.md` during
   it: agentd shuts the runner down, the VM is removed, and the host publishes with the hardened
   publish step: committing (co-authored by Colonizer Settlers) only what is uncommitted, pushing the colony's
   own `colonizer/…` branch only when origin is behind it, and reusing a pull request that is already
   open for the branch instead of opening a second one. It refuses to push anything else, checked
   before the VM is removed. A publish that fails part-way leaves the colony `failed`, and it can be
   published again from there (the kept worktree and the remote are enough, no new microVM) with the
   remaining steps picked up where the attempt stopped. A turn that ends with an error
   (not an interrupt) holds autopilot and flags the colony (`autopilot_held`). The mesh node is deleted.
6. **Resume** – a microVM that stops on its own (the sandbox's max session length, or the host restarting)
   leaves the worktree behind. Once a minute the harness checks which sandboxes are still running and marks
   a colony whose VM is gone `stopped`, rather than leaving it looking idle. "Resume" boots a fresh microVM
   on the same worktree and branch and tells the agent to continue from what is already there. A resume past
   the parallel limit queues instead, and boots on its worktree when a slot frees. The new
   agentd numbers its events from 1, so the previous transcript is rotated to `events-<n>.jsonl` first.
   Changing models does not need a resume: a live `set_model` switches the running colony's model for
   its next turns and keeps the session (docs/protocol.md §6.1b).

## Per-colony limits

Four sandbox module settings bound colonies, each with a per-org override:

- `max_parallel` caps colonies live at once, global or per org, which is the queue above. Beside it,
  `repo_max_parallel` (default 3, overridable per org) caps colonies live at once in one repository. The
  parallel limits layer instead of shadowing: global, org and repository must all have room, so the tightest wins.
- `budget_usd` caps a colony's whole model spend. The provider gateway counts the usage of every response
  it routes, prices it with the provider's `pricing`, and adds it to the colony's `routed_cost_usd`; the
  budget answers to that plus Claude's own `cost_usd`. Past it, a routed request is refused with `403` and
  the host stops the colony.
- `host_disk` caps what a colony leaves on the host (its worktree plus its session directory), measured
  every five minutes. It does not cover the microVM's root filesystem, which `root_disk` bounds. A colony
  past the quota is stopped and its worktree kept: removing a colony's work is the operator's call.

Two more sandbox settings watch the host's own disk rather than any one colony. `warn_free_disk`
(default 10G) warns in the cockpit when free space on the data dir's volume drops below it, and
`min_free_disk` (default 5G, or `COLONIZER_RECLAIM_MIN_FREE` when no explicit setting is saved) pauses
queue admission below it: queued colonies wait and running ones keep running — the pause
itself deletes nothing — and admission resumes by itself when space returns. Below the floor
the reclaim sweep (unless off with `COLONIZER_RECLAIM=0`) also reclaims finished colonies whose
work is already pushed without waiting for the retention window; unpushed work is never deleted.
0 turns either off.

A fifth sandbox setting carries a per-org override without bounding anything: the org's `stack` pins the
sandbox stack for its colonies, shadowing what the global `preset` would otherwise choose — `auto` by
default, which reads each repository's marker files at boot. `null` inherits.

`max_parallel` and `repo_max_parallel` default to 3. The other two default to unlimited: there is no
dollar figure or byte count that suits every deployment, and a default that silently stopped running colonies on upgrade would
be a surprise. When the host stops a colony, the only stop it decides on its own, the
microVM is torn down, the status goes to `stopped` with the reason in the colony log, and the worktree is
kept: Resume continues once the limit is raised, queued if the parallel limit is full.

## Mesh design

- Headscale listens on `127.0.0.1`; VMs reach it as `http://host.microsandbox.internal:<port>`
  through a single scoped `allow@host:tcp:<control-port>` rule, not the broad `host` profile
  (which would open every host-loopback port to the untrusted colony; see
  [sandbox-network.md](sandbox-network.md)).
- The harness node is a separate userspace `tailscaled` (own state dir, socket under
  `/run/user/<uid>/colonizer/`, fixed UDP port, `--no-logs-no-support`). It never touches the
  system tailscaled or the user's tailnet. Tailscale publishes no macOS `tailscaled`, so on a Mac the
  bundled one is built from the source pinned in `vendor/vendor.lock` (`scripts/build-tailscaled.sh`);
  on Linux it comes from upstream's tgz.
- VMs get one narrow extra rule, `allow@<host-lan-ip>:udp:<harness-udp-port>`, so WireGuard
  connects directly (≈1 ms) instead of through a public DERP relay. LAN access stays blocked.
- Users: `harness` and `vms`. Policy: `harness@` may reach `vms@:*`; VMs cannot reach each other.
- VM keys are single-use, ephemeral, 30-minute pre-auth keys; nodes are deleted on session end.
- Headscale reads a bundled DERP relay map (`vendor/derpmap.yaml`, refreshed with
  `scripts/update-derpmap.sh`) instead of fetching one, so the mesh starts without internet access.
  Relays are only a fallback; the direct UDP path doesn't need them.

## Trust boundaries

- GitHub token: host only. Claude credential: host only, injected by microsandbox's TLS proxy for
  `api.anthropic.com`; the guest sees a placeholder. Model provider keys: host only, added by the
  provider gateway, which accepts only a live colony's token.
- Git objects and worktree metadata are mounted read-only; publish treats VM output as untrusted.
- agentd requires a per-session bearer token even inside the private mesh.
- Browser API: loopback bind by default, Host/Origin checks (including WebSocket upgrades).
- Network: what a colony's microsandbox profiles allow and deny is in
  [sandbox-network.md](sandbox-network.md).

The external audit of v0.1.3 checked these boundaries against the code; its findings and the
release checkpoints are in [audit.md](audit.md).

## Testing

`cargo test --workspace` is the gate every change passes, and none of it needs KVM, network or
credentials. The deepest layer in it is agentd's (`crates/colonizer-agentd/tests/`): it boots the
real binary as a host process on loopback against a stub agent runner and asserts the documented
behaviour (docs/protocol.md §2–§3): the bearer-token wall, the initial prompt, events stamped with a
gap-free `seq` and an RFC 3339 `ts`, `user_message` and `answer` frames reaching the runner's stdin,
replay from `since`, the PTY roundtrip, and clean shutdown. `cargo test -p colonizer-agentd --test
smoke` runs just the single boot-path pass of those.

What it does not cover is the colony around agentd: the `msb run` boot itself, the session directory
the mothership writes (plugin mounts, `boot.sh`, the mesh key), subagent model resolution and cost
accounting: everything that needs a real microVM and a real model. Two things cover that:
`scripts/build-agentd.sh --smoke` runs agentd's boot checks inside a real microVM on any machine with
`/dev/kvm`, and CI's `colony-e2e` job boots a whole colony end to end on every pull request — a real
mothership, microVM, agentd, claude-code runner and Claude Code CLI against a scratch git repository
and a stub Anthropic-wire model server (`scripts/colony-e2e.mjs`), asserting the colony publishes and
comes back `no_changes`. GitHub-hosted runners do have `/dev/kvm` (the job chmods it for the runner
user), so this needs no self-hosted hardware; what it still does not cover is anything a real model
or a real GitHub write would do.

## Packaging

`scripts/install.sh` produces a self-contained app directory (`COLONIZER_HOME`, default
`~/.local/share/colonizer/app`):

```
bin/colonizer            host server
bin/colonizer-agentd             static musl build (built in a rust:alpine microVM)
bin/claude-guest                 Mac only: linux-arm64 Claude Code, fetched at install time (the host's own binary is Mach-O)
vendor/headscale              pinned + sha256-verified (vendor/vendor.lock)
vendor/tailscale/{tailscale,tailscaled}   static, pinned + verified (built from source on a Mac)
vendor/derpmap.yaml           DERP relay map snapshot (committed)
modules/agents/claude-code/   runner + production node_modules
web/                          built UI
claude-code.lock              guest Claude Code pin: version + sha256 per platform, read at install
images.lock                   each preset's colony image pinned by OCI digest (also compiled in)
```

Two things arrive lazily rather than with the install: the colony image, pulled by digest the first
time it is needed (`--pull-image` does it at install time), and the Headroom bundle when Headroom is
switched on. Each is checked against a pin before use.
