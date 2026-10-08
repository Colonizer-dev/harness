# Mothership log

The mothership writes down what its own process has to say, as redacted JSON lines in
`<data>/logs/mothership.jsonl` (issue
[#856](https://github.com/Colonizer-dev/harness/issues/856)). This is the [observability](observability.md)
add-on's tail source for the harness itself, and it is what a support answer about a *mothership* —
as opposed to a colony — reads.

A colony's turns are not here. They are in that colony's own session log and in `activity.jsonl`;
what lands here is the process: a provider went out of quota, a fleet file did not parse, a mesh
policy could not be written, the storage alert fired. Those used to be `eprintln!`s that vanished
when the terminal closed, and the exporter could not see them at all.

## The line

One JSON object per line, with these five keys in this order:

```json
{"ts":"2026-10-08T09:12:33.481Z","level":"error","target":"colonizer::push","message":"push: the subscription list could not be read (EACCES); nothing sent","fields":{"error":"EACCES"}}
```

- `ts` — when the event was emitted, RFC 3339 with milliseconds, UTC.
- `level` — `error`, `warn`, `info`, `debug` or `trace`, lowercase.
- `target` — the Rust module the line came from, which is the file you grep for.
- `message` — the sentence an operator reads, and the only part the terminal shows.
- `fields` — every structured field beside the message, as a JSON object. Numbers and booleans
  stay scalars (`"bytes":4096`), strings stay strings, and an event with no fields writes `{}`.

## The file

- **Location** — `<data>/logs/mothership.jsonl`, under the data dir the rest of the state is in.
- **Rotation** — at **8 MB** the live file becomes `mothership.jsonl.1`, replacing whatever was
  there, and a new live file starts. One generation only: **at most two files ever exist**, and the
  pair stays under 16 MB. Reading both in order (`.1` first, then the live file) gives the whole
  window; the [tailer](tailer.md) already handles a rotation the same way.
- **Redaction** — every line goes through the same secret redaction the activity log uses before
  it lands. A provider key a gateway echoed, a token in an upstream's error, and the line reads
  `[REDACTED:…]` instead. Nothing that would have been kept out of `activity.jsonl` can reach this
  file.
- **Failing to write** is not fatal and is not logged. An append failure is precisely the case
  where logging through the same broken file would flood it, so the writer retries on the next
  line and says nothing.

## The terminal

The same events also go to **stderr**, and there they are the message and nothing else: no
timestamp, no level, no target, no colour. **At the default filter** every line that used to be an
`eprintln!` appears byte-identical to how it did, so nothing an operator reads out of a terminal at
the default has changed. `COLONIZER_LOG` is the one difference, and it is the point of the knob: it
can suppress a migrated `warn!`/`info!` that the old `eprintln!` would always have printed, which
is how a log gets quieter on purpose.

```sh
COLONIZER_LOG=debug colonizer serve      # more than the default
COLONIZER_LOG=warn colonizer serve       # less
```

`COLONIZER_LOG` takes [`RUST_LOG`](https://docs.rs/env_logger/latest/env_logger/#enabling-logging)
syntax and applies to both layers at once — the file and the terminal always carry the same set of
lines. The default is **`info`**, and it is a floor rather than a starting value: unset, empty
(`COLONIZER_LOG=`), or whitespace-only all leave the default in place, so the log cannot be blanked
by a variable that was set to nothing. Parsing is lossy **per directive**, so
`COLONIZER_LOG=debug,bogus!!` keeps `debug` and reports the ignored one on stderr instead of
throwing the whole value away. A value that names a level or a target is obeyed as written — a bare
word is `RUST_LOG`'s shorthand for "everything for that target", so `COLONIZER_LOG=push` really does
quieten everything but that module.

## Logging never blocks

The thread that emits a line does not touch the disk. It hands the line to a bounded channel of
**4096** and returns; one dedicated writer thread drains it, redacts, rolls and appends.

When the channel is full the line is **dropped and counted**, never waited on. A log call is made
from the middle of a request or a colony's boot path, and a slow disk must not become a slow
gateway — so back-pressure resolves the wrong way on purpose. A run that dropped anything says so
on the way out (`… log lines were dropped: the log writer could not keep up`), which is the one
place a dropped line is worth a line of its own.

## What is never logged

This module — and `colonizer::observability` as a whole — targets **log state changes only**: the
provider went out of quota, the policy was not written, the subscription list could not be read.
**Never a per-record line.** A backend that is failing must not be able to flood the file it is
being shipped, because the flood would bury the one line that says why it is failing. A loop that
wants to say something once per item logs its outcome, its rate or its count — not each item.

## CLI subcommands write nothing

The subscriber is installed from `serve()` and nowhere else. `colonizer setup`, `colonizer update`
— anything that only inspects a data directory leaves no `logs/` directory and no log file
behind it.