# A colony's life

A colony is one agent in one microVM, working one repository, often on one issue. This guide
follows a colony from launch to cleanup. For each feature it says what the feature does, how to
switch it on or off and what the default is, and where it stops.

Many of these features already have a full reference elsewhere. In those cases this guide gives
a short summary and a link. Settings named here are module settings, changed in the cockpit under
**Settings → Modules** (for example **Settings → Modules → Sandbox**). Environment variables are
read by the mothership when it starts.

- [Launching a colony](#launching-a-colony)
- [Tools in the colony image and the setup hook](#tools-in-the-colony-image-and-the-setup-hook)
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

**Overrides.** The CLI (`colonizer launch --allow-duplicate`, `--queue-behind-holder`,
`--allow-epic`) and the MCP `launch_colony` tool (`allow_duplicate`, `queue_behind_holder`,
`allow_epic`) take the same overrides the cockpit's checkboxes and `POST /api/sessions` do. Without
one, a launch on a held issue or an epic is refused with a 409 (CLI exit code 5).

## Tools in the colony image and the setup hook

Every colony image carries a small, language-agnostic toolbox a repository's own build or
verification may reach for: `python3` with pip and venv, `python3-yaml` (PyYAML), `jq`, `ripgrep`,
`curl`, `git`, `make`, `unzip` and `ca-certificates`. The node image also carries bun, pnpm, yarn
classic and corepack. The image is pinned by digest, so a tool the image does not carry is one a
done-claim check reports unverifiable (see [Verifying "done"](#verifying-done)). On the Debian-based
presets `pip3 install` works despite PEP 668 — the images set `PIP_BREAK_SYSTEM_PACKAGES=1` for a
disposable VM — and an isolated `python3 -m venv` is the alternative.

For a repository that needs more, add a `.colonizer/setup.sh` to it. The daemon runs `sh` on it — it
need only be a file, not executable — as root, from the worktree, on every boot (a colony's microVM
filesystem does not survive a suspend, so a resume runs it again), once its HTTP listener is up and
before the coding agent starts:

```sh
#!/bin/sh
# Idempotent: the install is a no-op once the tool is there.
command -v shellcheck >/dev/null 2>&1 || apt-get install -y shellcheck
```

The hook runs as root with the VM's full capabilities, without the seccomp and capability hardening
the agent's runner child gets — that hardening forbids `apt-get`, the hook's point. Treat
`.colonizer/setup.sh` like a Dockerfile `RUN` step: repository-controlled code with more privilege
than the agent, so add it only to a repository you trust, and write it to be idempotent — it reruns
after every resume. It gets 600 s; past that it is killed, its whole process group with it. Its
output goes to `/tmp/colonizer-setup.log`, and the outcome is a `log` event (elapsed time on success;
exit code and log tail on failure); a failure or timeout never fails the boot, only adds a note to
the agent's first prompt. A stop or restart while the hook runs kills it and skips the agent. The
hook is subject to the colony's egress policy; in `allowlist` mode the package mirrors every preset's
toolbox fetches from are always allowed (see
[sandbox-network.md](sandbox-network.md#modes-and-settings)).

**Cost.** With no hook present the check is one file test, effectively free; a hook costs its own
runtime, logged per colony. The toolbox adds no boot-time work — the image is larger, paid once on
the first `image-pull` and cached after. Measured with `apt-cache` against a `node:24-bookworm` base:
the added packages are about 12 MB of downloads and 60 MB installed, almost all the Python 3
interpreter and its pip/venv stack. It is an estimate, not a VM measurement.

**What the agent is told.** On a fresh session the agent's first prompt gets one line naming the
toolbox commands found on `PATH` (`Preinstalled in this VM: …`), plus `Missing: …` when any is
absent; it is not repeated when the agent resumes its own session.

**Limits.** It is one root script with a 600 s cap, and there is no package cache between boots. The
`colony-<preset>` toolbox images are built and published, but the presets still boot the stock
images until the published digests are pinned in `images.lock` — the same step the node preset has
left.

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
It is not an access control. The CLI (`--allow-epic`) and MCP (`allow_epic`) take the same
override (see above). Details are in
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
- **Warm-up when you open the question:** the cockpit can bring the colony back before you answer.
  Opening a suspended colony's question asks the mothership to warm it up
  (`POST /api/sessions/{id}/prewarm`), through the same admission as any boot: never ahead of a
  colony that already holds an answer, and only after queued launches. Your answer then lands in
  the running VM without the cold boot; with no answer within `prewarm_timeout_minutes`
  (default 5) the colony is suspended again and its microVM torn down, freeing the slot, and a
  failed warm-up or a mothership restart reverts to suspended too.

Three Sandbox settings control this. All are mothership-wide, with no per-org override:

| Setting | Default | |
| --- | --- | --- |
| `suspend_waiting` | on | Suspend colonies that wait for an answer. |
| `suspend_after_minutes` | 10 | The grace period, from 1 to 1440 minutes. |
| `prewarm_timeout_minutes` | 5 | How long a warmed colony waits for your answer before it is suspended again. |

**Limits.**

- Only an agent that can resume its own session is suspended. Today that is Claude Code, Codex and
  ACP agents that advertise session loading. Any
  other agent keeps its microVM, and the colony log says so once.
- A colony waiting on an exec-policy approval (an `ask` rule, such as `writes-outside-repo`) is
  never suspended. Its agent — often a subagent — is blocked on the command in flight, and a
  resumed transcript cannot continue that call, so the agent would be lost. The question carries
  `kind: "exec_policy"`, and the colony keeps its microVM and its slot until you answer.

  `writes-outside-repo` now asks only for a write to a host-backed path outside the repository: a
  write to `/root`, `/usr`, a `CARGO_TARGET_DIR` such as `/root/colonizer-target`, or the rest of the
  microVM's own root filesystem — discarded with the VM — no longer asks, while a write to a host
  mount outside the repository, such as `/harness/out` or the agent's transcript directory, still
  does. A write onto a read-only host mount (`/colonizer`, `/opt/colonizer`) or into the checkout's
  own `.git` asks too, and the card says why. An org can restore the older, stricter behaviour — any
  absolute write outside the repository asks, the microVM's root filesystem included — with a policy
  rule whose predicate is `"writes_outside": "strict"`. The `secret-paths` and `script-egress` denies
  are unchanged.

- The same holds for a question a **subagent** asks with `AskUserQuestion`, and for every ACP
  permission request: the tool call that asked is blocked in flight, and suspending the colony
  would kill the agent and leave the answer with nobody to receive it. The question carries
  `blocking: true`. The lead agent's own questions still suspend as usual.
- Such a colony is not held for ever. After **two hours** without an answer it is suspended anyway,
  so it stops holding a microVM and a parallel slot. The colony log says so at `warn`, and says what
  it costs: the agent that asked is lost with the microVM, and your answer, when it comes, reaches
  the lead agent when the colony resumes. The cap is never shorter than `suspend_after_minutes`.
- This is transcript resume, not a memory snapshot. Processes that were running inside the VM,
  such as a dev server, are gone after the resume. Services the colony declares or registers come
  back — see [Services that come back after a resume](#services-that-come-back-after-a-resume).
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
- **Resume does not need GitHub.** The first boot stores the colony's issue in its session
  directory (`issue.json`), and a resume boots on that copy rather than asking GitHub again, so a
  suspended account, a rate limit or a network outage cannot fail it. A colony started before
  issues were stored recovers its issue from its first brief (`vm/session.json`, then its event
  logs). The resume then tries to refresh the base branch in the local mirror; if the remote is
  unreachable or refuses, the colony log says `resumed offline: base not refreshed` and the colony
  carries on with the mirror as it is. With `COLONIZER_NO_EXTERNAL_EFFECTS` set, the refresh is
  not tried at all. Launching a new colony still needs GitHub, and a suspended account is reported
  as suspended.
- **Delete** (the cockpit, or `DELETE /api/sessions/{id}`) removes the colony, its chat and its
  worktree. You can only delete a colony that is not live and not publishing. Its logs go to the
  [log archive](#the-log-archive) first.

The mothership also stops colonies itself: when a budget or the host-disk quota is passed, and
when a microVM dies on its own. In every case the worktree is kept, so Resume continues.

## Services that come back after a resume

A resume boots a fresh microVM: every process inside the old one is gone. A colony declares the
long-lived processes it wants back in `.colonizer/services.toml` at the worktree root, and anything
the agent starts during the run through `colonizer-svc` is recorded too; on a resume the mothership
hands both to the guest, which relaunches them before the agent sees your answer or brief, waits on
each one's readiness, and opens the resumed turn saying what came back. A fresh boot starts
nothing; services come back on a resume only.

> Restored from suspension. Restarted: `web` on :5173 (ready in 3.2 s). Lost: background
> `cargo test`, rerun if needed.

The manifest lists services, one `[[service]]` table each:

```toml
[[service]]
name = "web"
cmd = "npm run dev -- --port 5173"
cwd = "web"             # optional
ready = 5173            # optional: integer port or "http://localhost:5173/" URL string
env = ["VITE_API_URL"]  # optional: names only
timeout_secs = 30       # optional
```

| Field | Type | Required | Meaning |
| --- | --- | --- | --- |
| `name` | string | yes | 1-64 characters of letters, digits, dots, underscores or dashes. Also the record's file name. |
| `cmd` | string | yes | Shell command, run with `sh -c` from the worktree root (or `cwd`). |
| `cwd` | string | no | Directory to run in, relative to the worktree root. Absolute paths and `..` are refused. |
| `ready` | integer or string | no | TCP port or http(s) URL the resume waits on for readiness. |
| `env` | array of strings | no | Environment variable **names** passed through from the colony env. A table like `env = { TOKEN = "abc" }` is refused — values are never stored on this road; pass them through the colony env. |
| `timeout_secs` | integer | no | How long to wait for `ready` before the service counts as failed. Default 60. |

Services started during the run are recorded with `colonizer-svc` — the agentd binary under another
name, linked onto the guest's PATH at boot:

```sh
colonizer-svc start web --ready 5173 --cwd web --env VITE_API_URL -- npm run dev -- --port 5173
colonizer-svc stop web
```

Claude Code background Bash tasks are recorded the same way, as background tasks. They are never
relaunched — the mothership cannot judge whether a finished test run should run again — so a resume
reports them lost, exactly once, and leaves rerunning them to the agent.

The records live in the colony's session directory on the host, mounted into the VM at
`/colonizer/services`, so they survive the suspension like the worktree and transcript do. A resume
waits each service out to readiness or its `timeout_secs`, and reports one that never answers as
not ready, naming the log it wrote to.

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

The reference is in [configuration.md](configuration.md) and
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
3. It runs the checks the diff calls for in fresh, one-shot microVMs, on a `git archive` export
   of the snapshot. Nothing runs on the host, and no result comes from the agent's own logs.
   Each check has a 20-minute limit.

The verdict is `confirmed`, `contradicted`, `inconclusive` or `unverifiable`. Autopilot publishes
on `confirmed`, `inconclusive` and `unverifiable`, and holds the colony on `contradicted`. A claim
is contradicted only if a check fails in a way the base branch does not, or if *none* of the files
the description names are on the branch: when a check fails, the mothership runs the same check
once more on the merge-base, in a fresh checkout of its own. A check that fails there too is
**inconclusive** — the colony did not break it — and autopilot publishes anyway, with a note in
the pull request; only a failure new against the base holds the colony. If some named files are
missing but others are there, that is only an **advisory**. Advisories are shown with the verdict
and added to the pull request as "Verification notes". They never change the verdict.

A failing check leaves its last 200 lines of output in the colony's `out/` directory —
`out/verify-cargo-test.log`, `out/verify-web-npm-test.log`, one log per check — and the failing
test names (up to five, parsed from cargo, vitest or jest output) are quoted in the hold message
and the colony's attention detail.

**Which checks.** The **Publish** module's `verify` setting decides. You can override it for
one colony at launch.

- `auto` (the default) picks the checks from what the diff touches, so a colony is never held for
  code it did not go near. Rust files, `Cargo.toml` or `Cargo.lock` run `cargo test` — a diff with
  no Rust in it skips it. Every other changed file runs the test script of the nearest ancestor
  directory with a `package.json`, by that package's own package manager: the `packageManager`
  field first, then the lockfile (bun, pnpm, yarn or npm), else npm — so `web/**` runs web's own
  vitest, not the root's. Files neither covers run the root Makefile's `test:` target when the
  repository declares one, and nothing when it does not. The checks run one after another, each in
  its own microVM, from the subdirectory they belong to.
- `none` records every claim as unverifiable without checking.
- Any other text is the test command itself, replacing the diff-scoped checks.

**Which check first.** When the diff owes more than one check, the **Publish** module's
`verify_focus` setting decides the order. `shadow` (the default) runs them as above and records, in
the data dir's `jev_focus.jsonl` and the colony's log, which check would have gone first — the one
owning most of the changed files — and whether it would have caught the failure sooner. `act` runs
that check first and stops at its failure. `off` records nothing. A confirmed verdict always needs
every check to pass.

**Limits.** If the colony image lacks the tool a check needs, that check is unverifiable — every
image carries a small toolbox and a repository can add its own with a `.colonizer/setup.sh` hook
(see [Tools in the colony image and the setup hook](#tools-in-the-colony-image-and-the-setup-hook)).
A branch that rewrites the file a check's command is read from (`scripts.test`, the Makefile) cannot grade
its own homework: that check comes back unverifiable and nothing runs. A check whose directory the
branch deleted is skipped rather than run to a meaningless exit 1. A base result is remembered
per repository, image, base commit and check, so a re-verification does not pay for it twice. The
full description is in [architecture.md, Session lifecycle](architecture.md#session-lifecycle), step 5.

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

- Memory is never put into a colony's prompt. The agent asks for it with `memory_briefing` (a short
  summary, each entry with its kind and its source: colony, repository, commit) and
  `memory_changes` (what was added or revoked since it last asked).
- Every entry has a kind: plan, decision, file-change note, failure, architecture note or convention.
- A repository note stays with its repository. A note becomes global (fleet-wide) only when colonies
  in two different repositories propose it with confidence of at least 0.8, and you approve it.
  `GET /api/memory/candidates` lists what is waiting on a second repository.
- To take a note back, revoke it: `POST /api/memory/notes/{id}/revoke?scope=&key=`. Colonies stop
  seeing it at their next briefing, and the mothership keeps a record of what it was and where it
  came from.

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

## Which commits a colony wrote

When a publish pushes, the mothership records each commit the branch carries past its base in
`<data>/sessions/<id>/commits.json`: the full sha, its `git patch-id --stable`, the colony and the
agent session that wrote it. The patch-id names the change rather than the commit, so the link
survives a rebase, a cherry-pick or a message-only amend.

After the branch is rewritten, a reconcile re-points each link whose commit is no longer on the
branch to the one commit there with the same patch-id, keeping the old shas in `previous`. It runs:

- after the watcher's own auto-rebase pushes, and again at every publish;
- when the PR watcher or the merge train reads a head (`headRefOid`) different from the last one
  seen: a live colony's own force-push, GitHub's update-branch, a rewrite from elsewhere. The
  mothership fetches the colony branch into its mirror (the hardened host git) and reconciles
  against it, off the watcher's tick;
- after `sync_repo` fetches a repository, for each colony there with links whose branch tip moved.

Each stamps the tip it reconciled against (`tip` in `commits.json`), so a head seen again, or a
branch that did not move, costs one comparison and no fetch.

The links show in the cockpit's colony pane under **Commits**, and in
`GET /api/sessions/{id}/commits` ([protocol.md](protocol.md)). An orphaned link carries an
**orphaned** badge whose tooltip says why: a squash or rewrite made the match ambiguous, so the link
was kept rather than guessed.

**Limits.** The reconcile never guesses. A squash, an amend that changed the content, or more than
one matching commit leaves the link in place, flagged `orphaned`. A failing git call or fetch
changes nothing, and while a rebase is in progress the reconcile does not run. Merged and closed
colonies are not reconciled after a sync. A redo colony taking over a branch is not a trigger yet.

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

**Cleaning up the archive.** No bundles are removed until something asks for it. Automatic
retention is the **Disk cleanup** loop's **Session archives** category ([loops.md](loops.md#disk-cleanup)):
off by default, and, when switched on, it removes bundles older than 30 days (settable) and — past
an optional size cap — the oldest first.

The Storage panel on the Overview page shows the archive's size and a **Clean up now** form: a
one-off pass run on request. Set "keep N days", "cap at X GB" or both, press Preview to word the
plan, then Apply to run exactly what it listed. Every bundle is the only copy of that colony's
logs, so nothing is removed unless you also check "Allow deleting the only copy". If the archive
changed since the preview, you are asked to preview again. Over the API, this is
`POST /api/archive/retention` with `{keep_days, max_gb, allow_single_copy, dry_run, expect}`, and
`GET /api/archive` lists the bundles.

**Limits.** The archive is local only. `GET /api/storage` reports the archive as
`totals.archive_bytes` alongside the other categories, and its free-disk floor measures free
space on the data disk, so the archive counts against it. You cannot open an archived colony in
the cockpit. Read the bundle with `tar`.

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

**Limits.** A cleaned-up colony is normally unresumable, because resume needs its worktree — but
when the colony's pull request is still open, resume re-creates the worktree from the branch on the
remote instead of refusing (issue #623). Only a colony whose pull request is no longer open, or
whose branch was itself deleted, stays unresumable once cleaned up. Stopped and failed colonies are
never cleaned up automatically, because they can still be resumed. Work that was never pushed is
never deleted: `GET /api/storage` lists it under `unpushed` for you to publish or clean up by hand.
See [protocol.md, Automatic reclamation](protocol.md#automatic-reclamation).

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
[protocol.md §6.1c](protocol.md#61c-jev-second-opinion-shadow-mode)), which is shadow-only unless
`jev_routing_act` lets it pick the tier.

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
