# Changelog

What changed in each release of Colonizer, in the order it would matter to
someone running it.

Releases are `vX.Y.Z` tags on `main`, installed with
`curl -fsSL https://colonizer.dev/install.sh | sh`. Each entry links the pull
request, so the reasoning behind a line is one click away. Dates are the day the
release was published.

Colonizer is `0.x`: the shape of things still moves, and a minor version can
change behaviour. Anything that would cost work (a colony, a worktree, a
setting) is called out under **Take care** rather than left for you to find.

## Unreleased

Entries for the next release are not written here. Each pull request adds its own file under
[`changelog.d/`](changelog.d/README.md), and cutting a release folds them in with
`node scripts/changelog.mjs assemble`, so parallel pull requests never collide in this file.

## [v0.2.3] - 2026-10-04

v0.2.2 was tagged but never published: the tagged commit did not compile (below), so it has no
GitHub release and never reached crates.io. v0.2.3 is v0.2.2 plus this fix; everything listed under
v0.2.2 ships for the first time here.

### Fixed

- **The harness compiles again.** Two pull requests that landed together both declared the
  `observability` module, one as `observability/mod.rs` and one as `observability.rs`, so the
  `colonizer` crate failed to build (`E0761`) at the v0.2.2 tag. The tailer submodules now live in
  `observability/mod.rs`. ([#967])

## [v0.2.2] - 2026-10-04

Tagged but never published; its changes ship in v0.2.3.

### Added

- **The graft skillset downloads and installs instead of always saying it is not published.** The
  `graft-0.19.0-1` release is published and pinned in `crates/colonizer/graft.lock`, one row per
  colony architecture (`linux-x86_64`, `linux-aarch64`), each checked against the release's
  `SHA256SUMS`. On those architectures the Skillsets row offers **Download** instead of reporting
  the skillset as unpublished, and the download unpacks graft to `<data>/plugins/graft` with its
  own Node 22. An architecture no release pins still reports `unavailable`, and an operator's own
  `plugins/graft` directory is still used as is. ([#607])
- **Loops that work on GitHub now run instead of parking on a question.** A loop template like "Triage new issues" needs GitHub, but a colony has no GitHub token, so on a private repository the run booted a colony that could only ask what to do. A loop now carries a **Needs GitHub** switch, on by default for the triage, CI-flake and changelog templates. Before such a loop launches anything the mothership checks it can read the repository; if it cannot, no colony boots and the loop's note says what is missing and how to fix it ("connect a GitHub token with access to `<repo>`"). A run that does launch gets the inputs it used to fetch itself — open issues touched since the last run, failed runs on the default branch, and pull requests merged since the last run — as read-only JSON under `/colonizer/github`, plus three host-proxied tools (`issue_label`, `issue_comment`, `issue_close_duplicate`) the orchestrator may call; the mothership makes the call on the loop's own repository, capped per colony and held back by the external-writes kill-switch. ([#778])
- **A new Observability module: send the harness's logs, traces and metrics to your own backend.**
  Settings → Modules now offers an `observability` kind with two providers — an OTLP endpoint (Grafana
  Cloud, a local collector, Datadog, Honeycomb, Elastic, SigNoz, New Relic or any OpenTelemetry
  backend, over `http/protobuf` or `http/json`) and capped local files. It is off until you configure
  it. The endpoint must be `http://` or `https://` with no credentials or query string, and a plain
  `http://` is accepted only for loopback and private addresses unless you set `allow_insecure`; `grpc`
  is refused in this build. Conversation content and agent thinking are off by default and turning
  either on needs `"confirm_content": true` in the same save. A backend's header value lives in a new
  **Observability headers** secret, never in the settings or a URL. Nothing is exported yet — this
  issue lands the kind, its settings and the rules a save must pass. ([#840])
- **A "Your cockpit" card tells you the address to bookmark, and the cockpit asks you to on first
  sign-in.** Settings → Your cockpit (and a small header button, and the end of Setup) lists where
  this cockpit can be reached — "On this computer", "On your network" or tailnet, and "Anywhere" once
  remote access is on — each with Copy and a QR code. Every address is reduced to scheme, host and
  base path, so the one-time sign-in link from your terminal (`?token=…`) and a phone pairing code
  never ride into a bookmark or a QR code. On first sign-in a prompt offers a bookmark (with the
  browser's shortcut) and, where the browser supports it, the app install; phones get their platform's
  Add to Home Screen steps instead. Dismissing or installing is remembered per device, and the card's
  **Add to this device** offers it again. ([#867])

### Changed

- **A colony parked on an outlived autopilot hold now resumes itself; a resumed autopilot colony
  verifies and publishes like a fresh one.** When the autonomy judge is configured, a hold-parked
  colony auto-resumes on a 1h/3h/9h backoff, and the resume note tells the agent to pick the
  Recommended option and note it in `pr.md`; an answer sent while it sat parked resumes it with that
  answer; a question above the judge's risk ceiling stays parked and notifies a person once; after
  the last step a colony that parks again fails with `abandoned_question` and keeps its worktree.
  A colony resumed from parked (or stopped) now verifies and publishes after its first completed
  turn even when it leaves an existing `pr.md` untouched, so its work no longer waits on a hand-run
  publish. `colonizer list --parked` lists the parked ones. ([#876])
- **The `writes-outside-repo` exec policy no longer asks about writes into a colony's own microVM.** Its root filesystem is discarded when the colony stops, so `mkdir -p /root/target`, a rustup install under `/root/.cargo` or an install under `/usr` was never the host's to protect, and the built-in `ask` fired on nearly every build. The boot now writes the colony's writable host binds to `/colonizer/host-mounts`, and the rule asks only for a write at, under or above one of them (the worktree, `/harness/out`, the resume directory, `/colonizer/services`) — so a write to a host-backed path outside the repository still asks, and `rm -rf /root` asks while `/root/.claude/projects` is mounted. With no mount list (an older mothership, or a runner outside a VM) every absolute path outside the repository asks, as before. The `secret-paths` and `script-egress` denies are unchanged. ([#877])
- **A run that died on an infrastructure blip is retried, and a loop re-runs it once.**

  A colony whose microVM fails to start for an infrastructure reason — a runtime or image hiccup, a
  timeout, a dropped connection, an HTTP 5xx — is retried with a backoff of 1, 5 and 15 minutes
  before it is marked failed, so a blip no longer costs the work. A permanent failure (bad
  configuration, missing credentials, a policy refusal, or a transient one whose budget is spent)
  fails immediately as before. The class of the failure is recorded on the colony.

  A loop follows the same idea across runs: when a run ends failed for an infrastructure reason, the
  loop runs it once more within `retry_failed_runs` minutes of the run's start — 60 by default, `0`
  to switch the re-run off — without counting it as a run or moving the next one. And `colonizer loop
  list` now ends each row with the last run's outcome (`failed (transient_infra)`, `pr_opened`, …),
  `-` when the loop has not run.
  ([#881])
- **The architecture, boundaries and trust-model pages match the code again.** The architecture diagram shows the CLI and MCP entry points, the modules, the model providers behind the gateway, the agent runners beyond Claude Code and fleet members on the mesh; the module tables list `resume`, `screen` and `voice` and the Codex, Grok Build and ACP runners; the mesh policy names the fleet rule; the packaging tree lists what `install.sh` actually stages; and stale line references in boundaries.md and decisions.md point at the current code.
- **CLI, cockpit and Jev docs match the code again.** cli.md lists `redteam` and `fleet sync` as client commands, `migrate-store` among the local ones that refuse the client flags, `migrate-store --from`, and the full route set each token scope reaches (including the fleet scope's network policy and preview proxy); cockpit.md counts OpenCode among the modules that fetch their CLI on first boot; jev.md points at the shadow-only brief picks.
- **Fleet, outposts, colonies, remote-access and good-first-issues docs match the code again.** The colony guide no longer says the CLI and MCP lack an epic override, outposts.md names fleet placement and the closed #298 gate, the tunnel contract's code references point at the current lines, the security review records that R1 is fixed and R2 narrowed, and the good-first-issues list drops the eight issues that have all closed.
- **Install, configuration and hosted docs match the code again.** The Rust minimum for a build from source is 1.98, the local-commands table lists `fleet export`/`fleet import`, the config- and data-directory listings name `fleet.json`, `phones.json`, `deja/` and `fleet-imports/`, the credentials table says `OPENAI_API_KEY`/`XAI_API_KEY` also feed the codex and grok-build modules, development.md runs the hermes and grok-build suites, hosted.md no longer lists the shipped fleet view as planned, and usage-data.md names the telemetry crate as the crates.io dependency it now is.
- **Protocol docs match the code again.** docs/protocol.md now lists every colony `origin` the built-in loops stamp, the read and operate routes a scoped API token actually reaches (`seen`, `keep`, the TypeScript any loop), the cockpit's current views and Settings sections, where model calls go (the provider gateway, or the module's own API), and that burn-down does not use the red-team runs ([#927]); docs/conformance.md no longer calls §7 proposal-only.
- **Agent module docs match the code again.** The Pi and OpenCode READMEs name the Settings tab by its real label (Model providers), the ACP README drops a duplicated sentence in its Grok Build notes, the Claude Code README points at the Exec policy section below its table, and the runner-authoring checklist cites the codex `preflight` function instead of a stale line range.
- **The security docs match the code again.** `docs/escape-vectors.md` and `docs/sandbox-network.md` cite current line numbers for every in-repo mechanism (they had drifted by hundreds of lines), the escape checklist lists the extra `/proc` and `/sys` masks the boot script applies and states the egress compile order precisely, and `docs/audit.md` says CI is a required check on `main` (it is, through branch protection that also requires `vulnerabilities`) and names the CI jobs that run today. Open questions went to issues: [#932], [#933], [#934], [#935].
- **The vision page's Shape diagram matches today's harness.** Model calls go through the mothership's provider gateway (keys stay on the host) to any configured provider, the agent runner can be Claude Code, Codex, OpenCode or Pi, and the diagram now shows the modules (watchdog, autonomy, memory, notify, loops, merge train) and fleet members.

### Fixed

- **The ACP and Grok Build runner tests pass on macOS.** There `os.tmpdir()` is under `/var/folders/…`, a symlink to `/private/var/folders/…`, and the runners resolve paths with `realpath`, so a temp root made with a bare `mkdtempSync` disagreed with the resolved path a confinement or folder-trust assertion compared it against. Every such root in `modules/agents/acp/test/runner.test.mjs` and `modules/agents/grok-build/test/runner.test.mjs` is now built with `realpathSync(mkdtempSync(…))`, so the expected and actual values share one form. Test-only change: no production behaviour moves. ([#751])
- **The autonomy judge no longer fails silently when its model is unreachable.** A judge model call that failed — a `402 Insufficient Balance`, a rate limit, a timeout — used to be swallowed: the judge simply stopped answering and nothing said why. Every call is now classified (`ok`, `provider_error`, `rate_limited`, `timeout`, `unreachable`, `refused`), the outcome lands in the colony's `harness.jsonl` and in the ledger's drop reason (`undelivered: provider_error`), and three consecutive provider failures on the primary model raise exactly one notification and one attention item naming the provider and the error. A new `fallback_models` setting (a comma-separated list of `provider/model` ids) lets the judge answer through a second model while the primary is down, and the harness line and the answer's attribution record which model answered. `GET /api/autonomy/status` reports the judge's model, its fallbacks, the last success and failure and the failure streak, and `PUT /api/modules/autonomy` now probes the primary model with a tiny test call before saving — refusing with the provider's own error (`The judge model deepseek/deepseek-flash failed a test call: deepseek/deepseek-flash 402 Insufficient Balance`) unless the body carries `"save_anyway": true`. Skips that can hide a problem (a suspended colony, answers exhausted, a missing question) are logged once per question and reason. ([#875])
- **A colony whose agent said it was done but never ended its turn is finished for it.** The runner
  emits its final answer as an `assistant_text` and only then a `turn_end`; when it wobbles between
  the two, the turn never ended, nothing verified the claim, and autopilot never published — the
  colony just sat there until the stall watchdog nudged an agent that had already stopped. Now, two
  minutes past a final answer with no tool call in flight, no open question and nothing through the
  gateway, the watchdog asks agentd whether it is alive; when it is, the turn is ended on the
  colony's behalf, a `watchdog_turn_end` event is recorded, and verification and publish carry on as
  they would have. A later real `turn_end` for the same turn publishes nothing new. If agentd itself
  is not answering the colony is wedged: the recovery retries on the next tick and leaves the colony
  to the existing stall handling. ([#878])
- **An update or a restart drains the mothership first, so a colony that is booting is no longer killed by the restart.** `colonizer update` and the Settings button, and a SIGTERM stop, now hold the queue — no new boot starts, a launch or a resume asked for while draining waits — and wait up to five minutes (`COLONIZER_DRAIN_TIMEOUT_SECS`, default 300) for the colonies that are booting or publishing, then requeue any boot the wait gave up on with `interrupted_by_restart` on its log instead of leaving it stopped. A colony still publishing when the wait runs out is not interrupted: the update refuses there instead, clears the drain and says to try again once the publish finishes. Scripts can drive the flag with `GET`/`POST /api/admin/drain` (owner token) and poll `ready` before updating or killing the process. On a hand-run install, `scripts/install.sh` and `scripts/install-release.sh` no longer touch a slot a process is still running from: staging into a slot in use is refused, naming the slot and pid(s), and the previous slot is kept when a mothership or colony is still reading it, for the next start to sweep. A process started through a symlink outside the slot — the way `~/.local/bin/colonizer` starts the mothership, so `ps` shows the link, not the slot — is still found, by the real executable behind the link (`/proc/<pid>/exe` on Linux, `lsof` on macOS when installed) as well as by command line. The systemd user unit and the LaunchAgent now carry `KillMode=mixed` and `TimeoutStopSec=330` / `ExitTimeOut=330`, so the drain gets its time and only leftover helper processes (a boot's `msb`, `git-remote-http`, `gh`) are killed; an existing install re-writes the unit with `colonizer login-item enable`. ([#880])
- **macOS updates keep the Keychain's grant.** A release install or an in-place update replaces the host binary, and the macOS Keychain ties each saved secret to the binary that wrote it — ad-hoc signing makes the new binary a stranger, so the mothership asked for every secret again after `colonizer update`. The installer now re-signs the new `bin/colonizer` with `COLONIZER_CODESIGN_IDENTITY` (from the environment, or the identity a source build with it set records in `~/.local/share/colonizer/codesign-identity`) before moving the `app` symlink. An identity that came in the environment is recorded there too, so an update started from the cockpit — whose installer has no environment of its own — re-signs as well; delete the file to stop re-signing. A `codesign` failure removes the staged slot and stops before the switch, leaving the previous, already-trusted version installed. ([#939])
- **A `gh` that is installed but not logged in no longer makes an update look like a forged release.** The provenance step of `scripts/install-release.sh` ran `gh attestation verify` and read any non-zero exit as "these checksums are not what the release workflow signed" — so on a headless host, where `gh` prints `To get started with GitHub CLI, please run:  gh auth login` and exits non-zero, every update was refused with a false accusation of tampering. An unauthenticated `gh` is now a skip, like a missing `gh`: the checksum-verified install goes on and the note says: `gh is not logged in, so it cannot fetch the attestation; run 'gh auth login' or set GH_TOKEN to verify provenance`. Under `COLONIZER_REQUIRE_ATTESTATION=1` that skip is still fatal, with the same message. A logged-in `gh` that actually rejects the attestation remains the hard "it is wrong" failure, decided by the wording (`gh auth login`) or by `gh auth status` failing. The mothership also passes the GitHub token saved in settings to the installer as `GH_TOKEN` when the environment carries none, so a headless host with a saved token verifies provenance instead of skipping it. ([#940])
- **The installers parse under macOS `/bin/sh` (bash 3.2).** The app-slot process walk that came
  with update draining (above) put a `case` inside `$( … )`, which bash 3.2 cannot parse, so `curl …
  | sh` on macOS and `scripts/install.sh --bundle` would have stopped with `syntax error near
  unexpected token ';;'`. Caught before any release shipped it: the pattern now carries its optional
  leading `(`, which every POSIX shell accepts.
- **The vision page's Shape diagram shows how Claude calls really leave a colony.** Unrouted Claude models go straight to api.anthropic.com with a placeholder key swapped at the edge; only `<provider>/<model>` calls go through the mothership's provider gateway (modules/agents/claude-code/router.mjs).

## [v0.2.1] - 2026-10-03

v0.2.0 was tagged but never published: its release build failed on macOS (below), so it has no
GitHub release and never reached crates.io. v0.2.1 is v0.2.0 plus this fix; everything listed under
v0.2.0 ships for the first time here.

### Fixed

- **The web UI builds on macOS and Windows again.** `web/src/cockpit/fleetColonies.ts` sat next to
  `FleetColonies.tsx`, names that differ only in letter case, so on a case-insensitive filesystem
  `tsc` resolved the view's import to the wrong file and the build failed; that is what stopped the
  v0.2.0 macOS release build. The model file is now `fleetColoniesModel.ts`, and CI fails any pull
  request that adds tracked paths (or script modules, ignoring the extension) differing only in
  case. ([#915])

## [v0.2.0] - 2026-10-03

Never published: the tag exists, but the release build failed and nothing reached GitHub releases or
crates.io. These changes ship in [v0.2.1].

### Added

- **Jev's decision layer is generalised, with a ledger and a per-org switch.** Beyond the routing second opinion, the harness now has one shared place an external classifier can be consulted at any decision point: a point names its question, the closed set of options it may answer with, and a time budget, and each ask produces a pick or a miss saying why there was none. Every ask that is not off appends a row to the data dir's **`decisions.jsonl`** (`point`, `mode`, the `pick` or `miss`, `latency_ms`, and `did` — what the harness actually did, `jev` or `rule`) and one activity line (`decision.shadow`, `decision.act`, `decision.fallback`). Two hard limits ride along: a pick outside the options is never acted on, and a point whose id begins with `publish.`, `security.`, `delete.` or `destroy.` is refused before any network call. An org can now set `jev: false` to turn every point off for its colonies with no network call. Off remains the default. ([#582])
- **Colonies can log which memory notes and skill packs Jev would have loaded at boot.** A new
  claude-code setting, `jev_brief_shadow` (off by default), asks Jev at boot to pick up to five of the
  colony's shared-memory notes and skill packs. Notes tagged `house-rule` or `security` are mandatory:
  always loaded, never offered. Picks are recorded, never acted on — memory stays pull-only and nothing
  a colony sees changes — to `<data dir>/brief_picks.jsonl`, alongside a `used` row each time the colony
  is later seen to read a watched note or touch a watched skill pack. A new
  `node scripts/bench.mjs brief` report grades the picks against those uses (true/false positives and
  negatives, precision, recall, per run and overall); an `act` mode waits for the decision layer in
  [#582] and a measured token saving on the bench. ([#585])
- **Jev can now choose the recovery path when a step fails.** A second decision point, `recovery.path`, is consulted when a colony stops making progress or a turn ends with an error: from a closed set — `retry_same`, `retry_other_provider`, `narrow_task`, `nudge_agent`, `ask_human`, `stop` — the classifier may pick how to recover, offering `ask_human` and `stop` always and the other options by failure class. Nothing in the set pushes, publishes or deletes. **Shadow** mode (the shared `jev_shadow_mode`) asks and records Jev's pick beside the harness's own rule without applying it; **act** mode (`jev_recovery_act`) carries out a confident pick, with a per-colony cap (`jev_recovery_cap`, default 2) after which the colony is left for you — recoveries that ask a person or stop never count. Every decision is graded: ten minutes later a second `decisions.jsonl` row records whether the colony progressed (`{"progressed", "window_min"}`), in shadow as well as act. Off by default; a `JEV_API_KEY` secret is needed. ([#586])
- **A colony image that carries bun and pnpm, so a done-claim verification stops calling those repositories unverifiable.** The default colony image is stock `node:24-bookworm`, which has npm, yarn classic and corepack but neither bun nor pnpm, so the verifier (`crates/colonizer/src/verify.rs`) reported a bun or pnpm repository's declared test command as `unverifiable` (exit 127). This adds `images/colony-node/Dockerfile`: the same base image (pinned by the digest already in `crates/colonizer/images.lock`) plus bun 1.4.2 (the `-baseline` x64 build, because colony microVMs may not expose AVX2) and pnpm 12.8.1, each fetched from its upstream release and checked against a sha256 recorded in the Dockerfile; yarn and corepack are left to the base image. `.github/workflows/colony-image.yml` checks the base digest against `images.lock`, builds the image, runs the verifier's exact bun and pnpm commands against the fixtures in `images/colony-node/test/`, and produces a CycloneDX SBOM and a Grype scan. Only a push to `main` publishes it, as `ghcr.io/colonizer-dev/colony-node:24-bookworm`, and prints the index digest for a follow-up to pin: **the node preset keeps using stock `node:24-bookworm` until that digest is in `images.lock`, so nothing changes for a colony until that follow-up lands.** The agent's brief also now names the repository's own package manager. ([#589])
- **`colonizer launch` and the MCP `launch_colony` tool can pass the claim and epic overrides.** The cockpit and `POST /api/sessions` already took `allow_duplicate`, `queue_behind_holder` and `allow_epic`, but the CLI and the MCP tool did not, so a script hitting a held issue or an epic got a 409 (CLI exit 5) with no way through. `colonizer launch` now takes `--allow-duplicate`, `--queue-behind-holder` and `--allow-epic`, and `launch_colony` takes `allow_duplicate`, `queue_behind_holder` and `allow_epic`, sent as the matching body fields. The refusal texts now name the CLI flag alongside the field (`pass allow_duplicate (\`colonizer launch --allow-duplicate\`)`). ([#600])
- **Codex, Grok Build and the ACP gemini preset now fetch their pinned CLI on first boot.** A colony on those agent modules no longer needs the agent CLI staged into its image: the runner downloads the pinned build — Codex's static musl tarball from the GitHub release assets (`github.com`, plus `release-assets.githubusercontent.com` / `objects.githubusercontent.com` for the served asset), Grok Build's gzip'd static ELF from `x.ai`, and the platform-independent `@google/gemini-cli` bundle from `registry.npmjs.org` — checks its sha256 from the module's lock file before extracting, and caches it under `$XDG_CACHE_HOME/colonizer/<agent>` (or `~/.cache/…`) for later boots. An operator running these colonies behind an egress allowlist must allow the hosts above. Hermes is unchanged: its CLI is still staged by nothing, so a stock preset image refuses a Hermes launch. ([#602])
- **A provider's trusted mark, model map and disabled tools are now editable in Settings → Providers.**
  All three already worked at the gateway, but the cockpit had no controls for them and `GET
  /api/providers` left `trusted` out, so the form had nothing to prefill. The provider editor now has
  a Connection policy group: a **Trusted** switch (lets the security-aware routing gate send
  restricted-sensitivity paths — secrets, `.env` files, infra config — to this connection), a **Model
  map** editor mapping a canonical model name to the name sent on the wire (blank rows are dropped),
  and a **Disabled tools** list of Claude Code tools stripped from every request through it. `GET
  /api/providers` now returns `trusted` (and already returned `model_map` and `disabled_tools`), so an
  existing connection opens with its policy filled in and a save round-trips it — a field left out
  keeps its saved value, an explicit empty one clears it. ([#605])
- **A loop can be given an end date from the cockpit.** The loop form's new **Ends** field takes a
  date and time in your local zone (`end_at`, stored as UTC), and the loop stops when its next run
  would fall past it. A value that is not after now is refused in the form rather than by the
  server, and the Loops list shows the day a dated loop ends. ([#608])
- **The watchdog now notices a colony circling a denial, not only one that has gone quiet.** A run
  of errored `tool_result` events that carry a `denial` (an egress or read-only refusal, a disabled
  tool) with no successful result between them counts as no progress: from two in a row the watchdog
  nudges on the clock of the last real progress rather than the loop's own activity, and the nudge
  names the denied class and hint so the agent takes another route instead of retrying. The loop
  cannot spend the nudges back, so the usual `max_nudges` flag still ends it; a successful result, a
  person's message, a question or a turn end ends the loop and counts as progress as before. ([#609])
- **A colony's records now go through one session store, and `colonizer migrate-store` copies that store elsewhere.** Startup reads the session index through the configured `SessionStore` instead of straight off disk, so an index it cannot read or parse is quarantined the same way as before, and every per-session write — the event log, the host chain's events, the findings ledger and the harness log — is appended through the store, landing at exactly the paths the file helpers wrote. The new `colonizer migrate-store --to DIR` copies this install's colonies into another local store (with `--from` defaulting to `COLONIZER_DATA_DIR`, and `--dry-run` to count what would move and write nothing); it refuses to run while something is listening on the mothership's port (most likely a running mothership) and prints the `COLONIZER_DATA_DIR` to switch to. What stays local is unchanged: the microVM's `vm/` secrets, the archive's copy and event rotation. ([#610])
- **The inspector's pull request card now lists the changed files with their +/- counts.** Opening a
  colony's card fetches `GET /api/sessions/{id}/diff` and shows each file's added and removed lines,
  folding the list after five behind a "+N more" toggle; the answer is cached per colony for the tab,
  so re-opening a card does not refetch. A colony with no diff (never booted, or cleaned up) simply
  shows no file rows, and the branch and PR link are unchanged. ([#611])
- **The overview header shows today's spend beside the running total.** The meta line under
  "Overview" now reads "$N spent · $M today" when the spend journal has measured anything for the
  current local day. The figure comes from the same `GET /api/spend/history` fetch the sparklines
  already read — no extra request — and the history is now bucketed by the browser's local day (a
  new `tz_offset_minutes` query param) rather than UTC, so the header, the sparklines and the
  sparkline labels all agree on where a day ends. Today's figure is left out rather than shown as
  $0.00 when today has no measured row. ([#613])
- **Fleet placement: a colony records why it runs where it does, and a launch can pin a member.** A new
  pure placement policy reads this member and the fleet's last-known peer summaries and decides where a
  colony would go — this member when it has a free slot and can boot the image (a Linux peer needs a
  working `/dev/kvm`, a Mac needs Apple Silicon), else the eligible peer with the most room, else back
  here to queue — and the reason is stored on the colony as **`placement`** and shown under its status in
  the cockpit. `POST /api/sessions` takes an optional **`host`** (a member's id or name) to pin a launch;
  a peer's reduced `/api/status` now publishes its **`kvm_ok`** verdict so placement can read it.
  Cross-member execution is not built yet (#298): an unpinned choice of a peer is recorded but still runs
  here, and a pin to a peer is refused with the reason rather than moved. A duplicate-claim refusal also
  says when the holding host is a fleet member the fleet currently reads as unreachable, so it is clear
  its colony is not re-run here, and the fleet panel says the same of a member with colonies last seen
  running. ([#688])
- **The Overview now has a fleet colony list — every colony the fleet knows about, in one table.** Below the fleet panel, this host's own colonies (live from the session stream) sit beside the finished colonies members pushed to the owner. Each row shows the origin host, the colony (`repo#issue` and title), its status, what it is **waiting on** (`answer`, `slot`, `quota`, `ci` or `review`), its measured cost, and a link that opens the colony on its host — the cockpit's own `?colony=` deep link here, the member's cockpit URL for an imported row. Filters narrow by host, org and repository, and totals add cost up per repository, per host (member) and per day; a member that has pushed nothing shows as zero colonies and an em dash cost, never `$0.00`. Only finished colonies are imported (the same history Settings → Fleet history holds, newest five pages of a hundred), so an in-flight colony on another member is not listed yet. ([#689])
- **A fleet owner can set one network policy for every member's colonies, and members can reach a
  running colony's dev server.** The owner sets a fleet egress floor and a `reach` map with
  `PUT /api/fleet/policy` (a member answers 409), stored as `fleet-policy.json`; each member fetches
  it with its fleet token on the history-push cadence and clamps its colonies against it at boot, so
  a member can tighten the floor (an allowlist under an open floor, extra blocks) but never loosen
  it — a refused loosening is logged and listed in `GET /api/sessions/{id}/egress`. The owner's own
  colonies are clamped too, and the always-blocked set is unchanged. Separately, the owner opens a
  preview of a running colony's guest port with `POST /api/sessions/{id}/preview {"port": 5173}` and
  reaches it at `/api/previews/{id}/…`, reverse-proxied plain HTTP to the guest over the mesh;
  credentials are stripped before forwarding, it needs the owner's credentials or a fleet token
  (403 unless the `reach` map lets a fleet caller through), and it is 404 once the colony stops.
  Previews are mesh-only (409 with the mesh off), carry no WebSocket/HMR upgrade, and a dev server
  serving absolute asset paths needs its base set to `/api/previews/<id>/`. ([#690])
- **Memory snapshots for suspended colonies now have a host side, gated off for now.** A suspended
  colony can be frozen in place and thawed with its conversation and open processes intact — but only
  once the pinned microsandbox can restore a sandbox that has carried a `--secret`, which it cannot
  today (see `sandbox::supports_memory_snapshot`), so nothing changes for a running colony yet. What is
  in place behind that gate: a snapshot lives in `<session dir>/snapshots/`, sealed at rest with
  ChaCha20-Poly1305 under a per-colony 256-bit key kept under the mothership's private state (never in
  the snapshot directory), in 1 MiB chunks so a multi-gigabyte image is never held whole in memory.
  Snapshots are kept only below an 8 GiB resident-memory cap and dropped after 48 h; a missing,
  expired, corrupt or truncated snapshot — or a failed restore — falls back to today's transcript
  resume. A restore re-mints the colony's gateway token (revoking the old one) and its tailnet VM
  key. ([#702])
- **Read a colony's own agent transcript over the API.** `GET /api/sessions/{id}/transcript?format=common` answers the conversation the agent module kept natively — the user turns, the assistant replies with their tool calls and results — normalized to one message shape whatever ran the colony (Claude Code, Codex, OpenCode, Grok or Hermes), with `meta`, a message `total` and the same `limit`/`cursor` paging as the colony list. The colony-writable mount is copied into a host-private directory, symlinks skipped and never followed, before anything reads it, so a planted link cannot reach a file outside the colony. Only `claude-code` and `codex` persist a transcript today; a module that has none, an unknown colony, or a store that cannot be read is a **404**, one with no reader is a **422**, a store over the size caps is a **413**, and a store that is there but will not parse is a **500**. Reading is behind the same token scope as the diff route (`read`). ([#736])

### Changed

- **In `allowlist` egress mode a colony now reaches the hosts its agent module declares.** Until now
  an agent module's `egress` declaration in `module.json` was validated but unused, so allowlist mode
  let a colony reach only what the operator listed in `egress_allow`; the agent's own vendor hosts had
  to be restated there. Now the running module's `api`, `auth` and `extra` hosts join the allow list
  (the union with the global and org `egress_allow`), so claude-code, codex, grok-build, acp and
  opencode colonies reach their vendor's hosts on their own. The module's `telemetry` hosts stay out —
  there is no opt-in for telemetry, so list one in `egress_allow` yourself if you want it. A declared
  host is lowercased and validated like an operator entry, and it cannot reopen anything: the
  always-blocked deny set and the operator's `egress_block` still compile ahead of every allow.
  `open` mode is unchanged. Each colony's record at `<session dir>/egress.json`, served by
  `GET /api/sessions/{id}/egress`, gains a `module` object naming the module and the entries it
  contributed. ([#601])
- **`GET /api/storage` now reports the log archive, and the Storage panel words its archive form
  honestly.** Its `totals` gains `archive_bytes`, the size of the bundles under
  `<data_dir>/archive`, so the archive sits beside worktrees, repos and sessions in the breakdown
  (it already counted physically against the free-disk floor, which measures the data disk). The
  panel's log-archive form was headed "Automatic cleanup", which read as if pressing it saved a
  rule; it is now "Clean up now", with a line saying it runs once on request and that automatic
  retention is the Disk cleanup loop's **Session archives** category (off by default). The archive
  module docs and [colonies.md](docs/colonies.md) say the same. ([#606])
- **The Inbox now shows what happened, one line per event, instead of one line per colony.** It reads
  the same activity log History does (`GET /api/activity`), so a question, a returned pull request or
  a failure keeps its own line at the time it happened, rather than being folded into the colony's
  current state stamped with an `updated_at` that every housekeeping write moves. A line that has
  been dealt with stays: an answered question is marked "answered", a pull request opened then merged
  shows both lines with the opened one marked "handled", and merges and closes are marked as such —
  dimmed, with a small tag, and never counted as unread. A colony that needs you now but has no line
  in the log (the fetch failed, or the page does not reach back far enough) is still read off the
  colony list, so an empty log shows the inbox as before. ([#612])
- **The README is a short front page; the detail moved to docs/.** What the harness is, how to run it,
  how it works and a table of every docs page. The trust model is now
  [docs/trust-model.md](docs/trust-model.md), configuration and per-colony limits
  [docs/configuration.md](docs/configuration.md), the test suites and scripts
  [docs/development.md](docs/development.md); the modules and repository layout joined
  [docs/architecture.md](docs/architecture.md), the roadmap, the argument and the diagram
  [docs/vision.md](docs/vision.md), and what the harness does not do [docs/gaps.md](docs/gaps.md).

### Fixed

- **The relay test suite no longer flakes with "hello was never verified".** `services/relay/test/fakes.mjs` gave the fake relay a `helloTimeoutMs` of `2000` — the same value as the `within()` guard's default — so the relay's own hello-timeout timer and the test guard sat at the same duration, and under load the asynchronous Ed25519 hello verification could lose the race and have its socket closed before it finished. The fake's `helloTimeoutMs` is now `5000` (still below the production default of `10000`) and the `within()` guard is `10000`, so the handshake has comfortable headroom and the two timers no longer race. Test-only change: no production behaviour moves. ([#598])
- **The ACP agent's `model` setting is applied.** The runner now reads `COLONIZER_MODEL` and sends `session/set_model` right after `session/new`, using the same request the cockpit's live `set_model` command already sent, so the agent starts on the model the module setting names (the gemini and grok presets advertise model selection; an agent that does not keeps its own default and the runner logs one warning instead of failing). ([#603])
- **The local commands no longer silently ignore `--host`, `--token-file` and `--json`.** `colonizer
  update` is a thin client of a running mothership, so it now honours `--host` and `--token-file`
  and names the address it actually dialled when none answers; it refuses `--json`. The other local
  commands — `open`, `login-item`, `telemetry`, `version`, `completions` and `man` — refuse all
  three with a usage error (exit 2), and their `--help` no longer lists them. So `colonizer --host
  <tailnet:7878> update` now reaches the mothership there instead of `127.0.0.1`, while `colonizer
  open` stays local on purpose and always prints this machine's link. The client commands are
  unchanged. ([#604])
- **`colonizer update --force` now shows the real session and colony counts in its warning.** The warning read `GET /api/sessions` without the per-install bearer token the API has required since #421, so it always got a 401 and fell back to a vague "the sessions in sessions.json". The CLI now sends the same token it uses for the rest of `colonizer update`, and a non-2xx answer is turned into a definite error, so the count reads as unknown only when it genuinely is. ([#620])
- **A Claude token saved after an account exists is no longer ignored.** `POST /api/settings/claude-token` and the "Log in with Claude subscription" flow wrote the legacy `<config>/claude-token`, which the resolver only fell back to when the default account had no secret — so once any account existed (or a legacy token had been migrated into `default`), a later-saved token sat unused. Both now save into the default Claude account, creating the `default` account (and making it the default) when none exists, and clear the legacy file so there is one source of truth. `DELETE /api/settings/claude-token` clears that account's secret and the legacy file but keeps the account record. Migration and account deletion now go through the secret store too, so a legacy token held in the keychain or as `claude-token.enc` is moved rather than missed, and deleting an account clears its keychain/`.enc` copy rather than leaving it behind. A token saved *before* this change, while accounts already existed, is still sitting in `<config>/claude-token` and is only used while the default account has no secret — re-saving it puts it in the default account. ([#621])
- **Five small correctness fixes across the harness.** The live map no longer counts a suspended colony as running — its microVM is down, so it is counted with the same predicate the spend page already reported as parallel, and parked colonies no longer put a dot on the map. A spend row filed for a turn whose cost has no single owning model now carries a timestamp like every other row instead of an empty one. The provider editor now refuses a negative, NaN or infinite thinking-per-million rate, the same way it already refuses the other prices. A stacked colony whose base is another colony's branch — which has no `origin/` ref until it is pushed — now measures its diff against the local branch instead of failing with a 409, the same order catch-up merges by. And the VAPID signing key is now generated and stored under one lock, so two first-time subscribers can no longer race and each create a key (the push-subscriptions file was already locked; only the key race is fixed). ([#622])
- **The README's `curl -fsSL https://colonizer.dev/install.sh | sh` no longer 404s.** That URL is a
  redirect to the latest GitHub release's `install.sh`, and the graft and headroom bundle workflows
  were publishing their releases as "latest", so the newest bundle (a release with no `install.sh`)
  held the name and the app release went without. Both bundle workflows now pass
  `gh release create --latest=false`, and the app release's final `gh release edit` that publishes
  the draft now passes `--latest`, so a `v*` tag claims "latest" deterministically. The README and
  docs/install.md also now say a Linux user has to be in the `kvm` group before the installer will run
  (`sudo usermod -aG kvm "$USER"`, then log out and back in), and the README lists `curl`, `tar`,
  native Claude Code and the `~/.local/bin/colonizer` path among the prerequisites. ([#685])
- **A restricted-sensitivity colony no longer boots into subagent models the gateway will refuse.**
  A task whose paths classify `restricted` (or `vetted`) had its subagent, background and small
  models resolved against the class at launch: a model on a provider the class will not trust — a
  subagent model on an untrusted provider, say — is now replaced with an eligible one (the
  orchestrator's model, or the module's own `model` for the orchestrator itself) instead of the
  colony failing every subtask. Where the eligible model is itself blank — the default setup, where
  the module names no `model` — the setting is cleared so the task inherits the harness default, and
  recorded as "the orchestrator's model". Each substitution is logged once (a resume does not repeat
  it) and recorded on the colony as `model_substitutions`, which the cockpit shows on the colony view;
  a model the operator named at launch is resolved the same way. When no eligible model exists the
  setting is left as it is and the boot warns that the gateway will refuse it. ([#704])
- **colonizer-harness publishes to crates.io again.** v0.1.10 and v0.1.11 never reached crates.io: the harness compiles in the guest Claude Code pin with `include_str!`, and that file lived in `vendor/`, outside the crate, so the source package `cargo publish` builds from did not contain it and the build failed. The pin now lives at `crates/colonizer/claude-code.lock`, inside the crate like `images.lock`, and every script, workflow and doc that read `vendor/claude-code.lock` reads it from there. CI now builds both published crates from their own packaged source (`cargo publish --dry-run`, in the release's order) on every pull request, so a file the package leaves out fails the pull request instead of the release. ([#818])

## [v0.1.11] - 2026-10-02

### Added

- **Jev can now pick a colony's model tier, not just advise on it.** Two new claude-code module
  settings, `jev_routing_act` (off by default) and `jev_routing_act_confidence` (default 0.8), let
  Jev's `routing.tier` decision take effect: when act is on and Jev's confidence meets the threshold,
  the colony runs on Jev's tier, but never below a floor (the rule's tier for a task the security gate
  marks sensitive, medium for an unknown preset) and never over a tier the operator set explicitly;
  the #470 cost gate still applies. Decision rows in `routing.jsonl` gain `point`, `jev_mode`,
  `jev_agrees` and `floor`, and a `jev` source. A new `node scripts/bench.mjs routing` report shows how
  often Jev agrees with the rule, each disagreement with its outcome and cost, and a verdict on whether
  turning act on is justified. ([#583])
- **Verification can run the most likely failing check first.** When a diff owes more than one check, the **Publish** module's new `verify_focus` setting decides whether the check owning most of the changed files goes first. `shadow`, the default, runs the checks as before and records in the data dir's `jev_focus.jsonl` and the colony's log which check would have gone first, whether it would have caught the failure, and the time to the first failure with and without; `act` runs it first and stops at its contradiction; `off` records nothing. A confirmed verdict still needs every check to pass. ([#584])
- **Codex and Grok Build colonies can run on a model provider.** At boot the mothership now pushes
  the OpenAI key into codex colonies (the stored provider key of a provider named `openai`, else its
  own `OPENAI_API_KEY`, as `CODEX_API_KEY`) and the xAI key into grok-build colonies (provider
  `xai-grok`, else `XAI_API_KEY`), so a manual colony secret is only needed when neither is
  configured; a user-added secret keeps working. The provider gateway accepts the colony token as a
  bearer and passes OpenAI's Responses and Chat Completions requests through to `openai`-wire
  providers verbatim, recording usage. Both runners' model settings take any `<provider>/<model>`
  whose prefix matches a route: codex rides it through a `model_provider` override pointed at the
  gateway, grok-build through `GROK_MODELS_BASE_URL` with the colony token as the key — tokens and
  spend land in the journal, the budgets apply, and the gateway admits only the models the colony's
  settings name, as for every other agent, and the provider key never enters the colony. A
  connection on the `anthropic` wire, or a prefix nobody configured, is refused by name before the
  CLI runs. And the gateway's join no longer repeats a `/v1` the provider's base_url already ends in,
  so the catalog's `https://api.x.ai/v1` xai-grok entry works (it doubled to `/v1/v1` before).
  ([#629])
- **Codex and ACP colonies can now be suspended with their conversation intact.** Both agent modules
  declare `session_resume` and emit `agent_session`, so a colony waiting on its user is suspended and
  the answer that follows restores the same conversation instead of starting a new one: Codex
  announces its thread id the moment codex names it and resumes the thread with `codex exec … resume`
  off a persisted `CODEX_HOME` that each boot strips back to the session rollouts, falling back to a
  fresh thread once if the rollout is missing, and ACP announces a session when its agent advertises
  session loading, resuming with `session/load` and falling back to `session/new` with a warning log
  when the load fails. Claude Code is unchanged; stop, park and plain resumes never resume a session —
  only a suspended colony's restore passes `COLONIZER_RESUME_SESSION`. ([#632])
- **`colonizer pr --wait` follows a pull request's checks until they settle.** The new flag
  re-reads the colony every 15 s and exits when the mothership's checks reach a verdict: 0 on
  success or when the pull request has no checks, 7 when they fail, and 1 if the colony ends
  without a pull request, or its pull request is merged or closed untested, or it is parked. A
  settled verdict wins over the status, so a pull request the merge train merged once its checks
  went green reads as 0. `--timeout` (a `--wait`-only duration: `90`, `90s`, `30m`, `2h`, `1d`,
  the unit spellings `loop create` reads) stops the wait with 8 and a note naming the last checks
  state; without it the wait runs until a verdict. The mothership holds its own re-check at the
  first interval while an open pull request's checks run, so the verdict arrives within about a
  minute of CI settling instead of on a doubled backoff. ([#645])
- **Attempted access to masked paths is reported while the colony runs.** A masked file read as
  empty never said anyone had tried; now the runner judges each path-taking tool call against the
  same bind list the guest booted with and emits a `path_policy` event for a hit — a read of a
  masked path, or a write to a masked or protected one. The harness logs each distinct attempt on
  the colony at warn level (*path policy: agent tried to read masked `.env` (Read)*) and once to
  the History log, without repeating a path and capped at 100 per run. Reporting only: the mount
  enforced before the report existed, and shell commands reaching a masked path stay the exec
  policy's `secret-paths` rule to refuse. The Claude Code and ACP runners report; other agent
  modules follow when their runners are wired up. ([#647])
- **Path policy now covers nested checkouts created mid-session.** The binds a colony boots with
  cover the worktree paths that existed at boot; a checkout that appears later — a clone, `git init`,
  a worktree, a submodule — could smuggle a credential file the colony's agent can read until publish
  reported the change. agentd now watches the workspace and treats every directory below it with its
  own `.git` as a checkout root, binding each masked or protected path relative to that root as it
  appears, with the same mounts the boot script applies — a nested checkout's `.git/config` and
  `.git/hooks/` included, which the boot never binds because the root's git dir is host-mounted
  read-only instead. Best effort, unlike the boot: a failed mount is a warn line on the colony, never
  a stopped daemon. A read that races the watcher can still see a just-created masked file (publish
  still applies), and a bound path cannot be deleted or renamed inside the guest, so a nested
  checkout holding one cannot be `rm -rf`'d. See
  [docs/path-policy.md](docs/path-policy.md). ([#648])
- **Path policy: per-org masked and protected lists.** An org can carry `path_policy.mask_paths` and
  `path_policy.protect_paths` of its own in its settings (`PUT /api/orgs/{org}` or `orgs.json`),
  on top of the sandbox module's lists and the built-in defaults — and on top of the global
  `unmask_paths`, so an org can re-tighten a path the install opted out of. An org tightens, never
  loosens: there is no per-org unmask. Entries are validated like the global ones at save time
  (and dropped again at boot if `orgs.json` was edited by hand), and the effective lists, org
  entries included, are what each boot records to `vm/path-policy`. See
  [docs/path-policy.md](docs/path-policy.md). ([#649])
- **The mothership answers the Unified Harness Protocol's read-side core under `/uhp/v1`.** Discovery (`GET /uhp/v1/uhp`, no token needed), the installed harnesses (`/uhp/v1/harnesses`, `/uhp/v1/harnesses/{id}`), the model catalogue and the single colony (`/uhp/v1/sessions/{id}`, the same shape as a row of the session page #651 added) — with version negotiation (`UHP-Version: 2026-09-12`; any other version is refused with **400** `unsupported_protocol_version`) on every served `/uhp` route, the artifact reads included, and the UHP error envelope on every `/uhp` answer: a missing or wrong credential is **401** `authentication_error`, a scoped token off its allowlist **403** `permission_error`, a colony outside its limits **404** `session_not_found`, a method a route lacks **405**. Scoped tokens are held to the same limits as on the `/api` routes. Streaming, cancellation and input files are not served yet and are reported false in discovery on purpose; how the surface measures against the UHP conformance suite is in [docs/conformance.md](docs/conformance.md). ([#650])
- **A colony's artifacts are readable over the API, and the colony list paginates.** `GET /api/sessions/{id}/files` lists what the colony's agent left in its `out/` directory (top-level regular files only — symlinks are skipped, never followed), `…/files/{name}/content` downloads one capped at 16 MiB, and `…/files/archive` returns them all as a plain tar, refused with **413** `file_too_large` when the artifacts add up past 64 MiB. The same reads answer under their Unified Harness Protocol names beside `/uhp/v1` with the `UHP-Version` header and the §7.7 error envelope, and every unmatched `/uhp` path answers a JSON 404 instead of the cockpit's page. `GET /api/sessions` takes `limit` and `cursor`: with either present it answers a `{"sessions": […], "next_cursor": …}` page, newest first, refusing a cursor it cannot place with **400** `invalid_input`; without the params it stays the bare array the cockpit already reads. ([#651])
- **Red-team hunters chase the bench's raid set.** Point the mothership at a bench pool with `COLONIZER_BENCH_POOL=<dir>` and every red-team launch reads the pool's `raid.json`: the injected bugs recorded against the raided repository are dealt out round-robin to the hunters — no lead handed to two hunters, at most 20 per brief, a raid set longer than the swarm can carry waiting for a later run — as a paragraph on that hunter's brief saying to chase these first, even where they fall outside its focus. Unset, nothing changes; a missing or malformed `raid.json` logs a warning and the swarm launches with ordinary briefs. ([#652])
- **The bench grows synthetic tasks on Rust and Go stacks, not just Node.** `scripts/bench/synth.mjs` now
  detects a repository's stack from its root marker (`Cargo.toml` is Rust, `go.mod` is Go, `package.json` is
  Node; `generate --stack` overrides) and gates and runs each stack's own commands — `cargo test --no-run`
  and `cargo test` for Rust, `go test` for Go, `node --check` and `node --test` for Node — with a
  per-language mutation scanner that skips each language's strings, comments, lifetimes and raw strings, and
  stops at Rust's test-only `#[cfg(…)]`. Held-out companions are placed and run per stack
  (`tests/heldout_check.rs`, `heldout_check_test.go`) and keep their extension in the set; a clone whose
  layout gets in the way — no stack marker, or a `tests` symlinked somewhere else on the host — fails that
  task's held-out check instead of the run. ([#653])
- **Scoring time in the spend journal.** A bench `run` now journals its scoring time into the
  mothership's `spend.jsonl` — one `scoring` row under the `bench` org carrying the run's total
  `scoring_ms`, the only cost scoring has since it makes no model calls — and `GET /api/spend/history`
  sums those into each org day's `scoring_ms`, beside the colonies' spend for that day.
  See [docs/bench.md](docs/bench.md) and [docs/protocol.md](docs/protocol.md) §6.8. ([#654])
- **The ACP module has a `grok` preset.** `agent: grok` launches xAI's Grok Build in its ACP mode (`grok agent stdio`) instead of hand-writing a custom command, authenticated with the `XAI_API_KEY` colony secret for `api.x.ai` and hardened like the grok-build runner: a fresh `GROK_HOME`, the folder-trust gate forced on, no cross-session memory, telemetry and the auto-updater off, and no browser. Verified at the handshake against grok 1.0.34 (`initialize`, `session/new` with model selection) and on the error paths; **a successful turn with a real `XAI_API_KEY` has not been run yet**. Presets whose agent refuses their credential at the handshake now fail as `ACP_AUTH_FAILED` naming the env var to check, and grok's bare "Internal error" on a rejected key carries the same hint. ([#656])
- **A merge train.** An opt-in `merge_train` setting (off by default) lets the harness merge colony
  pull requests itself: about every two minutes it picks, per repository, the one open colony pull
  request whose mergeability is clean, whose checks and base-branch CI are green, that is no draft
  and carries no HOLD / do-not-merge / WIP mark, and whose commits pass the author and attribution
  allowlists — and squashes it, deleting the branch. Never force-merged, never `--admin`.
  `merge_train_overrides` turns it on or off per org or repo and `merge_train_deny_orgs` keeps
  named orgs out whatever the overrides say; `merge_train_authors` and `merge_train_forbid` bound
  who may author a commit and what may not appear in its message. A pull request behind or in
  conflict with its base goes through the existing auto-rebase path (`rebase.rs`) — driven by the
  publish watcher, or by the train itself for a pull request GitHub still calls clean that sits
  behind the base tip — and a stacked parent merges without deleting its branch so the child
  survives and retargets. Every merge and
  skip (with its reason) reaches the colony log, the activity feed and the new `GET /api/merge-train`
  route, which the cockpit shows as one row per repository. Separate from `automerge`, which is
  unchanged. ([#671])
- **A merge supersedes the colonies it covers.** When one colony's pull request merges, other open colonies of the same repository whose work overlaps it — the same supply-chain target, the same issue, or files overlapping enough (at least three shared paths outside lockfiles, 80% of the smaller side's list) — are marked superseded and held out of the queue and the resume route until you keep them (`POST /api/sessions/{id}/keep`, or Keep in the colony view; a quota-parked colony stays Parked until then), the merge train skips a superseded colony's pull request until it is kept, a superseded colony's own pull request is closed with a note only for repositories the org's new `close_superseded_prs` setting lists, and a second live colony for one supply-chain target is refused at launch (there is no queue behind a target) unless it passes `allow_duplicate`. ([#673])
- **A hosted demo of the cockpit.** `npm run build:demo` in `web/` builds a static bundle
  (`web/dist-demo/`, rooted at `/demo/`) with the mock backend forced on — no mothership, no `/api`
  calls, no service worker, no install manifest — for the website repo to serve at
  `https://colonizer.dev/demo`, under a banner that says the colonies are simulated and links the
  install docs. The page is not live yet.
  ([#682])
- **Fleets: motherships that join each other.** One mothership invites another into its fleet like
  phone pairing: the owner mints a single-use invite code that lives 15 minutes — shown once, stored
  only as its SHA-256 — the joining machine's operator enters the owner's private URL and the code
  in Settings → Fleet, both screens then show the same six-digit confirmation code, computed
  independently on each side, and the join completes only when the owner approves what they see and
  the joiner presses **Codes match**. A member keeps hosting its own colonies and appears in the
  fleet view; its credential is a new `fleet`-scoped API token, the lowest there is, admitted only
  on `GET /api/hosts` and `POST /api/fleet/peer/leave` and refused by the public token-create API —
  never the member's local cockpit token. Either side ends the membership: the token is revoked, the
  owner's mesh policy is updated (pairing does not enroll members into the mesh yet — that is a
  follow-up), and the member's local data stays. The read-only `COLONIZER_FLEET_PEERS` polling
  remains as the fallback, and `GET /api/hosts` now also polls fleet peers. Docs in
  docs/fleet.md. ([#686])
- **A machine's past can now travel with it into a fleet.** `colonizer fleet export` writes a
  machine's session history, colony logs and spend/usage stats into one versioned, zstd-compressed
  `.tar.zst` bundle, and `colonizer fleet import` backfills a bundle into a data dir under
  `fleet-imports/<origin_host>/` — resumably and idempotently, so interrupting either leaves
  nothing but a partial that the next run finishes. Export reads only an allowlist of the data dir
  (and of the config dir just its `host_id`), so no API token, provider key or Claude credential
  ever leaves the machine, and session records travel as an allowlist projection with ids
  namespaced by origin host. Both commands run locally with no mothership running, print a per-category preview before
  anything is written or sent, and honour the global `--json`. The format is documented in
  [docs/protocol.md §6.11](docs/protocol.md#611-fleet-export-bundle-687) with a JSON Schema at
  [docs/fleet-export.schema.json](docs/fleet-export.schema.json); wiring the preview → confirm →
  import flow into the fleet-join dialog comes with #686. ([#687])
- **Services come back after a resume.** A suspended colony's new microVM now relaunches the
  long-lived processes the old one was running: a repository declares what it needs in
  `.colonizer/services.toml` at the worktree root (a name, a shell command, and optionally a
  working directory, a readiness port or URL, environment variable names to pass through, and a
  readiness timeout, 60 s by default), and anything the agent starts during the run through the
  guest's `colonizer-svc` command — or as a Claude Code background Bash task — is recorded too.
  On a resume the mothership hands both to the guest, which starts each service before the answer
  or brief is delivered, waits it out to readiness or its timeout, and opens the resumed turn
  saying what came back and what was lost ("Restored from suspension. Restarted: `web` on :5173
  (ready in 3.2 s). Lost: background `cargo test`, rerun if needed."). Background tasks are never
  relaunched — they are reported lost exactly once, rerunning them is the agent's call. Nothing
  secret travels this road: the manifest's `env` holds names only (a table of values is refused),
  and every command is scrubbed of the colony's own secret values before it reaches the guest's
  session config. See "Services that come back after a resume" in [docs/colonies.md](docs/colonies.md).
  ([#700])
- **Opening a suspended colony's question now warms it up.** When the cockpit shows a suspended
  colony's question — the card, or a focus on the answer box — it calls the new
  `POST /api/sessions/{id}/prewarm`, and the mothership boots the colony ahead of your answer
  through the same admission as any restore: never ahead of a colony that already holds an answer,
  and only after queued launches, so warming never delays anyone else. The colony reads "Warming
  up…" while it boots and "Ready — waiting for your answer" once the VM is up, and your answer then
  lands in the running VM without the cold boot. With no answer within the new Sandbox setting
  `prewarm_timeout_minutes` (default 5) the microVM is torn down and the colony goes back to
  suspended, freeing its slot; a failed warm-up or a mothership restart reverts to suspended too.
  ([#701])
- **Answer a colony's question straight from its notification.** When the colony's open question set
  is exactly one single-select question with one to three options, its push now carries the option
  labels and the notification shows a button per option, then Other… for a typed reply where the
  browser delivers one and Open while button slots remain; more options, a multi-select, or several
  questions get a single "Open to answer" instead. Browsers that show no notification buttons at all
  (iPhone and iPad home-screen apps, Firefox, Safari on macOS) keep the old open-on-tap. A tap answers
  through `POST /api/push/answer` with the push's one-time token — scoped to that one question,
  expiring after 24 hours, on a mothership restart or once the question is answered or replaced,
  never an API token — and a silent "Answered: <label>" replaces the notification; on any failure, or when the cockpit answered first,
  the tap opens the colony instead, so exactly one answer lands. ([#742])
- **Web Push devices have settings of their own.** Settings → Notifications now lists every enrolled device with its name (rename it in place), when it was last seen, a **Send test** button and a **Prefs** editor the mothership enforces before it sends: which events arrive (Questions, Pull request opened, Needs rebase, Failed and Needs attention on by default), a repository filter (`org` or `org/repo` entries), quiet hours in the device's own time zone with an optional question break-through, whether a question may sound, whether it shows answer buttons and whether the app icon carries a badge — questions are still the only push that can. A focused cockpit tab already showing a colony holds that device's push for the same colony back. Existing subscriptions keep working on the defaults, with one behaviour change: Provider degraded and the hourly digest, which used to reach every device, now start off per device. ([#743])
- **A badge and grouped notifications for the colonies that need you.** The needs-you count — open
  questions, watchdog-flagged colonies, and failed colonies nobody has opened yet — now also shows as
  the installed app's badge, and pushes group one notification per colony, with a "N colonies need
  you" summary once two or more wait. Answering a question — or opening a colony that failed unseen
  (`POST /api/sessions/{id}/seen`) — sends a silent "resolved" push that closes its notification and
  lowers the badge on every other device at once; a colony whose question is still open keeps
  counting until its question is answered. Resolved pushes never go to Apple endpoints, which
  revoke a subscription for an invisible push, so there a notification waits until tapped and the
  badge catches up when the app next opens; Chrome and Firefox budget silent pushes, so a resolution
  may occasionally surface their generic "site updated in the background" notice. ([#744])
- **The cockpit installs properly on a phone, and an update waits for you.** The manifest gained
  maskable icons, wide and narrow screenshots for the richer install dialog, and shortcuts to Inbox,
  Colonize and the Nest; Android Chrome can install from the address bar, and on iPhone or iPad —
  where web push needs the Home Screen install (iOS 16.4+) — Settings → Notifications and Settings →
  Desktop walk through Add to Home Screen step by step, with splash screens and a matching status bar
  at launch. Sharing a GitHub issue or pull request link to Colonizer opens the colony holding it, or
  Colonize with that issue prefilled; a pull request matches only a colony that recorded it as its own
  pull request. When a new build has installed, a **Colonizer updated** card offers **Reload** instead
  of swapping under you: each build's chunks live in a cache named for that build, and the previous
  build's stays one build longer, so open tabs keep working until they reload. ([#745])
- **Add your phone: pair a phone from a QR code, confirmed on this machine, and keep answering offline.**
  Settings → Add your phone shows a single-use, five-minute invite as a QR code for the best address
  the phone can reach: the remote-access link when it is on, then the tailnet, then the LAN, with a
  warning and the fix when none is reachable. The phone shows a six-digit code that you type into the
  cockpit on this machine; only then does that phone get a sign-in of its own, listed in Settings and
  revocable on its own; revoking a phone, or a scoped API token, closes its open connections at once. The API token never appears in a QR code or a URL, and failed pairing
  attempts are rate limited. An answer or message sent while a colony's connection is down queues in
  the service worker and is delivered once when the mothership is back; a queued answer to a question
  that has since changed is refused rather than delivered. ([#746])
- **A careful, opt-in merge-train loop merges colony pull requests and rebases them when needed.**
  The Loops page and `colonizer loop merge-train` gain a built-in **Merge train** loop, off by
  default and hourly once on, that drives the existing merge train only in repositories you opt in
  (a `never` list keeps upstream-review-only forks out). It merges only while main's latest CI on its
  tip is completed and green, and only a pull request that is behind its base by 0 with every check
  on that exact head green; after a merge it updates the next candidate and waits for its fresh CI,
  one at a time, with a per-run merge cap (4), a cooldown between merges (2 minutes), and paced,
  budgeted GitHub calls — any 403/429 or secondary rate limit stops the run without retrying. A
  conflicting pull request gets the host's mechanical rebase, or becomes `needs_redo` with at most one
  redo colony when enabled; known-flaky red checks are re-run once; and a main the train itself turned
  red pauses the repository and, with `self_heal` on, is re-run once and then handed to a fix colony
  (or, with `revert_on_red`, a revert of the train's own merge). Every run's report — merged, updated,
  red, redo dispatched, skipped, each with its reason — lands in the loop's history, the activity log
  and the cockpit, a dry run lists all of it without writing, and `COLONIZER_NO_EXTERNAL_EFFECTS`
  turns every run into a dry run. ([#754])
- **A fleet owner can now read the history its members sync.** Settings → Fleet has a Fleet history
  section listing every member's synced colonies, newest first, each marked "finished on <member>",
  with filters for member, repository, status and finish date, totals per member and repository
  (colonies, merged, cost) at the top, and a drawer with one colony's record and its logs. The same
  view is `GET /api/fleet/history` (filtered, cursor-paged, with the totals),
  `GET /api/fleet/history/{member}/{row_id}` and `…/logs/{name}`, all owner-only. A removed member's
  history stays readable, marked removed under the name it had. Synced rows are kept for
  `COLONIZER_FLEET_INGEST_RETENTION_DAYS` (default 90) after they arrive; the five-minute reclaim
  tick prunes older ones and the logs only they referenced. ([#762])
- **A fleet member can push its history to the owner, once asked, and survives being cut off.**
  Joining a fleet sends nothing: each membership starts with history sync off, and the member's
  operator turns it on after seeing what would go — Settings → Fleet shows the finished colonies, log
  files and bytes beside the switch, and `colonizer fleet sync --preview` / `--enable` / `--disable`
  do the same from the command line; leaving and re-joining turns it off again. With consent given,
  the member drains every finished colony to its owner on its fleet token: the colony's logs upload
  first, keyed by hash, then the colony record that references them, and a record counts as sent
  only once the owner acknowledges it — so a drain killed at any point resumes without duplicates,
  the owner upserting by id. Batches are capped by rows and bytes; a batch refused without naming a
  row is split in half until the bad row stands alone, and a row that keeps failing is retired and
  listed instead of blocking the queue. A 401 stops and asks for attention, a 429 or 503 waits out
  its `Retry-After`, and a machine the owner removed now reads 403 "removed from the fleet" — the
  owner keeps a tombstone of the revoked token — and stops syncing with everything kept locally.
  The owner keeps each member's history under `fleet-ingest/<member_id>/`. ([#762])
- **Fleet members agree on which repository is which.** A repository now has one fleet identity
  however each machine cloned it: its root commit SHA(s), which survive forks, mirrors and remote
  renames, with the origin URL normalised as the fallback (SSH, HTTPS and scp-like spellings compare
  equal; user names, tokens and ports are dropped; github.com paths compare case-insensitively).
  Matching tries the roots first, then the URL, and an ambiguous match is no match rather than a
  guess. `colonizer fleet export` records each colony's `repo_identity` in the bundle's history,
  read from the machine's own mirror, and never the raw remote. It is the key placement (#688) and
  per-repo cost (#689) build on. ([#763])
- **Each fleet member shows one health state, with the reason and what to do.** `GET /api/fleet`
  now gives every member a `health` object — `{state, code, reason, hint}`, where `state` is `ok`,
  `unknown` (not polled yet, so never claimed healthy), `degraded` or `stopped` — worked out from what the owner already sees: how long the member went
  without answering the fleet poll, whether the latest poll reached it, how full its disk is, whether
  its fleet token still exists, and whether it published a URL to poll at all. Every problem comes
  with one next step ("Token revoked: re-pair this machine", "Disk 97% full: clean target/ dirs",
  "No heartbeat for 12 min: the machine may be asleep"); when several fire, the worst state wins and
  a fixed order breaks ties. Settings → Fleet shows it as a coloured badge beside each member, with
  the hint underneath. Each member's status answer also reports its history-push drain state and
  how long ago its queue loop last ticked, so a member whose sync drew a 401 or 403 reads stopped
  ("Token revoked"), one whose drain has left rows unsent for an hour reads degraded ("Sync behind by
  N rows"), and one whose queue loop has not ticked for five minutes reads degraded ("Colony runner
  not ticking"); history sync switched off is shown as a note, never as a fault. ([#764])
- **Commit links follow a pull request's head, and the cockpit shows them.** When the PR watcher or the
  merge train sees a colony's head change (its own force-push, GitHub's update-branch), the mothership
  fetches the branch into its mirror and re-points the colony's commit links; a `sync_repo` fetch that
  moved a colony branch does the same. An unchanged head costs nothing. The links are served at
  `GET /api/sessions/{id}/commits` and listed under **Commits** in the colony pane, where an orphaned
  link carries a badge explaining that a squash or rewrite made the match ambiguous, so it was kept
  rather than guessed. ([#765])
- **A colony's commits stay linked to it through rebase, amend and force-push.** Each publish now
  records the commits it pushed in the colony's `commits.json`, with their `git patch-id --stable`,
  the colony and the agent session that wrote them. When the branch is rewritten, a reconcile
  re-points each link to the one commit on the branch with the same patch-id. It runs after the
  watcher's auto-rebase and at every publish. It never guesses: a squash, a content-changing amend
  or several candidates leave the link in place flagged `orphaned`. A git failure changes nothing,
  and a rebase in progress is skipped. See [docs/colonies.md](docs/colonies.md#which-commits-a-colony-wrote).
  ([#765])
- **The out-of-quota card can switch every role at once, and remember a non-Claude fallback.** The
  card's Switch (and Settings → Providers) now offers **every role using this provider**: every model
  role in the install's agent settings (orchestrator, subagent, background, summary, small- and
  large-task models) and every org override that points at the exhausted provider move to the chosen
  model, and the card's colonies restart on it. Each role is checked first, so a model one of them
  cannot take (Claude as the summary model with no Anthropic API key, say) changes nothing; afterwards
  the cockpit lists each setting as "was X → now Y". **Remember as fallback** and the provider form's
  fallback picker now also take a model on another provider that speaks the same wire (anthropic to
  anthropic, openai to openai): when the provider answers quota exhausted, the Mothership retries the
  request there itself. A cross-wire fallback is refused with the reason. ([#767])
- **A "Provider out of quota" card instead of colonies that hang.** When a provider's plan runs out,
  the gateway now ties every colony whose requests come back quota-exhausted (with no Claude fallback)
  to that provider, and the maintainer gets one card per provider — in the inbox's "Needs you" list and
  at the top of Settings → Providers — naming the provider and model, the reset with a countdown, and
  every blocked colony. **Switch model** moves those colonies, or their orgs' model settings too, to a
  healthy model from a picker that shows each model's failure rate, and restarts them on it (a Claude
  pick can be remembered as the provider's `fallback_model`); **Wait until reset** parks them, not fails
  them, and the queue resumes them at the reset; **Stop** stops them. A colony blocked on an exhausted
  provider, including one still `starting` whose agent never got a turn out, is flagged
  `provider_quota_exhausted` instead of being nudged, and the watchdog's final "this colony needs you"
  flag is no longer cleared by gateway traffic alone. New endpoints: `GET /api/attention` and
  `POST /api/providers/{id}/quota-action`; `GET /api/status` carries `quota_cards`. ([#767])
- **A built-in Disk cleanup loop keeps builds from filling the disk, off until you switch it on.**
  Every install now has a **Disk cleanup** row on the Loops page. Switched on — the first time, after a
  preview that lists what a run would remove, path by path with sizes — it runs every hour and early
  whenever free space drops under 15%, removing git-ignored build output (`target/`, `node_modules/`,
  `.next/`, `dist/`) from finished colonies' worktrees, the worktrees the automatic reclaim would take,
  and microVMs no colony owns; old session archives and Cargo `target/` dirs under paths you list are
  opt-in. It never touches live or waiting colonies, keep-worktree colonies, uncommitted or unpushed
  work, `.git`, `~/.cargo`, package caches or anything outside its roots. Each run records what it
  freed per category in the loop's history and in History, and a run that leaves the disk under the
  threshold raises an attention item. From a terminal: `colonizer loop run disk-cleanup --dry-run`
  and `colonizer loop enable disk-cleanup`.
- **A Docs & README loop keeps documentation in step with the code, off until you enable it.** Name a
  repository or an org on the Loops page (or `POST /api/docs-loop/enable`) and a daily run — hourly at
  the most — reads the mothership's clone of each one without a model: merged changes to an
  identifier, flag or route a doc names, in code it describes, that left every doc alone (a docs
  map derived from the docs' own links, which a `.colonizer/docs-map.toml` can extend), broken relative links and anchors in README files and
  `docs/`, commands and flags the docs show that no longer exist, API routes added or removed since
  the last run that `docs/protocol.md` does not reflect, and missing changelog entries (a warning only, as the repository's own check has it) where the
  repository keeps `changelog.d/` or `## Unreleased`. A repository with findings gets one colony, told
  to change only documentation, keep the house style, claim nothing the code does not do, keep the
  diff small and run the repository's docs checks; none is dispatched while a docs colony or docs
  branch is open, within the cooldown (24 hours by default), or with external writes blocked. Every run's report is
  kept in the loop's history and the activity log, the last one shown on the Loops page, and **Dry
  run** reports without writing anything. See [docs/loops.md](docs/loops.md#docs--readme).
- **Security preset for red-team runs.** A run can now use `"preset": "security"` (the wizard's
  **Preset** choice, the API and schedules, or `colonizer redteam start --preset security`): eight
  security focus areas with the same cycling and briefs, a proof attached to every finding, and hunters
  kept to the repository and a local instance. Before the hunters launch, a deterministic pre-scan of
  the host mirror (no model tokens, no repository code run; gitleaks when installed, a built-in
  fallback otherwise) turns committed env files, secrets, client-exposed keys, lockfile gaps, open
  CORS, string-built SQL, unsigned webhooks, public buckets, disabled row level security and risky
  agent files into leads dealt to the matching hunter. The run's report adds the leads and an operator
  checklist that is never marked passed, and the synthesis ranks by severity with proofs. General runs
  are unchanged. See [docs/red-team.md](docs/red-team.md#the-security-preset).
- **A built-in "Dependencies & supply chain" loop.** Off by default and opt-in per org or repository,
  it checks each opted-in repository's lockfiles on the mothership's mirror (never inside a colony)
  with the scanners already on the host — `cargo-audit`, `cargo-deny` for a `deny.toml` licence
  policy, `npm audit` and `osv-scanner` — or the mothership's own OSV lookup, and says which scanner
  to install when none reads a lockfile. Fixable findings become one colony per repository and
  ecosystem with a brief asking for minimal bumps and nothing else; duplicates of an open target,
  the per-repository cooldown, the per-run caps and `COLONIZER_NO_EXTERNAL_EFFECTS` hold a dispatch
  back. Every run is reported on the Loops page and in the activity log, a critical or high finding
  with no fix raises an attention item, and a dry run writes nothing. See
  [docs/loops.md](docs/loops.md#dependencies--supply-chain).
- **A built-in "TypeScript: remove any" loop.** Off by default and opt-in per org or repository, it
  counts the explicit `any` in each opted-in TypeScript repository on the mothership's mirror (never
  inside a colony, and without a model): with the repository's own TypeScript when `node` is on the
  host and its dependencies install offline from the host's cache, and with a token scan that skips
  comments and strings otherwise, saying which method ran. Once per run and repository, the module
  with the most `any` goes to one colony as a batch of at most 20 `file:line` occurrences, with rules
  against casts, `@ts-ignore`, `eslint-disable` and new `any`; an open colony on the module, the
  per-repository cooldown, the per-run cap and `COLONIZER_NO_EXTERNAL_EFFECTS` hold a dispatch back.
  When the batch's pull request is published it is counted again, and a module count that did not
  drop or added suppressions raise an attention item. Every run's totals, trend, dispatches and skips
  are on the Loops page and in the activity log, and a dry run writes nothing. See
  [docs/loops.md](docs/loops.md#typescript-remove-any).

### Changed

- **The pinned microsandbox moved to 0.7.3.** `vendor/vendor.lock` now pins the v0.7.3 release
  tarballs from the project's current GitHub organization, sha256-checked against upstream's
  published checksums. Every `msb` invocation the harness makes (boot, remove, list, image list,
  pull, and all the network and secret flags) is unchanged on the 0.7.3 binaries. Suspension still
  resumes the agent's own session transcript rather than snapshotting the microVM: 0.7.3 gained
  the capture (`msb snapshot create --full`), but its measured restore cannot bring a colony back —
  a sandbox that has ever carried a secret fails its restore, and restore accepts no `--secret`
  that could re-register one — so `sandbox::supports_memory_snapshot()` stays false and
  `path: "session_resume"` remains the rule. See [docs/architecture.md](docs/architecture.md).
  ([#639])
- **The claude-code egress declaration comes from a live capture of the pinned CLI, no longer from documented guesses.** A proxy-and-tcpdump capture of Claude Code 2.1.280 inside a colony sandbox (2026-09-28) replaced the old declaration: `api.anthropic.com` was the only host observed across the runner invocation, CLI defaults, `claude doctor` and the login flows, the OAuth token exchange sits on `platform.claude.com`, and the CLI's hardcoded Datadog and Sentry intake hosts are now declared as telemetry — while `console.anthropic.com` and `statsig.anthropic.com`, absent from the 2.1.280 binary, are dropped. `api.typesafe.ai` stays in `extra`: it is the module's own Jev plugin, not the CLI. ([#655])
- **Autopilot verification checks what the diff touches, and compares a failure against the base
  before holding the colony.** With `verify: auto` a diff with no Rust in it no longer runs
  `cargo test`; a `web/` change runs web's own test script instead of the root's, and files nothing
  else covers fall back to the root `make test` only when the repository declares one. A check that
  fails now runs once more on the merge-base, in a fresh checkout of its own: failing there too is a
  new `inconclusive` verdict — the colony did not break it, so autopilot publishes with a note in the
  pull request instead of holding — and only a failure new against the base holds the colony. A
  failing check leaves its last 200 lines in the colony's `out/verify-<check>.log`, and the failing
  test names ride the hold message, which the cockpit banner now shows. ([#672])
- **A README hero: the pitch, one install command, the links and the demo.** The full-width banner gives
  way to a compact centred hero above the fold: every task runs in its own microVM and your secrets stay
  on the host, the install command, links to the local mock cockpit, the docs, the architecture and the
  audit, and the demo GIF — `node docs/media/record-demo.mjs` regenerates `docs/media/demo.gif` from
  the cockpit's mock mode and renders the 1280×640 `docs/media/social-preview.png` for the repository's
  social preview. The audit caveat moves out of the intro prose to one line directly below the hero, and
  `assets/readme-banner.svg`, the banner the hero replaces, is deleted.
  ([#683])
- **A new contributor reaches a first pull request in 15 minutes.** CONTRIBUTING.md gained "Your first PR in 15 minutes": from a fresh clone, Node 24 and a minimal rustup toolchain are enough to build and test the web, the guest agent or the docs — no KVM, no microVM. docs/good-first-issues.md lists ten open issues with a "Start here" hint into the code for each. ([#684])
- **Tests build agent modules through `AgentModule::test`, never a struct literal.** The test-only
  constructor puts every `AgentModule` field's neutral default in one place (chainable setters for
  what a test varies), so adding a field to the struct no longer breaks a dozen test helpers across
  queue, usage, providers and sessions; the manifest parser's literal stays on purpose. ([#707])

### Fixed

- **The relay's tunnel tests no longer flake on busy CI runners.** Verifying a mothership hello is asynchronous, and the test fakes waited for it by counting event-loop turns — a budget that a CPU-starved runner could exhaust before the crypto finished, failing healthy runs with `hello was never verified`. The relay now exposes an `onEstablish` hook to its test harness, and the tunnel tests await verification, response timeouts, and body-idle re-arming as events instead of polls and sleeps. ([#728])
- **A question asked by a subagent no longer loses the subagent.** A subagent's `AskUserQuestion`, like an exec-policy approval, blocks a tool call in flight, but the mothership treated it as an ordinary question and suspended the colony after the grace period, which killed the subagent and left the answer with nobody to receive it. The Claude Code runner now marks such a question `blocking: true` (from the SDK's `agentID`, or from the question arriving in a subagent's message), the ACP runner marks every permission request the same way, and the mothership keeps those colonies running while they wait. The lead agent's own questions still suspend as before. A blocking question is not held for ever: after two hours without an answer the colony is suspended anyway, and the colony log says that the waiting agent is lost and that the answer will reach the lead agent on resume. ([#759])
- **A colony waiting on an exec-policy approval no longer loses its agents.** When an `ask` rule such as `writes-outside-repo` put a command to you, the mothership could suspend the colony while it waited, which killed the subagent blocked on that command; the lead then spawned another that asked the same thing, and colonies lost several agents without doing any work. Exec-policy questions now carry `kind: "exec_policy"` and such a colony keeps its microVM until you answer, while ordinary questions still suspend as before. An Allow is also remembered for the rest of the colony's run, so the same command under the same rule is not asked about again, even by a newly spawned subagent; other commands still ask, and a Deny still refuses. ([#759])
- **The cockpit no longer black-screens when a colony spawns subagents.** A subagent could reuse a
  tool-call id that also appears in the orchestrator's blocks, and `buildThread` emitted both parts, so
  assistant-ui's `useResources` threw `Duplicate key toolCallId-… in useResources` and the error
  boundary looped into a blank window. `toParts` now dedupes tool-call ids across the whole thread — the
  first part for an id wins and later repeats are dropped — with a regression test in
  `web/src/sessionStream.test.ts`. ([#774])
- **Colonies booted before model scoping keep working after an upgrade.** #727 records the models each colony may use at boot, so a colony already running when the mothership was upgraded had no model list and every gateway request (every subagent) was refused with "model … is not routed to colony". Such a colony now keeps the provider-level scope it booted with until its next boot; a provider outside its record is still refused.
- **Resuming a colony no longer needs GitHub.** A resume re-fetched the colony's issue with `gh issue view`, so a GitHub that refused (a suspended account, a rate limit) or could not be reached marked the colony failed, although its brief, worktree and branch were all on the mothership. The first boot now stores the issue in the colony's session directory (`issue.json`) and a resume boots on that copy; a colony started before this recovers the issue from its first brief (`vm/session.json`, then its event logs). The resume's refresh of the base branch is best effort and only warns ("resumed offline: base not refreshed"), no identity check gates a resume, and with `COLONIZER_NO_EXTERNAL_EFFECTS` set a resume stays fully offline. A new colony still needs GitHub, and a suspended account is now named as one instead of being sent to reconnect.
- **`colonizer-harness` publishes to crates.io again.** It depended on `cratefield-module-telemetry` through a git pin, which crates.io refuses, so the v0.1.10 crates job failed after the GitHub release had shipped. It now uses the published `cratefield-module-telemetry` 0.2 (on `cratefield-core` 0.6).

### Security

- **Host git over colony content runs with a clean config and a scrubbed environment, and no
  publishing credential.** Every host-side git command now pins the global and system git configs
  out (`GIT_CONFIG_GLOBAL=/dev/null`, `GIT_CONFIG_NOSYSTEM=1`), resets credential helpers, and runs
  from an environment allowlist that drops tokens and `GIT_*` overrides — so a content filter a
  worktree's `.gitattributes` names has nothing defined to run and no token to inherit. The
  credential helper and token now ride only on an explicit authenticated variant used by `fetch`,
  `push`, `ls-remote` and `clone`, which read no worktree content; the mothership's own auto-rebase
  and the reclaim and catch-up paths get the same hardening, and a host-side rebase stamps its
  commits with a fixed host identity since the global config no longer supplies one. The
  authenticated variant keeps only the host's `url.*.insteadOf`/`pushInsteadOf` rewrites from its
  config, and a git credential prompt nobody can answer now fails a boot at once with a clear
  message instead of being retried for 20 minutes. ([#681])
- **Publishing waits until the colony's microVM is confirmed removed.** Publishing stops a live colony's agent and removes its microVM, but then went straight to work on the worktree without checking the removal had actually happened. The removal is now confirmed against the node's own sandbox list (`msb ls`, running or not) before the publish starts: if the removal fails, is still listed, or the list itself cannot be read, the colony is marked failed with a fixed message and nothing is committed, pushed or opened — the publish never reaches the worktree's git setup or its contents, which stay as the colony left them. Publishing again runs the confirmation first. The same confirmed removal backs the other teardown paths, which only say so on the colony's log. ([#681])
- **Changing a provider's base URL to another origin now requires entering its API key again**

  A connection's credential is sent wherever its `base_url` points — the gateway forwards it there and
  the health probe follows — so `PUT /api/providers/{id}` refuses a save that moves a keyed provider to
  a different origin (scheme, host or port) unless the API key is entered again or removed with the
  save; a path change on the same origin keeps the saved key. The quota probe is held to the same rule
  when the base URL moves out from under it. The cockpit's provider form says so before the save
  instead of after. ([#681])
- **Secrets are redacted before a colony's logs are written.** A token an agent echoes, a
  connection string a tool prints or a key in a request path is replaced with `[REDACTED:<kind>]`
  before the line reaches `events.jsonl`, `harness.jsonl` or `gateway.jsonl`, the cockpit's live
  view, or a log archive bundle, so nothing downstream (an export, a fleet sync, a shared archive)
  carries it. Detection is layered: known provider token prefixes (GitHub, Anthropic, OpenAI, AWS,
  Stripe, Slack and more), private keys, JWTs and bearer values, passwords in URIs and database or
  broker connection strings, secret-named `KEY=value` pairs and JSON fields, and long high-entropy
  strings as a last resort. Git SHAs, UUIDs, lockfile hashes and base64 images are left alone, and
  JSON lines stay valid JSON. ([#761])
- **OpenCode, Pi and ACP colonies pull shared memory through tools too.** The memory tools from the Claude Code, Codex, Grok Build and Hermes modules now reach the other three runners: OpenCode's colonizer MCP server serves `memory_briefing`, `memory_changes` and `memory_search`, Pi loads them as a Pi extension, and ACP agents get them from an MCP server registered on their session. Each answer is sourced (colony, repository, commit) and framed as data to verify, and a revoked note is gone from the next answer. None of these runners puts note text into the prompt: OpenCode's instructions no longer point the agent at the notes directory, and each carries one fixed line naming the tools (for ACP, at the head of a new session's first message). ([#766])
- **Shared memory is pulled through MCP tools, never injected into a colony's prompt.** No note text goes into a colony's system prompt or first message any more: the prompt carries one fixed line, and the agent asks for memory with `memory_briefing` (a short summary, each entry with its kind and its source: colony, repository and commit) and `memory_changes` (what was added or revoked since it last asked), on the Claude Code, Codex, Grok Build and Hermes modules. Every entry now has a kind (plan, decision, file-change note, failure, architecture note, convention) and keeps its provenance. A colony can no longer propose fleet-wide memory directly: a global proposal only reaches the review queue once colonies in two different repositories propose it with confidence of at least 0.8, and it is always reviewed. Repository notes stay with their repository. `POST /api/memory/notes/{id}/revoke` takes a note back: colonies stop seeing it at their next briefing, and the mothership keeps a record of what it was and where it came from; `GET /api/memory/candidates` lists what is waiting on a second repository. ([#766])
- **An ACP agent can no longer write outside its workspace through a dangling symlink.** The ACP runner confined `fs/read_text_file`, `fs/write_text_file` and terminal working directories by resolving the longest existing part of the path, so a symlink in the workspace pointing at a file outside that did not exist yet passed the check as an in-tree path, and a write through it created the file outside. Paths now resolve symlink by symlink, dangling ones included, against the link's own directory, and must stay under the workspace's real path; symlink loops and chains longer than 40 links are refused. The path-policy report shared with the Claude Code runner uses the same walk, so it names where a write through such a link would actually land.
- **Log archives and fleet exports no longer carry a colony's credentials.** A session directory
  holds live bearers: agentd's `vm/token`, the colony's `gateway-token`, the mesh auth key and
  `vm/session.json` (whose runner env repeats the gateway bearer). The log archive tarred all of
  them, so anyone holding a bundle could drive a colony that was still running or could be resumed.
  These files, and anything else in a session directory named like a credential (`*token*`,
  `*authkey*`, `*.key`, `*.pem`, `secrets*`), are now left out of every archive bundle and fleet
  export, including the export's fallback to an older archive bundle. The fleet export also redacts
  the logs it carries, and the findings ledger (`findings.jsonl`) is redacted as it is written.
  ([#761])
- **Secrets are redacted from every file the mothership writes from agent or model output.** A
  credential quoted in a filed finding, an independent review, a colony's `pr.md`, a chat message,
  a colony summary or an activity-log line is now stored, and published to GitHub, as
  `[REDACTED:<kind>]`: that covers `finding-body.md` and the issue it files, `review.md` and the PR
  comment, the commit subject, `pr-body.md` and the pull request, `chats/<id>.jsonl`, the summaries
  in `sessions.json` and `activity.jsonl`. A rotated `events-N.jsonl` written before redaction
  existed is redacted when it is read back into the cockpit's diagnosis or a resumed colony's
  prompt. Redaction is never silent: the colony's log names what was redacted (`pr.md contained 1
  secret (github token), redacted before publishing`), and autopilot holds a colony whose `pr.md`
  carried a secret until a person presses Create PR. ([#761])

### Take care

- **The microsandbox home migration is one-way.** The first command a 0.7 `msb` runs migrates
  `MSB_HOME` (`~/.microsandbox`) in place. The migrated home still lists and removes microVMs a
  0.6 `msb` created, but after it a 0.6 `msb` fails every command against the home (`database
  schema is newer than this msb binary`). To roll the harness back to a 0.6-era release, downgrade
  the home first, with the 0.7 binary: `msb self downgrade 0.6.18 --yes` (it refuses while
  sandboxes are active, backs the database up and purges the image cache, re-downloaded on the
  next boot). See [docs/install.md](docs/install.md). ([#639])

## [v0.1.10] - 2026-09-29

### Added

- **A quota probe per provider.** A provider can now carry `quota: {url, pointer}` — a `GET` and an
  RFC 6901 JSON pointer into its answer — fetched with the provider's own credential whenever
  `GET /api/providers/{id}/health` runs, so the cockpit shows what is left in a prepaid token plan. The
  probe rides along on the reachability check and never changes its verdict; its URL must sit on the
  base URL's origin — scheme, host and port, because the credential is sent there — and its pointer
  must start with `/`. Saving with `quota` omitted keeps the stored
  probe, an empty URL clears it, and with no probe the `quota` field stays out of the health answer.
  ([#199])
- **A token budget per colony.** A new `budget_tokens` sandbox setting caps the tokens one colony
  routes through the gateway, counted for every routed response whether or not the provider prices it —
  a prepaid token or coding plan prices nothing, so its colonies cost $0 and the USD budget can never
  trip; this is what holds them. Enforced exactly like `budget_usd`: past the budget the colony is
  stopped with its worktree kept, its next routed request is refused with `403`, and raising the budget
  and pressing Resume continues. It is global, with no per-org override; `0`, the default, means
  unlimited. ([#199])
- **A workspace dashboard beside the nest.** With no colony picked, the cockpit home's right-hand
  pane now summarises the chosen workspace's colonies — working against parallelism, needs-you with
  each colony's reason, queued, returned, failed or stopped, total spend, and the last colonies to
  move — read from the same scoped list the nest draws, so switching workspaces switches the panel
  with it. Clearing the workspace filter aggregates every workspace under "All workspaces". It
  occupies the inspector's slot, gives it back on a chamber click, and yields on narrow windows to
  a Dashboard button parked above the nest's own view toggle. ([#200])
- **Parking is a real status: `parked`, no longer `Stopped` wearing a flag.** A colony the host sets
  aside — the provider's quota ran out, or an autopilot hold outlived its slot — now carries a
  `parked` record (`{at, reason, resets_at?, vm_kept}`) instead of masquerading as stopped: it holds
  no parallel slot, is never auto-reclaimed, and reads as paused rather than finished everywhere a
  finished colony once matched. Parking persists the record first and tears down only after git
  verified the worktree (uncommitted work is the only copy of unpushed work), so an unverifiable
  worktree keeps the microVM running instead. Resume honours that: a park that kept the microVM
  resumes warm — the agent, still idle in its machine, is told to pick up where it left off — and
  every other resume boots cold with a short digest of the previous run's event log in the brief,
  so the colony knows its own story. A recovered provider recovers both park shapes: a
  discarded-VM park rejoins the queue, and a kept-VM park is routed through the same resume (warm
  when possible, cold otherwise), so nothing sits parked on a provider that has long since
  recovered — and a parked colony can be published directly, the cold push a stopped one takes.
  Whether parking discards the microVM at all is the new `resume` module's `discard_vm` setting
  (default on). ([#213])
- **A scan runner for the security hunters.** `hunters::scan` now drives an installed,
  checksum-verified hunter end to end from its manifest: it renders the `scan` template into argv
  without a shell, runs the binary from a working directory with the hunter's LLM client pinned to
  the Colonizer gateway — required for a scan, so hunter spend can never bypass it — then reads the
  run's artifacts back through the manifest's parser into `Finding`s, copying `llm_usage.cost` out
  of Strix's `run.json` so a run's spend is visible from both sides. Fatal exits (Strix: anything
  but 0 and 2), claimed vulnerabilities with no artifacts behind them, a missing binary and a
  four-hour deadline all fail loudly. Nothing decides when to scan yet, and Strix still needs a
  Docker daemon inside the colony. ([#216])
- **Spend you can attribute.** Every colony-scoped row of the spend journal (`<data>/spend.jsonl`)
  now names the colony (`session`) and the agent module — the harness — that ran it, and the routing
  decision recorded at boot carries the same `agent`, so a routing choice can be joined with what the
  colony went on to spend. `node scripts/colony-report.mjs --costs` reads the journal grouped per
  colony: the agent's own turn-end estimate against what the gateway metered for routed providers,
  shown separately (a `–` is unmeasured, never $0), colonies with tokens but no measured dollar
  labelled `unpriced — tokens only`, rows with no colony — chat, and older rows — under
  `unattributed` so the totals still sum to the whole window, and a harness × model rollup for the
  question the per-colony table cannot answer. The report defaults to the spend history's own
  window — the last 30 days ending today, `--days` changing the length and `--since` the floor —
  and names it in its Total line and `--json`. Bench comparisons gain a Harness · model column per
  task. See [docs/protocol.md](docs/protocol.md) §6.8 and [docs/bench.md](docs/bench.md). ([#296])
- **A published UHP conformance report, and a CI gate that keeps it honest.** [docs/conformance.md](docs/conformance.md)
  says plainly where the mothership stands against the Unified Harness Protocol — not conformant
  (no class) at spec 2026-09-12, with every failing check named and each gap either followed up or
  accepted — and `docs/uhp-conformance.json` records the per-check outcomes the new `conformance`
  CI job re-measures on every pull request: `scripts/ci/uhp-conformance.sh` builds the mothership,
  boots it on loopback with throwaway config and data, runs the commit-pinned suite, and fails when
  any check's outcome moved, so a protocol-behaviour change is re-measured deliberately instead of
  absorbed. The same run validates the Claude Code runner's event fixture against
  `docs/agent-events.schema.json`. ([#297])
- **The hosted contract, and its first two pieces in the code.** Setting
  `COLONIZER_DEPLOYMENT=hosted` makes the mothership refuse to save a credential without
  `COLONIZER_MASTER_KEY` — nothing is written and the existing value is left alone; `local`, the
  default, keeps the documented plaintext 0600 fallback. `GET /api/upload/manifest` returns the
  manifest an upload would carry before anything is uploaded: `copied` is an allowlist of
  module selections, providers without their keys, org overrides and skill-pack pins, `stays_home`
  names the rest, and the `digest` is what a future confirm step echoes back. The design contract
  that keeps a hosted deployment the same API as a laptop — routes, events, errors, the
  workspace-credential seams, credential recovery when the master key changes, the outpost seam
  and the audit gate — is in [docs/hosted.md](docs/hosted.md). ([#298])
- **An escape-vector review checklist for the sandbox.** `docs/escape-vectors.md` tests each documented colony-escape class against the mount / TLS-proxy / mesh setup and records a verdict per vector — blocked (with the exact mechanism), open (with a follow-up), or not-applicable (with the reason) — plus a per-release sign-off line and a manual KVM procedure for the attempts that need a live microVM. New publish-path regression tests assert the `.git` rewrite, nested-repo stripping and hook/fsmonitor neutralisation that `git`-shaped smuggling relies on. ([#299])
- **Path policy: masked and protected worktree paths.** Credential files in a checkout — `.env`,
  `.envrc`, `.npmrc`, `.netrc`, `.git-credentials`, `.pypirc` — are now masked out of a colony's
  view (the guest gets an empty file instead, before the agent starts), and agent-facing config —
  `.git/config`, `.git/hooks/`, `.gitmodules`, `.claude/`, `.codex/`, `.mcp.json`, `.devcontainer/`,
  `.vscode/`, `.idea/` — is pinned read-only. Three sandbox settings add to the lists or opt paths
  out of them (`mask_paths`, `protect_paths`, `unmask_paths`; every opt-out is logged at boot);
  unusable entries are refused at save time. At publish, empty boot placeholders are removed before
  staging and changed masked or protected paths are logged on the colony — reported, not rewritten.
  See [docs/path-policy.md](docs/path-policy.md). ([#300])
- **In-guest hardening for the agent.** The colony's agent runs as root in its microVM, and three
  layers now bound what that root can do before its first instruction: `boot.sh` locks the guest's
  kernel interfaces down (`dmesg_restrict=1`, `kptr_restrict=2`, `hidepid` on `/proc`, the readable
  `/proc` files masked with `/dev/null`, an empty read-only tmpfs over debugfs, tracefs, BPF,
  firmware and the ACPI/SCSI/ALSA corners, `/proc/sys` and `/sys` remounted read-only); agentd
  makes itself non-dumpable with core dumps off, so the agent can neither see nor read the
  daemon's `/proc` entries; and the runner child — the agent and everything it spawns — is exec'd
  with 21 capabilities dropped (mount, ptrace, `SYS_ADMIN`, BPF, kernel modules out;
  package-manager caps in), core dumps off, `no_new_privs`, and a seccomp denylist that turns
  io_uring, userfaultfd, mount, namespaces, ptrace, kexec and friends into ordinary `EPERM` tool
  failures instead of kills. Landlock path pinning waits for a libkrunfw built with it — measured
  2026-09-25, `landlock_create_ruleset` returns `ENOSYS` on the pinned stack's Linux 6.12.99. See
  the In-guest hardening section of [docs/architecture.md](docs/architecture.md). ([#301])
- **Every gateway request leaves one audit line.** The provider gateway now appends a per-request
  record to the colony's `gateway.jsonl`: what was asked for (provider, wire, method, path, the
  requested and sent model), how it ended (status, a typed failure code, whether the Claude fallback
  was licensed, queue and total duration, bytes, token counts) — and nothing else. The record is a
  fixed struct that is the whole allowlist, so keys, tokens and request bodies never reach the log,
  and the colony's own credential headers are never forwarded upstream. `colony-report` counts each
  colony's gateway requests and failures, Settings shows a provider's last failure code beside its
  failure rate, and the provider-degraded notification carries it. ([#302])
- **An egress policy on the colony fence.** Every colony now boots behind a configurable two-class
  egress policy (`crates/colonizer/src/egress.rs`): `open`, today's behaviour, and `allowlist`, which
  names no public profile, sets `--net-default-egress deny` and reaches only the hosts on the allow
  list. Both modes compile a non-overridable always-blocked deny set — cloud metadata, private and
  loopback ranges, CGNAT, benchmark and reserved ranges, NAT64, DNS64 and 6to4 prefixes — ahead of
  every configured allow, so no setting can reopen them, with `allow@dns` in front so name
  resolution survives. Sandbox settings gain `egress`, `egress_allow` and `egress_block` (validated
  at save time); an org can fix the mode and extend, never shrink, the lists. Each boot records the
  resolved policy to `<session>/egress.json` and serves it at `GET /api/sessions/{id}/egress`. The
  policy is applied at boot: a change takes a stop plus Resume. See the Egress policy section of
  [docs/sandbox-network.md](docs/sandbox-network.md). ([#303])
- **Declared egress, denial hints, and the boundary/guidance split.** Every agent module's
  `module.json` now declares the hosts its agent may reach — `egress: {api, auth, telemetry,
  extra}`, bare hostnames, a leading `*.` allowed — validated by a test that walks every manifest
  and requires each `secrets[].hosts` entry to be covered; deriving a colony's enforceable
  allowlist from it is a follow-up egress-policy issue. The claude-code runner classifies denied
  tool calls (`classifyDenial(text)` → `egress`, `read_only` or `tool_disabled`) and adds
  `denial: {class, hint}` to the errored `tool_result` event — `is_error` and content unchanged —
  and repeats the hint to the agent itself through a `PostToolUseFailure` hook's
  `additionalContext`, at most once per class per session.
  Two docs: [docs/boundaries.md](docs/boundaries.md) — what the harness enforces (boundary) versus
  what it only suggests (guidance), how a finding classifies on arrival, and the planned watchdog
  signatures on denial events — and [docs/runner-authoring.md](docs/runner-authoring.md), the
  checklist for a new agent module. ([#304])
- **Red-team synthesis.** A red-team run that finds something launches one more colony at `done` —
  the synthesis judge — which merges the hunters' findings into a single ranked report, tracked by
  the run's `synthesis` object and the tally's `merged` count. It fires exactly once, publishes
  nothing, and is retried via `POST /api/redteam/runs/{id}/synthesize`. ([#309])
- **The offline evolver loop, first slice.** `node scripts/evolve.mjs` turns what the bench and
  red-team runs already recorded into failure classes, prompt-only proposals against one agent
  module, and a retained-or-discarded verdict measured on a rerun of the same bench tasks: cost
  bounded, a single task regression flags the whole proposal, and a widened trajectory-monitor gap is
  never silently retained. It reads logs and writes proposal files into `<data
  dir>/evolver/proposals` — it launches no colonies, edits no repository files and opens no pull
  requests; writing the proposed prompt text and approving each proposal stay with you, and approval
  hands you the `git apply` for an ordinary pull request. See [docs/evolver.md](docs/evolver.md).
  ([#310])
- **A shared anti-spam ledger for the mothership's proactive messages.** Every outbound proactive
  action — a notify announcement, an autonomous judge answer — is now counted in one ledger
  (`<data_dir>/ledger.json`) before it leaves: duplicate facts within a window are dropped, a
  question that blocks its colony bypasses the soft layers but never the hard ones (quiet hours are
  configured, not yet on by default; a per-topic daily cap and per-kind hourly and daily quotas
  always are), and what the soft layers hold is summarised once an hour as one line ("Colonizer: 5
  held announcements — provider_degraded ×2, question ×3") with counts by class only, no colony ids
  or question text. The tallies ride the authenticated `/api/status` as `ledger`. The watchdog's
  nudges join the ledger in a later slice. ([#311])
- **Wait behind the holder.** A launch on an issue another colony already holds can now queue for
  the issue instead of being refused or duplicating it: `queue_behind_holder` on `POST /api/sessions`
  admits the colony as a `claim_wait` successor (`queued_behind` naming the holder), the oldest
  waiter takes over when the holder releases — carrying the GitHub `colonizer:claimed` mark with
  it — and the cockpit's launch form offers the choice beside Allow duplicate, with each waiter's
  place in line on its Inspector card. ([#321])
- **The mothership verifies a completion claim before publishing it.** When a colony's turn ends
  having written `pr.md`, the host snapshots its work without touching the worktree, reads the git
  state itself (commits ahead of base, changed files, the paths the PR description names) and re-runs
  the repository's test command in a fresh one-shot microVM over a git archive of the snapshot —
  never on the host, never trusting the agent's own logs. A confirmed claim publishes as before, an
  unverifiable one publishes too (said so, never counted as confirmed), and a contradicted one holds
  autopilot with `autopilot_held` and the contradictions stated. What runs comes from the `publish`
  module's new `verify` setting — `auto` (the default) reads the repository's own test declaration
  from the base branch (package.json, Cargo.toml, Makefile), `none` opts out per colony or globally,
  or an explicit command — and the verdict lands in the colony's event log as a `verification` host
  event. ([#328])
- **Trajectory monitor: resolved versus clean-resolved.** A post-hoc audit of a colony's persisted event log — every archived `events-N.jsonl` and the current one — for the shapes of shortcutting (history mining, weakened tests, writes to what the scorer executes, solution fetches, unflagged injections), as a versioned, calibrated pattern set: any pattern whose false-positive rate on the committed calibration set's normal transcripts passes its budget is demoted to advisory automatically, so it reports without judging. `node scripts/trajectory-monitor.mjs --session <id> [--bench run.json | --calibration]` reports hits with redacted evidence and logs its own operation to the colony's `audit.jsonl`; `scripts/bench.mjs run` records `clean` and `hacks` per result, its summaries add `clean_resolved`, `hacked_resolved`, `clean_rate` and `gap`, and comparisons gain the clean verdict and the gap. The contract a future Evolver consumes — fitness is the clean rate, and a proposal that widens the gap is rejected — is fixed in [docs/trajectory-monitor.md](docs/trajectory-monitor.md). ([#329])
- **External calibration against SWE-bench.** `scripts/swebench.mjs` runs the colonies on work nobody
  here chose: each instance becomes a private single-commit snapshot of the upstream repo (no history, no
  eval artifacts, no remote), runs under a required budget envelope that stops cleanly without
  extrapolating unpaid tasks, and is scored by the official SWE-bench harness, with raw and clean rates —
  a patch that edits the hidden tests is flagged and kept out of the clean count. Stages: Lite, then
  Verified, then Multilingual. Runs stay labeled uncalibrated until the remaining controls (#330's gold
  sanity gate, network and trajectory monitoring) land. ([#331])
- **Exec policy: programmable deny/ask/allow rules over the commands a colony runs.** Every Bash
  command — and, for `bash x.sh` / `python x.py` / `node x.js`-style commands, the contents of the
  script it runs — now meets layered rules before it runs. The built-in policy denies reads of
  secret paths (`~/.ssh`, `.env*` and the files the path policy masks; committed env templates
  like `.env.example` stay readable) and denies network calls
  inside a script (a direct `curl` command stays the egress policy's business), and asks before a
  command writes outside the repository. An `ask` becomes a question card you — or the autonomy
  judge, within its risk ceiling — answer with Allow or Deny. Rules are `{"id", "decision",
  "deny"|"ask"|"allow", "reason", "command"|"script"|"touches"|"writes_outside"}`, layered install
  (the agent module's `exec_policy` setting) → org (`COLONIZER_EXEC_POLICY_ORG`) → repository
  (`.colonizer/exec-policy.json`, read once at start), and a layer can only ever narrow: the
  strictest decision wins, so nothing a later layer allows can widen the built-in denies. Every
  decision leaves one line in the harness log naming the rule that matched. Guidance in front of
  the model, not a boundary — the microVM remains that. ([#471])
- **Sensitive paths reach only trusted providers.** At boot, the file paths a task names are now
  classified for sensitivity — `open` (docs, vendored code), `standard` (ordinary app code),
  `custom` and `restricted` (secrets: `.env` files, private keys, cloud credentials, infra config)
  — from built-in defaults, extendable per repository with `.colonizer/sensitivity.toml`, and the
  strictest class is recorded on the colony. The gateway refuses a `restricted` colony any provider
  not marked `trusted` in providers.json: configured is not vetted, and cheap is not private.
  Other classes change nothing yet; a vetted tier and org-level policy are still to come. ([#472])
- **Conditional instructions: per-directory `FOOTGUNS.md` that survive compaction.** A `FOOTGUNS.md`
  (or `AGENTS.md`) in any subdirectory is now injected when the agent works on files in or under it,
  and a repo `.colonizer/instructions.toml` can map instruction files to gitignore-style path globs
  or to the task's labels. Fragments load through harness hooks once per context window, so unlike
  `CLAUDE.md` they are re-injected after a compaction while their condition still holds — and
  dropped when it no longer does. Every load is logged to the colony log, and a repo with neither
  file pays only a couple of failed stat calls per directory the agent touches. ([#473])
- **A read-only `repo-explorer` subagent.** A new first-party agent alongside Explore, always
  available regardless of `subagent_effort`: before grepping, it checks the Skill tool for a shipped
  retrieval skill (starting with graft's code map) and prefers it, falling back to find/grep like
  Explore when none applies. First slice of #474 — pinning ast-grep, ast-outline and fff as skills of
  their own, and a bench comparison of read/search token share with and without them, are follow-up
  work. ([#474])
- **Optional deja memory: colonies can recall what earlier colonies of the same org did.** After a
  colony finishes, its Claude Code transcripts are indexed — secrets scrubbed first — into a per-org
  local deja index (github.com/vshulcz/deja-vu, a local binary: no LLM, nothing leaves the machine),
  and later colonies of the same org read it back through a new `recall` tool on the colony gateway.
  The scrub replaces each known secret value raw and JSON-escaped, six characters and up, and deja's
  own pattern redaction runs on top; base64- or percent-encoded forms of a secret are not caught.
  Off by default at both levels: switch it on for the install under Settings → Memory ("Transcript
  recall (deja)"), then per org in the org's memory settings; an org can also opt an enabled install
  back out. The mothership needs the deja binary (the installer fetches it; an install without one
  just leaves recall empty, with a one-line note). `GET /api/deja` reports whether the binary is
  installed and, per org, whether recall is on, the index size and the last index time;
  `GET /api/deja/search?org=<org>&q=<query>` runs the same recall a colony would get. Both switches
  are in the cockpit: the install-level one under Settings → Memory, the org-level one in the org
  settings dialog's Memory group. ([#495])
- **A local log archive, with opt-in retention.** When a colony ends, its session directory — `events.jsonl`, the
  rotated logs, `out/pr.md`, `egress.json` — is tarred, zstd-compressed and filed under
  `<data_dir>/archive/<org>/<repo>/<yyyy>/<mm>/`, one revision per real change (`<id>.tar.zst`, then `<id>.r2`,
  …) and never overwriting the bundle before it. Deleting a colony archives its logs first, so a delete moves them
  into the archive instead of destroying them; `DELETE /api/sessions/{id}?purge_logs=true` is the only way the
  bundles go with it. `GET /api/archive` lists every revision with its index record (cost, tokens, models, PR,
  mothership), and `POST /api/archive/retention` is the preview/dry-run pair: a pure plan over the index (bundles
  older than `keep_days`, then oldest-first until `max_gb`), and an apply that refuses to remove anything unless
  `expect` repeats exactly the preview's list — and nothing at all while a bundle is the only copy, unless the
  request explicitly allows it. First slice of #496. ([#496])
- **A CLI, an MCP server, and scoped API tokens.** The `colonizer` binary is now also a client:
  `launch`, `list`, `status`, `logs`, `ask`, `answer`, `stop`, `resume` and `pr` drive a mothership
  here or across a tailnet (`--host`, `COLONIZER_TOKEN` or `--token-file`), with exit codes a
  script can read, shell completions and a man page. `token create/list/revoke` mints scoped API
  tokens — named, least-privilege keys ordered `read` < `operate` < `launch`, with org and repo
  limits, a concurrency cap and a daily dollar budget, stored as a SHA-256 hash and accepted as a
  Bearer header only; what a token launches is marked to the agent as external input, and the
  activity log records it as `token:<name>`. And `colonizer mcp` serves the harness to MCP clients
  over stdio, its tool set following the token's scope. See [docs/cli.md](docs/cli.md) and
  [docs/mcp.md](docs/mcp.md). ([#508])
- **An agent module for any ACP agent.** A new `acp` module drives an Agent Client Protocol agent
  (Zed's ACP: JSON-RPC 2.0 over stdio) as a colony agent: one long-lived agent process per colony,
  every user message a `session/prompt` turn, streamed updates mapped onto the runner protocol
  (permission requests become question cards, plan updates render as a thinking checklist, and the
  agent's file requests resolve inside the workspace). Google's Gemini CLI
  (`gemini --experimental-acp`) is the first verified agent, pinned by npm version and integrity;
  any other ACP agent runs by setting the custom-command option. Nothing stages the `gemini` binary
  into the colony image yet. ([#509])
- **Web Push notifications, and a cockpit that works on a phone.** When a colony needs an answer,
  stalls, fails or opens a pull request, the mothership can now wake the phone on the operator's
  nightstand: the browser subscribes from Settings → Notifications and the harness delivers an
  RFC 8291-encrypted Web Push message, authorized by a VAPID key generated on first use and kept
  beside the other secrets. Subscribing is the opt-in — no new module setting to forget — each
  device carries a name of the operator's choosing and can be revoked alone, and a push service
  reporting an endpoint gone (404/410) drops the subscription by itself. A push carries only what
  the desktop popup already carries — a short title, the colony's one line, and a link back into
  the cockpit for the colony it names — never question text, agent output, or repository content,
  and it passes the same anti-spam ledger as every other channel. The cockpit grows a
  narrow-screen bottom tab bar, so the same pages work on the phone the pushes arrive on.
  ([#516])
- **Cached views survive a restart and a GitHub outage.** The Packages tab, repository meta,
  lines of code and registry facts are kept on disk (`<data>/cache`, capped, least-recently-used
  first out) and served at once after a restart with "updated 5m ago · refreshing" and a Refresh
  button, instead of "scanning" for minutes. Scans are reused per commit, GitHub and registry
  requests are conditional (a 304 costs nothing), a colony's push or merge refreshes only that
  repository, and GitHub avatars load through the mothership's week-long cache (`/api/img`). ([#519])
- **History shows what happened, and what you did.** The mothership now keeps an activity log
  (`<data>/activity.jsonl`, rolled over at 2 MB with one previous file kept): every colony outcome —
  pull request opened, merged or closed, nothing to change, stopped, failed, waiting on an answer —
  recorded once at the moment it happens, and every change a person makes through the API — launching,
  stopping, resuming, deleting, answering, Create PR, loops, red-team runs, workspaces switched on or
  off, providers, modules, secrets and tokens saved or removed — with who (`you` in the cockpit, or the
  API token) and when. Names only: a secret's id is logged, never its value, and request bodies are
  never read. `GET /api/activity` pages it (`before`, `limit`) and filters it (`kind`, `actor`, `org`,
  `repo`, `q`). The History page is rebuilt on it: one timeline with an icon per kind, sticky day
  headings, counts of pull requests, merges, failures, questions and your actions, filters by kind,
  repository and actor plus search, 50 rows a page with older activity on request, runs of quiet
  events folded into one expandable row ("12 colonies finished with nothing to change, 00:15–00:16"),
  and each row linking to its colony, pull request or settings section. See
  [docs/protocol.md](docs/protocol.md) §6.9. ([#527])
- **Remote access: the relay.** A Cloudflare Worker at `my.colonizer.dev` puts every install one
  subdomain away: the mothership registers an install — an Ed25519 public key in D1, nothing else —
  and dials `/tunnel/<id>`, where a Durable Object per install holds its tunnel: an Ed25519
  challenge/hello handshake proves the dialer holds the key, one live tunnel per install (a new dial
  replaces the old and fails what was riding on it), request, response and body frames for HTTP plus
  `ws_*` passthrough frames for WebSockets, 32 streams at a time, a per-install token-bucket rate
  limit, and a 502 offline page when no tunnel is connected. The owner signs in through GitHub; on
  an install with no owner the account is parked behind a 6-digit pairing code that the local
  cockpit confirms with a signed request, so the machine gets the final say on who owns it. No
  request or response body is ever stored or logged: D1 keeps the public key, the created-at and
  the owner binding, and the access log is method, path template, status, bytes and milliseconds.
  Deployment is manual (services/relay/README.md). ([#532], [#534])
- **Remote access: drive the cockpit through a relay.** An opt-in switch (`PUT /api/remote`) that
  keeps one outbound WebSocket to `my.colonizer.dev` — nothing is ever listened on — and serves the
  cockpit's own API and websockets through it, so the mothership is reachable from outside the
  machine without opening a port or joining the mesh. The install proves itself with an Ed25519 key
  (generated on first use, kept 0600 under the config dir, never logged) that the relay binds to a
  per-install host at registration; every tunnelled request then lands on the same router as
  localhost, under the same API token, admitted only while the switch is on, only for the tunnel's
  own Host, with the Origin fence pinned to exactly `https://<host>` — the tunnel host is never a
  LAN host. Disabling closes the tunnel and every in-flight stream at once but keeps the key;
  `POST /api/remote/reset` retires a leaked identity with a fresh one, reconnecting immediately.
  `GET /api/remote` shows the switch, the host and whether the link is live. Switch changes are
  recorded in the activity log. See [docs/protocol.md](docs/protocol.md) §6.10. ([#533])
- **Remote access in the cockpit.** Settings → Remote access carries the switch — off by default, explained in plain words — and, while on, the link with Copy and a QR code, the tunnel's status, the relay's pending pairing codes to confirm, and a two-step "Reset link"; a small **Remote access ON** badge sits in the top bar, and History reads the `remote.*` rows. ([#535])
- **Waiting colonies free their slot.** A colony whose question has waited past a grace period is
  now suspended: the mothership removes its microVM but keeps the worktree and the agent's own
  session transcript, freeing the parallel slot for the queue, while the status stays
  `waiting_for_answer` and the question stays answerable exactly as before — in the cockpit
  ("Suspended — resumes when you answer"), over the events WebSocket, at
  `POST /api/sessions/{id}/answer`, and from the phone. The answer is held on the record
  (persisted before it is acknowledged, cleared only once delivered, so a failed boot or a restart
  never loses it), and the next queue tick boots a fresh microVM ahead of new launches that resumes
  the agent's own session and delivers the answer as its first message. Only agents that can resume
  a session are suspended: Claude Code declares `session_resume` in its `module.json` (a writable
  mount on `/root/.claude/projects`) and reports its session id through a new `agent_session` runner
  event; anything else keeps its microVM. Two global sandbox settings: `suspend_waiting` (default
  on) and `suspend_after_minutes` (default 10). This is transcript resume, not a VM snapshot — the
  pinned microsandbox 0.6.18 cannot checkpoint a running VM's memory (0.7.x can; measured at
  0.46 s and 304 MB for a 512 MiB VM). The activity log gains `outcome.suspended` and
  `outcome.restored`. See [docs/architecture.md](docs/architecture.md). ([#562])
- **Loops on a longer leash, and maps that stay fresh.** A new `every_days` cadence runs a loop
  every N days (1–365) at a fixed UTC time — every 14 days at 03:00, say, anchored so a run that
  fires late never drifts the schedule. A loop can also now be a map loop (`kind: "map"`): instead
  of a prompt it keeps architecture maps current, drawing its own repository each firing or
  `owner/*` mapping every repository of the org one at a time, ten minutes apart, with the list
  taken fresh each cycle so repositories added later join in. Refreshes run through the same
  admission path as the Map view — parallel limits, budgets and the archify skillset rule them in —
  and a refresh that fails keeps the old map, notes why on the loop and records `map.refresh` in
  History. The Map view asks "Keep this map up to date?" once per repository, defaulting to every
  14 days with presets 7/14/30/60/90 or a custom number ("Not now" is remembered in that browser
  for 30 days); map loops are also creatable from the Loops page. See
  [docs/loops.md](docs/loops.md). ([#564])
- **The map's shaft from the surface comes straight down.** The mothership's entrance always sat at
  the middle of the surface, so on a map whose entry chamber is off to one side its corridor ran as
  one long diagonal across the map, cutting through a boundary's title on the way. The entrance now
  sits on the surface right above the entry chamber, stepping aside only when that way down would
  pass through another chamber, a name or a title, and its shaft drops straight down and turns in.
  Boundary titles are also drawn over the tunnels, so a passing tunnel no longer hides one. ([#566])
- Remote access can now bind an owner. The first GitHub sign-in on the link shows a six-digit code, and Settings → Remote access lists it with Confirm and Reject; once paired it names the owner, with Unbind. The mothership serves `GET /api/remote/pairing`, `POST /api/remote/pairing/confirm`, `POST /api/remote/pairing/reject` and `DELETE /api/remote/owner` as signed calls to the relay. Confirming, rejecting and unbinding work only from the cockpit on this machine, never through the link, and scoped API tokens cannot reach them. Reset link also unbinds the old link's owner.
- **Orgs can tune which providers sensitive work may reach.** Providers can be marked `vetted` —
  one step below `trusted`, which implies it — and record the `vendor` that actually runs their
  model; `.colonizer/sensitivity.toml` can classify paths `vetted` (between `custom` and
  `restricted`, which still need a trusted provider); and an org's workspace settings set the
  minimum provider mark per class (`any`, `vetted` or `trusted`). An org can tighten a loose class
  or loosen `restricted`, which never drops below `vetted`, and can pin restricted work to a list
  of vendors, matched case-insensitively against the provider's recorded `vendor` — a provider with
  no vendor recorded never matches. ([#626])
- **Scoped launch tokens can keep their recurring work in loops.** A loop a token creates records
  the token (`created_by_token`), and every run is admitted against the token's org/repo limits,
  concurrency cap and daily budget and marked as external input, exactly like a colony the token
  launched by hand — a run a cap refuses is recorded in the loop's note, and revoking the token ends
  its loops the next time they would run, so nothing launches after revocation. A read token lists
  loops and their runs; a launch token edits and runs only the loops it created, and map loops stay
  with the owner. ([#627])
- **Usage data has a sender — and no default endpoint, so nothing is sent until you name one.** The
  anonymous usage batch now composes Cratefield's `module-telemetry` payload: it is validated against
  the same grammar the collector parses before it is shown, and a background sender posts it at most
  once every 24 hours to the URL named in `COLONIZER_TELEMETRY_ENDPOINT`. There is no default endpoint,
  so an install that never sets it sends nothing, ever — the switch, the first-start notice and
  `colonizer telemetry show` all work exactly as before. The batch's shape changed with it: what was a
  flat record is now the payload's five fields (`schema`, `install`, `client`, `modules`, `events`),
  where the old flat fields become counted events whose names carry the label (`colonies.parallel_now.2-3`,
  `boot.vm-boot.5-15s`, `setting.agent.model`), the usage id becomes the 32-hex `install` value, and the
  id now rotates every 30 days. `GET /api/telemetry/usage` shows the new shape; a batch that cannot be
  sent is dropped, never queued or retried. ([#628])
- **Codex, Grok Build and Hermes colonies can ask you questions.** Each module now serves an
  `ask_user` tool through its vendored MCP server (`mcp.mjs`): a call turns into a `question` event
  on the runner's loopback bridge; the cockpit's answer comes back as the tool result and the turn
  continues (an interrupt or a turn end cancels a pending ask, per §2 the question is never also a
  `tool_call`/`tool_result`). Hermes only loads MCP servers with the optional `mcp` Python extra,
  so its colony image now installs hermes-agent with `pip install -e ".[mcp]"`. ([#630])
- **Codex and Grok Build colonies get the colonizer MCP tools.** The codex and grok-build modules now
  register a `colonizer` MCP server with every turn: `finding_file`, `memory_search`, `memory_propose`
  and `wait`, served by a dependency-free stdio server wired to the runner's loopback bridge, and gated
  on the same `COLONIZER_FINDINGS` and `COLONIZER_MEMORY_DIR` switches as the other agent modules.
  ([#631])
- **Agent modules are preflighted against the colony image before a colony starts.** The harness now reads `requires.binaries` and `requires.pins` from every module's `module.json`: launch and boot refuse a module whose declared binary the harness does not stage (`claude` today) and no stock colony image carries, naming the binary and, when the module pins it, the pinned version and install command, instead of leaving a colony to die in the runner's in-VM preflight after boot. A runner that fetches its own binary (OpenCode) declares it under `requires.fetched_by_runner` and is never refused; a custom `sandbox.image` stays the operator's word and is still only checked inside the VM. ([#633])
- **ACP colonies apply the exec policy.** Commands an ACP agent asks permission for now meet the same layered exec policy as Claude Code's Bash commands: a deny is answered with the agent's reject option, an allow with its allow option, and an ask surfaces on the question card with the rule and its reason named. Modules that do not apply the policy are now refused at launch while one is set. ([#635])
- **`disabled_tools` now works on every shipped agent backend, in each CLI's own language.** Each
  module declares the harness-level switch its CLI speaks — codex `-c` config overrides, grok-build
  `--disallowed-tools`, Hermes `agent.disabled_toolsets`, opencode permissions, pi `--exclude-tools`
  (ACP names no per-tool switch, so it declares no setting) — where before only `claude-code` could
  take tools away, via its session `disallowedTools`. A module names the values it accepts as
  `x-known-tools` on the setting's schema property, and the boot check validates
  `COLONIZER_DISABLED_TOOLS` against that list, falling back to Claude Code's tool names where none
  is declared. See [docs/providers.md](docs/providers.md). ([#636])
- **The bench grades colonies' Jev compaction against each other.** `node scripts/bench.mjs jev
  [bench-<label>.json ...]` reads the Jev visibility ladder (`<data dir>/jev_ladder.jsonl`), groups its
  rows by colony and by the bench run each colony worked in, and prints precision and recall per colony,
  per run and overall, with the tp/fp/fn/tn counts beside them so a small sample is visible.
  `--threshold` moves the predicted-positive cutoff (default 0.5, the score the plugin itself keeps at)
  and `--json` prints the report object. ([#637])
- **`loop_next` and `loop_stop` on Codex, Grok Build and OpenCode, not just Claude Code.** An agent module declares the loop tools with `"loop_tools": true` in its `module.json`, and a loop's brief names them only when the module its colony launches on declares them. On a module without them — Pi, Hermes and ACP, which serve no colonizer MCP server yet — the brief says nothing about pacing or stopping, a self-paced loop simply runs again in 24 hours, and the Loops form warns when you pick self-paced for an org whose agent module lacks the tools. ([#643])
- **A `colonizer loop` subcommand.** Loops are no longer cockpit-only: `colonizer loop list`, `create`, `run`, `stop`, `start` and `delete` drive the same `/api/loops` routes from a terminal, over the usual `--host`, `--token-file` and `--json`. The cadence reads the way the composer's `/loop` does — `30m`, `2h`, `7d`, `14d@03:00`, `daily@09:00`, `weekly@mon@09:00`, `monthly@15@09:00`, `self` — with local clock times stored as UTC, and `--kind map`, `--model`, `--no-autopilot` and `--max-runs` mirroring `launch`. `stop`/`start` are the Loops page's pause switch. See [docs/loops.md](docs/loops.md) and [docs/cli.md](docs/cli.md).
- **API tokens are managed in the Settings UI.** Settings → API tokens lists every scoped token with
  its scope, org/repo limits, launch caps and last use, and creates and revokes them over the same
  owner-only routes as `colonizer token` — the CLI and the raw API are no longer the only ways. A
  new token's plaintext is shown once, with a Copy button, and never again. ([#646])
- **A held-out bench suite with gap reporting.** `scripts/bench/heldout.mjs` gives each bench task family a
  companion check kept outside the repository (a set inside it is refused). `run --heldout <dir>` resolves
  every family's companion before any colony launches, scores each pull request against it on a fresh,
  guarded clone keeping only the pass bit, retires companions after three scoring decisions, and fails the
  run naming any family whose visible-vs-held-out gap beats `--max-gap` (0.25). See [docs/bench.md](docs/bench.md).
- **Chat keeps its images.** Pasted, dropped or picked images upload to the Mothership (with progress) and are stored once, content-addressed, with location-bearing metadata stripped; only real PNG, JPEG, GIF and WebP files up to 10 MB, 8 per message. Messages show them as thumbnails that open full size, and regenerate, edit and resend, branch and compare send them to the model again — a model that cannot read images is told one was left out. Deleting a conversation removes the images nothing else uses. The Markdown export becomes a zip with the images beside it. Persona preset edits and notes on unhelpful replies now live on the Mothership instead of one browser, moved up automatically.
- **Chat personas are ants now.** The top bar's persona picker is a cast of four named, animated ants, each keeping its preset and prompt: Pip the forager (Plain), Sarge the soldier ant (Code reviewer), Silka the weaver ant (Architect) and Mellie the honeypot ant (Release writer). Cards show the ant, its caste, role and what it does, with the full system prompt a click away; the chosen ant sits in the top bar and speaks the conversation's replies (walking while one streams in). Small inline SVGs, still under reduced motion. Conversations keep their stored persona, so older ones find their ant.
- **Chat, redesigned.** A searchable conversation list (workspace filter, pinned and Today / Yesterday / Previous 7 days / Older groups, inline rename, delete with undo, ⌘\\ to collapse); an empty-state hero with suggestions from the workspace (explain a repo's architecture from its map, what the colonies did today, why a colony failed, release notes from merged PRs, review a file, plan an issue); a centred composer with a model picker (provider marks, key status, prices, fast/cheap/strong tags), a "+" menu to attach a repository file, colony, map or map component, GitHub issue, snippet or image, slash commands (`/colony`, `/file`, `/loop`, `/model`, `/system`, `/clear`) and a token and cost estimate for what is attached. Replies show model, tokens, cost and latency, with syntax-highlighted code (loaded on demand) and file paths that open in the Code page. Edit and resend in a branch, regenerate with another model, branch from any message, compare two models side by side and keep one, persona presets, temperature and max tokens, Markdown export, search within a conversation, and hand-offs to a colony, a loop or a GitHub issue. The first reply names the conversation with the cheap summary model. See `/api/chat` in [docs/protocol.md](docs/protocol.md).
- **Chat.** Talk to a model directly from the cockpit, no colony: conversations stored on the Mothership, replies streamed, stop and regenerate, a colony's summary and recent activity as optional context, and "Turn into a colony" on any reply. Any configured `<provider>/<model>`, or a Claude model with an Anthropic API key or an Anthropic provider — never the Claude subscription login. The composer's Ask mode sends a question there. The dashboard's issues button is now the orange **Send colonies** action.
- **Codex as an agent module.** `modules/agents/codex` runs OpenAI's Codex CLI headlessly (`codex
  exec --json`) on the same runner protocol as Claude Code: one process per turn, the first turn's
  thread id resumed into one continuous thread, token totals (codex reports no cost) on each
  `turn_end`, and a `CODEX_API_KEY` colony secret for `api.openai.com` — no ChatGPT sign-in. The
  module is pickable now; nothing stages the `codex` binary into the colony image yet, so a codex
  colony stops at the runner's preflight until the pinned CLI is on the image's PATH.
- **graft as a downloadable skillset.** Settings → Skillsets offers graft (a code
  map of the repository: `graft ask`, `callers`, `skeleton`, `grep`) with a Download
  button. The bundle is not shipped with the app: the mothership downloads the
  per-architecture bundle pinned by sha256 in `crates/colonizer/graft.lock` into
  `<data>/plugins/graft`, where it is an ordinary skillset to switch on. Each colony
  builds its own map of its own worktree on first use, outside the worktree and
  offline — no model key reaches graft and its telemetry stays closed. The bundle
  carries its own Node 22, because graft's native tree-sitter core does not build
  on the Node 24 colonies run. Bundles are built by `.github/workflows/graft-bundle.yml`
  on `graft-*` tags; until one is published and pinned, the row says it is not
  available yet.
- **Loops: colonies on a schedule, like `/loop`.** A saved prompt on a repository launches a colony every N minutes (15 minutes to 7 days), daily, weekly, monthly, or self-paced, where each run names the next with a `loop_next` tool (15 minutes–24 hours). Any run can end its loop with `loop_stop`, and a loop also ends after its max runs or end date. One run at a time: a tick that finds the previous run still live skips and says so. Loops has its own page (templates, history with status, PR and cost, run now), loop colonies carry a ↻ badge, and `/loop 1h <task>` in the composer makes one. Red-team schedules now share the cadence code. See [docs/loops.md](docs/loops.md).
- **Org settings: pick which installed agent module an org's colonies launch on** (falls back to
  the mothership's agent choice).
- **Prompt screening at publish time.** A new opt-in `screen` module (provider `promptdecode`)
  scans the colony's final diff and the pull request body for hidden code points — tag characters
  encoding ASCII, bidi controls reordering what a reviewer reads, variation selectors smuggling
  bytes — after the commit and before the push, the first moment anything could leave the machine.
  A tiny deterministic decoder built into the mothership: no heuristics, no network, no model.
  `warn` (the default once the module is on) logs the findings, records them on the colony's event
  log and lists them in a sanitized section at the foot of the pull request;
  `block` holds the publish — no push, no pull request — naming the count per class and the way
  out. Flag emoji, ZWJ sequences, and Arabic or Hebrew prose without direction controls pass
  clean; direction embeddings, isolates and overrides are flagged even when balanced. Off until
  configured, like notify. See [docs/prompt-screening.md](docs/prompt-screening.md).
- **Where a colony's tokens go.** The colony report files every turn's token spend under one of six
  categories — read, search, command_output, edit, reasoning, replay — from the tools the turn called,
  a subagent's calls included (edit beats command_output beats search beats read beats reasoning; cache
  reads and writes are always replay). Each `turn_end`'s cumulative `model_usage` is diffed against the
  previous snapshot and floored at zero exactly as the spend journal does, so a colony's categories add
  up to its recorded usage; a `turn_end` without `model_usage` leaves the baseline for the next measured
  turn instead of re-counting it. The report gains a "Token categories" table, `--json` carries
  `tokenCategories`/`tokenCategoriesByModel`, and `bench-<label>.json` records `token_categories` per
  task and run so a change to where tokens go becomes measurable. A first slice of #469: the Rust
  journal and the cockpit chart are still to come.

### Changed

- **Vendored plugins are refreshed.** superpowers moves from v6.4.1 to v6.4.2 (the writing-plans
  skill changed), and vendored google-skills from `3863d56` to `f566651` (adds
  `cloud/gke-workload-scaling-troubleshooting`, updates 21 skills; the staged catalog is now 147).
  Run `scripts/fetch-vendor.sh` (or install a release) to restage. ([#556])
- **The colony launch path goes through the `ExecutionBackend` trait.** Boot pulls the image and boots the microVM, teardown removes it, and the liveness checks (the minute-tick watchdog, the restart sweep, warm resume) ask what is running — all through the new `execution` field on `App`, whose only backend is still the local microsandbox one. No behavior change. ([#625])
- **Settings and launch refusals say what they wanted.** A module setting of the wrong type is
  refused naming the type the schema asks for ("setting `cpus` must be an integer"), and one outside
  its declared range names the bounds it must sit inside, one-sided or both ("setting
  `hold_timeout_minutes` must be between 1 and 1440"), instead of a bare "wrong type" / "out of
  range". The launch refusals in `POST /api/sessions` — an unknown model tier, a
  `<provider>/<model>` override no configured provider owns, an uninstalled agent module, missing
  Claude credentials — are unchanged, but now carry direct unit tests. ([#642])
- **The cockpit uses wide screens.** Every view but the Nest now sits in one shared page container: fluid to the window with gutters that grow from 24px to 56px, capped at 2560px of content for dashboards, tables and lists and at 1280px for forms (Settings, Secrets, Launch). Charts grow taller with the window, the chart and its side list scale together, KPI notes wrap instead of being cut off, and on a wide desktop the Overview puts Workspaces and Colonies side by side, the Inbox its two lists, and Memory its review queue and notes. Phones and laptops look as before.
- **Colonize: one button that turns issues and plain words into colonies.** The sidebar's
  "New colony" and the dashboard's "Send colonies" are now one orange **Colonize** button, with an
  ant in place of the plus and the GitHub mark (its antennae twitch on hover, and hold still under reduced
  motion); both open the same pane, and so does ⌘K / Ctrl+K from anywhere in the cockpit. The pane
  keeps the old hand-off list (repository scope, search, labels, now ten to a page with the shared
  pager) and adds a box above it: type or speak what you want, press Enter, and the summary model
  that already titles chats (`summary_model`) drafts one issue, or a few when the text lists
  independent tasks. Confirm or edit the title and body, pick the repository when the scope has more
  than one, and **Create** files them with the Mothership's `gh` (the same path as a chat's "file an
  issue"); the new issues land at the top of the list, pre-selected, and are dispatched at once
  unless "Dispatch right after creating" is off. Without a summary model the text itself is the one
  draft. New routes: `POST /api/colonize/draft` and `POST /api/repos/{owner}/{repo}/issues`; the
  activity log and History record `colonize.issue` (issue created from Colonize) and
  `colonize.colony` (a colony dispatched from it). What moved: `/loop 1h <task>`, voice and
  "Launch without an issue" (an open colony on the text) are in the pane's box; the full launch form
  is behind the pane's "launch form" link and the phone's More sheet (now "Launch"); the floating
  composer stays, with `#123` links and Ask, but ⌘K now opens Colonize. On the workspace dashboard
  the button sits on the title row, centred on the org name, with the range and Compare controls
  below it.
- **The README is a front door again, and every doc says what is built and what is not.** The README covers what Colonizer is, how to install it, a first colony from the cockpit and from the CLI, and a "Read more" index of every doc; its settings table moved to docs/install.md. Agent modules are labelled honestly: Claude Code, OpenCode and Pi run on the stock images, while Codex, Hermes, Grok Build and ACP are `EXPERIMENTAL` because their CLIs are not staged. docs/gaps.md is now the register of everything the docs, the cockpit or the design promise that the code does not do yet, each row with its tracking issue. loops.md, burn-down.md, red-team.md, path-policy.md, sandbox-network.md, prompt-screening.md, skill-packs.md, runner-authoring.md, bench.md and the module READMEs were corrected against the code.
- **The reference docs match the code again, and a broken doc link now fails CI.** protocol.md documents every route in `routes.snap` (credentials, device and install settings, `DELETE /api/sessions/{id}`, diff, catch-up, egress), the reclamation and diagnosis rules as the code applies them, and the `loop_next`/`loop_stop` events; install.md lists every mothership setting and what the config and data directories hold; cli.md, mcp.md and `colonizer launch --help` say what autopilot really does (open the pull request, not answer questions); architecture.md follows the #577 server composition and the `sessions/` split; remote-tunnel.md says where the code differs from the pinned contract. Two new guides: docs/colonies.md (a colony's life: claims, epics, questions, suspension, budgets, verification, the log archive) and docs/cockpit.md (every cockpit view and shortcut). `node scripts/check-doc-links.mjs` checks every relative link and anchor, in the `scripts` job.
- **Filed issues carry the Source labels, so they stay in the list.** When Settings → Source
  offers only issues with certain labels, an issue created from Colonize or from a chat's "file an
  issue" now gets all of those include labels (none when the setting is empty), and Colonize's
  confirm step shows them ("labels: ready, colonize"). A label the repository lacks is created
  first; one that cannot be created is skipped, logged and named in the toast, and the issue is
  filed anyway. Both routes now also refuse while external writes are blocked, like every other
  filed issue.
- **Long cockpit lists page ten at a time.** The workspace dashboard's Packages tables (Published, Dependencies, Supply chain) and the colony lists on the overview and the workspace dashboard now show ten rows per page with a pager ("11–20 of 54"), a search box and filters that fit the data: status, ecosystem, repository and unreleased changes on Published; repository on Dependencies; search, ecosystem and repository on Supply chain; a failed bucket in the overview's status menu; and status, repository and agent on a workspace's colonies. Changing the search or a filter goes back to page one, and the tab counts still show totals. The Code page gets a Grid | List toggle, where List is one compact row per repository, and the browser remembers the choice.
- **The relay knows its GitHub OAuth app.** `services/relay/wrangler.toml` now carries the Colonizer Remote Access OAuth app's client id (public; the secret is a Worker secret), so owner sign-in on `my.colonizer.dev` can start once the relay is deployed. ([#531])

### Fixed

- **Read-only shared memory, enforced twice.** Subagents and background tasks can search shared memory but never
  propose: the runner's hook already refused `memory_propose` for any agent but the orchestrator, and now the
  mothership re-checks each proposal's `origin` before it touches a store — with the mem0 provider, a refused
  proposal is never sent upstream. Proposals record who made them (`source.origin` beside `source.session_id`,
  shown in review), the access matrix and what is *not* implemented (automatic extraction of memories from
  conversation turns) are documented. ([#324])
- **A swappable session store.** The session index (`sessions.json`) is now written through one
  interface, `SessionStore`, with the on-disk layout as the default backend and a reference
  object-store backend beside it, so the same semantics can later be served off the local disk and
  the per-session files under `data/sessions/<id>/` can follow. Each operation states its
  consistency contract (atomic replace, at-least-once appends deduplicated by `seq` on read, one
  writer per session), ids and file names are validated so host paths like worktrees are refused,
  and `migrate` copies colonies between stores — source untouched, destination verified, empty by
  requirement — with a dry-run mode. The contract, the local assumptions the object-store backend
  surfaced, and the migration procedure are in docs/session-store.md; the reads and per-session
  file writes, and a CLI to run a migration, are follow-ups. ([#325])
- **Misconfiguration refuses with a name and a fix instead of degrading silently.** A settings save
  refuses an unknown key, naming it and the settings the module does take (a key already stored still
  passes, or a provider switch would lock you out of saving); an enum refusal lists the options; a
  corrupt `colonizer.toml` names the file, the error and the fix instead of a bare "using defaults"; a
  corrupt `claude-accounts.json` is logged instead of silently resetting your default account, and its
  writers refuse to overwrite it; a local plugin copy shadowing a vendored one is logged when the
  skillset is saved; and duplicate provider ids in a hand-edited `providers.json` are named, with the
  save over them refused. The house rule and its audit table are in
  [docs/architecture.md](docs/architecture.md). ([#326])
- **History no longer repeats or re-dates events.** The page dated each colony by its `updated_at`,
  which moves on every housekeeping write — a reclaim sweep marking worktrees cleaned up, an update or
  restart touching every colony — so one sweep re-dated days-old outcomes to "just now" and drew them as
  a burst of identical lines, and three colonies launched on the same issue read as one event repeated.
  Outcomes now come from the activity log at the time they happened; a colony that finished before the
  log existed is shown once, with its time marked approximate, and every row names its colony. ([#527])
- **A pull request description that names a file it did not create no longer holds autopilot.**
  The done-claim verifier treated every path in `pr.md` as a claimed change, so a docs-only colony
  that explained its file name by pointing at a `docs/remote-access.md` it had deliberately avoided
  came back `contradicted` — the same path listed twice — and autopilot held. A described path that
  is missing now only contradicts the claim when **none** of the description's in-repo paths is on the
  branch or in the diff (and no changed file is named in it): the work it describes is not there.
  Otherwise the missing path is an advisory, recorded in the verification's new `advisories`, shown
  once in the activity line and added to the published pull request as a verification note, without
  changing the verdict. Empty branches (unverifiable) and failing tests (contradicted) are unchanged.
  ([#531])
- **Colonies are no longer launched on epics.** An epic is a planning container: a colony on it
  duplicates the colonies on its sub-issues (one spent $1.82 and an hour on #531). A launch on an
  issue with sub-issues, an `epic` label, or a title ending "(epic)" or starting "Epic:" is now a
  **409** naming why and listing up to ten open sub-issues to launch instead; `allow_epic: true`
  starts one anyway. The check sits in `POST /api/sessions`, so the dashboard, the Colonize pane,
  the MCP tool and the CLI all get it. Issue lists mark epics ("Epic · 5 sub-issues") and leave them
  out of bulk hand-offs, noting how many were skipped. ([#531])
- **The Overview's workspace card shows whole again.** Hovering a row of "Share by workspace" beside
  the merged-PRs chart opens a card for that workspace, centred on the row; for the top rows it
  reaches above the chart's top rule, and the chart body's `overflow: hidden` (there so the KPI
  strip's hairline grid does not poke past its edges) cut the card's top, rounded border and all,
  off at that rule. The chart section now clips sideways only, so the card rises over the heading
  intact; the KPI strip and every other ruled section still clip both ways. ([#565])
- **Remote access answers again instead of hanging on every request.** The relay read the response
  headers the mothership sends — a list of `[name, value]` pairs — as an object, threw on the first
  tunnelled response, and left the browser waiting until it timed out. It now reads headers in any
  of the shapes the two sides send, and forwards repeated `set-cookie` headers as separate headers
  instead of joining them into one value, so a sign-in behind the tunnel no longer loses the second
  cookie. ([#618])
- **Remote access no longer fights another mothership for the same link.** When the relay closed the
  tunnel because a newer one took the install over (close code 4000, or the pinned 4409), the
  mothership redialed at once and took the link straight back — so two motherships sharing one
  install key kept replacing each other forever. A replaced close now parks the tunnel: nothing
  redials until you re-enable remote access or reset the link, `GET /api/remote` reports
  `"replaced": true` meanwhile, and the cockpit shows "Another mothership took over this link" in
  place of "Offline — reconnecting". Transient closes still reconnect with the usual backoff.
  ([#619])
- **The agent can no longer read the session token that opens the unfiltered terminal shell.** agentd now covers the session token file with a read-only bind of `/dev/null` as soon as it has read it (`--seal-token`, passed by the boot script): a runner child that read `/colonizer/token` could open `ws://127.0.0.1:7070/v1/pty` and get a root shell without its seccomp/capability profile. The daemon refuses to start when the seal cannot be applied. The separate network route to the same token — a raw socket sniffing agentd's plaintext traffic — is tracked as a follow-up. ([#640])
- **Unknown `/api/*` paths answer a JSON 404.** A request for an API route no module serves used to
  fall through to the cockpit's SPA fallback and come back `200 text/html`, so a mistyped or
  probed route read as a success. Unmatched `/api` paths — with or without a trailing slash — now
  answer `404` with the API's `{"error": …}` body, behind the same token wall as every other API
  route; page loads outside `/api` keep the SPA fallback, and missing `/assets` chunks keep their
  own 404. ([#641])
- **A suspended colony you have answered no longer looks stuck.** Answering a suspended colony
  saves the answer with the time it arrived (`pending_answer.answered_at`), and the colony keeps
  its `waiting_for_answer` status with `suspended` and `pending_answer` both set until a slot
  frees — that pair means "answered, waiting for a slot". Suspended colonies now come back in
  answer order rather than the order they were suspended in, still ahead of fresh launches, and
  the answer-hold log line says whether a slot is free or how many answered colonies stand ahead.
  The cockpit chip reads "Answered · resumes when a slot frees" and the colony card shows the
  place in line (for example "2nd in line"). ([#667])
- **Building the app works again with the ACP agent module.** `modules/agents/acp` shipped without a `package-lock.json`, so `scripts/install.sh` (which runs `npm ci` for every agent module) failed, and so did the CI bundle jobs. The module now ships its lockfile like the others. ([#591])
- **Path-policy placeholders never count as the colony's work.** The empty files a boot makes as bind targets for the path policy (`.envrc`, `.git-credentials`, `.gitmodules`, `.mcp.json`, `.netrc`, `.pypirc` and the rest) sat untracked in the worktree, so the verification snapshot listed them as changed files. The snapshot and the publish commit now take them back out of the index after `git add -A` (recorded placeholders, or any empty policy path HEAD does not carry when the list was lost), and hold every masked path at HEAD's version, so a real masked file is never committed emptied or changed and an untracked one under a mask is never added. Stopping or tearing down a colony removes its still-empty placeholders from the kept worktree (a resume makes them again), and no cleanup ever deletes a file HEAD carries.
- **The relay can deploy.** `services/relay/wrangler.toml` declared `[[durable_objects]]` as an array where wrangler requires a table, so `wrangler deploy` refused the file. The config is fixed, it points at the real `colonizer-relay` D1 database, and CI now dry-runs the relay deploy so a malformed config fails on the pull request. ([#531])
- **Verification runs the repository's own package manager.** The fresh-checkout test run picked `npm install && npm test` for every package.json repository, so a bun, pnpm or yarn repository failed its install and the claim came back contradicted, holding autopilot (a colony on a bun repository sat held for twenty hours). The command now follows the base branch's `packageManager` field (through corepack), else its lockfile (`bun.lock`/`bun.lockb` → `bun install --frozen-lockfile && bun run test`, `pnpm-lock.yaml`, `yarn.lock` for Yarn 1 or 2+, `package-lock.json`), else npm, and `command_source` names what decided it. The VM checks for that tool before it runs anything: one the colony image does not carry (the default node image has no bun or pnpm) makes the claim unverifiable with the tool named, never contradicted.

### Security

- **Publishing, filing and resuming now require an authority grant, checked against the bytes the operator approved.**

  The governance machinery from #98 is wired into the paths that act. Creating a pull
  request (a press or an autopilot confirmation) mints a grant bound to the tree the publish would
  commit plus the pr.md body, and the commit, push and pull request each re-check it as they run —
  a worktree that moved between the approval and the commit is refused, not published. The body
  that finally goes up is that approved pr.md with harness-composed footers added on top
  (verification notes, a staleness note, screen warnings); those exact leaving bytes are hashed
  into the audit trail. A finding is filed only under a FileIssue grant over the exact issue body,
  checked before any `gh` call, and every resume boot checks a fresh Resume grant; a colony found
  mid-publish after a restart is fenced because no grant survives the restart. Grants live in
  memory for fifteen minutes and are never persisted, so anything still unspent after that — or
  after a harness restart — needs approving again. The kill-switch
  (`COLONIZER_NO_EXTERNAL_EFFECTS`) still refuses everything it did. ([#624])
- **Grok Build colonies now switch off project-scope hooks, plugins, MCP servers and skills instead of hoping.**

  The grok-build runner forces grok's folder-trust gate on with `GROK_FOLDER_TRUST=1` in every
  child environment (env beats a `[folder_trust] enabled` kill-switch in any config, and an
  inherited host value can no longer pass through), alongside the fresh `GROK_HOME` it already
  assigned. A headless run against an empty trust store then resolves the workspace untrusted, and
  grok skips a colonized repo's project `.grok/` MCP servers, plugins, hooks and skills, plus
  project LSP servers and instructions (AGENTS.md — brief the colony through the prompt instead).
  The runner never passes `--trust`, and a new `GROK_WORKSPACE_UNTRUSTABLE` preflight refuses to
  start at all when the workspace is the home directory or the filesystem root, the two paths
  folder trust auto-trusts because it could never record them. The gate exists in release-stamped
  binaries only, which the pinned install.sh build is. A live contract test
  (`COLONIZER_GROK_LIVE_BIN=/path/to/grok npm test`) drives the real binary and asserts it reports
  the workspace untrusted with none of a planted `.grok/` loadable. Note the boundary: this fences
  project scope only; the user-scope `~/.claude`/`~/.cursor` compat scanners are a separate issue.
  ([#634])
- **A colony's gateway token now opens only the inference requests its own model settings name, and only while the colony is there to receive them.**

  The provider gateway used to authorize a request once — the bearer token had
  to belong to a live colony — and then forward whatever method, path and body
  it carried. The token's reach is now the routing boot derived it from, recorded
  on the session before the token file is written: a colony reaches only the
  providers and the `<provider>/<model>` pairs its model settings name, a colony
  with no recorded set reaches nothing, and only `POST /v1/messages` (plus
  `/v1/messages/count_tokens` on the anthropic wire) is served — any other path or
  method is refused before the upstream is touched, with the query string limited
  to path-safe characters. Two checks cover the time a request spends waiting for
  a provider slot: a colony has at most 16 requests queued at once (agents fan out
  through parallel subagents, so bursts are routine; past the cap a further request
  is refused `429 overloaded_error` on arrival), and once a request holds its slot
  it re-checks that its token still belongs to that live colony and that the budget
  still admits it, refusing without sending anything upstream if not. Colonies
  started after the upgrade pick the set up from the settings boot already read;
  one whose machine survived the mothership upgrade keeps no record, so it needs
  a resume before its gateway calls are admitted again. ([#681])
- **The cockpit editor now ships a patched DOMPurify.** `monaco-editor` in the web console moves from 0.56.0 to 0.57.0. Monaco's ESM build inlines its own copy of DOMPurify into the app bundle, so pinning the npm `dompurify` package alone — through the lockfile or an `overrides` entry — would quiet scanners without changing what ships. 0.57.0 inlines DOMPurify 3.4.15 in place of 3.4.8, clearing GHSA-cmwh-pvxp-8882 (permanent `ALLOWED_ATTR` pollution via `setConfig()`), GHSA-c2j3-45gr-mqc4 (`CUSTOM_ELEMENT_HANDLING` bypasses `afterSanitizeElements` for allowed custom elements), GHSA-vxr8-fq34-vvx9 (a Trusted Types policy surviving `clearConfig()`) and GHSA-55q2-fjhq-7xh7 (IN_PLACE hook removal leaving a detached subtree executable). `npm audit` on `web/` reports no vulnerabilities.

### Take care

- **Colony agents can no longer ptrace, unshare or mount**, and `/proc/sys` and `/sys` are
  read-only in the guest: the runner child's seccomp denylist answers those with `EPERM` and the
  boot script remounts the kernel filesystems before the agent runs. A workload that relied on one
  of them now fails with an ordinary tool error instead of succeeding. Check a workload against
  the profile with `colonizer-agentd --seccomp-profile` and
  `scripts/seccomp-evidence.sh -- <workload>`. ([#301])
- **Claude Code colonies now mount a writable host directory at `/root/.claude/projects`**, where
  the runner keeps its session transcripts, so a suspension can stop the microVM without losing the
  conversation. The directory lives under the colony's session dir on the host: it counts against
  the per-colony host-disk quota and is deleted with the colony. ([#562])
- **Colonies waiting on an answer when you upgrade get suspended** once the default 10-minute grace
  has passed on the new build — the question stays answerable and answering brings the colony back
  — unless `suspend_waiting` is switched off first. ([#562])

## [v0.1.9] - 2026-09-24

### Added

- **Pi as a second agent module.** `modules/agents/pi` adds the Pi coding agent as an agent provider (`pi`) beside Claude Code, driven over Pi's RPC mode and speaking the same runner protocol. Pi reaches models only through the provider gateway (Settings → Providers, under each colony's spend and rate limits); it has no subagents, so the subagent and background model split does not apply. ([#403])
- **Cockpit v3.** The cockpit is rebuilt around a slim sidebar whose workspace switcher is the
  cockpit's scope (two-line rows with live/queued/colony counts, an opaque menu, workspace
  settings inside the cockpit), a composer for new colonies (⌘K, typed or spoken, `#123` issue
  links), a live nest, a Host page (CPU, memory, disk and microVM slots — now also read on macOS
  through `sysctl`/`vm_stat`) and settings with sections across the top, a picture per page and
  technical fields under Advanced. The Inbox moved into a notifications bell at the top right, and
  toasts stack there too. Model fields are a Provider and a Model menu instead of free text. The
  Overview measures lead time, PR cycle time and CI pass rate from recorded PR facts, shows the
  merged-PRs legend as workspace logos with a hover card each, and each workspace row has a red-team
  button with a three-step wizard (colony swarm today; Strix and Shannon shown as coming soon),
  weekly or monthly schedules and a history. A GitHub issues button opens a pane to hand issues off
  to colonies, monorepos (npm/pnpm/yarn/bun workspaces, turbo, nx, Cargo, go.work) expand into
  their packages on the workspace dashboard, and a colony's chat history now opens on its latest
  messages instead of filling in frame by frame. The Source module takes include/exclude label
  filters for which issues are offered. ([#457])
- **Secrets in the system keychain, and a Secrets page.** Saved keys (provider keys, Claude
  accounts, voice and integration keys) go to the macOS Keychain or the Linux Secret Service when
  it answers a startup probe, else to the 0600 files as before; the Secrets page lists every key
  write-only with where it lives, what colonies get of it (via the gateway, injected for one host,
  or not at all), and moves file keys into the keychain on request. Colony secrets are new:
  variables you name, with allowed hosts and a scope (all colonies, a workspace or a repository),
  substituted by microsandbox only on TLS to those hosts, so a colony only ever sees a
  placeholder. A subscription token's account is now identified by the organization it bills. ([#468])
- **The map shows what colonies read, the files, and their diffs.** On top of the Map mode below:
  ants patrol the chambers whose files their colony is reading as well as changing, with a bubble
  saying what they do; a right-hand explorer lists the repository's files at the map's revision
  (`GET /api/maps/{owner}/{repo}/files`, from the local clone) with the component's files marked,
  and clicking a file shows each live colony's recent steps there and its diff
  (`GET /api/maps/{owner}/{repo}/file`). Raw JSON, when and by which colony the map was drawn, and a
  mapping colony that runs as one agent at medium effort on Sonnet. ([#462])
- **A repository picker with GitHub's facts.** The map's repository menu is grouped by
  organization and shows each repository's description, languages as GitHub colours them, a
  commit sparkline and contributors; the chosen one gets a card with the language bar and 52 weeks
  of commits (`GET /api/repos/{owner}/{repo}/meta`, cached). ([#488])
- **One-line task summaries.** Each colony gets a one-sentence summary of its task, written by a
  cheap model you already pay for (the agent module's `summary_model`, else a routed
  `<provider>/<model>` such as `zai/glm-5.3-flash`, else an Anthropic API key — never the
  subscription token), shown on colony cards, the inbox and the nest. The Overview colonies table
  filters from its header row (search, org with logos, status, updated, spent). Switch off with the
  agent module's `summaries` setting. ([#490])
- **An OpenCode agent module.** `modules/agents/opencode` runs OpenCode on configured providers —
  including a local DeepSeek — through the gateway, with its binaries pinned by checksum.
  Opt-in. ([#202])
- **Colonies are claimed on GitHub, so two motherships never take one issue.** ([#454])
- **Boot medians per phase.** Warm-start caches boot-time provider probes and the cockpit shows
  median boot time per phase. ([#219])
- **Synthetic bench tasks, stage one.** `scripts/bench/synth.mjs` injects token-level bugs into Node
  source and admits, through a green-reference / parses / breaks-the-same-tests-twice gate, only mutants
  that break the repository's own tests deterministically. Each carries full provenance ($0: no model in
  the loop) into a held-out pool kept outside the repo, scored oldest-first once 20 hand-reviewed tasks
  open it and retired after three decisions; flaky mutants go to a raid set that is never scored.
  Measured so far: 57 of 110 candidates admitted (bench fixture + telemetry). See
  [docs/bench.md](docs/bench.md). ([#332])

- **A first slice of Grok Build as an agent module.** `modules/agents/grok-build` drives xAI's
  `grok` CLI headless on the runner protocol: one grok process per turn, resumed into a single
  session, with the streaming events mapped to agent events and unknown types logged rather than
  fatal. The binary is pinned (1.0.34, SOURCE_REV in `module.json`) with a loud preflight
  (`GROK_CREDENTIAL_MISSING`, `GROK_BINARY_MISSING`, `GROK_VERSION_DRIFT`), the colony never runs
  browser OAuth, and a fresh `GROK_HOME` plus `--sandbox off`, `--always-approve` and
  `--disable-web-search` carry the nesting decisions. Experimental and PLANNED: the mothership-side
  key push, gateway routing and question routing are follow-ups (see the module's README). ([#333])
- **A Hermes agent module, first slice.** `modules/agents/hermes` drives Nous Research's Hermes Agent
  CLI (verified against `v2026.9.24`) headlessly on the colonizer-runner/1 protocol: one
  `hermes chat -q --format stream-json` process per turn, resumed by session id, events mapped to the
  runner contract, non-JSON stdout tolerated as logs. The terminal backend is pinned to local (any
  other `TERMINAL_ENV` is refused at startup, because Hermes would fall back to local silently),
  Hermes' memory, skills, delegation, cronjob, tts and clarify toolsets are off, models go only
  through the provider gateway as `<provider>/<model>` with Nous Portal refused, and a per-turn
  timeout plus SIGTERM cover what Hermes' own signals don't. Covered by tests against a CLI stub,
  including a conformance check against `docs/agent-events.schema.json`, and verified live against
  real Hermes v0.21.5 through a fake Anthropic-wire gateway; a colony picking it stops at the
  runner's preflight, because nothing stages the `hermes` binary into the VM — the preflight fails
  loudly naming the pinned install. ([#334])
- **The nest as a map of the software.** The nest has a Map mode: a repository's
  architecture — drawn by a mapping colony with the newly vendored
  [archify](https://github.com/tt-a1i/archify) skill (MIT) and stored by the
  mothership — becomes the nest, with components as chambers, boundaries as mounds
  and connections as tunnels, and each live colony's ants walking to the chambers
  whose files it is changing. "Map this repo" launches the mapping colony, which
  leaves the repository untouched and ends in `no_changes`; `GET /api/touched`
  reports every live colony's changed files. Run `scripts/fetch-vendor.sh` (or
  install a release) to stage the `archify` skillset. ([#462])
- **A real colony runs in CI.** The `colony-e2e` job replaces the never-run `colony-smoke` (it needed a self-hosted KVM runner): on every pull request, `scripts/colony-e2e.mjs` launches a real mothership, boots a real microVM with the vendored msb, and runs the real agentd and claude-code runner against a scratch git repository and a stub Anthropic-wire model server — no secrets, no paid model, no GitHub writes — asserting the colony comes back `no_changes` and uploading its logs on failure. ([#368])

- **Configurable free-disk thresholds.** The sandbox module's `warn_free_disk`
  (default 10G) warns in the cockpit when the data dir's volume runs low, and
  `min_free_disk` (default 5G) pauses queue admission until space returns —
  running colonies keep running and the pause itself deletes nothing, though below
  the floor the reclaim sweep still reclaims finished colonies whose work is already
  pushed. The floor also honours
  `COLONIZER_RECLAIM_MIN_FREE`, and both ride in `/api/status`'s `storage` and
  `GET /api/storage` alongside the microsandbox home size. ([#220])
- **Connectable voice services.** A new `voice` module picks what the composer's
  microphone uses: the browser's own recognition (the default), or OpenAI, Groq,
  Deepgram, ElevenLabs or any OpenAI-compatible server (a local whisper, LiteLLM).
  With a service connected the cockpit records a clip and the mothership
  transcribes it with a key it keeps (`voice-keys/`, 0600; an OpenAI or Groq model
  provider's key is reused), so the key never reaches the browser. Settings →
  Modules → Voice has the key field and a three-second microphone test. Audio goes
  browser → mothership → service and is not stored. ([#457])
- **Realtime Cockpit dashboards.** The dashboards now update over a single authenticated `/api/stream` WebSocket (sessions including running cost/tokens, orgs, fleet hosts, storage), with a Live indicator, tweened counters, reduced-motion support, and a fallback to the existing poll schedule with reconnect/backoff when the stream drops. ([#446])
- **Colony PRs rebase themselves when GitHub marks them DIRTY or BEHIND.** A live colony is asked to rebase onto fresh main and re-run its own gates itself, in its own microVM, and push; a colony that's gone has its branch rebased on the host instead (git only, no gates — GitHub's own CI covers that push), and if that host rebase conflicts, it's flagged needs-rebase and notified instead — once per main SHA, with SHA-aware backoff. A newly launched colony can opt in to queueing behind a live colony already touching the same repository (off by default; pass `serialize` to ask for it), starting from fresh main once that colony publishes, merges or finishes; the cockpit shows `queued behind <colony>` and a needs-rebase indicator. ([#453])
- **Skill-pack validation at every gate.** The vendored-plugin updater validates a pin's new archive with the skill-pack validator and skips the pin instead of pinning a pack that breaks a rule — the errors land in the proposal when another pin is adopted, otherwise the run fails with them in its log; CI and the updater's workflow stage the pinned packs and run the validator over them; and colony boot now enforces the `mcp.json` rules too (every server needs a stdio command or a remote url, and a remote one its declared hosts), with the declared hosts readable for the future egress gate. ([#370])
- **Security-hunter modules, phase one (Strix).** A `Manifest` per hunter, a `hunters.lock` pin
  per platform, and an on-demand install (`POST /api/hunters/strix/install`) that downloads the
  pinned tarball and unpacks one verified binary — behind the `COLONIZER_HUNTER_INSTALL=1`
  opt-in, Linux only. Strix `vulnerabilities.json` + SARIF parsers exist but no scan runs yet;
  Shannon ships as a manifest-only stub. See
  [docs/security-hunters.md](docs/security-hunters.md). ([#440])
- **A script makes CI a required check on `main`.** `scripts/require-ci-checks.mjs` prints — and with `--apply`, sends through `gh api` — a repository ruleset requiring the six CI jobs that run on every pull request, each pinned to the GitHub Actions app so only a real run's report satisfies it. `colony-smoke`, the supply-chain jobs and the release jobs stay optional, and docs/audit.md says why, along with the two settings that travel with the ruleset: "Allow auto-merge" on (a colony pull request held for checks is queued with `gh pr merge --squash --auto`), and no merge queue (no workflow has a `merge_group` trigger). Applying it still takes a repository admin, which is why it is a script rather than a change this repository can commit. ([#367])

### Changed

- **The archify skillset is on by default** for a fresh install, so the Map works without finding
  the switch; installs that saved a skillset list keep theirs. ([#462])
- **Vendored google-skills** moves from `6e3838f` to `3863d56`. ([#335])
- **A red-team run can name its models** (`model_override`, `subagent_model_override` on
  `POST /api/sessions`), used by the wizard's model step. ([#457])

### Fixed

- **A mapping colony stops itself once its map is drawn**, instead of sitting idle on a parallel
  slot; one that goes idle without a valid map is stopped with a clear error after 15 minutes. ([#489])
- **A recovered provider error no longer leaves a colony on "needs you".** A provider answering
  again lifts the gateway's `model_error` flag, and a still-running colony is never listed for
  one — this also ends the needs-you entries that flickered on and off. ([#462])
- **Colony commits keep the executable bit**, and a child colony is restacked when its parent
  merges first. ([#455])
- **A tab left open across an update reloads itself** instead of failing on a module-script MIME
  error: missing `/assets/*` answer 404, not the page. ([#457])
- A mothership restart no longer stops every live colony when `msb ls` fails outright. After a host reboot the microsandbox daemon can come up after the harness, and recovery read that failure as "nothing is running" and removed every live colony's microVM; it now waits — retrying with backoff — until `msb ls` answers before it decides what to tear down, so colonies that kept running across the restart are reconnected once the daemon appears. ([#407])
- **A malformed `COLONIZER_GATEWAY_BIND` refuses startup instead of silently falling back to 41750.** The bind is parsed once as an IP:port socket address and the listener, the colony model routes and the per-colony network fence all use that port, so a typo'd bind (or a hostname like `localhost:41750`, which is no longer resolved) can no longer hand colonies an allow rule for a port nothing listens on. The fence is extracted into `colony_network` and unit-tested: a colony always boots with the `public` profile alone, and its only host allows are the mesh control port and the gateway port. ([#406])
- **A damaged `orgs.json`, `providers.json` or `modules.json` is no longer silently replaced by defaults.** A settings save over a file that will not parse is refused with an error naming it, the cockpit's storage banner says defaults are in effect until it is fixed or removed, `modules.json` is moved aside to a `.corrupt-*` file at startup like `sessions.json`, and concurrent saves are serialised so two at once cannot lose each other's org or provider. ([#408])
- **Concurrent writes to `claude-accounts.json` and `known-orgs.json` no longer lose each other's entries.** Account creates and deletes, the org-seen record and the five-minute GitHub refresh's save all run inside the same config-write section the settings files use, with the refresh re-reading the record just before it saves so a colony started mid-refresh keeps its org marked seen, and the first-use migration publishes create-if-absent so it cannot write over an account created while it ran. Every save names its temp file per call, so two writers cannot consume each other's temp. ([#486])

### Security

- **Each colony spends only on the providers its model settings route to**, and a request's
  budget is reserved before it is sent, so parallel requests cannot overshoot a colony's budget. A
  colony saved before this change keeps its access until its next boot. ([#409])
- **The VM-written PR description is read through one no-follow handle**, closing a
  check-then-read symlink race on publish. ([#410])
- **A `claude_account` id from a request is validated before it becomes a file path.** ([#482])
- **Hunter installs are fenced off the host Docker daemon and hardened.** The capability probe
  no longer touches the host's Docker daemon and never suggests pointing at it: Docker-dependent
  hunters stay not-ready until Docker runs inside the colony microVM. Installs are Linux-only,
  serialised per hunter, verified by checksum over the downloaded bytes before anything is
  unpacked, capped at 256 MiB, follow https redirects only, and land atomically with mode
  `0o555`. ([#442])

### Take care

- **Saved keys may move to the system keychain.** New keys go to the macOS Keychain or Linux Secret
  Service when it works; existing file keys stay where they are until you move them on the Secrets
  page. On macOS each rebuilt, unsigned binary is a new app to the Keychain and asks again for every
  key — set `COLONIZER_CODESIGN_IDENTITY` when building (`scripts/install.sh`) to sign with one
  stable identity. ([#468])
- **`COLONIZER_GATEWAY_BIND` must be an IP:port.** A hostname such as `localhost:41750`, or any
  malformed value, now refuses startup instead of falling back to 41750. ([#406])
- **Installing a security hunter needs `COLONIZER_HUNTER_INSTALL=1`** and is Linux-only. ([#442])
- **Colonies live across the upgrade** keep their provider access until their next boot, when the
  per-colony allowlist applies. ([#409])
- **A model setting that names an unconfigured provider refuses the boot.** A `<provider>/<model>`
  value whose prefix matches no configured provider used to start fine and quietly send every request
  to Anthropic — the runner's warning only landed in the colony's log, so a typo'd route spent the
  subscription unnoticed. The mothership now checks after tier substitution, so only the models a
  colony will actually run are looked at, and fails the boot with `model setting
  'locall/deepseek-flash' names provider 'locall', which is not configured`. Fix the setting or add
  the provider; bare names (`opus`, `claude-opus-5-5`) are unaffected. ([#366])

## [v0.1.8] - 2026-09-23

### Added

- **Cockpit dashboards.** The Overview is redesigned: a 7, 30 or 90-day range
  with a compare-to-previous-period toggle, a KPI strip (launched, returned,
  merged, spend, needs you) with deltas and sparklines, spend per day by
  workspace, and a table comparing workspaces. Each org card opens a per-org
  dashboard: outcomes per day, spend per day by model, a launch → PR → merged
  funnel, the model mix, and a per-repository table with the cost per merged
  pull request. Every figure comes from data the mothership already serves;
  the ones the design shows without a source yet (CI pass rate, lead time,
  MTTR, latency) are left out rather than invented. A workspace setting hides
  orgs with no colonies. ([#398])
- **An account usage limit pauses the nest instead of failing colonies.** A
  Claude session or usage limit ("You've hit your session limit · resets …")
  is recorded once for the account: colonies park until the reset, the cockpit
  shows the pause and when it ends, and one action resumes them all. ([#404])
- **A stuck colony can be diagnosed from the API:** a per-session diagnosis,
  the tail of its recent events, and a host-level stall signal. ([#230])
- **A parallel limit per repository.** Beside the global and per-org limits, the
  sandbox module's `repo_max_parallel` caps the colonies live at once in any one
  repository, and an org can set its own. All three apply and the tightest wins;
  a colony held back by its repository queues without holding up colonies of
  other repositories. ([#174])
- **No-write kill-switch.** Set `COLONIZER_NO_EXTERNAL_EFFECTS` (or
  `COLONIZER_NO_WRITE`) to anything but `0`, `false`, `off` or `no` and each
  of these fails closed: Create PR answers 409,
  autopilot logs a warning instead of publishing, the commit, push and
  pull-request steps each refuse, stacked pull requests keep their old base,
  fix-PR reviews neither merge nor comment, and findings are ignored. A draft
  pull request counts as a write too. Unset, nothing changes. ([#84])
- A publish refuses to open or reuse a pull request if the branch head moved
  after the push, and logs the SHA-256 of the PR body it sends. ([#84])
- **Jev compaction.** The agent module's `jev_compaction` switch prunes stale tool
  calls by Jev score at compaction instead of the lossy summary, tuned by
  `jev_keep_threshold`/`jev_preserve_recent`. Needs the staged plugin and a
  `JEV_API_KEY`; history goes to TypeSafe, billed directly, invisible to colony cost. ([#226])
- **Jev visibility ladder, first slice: measurement.** Each applied Jev compaction
  pass now reports its per-chunk keep/drop decisions with the plugin's own
  relevance scores to the harness, which logs them to a data-dir-wide
  `jev_ladder.jsonl` and watches for the agent re-issuing an equivalent tool call
  later in the session, logging each match once as a `reread`. Shadow measurement
  only: nothing changes what compaction keeps or drops, and fallback passes that
  were computed but not applied are not measured. Precision and recall against
  the reread ground truth are computed and tested; the bench-wide report is
  follow-up work. ([#475])
- **Switch a running colony's model.** A `set_model` command changes the model
  for the colony's next turns in the same session, conversation and microVM,
  until it is stopped. ([#240])

### Fixed

- Colony pull-request automerge and the publish watcher handle a pull request
  that is behind main or has conflicts, instead of attempting a merge that
  fails. ([#338])
- The desktop cockpit shows **Mothership unreachable** when the API stops
  answering, and a toast when a Stop or Resume fails, instead of keeping stale
  data without a word. ([#417])
- Release builds carry their version, so `colonizer update` no longer takes a
  Linux release for a development build and refuses to update it. ([#365])
- The cockpit's org list works as a switcher. The rail and the header menu both
  have an **All workspaces** choice, and clicking the selected org again clears
  the filter. The header menu opens settings for the selected org and lists
  switched-off orgs, so you can turn them back on. A new **memory** rail item
  shows how many proposals are waiting for review. Switching org keeps you on
  history, launch or memory instead of jumping to the nest. A saved org that is
  gone or switched off no longer filters the nest to nothing. The org list
  scrolls on its own, and the header menu works from the keyboard. ([#411])
- A mothership restart no longer forgets which providers are out of quota: the
  record and its reset time are kept in `provider-quota.json` beside
  `provider-usage.json`, so colonies parked on an exhausted quota stay parked
  until the reset instead of all resuming at once and parking again. ([#358])

### Security

- **Colonies are fenced off the cockpit API and the host's other loopback
  ports.** A colony could reach the mothership's management API through the
  host network profile; each colony now gets port-scoped allows for only the
  ports it needs. ([#375])
- **The cockpit API requires a per-install token.** It is created on first
  start in `api-token` in the config directory, readable by you only. The
  browser signs in once through the link the mothership prints at start (or
  `colonizer open`), which sets an HttpOnly cookie; scripts send
  `Authorization: Bearer <token>`. Anything else gets 401, except a reduced
  `GET /api/status` for fleet peers. ([#405])

### Take care

- **Sign in again after updating.** Open the link the mothership prints when it
  starts, or run `colonizer open`. Scripts that call the API need the token
  from `api-token`. A mothership bound to anything but loopback warns that
  plain HTTP exposes the token: put TLS or an SSH tunnel in front of it.
- `repo_max_parallel` defaults to 3. An install that raised `max_parallel` and
  ran more than 3 colonies on one repository now queues the rest; raise the
  new setting (up to 32) to keep the old behaviour.

## [v0.1.7] - 2026-09-22

### Added

- Overview: per-org tokens, models and cost, plus a spend history that survives
  cleanup.
- Red-team raids and burn-down mode are documented: [docs/red-team.md](docs/red-team.md)
  and [docs/burn-down.md](docs/burn-down.md).
- Mothership credentials at rest are encrypted with opt-in envelope encryption,
  no new dependencies. ([#286])
- Resume keeps the event stream: a run-epoch reset signal retires old sockets so
  reconnecting lands on live events instead of a torn tail. ([#276])

## [v0.1.6] - 2026-09-21

### Added

- **One colony per issue.** Starting a colony on an issue another colony is
  already queued on, working on, publishing or has an open pull request for is
  refused, naming the colony that holds it. A colony that stopped, failed, found
  nothing to change, or whose pull request is merged or closed leaves the issue
  free, so a retry still works; `allow_duplicate` starts a second one on purpose.
  One FindsYou issue drew four colonies, two of them ten seconds apart, and three
  complete implementations of the same feature were thrown away. ([#172])
- **A colony is told who else is in the repository.** The prompt lists the other
  colonies working the same repository from the same base, and asks for the
  smallest version of any shared scaffolding rather than the complete one. Four
  colonies once wrote four different versions of the same new crate because none
  of them knew the others existed. ([#172])
- Red-team mode: a hunter swarm with distinct briefs that only raids an empty
  nest, reporting through the findings tool and never opening or merging.
  ([#212])
- Burn-down mode: a weekly token plan spent to a reserve by bug-hunt colonies
  paced across the window, off until configured. ([#210])
- A colony's model tier is picked per task, with a pure rule you can read, test
  and override — orchestrator, subagent and background models separately.
  ([#178])
- The bench: the fixed tasks a change to an agent has to survive, scored the
  same way every time. ([#146])
- Jev as an optional shadow-mode second opinion for model-tier routing:
  recorded for comparison, never applied.
- A model provider's failure rate and average latency are shown on the
  providers screen and in `GET /api/status`, as a new `model_providers` array,
  instead of living only in `~/.local/share/colonizer/provider-usage.json`. A
  provider that has 50 or more requests and failed 10% or more of them reads as
  degraded, so it can be unhealthy even while its one-request health check
  passes. `GET /api/providers` gained a `health` object (`failure_pct`,
  `avg_latency_ms`, `rated`, `degraded`) and `usage.since`, the instant the
  tally started, so the percentage is labelled with the span it covers.
  ([#184])
- The notify module gained a `provider_degraded` event and an `on_provider`
  setting: crossing the degraded threshold announces once, and re-arms only
  after the failure rate falls back below 8%. ([#184])
- The docs say what an unset `max_concurrent` means: unlimited fan-out.
  ([#184])
- Optional stacked colonies: start a colony `after` another and branch from its
  branch. ([#197])
- Overview shows the host and its live stats: CPU, memory, microVMs, disk.
  ([#205])
- First-run Setup checklist: one pane that walks a new mothership to its first
  colony. ([#150])
- Orgs: choose which are workspaces, be asked about new ones, and see their
  avatars. ([#180])
- Pending questions render in the cockpit's right pane, answerable in place.
  ([#215])
- A colony can wait instead of polling a log, through a wait primitive.
  ([#195])

### Changed

- A colony's stack is detected from its repository by default. A new `auto`
  preset, now the sandbox default, reads the repository's marker files when the
  colony's worktree is checked out (`Cargo.toml`, `go.mod`, `pyproject.toml`,
  `package.json`, …) and falls back to Node when none match; a preset picked by
  hand still wins, and an org's workspace settings can pin its own stack over
  the global one. ([#177])
- The colony chat follows new content at the pace it is written, sitting
  level with the bottom instead of a line or two behind. ([#120])

### Fixed

- A new field in a colony's record can no longer make an existing
  `sessions.json` unparseable: every field now defaults when missing, so a
  sessions.json written by an older version loads with what it has. ([#127])
- One damaged record no longer throws away every colony in the list. Startup
  keeps the good records, copies the original file aside as
  `sessions.json.corrupt-<timestamp>` (the original stays put) and the alert
  says how many loaded, how many were damaged and where the copy is. ([#127])

## [v0.1.5] - 2026-09-18

### Added

- Colonizer now knows which version it is. A build records the tag it came from,
  its commit and when it was built, and Settings shows them. ([#110])
- It checks whether a newer release is out, every few hours, **on by default**,
  with a switch in Settings and `COLONIZER_UPDATE_CHECK=0` to keep it off from
  the environment. Switched off it makes no request at all. ([#110])
- **Update in place.** Settings offers the newer release, installs it with the
  same verified installer you would run by hand, and restarts into it. Colonies
  keep their microVMs and reconnect, and the pane says what happened to each
  one. ([#112])
- `colonizer version` says what a binary is, and `colonizer update` applies a
  newer release from a terminal against a running mothership, the same two
  routes the Settings button uses. `colonizer --help` lists both. ([#131])
- Shared memory can live in [mem0] instead of files on disk. ([#97])
- `scripts/colony-report.mjs` prints how colonies went, out of what they already
  log. ([#99])
- Settings names the Claude account that is connected, or says plainly that it
  cannot be named. ([#83])
- Prebuilt releases, installable with one command. ([#74])
- Motherships on the live map at colonizer.dev, off until switched on.
  ([#79])

### Changed

- Following a live colony moves at a steady speed instead of a burst per
  line: how far behind the view is sets a pace in pixels per second, averaged
  over 250 ms and aimed to sit level 300 ms ahead, so a stream of lines is one
  movement rather than a series of starts. ([#134])
- The colony list is ordered by what it wants from you, shows the queue, and
  keeps following a pull request after it opens. ([#82])
- An install now lives behind a symlink, so an install interrupted half-way
  leaves either the whole old app or the whole new one, never neither. ([#104])
- A colony that cannot be resumed says why, instead of quoting `gh` at you.
  ([#111])

### Fixed

- A finished colony stops taking answers it can never deliver. ([#114])

### Take care

- An update applied from Settings keeps the previous app directory until no
  running colony still mounts plugins from it, and sweeps it at the next start.
  An installer run by hand still replaces it immediately, which is safe when
  nothing is running and not when something is. ([#112])

## [v0.1.4] - 2026-09-17

### Changed

- The colony chat streams smoothly: even text, fade-ins and eased scrolling,
  instead of arriving in jerks. ([#80])

## [v0.1.3] - 2026-09-17

### Added

- A live map at [colonizer.dev/live](https://colonizer.dev/live): a dot for a
  mothership's area, lit while colonies run. **Off until you switch it on.**
  ([#79])

### Changed

- "Settler" now means what it does today, subagents included, and the README
  stops claiming nothing is downloaded at runtime: the Claude Agent SDK and,
  on a Mac, the Linux build of Claude Code, are fetched at install time from
  Anthropic's own channels. ([#78])

## [v0.1.2] - 2026-09-17

### Changed

- The published crates carry a banner and a README of their own. ([#77])

## [v0.1.1] - 2026-09-17

### Added

- `colonizer-harness` and `colonizer-agentd` are published to crates.io on
  release, as source to build from rather than as a way to install. ([#76])
- The one-command install is documented now that there is a release to install.
  ([#75])

## [v0.1.0] - 2026-09-17

The first release: prebuilt for Linux x86_64 with KVM, and for Apple Silicon
Macs. ([#74])

### Colonies

- A GitHub issue becomes a pull request. The agent works in a disposable KVM
  microVM on a fresh git worktree, and the **host**, never the VM, commits,
  pushes and opens the pull request, automatically when the agent finishes.
- Questions arrive as multiple-choice cards with an "Other…" answer, never a
  wall of text, and every step is described in plain language with the exact
  command one click away.
- A colony whose microVM stops keeps its worktree and can be resumed where it
  left off.
- Colonies past the parallel limit queue instead of being refused, and several
  issues can be launched in one go.
- A chat and a terminal per colony, with each subagent shown as its own speaker.

### Credentials and the mesh

- Credentials never enter a colony. Tokens stay on the mothership; the guest
  sees a placeholder and microsandbox's TLS proxy substitutes the real value at
  the network edge.
- A private mesh of bundled Headscale and Tailscale links each colony to the
  mothership, separate from any tailnet you already use, and starts without
  internet access.

### Models

- A provider gateway on the mothership holds the keys, queues providers that
  take one request at a time, and falls back to Claude when one is down.
  Orchestrator, subagent and background models are set separately.
- A searchable catalogue of Anthropic-compatible endpoints, with presets
  including Z.AI and Alibaba Qwen, and OpenAI reachable by translating Anthropic
  Messages in the gateway.
- Two switches that save tokens: terse replies and compact command output.

### The machine

- Sandbox presets (Node, Python, Rust, Go) instead of typing an image tag, and
  the colony image downloads when the stack is chosen rather than during your
  first colony.
- Where a launch spends its time is recorded per phase.
- Claude Code plugin directories mount read-only into colonies, with ECC's
  skills and agents vendored (its hooks removed, not merely switched off), plus
  Google's skills on demand and Headroom.
- An optional pre-flight scan of the workspace, inside the colony, advisory
  rather than a boundary.

### Beyond one colony

- Org workspaces, shared memory that agents propose and you approve, and a
  watchdog that nudges colonies which stop making progress.

### Take care

- Linux x86_64 with KVM, or an Apple Silicon Mac. On a Mac the private mesh has
  no build yet and colonies fall back to a loopback port.
- Colony images must be glibc-based: the Claude Code binary a colony runs is
  mounted into it.

[mem0]: https://mem0.ai
[#74]: https://github.com/Colonizer-dev/harness/pull/74
[#75]: https://github.com/Colonizer-dev/harness/pull/75
[#76]: https://github.com/Colonizer-dev/harness/pull/76
[#77]: https://github.com/Colonizer-dev/harness/pull/77
[#78]: https://github.com/Colonizer-dev/harness/pull/78
[#79]: https://github.com/Colonizer-dev/harness/pull/79
[#80]: https://github.com/Colonizer-dev/harness/pull/80
[#82]: https://github.com/Colonizer-dev/harness/pull/82
[#83]: https://github.com/Colonizer-dev/harness/pull/83
[#84]: https://github.com/Colonizer-dev/harness/issues/84
[#97]: https://github.com/Colonizer-dev/harness/pull/97
[#99]: https://github.com/Colonizer-dev/harness/pull/99
[#104]: https://github.com/Colonizer-dev/harness/pull/104
[#110]: https://github.com/Colonizer-dev/harness/pull/110
[#111]: https://github.com/Colonizer-dev/harness/pull/111
[#112]: https://github.com/Colonizer-dev/harness/pull/112
[#114]: https://github.com/Colonizer-dev/harness/pull/114
[#120]: https://github.com/Colonizer-dev/harness/issues/120
[#127]: https://github.com/Colonizer-dev/harness/issues/127
[#131]: https://github.com/Colonizer-dev/harness/pull/131
[#134]: https://github.com/Colonizer-dev/harness/pull/134
[#146]: https://github.com/Colonizer-dev/harness/pull/146
[#150]: https://github.com/Colonizer-dev/harness/pull/150
[#172]: https://github.com/Colonizer-dev/harness/pull/172
[#174]: https://github.com/Colonizer-dev/harness/issues/174
[#177]: https://github.com/Colonizer-dev/harness/issues/177
[#178]: https://github.com/Colonizer-dev/harness/pull/178
[#180]: https://github.com/Colonizer-dev/harness/pull/180
[#184]: https://github.com/Colonizer-dev/harness/issues/184
[#195]: https://github.com/Colonizer-dev/harness/pull/195
[#197]: https://github.com/Colonizer-dev/harness/pull/197
[#199]: https://github.com/Colonizer-dev/harness/issues/199
[#200]: https://github.com/Colonizer-dev/harness/issues/200
[#202]: https://github.com/Colonizer-dev/harness/issues/202
[#205]: https://github.com/Colonizer-dev/harness/issues/205
[#210]: https://github.com/Colonizer-dev/harness/issues/210
[#212]: https://github.com/Colonizer-dev/harness/issues/212
[#213]: https://github.com/Colonizer-dev/harness/issues/213
[#215]: https://github.com/Colonizer-dev/harness/issues/215
[#216]: https://github.com/Colonizer-dev/harness/issues/216
[#219]: https://github.com/Colonizer-dev/harness/issues/219
[#220]: https://github.com/Colonizer-dev/harness/issues/220
[#226]: https://github.com/Colonizer-dev/harness/issues/226
[#230]: https://github.com/Colonizer-dev/harness/issues/230
[#240]: https://github.com/Colonizer-dev/harness/issues/240
[#276]: https://github.com/Colonizer-dev/harness/pull/276
[#286]: https://github.com/Colonizer-dev/harness/pull/286
[#296]: https://github.com/Colonizer-dev/harness/issues/296
[#297]: https://github.com/Colonizer-dev/harness/issues/297
[#298]: https://github.com/Colonizer-dev/harness/issues/298
[#299]: https://github.com/Colonizer-dev/harness/issues/299
[#300]: https://github.com/Colonizer-dev/harness/issues/300
[#301]: https://github.com/Colonizer-dev/harness/issues/301
[#302]: https://github.com/Colonizer-dev/harness/issues/302
[#303]: https://github.com/Colonizer-dev/harness/issues/303
[#304]: https://github.com/Colonizer-dev/harness/issues/304
[#309]: https://github.com/Colonizer-dev/harness/issues/309
[#310]: https://github.com/Colonizer-dev/harness/issues/310
[#311]: https://github.com/Colonizer-dev/harness/issues/311
[#321]: https://github.com/Colonizer-dev/harness/issues/321
[#324]: https://github.com/Colonizer-dev/harness/issues/324
[#325]: https://github.com/Colonizer-dev/harness/issues/325
[#326]: https://github.com/Colonizer-dev/harness/issues/326
[#328]: https://github.com/Colonizer-dev/harness/issues/328
[#329]: https://github.com/Colonizer-dev/harness/issues/329
[#331]: https://github.com/Colonizer-dev/harness/issues/331
[#332]: https://github.com/Colonizer-dev/harness/issues/332
[#333]: https://github.com/Colonizer-dev/harness/issues/333
[#334]: https://github.com/Colonizer-dev/harness/issues/334
[#335]: https://github.com/Colonizer-dev/harness/issues/335
[#338]: https://github.com/Colonizer-dev/harness/issues/338
[#358]: https://github.com/Colonizer-dev/harness/issues/358
[#365]: https://github.com/Colonizer-dev/harness/issues/365
[#366]: https://github.com/Colonizer-dev/harness/issues/366
[#367]: https://github.com/Colonizer-dev/harness/issues/367
[#368]: https://github.com/Colonizer-dev/harness/issues/368
[#370]: https://github.com/Colonizer-dev/harness/issues/370
[#375]: https://github.com/Colonizer-dev/harness/issues/375
[#398]: https://github.com/Colonizer-dev/harness/issues/398
[#403]: https://github.com/Colonizer-dev/harness/issues/403
[#404]: https://github.com/Colonizer-dev/harness/issues/404
[#405]: https://github.com/Colonizer-dev/harness/issues/405
[#406]: https://github.com/Colonizer-dev/harness/issues/406
[#407]: https://github.com/Colonizer-dev/harness/issues/407
[#408]: https://github.com/Colonizer-dev/harness/issues/408
[#409]: https://github.com/Colonizer-dev/harness/issues/409
[#410]: https://github.com/Colonizer-dev/harness/issues/410
[#411]: https://github.com/Colonizer-dev/harness/issues/411
[#417]: https://github.com/Colonizer-dev/harness/pull/417
[#440]: https://github.com/Colonizer-dev/harness/pull/440
[#442]: https://github.com/Colonizer-dev/harness/issues/442
[#446]: https://github.com/Colonizer-dev/harness/issues/446
[#453]: https://github.com/Colonizer-dev/harness/issues/453
[#454]: https://github.com/Colonizer-dev/harness/issues/454
[#455]: https://github.com/Colonizer-dev/harness/issues/455
[#457]: https://github.com/Colonizer-dev/harness/pull/457
[#462]: https://github.com/Colonizer-dev/harness/pull/462
[#468]: https://github.com/Colonizer-dev/harness/pull/468
[#471]: https://github.com/Colonizer-dev/harness/issues/471
[#472]: https://github.com/Colonizer-dev/harness/issues/472
[#473]: https://github.com/Colonizer-dev/harness/issues/473
[#474]: https://github.com/Colonizer-dev/harness/issues/474
[#475]: https://github.com/Colonizer-dev/harness/issues/475
[#482]: https://github.com/Colonizer-dev/harness/pull/482
[#486]: https://github.com/Colonizer-dev/harness/pull/486
[#488]: https://github.com/Colonizer-dev/harness/pull/488
[#489]: https://github.com/Colonizer-dev/harness/pull/489
[#490]: https://github.com/Colonizer-dev/harness/pull/490
[#495]: https://github.com/Colonizer-dev/harness/issues/495
[#496]: https://github.com/Colonizer-dev/harness/issues/496
[#508]: https://github.com/Colonizer-dev/harness/issues/508
[#509]: https://github.com/Colonizer-dev/harness/issues/509
[#516]: https://github.com/Colonizer-dev/harness/issues/516
[#519]: https://github.com/Colonizer-dev/harness/pull/519
[#527]: https://github.com/Colonizer-dev/harness/pull/527
[#531]: https://github.com/Colonizer-dev/harness/issues/531
[#532]: https://github.com/Colonizer-dev/harness/issues/532
[#533]: https://github.com/Colonizer-dev/harness/issues/533
[#534]: https://github.com/Colonizer-dev/harness/issues/534
[#535]: https://github.com/Colonizer-dev/harness/issues/535
[#556]: https://github.com/Colonizer-dev/harness/issues/556
[#562]: https://github.com/Colonizer-dev/harness/issues/562
[#564]: https://github.com/Colonizer-dev/harness/issues/564
[#565]: https://github.com/Colonizer-dev/harness/pull/565
[#566]: https://github.com/Colonizer-dev/harness/pull/566
[#582]: https://github.com/Colonizer-dev/harness/issues/582
[#583]: https://github.com/Colonizer-dev/harness/issues/583
[#584]: https://github.com/Colonizer-dev/harness/issues/584
[#585]: https://github.com/Colonizer-dev/harness/issues/585
[#586]: https://github.com/Colonizer-dev/harness/issues/586
[#589]: https://github.com/Colonizer-dev/harness/issues/589
[#591]: https://github.com/Colonizer-dev/harness/issues/591
[#598]: https://github.com/Colonizer-dev/harness/issues/598
[#600]: https://github.com/Colonizer-dev/harness/issues/600
[#601]: https://github.com/Colonizer-dev/harness/issues/601
[#602]: https://github.com/Colonizer-dev/harness/issues/602
[#603]: https://github.com/Colonizer-dev/harness/issues/603
[#604]: https://github.com/Colonizer-dev/harness/issues/604
[#605]: https://github.com/Colonizer-dev/harness/issues/605
[#606]: https://github.com/Colonizer-dev/harness/issues/606
[#607]: https://github.com/Colonizer-dev/harness/issues/607
[#608]: https://github.com/Colonizer-dev/harness/issues/608
[#609]: https://github.com/Colonizer-dev/harness/issues/609
[#610]: https://github.com/Colonizer-dev/harness/issues/610
[#611]: https://github.com/Colonizer-dev/harness/issues/611
[#612]: https://github.com/Colonizer-dev/harness/issues/612
[#613]: https://github.com/Colonizer-dev/harness/issues/613
[#618]: https://github.com/Colonizer-dev/harness/issues/618
[#619]: https://github.com/Colonizer-dev/harness/issues/619
[#620]: https://github.com/Colonizer-dev/harness/issues/620
[#621]: https://github.com/Colonizer-dev/harness/issues/621
[#622]: https://github.com/Colonizer-dev/harness/issues/622
[#624]: https://github.com/Colonizer-dev/harness/issues/624
[#625]: https://github.com/Colonizer-dev/harness/issues/625
[#626]: https://github.com/Colonizer-dev/harness/issues/626
[#627]: https://github.com/Colonizer-dev/harness/issues/627
[#628]: https://github.com/Colonizer-dev/harness/issues/628
[#629]: https://github.com/Colonizer-dev/harness/issues/629
[#630]: https://github.com/Colonizer-dev/harness/issues/630
[#631]: https://github.com/Colonizer-dev/harness/issues/631
[#632]: https://github.com/Colonizer-dev/harness/issues/632
[#633]: https://github.com/Colonizer-dev/harness/issues/633
[#634]: https://github.com/Colonizer-dev/harness/issues/634
[#635]: https://github.com/Colonizer-dev/harness/issues/635
[#636]: https://github.com/Colonizer-dev/harness/issues/636
[#637]: https://github.com/Colonizer-dev/harness/issues/637
[#639]: https://github.com/Colonizer-dev/harness/issues/639
[#640]: https://github.com/Colonizer-dev/harness/issues/640
[#641]: https://github.com/Colonizer-dev/harness/issues/641
[#642]: https://github.com/Colonizer-dev/harness/issues/642
[#643]: https://github.com/Colonizer-dev/harness/issues/643
[#645]: https://github.com/Colonizer-dev/harness/issues/645
[#646]: https://github.com/Colonizer-dev/harness/issues/646
[#647]: https://github.com/Colonizer-dev/harness/issues/647
[#648]: https://github.com/Colonizer-dev/harness/issues/648
[#649]: https://github.com/Colonizer-dev/harness/issues/649
[#650]: https://github.com/Colonizer-dev/harness/issues/650
[#651]: https://github.com/Colonizer-dev/harness/issues/651
[#652]: https://github.com/Colonizer-dev/harness/issues/652
[#653]: https://github.com/Colonizer-dev/harness/issues/653
[#654]: https://github.com/Colonizer-dev/harness/issues/654
[#655]: https://github.com/Colonizer-dev/harness/issues/655
[#656]: https://github.com/Colonizer-dev/harness/issues/656
[#667]: https://github.com/Colonizer-dev/harness/issues/667
[#671]: https://github.com/Colonizer-dev/harness/issues/671
[#672]: https://github.com/Colonizer-dev/harness/issues/672
[#673]: https://github.com/Colonizer-dev/harness/issues/673
[#681]: https://github.com/Colonizer-dev/harness/issues/681
[#682]: https://github.com/Colonizer-dev/harness/issues/682
[#683]: https://github.com/Colonizer-dev/harness/issues/683
[#684]: https://github.com/Colonizer-dev/harness/issues/684
[#685]: https://github.com/Colonizer-dev/harness/issues/685
[#686]: https://github.com/Colonizer-dev/harness/issues/686
[#687]: https://github.com/Colonizer-dev/harness/issues/687
[#688]: https://github.com/Colonizer-dev/harness/issues/688
[#689]: https://github.com/Colonizer-dev/harness/issues/689
[#690]: https://github.com/Colonizer-dev/harness/issues/690
[#700]: https://github.com/Colonizer-dev/harness/issues/700
[#701]: https://github.com/Colonizer-dev/harness/issues/701
[#702]: https://github.com/Colonizer-dev/harness/issues/702
[#704]: https://github.com/Colonizer-dev/harness/issues/704
[#707]: https://github.com/Colonizer-dev/harness/issues/707
[#728]: https://github.com/Colonizer-dev/harness/issues/728
[#736]: https://github.com/Colonizer-dev/harness/issues/736
[#742]: https://github.com/Colonizer-dev/harness/issues/742
[#743]: https://github.com/Colonizer-dev/harness/issues/743
[#744]: https://github.com/Colonizer-dev/harness/issues/744
[#745]: https://github.com/Colonizer-dev/harness/issues/745
[#746]: https://github.com/Colonizer-dev/harness/issues/746
[#751]: https://github.com/Colonizer-dev/harness/issues/751
[#754]: https://github.com/Colonizer-dev/harness/issues/754
[#759]: https://github.com/Colonizer-dev/harness/issues/759
[#761]: https://github.com/Colonizer-dev/harness/issues/761
[#762]: https://github.com/Colonizer-dev/harness/issues/762
[#763]: https://github.com/Colonizer-dev/harness/issues/763
[#764]: https://github.com/Colonizer-dev/harness/issues/764
[#765]: https://github.com/Colonizer-dev/harness/issues/765
[#766]: https://github.com/Colonizer-dev/harness/issues/766
[#767]: https://github.com/Colonizer-dev/harness/issues/767
[#774]: https://github.com/Colonizer-dev/harness/issues/774
[#778]: https://github.com/Colonizer-dev/harness/issues/778
[#818]: https://github.com/Colonizer-dev/harness/issues/818
[#840]: https://github.com/Colonizer-dev/harness/issues/840
[#867]: https://github.com/Colonizer-dev/harness/issues/867
[#875]: https://github.com/Colonizer-dev/harness/issues/875
[#876]: https://github.com/Colonizer-dev/harness/issues/876
[#877]: https://github.com/Colonizer-dev/harness/issues/877
[#878]: https://github.com/Colonizer-dev/harness/issues/878
[#880]: https://github.com/Colonizer-dev/harness/issues/880
[#881]: https://github.com/Colonizer-dev/harness/issues/881
[#915]: https://github.com/Colonizer-dev/harness/issues/915
[#927]: https://github.com/Colonizer-dev/harness/issues/927
[#932]: https://github.com/Colonizer-dev/harness/issues/932
[#933]: https://github.com/Colonizer-dev/harness/issues/933
[#934]: https://github.com/Colonizer-dev/harness/issues/934
[#935]: https://github.com/Colonizer-dev/harness/issues/935
[#939]: https://github.com/Colonizer-dev/harness/issues/939
[#940]: https://github.com/Colonizer-dev/harness/issues/940
[#967]: https://github.com/Colonizer-dev/harness/issues/967
[v0.2.3]: https://github.com/Colonizer-dev/harness/releases/tag/v0.2.3
[v0.2.2]: https://github.com/Colonizer-dev/harness/releases/tag/v0.2.2
[v0.2.1]: https://github.com/Colonizer-dev/harness/releases/tag/v0.2.1
[v0.2.0]: https://github.com/Colonizer-dev/harness/releases/tag/v0.2.0
[v0.1.11]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.11
[v0.1.10]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.10
[v0.1.9]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.9
[v0.1.8]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.8
[v0.1.7]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.7
[v0.1.6]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.6
[v0.1.5]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.5
[v0.1.4]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.4
[v0.1.3]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.3
[v0.1.2]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.2
[v0.1.1]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.1
[v0.1.0]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.0
