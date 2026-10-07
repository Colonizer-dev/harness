# Activity log

Part of the [Colonizer protocol](../protocol.md).

## 6.9 Activity log

What happened on this mothership, for the cockpit's History page: a colony's **outcomes**, and what
a **person** did through the API. One JSON line per event in `activity.jsonl` in the data dir.

Outcomes are recorded once, at the transition, by the same edge the spend journal's `returned`
row keys on (`App::update_session`, and the queue's retire of a colony that can never start): a
status change into `pr_opened`, `merged`, `closed`, `no_changes`, `stopped`, `failed` or
`waiting_for_answer`. A write that leaves the status alone — a reclaim flipping `cleaned_up`, the
app slot moving, a restart re-marking a stopped colony — records nothing, so an outcome's time is
when it happened, not when its record was last touched. (History used to date colonies by
`updated_at`, which every such write moves.) Two outcomes are written directly rather than through
that edge, because neither moves the status: `outcome.suspended` records a colony's microVM being
torn down while it waits on its answer, and `outcome.restored` the resumed boot delivering it
([#562]).

Actions are recorded by one route layer inside the API token check, keyed on the matched route,
for the routes a person changes something with: colony launch, stop, resume, delete, Create PR,
catch-up, cleanup and keep; loop create, edit, pause/resume, delete and run-now; red-team runs and
schedules; workspace switch on/off and settings; module, provider, secret, Claude and GitHub token,
Claude account, mem0 key, notification secret, voice key, telemetry, usage, update and login-item
settings; memory review and notes; burn-down stop; the
update itself; map requests; filing an issue from chat. A colony's answer is recorded where it
arrives, on the colony's WebSocket, and the remote-access switches are recorded by their own
handlers too (§6.10), because the switch must already hold when the record is written. A refused
request (non-2xx) records nothing. When a request itself causes an outcome (Stop sets `stopped`
before the handler returns), the outcome line takes the person as its actor and the request writes
no second line. A map-refresh loop's colony records its outcome as `map.refresh` when it ends.

**Never recorded**: request bodies and secret values. A line names *which* thing changed — a
provider id, a secret's id, a module kind — never what it was set to. A response body is read only
for the few routes whose answer names what they just made (a colony, loop or run) or the pull
request they opened, and only a fixed few fields of it.

```json
{"seq": 812, "ts": "2026-09-25T00:16:11Z", "kind": "outcome.stopped", "actor": "you", "via": "cockpit",
 "org": "acme", "repo": "acme/web", "issue": 3473, "colony": "c1c9215b", "title": "Wire the method picker"}
{"seq": 813, "ts": "2026-09-25T00:17:02Z", "kind": "settings.save", "actor": "you", "via": "api",
 "target": "provider openrouter", "section": "providers"}
```

- `seq` climbs by one per line across restarts and rotation; it is the paging cursor.
- `actor` is `you` (whoever holds the API token — the cockpit is single-user, so there is no
  finer identity) or `colony`. `via` says how `you` came in: `cockpit` (the browser's cookie),
  `api` (an `Authorization: Bearer` token: the CLI or a script), or `token:<name>` (a scoped API
  token, named — never its secret).
- `kind` is closed: `outcome.{pr_opened,merged,closed,no_changes,stopped,failed,question,suspended,restored}`,
  `colony.{launch,stop,resume,delete,publish,catch_up,cleanup,retain,answer}`,
  `chat.{colony,issue}` (`chat.colony` is a launch whose request carried `origin: "chat"`),
  `colonize.{issue,colony}` (an issue filed through `POST /api/repos/{owner}/{repo}/issues`, and a
  launch whose request carried `origin: "colonize"`),
  `loop.{create,update,pause,resume,delete,run_now}`, `redteam.{start,stop,schedule,unschedule}`,
  `remote.{enable,disable,reset}`, `workspace.{enable,disable,settings}`, `settings.{save,remove}`,
  `memory.{review,note}`, `burn_down.stop`, `app.update`, `map.create`, `map.refresh`.
- `org`, `repo`, `issue`, `colony`, `title`, `pr_url` describe the colony (kept on the line, so it
  still reads after the colony is deleted); `target` names a non-colony target; `section` is where
  the cockpit shows it (a settings section id, `secrets`, `loops`, `memory`, `redteam`); `detail`
  is one line of context (a failure's error). Free text is clipped to 240 characters.

## `GET /api/activity?before=&limit=&kind=&actor=&org=&repo=&q=`

```json
{"entries": [ActivityEntry, ...], "next_before": 700, "skipped": 0}
```

Newest first. `limit` defaults to 100, at most 500. `before` answers only lines with a smaller
`seq` — pass the previous page's `next_before`, which is `null` on the last page. `kind` is a
comma-separated list of kinds or groups (`outcome`, `colony`, `settings`, …); `actor` is `you` or
`colony`; `org` keeps that workspace's lines plus the install-wide ones that belong to no org
(settings); `repo` is exact but ignores case (as does `org`); `q` is a case-insensitive substring over repo, title, target, detail,
colony id and `#issue`. An unknown kind or actor, or a `limit` outside 1–500, is refused with a
**400** naming the value and what is accepted. A line that does not parse is skipped, counted in
`skipped` and logged, never fatal.

**Bounded.** Past 2 MB the live file becomes `activity.jsonl.1` (replacing the previous
generation) and a new one starts, so the log never holds much more than 4 MB. A failed append
raises the app's sticky storage alert and the action carries on: a lost line is a lost record, not
a failed action.
