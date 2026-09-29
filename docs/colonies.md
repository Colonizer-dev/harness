# A colony's life

A colony is one agent in one microVM, working one repository, often on one issue. This guide
follows a colony from launch to cleanup. For each feature it says what the feature does, how to
switch it on or off and what the default is, and where it stops.

Many of these features already have a full reference elsewhere. In those cases this guide gives
a short summary and a link. Settings named here are module settings, changed in the cockpit under
**Settings → Modules** (for example **Settings → Modules → Sandbox**). Environment variables are
read by the mothership when it starts.

- [Launching a colony](#launching-a-colony)
- [Claims: one colony per issue](#claims-one-colony-per-issue)
- [When a merge supersedes a colony](#when-a-merge-supersedes-a-colony)
- [Epics are refused](#epics-are-refused)
- [Questions, and who answers them](#questions-and-who-answers-them)
- [Suspending a colony that waits for you](#suspending-a-colony-that-waits-for-you)
- [Stop, resume and delete](#stop-resume-and-delete)
- [Budgets and plan balance](#budgets-and-plan-balance)
- [What a colony cost](#what-a-colony-cost)
- [Verifying "done"](#verifying-done)
- [Conditional instructions](#conditional-instructions)
- [Shared memory](#shared-memory)
- [Recall from earlier colonies (deja)](#recall-from-earlier-colonies-deja)
- [The repo-explorer subagent](#the-repo-explorer-subagent)
- [Who caused each event](#who-caused-each-event)
- [The log archive](#the-log-archive)
- [Automatic cleanup of finished worktrees](#automatic-cleanup-of-finished-worktrees)
- [Measuring Jev compaction](#measuring-jev-compaction)
- [Rate limits on notifications and the judge](#rate-limits-on-notifications-and-the-judge)

## Launching a colony

There are three ways to start a colony. All of them go through `POST /api/sessions`, so they all
get the same checks: the parallel limits, issue claims and the epic guard.

- **The cockpit.** The Colonize pane opens from the orange button in the sidebar, the one on the
  dashboard, or ⌘K. It lists open issues to hand off, and it can draft an issue from text you
  type or speak. The **Launch** page also lists a repository's issues and has a "Launch colony"
  button for a colony with no issue.
- **The CLI.** `colonizer launch owner/repo --issue 42` starts a colony on issue 42.
  `colonizer launch owner/repo "migrate the auth tests"` starts one on a task with no issue. The
  other flags are `--model`, `--subagent-model` and `--autopilot`/`--no-autopilot`. See
  [cli.md](cli.md).
- **MCP.** The `launch_colony` tool takes `repo`, `issue?`, `task?`, `model?` and `autopilot?`.
  It needs a token with the `launch` scope. See [mcp.md](mcp.md).

"Autopilot" means the colony publishes on its own. When the agent finishes cleanly and has
written its PR description, the mothership pushes the branch and opens the pull request. The
default comes from the **Publish** module's `autopilot` setting, which is on.

A launch past the parallel limit is not refused. The colony is created `queued` and starts when
a slot frees. The limits are the Sandbox settings `max_parallel` (default 3) and
`repo_max_parallel` (default 3), plus an org's own limit if it sets one. See
[architecture.md, Session lifecycle](architecture.md#session-lifecycle) for every state a colony
passes through.

**Limits.** The CLI and the MCP tool cannot pass `allow_duplicate`, `queue_behind_holder` or
`allow_epic`. A launch from either one on a held issue or an epic is refused with a 409 (CLI exit
code 5). Use the cockpit, or call the API directly, to override.

## Claims: one colony per issue

Only one colony at a time may hold an issue. A second launch on the same issue is refused with a
409 that names the colony holding it. An issue is held while a colony on it is queued, live,
publishing, or has an open pull request. A colony that stopped, failed, found no changes, or whose
pull request merged or closed frees the issue.

There are two ways past a held issue. Both are checkboxes in the cockpit's launch form:

- **Allow duplicate** (`allow_duplicate: true`) starts a second colony anyway.
- **Wait behind the holder** (`queue_behind_holder: true`) queues the new colony behind the one
  holding the issue. When the holder releases the issue, the oldest waiter takes over. The
  cockpit shows the place in line, for example "Queued behind `ab12cd34` · #2 in line". Before a
  waiter starts, the mothership checks GitHub again. If the holder's pull request merged, or
  another mothership has claimed the issue, the waiter fails instead of redoing finished work.

On GitHub a launch marks the issue with the `colonizer:claimed` label and a claim comment. The
label comes off when the colony releases the issue. When a mothership starts, it removes marks it
left behind for colonies that are gone.

**Limits.** The duplicate check only looks at this mothership's colonies. Other motherships are
seen only through the GitHub label and comment. If a mothership never comes back, its label stays
until a person removes it. The full rules are in
[protocol.md, Duplicate-colony prevention and issue claims](protocol.md#duplicate-colony-prevention-and-issue-claims).

## When a merge supersedes a colony

When one colony's pull request merges, other colonies of the same repository whose work it covered
are marked **superseded**. One covers another when the two carry the same supply-chain target (the
package and advisory a colony was launched to fix), are on the same issue, or changed largely the
same files — at least three files in common (lockfiles don't count, so two dependency bumps sharing
a manifest and its lockfile stay two pieces of work), making up at least 80% of the smaller side's
changed-file list. A colony waiting for another to release its issue, and one stacked on the merging
colony, are left alone: that merge is what releases them.

A superseded colony is not deleted. One that is queued, parked or suspended is held exactly where it
is: it does not start — a quota-parked colony stays parked even when its provider recovers — and
Resume refuses, until you keep it. A running colony is told in its chat that the changes are now in
main, and it rebases and continues — or finishes with no changes if main already covers the task. A
superseded colony with an open pull request has that pull request closed with a note, but only for
repositories listed in the org's **Close superseded PRs** workspace setting; otherwise it is left
open for you — and the publish module's merge train skips it until you keep it.

In the cockpit a superseded colony wears a "Superseded by #N" badge, and while it is held a banner
offers **Keep** (run it anyway), with **Stop** beside it for a live or queued colony. Launching a
second colony for a supply-chain target one already holds is refused, not queued behind the holder,
unless the launch passes `allow_duplicate`.

**Limits.** Only this mothership's colonies are compared, and a pull request's file list is capped
at 500 paths. The API shapes are in
[protocol.md, Duplicate-colony prevention and issue claims](protocol.md#duplicate-colony-prevention-and-issue-claims).

## Epics are refused

An epic is a planning issue whose work lives in its sub-issues. A colony on the epic itself would
repeat the work of the colonies on those sub-issues. So a launch on an epic is refused with a 409.
The message says why the issue counts as an epic and lists up to ten of its open sub-issues to
launch instead.

An issue counts as an epic when any of these is true:

- it has sub-issues;
- it has a label named `epic` (any case);
- its title ends with `(epic)` or starts with `Epic:` (any case).

**How to override.** In the launch form, check "Start on the epic anyway". Over the API, send
`allow_epic: true`. The Colonize pane greys out epics and leaves them out of bulk hand-offs.

**Limits.** If the GitHub lookup fails, the launch goes ahead. The guard helps you avoid a mistake.
It is not an access control. There is no CLI or MCP override (see above). Details are in
[protocol.md](protocol.md#duplicate-colony-prevention-and-issue-claims), under "Epics".

## Questions, and who answers them

An agent asks you something with a multiple-choice question. The colony's status becomes
`waiting_for_answer`. You can answer in the cockpit, from your phone, with
`colonizer answer <id> <number|label|text>`, or with the MCP tool `answer_colony`.

Every question has a **risk class**. From lowest to highest the classes are `read_only`,
`workspace_write`, `publish_affecting` and `credential_adjacent`. The Claude Code runner assigns
the class by scanning the question and its options for words. Words about keys, tokens, passwords
or `.env` make it `credential_adjacent`. Words about publishing, pushing, merging, deploying or
pull requests make it `publish_affecting`. Anything else is `workspace_write`. The scan only ever
rounds up. The ACP runner assigns the class from the tool kind instead.

**The judge.** By default nobody answers for you. The **Autonomy** module is `off`, so questions
wait for a person. Choose "Judge model" to let a model answer questions you leave open:

| Setting | Default | What it does |
| --- | --- | --- |
| `model` | none | The judge model, from a provider you added under Model providers. Your Claude login is never used. |
| `after_minutes` | 10 | How long a question waits for you first. |
| `max_answers` | 5 | Judged answers per colony. After that, questions wait for you. |
| `free_text` | off | Whether the judge may answer a question that has no options. |
| `risk_ceiling` | `workspace_write` | The highest risk class the judge may answer. |

The judge only picks among the options the agent offered. A question above the ceiling is never
answered by the judge. It waits for you however long it takes, and the colony log says once why
it was left. The cockpit marks judged answers as the judge's.

**Limits.** The judge does not answer suspended colonies (see the next section). A question with
an unknown risk class counts as above every ceiling. The judge also answers to the
[rate limits](#rate-limits-on-notifications-and-the-judge) that notifications use. The full
contract is in [protocol.md, §6.2b Autonomous mode](protocol.md#62b-autonomous-mode-mothership).

## Suspending a colony that waits for you

A colony waiting for your answer keeps a microVM and a parallel slot while it does nothing. After
a grace period, the mothership suspends it.

- **What stops:** the microVM is removed. The colony no longer holds a parallel slot, so queued
  colonies can start.
- **What is kept:** the worktree, and the agent's own session transcript. The status stays
  `waiting_for_answer`, and the question stays open everywhere you can answer it.
- **After you answer:** the answer is saved on the colony first, with the time it arrived
  (`pending_answer.answered_at`). If no slot is free yet, the colony stays `waiting_for_answer`
  with `suspended` and `pending_answer` both set — that pair means "answered, waiting for a slot",
  and the cockpit shows "Answered · resumes when a slot frees" with the colony's place in line.
  The colony log says the same at answer time.
- **How it resumes:** on the next queue tick with a free slot, a fresh microVM boots — answered
  colonies come back in answer order, ahead of new launches — and the agent continues its own
  session with your answer as the next message. Pressing Resume also delivers a saved answer. If
  the boot fails or the mothership restarts, the answer is not lost.

Two Sandbox settings control this. Both are mothership-wide, with no per-org override:

| Setting | Default | |
| --- | --- | --- |
| `suspend_waiting` | on | Suspend colonies that wait for an answer. |
| `suspend_after_minutes` | 10 | The grace period, from 1 to 1440 minutes. |

**Limits.**

- Only an agent that can resume its own session is suspended. Today that is Claude Code, Codex and
  ACP agents that advertise session loading. Any
  other agent keeps its microVM, and the colony log says so once.
- This is transcript resume, not a memory snapshot. Processes that were running inside the VM,
  such as a dev server, are gone after the resume.
- Stopping a suspended colony clears the suspension and any saved answer.

A separate Sandbox setting, `hold_timeout_minutes` (default 30), parks a colony that autopilot
held. It removes the microVM and keeps the worktree. The full design, including why this is not
a VM snapshot, is in
[architecture.md, Suspending colonies that wait for an answer](architecture.md#suspending-colonies-that-wait-for-an-answer).

## Stop, resume and delete

- **Stop** (`colonizer stop <id>`, the MCP tool `stop_colony`, or the cockpit) removes the
  microVM and keeps the worktree. Stopping a queued colony takes it out of the queue.
- **Resume** (`colonizer resume <id>`, `resume_colony`, or the cockpit) boots a fresh microVM on
  the same worktree and branch. The agent is told to continue from what is already there. You can
  resume a colony that is `stopped` or `failed`, as long as its worktree still exists. If the
  parallel limit is full, the resume queues.
- **Delete** (the cockpit, or `DELETE /api/sessions/{id}`) removes the colony, its chat and its
  worktree. You can only delete a colony that is not live and not publishing. Its logs go to the
  [log archive](#the-log-archive) first.

The mothership also stops colonies itself: when a budget or the host-disk quota is passed, and
when a microVM dies on its own. In every case the worktree is kept, so Resume continues.

## Budgets and plan balance

Two Sandbox settings cap what one colony may spend. Both default to `0`, which means unlimited.

- **`budget_usd`**: dollars per colony, counting Claude's own cost estimate plus what the provider
  gateway priced. An org can set its own value.
- **`budget_tokens`**: tokens per colony routed through the provider gateway, counted whether or
  not the provider has prices. Use it for prepaid token or coding plans. Their pricing is empty,
  so they cost $0 and `budget_usd` never trips. This setting is mothership-wide, with no per-org
  override.

When a colony passes either budget, its next routed request gets a 403 and the colony is stopped
with its worktree kept. Raise the budget and press Resume to continue.

**Plan balance.** A provider can say where to read what is left in its plan. In **Settings →
Model providers**, open a provider and fill in "Plan balance": a URL and a JSON pointer into the
answer (the `quota` field, `{url, pointer}`). The URL must be on the same scheme, host and port as
the provider's base URL, because the provider's own credential is sent to it. The health card then
shows "N left in plan". A failing balance check never marks the provider unhealthy.

**Limits.**

- `budget_tokens` counts only traffic routed through the gateway. Claude traffic goes straight to
  `api.anthropic.com` and does not pass the gateway, so it is not counted here.
- Requests already in flight are not reserved against `budget_tokens`, so parallel requests can
  take a colony a little past it before it stops.
- No provider comes with a plan-balance URL preset. You enter the URL yourself.

The reference is in [README.md, Configuration](../README.md#configuration) and
[architecture.md, Per-colony limits](architecture.md#per-colony-limits).

## What a colony cost

Every colony-scoped row of the spend journal (`<data>/spend.jsonl`) names the colony (`session`)
and the agent module that ran it (`agent`, for example `claude-code` or `codex`). To read it back
per colony:

```sh
node scripts/colony-report.mjs --costs                             # the last 30 days, like the cockpit's spend history
node scripts/colony-report.mjs --costs --since 2026-09-01 --days 90
node scripts/colony-report.mjs --costs --json
```

The report ranks colonies by spend. It splits each one into **estimated** dollars (the agent's own
figure at the end of each turn) and **metered** dollars (what the gateway priced for a routed
provider). A colony that used only unpriced providers shows `unpriced — tokens only`, never $0.
Rows without a colony, such as older rows or cockpit chat, are grouped under `unattributed`, so the
total covers the whole window. A harness × model table follows.

**Limits.** A turn that used several models adds its cost to the totals but not to any single
model, because the split is not known. Dollars routed through the gateway are never split by model
either. The per-org and per-day views are in
[protocol.md, §6.8 Spend](protocol.md#68-spend-per-org-and-per-day).

## Verifying "done"

When a colony finishes a turn cleanly and has written its PR description, it is claiming it is
done. The mothership checks that claim before autopilot publishes:

1. It snapshots the colony's work without touching the worktree.
2. It reads the git state itself: commits ahead of the base branch, changed files, and whether the
   files the PR description names are on the branch.
3. It runs the repository's test command in a fresh, one-shot microVM, on a `git archive` export
   of the snapshot. The command never runs on the host, and the result never comes from the
   agent's own logs. The test run has a 20-minute limit.

The verdict is `confirmed`, `contradicted` or `unverifiable`. Autopilot publishes on `confirmed`
and `unverifiable`, and holds the colony on `contradicted`. A claim is contradicted only if the
tests fail, or if *none* of the files the description names are on the branch. If some named
files are missing but others are there, that is only an **advisory**. Advisories are shown with
the verdict and added to the pull request as "Verification notes". They never change the verdict.

**Which test command.** The **Publish** module's `verify` setting decides. You can override it for
one colony at launch.

- `auto` (the default) reads the base branch. For a `package.json` with a `test` script, it uses
  the repository's own package manager: the `packageManager` field first, then the lockfile
  (bun, pnpm, yarn or npm), else npm. Otherwise `cargo test` for a `Cargo.toml`, or `make test` for
  a Makefile with a `test:` target.
- `none` records every claim as unverifiable without checking.
- Any other text is the test command itself.

**Limits.** If the colony image lacks the tool the command needs, the claim is unverifiable. The
command comes from the base branch, so a branch that changes it cannot change what is run. The
full description is in [architecture.md, Session lifecycle](architecture.md#session-lifecycle),
step 5.

## Conditional instructions

`CLAUDE.md` and `AGENTS.md` load once when a colony starts, and a compaction can summarise them
away. Conditional instructions are loaded only when they apply, and loaded again after a
compaction.

- A `FOOTGUNS.md` file in any directory applies when the agent works on files in or under that
  directory. So does an `AGENTS.md` in a subdirectory.
- `.colonizer/instructions.toml` in the repository maps conditions to instruction files:

  ```toml
  [[rule]]
  file = "docs/STYLE.md"
  paths = ["web/**", "*.css"]   # gitignore-style globs
  labels = ["frontend"]         # the issue's labels, any one of them
  ```

There is nothing to switch on. Add the files to the repository. The Claude Code module watches
the paths the agent touches, and each time a fragment loads, the colony log says so.

**Limits.** This is in the Claude Code module only. A fragment is capped at 16 KiB. A file outside
the worktree, including one reached through `..` or a symlink, is refused. A path condition holds
only while one of the last 20 distinct paths touched matches it. Arrays in `instructions.toml`
must fit on one line. See
[the Claude Code module README](../modules/agents/claude-code/README.md#conditional-instructions).

## Shared memory

Shared memory is notes per repository, per org and global, which colonies can read. Inside a
colony, memory is read-only.

- The orchestrator can read memory and propose a new note.
- Subagents can read memory but cannot propose. The runner refuses the proposal and tells the
  subagent to report what it learned to the orchestrator instead.
- A proposal records who made it (`source.origin`, which is `orchestrator`, and the colony id).
  The review queue shows this.

Nothing becomes memory until it is approved. The **Memory** module's `require_review` setting is on
by default. Turning it off only lets repository notes through. Org and global notes are always
reviewed.

**Limits.** Nothing extracts memories from a conversation automatically. With the `mem0` provider,
the mem0 key never enters a colony. The access table is in
[architecture.md, Shared memory access](architecture.md#shared-memory-access).

## Recall from earlier colonies (deja)

**What it is.** After a colony finishes, the mothership can index its Claude Code transcripts into a
per-org [deja](https://github.com/vshulcz/deja-vu) index on the host. Later colonies of the same
org get a `recall` tool that searches it, read-only, through the colony gateway with the colony's
own token. deja is a local binary: no model call, and nothing leaves the machine.

**Turning it on.** Off by default, at two levels. Switch on "Transcript recall (deja)" under
Settings → Memory for the install, then in each org's settings dialog (Memory group). An org can
also opt an enabled install back out. Memory itself must be on for the org. The installer fetches
the deja binary; without it, recall stays empty and the mothership prints one line saying so.
`GET /api/deja` shows whether the binary is installed and, per org, whether recall is on, the
index size and when it last indexed. `GET /api/deja/search?org=<org>&q=<query>` runs the same
search a colony would.

**Limits.** Only Claude Code transcripts are indexed, and only the Claude Code runner has the
`recall` tool. A colony without an org is never indexed and gets no recall, and one org's index
never answers another org's query. Before anything is written, every secret value the mothership
knows is scrubbed, raw and JSON-escaped, for values of six characters and up, and deja's own
pattern redaction runs on top. Base64- or percent-encoded forms of a secret are not caught.

## The repo-explorer subagent

Every Claude Code colony has a read-only subagent called `repo-explorer`. It answers questions
about the code's structure, such as "where is Y defined" or "who calls Z". Before it searches with
`find` or `grep`, it checks for a retrieval skill in the colony, such as
[graft](skill-packs.md#graft), and uses that first.

It is always on, and there is no setting. It has the same write restrictions as the built-in
`Explore` subagent.

**Limits.** It only prefers graft when graft is installed. graft is a downloadable skillset, not
part of the default install. Without it, `repo-explorer` searches the same way `Explore` does.

## Who caused each event

Every line the mothership writes to a colony's event log carries an `origin` field. It names what
caused the line: `user`, `agent`, `subagent`, `watchdog`, `autonomy` (the judge), `burn_down`,
`redteam`, `notify` or `system`.

It is always on. To read one colony's transcript filtered by origin:

```sh
node scripts/colony-report.mjs --transcript <id> --origin autonomy,watchdog
```

**Limits.** Lines written before this field existed have no `origin`. The vocabulary is in
[protocol.md, Origins](protocol.md#origins).

## The log archive

When a colony ends, the mothership saves its session directory (event logs, the PR description,
and so on) as a compressed bundle:

```
<data>/archive/<org>/<repo>/<yyyy>/<mm>/<colony-id>.tar.zst
```

Each bundle has a JSON record beside it with the colony's repository, issue, status, pull request,
cost and tokens. Bundles are never overwritten. If a colony is resumed and ends again, a new
revision is written (`<id>.r2.tar.zst`). An unchanged colony is not archived twice.

It is always on, with no setting.

**Delete keeps the logs.** Deleting a colony archives it first. If that archive fails, the delete
is refused and nothing is removed. In the cockpit, a second prompt asks whether to delete the
archived logs too. Cancel keeps them. Over the API, `DELETE /api/sessions/{id}?purge_logs=true`
removes the bundles too.

**Cleaning up the archive.** Nothing removes bundles automatically, and there is no saved retention
setting. The Storage panel on the Overview page shows the archive's size. Its "Automatic cleanup"
form removes bundles on request. Set "keep N days", "cap at X GB" or both, press Preview, then
Apply. Every bundle is the only copy of that colony's logs, so nothing is removed unless you also
check "Allow deleting the only copy". Apply removes exactly what the preview listed. If the
archive changed since the preview, you are asked to preview again. Over the API, this is
`POST /api/archive/retention` with `{keep_days, max_gb, allow_single_copy, dry_run, expect}`, and
`GET /api/archive` lists the bundles.

**Limits.** The archive is local only. The disk figures and the free-disk floor in
`GET /api/storage` do not count it yet. You cannot open an archived colony in the cockpit. Read
the bundle with `tar`.

## Automatic cleanup of finished worktrees

Finished colonies leave worktrees behind. Every five minutes the mothership removes the worktree
and local branch of a finished colony whose work is safe on the remote. That means a colony with
a pull request that is open, merged or closed, or one that found no changes. It is removed once
it is older than the retention window since its last update.

| Control | Default | |
| --- | --- | --- |
| `COLONIZER_RECLAIM` | on | `0`, `false`, `off` or `no` switches automatic cleanup off. Manual cleanup still works. |
| `COLONIZER_RECLAIM_RETENTION_HOURS` | 12 | Hours after the last update before a finished colony is cleaned up. |
| Sandbox `min_free_disk` | 5G | Below this much free disk, queued colonies wait, and finished colonies are cleaned up without waiting for the retention window. `COLONIZER_RECLAIM_MIN_FREE` sets it when no value is saved in Settings. |
| Sandbox `warn_free_disk` | 10G | The cockpit warns below this much free disk. |
| "Keep worktree" in the colony view | off | Exempts one colony (`POST /api/sessions/{id}/retain` with `{"keep": true}`). |

**Limits.** A cleaned-up colony cannot be resumed, because resume needs its worktree. That includes
a colony whose pull request is still open. Stopped and failed colonies are never cleaned up
automatically, because they can still be resumed. Work that was never pushed is never deleted:
`GET /api/storage` lists it under `unpushed` for you to publish or clean up by hand. See
[protocol.md, Automatic reclamation](protocol.md#automatic-reclamation).

## Measuring Jev compaction

Jev compaction is an optional Claude Code setting (`jev_compaction`, off by default). When the
context fills, it deletes stale tool calls by score instead of summarising the conversation.
The mothership also measures how good those decisions were. This measurement only records. It
never changes what compaction keeps or drops.

When a compaction pass is applied, the mothership writes one `decision` row per chunk to
`<data>/jev_ladder.jsonl`. If the agent later repeats a tool call that a decision was about (the
same tool with the same input), it writes a `reread` row: evidence that the chunk was needed. The
colony log shows the running precision and recall.

There is no separate switch. It runs whenever `jev_compaction` is on. Turning on Jev compaction
sends the colony's conversation to TypeSafe (`api.typesafe.ai`) at each compaction, and needs a
`JEV_API_KEY`. Read [protocol.md, Token savings](protocol.md#token-savings) before you turn it on.

**Limits.** A pass that was computed but not applied is not measured. A reread only counts when
the tool and its input are exactly the same. The bench-wide report that grades colonies against
each other is `bench.mjs jev`
([bench.md, Grading Jev compaction](bench.md#grading-jev-compaction)). This is separate from the
Jev routing second opinion (`jev_shadow_mode`,
[protocol.md §6.1c](protocol.md#61c-jev-second-opinion-shadow-mode)), which is also shadow-only.

## Rate limits on notifications and the judge

Notifications (desktop and webhook) and the autonomy judge both check one shared record before
they act. The record is `<data>/ledger.json`. It stops a busy afternoon of colonies from flooding
you:

| | Notifications | Judge |
| --- | --- | --- |
| Per rolling hour | 12, then held for the digest | 30, then held |
| Per rolling day | 60, then dropped | 100, then dropped |
| Same topic again within | 10 minutes (held) | no cooldown |
| Same fact, told once within | 1 hour | 24 hours |
| Per topic per day | 10 | 20 per colony |

What is held is summarised in one digest line at most once an hour, sent down the same channels.
A question that is blocking a colony skips the hourly limit and the cooldown, but not the daily
limits or the duplicate check. Every delivered, held and dropped message is counted by class.
`GET /api/status` reports the counts under `ledger`, with no colony ids and no question text.

It is always on. **Limits:** the numbers above are fixed in code and cannot be changed yet. There
are no quiet hours by default. The cockpit does not show the counts, only `GET /api/status` does.
The watchdog's nudges do not use this record yet. If `ledger.json` is corrupt at startup, it is
moved aside and the record starts empty.
