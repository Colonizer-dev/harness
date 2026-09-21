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

### Changed

- A colony's stack is detected from its repository by default. A new `auto`
  preset, now the sandbox default, reads the repository's marker files when the
  colony's worktree is checked out (`Cargo.toml`, `go.mod`, `pyproject.toml`,
  `package.json`, …) and falls back to Node when none match; a preset picked by
  hand still wins, and an org's workspace settings can pin its own stack over
  the global one. ([#177])
- The colony list is ordered by what it wants from you, shows the queue, and
  keeps following a pull request after it opens. ([#82])
- An install now lives behind a symlink, so an install interrupted half-way
  leaves either the whole old app or the whole new one, never neither. ([#104])
- A colony that cannot be resumed says why, instead of quoting `gh` at you.
  ([#111])
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

### Take care

- An update applied from Settings keeps the previous app directory until no
  running colony still mounts plugins from it, and sweeps it at the next start.
  An installer run by hand still replaces it immediately, which is safe when
  nothing is running and not when something is. ([#112])

## [v0.1.7] - 2026-09-22

### Added

- Overview: per-org tokens, models and cost, plus a spend history that survives
  cleanup.
- Red-team raids and burn-down mode are documented: [docs/red-team.md](docs/red-team.md)
  and [docs/burn-down.md](docs/burn-down.md).

## [v0.1.5] - 2026-09-18

### Changed

- Following a live colony moves at a steady speed instead of a burst per
  line: how far behind the view is sets a pace in pixels per second, averaged
  over 250 ms and aimed to sit level 300 ms ahead, so a stream of lines is one
  movement rather than a series of starts. ([#134])

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
[#97]: https://github.com/Colonizer-dev/harness/pull/97
[#99]: https://github.com/Colonizer-dev/harness/pull/99
[#104]: https://github.com/Colonizer-dev/harness/pull/104
[#110]: https://github.com/Colonizer-dev/harness/pull/110
[#111]: https://github.com/Colonizer-dev/harness/pull/111
[#112]: https://github.com/Colonizer-dev/harness/pull/112
[#120]: https://github.com/Colonizer-dev/harness/issues/120
[#127]: https://github.com/Colonizer-dev/harness/issues/127
[#131]: https://github.com/Colonizer-dev/harness/pull/131
[#134]: https://github.com/Colonizer-dev/harness/pull/134
[#172]: https://github.com/Colonizer-dev/harness/pull/172
[#177]: https://github.com/Colonizer-dev/harness/issues/177
[#184]: https://github.com/Colonizer-dev/harness/issues/184
[v0.1.5]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.5
[v0.1.4]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.4
[v0.1.3]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.3
[v0.1.2]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.2
[v0.1.1]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.1
[v0.1.0]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.0
