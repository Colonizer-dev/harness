- **A swappable session store.** The session index (`sessions.json`) is now written through one
  interface, `SessionStore`, with the on-disk layout as the default backend and a reference
  object-store backend beside it, so the same semantics can later be served off the local disk and
  the per-session files under `data/sessions/<id>/` can follow. Each operation states its
  consistency contract (atomic replace, at-least-once appends deduplicated by `seq` on read, one
  writer per session), ids and file names are validated so host paths like worktrees are refused,
  and `migrate` copies colonies between stores — source untouched, destination verified, empty by
  requirement — with a dry-run mode. The contract, the local assumptions the object-store backend
  surfaced, and the migration procedure are in docs/session-store.md; the reads and per-session
  file writes, and a CLI to run a migration, are follow-ups. ([#325])

[#325]: https://github.com/Colonizer-dev/harness/issues/325
