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
| One microVM per colony: a KVM microVM with its own kernel, so the agent runs without permission prompts — there is nothing on the other side of the wall worth protecting | `crates/colonizer/src/sandbox.rs:47-49` (`boot`: `msb run --detach --replace`) |
| Read-only mounts: bare repository, agent binary, plugins and the rest mount `ro`; only the worktree and `/harness/out` are writable | declared in `crates/colonizer/src/boot.rs` (`read_only: true`), passed as `-v …` in `crates/colonizer/src/sandbox.rs:57,148` |
| Placeholder credentials: secret values stay in msb's host process; the guest env holds a placeholder, swapped on TLS to the hosts the secret names — and never otherwise | `crates/colonizer/src/sandbox.rs:62-67`; host-side storage in `crates/colonizer/src/secrets.rs` |
| Egress network rules: the sandbox `egress` setting picks `open` (the default: the `public` profile) or `allowlist` (no profile allow, only what the harness and the operator name), plus port-scoped `allow@host:tcp:` rules for the harness's own ports; a fixed deny set (network-internal destinations, cloud metadata) comes first and no setting can reopen it; the default deny closes every other host-loopback port | `crates/colonizer/src/sandbox.rs:68-76`, rules compiled in `crates/colonizer/src/egress.rs` and assembled in `crates/colonizer/src/boot.rs` (`colony_network`); the whole policy in [sandbox-network.md](sandbox-network.md) |
| Per-agent egress declaration: every `module.json` declares the hosts its agent may reach (`egress`), and every `secrets[].hosts` entry must be covered by that union; in `allowlist` mode the running module's `api`, `auth` and `extra` hosts join the colony's allow list (`telemetry` excluded — an operator lists those in `egress_allow` themselves), while the always-blocked deny set and the operator's `egress_block` still compile ahead of every allow | parsed in `crates/colonizer/src/modules.rs`, folded into the policy by `crates/colonizer/src/egress.rs` (`resolve`), walked over `modules/agents/*/module.json` by a test; [sandbox-network.md](sandbox-network.md#hosts-an-agent-module-declares) |
| Mesh ACLs: Headscale allows `harness@ → vms@:*`, plus `fleet@ → harness@:*` only while the fleet has members, and nothing else, so colonies cannot reach each other and fleet members cannot reach colonies; VM keys are single-use pre-auth keys, nodes deleted at session end | `crates/colonizer/src/mesh.rs:539-553` (`policy_json`); keys at `crates/colonizer/src/mesh.rs:374-405` |
| Publish treats colony output as untrusted: `.git` rewritten from the value recorded before the VM ran, nested `.git` stripped, `pr.md` a regular file; own-prefix branch only, no force-push | `crates/colonizer/src/github.rs:2324-2481` (`publish`, `restore_gitfile`, `strip_nested_git`); the commit/push/PR gates in `crates/colonizer/src/publish.rs`, grants in `crates/colonizer/src/authority.rs` |
| Auth: per-colony tokens at the provider gateway, the per-install token on every cockpit `/api` route | `crates/colonizer/src/gateway/mod.rs`, `crates/colonizer/src/auth.rs` |
| Delegation gate: `COLONIZER_DELEGATE=enforce` refuses every tool but plan, ask and delegate, and the mothership re-checks a memory proposal's `origin` before touching a store | hook in `modules/agents/claude-code/runner.mjs:562-671`; the host half in `crates/colonizer/src/memory.rs` (`role_of`). The hook's wall is the Agent SDK, not the kernel — the host-side re-check is the hard half |
| In-guest hardening: `boot.sh` hides kernel interfaces and remounts `/proc` with `hidepid`; agentd drops 21 capabilities, sets `no_new_privs` and no core dumps, and installs a seccomp denylist on the agent process and everything it starts (the human's terminal is not filtered). Codex's own landlock/seccomp is deliberately left off (`modules/agents/codex/runner.mjs:372`) — agentd's filter covers it | `crates/colonizer/src/boot.rs`, `crates/colonizer-agentd/src/harden.rs` (applied in `crates/colonizer-agentd/src/runner.rs:99`); details in [architecture.md](architecture.md#in-guest-hardening). Landlock is **not yet**: the pinned libkrunfw kernel is built without it |

## Guidance

| Guidance | Where it lives |
| :--- | :--- |
| System-prompt text: rules for questions, limits, memory, findings and delegation, appended at startup | `modules/agents/claude-code/runner.mjs:35` (`SYSTEM_PROMPT_APPEND`), assembled at `runner.mjs:504-518` |
| Skill packs: vendored skills and tool servers, switched on per org | [skill-packs.md](skill-packs.md). The read-only mount is a boundary; the `SKILL.md` text is guidance |
| Denial hints: `classifyDenial(text)` → `{class, hint}` (`egress`, `read_only`, `tool_disabled`); on an errored tool result the runner adds `denial: {class, hint}` to the `tool_result` event, and a `PostToolUseFailure` hook repeats the hint to the agent as `additionalContext` — mid-turn, bound to the failed call, so it costs no extra turn — at most once per class per session. `is_error` and content are unchanged, the hook returns no decision, and a strip test proves the events are identical without the layer apart from `denial` | `modules/agents/claude-code/denials.mjs`, hook in `modules/agents/claude-code/runner.mjs:672-693` |
| Watchdog nudges: `decide()` nudges a colony with no progress and flags it after `max_nudges`; gateway traffic counts as progress. A hint loop — consecutive denied tool results — is nudged with a message naming the denied boundary | `crates/colonizer/src/watchdog.rs` (`decide`, `nudge_text`, `hint_loop_text`); the busy check is `gateway.colony_busy`, called at `watchdog.rs:269` |
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

## Watchdog signatures

The watchdog (`decide()` in `crates/colonizer/src/watchdog.rs`) keys on three signatures:

- **Hint-loop** — consecutive errored `tool_result` events carrying a `denial`, with no successful
  (non-errored) tool result in between, count as *not progress*: from two in a row the watchdog
  nudges on the clock of the progress that preceded the loop, not on the loop's own churn, and the
  nudge names what was denied (`hint_loop_text`, the class and the hint). The loop's retried calls
  and text cannot spend the nudges back, so the existing `max_nudges` → `nudges_exhausted` path
  still ends it. A successful result ends the loop, and so does anything that is a real break in it —
  a person's message, a question, a turn end — which count as progress as before.
- **Genuine stall** — no events at all: the existing nudge-then-flag behaviour, unchanged.
- **Control-defeat** ([#609](https://github.com/Colonizer-dev/harness/issues/609)) — read off
  `boundary` events, below.

### Boundary events

A `boundary` event is the typed record that a control refused something
(`docs/agent-events.schema.json`, `#/$defs/boundary`): its `kind`, the `control` or rule that
decided, a one-line `detail` (redacted, at most 300 characters), the `target` host or path when the
reporter can name one, and `at`, when the control decided. It is reporting only — the control has
already decided, and the event grants nothing. Each lands in the colony's `events.jsonl` and shows in
the chat as a muted row after the message it followed (`web/src/components/BoundaryRow.tsx`).

| Kind | Control | Emitted by |
| :--- | :--- | :--- |
| `exec_policy_deny` | `exec_policy:<rule>` | The runner, when a rule denies a command, or when a person (or the judge) refuses a rule's ask — the Claude Code PreToolUse hook and `askColony`, the ACP permission flow (`modules/agents/*/boundary.mjs`) |
| `exec_policy_ask_bypass_attempt` | `exec_policy:<rule>` | The runner, when a rule whose ask was refused earlier in the run asks again, however the command was rephrased |
| `egress_denied` | `egress` | The Claude Code runner, after a tool result the denial layer classes as `egress`; the target is the host the output or the call names |
| `sandbox_denied` | `read_only_mount` | The Claude Code runner, after a tool result classed `read_only` (a read-only mount refused a write) |
| `path_policy_unbound` | `path_policy:masked` / `protected` | agentd, beside its warn line when it could not bind a policy path that appeared mid-session (`crates/colonizer-agentd/src/watch.rs`) |
| `path_policy_denied` | `path_policy:masked` / `protected` | The mothership, on the first `path_policy` report of each (access, path) (`crates/colonizer/src/path_policy.rs`, `on_attempt`) |
| `publish_rewrite_refused` | `gitfile`, `pr_redaction` | The mothership at publish, when the worktree's `.git` is not the gitfile it recorded (a directory, a symlink, another gitdir, removed) and it rewrites it, or when `pr.md` held a credential it redacts (`crates/colonizer/src/github.rs`, `publish`) |

Not reported: a seccomp denial is an `EPERM` the agent sees like any other, logged only to the guest
kernel's audit stream, so nothing on the wire tells it from an ordinary permission error; a nested
`.git` stripped at publish is logged but routine (tests and vendored checkouts leave them); a refusal
by the delegation gate (`tool_disabled`) is guidance-shaped and hit in normal work; and the runners
other than Claude Code and ACP apply no exec policy and classify no denials.

### The control-defeat signature

`note_boundary` and `note_reach_result` in `crates/colonizer/src/watchdog.rs` fold each boundary
event into the colony's trail, timed on the mothership's own receive clock, and fire on:

| Signature | Fires when |
| :--- | :--- |
| `repeated_denial` | 3 (`REPEATED_DENIALS`) events of the same kind and control within 10 minutes (`CONTROL_DEFEAT_WINDOW_MINUTES`) — a refused action retried with trivial variations matches the same rule each time. One denial, or one retry, is a wall the colony works around and fires nothing |
| `ask_bypass` | Any `exec_policy_ask_bypass_attempt`: a rule a person refused, asking again |
| `deny_then_reach` | A tool call whose input names a target a control refused within the last 30 minutes (`REACH_WINDOW_MINUTES`) — a host as `://host` or `@host`, a path as a whole path token — returns a successful result: the egress-denied host reached another way, the refused write target written through another tool, a path agentd could not bind read anyway. An errored result is the wall holding again |
| `publish_rewrite` | Any `publish_rewrite_refused`: colony output that crossed the publish wall |

A `path_policy_unbound` alone fires nothing — off Linux every nested checkout produces one — but its
target joins the deny-then-reach watch. The events a signature used are spent, so the same evidence
never flags twice. The trail is in memory, capped at 64 events, and a restart forgets it.

**Response: attention and a log line, not a stop.** A signature sets the colony's attention to
`control_defeat`, carrying `signature`, a one-sentence `detail` and the boundary events as
`evidence`; the colony's log gets an error line naming the signature; the cockpit's attention
banner lists the evidence; and an attention notification goes out if they are on. The watchdog does
not nudge over the flag or replace it with a stall flag, the colony carrying on does not lift it,
and only a person's own message (or the colony stopping) clears it. It does not stop the colony:
"Defeated a control" above is a stop once a person has confirmed the boundary failed, and a
signature is a pattern that says where to look, not that proof. Stopping is the person's call.
