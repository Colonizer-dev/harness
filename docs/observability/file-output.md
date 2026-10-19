# File output (the `file` provider)

Not every install can send anything anywhere: an air-gapped rack, a lab machine whose only network
is the cockpit's, a site where the collector runs on the same box as the mothership and its
operator walks the data off on a disk. For those, the observability module's second provider,
`file`, writes the same export stream the OTLP provider sends — the same records, the same
[export policy](export-policy.md), the same batching and committed offsets — as lines of OTLP/JSON
in a local directory, and nothing leaves the machine on its account.

It is the same add-on binary doing the writing, under the same supervision: the mothership hands
it a contract with `provider: "file"` and the sink's two settings, and it tails, maps, writes and
commits exactly as it would against a backend. A backend that later reads those files sees one
export request per line, in order, with no gaps and no duplicates within a clean shutdown.

## Where the files land

**Directory** (`dir` in the settings) is relative to the mothership's data dir, empty by default
for `<data>/observability/otlp`. It must stay under the data dir: an absolute path or one climbing
out with `..` is refused at save time and at resolve time alike.

**Total cap** (`max_mb`) bounds everything the sink writes, `512` MiB by default, at least `1`.
When the cap is crossed the oldest sink file is deleted first — and only ever a sink file: nothing
outside the directory is touched, and a file that is not the sink's own (`otlp-*.jsonl`) is never
deleted however big the directory grows.

The directory is `0700` and every file in it `0600`: the export stream carries what the ledgers
carry, redacted by the same [policy](export-policy.md), and needs no wider an audience than the
ledgers have.

## The shape of a file

One line per export request, uncompressed OTLP/JSON — whatever the protocol and compression
settings say, the sink writes JSON, because a file of protobuf blobs is a file nobody can read
without the tool that wrote it. Each line is a complete `Export{Logs,Traces,Metrics}ServiceRequest`
with a trailing newline, so the OpenTelemetry Collector's `otlpjsonfile` receiver can tail the
directory as-is:

```yaml
receivers:
  otlpjsonfile:
    include:
      - /var/lib/colonizer/data/observability/otlp/otlp-*.jsonl
    start_at: end

service:
  pipelines:
    logs:
      receivers: [otlpjsonfile]
      exporters: [otlp/your-backend]
```

Point `include` at wherever `dir` resolves on the machine doing the collecting; `otlp-*.jsonl` is
the whole naming scheme and nothing else is written there.

The files are named `otlp-<signal>-<yyyymmdd>.jsonl` in UTC — `otlp-logs-20260101.jsonl`,
`otlp-traces-20260101.jsonl` — one per signal per day, plus a rotation counter when a day's file
outgrows its cap: `otlp-logs-20260101.1.jsonl`, `.2`, and so on. The per-file cap is the total
eighth, at least 1 MiB (`max_mb` 512 gives 64 MiB files), so a cap of any size yields a handful of
files a day rather than thousands; a single request bigger than the cap still gets a file of its
own, because a request is never split across files.

## Durability and rotation

A line is fsynced before its request is acknowledged: the sink writes the bytes, calls `sync_all`,
and only then does the exporter commit the read offsets that produced it. A crash between the
write and the commit replays the request on restart — at least once, never less — and the cursors
in `state.json` never name bytes that are not on disk.

Rotation happens on the write that would pass the per-file cap, never mid-line, and the total cap
is enforced on the same write: after trimming, the sink's files together are within `max_mb`, with
the oldest deleted first and the file just written always kept.

## The disk guard

A sink that writes until the disk is full would take the mothership's own ledgers down with it, so
the mothership stands guard: every five seconds its supervision loop checks the same free-space
verdict the queue's admission pause uses, and the storage alert the cockpit shows, and while either
says the disk is in trouble it leaves a marker at `<data>/observability/disk-low`. The add-on
checks for that marker before every write and holds the batch when it is there — nothing is
written, nothing is committed, and the signal's health shows `backing_off` with `reason: "disk"`
until the marker lifts, which it does on the first clear tick.

The hold is a pause, not a loss: the ledgers are the spool, and everything written after the lift
resumes from the last committed offset.

## Stopping

The mothership's death ends the add-on's stdin, and the add-on races its stop signal against the
write in flight: a request still being written when the stop lands is cancelled rather than
awaited, nothing further is written, and the restart replays it from the committed offsets. Every
record is exported at least once across a stop, and none is lost; a record whose write completed
before the stop was fsynced, and is not written again.

What the files are worth once they exist — how to read them, move them, or throw them away — is on
[the reset page](reset.md).
