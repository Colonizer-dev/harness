# Colonizer

**Colonize your backlog.**

Colonizer sends coding agents out to settle your issues. Each one lands in its own colony, a real
microVM with a fresh worktree, stays linked to home over a private mesh, and returns with a pull
request you can trust. You stay in command: agents bring you decisions as choices, not walls of text.

## Why

Coding agents are good enough to work unsupervised for long stretches, but most setups make you
choose between **safety** (sandboxed, slow, one at a time, constant approvals) and **speed** (an agent
with your credentials loose on your laptop). Colonizer removes that trade-off:

- every task gets a disposable, hardware-isolated machine, so the agent can do anything *inside* it;
- credentials never enter it;
- you can run many at once and steer them with one click each.

## Vocabulary

| Term | Meaning |
| --- | --- |
| **Mothership** | The Colonizer app on your machine. Holds credentials, git, the mesh control plane, and publishes results. |
| **Colony** | One session: a microVM + git worktree + agent, landed on one task. Disposable. |
| **Settler** | An agent working in a colony. The agent module (Claude Code today) is a colony's first settler; the subagents it sends out for parts of the task are settlers too, each named for its role: Scout, Builder, Inspector, Tester and so on. |
| **Frontier** | Your backlog: issues and repositories waiting to be settled. |
| **Mesh** | The private network linking every colony to the mothership, separate from any network you already use. |
| **Return** | Bringing a colony's work home as a pull request. |

Use the metaphor lightly in the product; plain words win whenever clarity is at stake.

## Principles

1. **Real isolation by default.** Colonies are KVM microVMs, not shared-kernel containers. Secrets are
   injected at the network edge and never exist inside a colony.
2. **Decisions, not prose.** When an agent needs a human, it asks with concrete choices (plus
   "Other"). A good question takes one click to answer.
3. **Everything is a module.** Sources, sandboxes, meshes, settlers, interfaces and publishers are
   swappable providers behind small contracts.
4. **Local-first and self-contained.** Runs on your hardware and ships its own dependencies, with no
   cloud account required. After install, the only downloads are the colony images colonies run and the
   optional bundles you switch on, such as Headroom.
5. **Scale out, stay close.** Many colonies run in parallel; the mesh keeps each one a hop away for
   chat, terminals and previews.
6. **Trust, then verify.** Colony output is untrusted data until the mothership has sanitized and
   published it, and a human has reviewed the pull request.

These are the design, and where the code does not reach them yet, [audit.md](audit.md) says so.

## Horizon

- **More settlers:** other coding agents behind the same runner protocol.
- **More frontiers:** GitLab, Linear and Jira as sources; review comments as follow-up tasks.
- **Remote outposts:** other machines (a GPU box, a home server) join the mesh and host colonies.
- **Fleet view:** a live board of every colony, what it's waiting on, and what it costs.
- **Guardrails:** network policies and approval rules as modules.
- **Previews:** dev servers inside colonies reachable from the mothership over the mesh.

## Brand

- **Name:** Colonizer, at colonizer.dev.
- **Voice:** calm, technical, confident; a touch of frontier and space flavor, used sparingly.
- **Look:** deep-space neutrals with one beacon-orange accent; a simple hexagon outpost mark.
