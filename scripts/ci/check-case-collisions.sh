#!/bin/sh
# Fails when two tracked paths collide on a case-insensitive filesystem (macOS and Windows by
# default), which Linux CI never notices. Two kinds of collision are caught:
#
# - paths, or directory prefixes of them, that differ only in letter case (`Docs/a.md` next to
#   `docs/b.md`): a case-insensitive checkout can hold only one of them;
# - script modules in the same directory whose names differ only in case once the extension is
#   dropped (`FleetColonies.tsx` next to `fleetColonies.ts`, issue #915): an import of
#   `./FleetColonies` tries `FleetColonies.ts` first, which a case-insensitive filesystem answers
#   with `fleetColonies.ts`, so tsc fails on macOS. That one broke the v0.2.0 macOS bundle build.
#
# Reads the index, not the worktree, so it needs nothing beyond git, sort and awk.
set -eu

root=$(git rev-parse --show-toplevel)
cd "$root"

# Each line out of the first awk is "<key>\t<name>": every directory prefix keyed by itself, every
# file keyed by itself and, when it is a script module, also by its path without the extension.
collisions=$(
  git -c core.quotePath=false ls-files |
    awk -F/ '{
      p = $1
      for (i = 2; i <= NF; i++) { print p "\t" p; p = p "/" $i }
      print p "\t" p
      stem = p
      if (sub(/\.(d\.ts|ts|tsx|js|jsx|mjs|cjs|mts|cts)$/, "", stem)) print stem "\t" p
    }' |
    sort -u |
    awk -F'\t' '{
      k = tolower($1)
      if (k in first) {
        if ($1 != first[k]) {
          if (!(k in shown)) { print first_name[k]; shown[k] = 1 }
          print $2
        }
      } else { first[k] = $1; first_name[k] = $2 }
    }' |
    sort -u
)

if [ -n "$collisions" ]; then
  echo "Tracked paths that collide on a case-insensitive filesystem (macOS, Windows):" >&2
  printf '%s\n' "$collisions" | sed 's/^/  /' >&2
  echo "Rename one of each group so the names differ by more than letter case." >&2
  exit 1
fi
echo "No case-only path collisions."
