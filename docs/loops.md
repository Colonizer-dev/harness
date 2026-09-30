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

An end date (`end_at`) can be set through the API; the cockpit's form keeps one that is already set
but has no field for it.

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

## Cost

Each run is a full colony with its own microVM and model spend. A frequent loop on a large
repository adds up: start daily, and let the loop stop itself when its goal is met. The loop's
history lists every run with its status, pull request and cost.

## API

`GET/POST /api/loops`, `PUT/DELETE /api/loops/{id}`, `POST /api/loops/{id}/run-now`,
`GET /api/loops/{id}/runs` — see [protocol.md](protocol.md#loops). Loops are saved in
`<config_dir>/loops.json`; the built-in disk cleanup in `<config_dir>/disk-cleanup.json`.
