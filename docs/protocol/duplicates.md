# Duplicate-colony prevention and issue claims

Part of the [Colonizer protocol](../protocol.md).

[colonies.md](../colonies.md#claims-one-colony-per-issue) tells the same story for an operator;
this section is the API contract.

`POST /api/sessions` refuses a second colony on the same `(repo, issue)` while another colony
holds it: one `queued`, live (`starting`, `running`, `waiting_for_answer`, `idle`), `publishing`,
or `pr_opened` — answered **409** naming the holder, its state, and its PR URL when one is open.
Terminal states (`stopped`, `failed`, `no_changes`, `merged`, `closed`) free the issue for a retry.
Parking ([#213]) frees it locally the same way — but the colony's claim on GitHub stays: a park is
a pause, the colony is expected to come back, and the claim is what keeps a second colony off the
issue until it does (or until a stop, which does release).
A fast pre-check reads under a read lock and the authoritative claim re-checks while the admission
write lock is held, so two launches racing each other cannot both slip through; the loser gets its
holder back for the 409. The cockpit warns inline before submit — "already held by `<id>`", with a
link to that colony — and offers the ways past it as checkboxes. Scope is per-host only: fleet
peers listed by `GET /api/hosts` are not consulted.

Three ways past a held issue:

- `allow_duplicate: true` starts a second colony anyway.
- `queue_behind_holder: true` joins the issue's successor queue instead: the colony comes back
  `queued` with `claim_wait: true` and `queued_behind` naming the holder (`allow_duplicate` wins
  if both are set). Once only waiters remain, the oldest counts as the holder: a fresh launch
  without the flag is refused naming it. When the holder releases the issue — stop, failure, no
  changes, a closed PR — the oldest waiter takes over, inheriting the claim mark below; the
  remaining waiters re-point `queued_behind` at the new holder. Before a waiter is promoted the
  forge is re-checked: if the holder's pull request merged, or the claim has moved to another
  mothership, the waiter fails with the same message a fresh launch would get. The cockpit counts
  each queued waiter's place among the waiters created before it and shows it as
  "Queued behind `<id>` · #2 in line".
- A remote conflict — another mothership's claim, or an open or merged pull request, or a
  `colonizer/issue-<n>-*` branch that is not this launch's own — is a **409**, unless
  `allow_duplicate` is set, which skips the GitHub check. GitHub is checked
  on the `queue_behind_holder` path too: a conflict attributable to one of this mothership's own
  colonies on the issue is the holder being queued behind and is tolerated, while a merged PR —
  ours included, the issue is done — or a foreign claim refuses as above.

**Epics.** An epic is a planning container: a colony on it duplicates the colonies on its
sub-issues. Before any other issue check, a launch on an issue asks GitHub about it
(`GET /repos/{o}/{r}/issues/{n}`, plus `…/sub_issues` only when it has some or GitHub does not
summarise them) and answers **409** when it has sub-issues (`sub_issues_summary.total`, or the list),
carries a label named `epic` (any case), or has a title ending `(epic)` or starting `Epic:` (any case).
The message names the reason, lists up to ten open sub-issues to launch instead, and names the way
out: `allow_epic: true` skips the check. A lookup that fails lets the launch through. Every launch on
an issue goes through the create path behind `POST /api/sessions` (the dashboard, the Colonize pane,
the MCP tool, the CLI), so the one check covers them; loops, burn-down, red-team hunters and the map
call that path directly and launch no issue. The cockpit's
issue lists mark epics from `GET …/issues`'s `epic` and leave them out of bulk hand-offs.

On GitHub a launch marks its claim: the `colonizer:claimed` label plus a
`<!-- colonizer:claim host="<hostname> (<host_id>)" colony="<id>" issue="<n>" -->` comment naming
the mothership and colony. Release removes the label and posts a release note, but only while the
issue's latest claim is this colony's (host id and colony both match); a merged pull request keeps
the mark on purpose, as the record of who did the work. On boot a mothership reaps the marks it
owns — open issues whose latest claim carries its host id but whose colony is gone from its session
list, or ended the way a release would have followed (stopped, failed or no changes without a pull
request, or closed) — and never touches another host's mark. A release that races a successor's
claim puts the label back once it sees the newer claim comment. So the worst case is a mothership that
crashes leaving its marks until its next boot; one that never returns leaves them until a human
removes the label. GitLab, Linear and Jira should follow the same claim shape when those forges
land. Contested-claim detection after launch, and label repair, are not implemented yet.

**Superseded colony work.** When a colony's pull request merges, every other colony of the same org
and repository whose work it covered is marked `superseded` (issue #673). One covers another when
the first of these holds:

- both carry the same `supply_chain` target (compared trimmed and case-insensitively);
- both are on the same issue;
- their changed files overlap enough: at least three files in common — lockfiles (`Cargo.lock`,
  `package-lock.json`, `go.sum`, …, matched by name) don't count, so two dependency bumps sharing a
  manifest and its lockfile are two pieces of work — making up at least 80% of the smaller side's
  list. The files are read off the pull requests and capped at 500 paths, and a colony whose pull
  request listed none cannot be covered this way.

Colonies that are already finished (`merged`, `closed`, `no_changes`, `stopped`, `failed`) are never
marked, and neither is a `claim_wait` waiter (the queue retires those itself when the holder merges)
nor a stacked child whose parent is the merging colony — that merge is what releases it; a
`pr_opened` colony counts, since its pull request is exactly what may need closing. The marking runs
at the merge edge itself, before any queue tick could start a covered colony, and a second pass runs
once the merged pull request's final file list has been read back — a file overlap only shows itself
there, and the first pass's markings are skipped. An existing `superseded` record gives way only to
a kept one from a different merge: an unkept hold is never traded away, the same merge never marks
twice, and a colony you kept can be marked again by a later merge. The record is
`{by, pr_url, pr?, title, reason, at, kept}`, with `by` naming the colony whose pull request merged.
What happens to a newly marked colony, by its state:

- `queued`, parked or suspended: it is not started. The queue leaves a queued colony in place (it is
  not retired), a quota-parked colony stays `parked` instead of re-queueing when its provider
  recovers, and `POST /api/sessions/{id}/resume` answers **409** naming the covering pull request,
  until the operator keeps it.
- live (not starting): the runner is told, as a user message, that the changes just merged to main
  and overlap this colony's work — it can fetch, rebase onto main and continue, or finish with no
  changes if main already covers the task.
- `pr_opened`: its pull request is closed with a note — but only when the org's
  `close_superseded_prs` setting lists the repository (§6.3) and external writes are not blocked
  (§6.3); otherwise it is left open for a person.

`POST /api/sessions/{id}/keep` sets `kept: true`: the hold lifts and the queue starts the colony as
slots free, while the record stays for the history. Launch-time dedupe mirrors the issue hold: a
second live colony for one supply-chain target of the same repository is a **409** naming the holder
and its state (or its open pull request) — a target is refused, not queued behind its holder, and
there is no `queue_behind_holder` for one — and `allow_duplicate: true` starts one anyway. The check
runs as a fast pre-check plus an authoritative re-check under the admission write lock, and a
finished holder leaves its target free to try again.

Module `schema` is a JSON Schema subset (also used for `settings` in agent `module.json` manifests):

```json
{
  "type": "object",
  "properties": {
    "image":  { "type": "string",  "title": "Image", "description": "glibc-based OCI image, pinned by digest", "default": "node:24-bookworm@sha256:6dac556d…" },
    "cpus":   { "type": "integer", "title": "vCPUs", "minimum": 1, "maximum": 64, "default": 4 },
    "model":  { "type": "string",  "title": "Model", "enum": ["", "opus", "sonnet", "haiku"], "default": "" },
    "draft":  { "type": "boolean", "title": "Open PRs as drafts", "default": false }
  }
}
```

Digests in these examples are cut short on purpose: the exact hex a release boots lives in
`crates/colonizer/images.lock`, and quoting a full one here would only rot the next time a pin bumps.

Supported property keys: `type` (`string` | `integer` | `number` | `boolean` | `array` of strings, edited as one comma-separated line), `title`, `description`,
`default`, `enum` (renders a select), `minimum`, `maximum`. `settings` holds the current values;
missing values mean the `default`.

`Session`:

```json
{
  "id": "ab12cd34", "repo": "owner/repo", "issue": 12, "issue_title": "…",
  "status": "queued|starting|running|waiting_for_answer|idle|publishing|pr_opened|merged|closed|no_changes|parked|stopped|failed",
  "branch": "colonizer/issue-12-ab12cd34", "base": "main", "org": "owner", "worktree": "/…",
  "sandbox": "colonizer-ab12cd34", "mesh": {"name": "colonizer-ab12cd34", "ip": "100.64.0.3"},
  "agent": "claude-code", "autopilot": false,
  "pr_url": null, "publish_stage": "committed|pushed|pr_opened", "error": null,
  "merged_at": "…", "pr_opened_at": "…", "ci_state": "success|failure|pending|no_checks",
  "changed_paths": ["apps/pwa/src/main.ts"], "summary": "Fix the login redirect loop on expired sessions",
  "supply_chain": {"package": "lodash", "advisory": "ghsa-…"}, "superseded": null,
  "cost_usd": 0.42, "routed_cost_usd": null, "routed_tokens": null, "host_disk_bytes": null, "cleaned_up": false,
  "model_routing": {…}, "verification": {…}, "attention": null, "last_activity_at": "…",
  "boot_cpus": 4, "boot_memory": "8g",
  "boot_timing": {"total_ms": 12345, "phases": [{"name": "issue", "ms": 240}, {"name": "git", "ms": 810}]},
  "created_at": "…", "updated_at": "…"
}
```

`boot_cpus` and `boot_memory` are how this colony's microVM was sized at boot, exactly as `msb run`
received them. The microsandbox exposes no guest CPU% or RSS metrics (agentd serves only health,
events, pty and shutdown), so the boot spec is the only per-colony number about the VM — guest
figures are omitted rather than faked. `null` on colonies booted before these fields existed.

The example shows the common fields; the record carries more, and most optional ones are left out
of the JSON while unset rather than sent as `null`. Among them: `placement`, `origin`, `suspended`,
`parked`, `agent_session`, `pending_answer`, `switch_note`, `prewarm`, `instructions`, `model_tier`, `model_override`, `subagent_model_override`,
`claude_account`, `launched_by_token` (scoped tokens, above), `queued_behind` and `claim_wait`
(issue claims, below), `parent` and `stack` (a colony started with `after`), `needs_rebase`,
`keep_worktree` (reclamation, below), `app_slot` (§4 `POST /api/update/apply`), `model_routing`
(§6.1b), `model_substitutions` (§6.1b — the models the boot resolved away from because the gateway
would have refused them for the task's sensitivity class, as `[{setting, from, to, reason}]`),
`verification` (§6.3), `attention` (§6.3, Watchdog) and `unseen_failure` — set when the
colony moves into `failed` and cleared by the `seen` route above, so a failure stays in the
needs-you count (the app badge's) until a person has opened the colony; colonies from before the
field existed load as seen.

`supply_chain` is the `{package, advisory}` target the colony was launched to fix (§4 `POST /api/sessions`),
and `superseded` is set when a same-repository colony's pull request merged over this one's work
(*Superseded colony work*, below): `{by, pr_url, pr?, title, reason: "supply_chain"|"issue"|"files", at, kept}`.
Both are left out entirely on a colony they do not apply to.

`origin` names what launched the colony: `burn_down` (§6.2c), `redteam` (§6.7), `map` or
`map:loop:<loop id>` (Architecture maps), `loop:<loop id>` (Loops), the built-in loops'
`docs-loop`, `supply-chain:<ecosystem>`, `ts-any:<module>`, `merge-train:redo:<colony id>` and
`merge-train:fix:<owner/repo>` (Loops, below), or `chat` / `colonize` for a
colony a person started from the chat or the Colonize pane. It is taken from the create body, and
absent for a plain launch.

`placement` is why the fleet's placement policy put this colony where it runs, in the policy's own
words — this member and its free capacity (`archlinux: 3 free slots`), `pinned to box-2`, or a note
that a peer had room but cross-member launch is not built yet ([fleet.md](../fleet.md#placement)). It
is recorded on a fresh launch and left out of colonies written before it existed or re-admitted
from the queue; the cockpit shows it under the colony's status.

`suspended` is set while the colony waits on its user with its microVM torn down
([#562]): `{at, snapshot, reason: "waiting_for_answer", path: "session_resume"}` — the status stays
`waiting_for_answer`, the question stays answerable, and the colony holds no parallel slot. The
only reason and path this build writes are the two shown; `snapshot` is what a real VM memory
snapshot would carry, always `null` today. `agent_session` is the runner's own conversation id
from the `agent_session` event (§2), what a resumed boot continues. `pending_answer` holds an
answer that arrived while the colony was suspended, `{question_id, prompt, answered_at?}`:
persisted before the answer is acknowledged and cleared only once a boot has delivered it, so a
failed boot or a mothership restart never loses it. `answered_at` (RFC 3339) is when the answer
arrived, and is left out of records saved before answers kept one — those restore by the
suspension's own time. A colony with `suspended` and `pending_answer` both set, status still
`waiting_for_answer`, is answered and waiting for a slot ([#667]): no new status is invented for
it, restores take such colonies in answer order ahead of fresh launches, and the cockpit shows
"Answered · resumes when a slot frees" with the colony's place in line (its rank among the
answered ones by that same order). All three are absent on a colony that has never been suspended.

`prewarm` is set while a warm-up of such a colony is under way ([#701]) — someone opened the
question, so the mothership is booting it ahead of the answer: `{requested_at, started_at?,
ready_at?}`, the request's arrival, the boot's admission and the VM and agent link coming up, each
RFC 3339 and absent until it happens. `suspended` stays set while warming and the status moves on
to `starting` and then `running` — the colony holds its slot again — but an answer still lands in
`pending_answer` exactly as for any suspended colony, and is delivered as the first message once
the runner is up. `prewarm_timeout_minutes` (default 5) with no answer, a mothership restart or a
failed boot clears `prewarm` and leaves the colony suspended again, never failed; answering
clears `prewarm`, `suspended` and `pending_answer` together once the answer is delivered.

`switch_note` is set when the colony's agent module was switched mid-task ([#737], `POST
/api/sessions/{id}/switch-agent`): the note the resumed runner is told first, telling the new agent
it is continuing another agent's session and that the converted transcript may be missing detail.
It is what the boot's resume trigger and first turn read, and it is cleared once the runner is up —
persisted so a failed boot or a mothership restart never loses the switch, exactly like
`pending_answer`.

`parked` is set on a colony the host set aside for a reason it may outlive ([#213]): the status is
`parked` — not live, so it holds no parallel slot, and not terminal either, so it is never
auto-reclaimed and its spend is not settled — with the record
`{at, reason, resets_at?, vm_kept}`: `at` (RFC 3339), the `reason` it parked
(`provider_quota_exhausted` when the provider's plan ran out (§6.5), `hold_timeout` when an
autopilot hold outlived its slot), `resets_at` — the provider's reset words verbatim, only when the
reason names one — and `vm_kept`, whether the park left the microVM running. Parking persists the
record and the `provider_quota_exhausted`/`hold_timeout` attention flag (the cockpit banner's
resume ticket) in one step before any teardown, and the worktree and branch always stay. With the
`resume` module's `discard_vm` on (the default) the microVM is removed — but only after git verified
the worktree reads back; a worktree that cannot be verified (or `discard_vm` off) keeps the
microVM running idle instead, and `vm_kept` says so. `null` (or absent) on a colony that has never
been parked; cleared on resume. Resume semantics: a `vm_kept` park resumes warm — the running
microVM is kept and the idle agent is prompted to continue — while any other resume is cold: a
fresh microVM boots on the kept worktree, first tearing down a microVM the park kept if the
`discard_vm` setting changed meanwhile, and the brief carries a short digest of the previous run's
event log so the agent picks up where it left off. If the warm path is not achievable (restart
dropped the agent link, slot taken, setting changed), resume falls back to cold and the log says
why. The choice lives on a module: kind `resume`, provider `default` (the only one, and always on —
the module is required), whose `discard_vm` setting (boolean, default `true`) is what parking reads.
Read with `GET /api/modules`, changed with `PUT /api/modules/resume`.

`merged_at`, `pr_opened_at` and `ci_state` come from the PR watcher's `gh pr view` (`mergedAt`,
`createdAt`, `statusCheckRollup`), and for colonies merged before they existed from a best-effort
startup backfill; each is left out until known. `ci_state` sums the head commit's checks: any failed,
cancelled, timed-out or action-required check is `failure`, any unfinished one `pending`, all
passing, neutral or skipped `success`, and a pull request with no checks `no_checks`. A merged
pull request keeps its last settled verdict. The cockpit derives lead time (`merged_at` −
`created_at`), PR cycle time (`merged_at` − `pr_opened_at`) and CI pass rate from them.

`changed_paths` lists the files the colony's pull request changes (the first 500), read with
`gh pr view --json files` when the PR opens and again when it merges, and for older colonies by a
best-effort startup backfill; left out while empty. The cockpit maps each path to the monorepo
package with the longest matching path to show a monorepo's packages under its repository row.

`summary` is the task in one plain sentence (at most 120 characters), written by a cheap model
from the issue's title and body (or an open session's instructions) shortly after launch, rewritten
from the pull request's title and body when it opens, and backfilled at startup for up to 200
colonies that have none, live ones first. Only that task text is sent. The model is, in order: the agent module's
`summary_model` (`<provider>/<model>` or a Claude model); else the first `<provider>/<model>` among
its `subagent_model`, `model_low` and `background_model` whose provider is configured, called
through that provider's saved config and key like the autonomy judge; else `claude-haiku-4-5` with
an Anthropic API key (`sk-ant-api…`). The Claude subscription token is never used for summaries, so
with nothing else configured there are none (logged once). It is left out until written; the
`summaries` setting (`COLONIZER_SUMMARIES`, on by default) turns it off, and a failed request
leaves it out while the cockpit shows the title.

`publish_stage` records how far the last publish got (committed, pushed or pr_opened) so a retry
finishes from where it stopped and browsers can show the progress. It is left out until a publish
commits something, kept in place when a publish fails part-way, and cleared when a publish finds no
changes; the publish re-derives the truth from git and origin, so the field is the record, not the
authority.

`boot_timing` is where the last launch's time went. It is cleared when a colony is claimed for a
boot or a resume. While the colony is `starting` it is `{"phases": [...]}` with the phases done so
far, one added as each completes. `total_ms` appears only once the boot finishes, so its absence
marks a boot still under way or, on a colony that is no longer starting, one that stopped part way;
such a boot keeps the phases it got through. The phases are consecutive spans in boot order and
partition the launch, so they sum to at most `total_ms`:

| Phase | Covers |
| :--- | :--- |
| `issue` | Fetching the issue and resolving the base branch |
| `git` | Syncing the bare clone and laying down the worktree |
| `providers` | Health probes for the model providers this colony will use (60 s host-side cache; the manual health check always probes and warms it) |
| `mesh-start` | Starting the mesh and minting the colony's pre-auth key |
| `image-pull` | Downloading the colony image, when it was not in the local cache. Near zero once it is |
| `vm-boot` | `msb run`. The pull is its own phase above, unless it failed and `msb run` had to do it |
| `mesh-join` | Waiting for the node to come up in headscale |
| `agentd` | Waiting for the agent daemon to answer `/v1/health` |

The same breakdown is written to the colony's log as one line
(`boot 12345 ms: issue 240, git 810, …`), so it survives in the event stream whether or not anyone
reads the API.

Warm-start contract: what a second boot reuses, and what it cannot. The bare clone per repository
under the data dir persists, so `git` only runs `fetch --prune origin` — the colony must branch off
current upstream, not yesterday's. The colony image is pinned by digest (`presets::pinned()`
boots `<image>@sha256:<digest>` with the digest compiled in from `crates/colonizer/images.lock`)
and stays in the local cache,
pre-warmed from Settings (`POST /api/sandbox/pull`). The `providers` probes are served from a 60 s
host-side cache keyed on provider id plus endpoint, which the manual health check writes through —
the unreachable warning still logs on every boot, from the cached value when that is what was used,
while a route with no fallback model is refused, on a fresh probe.
Everything else repeats per boot on purpose: a new boot lays down a fresh worktree, `mesh-join`
waits on a node whose single-use pre-auth key was minted for this boot (a VM that did not exist
until `vm-boot` has no identity to reuse, and it runs its own tailscaled), and `agentd` runs inside
that fresh VM, so its `/v1/health` must be polled per boot. A VM warm pool is not attempted until
per-phase data shows it would pay.

Pre-worktree boot steps retry transient failures instead of failing the colony on the first blip.
Fetching the issue, resolving the default branch, syncing the bare clone and creating the
worktree each retry connection errors, DNS failures, HTTP 429 and HTTP 500/502/503/504 — and any
unrecognised error, which during boot is likelier a blip than a new permanent failure mode.
Permanent failures (a 404 the account cannot see, refused credentials) fail fast with the same
access wording as before. Retries back off exponentially from 1 s, capped at 30 s with jitter,
within a single 20-minute budget shared across the whole pre-worktree phase — each step gets whatever remains (`COLONIZER_BOOT_RETRY_BUDGET_SECS` overrides it in seconds);
when the budget is spent the colony fails naming the step, the attempts, the elapsed time and the
last error. The budget's clock is persisted on the colony, so a harness restart resumes it: a boot
that died before its worktree existed is re-queued on restart and continues under the same budget
rather than starting a new one — or being left where no resume could reach it, since without a
worktree a stopped colony can never be resumed.

`routed_cost_usd` is what the provider gateway has recorded for responses it routed (§6.5), on top of
`cost_usd`, which is only what Claude itself reports, when a turn ends. `routed_tokens` is what it has
counted the same responses at in tokens, whether or not it priced them; the sandbox module's
`budget_tokens` — global, no per-org override — answers to it, and passing it stops the colony exactly
as an overspend does. `host_disk_bytes` is what the
colony leaves on the host (its worktree plus its session directory) as last measured, every few
minutes, quota or not — `null` only until the first measurement. Both are estimates. A colony's dollar budget answers to `cost_usd + routed_cost_usd` and its
host-disk quota to `host_disk_bytes`; past either, the mothership stops the colony: `status` `stopped`,
the reason in `error`, and the worktree kept, so raising the limit (or, for the quota, cleaning up) and
pressing Resume continues it.

`GET /api/sessions/{id}` — only that route, never the list or the WS session frame — adds two fields,
each omitted when absent. `recent_events` is the last ≤ 20 events from the tail of `events.jsonl`
(at most the last 64 KiB are read, never the whole file), oldest first, as
`[{seq, ts, type, summary}]`: `seq`/`ts` are whatever the line carried (`ts` is `null` when the line
has none); `summary` is a one-line digest — the text for `assistant_text`/`user_message`, the pending
question for `question`, the `state` for `status`, the tool name for `tool_call`, the output for
`tool_result`, the result (or `"ok"`) for `turn_end` — newlines collapsed and cut to 200 chars plus `…`. `assistant_text_delta`
and `thinking` lines are skipped as noise. `diagnosis` is the best guess for a non-terminal colony
(`queued`, `starting`, `running`, `waiting_for_answer`, `idle`): `{state, text, resets_at?}`, with
`state` exactly one of `queued`, `booting`, `working`, `waiting_on_human`, `waiting_on_provider` or
`stuck`, first match wins. `queued` reads "queued, waiting for a free slot"; `starting` reads
"booting for \<dur\>; \<last phase\> done, next phase running \<dur\>", or "booting for \<dur\>; first
phase running" before any phase finished (clocked from the boot attempt, else the colony's birth);
a quota attention flag, or the tail's most recent `assistant_text` with no `user_message` after it
classifying as provider exhaustion, reads "waiting on provider: quota exhausted[, resets \<X\>]" with
`resets_at` carrying the provider's reset words verbatim when it named a reset; a colony waiting for
an answer, held by autopilot, or `idle` otherwise reads "waiting for an answer[: \<question\>]" /
"autopilot held, waiting for the next message" / "idle, waiting for the next message", naming the
tail's pending question when it has one; a `running` colony active within the last
15 minutes (the watchdog's stall default) reads "working (last activity \<dur\> ago)"; anything else
reads "no activity for \<dur\>; last event: \<type\>: \<summary>" (or "no events yet"). Durations are
compact (`45s`, `12m`, `3h 5m`, `2d 3h`).

## Automatic reclamation

Finished colonies accumulate worktrees, so a sweeper reclaims them without being asked. A colony is
reclaimed only once it is finished and pushed (`pr_opened`, `merged` or `closed` with `pr_url` set) or
ended with nothing to push (`no_changes`), **and** is older than `COLONIZER_RECLAIM_RETENTION_HOURS`
(default 12 h) past its last update. `stopped` and `failed` colonies are never reclaimed, since they can
be resumed. The sweep runs every five minutes and also removes microVMs no colony owns. Reclaiming removes the worktree and local branch exactly like manual
cleanup — and carries the same trade-off: a reclaimed colony is unresumable, because resume boots a
fresh microVM on the kept worktree and there is no worktree left. A `parked` colony ([#213]) is
never reclaimed: it is paused, not finished, and its worktree is the run it may yet resume.

What the sweeper never takes: a colony with no `pr_url` (other than `no_changes`, which has nothing to lose). Unpushed work may be the only copy of the
agent's changes, so it is never auto-deleted — `GET /api/storage` lists it under `unpushed` for a
person to publish or clean up by hand. Worktree directories with no colony behind them are swept as
orphans, but only with a git-state guard: dirty or unpushed content is reported, not removed.

Two more guards round it out. When free disk drops below the floor — the sandbox module's
`min_free_disk` (default 5G, or `COLONIZER_RECLAIM_MIN_FREE` when no explicit setting is saved) — the
sweeper ignores the retention window and takes every eligible colony, oldest first, and the queue
stops admitting new colonies until headroom returns; the sibling `warn_free_disk` (default 10G, no env var) warns earlier without holding the
queue, and 0 turns either off. And reclamation can be
switched off entirely — globally with `COLONIZER_RECLAIM=0`, or per colony with `keep_worktree` via
`POST /api/sessions/{id}/retain` (the colony view's "Keep worktree" checkbox). `GET /api/storage`
shows the whole ledger: byte totals, the `reclaimable` list with per-colony `due`, the `unpushed`
list, the `orphans` with their planned `action`, and the retention (`retention_secs`) and enablement in
force. The answer is cached for 30 s.
