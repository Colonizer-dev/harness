<p align="center">
  <img src="assets/readme-banner.svg" alt="Colonizer Harness. The open-source core. One microVM per task, a private mesh home, and a pull request at the end." width="100%">
</p>

<p align="center">
  <img src="https://img.shields.io/badge/STATUS-ALPHA-FF6B35?style=flat-square&labelColor=0A0A0B" alt="Status: alpha">
  <img src="https://img.shields.io/badge/LANGUAGE-RUST-EDEBE6?style=flat-square&labelColor=0A0A0B" alt="Language: Rust">
  <img src="https://img.shields.io/badge/SANDBOX-KVM%20MICROVMS-EDEBE6?style=flat-square&labelColor=0A0A0B" alt="Sandbox: KVM microVMs">
  <img src="https://img.shields.io/badge/MESH-HEADSCALE%20%C2%B7%20WIREGUARD-EDEBE6?style=flat-square&labelColor=0A0A0B" alt="Mesh: Headscale and WireGuard">
  <img src="https://img.shields.io/badge/AGENT-CLAUDE%20CODE%20%C2%B7%20PI-EDEBE6?style=flat-square&labelColor=0A0A0B" alt="Agent: Claude Code and Pi">
  <img src="https://img.shields.io/badge/LICENSE-MIT-FF6B35?style=flat-square&labelColor=0A0A0B" alt="License: MIT">
</p>

<p align="center">
  <b>colonizer.dev</b> · HARNESS · the open-source core
</p>

---

# The harness

Coding agents are good enough to work on their own for a long time. Most setups still make you pick
between **safe** (a sandbox, one task at a time, an approval every few seconds) and **fast** (an agent
with your credentials loose on your laptop). This is the setup that doesn't make you pick.

Every task gets a **colony**: its own KVM microVM with a fresh git worktree and an agent inside. The
agent can do anything in there. Every colony joins a **private mesh** with the machine that launched it,
so its chat and a real terminal are one hop away. When the agent needs you, it asks with **choices**.
When the work is done, your machine, the **mothership**, commits it and opens the pull request.

This repository is the open-source core, MIT, and it runs on one machine today: Linux with KVM, or an
Apple Silicon Mac. [colonizer.dev](https://colonizer.dev) is the name for everything around it. The
domain serves a page about the project, the docs, the installer and the live map. The hosted Colonizer
is not built yet, and nothing in this README pretends otherwise.

> **Colonies only ever hold placeholders.**
> The GitHub token never enters a colony. The agent's API credential is swapped in by the sandbox's
> host-side TLS proxy, for one host, on the way out. A colony that goes rogue can wreck its own
> worktree, and that's all. That is the design, not yet the measured truth: an external audit of
> v0.1.3 found four ways past that wall, and they are not fixed yet
> ([docs/audit.md](docs/audit.md)).

The design is in [docs/architecture.md](docs/architecture.md). The wire format between agent, microVM,
mothership and browser is in [docs/protocol.md](docs/protocol.md). Why any of this exists, and where
it's going, is in [docs/vision.md](docs/vision.md); what the design shows that the code has not built
yet is in [docs/gaps.md](docs/gaps.md). What has been decided against, and why, is in
[docs/decisions.md](docs/decisions.md). Knowing which version you run, and moving to a newer one without
losing colonies, is in [docs/updates.md](docs/updates.md). Which worktree paths a colony must never
read and which it cannot rewrite is in [docs/path-policy.md](docs/path-policy.md).
The remote-access review — security review of the relay and tunnel before deploy
([#536](https://github.com/Colonizer-dev/harness/issues/536)) — is in
[docs/remote-access-review.md](docs/remote-access-review.md).

---

## Run it

On Linux x86_64 with KVM, or an Apple Silicon Mac, with `git` and `gh`:

```sh
curl -fsSL https://colonizer.dev/install.sh | sh
colonizer               # prints a sign-in link and opens it (`colonizer open` reprints it)
```

That installs the latest [release](https://github.com/Colonizer-dev/harness/releases). To build from
source instead, you also need Node.js 20+ and Rust 1.88+:

```sh
git clone https://github.com/Colonizer-dev/harness
cd harness
scripts/install.sh      # builds everything into ./dist
dist/bin/colonizer
```

[docs/install.md](docs/install.md) has the rest: what a Linux machine needs for Claude Code, what the
installer does on a Mac, the install options, the first run, where things live and how to update. It is
also on [colonizer.dev/docs/install](https://colonizer.dev/docs/install). The binary takes a few
arguments, too: `colonizer --help` lists them — `telemetry show`, `telemetry on` and `telemetry off`
for the [usage data](docs/usage-data.md) switch, and `--version`.

---

## Two things, and which is which

| | What it is | Status |
| :--- | :--- | :--- |
| **Harness** | This repository: the mothership, the in-VM daemon, the agent modules, the web UI, the bundled mesh. Runnable today on your own machine. | `SHIPPING` |
| **Colonizer** | Anything beyond one machine: remote outposts, a fleet view, a hosted offering. | `PLANNED` |

Two labels are used everywhere below, and they set the tense of the sentence around them:

- `SHIPPING`: merged, in this repository, and exercised on a real machine.
- `PLANNED`: named, not specified, not started.

---

## A question, from inside a colony

Real, from a colony working on this repository. Unedited apart from line breaks.

```json
{"type": "question", "seq": 14,
 "question_id": "toolu_01Kz2S34mniQ6KJ476Twd2X3",
 "questions": [{
   "header": "README tweak", "multi_select": false,
   "question": "Which small README improvement do you prefer?",
   "options": [
     {"label": "Add a table of contents",
      "description": "Insert a short linked ToC near the top (Requirements, Install, Trust model, Configuration, Development, Run as a service) so readers can jump to a section in this fairly long README."},
     {"label": "Add a Quick Start block",
      "description": "Add a 3-line 'Quick Start' snippet right under the intro paragraph (clone, install.sh --install, open the URL) so skimmers get running before reading Requirements/Trust model/Configuration."}
   ]}]}
```

The web UI renders that as a card with both options and an **Other…** answer. One click sends
`question_answered` back through the mothership, over the mesh, to the agent that is waiting for it.

Agents never ask in plain text. The Claude Code module routes `AskUserQuestion` into this event, and if a
turn still ends on a plain-text question, the runner holds the turn open and has the agent ask again as
a card. Autopilot can't publish in the middle of a question.

---

## Shape

```mermaid
%%{init: {"theme":"base","themeVariables":{
  "background":"transparent",
  "fontFamily":"ui-monospace, SFMono-Regular, Menlo, monospace",
  "fontSize":"13px",
  "primaryColor":"#141821","primaryTextColor":"#EDEBE6","primaryBorderColor":"#3A3A3F",
  "lineColor":"#6E6E76","textColor":"#8A8A8E",
  "clusterBkg":"transparent","clusterBorder":"#3A3A3F",
  "edgeLabelBackground":"#0E121A"
}} }%%
flowchart LR
  U(["browser"]):::req --> M

  subgraph HOST["YOUR MACHINE · THE MOTHERSHIP"]
    M["<b>colonizer</b><br/>modules · colonies · publish"]:::core
    HS["headscale<br/>bundled control plane"]:::mod
    TS["tailscaled<br/>userspace node"]:::mod
    M --> HS & TS
  end

  subgraph C["ONE TASK · ONE MICROVM · ONE WORKTREE"]
    AD["<b>colonizer-agentd</b><br/>events · terminals"]:::port
    AG["agent runner<br/>Claude Code"]:::mod
    WT[("/workspace<br/>git worktree")]:::vendor
    AD --> AG --> WT
  end

  TS == "private mesh" ==> AD
  M --> GH["GitHub<br/>issues · pull requests"]:::vendor
  AG -. "placeholder, swapped at the edge" .-> API["api.anthropic.com"]:::vendor
  C2["another colony"]:::ghost
  TS -.-> C2

  classDef req fill:#0E121A,stroke:#FF6B35,stroke-width:1.5px,color:#EDEBE6
  classDef core fill:#141821,stroke:#FF6B35,stroke-width:1.5px,color:#EDEBE6
  classDef mod fill:#0E121A,stroke:#3A3A3F,color:#EDEBE6
  classDef port fill:#141821,stroke:#EDEBE6,stroke-width:1.5px,color:#EDEBE6
  classDef vendor fill:#0E121A,stroke:#3A3A3F,color:#A9A8A5
  classDef ghost fill:transparent,stroke:#55555A,stroke-dasharray:4 3,color:#8A8A8E
```

---

## The argument, in five points

**1. A colony is a machine, not a container.** Each task runs in a KVM microVM
([microsandbox](https://microsandbox.dev), libkrun) with its own kernel. The agent runs without
permission prompts because there is nothing on the other side of the wall worth protecting.

**2. Secrets stay home.** Git objects are mounted read-only, so the agent can read history but not
rewrite it. The mothership commits, pushes and opens the pull request after the microVM is gone, and it
treats everything the colony left behind as untrusted: `.git` is rewritten, nested repositories are
removed, and git runs with hooks and fsmonitor disabled.

**3. Every colony is one hop away.** The mothership runs its own Headscale and a userspace `tailscaled`,
both bundled. Every colony joins with a single-use key. The network is separate from any tailnet the
machine is already on. The mothership can reach colonies, and colonies can't reach each other. One narrow
UDP rule per colony keeps WireGuard direct (about 1 ms) instead of relayed.

**4. Decisions, not prose.** Agents bring you choices. Your job is to pick one, not to parse a paragraph
that ends in a question mark.

**5. Everything is a module.** Source, sandbox, mesh, agent, interfaces and publish are providers behind
small contracts, selected in the UI and saved in `modules.json`. The agent contract is a JSON Lines
protocol on stdio, so an agent module can be written in anything.

The agent modules so far. An org can pick which installed agent module its colonies launch on (Org
settings → Agent module; without a pick, the mothership's agent choice applies):

| Agent | What it is | Status |
| :--- | :--- | :--- |
| [`claude-code`](modules/agents/claude-code) | Anthropic's Claude Code via the Claude Agent SDK, with questions to the user as choice cards | `SHIPPING` |
| [`codex`](modules/agents/codex) | OpenAI's Codex CLI, headless: one `codex exec` process per turn, resumed into a single thread; the `codex` CLI must be present in the colony image | `SHIPPING` |
| [`grok-build`](modules/agents/grok-build) | xAI's Grok Build CLI, headless: one grok process per turn, resumed into a single session | `PLANNED` |

---

## What changed

[CHANGELOG.md](CHANGELOG.md) covers every release, newest first, with the pull
request behind each line and anything that could cost work called out. The
mothership also tells you when a newer release is out, and can install it.

## What's in the repository

| Path | What it is | Status |
| :--- | :--- | :--- |
| [`crates/colonizer`](crates/colonizer) | The mothership: HTTP and WebSocket API, module registry, colony lifecycle, mesh supervision, publish | `SHIPPING` |
| [`crates/colonizer-agentd`](crates/colonizer-agentd) | The daemon inside every colony: runner supervision, event log with replay, PTY terminals. Static musl binary | `SHIPPING` |
| [`modules/agents/claude-code`](modules/agents/claude-code) | Claude Code through the Claude Agent SDK, speaking the runner protocol | `SHIPPING` |
| [`modules/agents/opencode`](modules/agents/opencode) | OpenCode through `opencode run`, speaking the runner protocol | `SHIPPING` |
| [`modules/agents/pi`](modules/agents/pi) | Pi through its RPC mode, speaking the runner protocol; models only through the provider gateway | `SHIPPING` |
| [`modules/agents/hermes`](modules/agents/hermes) | Nous Research's Hermes Agent CLI, driven headlessly on the same runner protocol | runner in-tree; not yet exercised in a colony — the `hermes` binary is not staged into the VM |
| [`modules/agents/codex`](modules/agents/codex) | OpenAI's Codex CLI driven headlessly on the same runner protocol; the `codex` CLI must be present in the colony image | `SHIPPING` |
| [`web`](web) | The UI: colonies, chat on [assistant-ui](https://www.assistant-ui.com), choice cards, [xterm.js](https://xtermjs.org) terminal, settings | `SHIPPING` |
| [`vendor`](vendor) | Pinned, sha256-verified microsandbox, Headscale and Tailscale, a DERP map snapshot, and the pin for the guest Claude Code build (`claude-code.lock`) with a snapshot of its built-in subagents (`claude-code-builtins.json`) | `SHIPPING` |
| [`scripts`](scripts) | `install.sh`, vendoring, the in-microVM agentd build and, on a Mac, the mesh's tailscaled | `SHIPPING` |

## Modules

| Kind | Providers today | Next |
| :--- | :--- | :--- |
| `source` | GitHub issues and repositories | GitLab, Linear, Jira `PLANNED` |
| `sandbox` | microsandbox (KVM microVMs), with the stack detected from each repository by default — or presets for Node, Python, Rust and Go picked by hand — each image pinned by digest | other VMMs `PLANNED` |
| `mesh` | Private mesh (bundled Headscale), or a loopback port | remote outposts `PLANNED` |
| `agent` | Claude Code or OpenCode, each able to run on any Anthropic-compatible provider (DeepSeek, a local model); Pi, reaching models only through the provider gateway; Hermes as an in-tree module whose colonies stop at the runner's preflight until the `hermes` CLI is staged into the VM | more agents behind the same protocol `PLANNED` |
| `interfaces` | Chat with choice cards, terminal | dev-server previews `PLANNED` |
| `publish` | GitHub pull request from the colony's own branch, opened automatically when the agent finishes (autopilot, on by default) | review-comment follow-ups `PLANNED` |
| `memory` | Shared notes per repository, org and globally; agents propose, you approve. Kept on the mothership, or in your [mem0](https://mem0.ai) project with each colony's index ordered by relevance to its task | semantic search inside a colony `PLANNED` |
| `watchdog` | Nudges colonies that stop making progress, flags the ones that need you | automatic restarts `PLANNED` |
| `autonomy` | Off, or a judge model that answers a colony's questions when nobody does — choosing only among the options the agent offered | judging its own answers `PLANNED` |
| `notify` | A desktop notification or a webhook when a colony asks a question, stalls, fails or opens a pull request, or when a model provider starts failing. Off until configured, and the webhook carries no repository content — the event, the time, and the colony or provider counters behind it | Slack or email relays `PLANNED` |
| `burn_down` | Spends a weekly token plan before it resets: launches bug-hunt colonies paced across the window down to a reserve, then stops. Off until configured ([docs/burn-down.md](docs/burn-down.md)) | — |

Each GitHub org the signed-in account belongs to can be a workspace with its own overrides for models, the
parallel limit, the per-colony budget and host-disk quota, the sandbox stack, memory, the watchdog and
notifications. An org is offered the first time the account shows it — you choose which become workspaces;
a first install adopts the ones it already had ([#176](https://github.com/Colonizer-dev/harness/issues/176)).
Model providers (DeepSeek, a server on your LAN or tailnet, any Anthropic-compatible endpoint)
are added in Settings. Colonies reach them through the mothership's provider gateway, which holds the
keys, queues requests for servers that handle one at a time, allows slow prefill, and falls back to
Claude when a provider is down or busy.

One thing worth knowing before you point a provider at a model setting: the orchestrator model does
nearly all of the work. The subagent setting only carries traffic when a colony delegates to a
subagent, and colonies rarely do — four recent colonies of 413 to 2 095 events spawned 0, 0, 0 and 1
between them — and the background setting carries only small auxiliary calls. A provider wired to just
those two is configured correctly and will still look idle. Pi has no subagents, so its single model
setting carries all of its traffic. To put real traffic on your own hardware,
point the orchestrator model at it. The full breakdown is in the
[Claude Code module](modules/agents/claude-code/README.md).

---

## What this does not do

Stated here rather than buried.

- **One machine.** Colonies run on the host that launched them: Linux x86_64 with KVM, or an Apple
  Silicon Mac — where the bundled `tailscaled` is built from pinned source, because Tailscale
  publishes no macOS build of it.
- **Two agents, one forge.** Claude Code and Pi are the agent modules; GitHub the only source and publisher.
- **The cockpit needs its per-install token.** Startup prints a sign-in link and opens it
  (`colonizer open` reprints it later; `COLONIZER_NO_BROWSER=1` skips the auto-open). The token is
  kept in `~/.config/colonizer/api-token`. The server binds to `127.0.0.1`, checks `Host` and
  `Origin` headers, and should stay there.
- **Colony images need glibc.** A Linux Claude Code binary is mounted read-only into the microVM: the
  host's own on Linux, the `linux-arm64` build fetched at install time on a Mac.
- **Relays are Tailscale's.** Direct connections don't need them; when a colony falls back to a relay,
  encrypted traffic crosses Tailscale's public DERP servers.
- **`install.sh --install` is not exercised yet.** It is implemented, but it hasn't been run against the
  real world. Colonies opening pull requests has been.
- **Cross-provider subagents are off the beaten path.** Anthropic doesn't support routing Claude Code to
  non-Claude models. Routing and the gateway are tested with stub Anthropic-compatible providers inside
  real colonies and against a local `ds4-server` on the operator's tailnet, not against DeepSeek's hosted
  API, and Claude-specific request fields are forwarded as they are. The OpenAI translation (the `openai`
  wire) is exercised against real Claude Code and a stub gateway, not against OpenAI's hosted API.
- **ChatGPT subscriptions are not a credential.** OpenAI-compatible providers take an API key: a ChatGPT
  plan is honoured by the Responses API behind Codex sign-in, which the gateway's `openai` wire does not
  speak. The [`codex` agent module](modules/agents/codex) runs on an OpenAI API key instead — a `CODEX_API_KEY`
  colony secret for `api.openai.com` — but a ChatGPT sign-in is still nothing the harness can spend
  ([#30](https://github.com/Colonizer-dev/harness/issues/30), [docs/decisions.md](docs/decisions.md)).
- **Memory search inside a colony is plain text matching.** With the mem0 provider, a colony's `MEMORY.md`
  is ordered by mem0's relevance to the task, but `memory_search` still matches words in the notes it was
  given. mem0's Platform API is supported; self-hosted mem0 serves a different API and is not.
- **Not ready for unattended work on sensitive repositories.** That is the v0.1.3 audit's verdict,
  real credentials included. It found four ways a colony could cross into the host, filed as draft
  security advisories and not fixed yet ([docs/audit.md](docs/audit.md)).
- **The crates are source, not an install.** `colonizer-harness` and `colonizer-agentd` are on
  crates.io, but `cargo install colonizer-harness` gives only the `colonizer` binary, without
  microsandbox, the in-VM daemon, the agent modules and the web UI beside it — use the installer.
  Nothing is published to npm.
- **CI runs every suite, including one that boots a real colony.** The Rust tests and clippy, the
  runner's, the live map receiver's and the web UI's all run on every pull request; releases are
  built, smoke-tested and attested with build provenance; dependency audits and SBOMs run with every
  change and on a weekly schedule; runtime pins move only by reviewed pull request. GitHub-hosted
  runners do have `/dev/kvm` (the job makes it usable), so the `colony-e2e` job also boots a whole
  colony end to end — mothership, microVM, agentd and the Claude Code runner against a scratch
  repository and a stub model server, asserting it reaches `no_changes`. What that still does not
  cover is a real model or a real GitHub write ([roadmap](#roadmap-in-public)). The crates are
  published to crates.io through Trusted Publishing; nothing is published to npm.

---

## Roadmap, in public

| Capability | Status |
| :--- | :--- |
| Colonies, private mesh, choice cards, terminal, Claude Code module, GitHub source and publish | `SHIPPING` |
| Orchestrator and subagents on different providers ([#1](https://github.com/Colonizer-dev/harness/issues/1)) | `SHIPPING` |
| Org workspaces ([#2](https://github.com/Colonizer-dev/harness/issues/2)) | `SHIPPING` |
| Shared memory with review ([#3](https://github.com/Colonizer-dev/harness/issues/3)) | `SHIPPING` |
| Watchdog for stalled colonies ([#4](https://github.com/Colonizer-dev/harness/issues/4)) | `SHIPPING` |
| Provider gateway: private-network models, queues, long timeouts, health, Claude fallback ([#5](https://github.com/Colonizer-dev/harness/issues/5)) | `SHIPPING` |
| CI running the Rust, runner and UI test suites | `PLANNED` |
| Local Claude Code plugins mounted read-only into colonies, with ECC's skills and agents vendored ([#6](https://github.com/Colonizer-dev/harness/issues/6)) | `SHIPPING` |
| Skillsets switched on and off in Settings, globally and per org ([#46](https://github.com/Colonizer-dev/harness/issues/46)) | `SHIPPING` |
| superpowers vendored, with its bootstrap in the system prompt instead of a hook ([#44](https://github.com/Colonizer-dev/harness/issues/44)) | `SHIPPING` |
| Google's skills vendored and loaded on demand from a pinned local catalog ([#43](https://github.com/Colonizer-dev/harness/issues/43)) | `SHIPPING` |
| Daily proposals for vendored plugin updates, described in skills added, removed and changed ([#43](https://github.com/Colonizer-dev/harness/issues/43)) | `SHIPPING` |
| Token savings: terse replies (caveman) and compact command output (rtk), each a switch | `SHIPPING` |
| Token savings: Headroom compacting tool results, its bundle downloaded when switched on ([#53](https://github.com/Colonizer-dev/harness/issues/53)) | `SHIPPING` |
| Live map of motherships, off until you switch it on: the heartbeat and its receiver | `SHIPPING` |
| Release provenance: every release artifact attested, colony images and the guest agent pinned, SBOMs and dependency audits, pins proposed by pull request ([#89](https://github.com/Colonizer-dev/harness/issues/89)) | `SHIPPING` |
| More agent modules behind the runner protocol | `PLANNED` |
| GitLab, Linear and Jira sources; review comments as follow-up tasks | `PLANNED` |
| Remote outposts: other machines joining the mesh to host colonies | `PLANNED` |
| Per-colony budgets and host-disk quotas ([#86](https://github.com/Colonizer-dev/harness/issues/86)) | `SHIPPING` |
| Red-team raids: hunters with distinct briefs raiding one repo while the nest is empty ([docs/red-team.md](docs/red-team.md), [#212](https://github.com/Colonizer-dev/harness/issues/212)) | `SHIPPING` |
| Burn-down mode: weekly token plan spent to a reserve by paced bug-hunt colonies ([docs/burn-down.md](docs/burn-down.md), [#210](https://github.com/Colonizer-dev/harness/issues/210)) | `SHIPPING` |
| Fleet view and network policies | `PLANNED` |
| Dev-server previews over the mesh | `PLANNED` |

The roadmap is the issue tracker. There is no private version of it. On top of it, the v0.1.3 audit
sets four release checkpoints ([docs/audit.md](docs/audit.md)).

---

## Trust model

| What | Where it lives |
| :--- | :--- |
| GitHub token | Mothership only. Commit, push and `gh pr create` run on the host after the colony is gone. |
| Claude token | Mothership only (system keychain, or a 0600 file). The colony sees a placeholder; microsandbox's TLS proxy substitutes the real value for `api.anthropic.com` only. |
| Model provider keys | Mothership only (system keychain, or a 0600 file). Colonies send provider requests to the gateway with a per-colony token; the gateway adds the key. |
| Worktree | Mounted read-write at `/workspace`. |
| Colony secrets you name | Mothership only; microsandbox substitutes the value on TLS to the hosts you allowed, so the colony sees a placeholder. |
| Git objects and worktree metadata | Mounted read-only: `git status`, `diff` and `log` work in the colony, commits don't. |
| Colony output | Untrusted until published: `.git` rewritten, nested `.git` removed, no hooks or fsmonitor, `pr.md` must be a regular file. |
| Prompt screening (screen module) | Off until you configure it. When on, it reads the colony's diff and PR body at publish time, classifies hidden code points, and holds (`block`) or annotates (`warn`) the publish. It sees colony output, holds no credentials, and sends nothing anywhere — no network, no model ([docs/prompt-screening.md](docs/prompt-screening.md)). |
| What a colony runs | Pinned, not floating: the image by OCI digest (`crates/colonizer/images.lock`), the guest Claude Code build by sha256 (`vendor/claude-code.lock`), the vendored tools by sha256 (`vendor/vendor.lock`). Pins move only through a reviewed pull request. |
| Release downloads | Checked against the release's `SHA256SUMS`, which itself carries a build-provenance attestation the installer verifies whenever `gh` can reach a verdict ([docs/install.md](docs/install.md)). |
| Mesh | Own Headscale and userspace `tailscaled`, own state and socket, `--no-logs-no-support`. Mothership reaches colonies; colonies can't reach each other. |
| colonizer-agentd | Per-colony bearer token, even inside the mesh. |
| Live map | Off until you switch it on. When on, a heartbeat every 5 minutes: a random id, version, platform and colony count. No code, repositories or names ([docs/telemetry.md](docs/telemetry.md)). |
| Usage data | On by default: an anonymous batch of counts, built and shown locally — and nothing is sent at all in this release. A different random id from the live map's; `colonizer telemetry off` switches it off ([docs/usage-data.md](docs/usage-data.md)). |

Colonies are detached: they keep running when the mothership restarts, and it reconnects to them.

An external audit read this table against the code at v0.1.3. What it confirmed, what it found
instead, and what has to be true before unattended work: [docs/audit.md](docs/audit.md).

## Configuration

Module settings live in `~/.config/colonizer/modules.json` and are edited in the UI. The answers to
the [live map](docs/telemetry.md) and [usage data](docs/usage-data.md) questions live beside it, in
`telemetry.json` and `usage.json`, and `usage-last.json` beside those keeps the last usage batch
built. Process settings come from the environment:

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
| `COLONIZER_APP` | `~/.local/share/colonizer/app` | The symlink an install moves; what an [update](docs/updates.md) follows |
| `COLONIZER_UPDATE_CHECK` | on | `0` keeps the update check off whatever Settings says — then no request is made at all |
| `COLONIZER_RELEASES_URL` | GitHub's latest release for this repo | Where the update check looks |
| `DO_NOT_TRACK`, `COLONIZER_TELEMETRY=off` | – | Keep the [live map](docs/telemetry.md) and [usage data](docs/usage-data.md) off whatever Settings says |
| `CI=true` | – | Also keeps [usage data](docs/usage-data.md) off; the live map does not read it |
| `COLONIZER_TELEMETRY_URL` | `https://telemetry.colonizer.dev` | Where live map heartbeats go |
| `COLONIZER_MASTER_KEY` | – (secrets saved in plaintext, 0600) | Encrypts the secrets the mothership saves, at rest; see below |

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

When the macOS Keychain or the Linux Secret Service answers a startup probe, newly saved secrets go
there instead of to files (existing files stay until you move them on the cockpit's Secrets page, which
also shows where each one lives). On macOS the Keychain ties an item to the binary that wrote it, so
build with `COLONIZER_CODESIGN_IDENTITY` set (see `scripts/install.sh`) to keep access across rebuilds.

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

An org's own budget or quota beats the sandbox default, and an org set to `0` opts out of a global limit. Routed
providers need `pricing` — dollars per million tokens for input, output, cached read, cache write and
thinking — to count toward the budget; an unpriced provider still counts its tokens but contributes $0,
so a colony that spends only through one never reaches `budget_usd` and is never stopped for spend —
`budget_tokens` is what holds it.

Every colony-scoped row of the spend journal (`<data>/spend.jsonl`) names the colony (`session`) and the
agent module that ran it (`agent`), and splits its spend by who measured it: the agent's own turn-end
estimate (first-party traffic never passes the gateway) against the gateway's metered price for routed
providers. `node scripts/colony-report.mjs --costs` reads the journal back grouped per colony and per
harness × model, by default over the same last-30-days window the spend history answers
([docs/protocol.md](docs/protocol.md) §6.8).

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

## Development

```sh
cargo test --workspace                          # mothership and agentd, including agentd's no-KVM smoke test
cargo clippy --workspace --all-targets -- -D warnings
(cd modules/agents/claude-code && npm test)
(cd modules/agents/pi && npm test)
(cd modules/agents/codex && npm test)
(cd services/telemetry && npm test)             # the live map's receiver
(cd web && npm run build && npm test)           # tsc, vite, and the UI's own tests
node --test scripts/test/colony-report.test.mjs
node scripts/colony-report.mjs                  # how colonies went, from what they already log
node scripts/colony-e2e.mjs                     # boots a real colony against a stub model (CI's colony-e2e job)
node scripts/colony-report.mjs --transcript <id> # one colony, step by step
node scripts/colony-report.mjs --costs          # spend.jsonl by colony, last 30 days: estimated vs metered, harness × model
node --test scripts/test/bench.test.mjs
node scripts/bench.mjs run --repo owner/bench --label before   # the fixed tasks, scored (docs/bench.md)
node scripts/trajectory-monitor.mjs --session <id>  # post-hoc: was a resolved colony clean? (docs/trajectory-monitor.md)
sh scripts/test/build-scripts.test.sh           # the build scripts: here mode refused off Linux, no non-ELF artefact installed or served
sh scripts/test/install-release.test.sh         # the installer: an install interrupted at any point leaves a working colonizer, and the next one recovers
(cd web && npm run dev)                         # UI dev server; proxies /api to 127.0.0.1:7878
# http://127.0.0.1:5173/?mock=1                 # the UI against an in-browser mock backend
node scripts/require-ci-checks.mjs             # dry run: the ruleset that makes CI required on main
node scripts/require-ci-checks.mjs --apply     # send it (idempotent; needs a repository-admin token)
```

CI is meant to gate merges on `main`: the ruleset that makes the six always-running CI jobs required
checks is printed by the dry run above, and [docs/audit.md](docs/audit.md) ("Required checks, issue
#367") says which checks those are, which are deliberately left optional, and the two repository
settings that travel with them.

Vendor logos in the UI are CC0 artwork from Simple Icons; the marks stay their owners' trademarks. See [NOTICE](NOTICE).

<p align="center">
  <br>
  <a href="https://colonizer.dev"><b>colonizer.dev</b></a>
  &nbsp;·&nbsp;
  <a href="docs/vision.md">Vision</a>
  &nbsp;·&nbsp;
  <a href="docs/architecture.md">Architecture</a>
  &nbsp;·&nbsp;
  <a href="docs/protocol.md">Protocol</a>
  &nbsp;·&nbsp;
  <a href="docs/updates.md">Updates</a>
  &nbsp;·&nbsp;
  <a href="docs/audit.md">Audit</a>
  &nbsp;·&nbsp;
  <a href="docs/boundaries.md">Boundaries</a>
  &nbsp;·&nbsp;
  <a href="docs/gaps.md">Gaps</a>
  &nbsp;·&nbsp;
  <a href="docs/runner-authoring.md">Runner authoring</a>
</p>

<p align="center">
  <sub>MIT license · Colonize your backlog.</sub>
</p>
