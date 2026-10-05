# Session storage

Colonies outlive every process that runs them: a mothership restart drops nothing, because the
records and the per-colony evidence live in a store, not in memory. Issue #325 gives that store an
interface — `SessionStore` in `crates/colonizer/src/store.rs` — so the same semantics can later be
served by something other than the local disk. This page is the contract and the reasoning; the
code carries the same statements next to the operations they constrain.

## The interface

Nine core operations, each with its consistency promise written into the trait. Every operation is
async and the trait is object-safe — each method returns a boxed `Send` future, so a caller can
drive any backend, written or not yet written, as `&dyn SessionStore` (which is how `migrate`
works):

| Operation | Promise |
| --- | --- |
| `read_index` | The bytes of `sessions.json`, or `None` before the first write (a first run). A read overlapping a write sees old bytes whole or new bytes whole. |
| `write_index` | Replaces the index whole. `Ok` means every later read sees exactly these bytes; a failure leaves the previous bytes in place. |
| `list_sessions` | Every id with a session, sorted. |
| `read_file` | One per-session file's bytes, `None` if absent. Same visibility rules as the index. |
| `write_file` | Creates or replaces one file whole; the index's guarantees per file. |
| `append` | Adds one line (newline included, like `util::append_line`), creating the file if needed. At least once per writer; readers replay with the `seq` rule below. |
| `list_files` | A session's files, relative and `/`-separated (`vm/token`), sorted, recursive. |
| `remove_session` | Removes a session and everything in it; idempotent. |
| `quarantine_index` | Moves an unusable index aside to `sessions.json.corrupt-<unix-ts>` and returns the name; `None` when there is no index. The stamp has one-second resolution, so an aside from an earlier second is never overwritten. |

Five more operations are derived from those nine: each has a default built from them, so a backend
is correct by implementing the nine alone, and overrides one only to do it cheaper.

| Operation | Promise | Local store |
| --- | --- | --- |
| `read_tail` | The last bytes of a file within a budget, cut forward to a line start, so only whole lines come back; `None` if absent. | Seeks; never reads the whole log. |
| `stat` | A file's length and, where the backend keeps one, its modified time; `None` if absent. | `metadata`. |
| `write_private` | `write_file` for a credential (`vm/token`, `vm/mesh-authkey`, `gateway-token`, `issue.json`). | Owner-only (0600), through `util::write_private`. |
| `remove_file` | Removes one file; idempotent. | `remove_file`. |
| `rename_file` | Renames a file within a session; the default copies then removes, so a crash leaves both, never neither. | `rename`, with the same fault seam the event-log rotation always had. |

Every id and file name is validated before a backend sees it: an id is one plain component, a file
name is relative with only normal components (`vm/token` is fine, `../x`, `/abs`, `C:/x` and
`vm/../token` are not). This is the wall that keeps host paths out of the API — a worktree is an
absolute path, so a worktree can never be mistaken for a key.

## The contract

- **Replace is atomic, everywhere.** Index and file writes are all-or-nothing and visible on
  return. The local store gets this from temp-file-then-rename (`util::write_atomic`); an object
  store gets it from the whole-object put, because objects appear with all their bytes or not at
  all.
- **Appends are at least once, deduplicated on read.** A confirmed append is visible to later
  reads; a crash can lose the last line, never corrupt earlier ones. Because agentd re-sends its
  events after a reconnect, a log may hold the same line twice, and readers replay with the
  `seq` rule: drop any line whose `seq` is at or below the last one seen, **per file** — a resume
  rotates `events.jsonl` to `events-N.jsonl` and the new file's `seq` restarts at 1, so the rule
  never spans the archive.
- **One writer per session.** The mothership that owns a colony is the only process that appends
  to it. This is what makes object-store appends (read-modify-write) safe; a real remote backend
  still does a conditional put on the object's etag to enforce it.
- **Reads may lag writes, by a declared bound.** The reference memory backend lags by zero. A
  remote backend must state its bound; absent a stated bound, assume it is at most the mothership's
  reconnect interval — the time a restarted mothership waits before it looks at the store again.
- **Quarantine preserves.** A corrupt index is moved aside under a forward-moving stamp, and its
  bytes survive until a person decides about them.

## Fresh agents, durable sessions

The store is what makes agents disposable. Nothing an agent process holds is needed to continue a
colony: the record is the index row, the evidence is the session's files, and the conversation is
the event log. Any agent process — the original one, or a fresh one after a mothership restart, or
a different host entirely once backends are remote — attaches by session id and replays from the
log, deduplicating by `seq`. This is the same model that already lets detached colonies survive a
mothership restart and resume on a fresh microVM (architecture.md's session lifecycle, step 6); the
interface only states it as a property of the store instead of an accident of the local disk.

## What the reference backend taught

`MemoryObjectStore` models a flat object namespace — keys `sessions.json` and
`sessions/<id>/<name>` — to prove the nine operations are enough off the local disk. Writing it
surfaced the local layout's implicit assumptions, and each is now either a guarantee of the
interface or a documented local-only behavior:

| Local assumption | What became of it |
| --- | --- |
| Temp file + rename (`write_atomic`) | Guarantee: replace is atomic on every backend, via whole-object put remotely. |
| `O_APPEND` (`append_line`) | Guarantee: appends visible after return; remotely read-modify-write under the single-writer rule + conditional put. |
| Directories under `sessions/<id>/` | Guarantee, reshaped: backends list by key prefix; the on-disk shape stays for the local store. |
| Path shapes (`vm/token`, ids as components) | Guarantee: validated in the interface; worktree paths and traversal are refused. |
| File modes: `write_private`'s 0600 on `vm/token`, `vm/mesh-authkey` | Guarantee, through its own operation: `write_private` writes a credential owner-only on the local store, and a migration copies credentials with it. A remote backend protects such bytes by access control, not mode bits. |
| Symlinks | Local-only, never followed when listing; nothing in the contract may require one. |
| No fsync of the parent directory after rename | Local-only (unchanged): a power loss can revert a rename; the next save rewrites the file. |

## What doesn't move

The interface covers the session index and per-session files only. Everything else under the data
dir stays where it is: git state (`worktrees/`, the bare clones in `repos/`) is referenced by
absolute paths inside the records and is no use remotely; mesh state (`mesh/`), headroom, hunters,
maps, memory and plugins are other modules' own stores, and so are the log archive (`archive/`)
and the answer and HTTP caches (`cache/`); the top-level journals (`spend.jsonl`,
`routing.jsonl`, `provider-usage.json`, `provider-quota.json`, `redteam.json`) are outside session
storage. Settings (`orgs.json`, `providers.json`, `modules.json`, `claude-accounts.json`) live in
the config dir. MicroVM disks belong to microsandbox and were never in the data dir.

## Configuration

The store an install runs on is named in `<config dir>/session-store.json` (`COLONIZER_CONFIG_DIR`,
`~/.config/colonizer` by default):

```json
{ "backend": "local" }
```

No file means `local`: `sessions.json` and `sessions/<id>/` under the data dir, the layout every
release has written, so an existing install reads unchanged. A file the build cannot read stops the
mothership at startup with the file's path in the error, rather than starting it on an empty colony
list while the colonies sit somewhere else. The file has one writer, `colonizer sessions migrate`,
and it writes it only after a copy that verified itself.

Whatever the backend, the data dir keeps a working copy of every session: a microVM mounts its
session's `vm/` and `out/` from a host path, so the backend decides where the records are kept, not
where a colony runs.

## Migration and rollback

Moving colonies between stores is copy-then-switch, and the copy only ever reads its source, so
rollback is pointing the mothership back at the old store, which never changed:

1. Stop the mothership, so the single-writer rule holds while copying. When the source is the
   configured store, the command refuses to run while something is listening on `COLONIZER_BIND`.
2. Dry run: `colonizer sessions migrate --to <store> --dry-run` counts what would move (colonies,
   files, bytes, and how many are already there) and writes nothing.
3. Migrate: `colonizer sessions migrate --to <store>` copies each session's files, then the index
   last, so every failure before the index lands leaves the destination index-less: visibly
   unfinished, not half-written.
4. Verification is part of the call. The session listing, every per-session file listing, and every
   file's SHA-256 are read back out of the destination and compared *before* the index is written;
   the index's own bytes are compared after it lands. The report ends with a checksum: the SHA-256
   of the source's manifest (every file's path and SHA-256, then the index's), the figure two runs
   over the same source agree on.
5. The switch. When the source was the configured store, the copy verified and `--to` is a backend,
   the command records `--to` in `session-store.json`, and the next mothership start runs on it.
   A directory target (`local:<dir>`, or a bare path) is a copy, not a switch: to run on it, point
   the mothership at it with `COLONIZER_DATA_DIR`, as the command says.

`--from` names a source other than the configured store; the setting is then left alone. A store is
named as `local` (this install's local store), `local:<dir>`, or a bare directory path.

**Idempotent and resumable.** The destination may be empty, hold part of this source's copy (an
interrupted run), or hold all of it (a finished run). A file the destination already holds byte for
byte is skipped, a changed one is copied over, and a destination file the source's colony no longer
has is removed, so running the command again after an interruption copies only what is left, and
running it after a finished migration copies nothing and re-verifies. Anything else is someone
else's store: a destination whose index differs from the source's, or that holds a colony the source
does not, is refused (`AlreadyExists`) before anything is written.

**Rollback.** The source is never written. Restore the previous `session-store.json` (or remove the
file, for the local store) and start the mothership.

`colonizer migrate-store --to <dir> [--from <dir>] [--dry-run]` is the older, directory-to-directory
form of the same copy, kept for scripts written against it. It never changes the setting.

## Limits

- **One writer per session.** A store holds no lock of its own; the mothership that owns a colony
  is the only process that writes it, and a migration needs the mothership stopped.
- **What still uses a local path.** Some reads and writes cannot go through the store API, because
  something other than the mothership's own code consumes a path: the microVM mounts `vm/`, `out/`,
  `transcripts/` and the services directory; `gh --body-file` reads `finding-body.md`, `review.md`
  and `pr-body.md`; txcript reads and writes the agent's native transcripts as a directory; msb
  writes memory snapshots; the gateway's request audit (`gateway.jsonl`) is appended from a
  synchronous `Drop` on the request path; `colonizer fleet export` and the fleet drain run with no
  mothership; and the disk measurements walk the working copy. Each is listed, with its count and
  reason, in `LAYOUT_ALLOWLIST` (`crates/colonizer/src/store.rs`), and
  `store::tests::the_session_layout_is_touched_only_through_the_store` fails on any new path into the
  session layout that is neither routed through the store nor listed there.
- **Salvage stays local.** A damaged index's salvage *copy* (`copy_corrupt_aside`) and the
  pre-update backup of `sessions.json` are copies of the local file; the store has no copy
  operation, and moving the original would destroy what the copy is meant to keep.

## Where each operation goes

Every read and write of a session's records goes through the `SessionStore` the mothership opened at
startup (`App::store`):

| What | Through |
| --- | --- |
| The index at startup, and every save | `read_index`, `quarantine_index`, `write_index` (`app.rs`, `sessions/persist.rs`) |
| A colony's runtime at first use: its event and harness logs, and whether it was resumed | `read_file`, `list_files` (`SavedLogs::read`, `sessions/runtime.rs`) |
| The event log: appends, the cockpit's replay, the diagnosis and map tails, history search, the judge's context | `append`, `read_file`, `read_tail`, `stat` |
| Event-log rotation on resume, and the run epoch | `list_files`, `rename_file` (`rotate_events`, `run_epoch`, `lifecycle.rs`) |
| The harness log, findings, GitHub and message ledgers (`harness.jsonl`, `findings.jsonl`, `github.jsonl`, `inbox.jsonl`, `sent.jsonl`) | `append`, `read_file` |
| Commit links, claims, the egress record (`commits.json`, `claims.json`, `egress.json`) | `write_file`, `read_file`, `stat` |
| The stored issue, `vm/session.json`, `vm/boot.sh`, and the archived runs a resume reads | `write_private`, `write_file`, `read_file` (`boot.rs`) |
| The colony's credentials (`vm/token`, `vm/mesh-authkey`, `gateway-token`) | `write_private`, `read_file`, `remove_file` |
| The log archive of a finished colony | `list_files`, `read_file`, `stat` (`archive.rs`) |
| Deleting a colony | `remove_session` |

Tests cover the endpoint here: the conformance suite every backend runs
(`store::tests::the_contract_holds_for_both_reference_backends`, derived operations included), the
migration (`a_migration_round_trips_local_to_memory_and_back_byte_for_byte`,
`a_dry_run_migration_counts_without_writing`,
`an_interrupted_migration_resumes_and_a_finished_one_reruns_idempotently`,
`a_resumed_migration_replaces_changed_files_and_drops_stale_ones`,
`a_migration_refuses_a_destination_that_already_holds_other_colonies`,
`a_failed_migration_leaves_the_source_whole_and_the_destination_without_an_index`,
`a_migration_keeps_credentials_owner_only`), the command
(`store_config::tests::sessions_migrate_dry_runs_copies_reruns_and_leaves_the_setting_for_a_directory`,
`the_migrate_store_command_dry_runs_then_copies`), the checked-in v0.1.9 data dir
(`the_v0_1_9_fixture_reads_and_writes_byte_for_byte`), the startup read
(`app::tests::startup_loads_the_index_through_the_store`), and the static check above.
