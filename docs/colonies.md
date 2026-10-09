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
- [Recovering on its own](#recovering-on-its-own)
- [GitHub failures](#github-failures)
- [Budgets and plan balance](#budgets-and-plan-balance)
- [What a colony cost](#what-a-colony-cost)
- [Verifying "done"](#verifying-done)
- [Conditional instructions](#conditional-instructions)
- [Shared memory](#shared-memory)
- [Recall from earlier colonies (deja)](#recall-from-earlier-colonies-deja)
- [Operator vault](#operator-vault)
- [Search earlier colonies' conversations](#search-earlier-colonies-conversations)
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
`repo_max_parallel` (default 3), plus an org's own limit if it sets one.

**Queue priority.** Queued colonies start by `(priority desc, created_at asc)`, so with nothing set
it is first in, first out. An org's `queue_priority` (High = 10, Normal = 0, Low = -10 in the
cockpit; any integer through the API) puts all its colonies ahead of or behind other orgs'. A single
queued colony can be moved with **Move to front** (above every other queued colony) or **Move to
back** (below), on its page and in the Overview table; the Nest header names the colony that is
**next up**. Priority only picks who is tried first: the global, org and per-repository limits still
apply, and a colony whose repository is at its cap is skipped for the next one that fits. An org's
optional `max_wait_hours` is a starvation guard: once a colony has queued that long it counts as
High, however low its org or own priority, though never above a colony moved to the front.

**Automatic mode.** With the stack on Automatic and no number in `max_parallel`, there is no fixed
parallel limit. Each colony is sized from the host: the host keeps the larger of 8 GB or a tenth of
its RAM and 2 vCPUs, the colonies share the rest (3 vCPUs and 11 GB on 32 cores and 124 GB; a
`cpus` or `memory` you set still wins). A queued colony starts when the live free memory (`MemAvailable`
on Linux, free plus inactive pages from `vm_stat` on macOS) less that reserve holds its memory and the
1-minute load average leaves room for its vCPUs, re-checked on every queue tick. Below the reserve
nothing new starts and admission resumes when memory frees; running colonies are never stopped for it.
A microVM allocates lazily, so free memory alone would let a burst of colonies in that the host cannot hold once they work. Admission therefore also counts what is committed: the memory sizes of the live colonies times `auto_overcommit` (default 0.75, kept within 0.5 to 1.0), plus the reserve, must fit in RAM, and their vCPUs must stay within 1.5 times the cores. Over the limit, nothing new starts.
`auto_max_parallel` (default 32) is the cap that holds regardless, and a number in `max_parallel`
switches back to a fixed limit. If the host cannot be measured the fixed `max_parallel` applies.
`/api/status` reports it as `sandbox.mode`, `size`, `room_for`, `waiting_reason`, `committed_gb` and `limited_by` (`cap`, `memory-commit`, `cpu-commit`, `free` or `load`).

See
[architecture.md, Session lifecycle](architecture.md#session-lifecycle) for every state a colony
passes through.

**Overrides.** The CLI (`colonizer launch --allow-duplicate`, `--queue-behind-holder`,
`--allow-epic`) and the MCP `launch_colony` tool (`allow_duplicate`, `queue_behind_holder`,
`allow_epic`) take the same overrides the cockpit's checkboxes and `POST /api/sessions` do. Without
one, a launch on a held issue or an epic is refused with a 409 (CLI exit code 5).

## Tools in the colony image and the setup hook

Every colony image carries a small, language-agnostic toolbox a repository's own build or
verification may reach for: `python3` with pip and venv, `python3-yaml` (PyYAML), `jq`, `ripgrep`,
`curl`, `git`, `make`, `unzip` and `ca-certificates`. The node preset's stock `node:24-bookworm`
carries npm, yarn classic and corepack but not bun or pnpm: the colony-node image that adds them is
built but not yet pinned ([#589](https://github.com/Colonizer-dev/harness/issues/589)), so a bun or
pnpm repository's check comes back unverifiable, naming the missing tool, until it is or a
`.colonizer/setup.sh` hook installs it. The image is pinned by digest, so a tool the image does not carry is one a
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

On GitHub a launch marks the issue with the `colonizer:claimed` label, a host label such as
`colonizer:host:omarchy` (the hostname as a slug, one fixed colour per host, created when first
needed), and one claim comment. Filter an org's issues by the host label to see what each machine
is working on.

The claim comment is edited in place, never posted again. It shows the colony id, the host, the
status (queued, running, waiting for an answer, pull request opened, merged, or released with a
short reason), the branch, the pull request link once there is one, and when it was last updated.
It never shows prompts, costs or errors. A retry on the same issue edits the same comment and names
the colony before it. After a restart the mothership finds its comment again by the hidden
`<!-- colonizer:claim … -->` marker, so it never posts a second one.

Status edits are gentle on GitHub: at most one edit per issue every two minutes (a final state is
never held back), no request at all when nothing changed, and a pause from 15 minutes up to four
hours, doubling, whenever GitHub answers with a rate limit, abuse warning or 429. If the token may
not add labels, the claim keeps only the comment and logs a warning.

When the colony releases the issue (failed, stopped or no changes without a pull request, or its
pull request closed unmerged), the comment gets its final state and both labels come off. A merged
pull request keeps both labels as the record of who did the work. When a mothership starts, it
removes marks it left behind for colonies that are gone.

Host labels and status edits are on by default. To turn them off for an org, set
`"claim_updates": false` in that org's settings (`orgs.json`, or `PUT /api/orgs/{org}`). The claim
label and comment stay, since other motherships read them. `COLONIZER_NO_EXTERNAL_EFFECTS` stops
every claim write.

One rule decides every duplicate, whoever launches: the cockpit, `colonizer launch`, the API, MCP,
the loops, burn-down, the red team or a redo. The same answer comes back every way, and a refused
launch in the cockpit shows who holds the work, with a link to that colony or its pull request
(or the host, for another mothership's claim) and the **Allow duplicate** option.

**Limits.** The duplicate check only looks at this mothership's colonies. Other motherships are
seen only through the GitHub labels and comment. If a mothership never comes back, its label stays
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
unless the launch passes `allow_duplicate`. A target is a package and an advisory; a fix the
supply-chain loop started holds every finding it was given, so the Packages tab, `colonizer launch
--package P --advisory A` and the loop refuse the same finding in the same words.

**Limits.** Only this mothership's colonies are compared, and a pull request's file list is capped
at 500 paths. The API shapes are in
[protocol.md, Duplicate-colony prevention and issue claims](protocol.md#duplicate-colony-prevention-and-issue-claims).

## The merge steward: getting a colony's pull request merged

Off by default, per org. In **Settings → Workspaces → (org) → Pull requests**, **Auto-merge** is `off`,
`green` or `green+rebase`, next to the **Merge method** (squash unless you pick otherwise) and
**Delete branch** (off; a branch another colony's pull request is stacked on is always kept). The
steward only looks at pull requests this mothership's colonies opened, and never calls GitHub for an
org that has not opted in.

Every five minutes it reads an org's pull requests with **one GraphQL query** and decides each one
from what GitHub says. The decision is a pure function, `merge_steward::decide`:

| What it finds | What it does |
|---|---|
| Green, GitHub says `CLEAN`, not a draft, no `hold` / `do-not-merge` / `needs-human` label, no open question on the colony | Merges, pinned to the head it read (`--match-head-commit`). A merge queue is asked for GitHub's auto-merge instead. At most one per repository per cycle. |
| Behind or conflicting, in `green+rebase` | GitHub's **update-branch** first. If that conflicts, and the watcher's own rebase has flagged the colony (`needs_rebase`), the colony is resumed with a rebase task. |
| A real failing check | The colony is resumed with the failing job's name and the tail of its log. Two rounds at most, and never twice for the same head; then the pull request is marked **needs attention**. |
| Every failed job ended in under 10 s with no steps, or GitHub's annotation names billing or a spending limit | Marked **ci blocked**. No colony is spent on it, and one banner per org says GitHub Actions is blocked there. |
| It adds a `changelog.d/` fragment while a `release: vX.Y.Z` pull request is open in the repository | Waits: the [release train](release.md#the-freeze)'s changelog check would fail on a fragment that lands after the release assembled its own. |
| Anything else (running or missing checks, `BLOCKED`, a requested change, a fork) | Waits, and says why. |

Branch protection is never second-guessed: only `mergeStateStatus: CLEAN` merges, so a pending or
failing required check, a missing review or a merge queue is GitHub's to settle. The steward waits
out an open GitHub circuit breaker ([GitHub failures](#github-failures)), and leaves a repository the
publish module's merge train or the merge-train loop drives to them.

The cockpit lists each pull request with its state (waiting, merging, rebasing, fixing, ci blocked,
needs attention) and a **Merge now** button, which asks GitHub again and merges only a pull request
GitHub itself calls mergeable. `GET /api/merge-steward` serves the list; `/api/status` carries the
blocked orgs as `merge_steward.ci_blocked`. A pull request needs at least one check to report before
the steward merges it.

A blocked org is not silent: the steward announces one newly blocked org once, through the notify
settings (desktop, webhook or Web Push), and keeps rebasing its pull requests in `green+rebase` — a
billing block is not an excuse to let a branch go stale. An org that wants to keep merging through
the block can opt into **Verify locally when CI is blocked** (`verify_locally_when_ci_blocked`): the
steward then runs the repository's `.colonizer/merge.toml` merge gates in a build VM, one pull
request at a time, comments the result on the pull request, and merges on a pass. A billing block is
still never treated as green, and a repository that declares no gates just waits.

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

## A repo can opt out

A maintainer can say their repository does not want colonies. A launch on such a repo is refused
with a 409, and the message names which opt-out was found. Any one of these signals is enough:

- a `.colonizer-ignore` file at the repo's root — its presence alone is the signal;
- a `colonizer: ignore` label (any case) on the issue being launched against;
- `enabled = false` under a `[colonizer]` table in `.colonizer/config.toml` at the repo's root.

**How to override.** The repo's own owner (the `owner` of `owner/repo`, in any case) can always
launch there: the opt-out is checked against the signed-in GitHub user, and the owner's launches are
let through. Nobody else can, and there is no request flag for it.

**Limits.** If the GitHub lookup fails, the launch goes ahead. The opt-out is a guard that respects
a maintainer's wishes. It is not an access control. Like the epic guard, it is best effort: a
missing file or label is simply no signal, and only a config file that exists but cannot be read is
logged and ignored.

## A repo's daily PR cap

Colonies open pull requests, and some repositories would rather be handed a few a day than be
flooded. You can cap how many pull requests colonies open in a repo per UTC day. **The cap is off
unless you set one**: nothing is parked by default.

**How to set it.** Install-wide, set **Pull requests per repository per day** (`max_prs_per_day`)
in the publish settings; it applies to every repo. A repo can set its own with `max_prs_per_day`
under the `[colonizer]` table in `.colonizer/config.toml` at its root, next to the `enabled` key
above, and that wins over the install-wide setting:

```toml
[colonizer]
max_prs_per_day = 12   # this repo's own cap
```

`0` means uncapped, in either place: it is the default, and a repo can set it to opt out of an
install-wide cap. Like the opt-out above, the repo lookup is best effort: if the config file cannot
be read, the install-wide setting applies.

**What a cap does.** The cap is checked when a pull request is about to be opened, not at launch, so
a colony that runs out of room is the only thing delayed. The Create PR press and autopilot's
verdict both go through the same check, so neither can outrun it, and a publish already in flight
counts toward the cap — so two presses at once cannot both slip one past it. A colony that hits the
cap is **parked** with the reason `repo_pr_rate_limit`: it keeps its worktree, its branch and its
work, the cockpit shows the park and when it resumes, and the queue requeues it on its own once the
UTC day rolls over — nothing to press. It resumes sooner if the cap stops holding it back: set the
cap to `0`, or raise it past what the repo has opened today, and the queue requeues as many parked
colonies as the new room allows within a few minutes. A park releases the colony's slot and tears
its microVM down, as every park does, unless your org sets `discard_vm = false` (or its worktree
cannot be verified, in which case the machine is kept for the work in it).

## Exec policy: install, org and repo

The exec policy is rules about the shell commands a colony's agent runs: deny, ask you, or allow
(the format and the built-in rules are in
[`modules/agents/claude-code/README.md`](../modules/agents/claude-code/README.md#exec-policy)). A
colony layers up to three policies over the built-in one, in this order:

1. **install** — the agent module's `exec_policy` setting in Settings → Modules;
2. **org** — the Exec policy box in the org's workspace settings (issue #924), stored as
   `exec_policy` in `orgs.json` and handed to the org's colonies as `COLONIZER_EXEC_POLICY_ORG`;
3. **repo** — the repository's own `.colonizer/exec-policy.json`.

Across layers the strictest decision wins, so each layer can only narrow the ones before it: an org
`deny` beats an install `allow` of the same command, and an org `allow` cannot undo an install
`deny`.

Only the owner can change an org's policy. The save is refused, with the reason under the box, unless
the runner would keep every rule of it: valid JSON of at most 64 KiB, an object with a `rules` array,
and every rule a `deny`, `ask` or `allow` decision with at least one usable `command`, `script`,
`touches` or `writes_outside`. Leave the box empty for no org layer.

**Limits.** Claude Code and ACP apply the policy; Codex, Grok Build, Hermes, OpenCode and Pi do not.
While any layer is set, a colony on one of those refuses to launch and names where the policy came
from — the org by name — rather than run without it. The org's policy reaches colonies launched
after the save; a running colony keeps the policy it booted with. A pattern's regex syntax is checked
only by the runner, which drops a rule it cannot compile.

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

**Full autonomy (YOLO).** The second autonomy choice is the judge with no answer limit. It takes
the same settings — `model`, `fallback_models`, `after_minutes` (default 1), `free_text` and
`risk_ceiling` — but has no `max_answers`, so every `ask` question at or below the ceiling is
answered for as long as the colony runs, however many that is. Everything that bounds the judge
still bounds this: it answers only `ask` questions, only among the options the agent offered,
never overrides a deny, and leaves anything above the ceiling for you. The sandbox, egress and
path policies hold unchanged, and every answer is logged as the judge's. It needs a model from a
provider under **Model providers** — the judge cannot use your Claude login — and saving a ceiling
above `workspace_write` asks you to confirm once in the cockpit. If three judged calls in a row
cannot reach the model, the colony still falls back to waiting for a person.

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

A Watchdog setting, `idle_park_minutes` (default 15, 1 to 1440), parks a colony sooner when it is
doing nothing. A colony that is idle, held by autopilot or flagged by the watchdog, with no open
question and no publish or verification in flight, is parked once it has sat that long: its microVM
stops and its slot is freed, the worktree and branch are kept, and Resume brings it back. A colony
with an open question keeps the behaviour above. When the colony is idle because its last turn did
not write or update `/harness/out/pr.md` (for example after a redacted description was held), it
first gets one automatic message asking it to rewrite the description; if that does not help, it
parks like any other.

## Stacked colonies that wait

A colony stacked on another (`after`) builds on that colony's branch, and one failure used to take
the whole chain with it. Now a dependent whose parent is stopped or parked goes to `blocked`
instead: it holds no slot and no microVM and is not failed, and its card says what it waits on
("waiting on #5 (`c8a6d23c`, stopped)"). It is `queued` again when the parent resumes or finishes,
and waits for the parent's branch as before. When the parent is gone for good (failed with no
parent of its own, cleaned up before it published, a closed pull request, no changes, or deleted)
the dependent re-bases on the default branch and queues like any other colony; a conflict is then
handled by the normal rebase path. A cascade never produces `failed`. A blocked colony can be
stopped (Leave the queue) or deleted like a queued one.

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

## Recovering on its own

Some failures have no work behind them: a provider that blipped, a status that fell out of step, a
parent that paused, a sign-in that expired. The mothership handles these itself rather than
handing each one to you as a colony that needs you.

- **A transient provider error is retried, not held** (issues #980, #1093). When an autopilot
  colony's turn ends with an error the retry classifier calls transient — a gateway 5xx, 429 or 529,
  an "unreachable" or overloaded provider, a timeout, a dropped, reset or refused connection
  (`UND_ERR_SOCKET`, `ECONNRESET`, the router's "the connection to Anthropic failed"), a gateway
  restarting — the colony is parked with reason `provider_retry` (worktree kept, parallel slot
  released) and continued automatically after 1, 5, then 15 minutes. Each attempt is a line in the
  colony log. While a retry is pending the colony is not "waiting on you": its card reads "Stopped on
  a model gateway error (502, connection to Anthropic): retrying in 4 min", with **Retry now**. Only
  when the attempts run out is the colony held as `autopilot_held`, and the card then reads "Stopped
  on repeated gateway errors", with **Retry** (which sends the agent on again) and **Open colony**. A
  turn that ends cleanly resets the count. Two Watchdog settings shape it:
  `provider_retry_max_attempts` (default 3, at most 10, `0` turns the retry off) and
  `provider_retry_schedule_minutes` (default `1, 5, 15`; retries past the end of the list wait its
  last entry). An error a retry cannot fix — a refused sign-in, permission or policy — still holds
  at once, and the card names that error instead of saying the watchdog flagged the colony.
- **A turn the mothership's own restart cut off is continued once** (issue #1093). A colony that
  keeps running through a mothership restart loses its gateway socket, so its turn can end on a
  gateway error while the mothership is down. When the restarted mothership reconnects, the first
  turn end within 15 minutes that is such an error is continued once, straight away, without
  parking the colony or spending a retry; the colony log says so. A later failure takes the
  ordinary retry path.
- **A stale "waiting for an answer" is reconciled** (issue #981). On every watchdog tick, a colony
  whose status is `waiting_for_answer` but which has no question actually pending is set back to
  `idle`, and the colony log says why. This runs even with the Watchdog module off. An answer to one
  question also no longer closes a different one still open (a subagent's exec-policy `ask`, say),
  so `colonizer ask` and `colonizer answer` see what the cockpit shows.
- **A stacked colony waits for its parent instead of failing** (issue #982). A colony launched with
  `after` waits while its parent is `stopped` or `parked`, since the parent may still be resumed.
  When the parent fails, the child moves onto the parent's own parent and keeps waiting there; a
  failed parent with nothing above it leaves the child queued. Only a parent whose record is gone,
  or one that made no changes, still retires the child. A new `after` launch onto a parent that has
  already failed is refused, as before.
- **Claude accounts are health-checked** (issue #983). Every five minutes the mothership makes a
  cheap check against each configured Claude account. **Settings → Connections** shows the result
  on the Claude card — reachable, token rejected, or unreachable — with when it last ran, and
  `GET /api/claude-accounts` carries each account's `health_status` and `health_checked_at`. An API
  key account is not called and always reads as reachable.
- **An expired sign-in holds its colonies and tells you once** (issue #984). When Anthropic answers
  a subscription account with 401 or 403, the account is marked and every colony routed to it is
  parked with reason `waiting_for_account`. The colonies release their slots, use none of their
  provider retries and do not land in "Needs you" one by one. You get one notification per account
  and change of state ("Claude account `default` needs you to sign in again. N colonies are waiting
  on it.", then a resolved message). The cockpit shows a banner with a **Sign in** button,
  `colonizer list` prints a warning line, and `GET /api/status` carries `account_alerts`. Once you
  sign in again — noticed from the credential file's timestamp, never its contents — the colonies
  resume through the normal queue. A usage limit (429) is not an expired sign-in: it keeps the quota
  pause described in [providers.md](protocol/providers.md).

The marks for a broken account are kept in memory. After a mothership restart, waiting colonies
resume, and if the account is still broken it is marked and announced again, through the same
notification rate limits as every other alert. A keychain-held credential has no file timestamp,
so a re-sign-in there is not noticed until the mothership restarts.

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

## GitHub failures

Every call the mothership makes to GitHub — `gh` and the network half of `git` (fetch, push,
`ls-remote`, clone) — goes through one circuit breaker per GitHub identity (the saved token, the
`GH_TOKEN`/`GITHUB_TOKEN` environment, or the `gh` CLI login). A failed call is classified as one of:

| Class | What GitHub said | Effect |
|---|---|---|
| Suspended | a 403 whose body says the account is suspended | opens the breaker |
| Token revoked | a 401, "Bad credentials", git's refused token | opens the breaker |
| Missing scope | the refusal names a scope the token lacks | that action fails, with what to add |
| Secondary rate limit | a 403/429 naming a secondary limit, or carrying `Retry-After` | opens the breaker at the third within 10 minutes |
| Transient | a 5xx, a 429, the primary rate limit, a network failure | retried where the caller retries |
| Other | a 404, a validation error, a permission on one repository | that action fails |

While the breaker is open:

- **Nothing calls GitHub.** A `gh` or network `git` command fails at once with the cause instead
  of reaching GitHub, so no call is spent against a refused account.
- **Launches wait.** Queued colonies stay queued, in order; nothing is re-dispatched in a loop.
- **Publishes are held.** Running colonies keep working locally. An autopilot publish is held, as
  `COLONIZER_NO_EXTERNAL_EFFECTS` holds it, and a Create PR click is refused with the cause.
- **Merges, claim and comment writes and the decisions inbox skip their ticks.**
- **One banner** above every cockpit view names the cause and the next step: "GitHub account
  suspended: contact GitHub support", "Token revoked: reconnect GitHub in Settings → Connections",
  or a secondary limit to wait out. `GET /api/github/status` (and `github_pause` in
  `GET /api/status`) carries the same: `paused`, `cause`, `message`, `next_step`, `since`,
  `next_probe_at`, `detail`, `queued`, `held_publishes` and `refused_calls`.

A slow probe makes the only call: one `GET /user` every 30 minutes for a suspension or a revoked
token, and for secondary limits after 5 minutes, doubling up to 30. When it succeeds the breaker
closes, the queue moves on its next tick, and the held publishes go out one at a time, each verified
first. Reconnecting GitHub with another token is a new identity, so the breaker closes at once.
The breaker lives in memory: after a restart, the first refused call opens it again.

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

## Self-healing: the watchdog playbook

The watchdog used to send a generic "no progress" nudge, and a person (or an outside agent polling
`harness.jsonl`) did the rest: recognise a known stall, send the colony the exact fix, publish,
switch model. The playbook is that step done by the mothership. It is a table of **signature,
action, tries**; each fix is logged as `auto-fixed: <signature>` in the colony's `harness.jsonl`,
listed on the session as `auto_fixes`, and shown in the colony header ("auto-fixed: pr_md_write").

| Signature | Matches | Action | Tries |
|---|---|---|---|
| `placeholder_dotfiles` | a `secret-paths` denial whose target is a harness placeholder (`.env`, `.netrc`, ...) in the worktree and names no real credential path | message: the placeholders are harness mounts, leave them, continue the issue | 1 |
| `pr_md_write` | a `writes-outside-repo` denial on `/harness/out/pr.md` | message: write `pr.md` with the file tool | 1 |
| `toolchain_installer` | a `script-egress` denial on a toolchain installer (rustup, swift, ghcup, ...) | message: no toolchains, say in `pr.md` what was not compiled | 1 |
| `provider_unavailable` | a turn-error hold on `unrecognized_model`, or a provider quota flag, where the provider's `fallback_model` is a `<provider>/<model>` on a configured provider that is not itself out of quota | `switch_fallback_and_resume` | 2 |
| `idle_verified` | idle, autopilot on, `pr.md` written, a confirmed verification of the tree as it stands now, nothing flagged, quiet for 10 minutes | `publish` | 1 |

The same denial coming back after its tries are spent (the colony is looping) runs `stop_looping`:
the colony is stopped, its slot is freed, and it carries the attention reason `looping` naming the
signature. Between a fix and the next action on the same signature the playbook waits
`settle_secs` (120 by default) for the agent to read the message. A person's resume gives a
stopped colony fresh tries.

Rows handled by their own mechanism are not repeated here: a contradicted verification is sent back
to the agent in fix rounds, a `pr.md` that only redaction changed is published redacted, and an
open question is closed by the judge.

**The playbook never releases a security hold.** A colony carrying a control-defeat flag, or any
attention reason naming a secret, a redaction or a defeat, is left alone, and a denial that
completes a control-defeat signature is flagged as before and not answered with a message. The
publish action refuses when the tree is not the one that was verified, when the kill-switch is up,
or when GitHub's breaker is open.

### Adding a pattern: `playbook.toml`

The table is data. The compiled-in rows are the defaults; `<config dir>/playbook.toml` (next to
`updates.json`) adds rows, replaces a default by naming the same `signature`, or turns one off. It
is read on each use, so a new pattern needs no release and no restart. A file that does not parse
is ignored with a line in the mothership's output.

```toml
# replace = true            # start from an empty table instead of the defaults

[[entry]]
signature = "npm_registry_denied"       # what the cockpit shows as "auto-fixed: ..."
trigger = "denial"                      # denial (default) | provider_failure | idle_verified
action = "send_message"                 # send_message | publish | switch_fallback_and_resume | stop_looping
message = "The npm registry is not reachable from here; use the vendored packages."
max_tries = 2                           # fixes per colony before the signature counts as looping
settle_secs = 120                       # wait this long after a fix before acting again
stop_when_exhausted = true              # stop the colony (attention `looping`) when it comes back
# enabled = false                       # switch a default off by repeating its signature

[entry.when]
kind = "egress_denied"                  # the boundary kind; exec_policy_deny when absent
control = "egress"                      # substring of the control
text_any = ["registry.npmjs.org"]       # one of these in the denial's detail or target
text_none = []                          # none of these
target_in = []                          # target must be a relative path with one of these file names
# error_any = ["unrecognized_model"]    # provider_failure: words in the hold's detail
# quota = true                          # provider_failure: a quota flag matches too
```

A stall that no row matches goes to `playbook::on_unmatched_stall`, which does nothing today; it is
the hook for an operator agent (#1192).

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

## Operator vault

Stage a filtered, secret-scrubbed snapshot of a local Markdown vault (an Obsidian vault, say)
read-only into each colony at `/colonizer/vault/`, with an `INDEX.md`. Off by default; the
mothership only ever reads the vault. Point it at the vault in `colonizer.toml` and allowlist the
folders to stage, each scoped like
[colony secrets](protocol/secrets.md#colony-secrets-post-apisecretscolony) — `all`, one `org` or one
`repo`:

```toml
[vault]
path = "/home/me/Obsidian/Work"
[[vault.folders]]
path = "Projects/web"
scope = { kind = "repo", repo = "acme/web" }
[[vault.folders]]
path = "Decisions"
scope = { kind = "all" }
```

A folder path is relative to the vault; `""` or `"."` is the whole vault. An absolute path, one with
a `..` component, or one reached through a symlink is skipped with a warning. Only `*.md` files are
staged: dot-named files and directories (`.obsidian/`, `.trash/`, `.git/`) and attachments stay out,
a note whose frontmatter says `colonizer: false` is left out, and symlinks are never followed. One
note over 256 KiB is skipped and the snapshot stops adding notes at 8 MiB. Every note — and its path
— is scrubbed of the secret values the mothership knows, the same `deja` scrub as the transcript
index, before anything is written, and the `INDEX.md` (titles, paths, tags, status, `[[links]]` and
backlinks) is built from the scrubbed text. The snapshot is taken at each boot and resume, so a later
edit needs a new boot to reach a colony; a folder out of scope stages nothing, so `/colonizer/vault/`
is absent. Base64- or percent-encoded forms of a secret are not caught.

**Searching it.** A colony with a staged vault gets a `vault_search(query, limit?)` tool beside the
memory tools (Claude Code, Codex, Grok Build, OpenCode, Pi and ACP; Hermes serves neither). It is a
plain-text search of the snapshot like `memory_search`: every term must appear in a note, case
aside, and matches are ranked by how often the terms appear, a hit in the title or a heading
counting more. Each match names its path under `/colonizer/vault/`, the line and the heading it sits
under, and a short excerpt, wrapped in an `<operator-vault>` frame that calls it data to verify, not
instructions. The snapshot holds only the folders in scope for the colony, so the search never
reaches anything else; `INDEX.md`, dot-named entries and symlinks are left out of it.

**Proposing a note.** `vault_propose(path, title, body, reason)` (Claude Code, Codex, Grok Build
and OpenCode; Pi and ACP have no channel back to the mothership) never writes inside the colony or
the vault: it sends a `vault_proposal` event, and the mothership queues it for review with its
provenance — the colony, its repository and the commit its worktree was at. Only the orchestrator
proposes: a subagent's call is refused in the colony and again on the mothership. A proposal from
a colony no vault folder reaches is ignored, the secret values the mothership knows are scrubbed
from it, and the path must be relative (at most four parts and 200 characters, no `..` or
dot-named part; `.md` is added); the title is capped at 200 characters, the reason at 2,000 and the
body at 64 KiB, and at most 200 proposals wait at once. The cockpit lists them under **Proposed for
your vault** on the Memory page, as plain escaped text. **Accept** writes the note as a new file
under the vault's inbox folder — `Inbox/colonizer/` unless `colonizer.toml` says otherwise — with
the provenance in its frontmatter; it never overwrites a file, never follows a symlink and never
writes outside the vault root, and a proposal it cannot write stays in the queue. **Reject** drops
it. Notes you accept reach colonies at their next boot only if the inbox sits in an allowlisted
folder.

```toml
[vault]
path = "/home/me/Obsidian/Work"
inbox = "Inbox/colonizer"   # relative to the vault; this is the default
```

## Search earlier colonies' conversations

**What it is.** Where deja recalls a finished colony's transcript, this searches every colony's
conversation — your prompts and the agents' replies — at once, to find how an earlier colony met a
problem before. A colony has the same reach, read-only, through the `colony_history_search` tool
(Claude Code only so far, issue #739): it asks its own gateway, which scopes the answer server-side
to same-org colonies — or, when it has no org, to org-less colonies of its own repository — and never
to itself. Every query word must appear in a message, any case; a search returns at most 50 hits, 20
by default, and no one colony may contribute more than three, newest colony first.

**Turning it on.** Always on with memory: the mothership hands the colony the gateway URL and its
token whenever memory is enabled for it. Nothing is indexed and no model is called — a search just
reads conversation logs, so it costs nothing to leave on.

**In the cockpit.** The panel at the top of History takes a query and filters by repo, org, agent,
status and a date range. Each hit names its colony, role and turn; clicking one opens that colony
and scrolls to and highlights the turn. It is backed by
`GET /api/history/search?q=&repo=&org=&agent=&status=&since=&until=&limit=`, owner-only, which
answers `{hits: [...]}` newest colony first. An empty `q`, or a `since`/`until` that is not a date,
is a 400.

**Limits.** This is a plain scan, not an index (a normalised transcript index is issue #736): each
colony's `events.jsonl` is read in turn, from its tail — at most the last 8 MiB of each log and 64 MiB
across one request, skipping any line over 256 KiB — so it is slower on a large data directory and
finds only messages that carry their text inline. Every snippet is redacted again on the way out. Over
the gateway a colony whose sensitivity is `restricted`, missing or unrecognised is visible only to a
`restricted` caller, and one org's log never answers another org's query. It searches conversations,
not code, issues or notes.

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
