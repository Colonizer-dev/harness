# Backfill: replaying history

A destination starts at "now". **Start from** = `now` (the default) sends only what is written from
the moment it is configured, and `backlog` only reaches back to **Max backlog (days)** — 7 by
default — because the live exporter drops lines older than that rather than send them late. Everything
before the line those two rules draw was written to disk and never left the machine. The backfill
(issue #858) replays it: a second pass over the ledgers the exporter already reads, through the same
add-on, at a rate you choose, with the same ids the live exporter would have produced. It never
touches the live exporter's cursors, so an export running beside it is unaffected.

## Starting one

From the CLI (a client of the running mothership, like the colony commands):

```sh
colonizer observability backfill --since 2026-01-01 --until 2026-06-30 --repo acme/widgets --rate 500 --dry-run
colonizer observability backfill --since 2026-01-01 --until 2026-06-30 --repo acme/widgets --rate 500
```

The same two calls as API routes, both owner-only:

| Route | Does |
| :--- | :--- |
| `POST /api/observability/backfill` | `{since?, until?, repo?, rate?, dry_run?}`. With `dry_run` the answer is the counts summary (below); otherwise `{started: true}`, and the replay runs beside the exporter. 409 when export is off or a backfill is already running; 503 when the add-on is missing or its version does not match. |
| `DELETE /api/observability/backfill` | Cancels the running one: `{cancelled: true}`, or 409 when none is. |

The dates take RFC3339, `YYYY-MM-DDTHH:MM:SS` or `YYYY-MM-DD` (a bare `until` date means that day's
end). Nothing is validated on the mothership: the add-on parses the window, and a bad date is
refused before the run starts — a dry run answers 502, a started run dies at once with the reason
redacted in the mothership's log. `state: "failed"` in the progress file is a run that failed
midway, the reason beside it in `error`.

## The dry run

`--dry-run` counts instead of sending: per signal and per org, the ledger bytes those records come
from, and an estimate of the encoded volume on the wire (OTLP/protobuf, before compression). It sends
nothing and writes nothing — no cursor is committed, no state file appears — so it can be run as
often as the window needs narrowing. With the global `--json` the counts print as the add-on's
summary; the API returns it as the response body.

## What it sends

Every ledger line in the window as the log record the live exporter would have produced — the same
mapping, the same [export policy](export-policy.md), the same `colonizer.record.id` — and each
colony's trace spans, with the same span and trace ids (the ids are derived from the colony id and
the turn or tool call, never generated, so a replay recomputes them exactly; the design's decision 2).

Spend and gateway **metric series are not backfilled**. The exporter's metric points are cumulative
folds over the same ledgers, already delivered by the live exporter; replaying old history would
double-count them. A destination that wants historical spend can compute it from the `colonizer.spend`
log records, which are replayed.

## The window

With no `--until`, the window ends at the live exporter's committed cursors, so live and backfill
never overlap: every record is sent exactly once by one side or the other. A destination whose
**Start from** has not settled yet (first configured, `now` not yet bound to files) bounds the window
at the moment it went live. To be exact — or to re-run a window on purpose — pass `--until` with the
date you enabled export.

## Rate, cancel, resume

The replay sends at most **500 records per second** by default (`--rate`), shared across signals, so
a backfill cannot crowd out the live exporter's batches or trip a backend's rate limit. `DELETE
/api/observability/backfill` (or the mothership closing the child's stdin) stops it within one tick:
the child finishes the batch in flight, writes `state: "cancelled"` and exits.

Progress and the resume point live in `<data>/observability/backfill.json` — the backfill's own
cursor set, committed only after the backend acknowledged a batch, exactly like the live exporter's
`state.json`. A cancelled, failed or interrupted backfill restarts where it stopped, without gaps,
and never moves the live cursors. `GET /api/observability/status` carries the file as `backfill`:
`{state, window, records_sent, bytes_sent, cursors, started_unix, updated_unix, …}` with `state`
one of `running`, `complete`, `cancelled`, `failed` (`error` beside the last).

## Duplicates and backend limits

Re-running a window re-sends records with the identical `colonizer.record.id` and spans with
identical span ids, by design. What happens then is the backend's rule:

- A backend that deduplicates by id (Honeycomb, Datadog) absorbs the replay.
- Loki keys a log line by its labels and timestamp and stores it again — the same line, twice, at
  the same instant; queries return duplicates, dashboards that count lines see double.
- Tempo keys spans by id, so a replayed trace does not multiply its spans.

Old timestamps are the harder limit: Loki rejects samples older than `reject_old_samples_max_age`
(one week by default), and Grafana Cloud applies a similar window to its Loki and Mimir. Backfilling
older than that either lands only in Tempo (traces are keyed by id, not age) or needs the backend
configured to accept old samples first. Check the destination's own window before a long `--since`;
the dry run's counts are the cheapest way to see what a window holds.

Both routes are owner-only: the API token the client commands carry, nothing narrower.
