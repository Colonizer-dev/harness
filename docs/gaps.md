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
Last checked against the code on 2026-10-05, at commit `564c794`.

## Not built

| Element | Where the design shows it | What the code has instead | Issue |
| --- | --- | --- | --- |
| Remote access that is safe to leave on | `docs/remote-tunnel.md`; the Settings switch (`web/src/components/RemoteAccessPane.tsx`) | The relay is deployed at my.colonizer.dev with per-install TLS, and the tunnel client dials it by default (`crates/colonizer/src/remote.rs:49`); pairing is built (#599), and finding R1 is fixed — `stripHopByHop` reads the client's `[name, value]` pairs (`services/relay/src/protocol.js:71`, #659). Left from `docs/remote-access-review.md`: R2 is only narrowed (a reset drops the old install's owner, but the relay has no endpoint that deletes an install), and R3–R5 are open. Off by default | [#531](https://github.com/Colonizer-dev/harness/issues/531) |
| Strix as a red-team hunter | The red-team wizard shows Strix as "Coming soon" (`web/src/cockpit/RedTeamWizard.tsx:239`) | Strix installs and probes (off unless `COLONIZER_HUNTER_INSTALL=1`), but a run that names it still gets a 400 (`check_hunter`, `crates/colonizer/src/redteam.rs:1632`): its scans need a Docker daemon colonies do not have (`needs_docker`, `crates/colonizer/src/hunters.rs:66`). Shannon is no longer a gap — a run with `"hunter": "shannon"` launches one colony per repository running the pinned `npx` Shannon, and the host files the SARIF it leaves through validation | [#216](https://github.com/Colonizer-dev/harness/issues/216) |
| Watchdog control-defeat signature | `docs/boundaries.md`, "Watchdog signatures" | The hint-loop and genuine-stall signatures are built (`crates/colonizer/src/watchdog.rs:44`), but no boundary event — an audit, publish-rewrite or sandbox event showing a control was bypassed — reaches the mothership for the watchdog to read, so control-defeat is not (`docs/boundaries.md:61`) | [#609](https://github.com/Colonizer-dev/harness/issues/609) |
| Verifying bun and pnpm repositories | `docs/colonies.md`, "Verifying done" | Verification detects the package manager, and a stock image without bun or pnpm reports the check unverifiable by name, never contradicted (`crates/colonizer/src/verify.rs:922`). The colony-node image adds pinned, checksum-verified bun and pnpm (`images/colony-node/Dockerfile:16`, `:26`) and is published by `.github/workflows/colony-image.yml` on each push to main (`ghcr.io/colonizer-dev/colony-node@sha256:5aab820b…`, linux/amd64 and arm64), but the GHCR package is private, so an anonymous pull — what a colony does — is refused (`images/anonymous-pull.sh` reports it). What remains: an org owner makes the package public, the next publish attests it, and `crates/colonizer/images.lock:11` follows by pull request; until then the node preset pins stock `node:24-bookworm`, which has neither | [#589](https://github.com/Colonizer-dev/harness/issues/589) |
| Landlock inside the colony | `docs/architecture.md`, in-guest hardening | Capabilities, no_new_privs, no core dumps and a seccomp denylist apply; Landlock is blocked upstream — no libkrunfw release through 5.6.2 or main builds the guest kernel with it, and microsandbox 0.7.3 (`vendor/vendor.lock:5`) still bundles the 5.6.1 build, so it waits on upstream, not on us | [#638](https://github.com/Colonizer-dev/harness/issues/638) |
| Remote outposts and a fleet board of every colony | `docs/vision.md`, `docs/outposts.md` | The colony launch path runs through the `ExecutionBackend` trait, but `LocalBackend` is the only one — no remote outpost, no scheduler (`crates/colonizer/src/execution.rs:69`); a [fleet](fleet.md) lists peer motherships and, once a member syncs its history, its finished colonies (`crates/colonizer/src/fleet_history.rs`), but not the ones still running | none filed |
| A settler count on each overview row | Cockpit prototype, overview ([#194](https://github.com/Colonizer-dev/harness/pull/194)) | No count: the mothership streams one colony's events at a time (`web/src/cockpit/OverviewView.tsx`) | none filed |
| Role captions under a crew's ants | `colonizer-website/colonies.html`, "a crew on one trail" illustration | The ants on one trail and a count ("3 settlers · all done"); each ant's name is only its hover title (`web/src/components/ChatPanel.tsx`, `CrewStrip`) | none filed |
| A done settler's duration, and "retried" after a failed step | Settler Showcase, settler card ([#71](https://github.com/Colonizer-dev/harness/pull/71)) | The step count, and "didn't work" on a failed step (`web/src/components/SettlerCard.tsx:156`); the stream records neither | none filed |
| Observability export (OTLP to Grafana, Datadog, Honeycomb and others) | [vision.md](vision.md) roadmap; the ADR in [#839](https://github.com/Colonizer-dev/harness/issues/839) | The `observability` module kind and its `otlp` and `file` settings are declared, and a rotation-safe jsonl tailer with a durable cursor exists, but nothing calls it and no exporter sends a byte (`crates/colonizer/src/observability/mod.rs`). The [live map](telemetry.md) and [usage data](usage-data.md) report to colonizer.dev's services, not to your backend (`crates/colonizer/src/telemetry.rs`, `crates/colonizer/src/usage.rs`) | [#839](https://github.com/Colonizer-dev/harness/issues/839) |

"Where the design shows it" means wherever the promise is made: a doc, a screen, a comment in the
code, or a design. "none filed" means the gap is known and no issue tracks it, usually because it
is a design detail or depends on something outside this repository.

## Checked and built

Elements that are easy to remember as missing, and where they are.

| Element | Where the design shows it | Where the code draws it |
| --- | --- | --- |
| A session store other than local disk ([#610](https://github.com/Colonizer-dev/harness/issues/610)) | `docs/session-store.md` | Every read and write of a session's records goes through the `SessionStore` trait (`crates/colonizer/src/store.rs`), with what must stay a host path listed in `LAYOUT_ALLOWLIST` and enforced by a test. `session-store.json` can name an S3-compatible bucket (R2, MinIO, AWS S3), which the mothership runs on through a write-ahead working copy that uploads with retries (`crates/colonizer/src/store_s3.rs`); `colonizer sessions migrate` copies into it resumably, verifies by SHA-256 and switches (`crates/colonizer/src/store_config.rs`) |
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
| Today's spend in the header | Cockpit prototype, header ([#187](https://github.com/Colonizer-dev/harness/pull/187)) | Since [#613](https://github.com/Colonizer-dev/harness/issues/613), the overview header shows today's spend beside the running total — "$N spent · $M today" — from the spend journal, bucketed by the browser's local day (`web/src/spend.ts`, `web/src/cockpit/OverviewView.tsx`) |
| The ACP module's Model setting | `modules/agents/acp/module.json`, setting `model` | Since [#603](https://github.com/Colonizer-dev/harness/issues/603), the runner applies `COLONIZER_MODEL` with `session/set_model` at session start, the same request the cockpit's live switch sends, and logs a warning when the agent advertises no model selection (`modules/agents/acp/runner.mjs:684`) |
| Provider Trusted, model map and disabled tools in the cockpit | `docs/providers.md`; the gateway's 403 says "mark it trusted in providers.json" | Since [#605](https://github.com/Colonizer-dev/harness/issues/605), the provider editor has a Trusted switch, a model-map editor and disabled tools (`web/src/components/settings/ProviderForm.tsx:540`), and `GET /api/providers` returns `trusted` (`crates/colonizer/src/providers.rs:975`) |
| Automatic log-archive retention | The Storage panel's "Automatic cleanup" form (`web/src/cockpit/StoragePanel.tsx`) | Since [#606](https://github.com/Colonizer-dev/harness/issues/606), the Disk cleanup loop's "Session archives" category (off by default) sweeps bundles past a keep-days or size limit on its own (`crates/colonizer/src/disk_cleanup.rs:1056`, fired from `crates/colonizer/src/loops.rs:1085`); `GET /api/storage` counts the archive (`archive_bytes`, `crates/colonizer/src/reclaim.rs:556`). The Storage panel's form is the one-off Clean up now |
| Resuming a colony whose tokens ran out | `docs/protocol.md`, quota exhaustion | Since [#213](https://github.com/Colonizer-dev/harness/issues/213), a quota-exhausted colony is `Parked` and the queue's resume pass requeues it once its provider recovers or the reset a card's "wait" scheduled comes (`crates/colonizer/src/queue.rs:1562`, `crates/colonizer/src/quota_cards.rs:251`) |
| Jev deciding, not only measuring | `docs/colonies.md`, "Measuring Jev compaction" | Since [#582](https://github.com/Colonizer-dev/harness/issues/582), the shared decision layer has off, shadow and act modes (`crates/colonizer/src/decide.rs:31`); act applies a confident pick to model-tier routing (`crates/colonizer/src/routing.rs:258`), recovery (`crates/colonizer/src/recovery.rs:165`) and verify focus (`crates/colonizer/src/verify_focus.rs:30`). The compaction ladder itself still only measures (`crates/colonizer/src/jev_ladder.rs`), as its doc says |
| Previews over the mesh | `docs/vision.md`, principle 5: "a hop away for chat, terminals and previews" | Since [#690](https://github.com/Colonizer-dev/harness/issues/690), an owner points a live colony at a guest-local port and browsers reach it through `/api/previews/{id}/` (`crates/colonizer/src/previews.rs:124`), linked from the colony's dashboard row (`web/src/cockpit/DashChart.tsx:852`); WebSocket/HMR upgrades are out of scope |
| Searching the operator vault, and proposing notes back to it | `docs/colonies.md`, "Operator vault" | Since [#777](https://github.com/Colonizer-dev/harness/issues/777), the runners serve `vault_search` over the staged snapshot (`modules/agents/claude-code/vault.mjs`, copied into the ACP, OpenCode and Pi modules; inline in `modules/agents/codex/mcp.mjs`), and `vault_propose` sends a `vault_proposal` event the mothership queues for review (`crates/colonizer/src/vault.rs`); the Memory page lists them with Accept, which writes a new note into the vault's inbox folder, and Reject (`web/src/components/VaultProposals.tsx`) |
| The cockpit address, bookmark and home-screen install in one step | [vision.md](vision.md) roadmap; [#867](https://github.com/Colonizer-dev/harness/issues/867) | Since [#867](https://github.com/Colonizer-dev/harness/issues/867), the "Your cockpit" card lists where the cockpit can be reached — on this computer, the network or tailnet, and anywhere — each with Copy and a QR code, stripped of any token (`web/src/components/YourCockpitCard.tsx`, `web/src/cockpitAddress.ts`), with the browser's install prompt where it offers one (`web/src/installApp.ts:139`) |
| Hermes colonies on the stock images | The org dialog offers every agent module (`web/src/components/OrgSettingsDialog.tsx`) | Since [#602](https://github.com/Colonizer-dev/harness/issues/602), the Hermes runner builds hermes-agent from its pinned source commit on first boot (`modules/agents/hermes/stage.mjs`): uv, the source tarball and a fallback CPython are sha256-pinned in `modules/agents/hermes/hermes.lock`, and every Python dependency is installed with `--require-hashes` from `modules/agents/hermes/hermes-requirements.lock`. `fetched_by_runner` in `modules/agents/hermes/module.json` lets a launch through on every stock preset. `scripts/pin-hermes.mjs` moves the pin |
| The loop tools (`loop_next`, `loop_stop`) on Pi, Hermes and ACP ([#643](https://github.com/Colonizer-dev/harness/issues/643)) | `docs/loops.md` | Every shipped agent module declares `loop_tools` in its `module.json`. Pi loads `modules/agents/pi/loop-extension.mjs`; ACP registers `modules/agents/acp/loop-tools.mjs` as a stdio MCP server on `session/new` and `session/load`; Hermes passes the loop switches to its vendored `modules/agents/hermes/mcp.mjs` and its bridge in `modules/agents/hermes/runner.mjs` emits the events |

## Keeping it true

A pull request that builds a **Not built** element moves its row to **Checked and built**, or drops
it. One that adds or changes a claim in [vision.md](vision.md) or any other doc, ships a setting or
screen that does less than its name says, or ports a design and leaves part of it out, adds a row.
The pull request template asks.

`scripts/test/gaps.test.mjs` checks that every repository path cited here exists, and that every
**Not built** row names a tracking issue or says `none filed`. It cannot check the pictures: the
website lives in another repository, so when an illustration changes there, record it here by hand.

## What this does not do

Stated here rather than buried.

- **One machine.** Colonies run on the host that launched them: Linux x86_64 with KVM, or an Apple
  Silicon Mac — where the bundled `tailscaled` is built from pinned source, because Tailscale
  publishes no macOS build of it.
- **Seven agent modules, one forge.** Claude Code, Pi, OpenCode, Codex, Grok Build, ACP and Hermes
  ship as agent modules (`modules/agents`); GitHub is the only source and publisher. Claude Code is
  staged from the host (or fetched at install time on a Mac), Pi is bundled with its module, and
  OpenCode, Codex, Grok Build and ACP's gemini preset fetch their pinned CLI on first boot, and
  Hermes builds its pinned source, hash-checked, on first boot. ACP's grok preset needs an image that
  already carries its CLI.
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
  speak. The [`codex` agent module](../modules/agents/codex) runs on an OpenAI API key instead — a `CODEX_API_KEY`
  colony secret for `api.openai.com` — but a ChatGPT sign-in is still nothing the harness can spend
  ([#30](https://github.com/Colonizer-dev/harness/issues/30), [docs/decisions.md](decisions.md)).
- **Memory search inside a colony is plain text matching.** With the mem0 provider, a colony's `MEMORY.md`
  is ordered by mem0's relevance to the task, but `memory_search` still matches words in the notes it was
  given. mem0's Platform API is supported; self-hosted mem0 serves a different API and is not.
- **Not ready for unattended work on sensitive repositories.** That is the v0.1.3 audit's verdict,
  real credentials included. It found four ways a colony could cross into the host, filed as draft
  security advisories and not fixed yet ([docs/audit.md](audit.md)).
- **`cargo install` is now an install path.** `cargo install colonizer-harness --locked` builds only
  the `colonizer` binary, without microsandbox, the in-VM daemon, the agent modules and the web UI
  beside it, but `colonizer setup` installs this version's release over it, and a bare `colonizer`
  start hands over to the installed app when it finds one. `colonizer-agentd` is still source only.
  Nothing is published to npm.
- **CI runs every suite, including one that boots a real colony.** The Rust tests and clippy, the
  runner's, the live map receiver's and the web UI's all run on every pull request; releases are
  built, smoke-tested and attested with build provenance; dependency audits and SBOMs run with every
  change and on a weekly schedule; runtime pins move only by reviewed pull request. GitHub-hosted
  runners do have `/dev/kvm` (the job makes it usable), so the `colony-e2e` job also boots a whole
  colony end to end — mothership, microVM, agentd and the Claude Code runner against a scratch
  repository and a stub model server, asserting it reaches `no_changes`. What that still does not
  cover is a real model or a real GitHub write ([roadmap](vision.md#roadmap-in-public)). The crates are
  published to crates.io through Trusted Publishing; nothing is published to npm.
