# The observability tailer

The lowest layer of the observability exporter (issue #842), in the add-on crate
`crates/colonizer-observability/src/` (`cursor.rs`, `state.rs`). The exporter loop (`exporter.rs`)
drives it over the sources `sources.rs` lists through `tailer.rs`, one cursor per file and signal. This note is for maintainers and operators.

## What it reads

One jsonl file (the *live* file) and, while the reader is still catching up, its single rolled
predecessor (the live file renamed to `<name>.1` at rotation). The reader is synchronous
`std::fs`, meant to run on a `spawn_blocking` thread; it takes no lock of its own.

It consumes whole lines only: a line is not handed on until its newline has been seen, so a line
being written in two parts is simply read once when it is complete. Blank lines are skipped; a line
that does not parse as JSON is counted as malformed and skipped, never fatal. A line longer than
`max_line_bytes` is skipped with one `oversized_line` gap, scanned in bounded chunks so it is never
buffered whole. The rolled file's trailing bytes are its final line even without a newline, so an
oversized trailing fragment there is skipped (with its gap) and the file drains; the live file's
trailing fragment is held until its newline arrives. Each batch yields at most `max_lines` lines and
consumes about `max_bytes` bytes.

## The cursor and the state file

Each stream keeps a cursor: the identity of the file it is in, the byte offset it reached, and an
FNV-1a hash of the last line it consumed. A cursor with no file identity is *unbound*: the next
read starts the live file at 0 with no gap.

Cursors live in `<data_dir>/observability/state.json`, committed by `State::commit` through
`util::write_atomic` at most once a second (or when forced). The file is one JSON object:

```json
{
  "version": 1,
  "cursors": [
    { "destination_hash": "…", "signal": "logs", "relative_path": "…",
      "cursor": { "file_id": { "dev": 1, "ino": 2 }, "offset": 42, "last_key": 123 } }
  ],
  "extra": {}
}
```

`cursors` is an array because JSON object keys must be strings; it is a map in memory, keyed by
destination, signal (`logs`/`traces`/`metrics`) and path, so one signal's outage does not hold back
another's. `extra` is where later work keeps open-span state, so a cursor and its span commit in
one atomic write.

A missing state file is an empty state with no gap. An unknown `version`, an unreadable file or
corrupt JSON is an empty state plus one `state_reset` gap — the reader never panics or fails on it.

## Gap reasons

A batch carries gaps, each with a `reason`:

- `rotated_past` — the cursor's file is gone and neither the live nor the rolled file is it: a
  rotation (or two) happened while the reader was behind, so that file's unread lines are lost.
- `truncated` — the cursor's file is shorter than the cursor: it was truncated in place; reading
  restarts from the top. This applies whether the cursor is in the live file or, after a rotation, in
  the rolled one.
- `deleted` — the live file is missing and the rolled file does not match the cursor. The returned
  cursor is unbound, so repeated calls on the missing file stay silent.
- `oversized_line` — one line was longer than `max_line_bytes` and was skipped.
- `state_reset` — the persisted state was unusable; the reader starts from nothing.

## Many sources at once

`tailer.rs` (#843) reads every source `sources.rs` lists, per tick:

- **Which colonies.** The colony list in `exporter.json`, plus any colony `state.json` still holds a
  read position for. `sessions/` is never walked, so a stray directory is never read. A colony the
  mothership lists is read on the next tick.
- **Fair shares.** The byte budget (a token bucket refilled at **Max read rate**, holding one
  second's worth) and the 20 000-record cap are split equally among the files with something new,
  in passes; whatever a file leaves unused goes to the others in the next pass. The first file
  moves one place each tick.
- **Drained colonies.** A colony whose status is `merged`, `no_changes`, `stopped` or `failed`, whose
  every file has been read to the end and left untouched for a minute, is drained: from then on one
  `stat` of its directory per tick, no file opened, until its status or its directory's mtime
  changes. `pr_opened` and `closed` colonies are never drained (their pull request is watched).
- **Deleted colonies.** A colony whose directory is gone while read positions remain yields one
  `export_gap` (`reason` `deleted`, `colony`, and `archived`: whether `archive/` holds a sidecar of
  it, which a backfill can recover). Its read positions are dropped and it is not looked at again.
- **Backlog.** A line older than **Max backlog (days)** is skipped and counted (`backlog`); each run
  of skipped lines in a file yields one `export_gap` with `reason` `backlog` and the `bytes` and
  `lines` skipped, emitted when the run ends (a line inside the window, or the end of the file).
  A run is tracked in memory, so a restart in the middle of one reports the rest as a second gap.
- **Cursor gaps.** `rotated_past`, `truncated`, `deleted` (a file) and `oversized_line` each yield an
  `export_gap` naming the `file`, besides their `gap_<reason>` count. `state_reset` is counted only.
- **Start position.** The first time a destination (endpoint and protocol) is seen, `start_from =
  now` binds every file that exists to just past its last complete line; `backlog` leaves them
  unbound, so they are read from the top. The destination is then recorded in `state.json`'s
  `extra.destinations`, so a restart never re-seeds and a file created later starts at its first
  line. State from before this setting, which already holds positions for the destination, is
  treated as settled.

## Caveats

On unix the file identity is `(device, inode)`, taken from the open handle, so a rename is followed
and a copy is not. On other platforms it is a best-effort `(length, creation time)` that a rename
preserves but a copy does not distinguish. And a `copy-truncate-refill` that rewrites a file in
place and grows it *past* the cursor's old offset while keeping the same identity is undetectable:
the reader resumes at the old offset and would re-read or skip bytes. Real rotations are renames,
which the identity always catches.
