# Loops

Part of the [Colonizer protocol](../protocol.md).

Scheduled colonies ([loops.md](../loops.md)), saved in `<config_dir>/loops.json`; every write needs the
Bearer token and `Origin` like the other writes.

| Route | What it does |
|---|---|
| `GET /api/loops` | Every loop: `{id, name, org, repo, prompt, cadence, kind, tz_offset_minutes, model, subagent_model, autopilot, max_runs, retry_failed_runs, end_at, enabled, next_run_at, runs, last_run: {session, at, retried, outcome}, last_note, ended_reason, created_at}` — `last_run.outcome` is the last colony's status with a failed run's class on it (`failed (transient_infra)`), derived on the read and not stored; `retry_failed_runs` is the minutes within which a run that failed for an infrastructure reason is run once more (null = 60, 0 = off, issue #881). Plus, on a map loop over `owner/*`, `pending`: the repositories still to map in the current org cycle (server-owned; a body without it still reads, as `[]`), and `created_by_token` when a scoped API token created the loop. `next_run_at` is null once the loop has ended. |
| `POST /api/loops` | Creates one from `{name, repo, prompt, cadence, kind? ("colony"), tz_offset_minutes?, model?, subagent_model?, autopilot? (true), max_runs?, retry_failed_runs?, end_at?, enabled? (true)}`. A `<provider>/<model>` must name a configured provider. A map loop (`kind: "map"`) ignores `prompt` and may hold `owner/*` — every repository of the org; a colony loop may not. A scoped launch token (above) creates a loop only inside its org/repo limits and only as a colony loop; the answer records its id in `created_by_token`. |
| `PUT /api/loops/{id}` | Replaces its settings; id, creation time, run count, last run and `created_by_token` are kept, and the next run is recomputed (`pending` restarts empty). On the built-in `disk-cleanup` loop (`kind: "disk_cleanup"`, [loops.md](../loops/disk-cleanup.md)) only `enabled`, an interval `cadence`, `tz_offset_minutes` and `disk_cleanup` apply — `{trigger_free_pct, build_output, stopped_after_days, worktrees, microvms, archives, archive_keep_days, archive_max_gb, host_paths, extra_paths, host_min_age_days}`, missing fields defaulted, the whole object kept when left out; its `disk_cleanup` in `GET` adds `history` (newest first, 30 runs), `attention` and `previewed_at`. Owner only: a scoped token reads it as `404`, and `GET /api/loops` does not list it to one. |
| `DELETE /api/loops/{id}` | Removes the loop; its past colonies stay. The built-in `disk-cleanup` loop answers `409`: switch it off instead. |
| `POST /api/loops/{id}/run-now` | Starts a run now → the new session. `409` while the previous run is still in flight (not checked for an org-wide map loop), when a map loop had nothing to map, or when the loop's API token was revoked (the loop is ended). On the built-in `disk-cleanup` loop it answers the run's report instead — `{at, dry_run, trigger, bytes, categories: [{category, enabled, items: [{path, bytes, colony?}], count, bytes, held?: [{path, reason}], failed?, note?}], free_bytes_after?, used_pct_after?, attention?}` — and `?dry_run=1` answers the same report for a run that removes nothing (`400` on any other loop); `409` while a cleanup is already running. |
| `GET /api/loops/{id}/runs` | The loop's colonies (origin `loop:<id>`, a map loop's `map:loop:<id>`), newest first. |
| `GET /api/loops/{id}/history?days=7&tz_offset_minutes=0` | How the loop did over the last `days` (1–90, default 7; history is kept 90 days, issue #1199): `{id, days, from, to, retention_days, totals, buckets, last, runs}`. `buckets` has one entry per day, zero-filled, cut at the caller's midnight (`tz_offset_minutes` east of UTC): `{day, runs, ok, partial, failed, skipped, running, colonies, cost_usd}`. `runs` is the newest 200 in the range: `{at, finished_at?, trigger, outcome, summary, counts, colonies, cost_usd}`, with `outcome` one of `ok`, `partial`, `failed`, `skipped` (nothing to do) or `running` (a colony loop whose colony is still at work). `last` is the loop's latest run whatever the range. `cost_usd` is the model spend of the colonies the run dispatched, summed on each read, never stored. A colony loop's outcome is its colony's status. The built-in loops answer under `merge-train`, `supply-chain`, `ts-any`, `docs` and `disk-cleanup`; dry runs are not recorded. A scoped token reads it like `runs`; the built-in loops are the owner's. **404** for an unknown loop. Saved in `<config_dir>/loop-history.json`. |
| `GET /api/supply-chain-loop` | The built-in dependencies and supply-chain loop ([loops.md](../loops/dependencies.md)): `{name, settings, next_run_at, running, scanners: {"cargo-audit": bool, "cargo-deny": bool, "npm audit": bool, "osv-scanner": bool}, blocked, last_report, history, attention}`. `settings` is `{enabled (false), allow ([]), cadence (daily), max_per_repo (1), max_per_run (3), cooldown_hours (12), min_severity ("moderate"), outdated (false), builtin (true), autopilot (true)}`. Read scope for API tokens. Saved in `<config_dir>/supply-chain-loop.json`. |
| `PUT /api/supply-chain-loop` | Replaces `settings`. `400` for an allowlist entry that is not an org or `owner/repo`, a self-paced cadence or an interval under 60 minutes, or a cap outside its range. Owner only. |
| `POST /api/supply-chain-loop/run` | `{dry_run, repo?}` → the run's report: `{id, started_at, finished_at, dry_run, trigger, blocked, repos: [{repo, sha, scanners, findings, notes, missing, error}], counts, dispatched: [{repo, ecosystem, session, title, findings, worst}], skipped: [{repo, ecosystem, reason, findings}], attention, note}`. A dry run starts nothing and saves nothing, and may name any repository; a real run only one on the allowlist. `409` while a run is in progress. Owner only. |

`cadence` is tagged by `every`, all times UTC: `{"every":"interval","minutes":60}` (15–10080),
`{"every":"daily","hour":9,"minute":0}`, `{"every":"weekly","weekday":0,"hour":9,"minute":0}` (0 =
Monday), `{"every":"monthly","day":31,"hour":6,"minute":0}` (clamped to the month's end),
`{"every":"every_days","days":14,"hour":3,"minute":0}` (1–365 days, anchored: the next slot is the
first strictly after the last firing plus `days − 1` days, so a late firing never drifts), or
`{"every":"self_paced"}`.

The built-in TypeScript any loop ([loops.md](../loops/typescript-remove-any.md)) has its own routes:

| Route | What it does |
|---|---|
| `GET /api/ts-any-loop` | `{name, settings, next_run_at, running, node, blocked, last_report, history, attention, trend}`. `settings` is `{enabled (false), allow ([]), cadence (daily), batch_cap (20), max_per_run (3), cooldown_hours (20), implicit (false), offline_install (true), autopilot (true)}`; `trend` maps each repository to its totals, oldest first. Read scope for API tokens. Saved in `<config_dir>/ts-any-loop.json`. |
| `PUT /api/ts-any-loop` | Replaces `settings`. `400` for an allowlist entry that is not an org or `owner/repo`, a self-paced cadence or an interval under 60 minutes, or a cap outside its range. Owner only. |
| `POST /api/ts-any-loop/run` | `{dry_run, repo?}` → the run's report: `{id, started_at, finished_at, dry_run, trigger, blocked, repos: [{repo, sha, typescript, method ("typescript" or "token_scan"), method_note, ts_version, total, implicit, as_casts, suppressions, ts_files, forms, modules, files, previous, notes, error}], total, dispatched: [{repo, module, session, title, occurrences, module_total}], skipped: [{repo, module, reason}], checks: [{session, repo, module, pr_url, flagged, summary}], attention, note}`. A dry run starts nothing, recounts nothing and saves nothing, and may name any repository; a real run only one on the allowlist. `409` while a run is in progress. Owner only. |

A loop's colony emits two runner events (§2), acted on only for colonies whose origin names a loop:

```json
{"type":"loop_next","delay_minutes":120,"reason":"CI reruns at 11"}
{"type":"loop_stop","reason":"all flakes fixed"}
```

`loop_next` sets a self-paced loop's `next_run_at` to now + `delay_minutes` (clamped to 15–1440); a
fixed loop only notes it. `loop_stop` disables the loop and records `ended_reason: "stopped by the
colony: <reason>"`. Which runner offers the tools is the module's `loop_tools` manifest flag
([loops.md](../loops.md)): Claude Code serves them as `mcp__colonizer_loop__loop_stop` when the
mothership sets `COLONIZER_LOOP=true`, and `mcp__colonizer_loop__loop_next` only when it also sets
`COLONIZER_LOOP_SELF_PACED=true` (subagents are refused); the Codex, Grok Build, OpenCode, Hermes,
Pi and ACP runners gate the same two tools on the same env under their own names (Pi as extension
tools, ACP as the `colonizer_loop` MCP server on the session). A self-paced loop whose colony never
calls `loop_next` runs again a day later.

## Docs & README loop

The built-in docs loop ([loops.md](../loops/docs-readme.md)), saved in `<config_dir>/docs-loop.json`.
Owner only: a scoped API token reaches none of these routes.

| Route | What it does |
|---|---|
| `GET /api/docs-loop` | `{name, settings: {allow, interval_hours, cooldown_hours}, enabled, next_run_at, last_report, history: [{id, at, trigger, summary}], limits}`. `enabled` is `allow` being non-empty; `last_report` is the newest run's full report, or null. |
| `PUT /api/docs-loop` | Replaces the settings: `allow` (repositories `owner/name` and orgs `owner`, at most 100), `interval_hours` (1–168, default 24), `cooldown_hours` (1–720, default 24). **400** for anything else. |
| `POST /api/docs-loop/enable` · `POST /api/docs-loop/disable` | `{target}`: adds a repository or org to the allowlist, or removes one (**404** when it is not there). A loop that becomes enabled runs 10 minutes later; removing the last entry switches it off. |
| `POST /api/docs-loop/run` | `{dry_run?}` (default false): runs over the allowlist now and answers the report. A dry run launches nothing and records nothing. **409** while the loop is off or a run is in progress. |

A report is `{id, at, trigger: "schedule"|"run_now"|"dry_run", dry_run, external_writes_blocked,
repos: [{repo, head, since, findings: [{kind, file?, line?, change?, message, advisory?}], more, action,
reason, colony}]}`. An `advisory` finding (a missing changelog fragment) is reported but never
dispatches a colony on its own. `kind` is `undocumented_change`, `broken_link`, `broken_anchor`, `missing_command`,
`routes_drift`, `changelog` or `docs_map`; `action` is `clean`, `dispatched`, `skipped`,
`report_only` or `error`; `more` counts the findings past the 40 a report keeps. The colony a run
dispatches carries the origin `docs-loop`.

## 6.12 GitHub loops (issue #778, PR actions issue #807)

Some loops do GitHub's work (triaging issues, CI flakes, merged PRs), but a colony has no GitHub
token. Such a loop sets `needs_github: true` on its definition (colony loops only; the map and
disk-cleanup loops ignore it) — a full-replace field on `POST /api/loops` and `PUT /api/loops/{id}`,
set by the cockpit's "Needs GitHub" switch and by the templates that need it.

**Preflight.** Before such a loop launches anything (a scheduled firing or run-now) the mothership
checks it can read the loop's repository. If it cannot — no token, no access, no such repository, or
GitHub is unreachable just now — no colony boots, and the loop records one `last_note` saying what is
missing and how to fix it. A scheduled loop tries again at its next slot.

**Read-only context.** A run that does launch gets the loop's inputs as read-only JSON under
`/colonizer/github` (the session's `vm` dir, mounted read-only): `issues.json`, `ci-failures.json`
and `merged-prs.json`, each narrowed server-side to what happened since the loop's last run (the 24
hours before a first run) and carrying the `since` timestamp. A fetch that fails writes nothing for
that file and logs; it never fails the boot.

**Host-proxied writes.** When the context was written, boot sets `COLONIZER_GITHUB=true`, which adds
the runner's in-process MCP server `colonizer_github` — six write tools the orchestrator alone may
call (a subagent's call is refused by a `PreToolUse` hook, as with `finding_file`):

```jsonc
{"type":"github_action","tool":"issue_label","issue":42,"labels":["bug","P1"]}
{"type":"github_action","tool":"issue_comment","issue":42,"body":"markdown…"}
{"type":"github_action","tool":"issue_close_duplicate","issue":43,"duplicate_of":42}
{"type":"github_action","tool":"pr_comment","pr":219,"body":"markdown…"}
{"type":"github_action","tool":"pr_label","pr":219,"labels":["needs-human"]}
{"type":"github_action","tool":"pr_merge","pr":231,"head_sha":"<40 hex>","reason":"one line"}
```

`github_action` is host-consumed (the web ignores it, like `loop_next`/`loop_stop`). The `loop_github`
module validates it (positive issue/PR numbers, ≤ 10 labels of ≤ 100 chars, a comment ≤ 20 000 chars,
a merge `reason` ≤ 500 chars and a full 40-hex `head_sha`, no self-duplicate), refuses it while
external writes are blocked (§6.3: `ignored a github action: external writes are blocked …`), caps one
colony at 30 writes counted from `sessions/<id>/github.jsonl`, and makes the `gh` call on the colony's
own repository — the guest never names a repository.

**Merge gates (issue #807).** `pr_merge` is refused unless the operator has enabled merges for this
repository: the org setting `merge_prs` is a list of repos of that org, empty by default, set with
`PUT /api/orgs/{org}`. Even then a merge is made only for an open, non-draft, same-repo pull request
whose head is still `head_sha`, that is mergeable, with every check green, targeting the default
branch, touching nothing under `.github/` and no credential-like file, and only up to 5 merges per
colony inside the same 30-action cap. A refusal is recorded like any other action — one ledger line
`{ts, repo, outcome:"refused", tool, …}` in `github.jsonl` and one line of the colony log. The kill
switch `COLONIZER_NO_EXTERNAL_EFFECTS` covers these writes as it does the issue tools. `pr_update` (a
fast-forward push to a PR branch) is not implemented yet.

Every outcome, success or failure, is one ledger line `{ts, repo, issue, outcome, tool, …}` and one
line of the colony log; the agent is told only that the call was handed over. Because the cap is
counted from the ledger, a partial failure (a close-duplicate's comment written, its close refused)
still counts.
