#!/bin/sh
# Fails when a tracked, non-test Rust source file grows past 2000 lines (issue #825). Big modules
# are split into a directory module with a tests.rs, not kept as one wall of text. Files whose
# counts predate the limit, or that are mid-split, are listed in scripts/ci/rust-file-size-
# allowlist.txt; that list only ever shrinks, so a file that is gone or back under the limit fails
# until its line is removed. Test files (tests.rs, *_tests.rs, anything under a tests/ directory)
# have no limit. Runs from any cwd, like scripts/ci/check-exec-bits.sh.
set -eu

limit=2000
root=$(git rev-parse --show-toplevel)
cd "$root"
allowlist=scripts/ci/rust-file-size-allowlist.txt

allowed=$(mktemp)
tracked=$(mktemp)
over=$(mktemp)
trap 'rm -f "$allowed" "$tracked" "$over"' EXIT INT TERM
# Allowlist entries: comments and blank lines dropped, trailing whitespace trimmed.
if [ -f "$allowlist" ]; then
  sed -e 's/#.*//' -e 's/[[:space:]]*$//' -e '/^[[:space:]]*$/d' "$allowlist" > "$allowed"
else
  : > "$allowed"
fi

# A test file has no size limit: tests.rs, a *_tests.rs, or anything under a tests/ directory.
is_test() {
  case $1 in
    tests.rs|*/tests.rs|*_tests.rs|tests/*|*/tests/*) return 0 ;;
  esac
  return 1
}

# The non-test files currently over the limit, so a stale allowlist entry can be spotted below.
git ls-files '*.rs' > "$tracked"
: > "$over"
while IFS= read -r path; do
  if is_test "$path"; then continue; fi
  # A path staged for deletion but still in the index is not on disk; skip it (the stale
  # allowlist check below still reports an allowlisted entry whose file is gone).
  [ -f "$path" ] || continue
  lines=$(wc -l < "$path" | tr -d ' ')
  if [ "$lines" -gt "$limit" ]; then
    printf '%s\n' "$path" >> "$over"
    if ! grep -Fxq -- "$path" "$allowed"; then
      printf 'over %s lines: %s has %s lines; split it into a directory module (issue #825), do not add it to %s\n' \
        "$limit" "$path" "$lines" "$allowlist"
      fail=1
    fi
  fi
done < "$tracked"

# Every allowlist entry must still be needed, else the list is not shrinking.
while IFS= read -r entry; do
  if grep -Fxq -- "$entry" "$over"; then continue; fi
  if [ -f "$entry" ]; then
    printf 'stale allowlist entry: %s is no longer over %s lines; remove it from %s\n' \
      "$entry" "$limit" "$allowlist"
  else
    printf 'stale allowlist entry: %s no longer exists; remove it from %s\n' "$entry" "$allowlist"
  fi
  fail=1
done < "$allowed"

if [ "${fail:-0}" -ne 0 ]; then
  exit 1
fi
echo "rust file sizes OK"
