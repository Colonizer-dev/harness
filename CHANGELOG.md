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

### Added

- **Configurable free-disk thresholds.** The sandbox module's `warn_free_disk`
  (default 10G) warns in the cockpit when the data dir's volume runs low, and
  `min_free_disk` (default 5G) pauses queue admission until space returns —
  running colonies keep running and the pause itself deletes nothing, though below
  the floor the reclaim sweep still reclaims finished colonies whose work is already
  pushed. The floor also honours
  `COLONIZER_RECLAIM_MIN_FREE`, and both ride in `/api/status`'s `storage` and
  `GET /api/storage` alongside the microsandbox home size. ([#220])

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
- **Switch a running colony's model.** A `set_model` command changes the model
  for the colony's next turns in the same session, conversation and microVM,
  until it is stopped. ([#240])
- **Security-hunter modules (Strix).** On-demand, checksum-verified install of
  the pinned Strix binary, Strix `vulnerabilities.json` + SARIF findings
  parsers, a capability probe (runtime + Docker), and
  [docs/security-hunters.md](docs/security-hunters.md). Shannon ships as a
  manifest-only stub for now.

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
[#205]: https://github.com/Colonizer-dev/harness/issues/205
[#210]: https://github.com/Colonizer-dev/harness/issues/210
[#212]: https://github.com/Colonizer-dev/harness/issues/212
[#215]: https://github.com/Colonizer-dev/harness/issues/215
[#226]: https://github.com/Colonizer-dev/harness/issues/226
[#240]: https://github.com/Colonizer-dev/harness/issues/240
[#220]: https://github.com/Colonizer-dev/harness/issues/220
[#276]: https://github.com/Colonizer-dev/harness/pull/276
[#286]: https://github.com/Colonizer-dev/harness/pull/286
[#358]: https://github.com/Colonizer-dev/harness/issues/358
[#411]: https://github.com/Colonizer-dev/harness/issues/411
[#230]: https://github.com/Colonizer-dev/harness/issues/230
[#338]: https://github.com/Colonizer-dev/harness/issues/338
[#365]: https://github.com/Colonizer-dev/harness/issues/365
[#375]: https://github.com/Colonizer-dev/harness/issues/375
[#398]: https://github.com/Colonizer-dev/harness/issues/398
[#404]: https://github.com/Colonizer-dev/harness/issues/404
[#405]: https://github.com/Colonizer-dev/harness/issues/405
[#417]: https://github.com/Colonizer-dev/harness/pull/417
[v0.1.5]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.5
[v0.1.6]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.6
[v0.1.7]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.7
[v0.1.8]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.8
[v0.1.4]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.4
[v0.1.3]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.3
[v0.1.2]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.2
[v0.1.1]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.1
[v0.1.0]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.0
