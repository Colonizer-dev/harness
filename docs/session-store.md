# Session storage

Colonies outlive every process that runs them: a mothership restart drops nothing, because the
records and the per-colony evidence live in a store, not in memory. Issue #325 gives that store an
interface — `SessionStore` in `crates/colonizer/src/store.rs` — so the same semantics can later be
served by something other than the local disk. This page is the contract and the reasoning; the
code carries the same statements next to the operations they constrain.

## The interface

Nine operations, each with its consistency promise written into the trait. Every operation is
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
| File modes: `write_private`'s 0600 on `vm/token`, `vm/mesh-authkey` | Local-only, and outside the store API: `write_file` carries no mode and goes by the umask, so those two secrets stay written by `write_private` (as they are today); moving them onto the store first needs a private-write operation. A remote backend protects such bytes by access control, not mode bits. |
| Symlinks | Local-only, never followed when listing; nothing in the contract may require one. |
| No fsync of the parent directory after rename | Local-only (unchanged): a power loss can revert a rename; the next save rewrites the file. |

## What doesn't move

The interface covers the session index and per-session files only. Everything else under the data
dir stays where it is: git state (`worktrees/`, the bare clones in `repos/`) is referenced by
absolute paths inside the records and is no use remotely; mesh state (`mesh/`), headroom, hunters,
maps, memory and plugins are other modules' own stores; the top-level journals (`spend.jsonl`,
`routing.jsonl`, `provider-usage.json`, `provider-quota.json`, `redteam.json`) are outside session
storage. Settings (`orgs.json`, `providers.json`, `modules.json`, `claude-accounts.json`) live in
the config dir. MicroVM disks belong to microsandbox and were never in the data dir.

## Migration and rollback

Moving a colony between stores is copy-then-switch, and the copy only ever reads its source —
rollback is pointing the mothership back at the old store, which never changed:

1. Stop the mothership, so the single-writer rule holds while copying.
2. Dry run: `migrate(src, dst, true)` counts what would move — colonies, files, bytes — and writes
   nothing.
3. Migrate: `migrate(src, dst, false)` copies each session's files and the index last, so every
   failure before the index lands leaves the destination index-less — visibly unfinished, not
   half-written. The destination must be empty; a store that already holds an index or sessions is
   refused (`AlreadyExists`) before anything is written.
4. Verification is part of the call: the session listing, every per-session file listing, and every
   file's bytes are read back out of the destination and compared *before* the index is written;
   the index's own bytes are compared after it lands. A migration that cannot prove itself returns
   an error — the one exception being that final index check, where the destination is left
   holding a written index that says so in the error.
5. Spot-check the destination (open a colony in the UI, resume one), then point the mothership at
   the new store.

A failed migration leaves the source untouched and, short of that final index check, the
destination without an index; just point back, or fix the destination and run `migrate` again —
it is a copy, not a move.

There is no CLI entry point yet: this slice wires `persist_sessions`' index write through the
default `LocalDirStore` and exercises the procedure in tests
(`store::tests::a_migration_round_trips_local_to_memory_and_back_byte_for_byte`,
`a_dry_run_migration_counts_without_writing`, `a_migration_refuses_a_destination_that_already_holds_colonies`,
`a_failed_migration_leaves_the_source_whole_and_the_destination_without_an_index`), with the
reads, the per-session file writes and the command to follow.
