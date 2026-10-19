# Archived colonies and offline export

Two additions to the observability tooling (issue #859): `--include-archive` reaches colonies that
live only in the [local log archive](../colonies.md#the-log-archive), and `observability export`
writes an export to a file instead of a backend — no network at all.

## Archived colonies

When a colony ends, its session directory is tarred into the archive
(`<data>/archive/<org>/<repo>/<yyyy>/<mm>/`), and once it is deleted from the session store that
bundle is the only copy. `--include-archive` — on `observability backfill` and on
`observability export` — reads those colonies too:

- The **latest revision** of each bundle is read, never an earlier one.
- Only the ledgers the exporter already reads are opened: `events.jsonl`, `harness.jsonl`,
  `gateway.jsonl` and `findings.jsonl`. Credential files in a bundle are never read.
- **Disk wins**: a colony that still exists in the session store is read from disk, and its
  archive revisions are ignored, so a colony is never exported twice.
- Record and span ids are derived from the **bundle's original host id**, so an archived colony's
  records carry the same ids the live exporter would have produced — a backend that deduplicates
  by id still absorbs a replay.
- Archived colonies export **structure only**, never conversation content, unless the colony's
  live session still exists on disk and passes the content gate ([below](#offline-export)).

**Retention:** the archive can be swept under a keep-days or size limit
([#606](https://github.com/Colonizer-dev/harness/issues/606), the Disk cleanup loop's
**Session archives** category, off by default). A sweep that runs between the archiving and the
backfill deletes bundles before they are read: run the backfill first, or lengthen the retention
window, when exporting old history matters.

## Offline export

```sh
colonizer observability export --out colonizer-export.jsonl
colonizer observability export --out /tmp/dir --since 2026-01-01 --until 2026-06-30 --include-archive
colonizer observability export --out export.jsonl --signals traces --content
```

| Flag | Does |
| :--- | :--- |
| `--out FILE\|DIR` | Where to write. An existing directory gets `logs.jsonl` and `traces.jsonl` inside; anything else is one file, created with mode 0600. A relative path is resolved against the CLI's directory before it travels. |
| `--since`, `--until` | The window, the backfill's date forms. |
| `--signals` | `logs`, `traces`, or both comma-separated (the default). |
| `--include-archive` | Also export colonies that live only in the archive ([above](#archived-colonies)). |
| `--content` | Include conversation content, not just structure. |

The file holds the OTLP/JSON requests the exporter would have sent — one JSON request per line,
the same records, attributes and ids, through the same [export policy](export-policy.md) — but
nothing is sent anywhere: the add-on writes the file and exits. The command is a client of the
running mothership like `observability backfill` (`POST /api/observability/export`, owner-only),
and prints the files written and their counts; with the global `--json`, the add-on's summary
(`{files, requests, logs, spans, colonies, archived}`) prints instead.

`--content` includes conversation text — prompts, completions, tool input and output — the most
sensitive thing an export can carry, so the command asks on the terminal first and only a typed
`yes` proceeds; a stdin that is not a terminal never confirms, so a script or a pipe cannot answer
for the person. The flag stands in for the install switch only: an organisation's opt-in and each
colony's sensitivity still gate what leaves, and the export policy's redaction still runs.

**Loading it elsewhere:** each line is one OTLP/JSON HTTP request body, the same bytes a
`POST /v1/logs` or `/v1/traces` would carry, so the file loads anywhere without this machine: a
collector on another side can replay it (its file receiver, where it has one, or one POST per line
to any OTLP/HTTP endpoint — `logs.jsonl` and `traces.jsonl` name their signal), and the file is
also a self-contained support bundle to hand over instead of access to the backend. Keep it as
private as one: mode 0600, conversation text when `--content` was confirmed.
