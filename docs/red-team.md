# Red-team raids

A red-team run sends N hunters with distinct briefs to raid one repository at the same time.
Each hunter is an ordinary colony — same validation, same queue, same microVM — with one
assignment and one rule: find and report bugs, don't change the code. When every hunter is
done, the run is done, and its tally says what the swarm found.

## The empty-nest rule

A run launches only while no colony is live — the nest is empty. There are two ways to ask:

- **Start now.** Refused with a 409 while any colony is live, naming the live count
  ("2 colonies are live — a red-team run can only start when the nest is empty"), so
  starting costs nothing when the nest is occupied.
- **Arm.** The run waits (`armed`, with a `gate_reason` saying what it waits for) and a
  background tick launches it the next time the nest empties.

One active run per repository at a time; a second repo can raid in parallel (its run arms
while the first runs, since the gate is global). Stopping a run is idempotent: live hunters
are stopped and their microVMs removed, queued hunters leave the queue, finished hunters
are not touched.

## The swarm

Swarm size is 1 to 8, default 3. Hunters draw their briefs from eight focus areas, cycling
`i % 8`, and each brief names its own focus and lists the other seven, so the swarm keeps
out of one another's way:

1. error handling and edge cases
2. concurrency and race conditions
3. input validation and injection
4. resource leaks and exhaustion
5. auth and permission boundaries
6. core-flow logic errors
7. silent failures and swallowed errors
8. API and contract mismatches

The module a hunter runs on defaults to `general` and can be set per run. Hunters are
titled `Red-team hunter i/N: <focus>` and numbered from 1, matching what the UI shows.

## The brief

With autofix off — the default — the brief is explicit: hunt aggressively, reproduce each
bug before reporting it, report with the findings tool, and **never open, merge or
autofix anything**. Autopilot stays off. With autofix on, the brief instead expects the
fix itself, and autopilot runs.

When the operator points the mothership at a bench pool (`COLONIZER_BENCH_POOL=<dir>`), the brief
also carries that pool's raid leads for the raided repository: injected bugs the bench admitted but
its own tests caught unreliably, dealt out round-robin so no lead is handed to two hunters — a set
longer than the swarm can carry at the 20-per-brief cap waits for a later run. The brief names them
the hunter's own and says to chase them first, even where they fall outside the focus assignment —
the one exception to it ([docs/bench.md](bench.md#the-raid-set)).

## The Security preset

A run has a preset: `general` (the default, and what every run made before presets reads as) or
`security`. The general preset is the bug hunt above, unchanged. The security preset keeps the same
swarm mechanics — 1 to 8 hunters, focus areas cycling `i % 8`, each brief naming its own focus and
listing the other seven, the findings tool, never open, merge or autofix unless autofix is on, bench
raid leads, and a synthesis colony at the end — with its own eight focus areas:

1. **auth on every route** — every route, endpoint and handler refuses an unauthenticated caller on
   the server; admin checks happen on the server, never only in the UI.
2. **object-level access (IDOR)** — changing IDs in URLs and bodies must not reach another user's or
   tenant's rows; row level security is on and scoped where the stack has it.
3. **sessions, tokens and secrets** — token lifetimes, refresh tokens revoked on logout, no secrets in
   URLs or logs, no home-rolled auth or crypto.
4. **input handling and injection** — server-side validation, SQL built from strings, command
   injection, path traversal, output escaping and XSS, unsafe deserialisation.
5. **the web boundary** — CORS (never a wildcard with credentials), CSRF, webhook signature checks,
   open redirects, SSRF on fetch-by-URL, uploads (size, type, isolated processing).
6. **abuse and cost limits** — rate limits on login, signup, password reset and AI endpoints; spending
   caps; loops and queues a caller can make unbounded.
7. **AI and agent safety** — model output and fetched content treated as untrusted; tool, SQL and
   shell calls a model makes bounded and confirmed; injected instructions in repo agent files; agents
   kept away from production credentials; dependencies an assistant may have invented.
8. **failure and leakage** — generic errors to clients, secrets and personal data kept out of logs,
   an audit trail, and backups with a restore path.

A security brief adds two rules. A finding needs a proof attached: the request and response, a
failing test, or a minimal script run against a local instance the hunter started. And the rules of
engagement: attack only the repository and a local instance inside the hunter's microVM — no
deployed environment, external host or third-party service, and no real credentials.

Pick it with the wizard's **Preset** choice, `"preset": "security"` on `POST /api/redteam/runs` or
`POST /api/redteam/schedules`, or `colonizer redteam start owner/repo --preset security`
([cli.md](cli.md#red-team-runs)). An unknown preset is a 400.

### The pre-scan

Before the first hunter of a security run exists, the mothership pre-scans the repository's host
mirror (`data/repos/<owner>/<repo>.git`). It is deterministic: no model tokens, and the repository's
code never runs. The tree is read blob by blob with `git ls-tree` and `git cat-file` — no checkout,
so no attributes, filters or hooks — and the history with `git log -p`, both with repository-controlled
execution and lazy fetches turned off. It checks for:

- `.env` files (not templates) committed, or env files in use with no `.gitignore` rule covering them;
- secrets in the tree and the history: with gitleaks when the
  operator has it on the host's `PATH` (it is never downloaded; its report is redacted), otherwise a
  built-in provider-prefix scan, and the report says which ran and that the fallback knows fewer key
  shapes;
- client-exposed env vars that are secret-shaped (`NEXT_PUBLIC_*SECRET*`, `VITE_*_API_KEY`; anon,
  publishable and public keys are left alone);
- manifests with no committed lockfile, and dependencies on `*`, `latest` or open ranges;
- declared dependencies the lockfile has no entry for, or lockfile entries with no registry metadata —
  checked against the lockfile only, offline — as candidates for invented packages;
- CORS allowing any origin in a file that also allows credentials;
- SQL assembled from strings (concatenation, templates, f-strings, `format!`);
- webhook routes in files with no sign of a signature check;
- public storage buckets and rules (S3 ACLs and policies, GCS `allUsers`, Supabase public buckets,
  Firebase `if true`);
- row level security disabled in SQL migrations, or (on Supabase) tables created without it;
- repo agent files (`CLAUDE.md`, `AGENTS.md`, `SKILL.md`, `.mcp.json`, Claude and Cursor settings)
  with suspicious instructions, hidden characters or wide tool grants, flagged for human review.

Every hit is a **lead**, never a vulnerability. Leads are numbered `P1`, `P2`, … and each belongs to
one focus area; the run deals it to the hunter holding that focus (round-robin when a swarm of more
than eight holds a focus twice; to hunter `focus % n` when a smaller swarm holds none), up to 20 per
brief, like bench raid leads. The brief asks the hunter to confirm or dismiss each one and cite its id.
A mirror that does not exist yet (no colony has cloned the repository) skips the scan and says so.

### The report

The pre-scan is stored on the run as `prescan` — the scanned commit, which secret scanner ran,
notes, the leads, and the operator checklist — and the cockpit's red-team history shows it under the
run as two sections: **Pre-scan leads** and **Operator checklist**.

The checklist lists what code cannot prove: keys rotated after any exposure, spending caps at every
provider, token lifetimes set, backups restore-tested, production credentials out of agents' reach,
and upload processing isolated. Each item carries the evidence the repository or the mothership
shows (the exposures the pre-scan found, "no spend cap in Colonizer provider settings for provider
X", "no backup job found in repo", where lifetimes or upload handling appear) and is either
`needs_review` or `not_verifiable`. No item is ever marked passed.

The security synthesis judge ranks the merged defects by severity (reproduced before unconfirmed at
the same severity), attaches the strongest proof to each line (`proof`), and merges the pre-scan leads
hunters confirmed into the defects they became (`prescan_leads`). A lead nobody confirmed stays out of
the merged report; it is still listed on the run.

## Findings and the tally

Each hunter writes `findings.jsonl` in its session directory (protocol §6.6), one line
per stage a finding reaches: `validated` or `rejected` by the orchestrator, then `filed`
or `duplicate` (an open issue already had the title), and with autofix the fix colony,
its review and merge. The run's tally groups a hunter's lines by the finding's title and
counts each finding once: every finding counts as found, and as validated, rejected or
filed (`filed` or `duplicate`) when it reached that stage. Lines written before stages
existed carry no `state` and are read by what they carry: an `issue` or `duplicate_of`
counts as filed.

- `found`: raw findings across hunters (a defect two hunters report counts twice).
- `validated` / `rejected`: the validator's verdicts from the ledgers.
- `filed`: filed as an issue, or matched to one already open.
- `merged`: distinct defects after synthesis (null until one finishes).

## Synthesis

When a run with findings lands `done`, the tick launches one more colony — the synthesis
judge — to merge the hunters' findings into a single report. It fires exactly once, at the
transition into `done`: never mid-run, never on a stopped run, never for a run already done,
and never when the hunters found nothing. The judge publishes nothing (autopilot, autofix and
automerge off; the brief orders it to write only `/harness/out/redteam-report.jsonl` — file
nothing, open no issue or pull request, no code changes). Colonies cannot mount host files, so
the brief carries the hunters' ledgers inline, cut to an equal share under the instructions cap.

The report is JSON lines, one object per distinct defect, most severe first (the schema is in
protocol §6.7); each line names every hunter that reported the defect (`hunters`, `merged_from`)
and carries the validator's verdict explicitly (`validation`, #211).

`synthesis` on the run tracks the judge independently of the run's state, which stays `done`:
`pending` → `running` → `done` (the report is linked, and `merged` counts its lines) or `failed`
with a `reason`. `POST /api/redteam/runs/{id}/synthesize` (**409** on a run that is not done or found nothing)
retries it: the previous colony's id moves to `superseded`, its report stays on disk, and the
linked `report` and `merged` stay until the new colony finishes; while one is pending or running
the route is idempotent. `GET /api/redteam/runs/{id}/report` serves the linked report parsed
(**404** when nothing is linked or the file is gone).

## States

`armed` → `waiting` / `running` → `draining` → `done`, plus `stopped`:

- `armed`: created to arm, waiting for the gate; `gate_reason` says why.
- `waiting`: hunters launched but all still queued for a parallel slot (hunters
  consume parallel slots like any colony).
- `running`: at least one hunter starting or live.
- `draining`: no hunter live, but some still in flight (publishing, PR open) or queued.
- `done` / `stopped`: terminal; the tick never touches them again.

A crash mid-launch cannot duplicate the swarm: the launch is recorded before the
first hunter exists, a half-launched run drains to `done`, and hunter sessions missing
after a restart count as ended. Runs persist in `data/redteam.json`, load-tolerant the
same way `sessions.json` is.

## Operating it

Overview → the Workspaces table → a workspace row's **Red team** button (the hooded
figure) opens a three-step wizard scoped to that workspace:

1. **Hunter.** A short "what is a red team" note, **Who hunts**, and the workspace's
   repositories (one run per repository; one that already has an active run is
   skipped). Only the **colony swarm** can run. **Strix** and **Shannon** appear as
   disabled cards marked "Coming soon" (Strix reads "Installed · runs coming soon" once
   its binary is installed). They are not built as red-team hunters: Strix can be
   installed and probed (see [security-hunters.md](security-hunters.md)), but no run
   drives its scans, and Shannon is a manifest with no install or run behind it.
   `POST /api/redteam/runs` with `"hunter": "strix"` or `"shannon"` returns 400:
   *Strix cannot run as a red-team hunter in this build yet*.
2. **Models.** A provider and model for the hunters and for their subagents, and the
   number of hunters per repository (1–8). They reach each hunter as
   `model_override` / `subagent_model_override` on `POST /api/sessions`.
3. **Review.** A cost warning with an estimate from past runs (or average colony
   spend), "let hunters fix what they find" off unless ticked (a raid never merges
   unless autofix is on), and **Once, now**, **Weekly** or **Monthly** in local time, saved as
   UTC. A one-off run starts armed and launches as soon as no colony is live.

Schedules persist in `<config_dir>/redteam-schedules.json`; a once-a-minute loop
starts every due schedule through the same path as a manual start (a monthly schedule
on the 29th–31st fires on a shorter month's last day). The row's history button opens
the workspace's red-team history: its schedules (pause, resume, delete) and its runs,
live first, with state, hunter, models, found / validated / filed / rejected, cost and
a stop button.

API: `GET`/`POST` `/api/redteam/runs`, `GET /api/redteam/runs/{id}`,
`POST /api/redteam/runs/{id}/stop`, `POST /api/redteam/runs/{id}/synthesize`,
`GET /api/redteam/runs/{id}/report`, `GET`/`POST` `/api/redteam/schedules`,
`PUT`/`DELETE` `/api/redteam/schedules/{id}`.
