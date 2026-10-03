# Boundary vs guidance

Everything the harness does to keep a colony in line is one of two things. A **boundary** is
enforced by the kernel or the host: it holds whatever the agent does or says, because the agent
takes no part in it. **Guidance** is the rest — prompt text, skill packs, denial hints, nudges —
that translates a denial into a next action. Guidance never changes a return code, and removing it
weakens nothing. The split is a reviewer's first question: a control claimed as a boundary that
turns out to be guidance is a finding.

## Boundaries

| Enforcement | Where it lives |
| :--- | :--- |
| One microVM per colony: a KVM microVM with its own kernel, so the agent runs without permission prompts — there is nothing on the other side of the wall worth protecting | `crates/colonizer/src/sandbox.rs:46-49` (`boot`: `msb run --detach --replace`) |
| Read-only mounts: bare repository, agent binary, plugins and the rest mount `ro`; only the worktree and `/harness/out` are writable | declared in `crates/colonizer/src/boot.rs` (`read_only: true`), passed as `-v …` in `crates/colonizer/src/sandbox.rs:57,113` |
| Placeholder credentials: secret values stay in msb's host process; the guest env holds a placeholder, swapped on TLS to the hosts the secret names — and never otherwise | `crates/colonizer/src/sandbox.rs:57-61`; host-side storage in `crates/colonizer/src/secrets.rs` |
| Egress network rules: the sandbox `egress` setting picks `open` (the default: the `public` profile) or `allowlist` (no profile allow, only what the harness and the operator name), plus port-scoped `allow@host:tcp:` rules for the harness's own ports; a fixed deny set (network-internal destinations, cloud metadata) comes first and no setting can reopen it; the default deny closes every other host-loopback port | `crates/colonizer/src/sandbox.rs:68-76`, rules compiled in `crates/colonizer/src/egress.rs` and assembled in `crates/colonizer/src/boot.rs` (`colony_network`); the whole policy in [sandbox-network.md](sandbox-network.md) |
| Per-agent egress declaration: every `module.json` declares the hosts its agent may reach (`egress`), and every `secrets[].hosts` entry must be covered by that union; in `allowlist` mode the running module's `api`, `auth` and `extra` hosts join the colony's allow list (`telemetry` excluded — an operator lists those in `egress_allow` themselves), while the always-blocked deny set and the operator's `egress_block` still compile ahead of every allow | parsed in `crates/colonizer/src/modules.rs`, folded into the policy by `crates/colonizer/src/egress.rs` (`resolve`), walked over `modules/agents/*/module.json` by a test; [sandbox-network.md](sandbox-network.md#hosts-an-agent-module-declares) |
| Mesh ACLs: Headscale allows `harness@ → vms@:*` and nothing else, so colonies cannot reach each other; VM keys are single-use pre-auth keys, nodes deleted at session end | `crates/colonizer/src/mesh.rs:299-301`; keys at `crates/colonizer/src/mesh.rs:331-362` |
| Publish treats colony output as untrusted: `.git` rewritten from the value recorded before the VM ran, nested `.git` stripped, `pr.md` a regular file; own-prefix branch only, no force-push | `crates/colonizer/src/github.rs:1920-2002` (`publish`, `restore_gitfile`, `strip_nested_git`); the commit/push/PR gates in `crates/colonizer/src/publish.rs`, grants in `crates/colonizer/src/authority.rs` |
| Auth: per-colony tokens at the provider gateway, the per-install token on every cockpit `/api` route | `crates/colonizer/src/gateway/mod.rs`, `crates/colonizer/src/auth.rs` |
| Delegation gate: `COLONIZER_DELEGATE=enforce` refuses every tool but plan, ask and delegate, and the mothership re-checks a memory proposal's `origin` before touching a store | hook in `modules/agents/claude-code/runner.mjs:416-570`; the host half in `crates/colonizer/src/memory.rs` (`role_of`). The hook's wall is the Agent SDK, not the kernel — the host-side re-check is the hard half |
| In-guest hardening: `boot.sh` hides kernel interfaces and remounts `/proc` with `hidepid`; agentd drops 21 capabilities, sets `no_new_privs` and no core dumps, and installs a seccomp denylist on the agent process and everything it starts (the human's terminal is not filtered). Codex's own landlock/seccomp is deliberately left off (`modules/agents/codex/runner.mjs:106`) — agentd's filter covers it | `crates/colonizer/src/boot.rs`, `crates/colonizer-agentd/src/harden.rs` (applied in `crates/colonizer-agentd/src/runner.rs:99`); details in [architecture.md](architecture.md#in-guest-hardening). Landlock is **not yet**: the pinned libkrunfw kernel is built without it |

## Guidance

| Guidance | Where it lives |
| :--- | :--- |
| System-prompt text: rules for questions, limits, memory, findings and delegation, appended at startup | `modules/agents/claude-code/runner.mjs:24` (`SYSTEM_PROMPT_APPEND`), assembled at `runner.mjs:429-471` |
| Skill packs: vendored skills and tool servers, switched on per org | [skill-packs.md](skill-packs.md). The read-only mount is a boundary; the `SKILL.md` text is guidance |
| Denial hints: `classifyDenial(text)` → `{class, hint}` (`egress`, `read_only`, `tool_disabled`); on an errored tool result the runner adds `denial: {class, hint}` to the `tool_result` event, and a `PostToolUseFailure` hook repeats the hint to the agent as `additionalContext` — mid-turn, bound to the failed call, so it costs no extra turn — at most once per class per session. `is_error` and content are unchanged, the hook returns no decision, and a strip test proves the events are identical without the layer apart from `denial` | `modules/agents/claude-code/denials.mjs`, hook in `modules/agents/claude-code/runner.mjs:571-592` |
| Watchdog nudges: `decide()` nudges a colony with no progress and flags it after `max_nudges`; gateway traffic counts as progress | `crates/colonizer/src/watchdog.rs` (`decide`, `nudge_text`); the busy check is `gateway.colony_busy`, called at `watchdog.rs:159` |
| Autonomy judge: answers a colony's question when nobody does, choosing only among the options the agent offered, capped by risk class | `crates/colonizer/src/autonomy.rs` |
| Choice-card re-ask: a turn that ends on a plain-text question is held open and the agent asks again as a card | `modules/agents/claude-code/runner.mjs` (turn handling) |

## Classifying a finding

- **Hit a wall.** The boundary held: an errored tool result carrying `denial` — an egress deny, a
  read-only mount, a refused tool. The colony works around it or asks. At worst this is a guidance
  gap (the hint was unhelpful), never a security finding.
- **Defeated a control.** The boundary failed: an event shows the publish rewrite, a secret swap, a
  mount or a gate crossed. Stop the colony and flag it; this is a security finding, and it goes to
  [audit.md](audit.md) with a negative test on a real colony.

The audit record and the watchdog key on the same split: a guidance gap is work on the hints; a
crossed boundary is a stop.

## Watchdog signatures (planned)

Coordinating the three signatures below is **not implemented yet**; the watchdog today is
`decide()` in `crates/colonizer/src/watchdog.rs` over its activity feed, and only its stall path
exists. The proposal, so the denial layer has something to feed:

- **Hint-loop** — consecutive errored `tool_result` events carrying `denial`, with no successful
  non-denied tool result in between, counts as *not progress*: take the existing nudge path, and
  say what was denied in the nudge.
- **Genuine stall** — no events at all: the existing nudge-then-flag behaviour, unchanged.
- **Control-defeat** — a boundary signal, such as an audit, publish-rewrite or sandbox event
  showing a control was bypassed: stop the colony and flag it at once, without waiting for
  `max_nudges`.
