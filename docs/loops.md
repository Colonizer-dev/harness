# Loops

A loop is a saved prompt on a repository that launches a colony on a schedule — the mothership's
version of Claude Code's `/loop`. Use one for work that recurs: triaging new issues every morning,
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


Each run is a full colony with its own microVM and model spend. A frequent loop on a large
repository adds up: start daily, and let the loop stop itself when its goal is met. The loop's
history lists every run with its status, pull request and cost.

## API

`GET/POST /api/loops`, `PUT/DELETE /api/loops/{id}`, `POST /api/loops/{id}/run-now`,
`GET /api/loops/{id}/runs` — see [protocol.md](protocol.md#loops). Loops are saved in
`<config_dir>/loops.json`. The supply-chain loop has its own routes: `GET/PUT /api/supply-chain-loop`
and `POST /api/supply-chain-loop/run`.
