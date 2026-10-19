# Turning it off, resetting, uninstalling

Three ways an export stops being what it was, in increasing order of destruction: switched off
(reversible, everything kept), reset (the export starts over, the ledgers untouched), uninstalled
(everything the export ever wrote on this machine goes). The source ledgers — the mothership's own
`activity.jsonl` and friends — are never touched by any of the three.

## Turning it off

There are three switches, and they mean different things:

- **Switch the module off** in the cockpit (Settings → Modules → Observability), or save it
  disabled. The saved settings stay; turning it back on resumes where it left off.
- **`COLONIZER_OBSERVABILITY=off`** (or any value that is not `on`/`true`/`1`/`yes`) in the
  mothership's environment. A kill switch that wins over the saved module: the operator who set
  it meant off, whatever the cockpit says. Unset it to resume.
- **`OTEL_SDK_DISABLED=true`** likewise stops the export. It is the standard OpenTelemetry
  variable; this harness honours it for the same reason any OTTL collector does.
- **Removing the module** from the cockpit's module list is the same as switching it off, minus
  the saved settings.

All three stop the export the same way: the mothership closes the supervised add-on's stdin, the
child finishes the batch in flight, commits its read positions to
`<data>/observability/state.json` and exits — within one tick of the change (the supervisor
re-reads the config every five seconds). Nothing more is sent.

**The cursors are kept.** Turning it off and on again resumes where the export stopped, subject
to the [backlog guard](backfill.md): records older than **Max backlog (days)** — 7 by default —
are dropped rather than sent late, so a long off period means those records reach the destination
through a [backfill](backfill.md), not through the live exporter. Re-enabling with **Start
from** unsettled settles it as if it were new: `now` seeds at the files' ends, `backlog` reads
within the guard.

## Resetting

A reset forgets where the export got to, so it starts over:

```sh
colonizer observability reset           # lists what will go, asks y/N, then deletes
colonizer observability reset --yes     # no prompt
colonizer observability reset --hash-key  # also drops the pseudonymising key
```

The CLI always runs a dry run first — one line per file, with its size, as the mothership
measured it — then asks `y/N` (unless `--yes`). When stdin is not a terminal and `--yes` was not
given, it refuses with exit code 2 rather than delete what no one confirmed: a script must say
`--yes` out loud.

The same call as an API route, **owner-only** (never in a token's scope):

| Route | Does |
| :--- | :--- |
| `POST /api/observability/reset` | `{dry_run?, hash_key?}`. With `dry_run` the answer is `{dry_run: true, delete: [{path, bytes}, …]}` and nothing is deleted; otherwise `{reset: true, deleted: [{path, bytes}, …]}`. 409 while a [backfill](backfill.md) is registered — it reads `state.json` to know where history ends. |

What a reset deletes, all of it and only it:

- `<data>/observability/state.json` — the live exporter's committed cursors.
- With `--hash-key`, `<data>/observability/hash.key` too, so records re-sent after the reset are
  pseudonymised with a **fresh** key and cannot be correlated with the ones sent before it (see
  [export policy](export-policy.md) for what is hashed). Without the flag the key is kept, so
  hashed org and repo names stay stable across the reset.

It never touches the source ledgers, the file sink's output (`<data>/observability/otlp/`, see
[file output](file-output.md)), `status.json`, `backfill.json`, the contract, the logs, or anything
else under `<data>`. The route stops the supervised child first — it holds the cursors in memory
and would rewrite `state.json` over the deletion — and the supervisor restarts it on its next
tick (within five seconds). A child that finds no `state.json` starts as if its destination were
new: **Start from** = `now` (the default) seeds every file at its end, `backlog` re-reads within
**Max backlog (days)**; the settle is per destination and survives the reset.

## Uninstalling

To remove the export from a machine for good:

1. Turn it off (above) and let the child stop — `GET /api/status`'s `observability` field reads
   `off` when it has, or watch `GET /api/observability/status`.
2. Delete `<data>/observability/` — the contract, the state, the status file, the add-on's
   downloads under `addons/`, and, if the [file sink](file-output.md) was used, its output.

The source ledgers are the mothership's own records and stay where they are. A custom file-sink
directory (the **Directory** setting) was named by you and is not under `<data>/observability/`:
delete it separately, knowing it holds everything the sink wrote.
