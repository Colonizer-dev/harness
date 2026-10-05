# 6.3 Mothership API additions

Part of the [Colonizer protocol](../protocol.md).

**Skillsets** (plugin directories a colony can load; see "Plugin directories"):

| Method & path | Purpose |
| --- | --- |
| `GET /api/plugins` | `{local_root, plugins: [{name, description, version, source: "vendored"\|"local", shadows_vendored, skills, agents, commands}]}`, one entry per name resolved the way a colony's boot resolves it. `local_root` is where an operator adds their own; the counts are what Claude Code discovers: `skills/<name>/SKILL.md`, `agents/*.md`, `commands/*.md` |
| `GET /api/plugins/graft` | The downloadable graft skillset (docs/skill-packs.md, "Downloadable skillsets"): `{name, release, installed_release, state: "idle"\|"downloading"\|"unpacking"\|"installed"\|"failed"\|"unavailable"\|"local", bytes, total, started_at, finished_at, error}`. `GET /api/plugins` carries the same objects under `downloadable` |
| `POST /api/plugins/graft/download` | Start downloading the bundle `graft.lock` pins for this architecture into `<data>/plugins/graft`, or join the running download; returns at once. `409` when nothing is pinned, or when `plugins/graft` is the operator's own directory |

**Model providers** (credentials stay on the mothership, keys stored 0600):

| Method & path | Purpose |
| --- | --- |
| `GET /api/providers` | `[{id, name, base_url, auth, wire: "anthropic"\|"openai", has_key, models: [string], preset}]` plus the provider fields and live figures of §6.5 (`timeout_secs`, `max_concurrent`, `queue_timeout_secs`, `context_tokens`, `fallback_model`, `trusted`, `pricing`, `quota`, `model_map`, `disabled_tools`, `normalize_cache_ttl`, `in_flight`, `queued`, `usage`, `health`, `used_by`, `quota_exhausted`). `preset` is the catalogue id it was added from (`deepseek`, `openai`, `zai`, `alibaba`, `local`, …) or `custom` |
| `PUT /api/providers/{id}` | `{name, base_url, auth, wire?, models, api_key?, preset?, model_map?, disabled_tools?}` plus the optional provider fields of §6.5 (`timeout_secs`, `pricing`, `quota`, …): `wire` omitted is `anthropic`; `api_key` omitted keeps the saved key, `""` removes it — and a save that moves `base_url` to another origin is refused with the saved key kept, so it must bring the key again or remove it; `model_map`/`disabled_tools` omitted keep the saved values, an empty one clears (docs/providers.md) |
| `DELETE /api/providers/{id}` | Remove a provider |
| `GET /api/providers/{id}/health` | Probes the provider (§6.5, Health) |
| `POST /api/providers/{id}/test` | Sends a one-token request through the colony's route; answers `{ok, url, status, model, latency_ms, error}` (§6.5, Test request) |
| `POST /api/providers/{id}/quota-action` | `{action: "switch"\|"wait"\|"stop", model?, scope?, colonies?, org?, remember?}`: answers the provider's out-of-quota card (§6.5, Provider out of quota cards) |
| `GET /api/attention` | `{quota_cards: [card]}`: the provider-out-of-quota cards (§6.5) |
| `GET /api/models` | `[{id, label, provider}]` for model pickers: Anthropic aliases plus `<provider>/<model>` for every provider model |

Presets: `deepseek` = `https://api.deepseek.com/anthropic`, `x-api-key`, models `deepseek-flash`,
`deepseek-v4-pro`. `openai` = `https://api.openai.com`, `bearer`, wire `openai`, models `gpt-5.6`, `gpt-5.5`,
`context_tokens` 272000. `local` = `http://127.0.0.1:8080`, `none`, no models, `timeout_secs` 900, `max_concurrent` 1. The Settings
catalogue also offers `zai` and `alibaba`. Colonies reach every provider through the mothership's
gateway (§6.5), which calls the `base_url` as saved, so a loopback base URL works unchanged.

The agent module schema gains `subagent_model` and `background_model` next to `model` (all free-text
strings; UIs offer `GET /api/models` as suggestions).

**Org workspaces.** `Session` gains `"org": "<repo owner>"`.

| Method & path | Purpose |
| --- | --- |
| `GET /api/orgs` | `[{org, colonies: {live, total}, pending_memory, settings, spend, avatar_url?, description?, awaiting_decision?}]` for every org that has a reason to be a workspace — saved settings, colonies, repository owners — plus the orgs still awaiting an answer, which appear only so a UI can ask about them. `spend` sums the org's sessions (§6.8). `description` is GitHub's, when it has one. Switched-off orgs are still listed, so a UI can offer them back |
| `PUT /api/orgs/{org}` | `{settings}`; merged into the saved settings instead of replacing them: a field the body names always wins (`null` = inherit the global module setting), one it omits keeps its saved value. A save is also the answer to a pending "do you want this org?" prompt for that org |

The PUT is a merge, not a replace. A field of `settings` the body does not name keeps its saved value; a
field it names always wins, `null` included: an explicit `null` is how a client inherits the global
module setting. The merge reaches one level deeper for nested fields: an `agent` object without
`skillsets` or `module` keeps the saved skillset overrides and module pick, an `egress` object keeps
whichever of `mode`, `allow` and `block` it leaves out, a `sensitivity` object keeps whichever class
it leaves out, and a `watchdog` object without `waiting_minutes` keeps its saved value (the web form
never sends `waiting_minutes`, and a save
from a client that predates a field must not quietly clear it). So a body naming only `max_parallel`
changes just that, where a plain replace would have cleared everything it left out.

`settings.enabled` ([#176](https://github.com/Colonizer-dev/harness/issues/176)) is the on/off switch
per org: absent or `true` the org is offered as a workspace, `false` — or a form save that names it
`false` — takes the workspace off the list and refuses new colonies for it ("the acme workspace is
switched off; turn it back on in its org settings to start a colony there") while keeping its settings
and its existing colonies: a colony of a switched-off org is still listed and resumable, and an
`orgs.json` written before the switch existed reads as every org on.

`agent.skillsets` is a map of plugin directory name to `true` or `false`: those skillsets are switched on
or off for the org's colonies, on top of the global `plugins` setting; any it doesn't name follow the
global switch. Names are plain directory names, at most 64. An empty map is stored as `null`.

`agent.module` is which installed agent module the org's colonies launch on, shadowing the module chosen
for the whole install (§7.2); `null` inherits. It must name an installed module. The pick is read at
create and recorded on the colony, so a later change moves new colonies only.

`max_parallel` is the org's own parallel limit and `repo_max_parallel` its own per-repository one
(`null` inherits the sandbox module's `repo_max_parallel`, default 3); both are 1 to 32. The limits
layer rather than replace each other: a colony starts only while the global `max_parallel`, the org's
`max_parallel` if set, and the per-repository limit all have room, so the tightest wins. The sandbox
module's `hold_timeout_minutes` (default 30, 1 to 1440) bounds how long an autopilot-held colony keeps
counting: past it the queue parks the colony and frees its slot (see the Watchdog section). The two
per-colony limits override the global ones: `budget_usd` is the org's own spend budget per colony in dollars, `host_disk` its own
host-disk quota per colony, a size like `16G`. `null` inherits the sandbox module's setting (`budget_usd`,
`host_disk`); `0` (or `"0"`) means unlimited, which is how an org opts out of a global limit. The same
validation applies as at the module setting: a budget is `0` or more dollars, a quota must parse as a size.
Past either limit the mothership stops the colony with its worktree kept (§6.5 covers the budget's
gateway half).

`stack` is not a limit, but it overrides the same way: the sandbox stack the org's colonies boot,
shadowing the sandbox module's `preset` (`auto`, which reads each repository's stack at boot, unless
something is pinned above it). `null` inherits.

`close_superseded_prs` (issue #673) is not inherited either: a list of this org's repositories, full
`owner/name`, whose superseded colonies' pull requests Colonizer may close on GitHub when another
colony's pull request merges over them (*Duplicate-colony prevention*, *Superseded colony work*).
Empty — the default, and what inheriting resolves to, since no module setting sits behind it — only
marks the colonies superseded and leaves their pull requests open for a person. Each entry is
validated as a repository name; the compare against a colony's repository is case-insensitive, and
the close is skipped while external writes are blocked (§6.3).

`merge_prs` (issue #807) is the same shape and the same not-inherited rule, for the other
irreversible write: a list of this org's repositories, full `owner/name`, whose pull requests a
colony in a GitHub loop may ask the mothership to merge (`pr_merge`, §6.12). Empty — the default —
refuses every merge, so a repository merges only once the operator lists it here. Each entry is
validated as a repository name, and the compare is case-insensitive like `close_superseded_prs`.

`agent.claude_account` names the Claude account the org's colonies run on (Connections, §4).
`egress` is `{mode, allow, block}` on top of the sandbox module's egress policy: an org can widen its
allow list or add blocks but never remove a global block ([sandbox-network.md](../sandbox-network.md)).
`memory` and `watchdog` switch those modules per org, and `notify` overrides the notify module's
switches and webhook (§6.3, Notify) for the org's colonies.

`sensitivity` moves the provider-mark bar per class (§6.5, `trusted`): each of `open`, `standard`,
`custom`, `vetted` and `restricted` names one of `any`, `vetted` or `trusted`, `null` inheriting the
built-in default. An org can tighten a loose class or loosen `restricted` — but never below `vetted`,
and the API refuses `restricted: "any"`. `restricted_vendors`, when set, pins restricted work to
providers whose recorded `vendor` is on the list (case-insensitive); a provider with no vendor
recorded never matches, and the list must name at least one vendor or be cleared to inherit.

```json
{"settings": {
  "enabled": true,
  "agent": {"module": null, "model": "opus", "subagent_model": "deepseek/deepseek-flash",
            "background_model": null, "skillsets": {"ecc": false, "google-skills": true}},
  "max_parallel": 2,
  "repo_max_parallel": 1,
  "close_superseded_prs": ["acme/api"],
  "merge_prs": ["acme/web"],
  "budget_usd": 20,
  "host_disk": "32G",
  "stack": "rust",
  "egress": {"mode": null, "allow": ["registry.npmjs.org"], "block": null},
  "memory": {"enabled": true},
  "watchdog": {"enabled": true, "stall_minutes": 15, "max_nudges": 3},
  "sensitivity": {"open": null, "standard": "vetted", "custom": null, "vetted": null,
                  "restricted": "vetted", "restricted_vendors": ["anthropic"]}
}}
```

Avatars and the new-org prompt. The mothership fetches the orgs the signed-in GitHub account belongs
to, together with their avatars, and keeps what it saw in `config/known-orgs.json` — login to
`avatar_url`, written only when something changed. A successful fetch is throttled to once every five
minutes; a failed `gh` records nothing and is retried after a minute. Avatars are refreshed for
every org the fetch reports, workspace or not (a switched-off org keeps its face for the Hidden list
and its settings dialog), except the ones still awaiting an answer: the record doubles as the
seen-set, so an unanswered sighting stays out of it, avatar and all, until the `PUT` that answers
records both.
`GET /api/orgs` carries `avatar_url` only where it knows one (the saved record, or the sighting that
is still waiting for an answer); an org that only shows up in the colony list has none, and a UI falls
back to an initial. The same record is the seen-set behind the prompt:

- The **first** fetch after an install — no record yet — adopts every org the account belongs to and
  records them all without asking; the count of workspaces added is logged. That is what keeps an
  upgrade from asking about orgs the account always had.
- After that, an org the record has never heard of is **not** adopted: it appears in the list with
  `"awaiting_decision": true` until a `PUT` answers for it —
  `enabled: true` adds it, `enabled: false` declines it, and either way it is never asked about again.
- An org GitHub stops reporting (the account left it) is dropped from the workspace list by the fetch
  that no longer reports it — unless it has settings of its own saved: a switched-off or declined org
  keeps its `orgs.json` entry on purpose, so it stays listed and reachable. Starting a colony for an
  org with no record also marks it known, because working in an org is an answer. The signed-in
  account's own login is never asked about, but its switch is respected like any other org's:
  switching it off takes its workspace off the list and stops new colonies on its own repositories.
  The prompt itself is in-memory only: after a restart the next fetch rebuilds it from
  `known-orgs.json`.

**Shared memory.** `scope` is `global`, `org` (key = org) or `repo` (key = `owner/repo`).

| Method & path | Purpose |
| --- | --- |
| `GET /api/memory?scope=&key=` | `{scope, key, provider, notes: [Note], proposals: [Proposal]}`; `provider` is `files` or `mem0` |
| `GET /api/memory/proposals` | Every pending proposal, newest first |
| `POST /api/memory/proposals/{id}/approve` | Optional `{title, content}` edits; creates the note |
| `POST /api/memory/proposals/{id}/reject` | Discard |
| `POST /api/memory/notes` | `{scope, key, title, content}`: a note written by you |
| `DELETE /api/memory/notes/{id}?scope=&key=` | Remove a note. With mem0, only one Colonizer wrote into that scope |
| `POST /api/memory/notes/{id}/revoke?scope=&key=` | Optional `{reason}`. Revoke a note: later briefings go without it, and `revoked.json` keeps its provenance (issue #766) |
| `GET /api/memory/candidates` | Fleet-wide candidates and their sightings (colony, repo, commit, confidence), including those not yet promoted |
| `GET /api/memory/mem0` | `{has_key, source, active}`: whether a key is set (`saved` or `MEM0_API_KEY`) and mem0 is the provider. Never the key |
| `GET /api/deja` · `GET /api/deja/search?org=&q=` | Transcript recall (deja, off by default, [colonies.md](../colonies.md#recall-from-earlier-colonies-deja)): whether the deja binary is installed and, per org, whether recall is on, the index size and the last index time; the search runs the recall a colony of that org would get. Owner only. A colony reaches its own org's index through `POST /recall` on the colony gateway, with its colony token |
| `GET /api/history/search?q=&repo=&org=&agent=&status=&since=&until=&limit=` | Search every colony's conversation (issue #739, always on with memory, [colonies.md](../colonies.md#search-earlier-colonies-conversations)): `{hits: [{colony, repo, org, agent, status, created_at, seq, ts, turn, role, snippet}]}`. A hit is a `user_message` or `assistant_text` event whose text contains every whitespace-separated query term, any case; newest colony first, at most 50 hits and three a colony. `since`/`until` take RFC 3339, a naive `YYYY-MM-DDTHH:MM:SS` read as UTC, or a bare `YYYY-MM-DD`; an empty `q`, or a `since`/`until` that is none of these, is a **400**. Owner only. A colony searches its neighbours through `POST /history` on the colony gateway, with its colony token, scoped server-side to its org — org-less colonies of its own repository when it has no org — never itself, and a colony whose sensitivity is `restricted`, missing or unparseable only to a `restricted` caller |
| `PUT /api/memory/mem0` | `{api_key}`: save the key on the mothership (`config/memory-keys/mem0`, mode 0600); an empty string removes it |
| `POST /api/memory/mem0/check` | `{ok, error?}`: try the key against the configured base URL |
| `GET /api/voice` | `{provider, name, model, language, configured, has_key, source, key_optional, max_seconds, max_bytes}`: the voice module's active speech-to-text service. `provider` is `browser` when the module is unset or off; `source` is `saved`, the provider's env var (`OPENAI_API_KEY`, `GROQ_API_KEY`, `DEEPGRAM_API_KEY`, `ELEVENLABS_API_KEY`, `COLONIZER_VOICE_API_KEY`) or `provider:<id>` when a model provider's key on the same host is reused. Never the key |
| `PUT /api/voice/key` | `{provider, api_key}`: save a voice service's key on the mothership (`config/voice-keys/<provider>`, mode 0600, encrypted under `COLONIZER_MASTER_KEY` when set); an empty string removes it. Answers like `GET /api/voice` |
| `POST /api/voice/transcribe` | Body: the raw clip, `Content-Type` `audio/webm`, `audio/ogg`, `audio/mp4`, `audio/mpeg` or `audio/wav`, at most 25 MB. Answers `{text, provider}`. `409` when the module is `browser` or the service lacks its key/base URL, `413` too large, `415` another type, `502` when the service fails (its 401/429 said plainly; the key is never echoed). The clip is forwarded once and not stored |
| `GET /api/maps/{owner}/{repo}` | `{repo, map, mapping}`: the repository's architecture map, `map` = `{repo, revision, generated_at, session, map: {title, subtitle, components: [{id, type, label, sublabel, pos, size, sources: [{path, line?, label?}]}], connections: [{from, to, label?}], boundaries: [{label, wraps}]}}` or `null`, and `mapping` = the newest mapping colony `{id, status, created_at}` or `null`. A mapping colony that ended with a valid `/harness/out/architecture.json` newer than the stored map is picked up here (see "Architecture maps") |
| `POST /api/maps/{owner}/{repo}` | `{repo, mapping}`: launches a mapping colony through the ordinary admission path (`origin: "map"`, autopilot on, the `archify` skillset loaded whatever the org enables), or returns the one already running. 409 when the app has no `archify` skillset |
| `GET /api/maps/{owner}/{repo}/files` | `{repo, revision, paths, truncated}`: every file path at the stored map's revision (else `HEAD`), from `git ls-tree -r` on the mothership's bare clone, at most 20,000 |
| `GET /api/maps/{owner}/{repo}/file?path=<repo-relative path>` | `{repo, path, colonies: [{id, title, issue, status, mode: "changing"\|"reading", activity: [{ts, tool, summary, agent}], diff, diff_truncated}]}`: every live colony on the repository that changed or read `path`. `activity` is its last ≤15 tool calls naming the file (newest first; `agent` = the subagent it ran in), `diff` the committed and uncommitted change since its merge base (`git diff <merge-base> -- path` against the work tree; an untracked regular file as a new-file diff), capped at 200 KB, `null` when unchanged. 400 for an absolute path, a `..`/`.` segment, or more than 1024 characters. Not cached |
| `GET /api/touched` | `{sessions: {id: [path…]}, reading: {id: [path…]}}`: every live colony's changed files, host-side `git status --porcelain -z --untracked-files=all` plus `git diff --name-only origin/<base>...HEAD`, at most 200 a colony; and `reading`, the repository paths its last 40 tool calls looked at (Read/Edit/Write `file_path`, Glob/Grep `path`, `/workspace/…` and, under `/workspace`, relative paths in Bash commands), from the last 256 KiB of `events.jsonl`, newest first, at most 20; both cached for 4 s |

`Note` = `{id, scope, key, title, content, tags, created_at, source}`; `Proposal` adds `status`
(`pending`). `source` = `{session_id, repo, origin}` or `{user: true}`; a colony's note gains `reviewed: true`
when approved, or `reviewed: false` when stored with review off.

**Operator vault proposals.** Notes colonies proposed for the operator vault (issue #777, [memory](memory.md#the-operator-vaults-tools-issue-777)). Owner only.

| Method & path | Purpose |
| --- | --- |
| `GET /api/vault/proposals` | `{configured, inbox, proposals: [{id, path, title, body, reason, source, created_at}]}`, newest first; `inbox` is the folder an accepted note lands in, relative to the vault |
| `POST /api/vault/proposals/{id}/accept` | Writes the note as a new file at `<inbox>/<path>` in the vault and drops the proposal: `{ok, path}`. `409` when no vault is configured or the file already exists (nothing is overwritten), `400` when the inbox or path would leave the vault or crosses a symlink; the proposal stays queued on any error |
| `POST /api/vault/proposals/{id}/reject` | Drops the proposal; the vault is not touched |

**Watchdog.** New module kind `watchdog` (provider `default`, on by default; settings
`stall_minutes` = 15, `max_nudges` = 3, `waiting_minutes` = 30, `provider_retry_max_attempts` = 4) and kind `memory` (provider `files`,
on by default; setting `require_review` = true; off lets only `repo` notes skip review). `Session`
gains `last_activity_at` and `attention`:

```json
{"attention": {"reason": "stalled|waiting_for_answer|nudges_exhausted|autopilot_held|provider_quota_exhausted|hold_timeout|agent_failed|model_error", "since": "…", "nudges": 2, "detail": "…"}}
```

Every minute the mothership checks live colonies. A colony that is `running` with no agent event for
`stall_minutes` is nudged with a `user_message` whose id starts with `watchdog-` (UIs render it as a
notice, not a user bubble), at most `max_nudges` times per stall; then `attention.reason` becomes
`nudges_exhausted` — the flag is written with the log line that says the colony needs you, and
gateway traffic alone (a request in flight) does not clear it again; only agent progress does. A
colony blocked on an exhausted provider is flagged `provider_quota_exhausted` instead of being nudged
(§6.5, Provider out of quota cards). A question open longer than `waiting_minutes` sets `waiting_for_answer`. An
autopilot colony whose turn ends with an error (not an interrupt) — or whose completion claim the
mothership contradicted (Autopilot, below) — is not published and gets
`autopilot_held`. An error the retry classifier calls transient (a gateway 5xx, 429 or 529, an
unreachable or overloaded provider, a timeout, a dropped or refused connection) is retried first
(issue #980): the colony parks with `parked.reason` and `attention.reason` `provider_retry`, and the
queue tick continues it after 2, 5, 10 and 20 minutes, re-checking under the lifecycle lock that it
is still parked for that reason. After `provider_retry_max_attempts` (0–4; 0 turns the retry off)
it is held as `autopilot_held`, with a message naming the provider's error; a clean turn end resets
the count (`Session.provider_retries`). A colony whose Claude account answered 401 or 403 parks
with reason `waiting_for_account` ahead of all of this (Claude account health, in
[harness-api.md](harness-api.md#get-apistatus)). On every tick, whether or not the watchdog is
enabled, a colony that is `waiting_for_answer` with no question actually pending is set back to
`idle` with a colony log line saying why (issue #981). A flag carries a `detail` when the mothership can say why in one line: a claim
held as `autopilot_held` names the failing checks and the `out/verify-*.log` their output is in
(Done-verification, below); the other reasons carry none. Two reasons come from elsewhere: `agent_failed` when the runner never started (§1),
and `model_error`, set by the gateway when an upstream model call fails (§6.5) and cleared when the
provider answers again. Any new agent progress event (not a `status` change, a `model_changed`, a
`boundary` event, or a watchdog or judge message) clears `attention`; a disabled watchdog clears only the reasons
it sets itself. The exception is `control_defeat` (issue #609, docs/boundaries.md "Watchdog
signatures"): set when the colony's `boundary` events complete a control-defeat signature, it carries
`signature`, `detail` and the events as `evidence`, the watchdog's tick neither nudges over it nor
replaces it, and only a person's own `user_message` (or the colony stopping) clears it. A turn that dies on an exhausted provider parks the colony instead of holding it
(see §6.5 "Quota exhaustion"): `status` `parked` with the worktree kept, and `attention.reason`
`provider_quota_exhausted` — like `autopilot_held`, set outside the watchdog, so it does not
announce here either. A hold that waits longer than the sandbox module's `hold_timeout_minutes`
(default 30) parks the same way ([#213]): an `idle` colony with `attention.reason` `autopilot_held`
past the timeout parks with `attention.reason` `hold_timeout`, so its microVM slot
frees for queued colonies (one org's held colonies cannot block every other org past the timeout)
while staying resumable. Within the timeout a held colony still counts against the parallel limits.

One wedge the rules above cannot see is finished separately (issue #878): a running colony whose
runner emitted its final, non-delta `assistant_text` — its "it's done" — and then never ended the
turn. Two minutes past that final answer, with no tool call in flight, no open question and nothing
through the gateway, the watchdog asks agentd's `/v1/health`. If agentd answers and its runner is
still running, it ends the turn for it: a host-generated `watchdog_turn_end` event goes on the
colony's `events.jsonl` (like the §6.6 host chain events, and cut out of the agentd reconnect cursor
the same way) and the ordinary turn-end path runs, so spend, budget, verification and publish proceed
as they would have. A later real `turn_end` for the same turn publishes nothing new (the description
is unchanged), and a synthetic end carries no cost, so no spend is double-counted. If agentd does
not answer, the watchdog keeps the final answer for the next tick to retry, logs once that it could
not finish the turn, and leaves the colony to the stall handling above rather than restarting it.

**Notify.** New module kind `notify` (provider `default`, issue #119; settings `on_question` = true,
`on_attention` = true, `on_failed` = true, `on_pull_request` = true, `on_provider` = true,
`on_quota` = true, `on_lifecycle` = false, `desktop` = false, `webhook_url` = ""). Like `autonomy`, it is absent from `modules.json` until first configured: it
announces colonies to the outside world, so it is off until asked for. Every thirty seconds the
mothership diffs the session list against what it last saw, seeding new colonies without firing so a
restart does not replay a backlog, and announces the edges once each: `status` became
`waiting_for_answer` (question), `failed`, or `pr_opened` (pull request), a pull request fell behind its
base with no colony left to rebase it (`needs_rebase`, under `on_attention`), or `attention.reason`
became the watchdog's `stalled` or `nudges_exhausted` — `waiting_for_answer` belongs to the question
event and `autopilot_held` is not the watchdog's, so neither announces here. The text is one short
line naming the repository and issue (`acme/webshop #42 needs an answer`, `… has stalled`, `… is out
of nudges`, `… failed`, `… opened a pull request`); colonies with no issue are just the repository.
Every announcement first passes a shared rate limiter (quiet hours, a cooldown, an hourly quota;
questions get past the soft limits): a held one is summed up in an hourly `digest` event instead.

A provider failing under fan-out does not look like a failing provider from the colonies' side — it
looks like every colony running slowly at once. So every thirty seconds the same loop also rates each
configured model provider's cumulative usage by the one rule every surface shares (§6.5): at least 50
requests to be rated, a failure rate of 10% or more to be degraded
([#184](https://github.com/Colonizer-dev/harness/issues/184)). A provider crossing the line announces
the `provider_degraded` event once — `zai is failing 29.4% of its requests` — and stays quiet while
it holds; the announcement re-arms only when the rate falls clearly back, under 8%, so a rate
hovering at the line does not announce every tick. Because those counters are cumulative for the
life of the install, that re-arm is not routine, and it is worth saying so plainly: the lifetime
rate only falls under 8% once healthy traffic has diluted the outage many times over — after the
episode that prompted this (9,599 failures in 32,689 requests), a provider that never failed again
would need roughly 120,000 cumulative requests to get there — so a long-lived tally may in practice
never re-arm, and a second, separate outage months later announces nothing. A provider seen for the
first time only seeds its state, so a restart does not announce the providers that were already
failing before it. A provider
has no colony and no org, so its settings are the notify module's own global ones — org overrides do
not reach it, and `on_provider` has no per-org override — and the announcement carries no repository
at all: only the id and name the operator chose, plus the counters behind the rate.

A provider that runs out of quota while colonies are blocked on it opens one out-of-quota card
(§6.5, issue #767), and the same loop announces the card once as the `provider_quota_exhausted` event
under `on_quota` — `Z.AI is out of quota: 3 colonies are waiting; resets Oct 6, 04:00 UTC` — one line
per provider, never one per colony. It stays quiet while the card stays open and re-arms when the
card closes (the plan reset, or every colony moved on). Like a provider crossing, it has no per-org
override, and it carries only the provider's id and name, its reset time and how many colonies wait.
A colony's own `provider_quota_exhausted` attention flag is never an event of its own.

The desktop channel runs `osascript -e 'display notification …'` on macOS or `notify-send` on Linux
under a graphical session, with the text passed as an argument and escaped for AppleScript. Over SSH
or headless it does nothing, logging the reason once rather than a line a tick. A non-empty
`webhook_url` POSTs one JSON note per event:

```json
{"version": 1, "id": "evt_3f9c0d1e2a4b5c6d7e8f90a1b2c3d4e5",
 "event": "question|attention|failed|pull_request|needs_rebase|provider_degraded|provider_quota_exhausted|digest", "at": "2026-09-18T00:00:00+00:00",
 "text": "acme/webshop #42 needs an answer",
 "colony": {"id": "…", "repo": "acme/webshop", "org": "acme", "issue": 42, "status": "waiting_for_answer"},
 "pr_url": null,
 "provider": null}
```

The note carries no repository content — no issue title, no question text, no branch, no error — and
`pr_url` is the colony's pull request address only on the `pull_request` and `needs_rebase` events, `null` otherwise.
`provider` is `null` on every colony event; on `provider_degraded` it is the reverse — `colony` and
`pr_url` are `null` and `provider` carries `{id, name, failure_pct, avg_latency_ms, requests, failure}`
(`failure` is the code of its most recent failure, or `null`); on `provider_quota_exhausted`
`provider` is `{id, name, reset_at, colonies}` (`colonies` is a count) — so
a receiver reads one eight-key shape either way.
`on_lifecycle` adds the whole colony lifecycle to the webhook — one event per status transition
(`queued`, `started`, `running`, `idle`, `answered`, `publishing`, `merged`, `closed`,
`no_changes`, `parked`, `resumed`, `stopped`, plus `cleaned`), webhook only and outside the rate
limiter (issue #897); every payload also carries `version` (`1`). The full event list and the
schema are in [Webhooks](webhooks.md).
`id` is the event's stable id (issue #896): the same event always carries the same one, so a
receiver can dedupe on it, and it also travels in the `X-Colonizer-Event-Id` header. How it is
derived, and how to verify a request, is in [Webhooks](webhooks.md).
Every request carries `X-Colonizer-Timestamp` (unix seconds); when a signing secret is set
(`config/notify-secret`, mode 0600, or `COLONIZER_NOTIFY_SECRET`) it also carries
`X-Colonizer-Signature: sha256=<hex>` — HMAC-SHA256 over the exact bytes `"{timestamp}.{body}"` —
and without one it is sent unsigned. Transport errors and non-2xx answers are logged, never
retried.

| Method & path | Purpose |
| --- | --- |
| `GET /api/notify/secret` | `{has_secret, source}`: whether a webhook signing secret is set (`file` or `env` for `COLONIZER_NOTIFY_SECRET`). Never the secret |
| `PUT /api/notify/secret` | `{secret}`: save it on the mothership; `null` or an empty string removes it. **400** over 512 characters or with non-printable characters |

**Autopilot.** When a turn ends, an autopilot colony is published only if the turn ended without an
error or open question and the agent wrote or updated `/harness/out/pr.md` since the previous turn
ended. An unchanged `pr.md` from an earlier turn doesn't publish a colony the maintainer is still
talking to. While external writes are blocked (below), a turn that would publish does not: the colony
log gets a `warn` line (`autopilot: not publishing, external writes are blocked
(COLONIZER_NO_EXTERNAL_EFFECTS); press Create PR when writes are enabled`) and the colony's
`attention` is left as it was. Opening pull requests as drafts (`publish.settings.draft`) does not
change this: a draft PR is still an external write, refused the same way as a ready one.

**Done-verification (issue #328).** Before a completion claim is published, the mothership verifies it
on its own. It snapshots the colony's work — commits and uncommitted files — without touching the
worktree, reads the git state directly (commits ahead of base, changed files, whether the paths the PR
description names are on the branch), and re-runs the checks the diff calls for in fresh one-shot
microVMs over a `git archive` of the snapshot: never on the host, never from the agent's logs or exit
codes. The verdict is `confirmed` (every fresh run is green and the git state matches the description),
`contradicted` (a check or the git state disagrees, the contradictions stated plainly), `inconclusive`
([#672]: a check fails on the colony's work but fails on the merge-base too, so it is not this
colony's doing) or `unverifiable` — an empty branch, no check known, the runner unavailable — which is
never treated as confirmed. Each run's exit number is the one the guest itself writes to a report file
mounted for exactly that (`/colonizer-verify/exit`); the sandbox's own exit code only corroborates it,
so a runner that never reported has not verified anything. A check that fails is re-run once on the
merge-base commit, in a fresh checkout of the same kind: failing there too makes the verdict
`inconclusive` — autopilot still publishes, and the pull request body notes it — while only a failure
new against the base is `contradicted`; a base that cannot be run for infra reasons leaves the head
failure a contradiction. Base results are cached in memory per repository, image, base commit,
directory and command. The last ~200 lines of a failing check's output are written to the session's
`out/verify-<check>.log` (`out/verify-cargo-test.log`, `out/verify-web-npm-test.log`), and failing
test names parsed from cargo, vitest or jest output — up to five — are quoted in the contradiction,
which is the held colony's `attention.detail`. Autopilot publishes on `confirmed`, `inconclusive` and
on `unverifiable` exactly as before; on `contradicted` the colony is held with `attention.reason`
`autopilot_held` and the contradictions in the event below.

The description's paths are the backticked path-like tokens in `pr.md`; a path counts as in the
repository when the branch or the diff carries it or its directory is on the branch (an example URL
or an untracked build dir is wording, not weighed). The git state **contradicts** the description
only when it names in-repo paths and **none** of them is on the branch or in the diff, and no changed
file is named in the text either (by path, or by file name in prose): the work it describes is not
there. A described path that is missing while the description is otherwise borne out is an
**advisory** — descriptions routinely name files that were deliberately not created, belong to other
or future work, or were renamed on the way. Advisories are listed once each in `advisories`, logged,
and added to the published pull request as a "Verification notes" block; they never change the
verdict or hold autopilot.

The checks are never guessed from chat text. An explicit `verify` on the colony (`NewSession.verify`)
or the `publish` module's `verify` setting — `auto` by default, `none`, or a command — replaces the
whole selection with that one command. For `auto` ([#672]) the checks are chosen from what the diff
touches, so a colony is never held for code it did not go near, and each check runs from the checkout
root or, when it belongs to a subdirectory's own package, from that subdirectory:

- **Rust:** any `*.rs` file, `Cargo.toml` or `Cargo.lock` in the diff → `cargo test`. A diff with no
  Rust in it skips `cargo test` entirely.
- **Every other changed file:** the test script of the nearest ancestor directory with a
  `package.json`, read from the **base** branch and run by that package's own package manager — the
  `packageManager` field through corepack (`corepack pnpm …`, `corepack yarn …`; `bun` and `npm`
  directly), else the package's lockfile: `bun.lock`/`bun.lockb` → `bun install --frozen-lockfile &&
  bun run test`, `pnpm-lock.yaml` → `pnpm install --frozen-lockfile && pnpm test`, `yarn.lock` →
  `yarn install --immutable && yarn test` (Yarn 2+'s lockfile) or `--frozen-lockfile` (Yarn 1's),
  `package-lock.json`/`npm-shrinkwrap.json` → `npm ci && npm test`, and no lockfile →
  `npm install && npm test`; a bun package without the script runs `bun test` when there are test
  files for it. So `web/**` runs web's own test script (vitest), not the root's, and a module's
  lockfile runs that module's tests when it declares a script.
- **Covered by neither:** the root Makefile's `test:` target → `make test`, and nothing at all when
  the repository declares none.

The checks run sequentially, one microVM each, in the diff's order unless the `publish` module's
`verify_focus` setting says otherwise ([#584](https://github.com/Colonizer-dev/harness/issues/584)): `act` runs first the check owning the most changed
files and, when it contradicts the claim, skips the rest; `shadow` (the default) keeps the order and
only measures; `off` does neither. Shadow and act append one row per verification that ran a check
to the data dir's `jev_focus.jsonl` — `{kind: "focus", ts, session, mode, candidates: [{label,
owned}], chosen, would_catch, verdict, actual_first_failure_ms, focused_first_failure_ms, total_ms,
checks_run}`, `chosen` being `full` when there is nothing to focus on (fewer than two checks) —
and `confirmed` still needs every check run green. Each fresh-checkout VM checks for the tool the command
needs before running it: a tool the colony image does not carry (the default node image has no bun or
pnpm, until the colony-node image is pinned) makes that check `unverifiable`, named in the summary,
never `contradicted`. A branch that
rewrote an entry a resolved check comes from (`scripts.test`, the Makefile) would be grading its own
homework: that check comes back `unverifiable` with that said plainly, and nothing runs. A check
whose directory the branch deleted is skipped rather than run to a meaningless exit 1 — if no check
is left, the claim is `unverifiable` for want of one. `verify: none` means
unverifiable by declaration. Verification also runs with autopilot off and while external writes are
blocked; it then records the verdict without publishing. The verdict is appended to the colony's
`events.jsonl` as a host event
with the usual `seq`/`ts` — host-generated the same way as the finding chain's events (§6.6), so the
runner-event schema is unchanged — and the `Session` carries the latest one as `verification` (the
same object minus `type`/`seq`/`ts`):

```jsonc
{"type":"verification","verdict":"confirmed","by_declaration":false,"summary":"one plain line",
 "contradictions":[],"advisories":[],"inconclusive":[],"command":"npm test","command_source":"package.json","exit_code":0,
 "tests_ms":8100,"commits":2,"files_changed":["src/scan.rs"],"snapshot":"<sha>|null","ms":12345}
```

`command_source` names where the command came from (`config` for an explicit command — the colony's
`verify` or the `publish` module's setting — or what on the base branch declared it:
`packageManager` (the package.json field), the lockfile that picked the package manager
(`bun.lock`, `bun.lockb`, `pnpm-lock.yaml`, `yarn.lock`, `package-lock.json`,
`npm-shrinkwrap.json`), `package.json` (no lockfile, so npm), `Cargo.toml`, `Makefile`; null when no
command ran; with several checks, `command` and `command_source` are the first check's),
`inconclusive` lists the checks that failed on the merge-base as well, one reviewer-ready clause each
([#672]; absent on events recorded before it existed), `exit_code` is the first failing check's (0 when
every check reported green) and `tests_ms` the checks' summed run time, `files_changed` lists at most 50, and `ms` is the verification's whole wall time — purely mechanical, no model calls.

**No-write kill-switch (issue #84).** Setting `COLONIZER_NO_EXTERNAL_EFFECTS` or `COLONIZER_NO_WRITE`
in the mothership's environment to any non-empty value other than `0`, `false`, `off` or `no`
(trimmed, case-insensitive) blocks the writes the mothership makes on a colony's behalf, each failing
closed:

- Publish: `POST /api/sessions/{id}/publish` answers **409**, and the publish task (autopilot's
  included) refuses before it claims the colony, so the colony keeps its status and its `error`
  carries the reason. The commit, push and pull-request steps each check again before they run.
- Retargeting a stacked child's pull request: GitHub is not asked, the child keeps its old base and
  its log says so. Nothing retries it; retarget it by hand once writes are allowed.
- Reviewing a fix PR (§6.6): whatever the verdict, nothing is merged or commented on GitHub; a
  `warn` line in the fix colony's log says so.
- Findings (§6.6): ignored, no issue is filed.

Independently of the kill-switch, a publish refuses to open (or reuse) a pull request when the
branch's local head moved after the push step, and logs the SHA-256 of the exact PR body it sends
(`opening the pull request; body sha256 <hex>`). The hash is only logged; nothing yet checks it
against an approval (issue #98).
