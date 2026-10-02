**A built-in Disk cleanup loop keeps builds from filling the disk, off until you switch it on.**
Every install now has a **Disk cleanup** row on the Loops page. Switched on — the first time, after a
preview that lists what a run would remove, path by path with sizes — it runs every hour and early
whenever free space drops under 15%, removing git-ignored build output (`target/`, `node_modules/`,
`.next/`, `dist/`) from finished colonies' worktrees, the worktrees the automatic reclaim would take,
and microVMs no colony owns; old session archives and Cargo `target/` dirs under paths you list are
opt-in. It never touches live or waiting colonies, keep-worktree colonies, uncommitted or unpushed
work, `.git`, `~/.cargo`, package caches or anything outside its roots. Each run records what it
freed per category in the loop's history and in History, and a run that leaves the disk under the
threshold raises an attention item. From a terminal: `colonizer loop run disk-cleanup --dry-run`
and `colonizer loop enable disk-cleanup`.
