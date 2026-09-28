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
| **Settler** | An agent working in a colony. The agent module (Claude Code by default) is a colony's first settler; the subagents it sends out for parts of the task are settlers too, each named for its role: Scout, Builder, Inspector, Tester and so on. |
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
   cloud account required. After install, the downloads are the colony images colonies run, the
   optional bundles you switch on (such as Headroom), the pinned OpenCode binary when a colony runs
   that agent module, and a release check every six hours, which you can switch off.
5. **Scale out, stay close.** Many colonies run in parallel; the mesh keeps each one a hop away for
   chat, terminals and previews.
6. **Trust, then verify.** Colony output is untrusted data until the mothership has sanitized and
   published it, and a human has reviewed the pull request.

These are the design. Where the code does not yet meet the security and trust claims, [audit.md](audit.md)
says so; where something this page describes or the website illustrates has not been built yet,
[gaps.md](gaps.md) says so, with its tracking issue where one is filed.

## Horizon, and where it stands

Checked against the code on `main`. **Built** means merged and usable; **partly** says which part;
**not built** means nothing runs yet.

| Horizon | Where it stands |
| --- | --- |
| **More settlers:** other coding agents behind the same runner protocol. | **Partly.** Claude Code, OpenCode and Pi run on the stock colony images. Codex, Hermes, Grok Build and an Agent Client Protocol runner (verified against Gemini CLI) are in the repository, but none of their CLIs is staged into a colony image: each needs an image you build with it on the `PATH`. An org picks its agent module in its settings. |
| **More frontiers:** GitLab, Linear and Jira as sources; review comments as follow-up tasks. | **Not built.** GitHub is the only source and the only publisher. |
| **Remote outposts:** other machines (a GPU box, a home server) join the mesh and host colonies. | **Not built.** The control/execution seam is drawn in the code and only the local side exists ([outposts.md](outposts.md)). |
| **Fleet view:** a live board of every colony, what it's waiting on, and what it costs. | **Partly.** On one machine, the cockpit's Overview, Nest, Inbox and org dashboards show every colony, what it waits on and what it has spent. Across machines, a read-only Fleet panel lists other motherships you name in `COLONIZER_FLEET_PEERS`, with their slots, queue and disk; their colonies are not listed. |
| **Guardrails:** network policies and approval rules as modules. | **Partly.** An egress policy (open, or an allowlist, with allow and block lists per install and per org), a path policy that masks or write-protects worktree paths ([path-policy.md](path-policy.md)), prompt screening at publish ([prompt-screening.md](prompt-screening.md)), and a risk ceiling on which questions the autonomy judge may answer. They are sandbox, screen and autonomy settings, not a policy module of their own. Rules over which commands an agent may run are not built ([#471](https://github.com/Colonizer-dev/harness/issues/471)). |
| **Previews:** dev servers inside colonies reachable from the mothership over the mesh. | **Not built.** Chat and a terminal are what the mesh carries today. |
| **Your cockpit from anywhere:** an opt-in link to your own cockpit, with no port forwarding and no tailnet. | **Partly.** The Settings switch, the mothership's tunnel client and the relay are in the repository, but the relay is not deployed and the two halves do not work end to end yet ([remote-tunnel.md](remote-tunnel.md), [#531](https://github.com/Colonizer-dev/harness/issues/531)). |

## Brand

- **Name:** Colonizer, at colonizer.dev.
- **Voice:** calm, technical, confident; a touch of frontier and space flavor, used sparingly.
- **Look:** deep-space neutrals with one beacon-orange accent; a simple hexagon outpost mark.
