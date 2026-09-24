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

- **Loops: colonies on a schedule, like `/loop`.** A saved prompt on a repository launches a colony every N minutes (15 minutes to 7 days), daily, weekly, monthly, or self-paced, where each run names the next with a `loop_next` tool (15 minutes–24 hours). Any run can end its loop with `loop_stop`, and a loop also ends after its max runs or end date. One run at a time: a tick that finds the previous run still live skips and says so. Loops has its own page (templates, history with status, PR and cost, run now), loop colonies carry a ↻ badge, and `/loop 1h <task>` in the composer makes one. Red-team schedules now share the cadence code. See [docs/loops.md](docs/loops.md).
- **Chat.** Talk to a model directly from the cockpit, no colony: conversations stored on the Mothership, replies streamed, stop and regenerate, a colony's summary and recent activity as optional context, and "Turn into a colony" on any reply. Any configured `<provider>/<model>`, or a Claude model with an Anthropic API key or an Anthropic provider — never the Claude subscription login. The composer's Ask mode sends a question there. The dashboard's issues button is now the orange **Send colonies** action.
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
[#403]: https://github.com/Colonizer-dev/harness/issues/403
[#404]: https://github.com/Colonizer-dev/harness/issues/404
[#405]: https://github.com/Colonizer-dev/harness/issues/405
[#406]: https://github.com/Colonizer-dev/harness/issues/406
[#417]: https://github.com/Colonizer-dev/harness/pull/417
[#446]: https://github.com/Colonizer-dev/harness/issues/446
[#453]: https://github.com/Colonizer-dev/harness/issues/453
[#368]: https://github.com/Colonizer-dev/harness/issues/368
[#407]: https://github.com/Colonizer-dev/harness/issues/407
[#408]: https://github.com/Colonizer-dev/harness/issues/408
[#370]: https://github.com/Colonizer-dev/harness/issues/370
[#440]: https://github.com/Colonizer-dev/harness/pull/440
[#442]: https://github.com/Colonizer-dev/harness/issues/442
[#367]: https://github.com/Colonizer-dev/harness/issues/367
[#366]: https://github.com/Colonizer-dev/harness/issues/366
[#334]: https://github.com/Colonizer-dev/harness/issues/334
[#202]: https://github.com/Colonizer-dev/harness/issues/202
[#219]: https://github.com/Colonizer-dev/harness/issues/219
[#332]: https://github.com/Colonizer-dev/harness/issues/332
[#333]: https://github.com/Colonizer-dev/harness/issues/333
[#335]: https://github.com/Colonizer-dev/harness/issues/335
[#409]: https://github.com/Colonizer-dev/harness/issues/409
[#410]: https://github.com/Colonizer-dev/harness/issues/410
[#454]: https://github.com/Colonizer-dev/harness/issues/454
[#455]: https://github.com/Colonizer-dev/harness/issues/455
[#457]: https://github.com/Colonizer-dev/harness/pull/457
[#462]: https://github.com/Colonizer-dev/harness/pull/462
[#468]: https://github.com/Colonizer-dev/harness/pull/468
[#482]: https://github.com/Colonizer-dev/harness/pull/482
[#486]: https://github.com/Colonizer-dev/harness/pull/486
[#488]: https://github.com/Colonizer-dev/harness/pull/488
[#489]: https://github.com/Colonizer-dev/harness/pull/489
[#490]: https://github.com/Colonizer-dev/harness/pull/490
[v0.1.5]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.5
[v0.1.6]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.6
[v0.1.7]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.7
[v0.1.8]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.8
[v0.1.9]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.9
[v0.1.4]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.4
[v0.1.3]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.3
[v0.1.2]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.2
[v0.1.1]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.1
[v0.1.0]: https://github.com/Colonizer-dev/harness/releases/tag/v0.1.0
