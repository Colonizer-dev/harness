#!/bin/sh
# Fails when a tracked file that looks executable is not 100755 in the index: a `*.sh` file or a
# file under `scripts/` (at any depth) whose content starts with a `#!` shebang. Agent file tools
# rewrite files without their mode, so a colony commit can silently turn 100755 into 100644 and CI
# then fails with exit 126 (issue #455); this check catches the drop at the index, where the bit
# lives. Files without a shebang (a .mjs run via `node`, a README, a fixture) are ignored,
# whatever their mode. Paths with spaces are safe: the index listing is split on its tab, never
# on whitespace.
set -eu

root=$(git rev-parse --show-toplevel)
cd "$root"

listed=$(mktemp)
trap 'rm -f "$listed"' EXIT INT TERM
git ls-files -s > "$listed"

fail=0
# `git ls-files -s` prints "<mode> <sha> <stage>\t<path>" per line; `cut -f2-` splits on the tab,
# so spaces inside paths survive.
while IFS= read -r line; do
  case $line in
    100755\ *) continue ;;
  esac
  path=$(printf '%s\n' "$line" | cut -f2-)
  case $path in
    *.sh|scripts/*|*/scripts/*) ;;
    *) continue ;;
  esac
  if [ "$(head -c 2 -- "$path")" = '#!' ]; then
    printf 'not executable in the index: %s\n' "$path"
    fail=1
  fi
done < "$listed"

if [ "$fail" -ne 0 ]; then
  echo "FAIL: shebang scripts above lost their executable bit (git update-index --chmod=+x -- <path> to fix)"
  exit 1
fi
echo "exec bits OK"
