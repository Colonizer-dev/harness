# Loops

A loop is a saved prompt on a repository that launches a colony on a schedule — the mothership's
version of Claude Code's `/loop`. One loop is built in: [Disk cleanup](#disk-cleanup), off until you
switch it on. Use one for work that recurs: triaging new issues every morning,
keeping dependencies current every week, fixing last night's flaky tests, or writing yesterday's
changelog entries (as `changelog.d/` fragments in a repository that keeps them, like this one).

## Making one

- **Loops** in the sidebar → **New loop**: choose what each run does (a colony from a prompt, or
  refresh the architecture map), pick a repository, write the prompt (or start from a template:
  triage new issues, keep dependencies current, fix last night's flaky tests, write changelog
  entries), choose when, and optionally a model, a subagent model, autopilot (on by default) and a
  run limit.
- **Colonize (⌘K) or the composer**: `/loop 1h check CI on main and fix flakes` creates a loop on
  the repository picked there that runs every hour. The interval takes `m`, `h` or `d`. `/loop 14d …`
  runs every 14 days at the current time of day: a whole-day interval past a week becomes an
  every-N-days cadence (up to 365 days). `/loop <task>` without an interval is self-paced.
- **The CLI**: `colonizer loop create acme/app --name "Triage" --prompt "Triage new issues"
  daily@09:00`, with `loop list`, `run`, `stop`, `start` and `delete` beside it — the grammar is in
  [cli.md](cli.md#loops).
- **The Map view** offers a map-refresh loop once a repository has a map ([Map refresh](#map-refresh)).

An optional **Ends** date and time in the loop form (your local time, stored as `end_at`) stops the
loop when its next run would fall past it; a value that is not after now is refused in the form. The
Loops list shows the day a dated loop ends.

## When it runs

| Cadence | Runs |
|---|---|
| Every N minutes | N minutes after the previous firing (15 minutes to 7 days) |
| Daily | every day at a time you pick, in your local time |
| Weekly | on a weekday at a time |
| Monthly | on a day of the month at a time; days 29–31 fire on a shorter month's last day |
| Every N days | every N days (1–365) at a time you pick; a run that fires a little late does not push the next one later |
| Self-paced | each run names the next with `loop_next` (15 minutes to 24 hours); without one, 24 hours later |

Times are stored in UTC; the cockpit converts your local choice when you save. The scheduler checks
for due loops once a minute.

**One run at a time.** A tick that finds the loop's previous run still live — queued, starting,
working, idle, waiting for an answer (suspended included), or publishing — skips, and the loop's note says so. A fixed loop tries again
at its next slot; a self-paced one 15 minutes later. **Run now** starts a run immediately, and is
refused (409) while the previous one is live.

## What the colony can do

Every run is an ordinary colony (its origin is `loop:<id>`, and colony lists badge it ↻ loop). It
goes through the same admission path as any launch: parallel limits, budgets and a switched-off
workspace apply. A run that cannot start says why in the loop's note and tries again at the next
slot. The colony gets the loop's prompt plus a note saying which run it is, and two tools only the
orchestrator may call:

- `loop_next(delay_minutes, reason)` — self-paced loops only: when the next run starts, and why.
  The value is clamped to 15 minutes – 24 hours and shown in the loop's note.
- `loop_stop(reason)` — ends the loop ("stopped by the colony: …"). Re-enable it on the Loops page.

The Claude Code, Codex, Grok Build and OpenCode agent modules serve these two tools. A loop whose
colonies run on a module without them — Pi, Hermes and ACP today — still runs on its schedule, but
its brief never mentions the tools, the loop form warns when you pick self-paced, and a self-paced
one simply runs again every 24 hours: its colonies can neither pace the loop nor stop it.

A loop also ends by itself after its **max runs**, when its next run would fall past its **end
date**, or — at its next slot — when its API token has been revoked.

## Tokens

A loop can be created by a scoped API token as well as by the owner ([Scoped API
tokens](cli.md#scoped-api-tokens)): the loop records the token, and every run is admitted against
the token's org/repo limits, concurrency cap and daily budget and marked as external input,
exactly like a colony the token launched by hand. Revoking the token ends the loop the next time
it would run (a run-now answers 409), so nothing launches after revocation. The token lists every
loop inside its limits, but edits and runs only the loops it created — an owner's loop reads as
unknown to it — and map loops stay with the owner.

## Map refresh

A map loop keeps architecture maps fresh instead of running a prompt: each firing launches the same
mapping colony as the Map view — same instructions, same `archify` skillset, reuse of a colony
already drawing — so its runs are the map, exactly as if it had been drawn by hand. Its colonies
carry the origin `map:loop:<id>`. Its prompt is not used and may be left empty. Choose a scope:

- **This repository** — maps the one repository on its cadence.
- **All repositories in the org** (`owner/*`) — maps them one at a time, ten minutes apart, and
  re-lists the org at the start of every cycle, so repositories added later are included. The next
  repository starts even while the previous one is still drawing.

Launches go through the ordinary admission path, so parallel limits, org budgets and the archify
skillset rule the loop in exactly as they rule the Map view. A firing that is refused says so in the
loop's note and tries again at its next slot; org-wide, one repository's failure is noted and the
cycle moves on to the next — and a workspace that is switched off altogether skips its whole cycle
with that single note rather than one per repository. When a refresh ends, History records it as
`map.refresh` — and a repository whose refresh failed keeps its old map.

**Keep this map up to date?** — the Map view asks this once a repository has a map and no enabled
map loop covers it. The answer defaults to every 14 days, with presets 7/14/30/60/90 or a custom
number of days (up to 365), for this repository or all repositories in its org; the loop runs at
03:00 your local time. **Not now** is remembered in that browser for 30 days, and the map bar's
**Keep fresh…** link asks again. Once a refresh loop covers the repository, the map shows
"Refreshed every N days · edit". Map loops can also be made from the Loops page.

## Merge train

The Loops page opens with one built-in loop, **Merge train** (issue #754): the careful way to run
the [merge train](architecture.md#merge-train) — merge colony pull requests, and rebase them when
needed — on a schedule instead of the train's own two-minute tick. It is **off by default**, runs
**hourly** once switched on (any interval from 15 minutes), and merges **only in repositories you
opt in**: the allowlist (`owner/repo`, or `owner` for a whole org) starts empty. A `never` list —
an upstream-review-only fork, say — beats the allowlist, and so does the train's own
`merge_train_deny_orgs`. While the loop drives a repository, the train's two-minute tick leaves it
alone.

Each run follows the rules an operator would follow by hand:

1. **Keep main green.** Before any merge, the default branch's latest CI on its tip must be
   completed and successful. A run still going means wait; a cancelled run — re-queued or not — is
   a wait too, never red. A main that is not green merges nothing.
2. **Merge only on fresh CI.** A pull request merges only when it is behind its base by 0 and every
   check on that exact head is green. GitHub's CLEAN is not enough: it can reflect CI that ran on an
   older base.
3. **One at a time.** After a merge, the next candidate is updated onto the new base
   (`update-branch`, once per run) and the run waits up to `ci_wait_minutes` (default 20) for its
   CI and main's before re-checking. At most `max_merges` merges per repository per run (default 4,
   overridable per repository), at least `cooldown_secs` (default 120) between two merges in a
   repository, even across runs. Every GitHub call is paced (`min_call_gap_ms`, default 1 s) and
   budgeted (`max_api_calls`, default 400 per run); any 403 or 429, abuse-detection or
   secondary-rate-limit answer **stops the run** there and then, and nothing is retried — the rest
   waits for the next run.
4. **Eligibility.** Only colony pull requests (`colonizer/*` branches the mothership published);
   never drafts, never a HOLD, WIP or do-not-merge label or title, never a colony held with
   `loop merge-train hold`, never a colony superseded by its redo or by a newer pull request on the
   same issue; and the train's own author and attribution guards apply.
5. **Rebase only when mechanical.** A conflicted (DIRTY) pull request gets the host's mechanical
   rebase, and then needs fresh CI before it can merge. If that rebase conflicts, nothing is guessed:
   the pull request is marked `needs_redo`, and — only with `redo_on_conflict` on — one redo colony
   is dispatched for it, ever, with the pull request as its reference (`git fetch origin
   pull/N/head`, `allow_duplicate`). The redo supersedes the original.
6. **Self-heal main — off by default.** When main goes red and its tip is the train's own merge, the
   repository is **paused** until main is green again. With `self_heal` on, the failed jobs are
   re-run once (a flake), and if main is still red on the next run a small fix colony is sent with
   the failing job's log and instructions to fix main minimally. `revert_on_red` sends a colony that
   reverts the train's own last merge instead — never anything else. A red main whose tip is not the
   train's merge is left to people.
7. **Red pull request CI** is re-run once when every failing check is on `flaky_checks` (a trailing
   `*` matches a prefix); otherwise it is left red and reported.
8. **No attribution.** Merges are squashes titled `<pull request title> (#N)`, pinned to the head
   that was read; a pull request whose commits carry AI attribution is refused.
9. **A report every run** — merged, updated (CI running), red (why), redo dispatched, skipped (why),
   plus what was done about a red main — kept in the loop's history (the last 20 runs), written to
   the activity log (one `publish.merge_train` line per repository) and to each colony's own log.
   The card shows the last report.

**Dry run** reads everything and writes nothing: it lists what the loop would merge, update, rebase
and skip, and why. With `COLONIZER_NO_EXTERNAL_EFFECTS` set, every run — scheduled or not — is a
dry run, and its report says so.

From a terminal:

```sh
colonizer loop merge-train show                    # settings, next run, paused repositories, last report
colonizer loop merge-train allow acme/web          # opt a repository in (or `allow acme` for the org)
colonizer loop merge-train never acme/upstream     # never merge here
colonizer loop merge-train set --every 120 --max-merges 2 --repo-cap acme/web=1 --flaky 'e2e*,lint'
colonizer loop merge-train set --self-heal on --redo on
colonizer loop merge-train run --dry-run           # what it would do, and why
colonizer loop merge-train on                      # switch it on; `off` switches it off
```

## Disk cleanup

Rust and Node builds fill disks; a full one has stopped the mothership before. **Disk cleanup** is a
loop every install has, off until you switch it on. It launches no colony: each run is the
mothership's own housekeeping, so it costs no model spend.

- **Switch it on**: Loops → **Disk cleanup** (its own row above your loops) → the switch. The first
  time, the switch opens the **preview** — what a run would remove right now, path by path with
  sizes, and what it keeps and why — and **Turn on disk cleanup** there enables it. From a terminal:
  `colonizer loop run disk-cleanup --dry-run`, then `colonizer loop enable disk-cleanup` (and
  `colonizer loop disable disk-cleanup`).
- **When it runs**: every hour by default, on any interval the scheduler allows (15 minutes up), and
  **early when free space** on the data dir's volume **is under 15%** (0 turns that off). The
  free-space check runs at most every five minutes, and an early run at most every 15 minutes. Run
  now starts one at once; one run at a time.

What it may clean, each with its own switch in **Settings**:

| Category | Default | What goes |
|---|---|---|
| Build output | on | git-ignored `target/`, `node_modules/`, `.next/` and `dist/` inside the worktrees of finished colonies: merged, closed, nothing to change, or stopped/failed for 7 days (settable) — not while a pull request is still open. The colony stays resumable; a build just starts cold. |
| Worktrees | on | the automatic reclaim's own candidates: pushed colonies past the reclaim retention (`COLONIZER_RECLAIM_RETENTION_HOURS`, 12 h), never keep-worktree, never unpushed work |
| MicroVMs | on | stopped `colonizer-*` microVMs no colony owns. Images are kept: `msb` has no prune that can tell which images a colony still needs |
| Session archives | off | archive bundles older than 30 days (settable), plus oldest-first past an optional size cap — the archive's retention plan, which removes the only copy |
| Host build dirs | off | Cargo `target/` dirs (beside a `Cargo.toml`, carrying Cargo's `CACHEDIR.TAG`) under directories you list, untouched for 3 days — for people who build on the mothership's host |

**Never**: a queued, live, waiting, publishing or parked colony; a colony marked **Keep worktree**; a worktree with uncommitted changes, or — when its work is
not on a pull request — with commits no remote has; anything git tracks; `.git`, `~/.cargo`,
`~/.rustup`, package-manager caches (`.npm`, `.pnpm-store`, `.yarn`, `.cache`), credential stores
(`.ssh`, `.gnupg`, `.aws`, `.docker`, `.config`); and anything outside the data dir's worktrees,
its archive and the paths you list. Symlinks are never followed, and every path is re-checked just
before it goes. A listed path must be absolute, and neither `/` nor your home directory itself.

**Cheap when there is nothing to do.** A worktree's build directories are found once and cached until
the colony changes; the host paths' walk is cached for six hours; both walks are bounded in depth
and in directories visited. An hourly run over unchanged colonies reads the colony list, asks
`msb` for its microVMs and runs `df`.

**What a run leaves**: the loop's **History** lists each run — the bytes freed per category, the
paths, what was kept and why, and what started it (schedule, low disk, or you). Each run is also an
activity line in History (`disk_cleanup.run`). If the disk is still under the trigger afterwards the
loop carries an attention item — "Disk still 94% full after cleanup — 40G in live colonies" — shown
on its row, recorded once as `disk_cleanup.attention`, and cleared by the next run that ends above
the trigger.

**Who may change it**: it is per install, since every user runs their own mothership. Only the
owner (the cockpit, or the owner's API token) sees, switches, previews or runs it; a scoped API
token never does, so the host-level categories are the owner's alone. It cannot be deleted —
switch it off — and there is only ever one.

The API is the loops API with the id `disk-cleanup`: `PUT /api/loops/disk-cleanup` takes
`enabled`, `cadence` and `disk_cleanup` (the settings; left out, they are kept), and
`POST /api/loops/disk-cleanup/run-now` answers the run's report — with `?dry_run=1`, the preview,
which removes nothing ([protocol.md](protocol.md#loops)).

## Docs & README

A built-in loop that keeps a repository's documentation in step with its code. It is **off** until
you name a repository (`owner/name`) or an org (`owner`) for it: **Loops** → **Docs & README** →
**Enable**, or `POST /api/docs-loop/enable`. It then runs daily; the interval goes down to hourly,
and a loop you have just enabled first runs 10 minutes later.

Each run reads the mothership's bare clone of every allowlisted repository (at most 20 per run, 5
seconds apart) at its default branch. The checks are deterministic, spend no model tokens and never
run the repository's own code on the host:

- **Code changed, docs did not**: merged changes since the loop's last run (or, the first time,
  since one interval ago) that changed something a doc names — an identifier that reads as code
  (`loop_next`, `OrgSettings`), a flag (`--max-runs`) or an API route (`/api/loops/{id}`) in its
  inline code — in a code file that doc describes, without touching it. A change that touched any
  doc (the changelog aside) is left out, and so are comment-only, formatting-only and moved lines.
  The docs map is derived from the docs themselves: a README or `docs/` page that names at most
  eight code files (by a link or in inline code) describes each of them, and `docs/<name>.md`
  describes the code files called `<name>` (when at most three share the name). Entry points
  (`main`, `lib`, `mod`, `index`) and tests are left out.
  A `.colonizer/docs-map.toml` in the repository adds to it:

  ```toml
  derive = true                        # false: use only the [[map]] entries below
  docs = ["guides/**"]                 # more paths that count as docs
  ignore = ["src/generated/**"]        # code never flagged
  checks = ["npm run docs:check"]      # the docs checks the colony runs
  [[map]]
  code = ["src/api/**"]
  docs = ["docs/api.md"]
  [[cli]]
  name = "mytool"
  sources = ["src/cli.rs"]
  ```

- **Broken links and anchors**: relative links in README files and `docs/` whose file is gone, or
  whose `#anchor` matches no heading — GitHub's rules, as `scripts/check-doc-links.mjs` reads them.
- **Commands that no longer exist**: in shell code blocks and inline code, `npm`/`pnpm`/`yarn`/`bun
  run` scripts no `package.json` defines, script paths that are gone, `make` targets and `cargo -p`
  crates that are not declared, and the repository's own CLI's subcommands and flags — for
  Colonizer, `colonizer` against `crates/colonizer/src/cli.rs`; elsewhere, the `[[cli]]` entries.
- **Route drift** (Colonizer's own layout, or a `[routes]` entry with `snapshot` and `doc`): routes
  `crates/colonizer/routes/` gained since the last run that `docs/protocol.md` does not name, and
  routes it lost that the doc still names.
- **Changelog**: only where the repository keeps `changelog.d/` fragments or a `## Unreleased`
  section. With fragments it follows the repository's own `scripts/changelog.mjs check`: a change
  touching its `CODE_PATHS` (or code, without that script) that neither adds a fragment nor edits
  `CHANGELOG.md` — so never a release, which folds and deletes fragments — and only changes since
  the repository adopted `changelog.d/`, dependency bumps aside. Like that check, it only warns:
  the finding is reported, but never dispatches a colony on its own. With `## Unreleased`: an
  empty section, or one missing a merged change.

When a repository has findings, the run dispatches **one** colony for it (origin `docs-loop`) with
the findings as its brief and these rules: change only documentation, never code; keep the
repository's writing style; claim nothing the code does not do, and verify every statement against
the code; keep the diff small; run the repository's docs checks. It does not dispatch when a docs
colony of this loop is still live or its pull request is open, when a branch named for docs or the
README with commits the default branch lacks was pushed in the last 14 days, or within the
repository's cooldown (24 hours after its last dispatch by default). With external writes blocked
(`COLONIZER_NO_EXTERNAL_EFFECTS`) a run only reports.

Every run's report — the findings, and what was dispatched or skipped and why — is kept in the
loop's history (the last 30 runs) and written to the activity log as `loop.docs`; the Loops page
shows the last one. **Dry run** reports the findings and writes nothing: no colony, no history, no
activity. The routes are in [protocol.md](protocol.md#docs--readme-loop).

## Dependencies & supply chain

A built-in loop that checks the dependencies of the repositories you opt in and opens pull requests
that fix what it finds. It is **off** until you switch it on *and* add an org (`acme`) or a
repository (`acme/app`) to its allowlist; both start empty. It is at the top of the Loops page, and
its settings are in `<config_dir>/supply-chain-loop.json`.

**When it runs.** Daily by default (06:17 UTC); hourly, every 6 hours or weekly from the page, and
never more often than hourly. **Dry run** runs the whole check and lists what it would dispatch,
without starting a colony or saving anything; **Run now** does a real run.

**What it checks.** The manifests and lockfiles at each repository's default branch, read from the
mothership's own mirror, never inside a colony: `Cargo.lock`, `package-lock.json`, `bun.lock`,
`pnpm-lock.yaml`, `yarn.lock`, `poetry.lock`, `uv.lock`, `requirements.txt` and `go.mod`. Each
lockfile goes to the first scanner installed on the host that reads it:

| Lockfile | Scanners, best first |
| :--- | :--- |
| `Cargo.lock` | `cargo-audit`, then `osv-scanner` |
| `package-lock.json` | `npm audit`, then `osv-scanner` |
| `pnpm-lock.yaml`, `yarn.lock`, `poetry.lock`, `uv.lock`, `requirements.txt`, `go.mod` | `osv-scanner` |
| `deny.toml` (a licence policy) | `cargo-deny` (`check licenses bans`) |

A lockfile no installed scanner reads, `bun.lock` included, is checked with the mothership's own OSV
lookup (the one behind the Packages tab), unless you switch that off. The report says which scanner
was missing and how to install it; Colonizer never downloads or runs a tool you have not installed.

It collects known vulnerabilities with their severity and fixed version, yanked versions,
unmaintained (RustSec) and deprecated packages, and licence-policy violations when the repository
has a `deny.toml`. **Outdated direct dependencies** (a major version or more behind) are reported
when you tick that setting; they are off by default and never dispatched.

**How it fixes.** The fixable findings (a vulnerability with a fixed version, or a yanked release)
at or above the dispatch threshold (moderate by default) are grouped into one *supply-chain target*
per repository and ecosystem, and one colony gets the whole group, never one per package. Its brief
lists the exact findings and says: make the minimal bump to each fixed version, update the
lockfiles, run the repository's checks, make no unrelated upgrades, and make no major bump unless
it is the only way to a fix (and then explain it in the pull request). Its origin is
`supply-chain:<ecosystem>`.

A target is **not** dispatched, and the report says why, when:

- `COLONIZER_NO_EXTERNAL_EFFECTS` (or `COLONIZER_NO_WRITE`) is set: the run reports only;
- a supply-chain colony on the same repository and ecosystem is still live, queued, parked or has
  its pull request open, and was given any of the same findings (or a Packages-tab hand-off on one
  of the same packages is still open): the duplicate target is refused;
- the repository is cooling down: 12 hours after its last dispatch by default;
- the run has reached its caps: 1 colony per repository and 3 per run by default. Repositories are
  checked one at a time, so a run never bursts.

**The report.** Every run records its findings by severity, what it dispatched and what it skipped
and why. The last report and the history of runs are on the Loops page, and each run and each
dispatch is a `loop.supply_chain` line in the activity log. A critical or high vulnerability with
no fixed version raises an attention item on the page, since no colony can bump past it.

## Cost

Each run is a full colony with its own microVM and model spend. A frequent loop on a large
repository adds up: start daily, and let the loop stop itself when its goal is met. The loop's
history lists every run with its status, pull request and cost.

## TypeScript: remove any

A built-in loop that counts the explicit `any` in the TypeScript repositories you opt in and hands
one small batch at a time to a colony that replaces them with real types. It is **off** until you
switch it on *and* add an org (`acme`) or a repository (`acme/app`) to its allowlist; both start
empty. It is on the Loops page, below your own loops, and its settings are in
`<config_dir>/ts-any-loop.json`.

**When it runs.** Daily by default (07:43 UTC); hourly, every 6 hours or weekly from the page, and
never more often than hourly. **Dry run** counts and lists what it would dispatch, without starting
a colony or saving anything; **Run now** does a real run.

**How it counts.** On the mothership, never in a colony, and without a model: the TypeScript
sources (`.ts`, `.tsx`, `.mts`, `.cts`) at each repository's default branch, read from the
mothership's own mirror, in any repository with a `tsconfig.json`. `node_modules`, `dist`, `build`,
`out`, `coverage` and `vendor` are left out. Two methods, and every report says which one ran and
why:

- **The repository's own TypeScript**, when `node` is on the host and `node_modules/typescript` can
  be had: it is not in the mirror, so the loop installs the repository's dependencies into a scratch
  copy with its own package manager (`npm ci`, `pnpm install`, `yarn install`) *offline* — only what
  the host's package cache already holds, with install scripts off. A parse with its compiler API
  then finds every `any` keyword. With **Also count implicit any** ticked, a program built from each
  tsconfig with `noImplicitAny` counts what that flag would report as well (reported, never
  dispatched). Switch **Install offline** off to skip the install.
- **A token scan** otherwise (no `node`, a bun lockfile — bun has no offline-only install — a cache
  that lacks packages, or TypeScript 7, which has no JavaScript compiler API). It skips comments,
  strings, template text and regular expressions, and reads the same forms from the tokens around
  each `any`. It is close to the parse but not exact: an identifier named `any` in an odd place can
  fool it.

Both count `: any`, `as any`, `<any>x`, `Foo<any>`, `any[]`, `Array<any>`, `Record<string, any>`,
generic defaults (`<T = any>`) and `any` elsewhere in a type (`string | any`, `() => any`, `type X =
any`), per file and per module (a file's directory). Each real run keeps the totals, so the page
draws the trend and each report says how the count moved since the last run.

**How it fixes.** One colony per repository per run, with a small batch: the module with the most
explicit `any`, and its first 20 occurrences (the batch size is a setting). The brief lists each
`file:line:column` with its source line and the rules: replace each with a real type, `unknown`
plus narrowing, or a generic; never an `as` cast to silence an error, `// @ts-ignore`,
`// @ts-expect-error`, `// eslint-disable` or a new `any`; no change in behaviour; the repository's
type check (`tsc --noEmit` or `tsc -b`) and tests must pass; keep the diff small. Its origin is
`ts-any:<module>`, and its title `TypeScript: remove any in <module> (20 of 57)`.

A batch is **not** dispatched, and the report says why, when:

- `COLONIZER_NO_EXTERNAL_EFFECTS` (or `COLONIZER_NO_WRITE`) is set: the run reports only;
- a colony on the same repository and module is still live, queued or parked, or has its pull
  request open: the next module down goes instead;
- the repository is cooling down: 20 hours after its last dispatch by default;
- the run has reached its cap: 3 colonies per run by default, never more than one per repository.
  Repositories are counted one at a time, so a run never bursts.

**The post-check.** Once a batch colony has published its pull request, the next run checks out its
branch from the mirror and counts again (at most three such recounts per run). The batch is flagged
when the module's explicit `any` did not drop, or when suppression comments (`@ts-ignore`,
`@ts-expect-error`, `@ts-nocheck`, `eslint-disable`), `as` casts or `any` outside the module were
added. A flagged batch is listed in the report and stays an attention item on the page while its
record is kept (30 days).

**The report.** Every run records the totals, the busiest modules and files, the trend, what it
dispatched and what it skipped and why, and any recount. The last report, a small trend line and
the history of runs are on the Loops page, and each run and each dispatch is a `loop.ts_any` line in
the activity log. Its routes are `GET/PUT /api/ts-any-loop` and `POST /api/ts-any-loop/run` (see
[protocol.md](protocol.md#loops)).

## API

`GET/POST /api/loops`, `PUT/DELETE /api/loops/{id}`, `POST /api/loops/{id}/run-now`,
`GET /api/loops/{id}/runs` — see [protocol.md](protocol.md#loops). Loops are saved in
`<config_dir>/loops.json`; the built-in disk cleanup in `<config_dir>/disk-cleanup.json`. The
merge-train loop is `GET/PUT /api/merge-train/loop` and
`POST /api/merge-train/loop/run[?dry_run=true]`, saved with its history in
`<config_dir>/merge-train-loop.json`. The supply-chain loop has its own routes:
`GET/PUT /api/supply-chain-loop` and `POST /api/supply-chain-loop/run`.
