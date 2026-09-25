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
| One microVM per colony: a KVM microVM with its own kernel, so the agent runs without permission prompts — there is nothing on the other side of the wall worth protecting | `crates/colonizer/src/sandbox.rs:43,62-67` (`msb run --detach --replace`) |
| Read-only mounts: bare repository, agent binary, plugins and the rest mount `ro`; only the worktree and `/harness/out` are writable | declared in `crates/colonizer/src/boot.rs` (`read_only: true`), passed as `-v …` in `crates/colonizer/src/sandbox.rs:52,96` |
| Placeholder credentials: secret values stay in msb's host process; the guest env holds a placeholder, swapped on TLS to the hosts the secret names — and never otherwise | `crates/colonizer/src/sandbox.rs:57-61`; host-side storage in `crates/colonizer/src/secrets.rs` |
| Egress network rules: the `public` profile plus port-scoped `allow@host:tcp:` rules; the default deny closes every other host-loopback port | `crates/colonizer/src/sandbox.rs:62-67`, rules built in `crates/colonizer/src/sessions.rs`; the whole policy in [sandbox-network.md](sandbox-network.md) |
| Per-agent egress declaration: every `module.json` declares the hosts its agent may reach (`egress`), and every `secrets[].hosts` entry must be covered by that union | parsed in `crates/colonizer/src/modules.rs`, walked over `modules/agents/*/module.json` by a test. Enforcement is not wired: deriving the colony's allowlist from the declaration plus the task's providers is a separate egress-policy issue — **planned** |
| Mesh ACLs: Headscale allows `harness@ → vms@:*` and nothing else, so colonies cannot reach each other; VM keys are single-use pre-auth keys, nodes deleted at session end | `crates/colonizer/src/mesh.rs:301`; keys at `crates/colonizer/src/mesh.rs:352-362` |
| Publish treats colony output as untrusted: `.git` rewritten from the value recorded before the VM ran, nested `.git` stripped, `pr.md` a regular file; own-prefix branch only, no force-push | `crates/colonizer/src/github.rs:1704-1762`; the commit/push/PR gates in `crates/colonizer/src/publish.rs`, grants in `crates/colonizer/src/authority.rs` |
| Auth: per-colony tokens at the provider gateway, the per-install token on every cockpit `/api` route | `crates/colonizer/src/gateway.rs`, `crates/colonizer/src/auth.rs` |
| Delegation gate: `COLONIZER_DELEGATE=enforce` refuses every tool but plan, ask and delegate, and the mothership re-checks a memory proposal's `origin` before touching a store | hook in `modules/agents/claude-code/runner.mjs:386-539`; the host half in `crates/colonizer/src/memory.rs` (`role_of`). The hook's wall is the Agent SDK, not the kernel — the host-side re-check is the hard half |
| seccomp / Landlock: none in the harness. Codex's own landlock/seccomp is deliberately left off — the microVM is the boundary (`modules/agents/codex/runner.mjs:106`) | **planned**: once landed it would sit in this table |

## Guidance

| Guidance | Where it lives |
| :--- | :--- |
| System-prompt text: rules for questions, limits, memory, findings and delegation, appended at startup | `modules/agents/claude-code/runner.mjs:23` (`SYSTEM_PROMPT_APPEND`), assembled at `runner.mjs:398-406` |
| Skill packs: vendored skills and tool servers, switched on per org | [skill-packs.md](skill-packs.md). The read-only mount is a boundary; the `SKILL.md` text is guidance |
| Denial hints: `classifyDenial(text)` → `{class, hint}` (`egress`, `read_only`, `tool_disabled`); on an errored tool result the runner adds `denial: {class, hint}` to the `tool_result` event, and a `PostToolUseFailure` hook repeats the hint to the agent as `additionalContext` — mid-turn, bound to the failed call, so it costs no extra turn — at most once per class per session. `is_error` and content are unchanged, the hook returns no decision, and a strip test proves the events are identical without the layer apart from `denial` | `modules/agents/claude-code/denials.mjs`, hook in `modules/agents/claude-code/runner.mjs:540-562` |
| Watchdog nudges: `decide()` nudges a colony with no progress and flags it after `max_nudges`; gateway traffic counts as progress | `crates/colonizer/src/watchdog.rs` (`decide`, `nudge_text`; `colony_busy` at line 156) |
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

Coordinating the three signatures below is **not implemented in this PR**; the watchdog today is
`decide()` in `crates/colonizer/src/watchdog.rs` over its activity feed, and only its stall path
exists. The proposal, so the denial layer has something to feed:

- **Hint-loop** — consecutive errored `tool_result` events carrying `denial`, with no successful
  non-denied tool result in between, counts as *not progress*: take the existing nudge path, and
  say what was denied in the nudge.
- **Genuine stall** — no events at all: the existing nudge-then-flag behaviour, unchanged.
- **Control-defeat** — a boundary signal, such as an audit, publish-rewrite or sandbox event
  showing a control was bypassed: stop the colony and flag it at once, without waiting for
  `max_nudges`.
