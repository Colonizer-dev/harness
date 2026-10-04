# Disk cleanup

Part of the [Colonizer loops](../loops.md).

Rust and Node builds fill disks; a full one has stopped the mothership before. **Disk cleanup** is a
loop every install has, off until you switch it on. It launches no colony: each run is the
mothership's own housekeeping, so it costs no model spend.

- **Switch it on**: Loops → **Disk cleanup** (its own row above your loops) → the switch. The first
  time, the switch opens the **preview** — what a run would remove right now, path by path with
  sizes, and what it keeps and why — and **Turn on disk cleanup** there enables it. From a terminal:
  `colonizer loop run disk-cleanup --dry-run`, then `colonizer loop enable disk-cleanup` (and
  `colonizer loop disable disk-cleanup`).
- **When it runs**: every hour by default, on any interval the scheduler allows (15 minutes up), and
  **early when free space** on the data dir's volume **is under 15%** (0 turns that off). The
  free-space check runs at most every five minutes, and an early run at most every 15 minutes. Run
  now starts one at once; one run at a time.

What it may clean, each with its own switch in **Settings**:

| Category | Default | What goes |
|---|---|---|
| Build output | on | git-ignored `target/`, `node_modules/`, `.next/` and `dist/` inside the worktrees of finished colonies: merged, closed, nothing to change, or stopped/failed for 7 days (settable) — not while a pull request is still open. The colony stays resumable; a build just starts cold. |
| Worktrees | on | the automatic reclaim's own candidates: pushed colonies past the reclaim retention (`COLONIZER_RECLAIM_RETENTION_HOURS`, 12 h), never keep-worktree, never unpushed work |
| MicroVMs | on | stopped `colonizer-*` microVMs no colony owns. Images are kept: `msb` has no prune that can tell which images a colony still needs |
| Session archives | off | archive bundles older than 30 days (settable), plus oldest-first past an optional size cap — the archive's retention plan, which removes the only copy |
| Host build dirs | off | Cargo `target/` dirs (beside a `Cargo.toml`, carrying Cargo's `CACHEDIR.TAG`) under directories you list, untouched for 3 days — for people who build on the mothership's host |

**Never**: a queued, live, waiting, publishing or parked colony; a colony marked **Keep worktree**; a worktree with uncommitted changes, or — when its work is
not on a pull request — with commits no remote has; anything git tracks; `.git`, `~/.cargo`,
`~/.rustup`, package-manager caches (`.npm`, `.pnpm-store`, `.yarn`, `.cache`), credential stores
(`.ssh`, `.gnupg`, `.aws`, `.docker`, `.config`); and anything outside the data dir's worktrees,
its archive and the paths you list. Symlinks are never followed, and every path is re-checked just
before it goes. A listed path must be absolute, and neither `/` nor your home directory itself.

**Cheap when there is nothing to do.** A worktree's build directories are found once and cached until
the colony changes; the host paths' walk is cached for six hours; both walks are bounded in depth
and in directories visited. An hourly run over unchanged colonies reads the colony list, asks
`msb` for its microVMs and runs `df`.

**What a run leaves**: the loop's **History** lists each run — the bytes freed per category, the
paths, what was kept and why, and what started it (schedule, low disk, or you). Each run is also an
activity line in History (`disk_cleanup.run`). If the disk is still under the trigger afterwards the
loop carries an attention item — "Disk still 94% full after cleanup — 40G in live colonies" — shown
on its row, recorded once as `disk_cleanup.attention`, and cleared by the next run that ends above
the trigger.

**Who may change it**: it is per install, since every user runs their own mothership. Only the
owner (the cockpit, or the owner's API token) sees, switches, previews or runs it; a scoped API
token never does, so the host-level categories are the owner's alone. It cannot be deleted —
switch it off — and there is only ever one.

The API is the loops API with the id `disk-cleanup`: `PUT /api/loops/disk-cleanup` takes
`enabled`, `cadence` and `disk_cleanup` (the settings; left out, they are kept), and
`POST /api/loops/disk-cleanup/run-now` answers the run's report — with `?dry_run=1`, the preview,
which removes nothing ([protocol.md](../protocol/loops.md)).
