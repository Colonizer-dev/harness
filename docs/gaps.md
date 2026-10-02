# Gaps: what is promised and not built yet

This page lists what the docs, the cockpit, [vision.md](vision.md) or
[colonizer.dev](https://colonizer.dev) promise or imply that the code does not do yet. Check an item
here before you report it as a regression, and before you write about it as if it works.

It covers two kinds of promise:

- **Features.** A setting, button, route or module that exists in some form but does not do
  everything its name or docs suggest (a setting that is read by nothing, a download with nothing
  published, a module whose CLI is not staged).
- **Design.** Elements drawn in the design: vision.md, the website's illustrations, and two
  prototypes the cockpit was ported from, which are not in this repository. The cockpit prototype,
  "Colonizer Cockpit", was first ported in
  [#187](https://github.com/Colonizer-dev/harness/pull/187) and caught up with in
  [#194](https://github.com/Colonizer-dev/harness/pull/194). The Settler Showcase was ported in
  [#71](https://github.com/Colonizer-dev/harness/pull/71). Nest text balloons were once reported as
  lost when the nest had never drawn them
  ([#232](https://github.com/Colonizer-dev/harness/issues/232)).

It is a register, not a roadmap. Where the code does not yet meet the security and trust claims,
[audit.md](audit.md) says so. Planned work is the horizon in [vision.md](vision.md) and the issues.
Last checked against the code on 2026-09-27.

## Not built

| Element | Where the design shows it | What the code has instead | Issue |
| --- | --- | --- | --- |
| Remote access that works end to end | `docs/remote-tunnel.md`; the Settings switch (`web/src/components/RemoteAccessPane.tsx`) | The relay (`services/relay`), the tunnel client (`crates/colonizer/src/remote.rs`) and the Settings switch are merged, pairing included (#599), but the relay is not deployed, and the relay throws on the mothership's response headers (finding R1 in `docs/remote-access-review.md`). Off by default | [#531](https://github.com/Colonizer-dev/harness/issues/531) |
| Hermes colonies | The org dialog offers every agent module (`web/src/components/OrgSettingsDialog.tsx`) | The runner and its tests exist, but nothing stages the `hermes` binary: Claude Code is staged, Pi is bundled, and OpenCode, Codex, Grok Build and the ACP gemini preset fetch their pinned CLI on first boot (sha256-verified), while Hermes has no fetcher — so the harness refuses a Hermes launch on the stock preset images, which carry no agent CLIs, unless you build your own image | [#602](https://github.com/Colonizer-dev/harness/issues/602) |
| The ACP module's Model setting | `modules/agents/acp/module.json`, setting `model` | Nothing reads it (`modules/agents/acp/runner.mjs`); only a live model switch from the cockpit works | [#603](https://github.com/Colonizer-dev/harness/issues/603) |
| Strix and Shannon as red-team hunters | The red-team wizard shows both as "Coming soon" (`web/src/cockpit/RedTeamWizard.tsx`) | Strix installs and probes (off unless `COLONIZER_HUNTER_INSTALL=1`), Shannon is a manifest only; a run that names either gets a 400. Only the colony swarm hunts | [#216](https://github.com/Colonizer-dev/harness/issues/216) |
| Claim and epic overrides from the CLI and MCP | `docs/cli.md`, `docs/mcp.md`; the refusal text says "pass allow_duplicate" | The cockpit and `POST /api/sessions` take `allow_duplicate`, `queue_behind_holder` and `allow_epic`; `colonizer launch` and `launch_colony` do not | [#600](https://github.com/Colonizer-dev/harness/issues/600) |
| `--host` and `--token-file` on local commands | `colonizer open --help` and the other local commands list them | `open`, `update`, `login-item`, `telemetry`, `version`, `completions` and `man` ignore them (`crates/colonizer/src/cli.rs`) | [#604](https://github.com/Colonizer-dev/harness/issues/604) |
| Provider Trusted, model map and disabled tools in the cockpit | `docs/providers.md`; the gateway's 403 says "mark it trusted in providers.json" | The fields work through `PUT /api/providers/{id}` or `providers.json`; the cockpit has no controls and `GET /api/providers` does not return `trusted` | [#605](https://github.com/Colonizer-dev/harness/issues/605) |
| Automatic log-archive retention | The Storage panel's "Automatic cleanup" form (`web/src/cockpit/StoragePanel.tsx`) | Retention runs only on request (`POST /api/archive/retention`); nothing sweeps on its own, and `GET /api/storage` leaves the archive out (`crates/colonizer/src/archive.rs`) | [#606](https://github.com/Colonizer-dev/harness/issues/606) |
| The graft skillset download | Settings offers it (`web/src/components/Skillsets.tsx`) | No bundle is published or pinned in `crates/colonizer/graft.lock`, so it is always "Not published for this machine yet" | [#607](https://github.com/Colonizer-dev/harness/issues/607) |
| The loop tools on Pi, Hermes and ACP | `docs/loops.md` | Their runners serve no colonizer MCP server, so loops on them get a brief without `loop_next`/`loop_stop`, the Loops form warns when you pick self-paced, and a self-paced one runs every 24 hours. Claude Code, Codex, Grok Build and OpenCode serve both tools | [#643](https://github.com/Colonizer-dev/harness/issues/643) |
| Watchdog hint-loop and control-defeat signatures | `docs/boundaries.md`, "Watchdog signatures (planned)" | Only the stall path exists (`crates/colonizer/src/watchdog.rs`) | [#609](https://github.com/Colonizer-dev/harness/issues/609) |
| A session store other than local disk | `docs/session-store.md` | The `SessionStore` trait and a reference object-store backend exist, but startup reads and per-session writes bypass it, and no command runs the migration | [#610](https://github.com/Colonizer-dev/harness/issues/610) |
| Verifying bun and pnpm repositories | `docs/colonies.md`, "Verifying done" | Verification detects the package manager, but the stock colony image has no bun or pnpm | [#589](https://github.com/Colonizer-dev/harness/issues/589) |
| Resuming a colony whose tokens ran out | `docs/protocol.md`, quota exhaustion | A quota-parked colony reuses `stopped` and is resumed by hand | [#213](https://github.com/Colonizer-dev/harness/issues/213) |
| Jev deciding, not only measuring | `docs/colonies.md`, "Measuring Jev compaction" | Stage 1 is shadow-mode measurement only; nothing acts on it | [#582](https://github.com/Colonizer-dev/harness/issues/582) |
| Landlock inside the colony | `docs/architecture.md`, in-guest hardening | Capabilities, no_new_privs, no core dumps and a seccomp denylist apply; Landlock is blocked upstream — no libkrunfw release through 5.6.2 or main builds the guest kernel with it, and microsandbox 0.7.3 still bundles the 5.6.1 build, so it waits on upstream, not on us | [#638](https://github.com/Colonizer-dev/harness/issues/638) |
| Remote outposts and a fleet board of every colony | `docs/vision.md`, `docs/outposts.md` | The colony launch path runs through the `ExecutionBackend` trait, but the local backend is the only one — no remote outpost, no scheduler (`crates/colonizer/src/execution.rs`); a [fleet](fleet.md) lists peer motherships and, once a member syncs its history, its finished colonies, but not the ones still running | none filed |
| Previews over the mesh | `docs/vision.md`, principle 5: "a hop away for chat, terminals and previews" | Chat and terminal only (`web/src/components/ChatPanel.tsx`, `web/src/components/TerminalPanel.tsx`) | none filed |
| Today's spend in the header | Cockpit prototype, header ([#187](https://github.com/Colonizer-dev/harness/pull/187)) | The workspace's running total, "$N spent" (`web/src/cockpit/OverviewView.tsx`) | [#613](https://github.com/Colonizer-dev/harness/issues/613) |
| A settler count on each overview row | Cockpit prototype, overview ([#194](https://github.com/Colonizer-dev/harness/pull/194)) | No count: the mothership streams one colony's events at a time (`web/src/cockpit/OverviewView.tsx`) | none filed |
| Role captions under a crew's ants | `colonizer-website/colonies.html`, "a crew on one trail" illustration | The ants on one trail and a count ("3 settlers · all done"); each ant's name is only its hover title (`web/src/components/ChatPanel.tsx`, `CrewStrip`) | none filed |
| A done settler's duration, and "retried" after a failed step | Settler Showcase, settler card ([#71](https://github.com/Colonizer-dev/harness/pull/71)) | The step count, and "didn't work" on a failed step; the stream records neither (`web/src/components/SettlerCard.tsx`) | none filed |

"Where the design shows it" means wherever the promise is made: a doc, a screen, a comment in the
code, or a design. "none filed" means the gap is known and no issue tracks it, usually because it
is a design detail or depends on something outside this repository.

## Checked and built

Elements that are easy to remember as missing, and where they are.

| Element | Where the design shows it | Where the code draws it |
| --- | --- | --- |
| What each settler is doing, in words | `colonizer-website/index.html`, "01 / the colony" (window "127.0.0.1:7878 · colonizer"): a card per settler with name, task, status, summary, step count and "Show work" | The colony window, not the nest: `web/src/components/SettlerCard.tsx`; the summary shows once a settler is done and its card is closed |
| Text balloons in the nest | Nowhere on the website: it never draws the nest | Since [#269](https://github.com/Colonizer-dev/harness/pull/269) for [#218](https://github.com/Colonizer-dev/harness/issues/218), a balloon saying what the colony is doing on each chamber with room for one; the selected chamber always has one (`web/src/cockpit/NestView.tsx`, `planBalloons`, decides which). Per settler: each ant's hover title, and the inspector's SETTLERS list (`web/src/cockpit/Inspector.tsx`). A bubble per ant is asked for in [#397](https://github.com/Colonizer-dev/harness/issues/397) |
| "Described in plain language" and "Show detail" | `colonizer-website/index.html`, "01 / the colony" | `web/src/components/ChatPanel.tsx` |
| The crew strip, "3 settlers · all done" | `colonizer-website/index.html`, "01 / the colony" | `web/src/components/ChatPanel.tsx`, `CrewStrip` |
| A question card with choices and "Other…" | `colonizer-website/index.html`, "01 / the colony" | `web/src/components/AskUserCard.tsx` |
| Create PR, Stop, Clean up, and the activity strip | `colonizer-website/index.html`, "01 / the colony" | `web/src/components/SessionView.tsx` |
| The sidebar: orgs, connections, colonies, memory | `colonizer-website/index.html`, "01 / the colony" | `web/src/components/Sidebar.tsx`, in windows narrower than 900px; a wider window shows the cockpit's sidebar instead (`web/src/App.tsx`, `web/src/cockpit/NavRail.tsx`) |
| Twelve settler roles | `colonizer-website/colonies.html`, "03 / settlers" | `web/src/settlers.ts`, `web/src/components/AntAvatar.tsx` |
| Five ant states, and a stumble on a failed step | `colonizer-website/colonies.html`, "the ant shows what its settler is doing" | `web/src/components/AntAvatar.tsx`; "stopped" is its `paused` state |
| A model for each kind of work | `colonizer-website/index.html`, "03 / the router" illustration | `web/src/components/SettingsDialog.tsx` |
| History read from an event log | Cockpit prototype, history ([#187](https://github.com/Colonizer-dev/harness/pull/187)) | Since [#527](https://github.com/Colonizer-dev/harness/pull/527), History reads the activity log (`GET /api/activity`, `web/src/cockpit/history.ts`); since [#612](https://github.com/Colonizer-dev/harness/issues/612) the inbox reads it too, one line per event at its own time, answered or resolved entries kept and marked (`web/src/cockpit/feed.ts`, `inboxEntries`) |
| Sending usage data | `docs/usage-data.md` | Built since #628: the batch is Cratefield's `module-telemetry` payload, and the sender posts it at most once a day — but there is no default endpoint, so an install that never sets `COLONIZER_TELEMETRY_ENDPOINT` in the mothership's environment sends nothing, ever (`crates/colonizer/src/usage.rs`) |
| Per-file +/- counts on the pull request card | Cockpit prototype, inspector ([#194](https://github.com/Colonizer-dev/harness/pull/194)) | Since [#611](https://github.com/Colonizer-dev/harness/issues/611), the card reads `GET /api/sessions/{id}/diff` and lists the changed files with their +/- counts, folded after five (`web/src/cockpit/Inspector.tsx`, `web/src/sessionDiff.ts`) |

## Keeping it true

A pull request that builds a **Not built** element moves its row to **Checked and built**, or drops
it. One that adds or changes a claim in [vision.md](vision.md) or any other doc, ships a setting or
screen that does less than its name says, or ports a design and leaves part of it out, adds a row.
The pull request template asks.

`scripts/test/gaps.test.mjs` checks that every repository path cited here exists, and that every
**Not built** row names a tracking issue or says `none filed`. It cannot check the pictures: the
website lives in another repository, so when an illustration changes there, record it here by hand.
