#!/bin/sh
# Drives scripts/ci/check-rust-file-size.sh, unmodified, against throwaway git repos: it passes
# with test files over the limit, fails naming a non-test file that grew past 2000 lines, passes
# for one listed in the allowlist, and fails when an allowlist entry is stale — a file shrunk back
# under the limit, or one that is gone. A tracked file removed from the worktree is skipped, not an
# error. Runs offline and needs nothing but git and sh.
set -eu

# A colony sandbox exports GIT_DIR/GIT_WORK_TREE/GIT_INDEX_FILE for its own worktree; a test
# driving throwaway repos must not inherit them.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE

repo=$(cd "$(dirname "$0")/../.." && pwd)
check=$repo/scripts/ci/check-rust-file-size.sh

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
log=$scratch/run.log
err=$scratch/run.err

note() { printf '%s\n' "$*"; }

bad() {
  printf 'FAIL: %s\n' "$1"
  echo "--- stdout ($log)"
  cat "$log" 2>&1 || true
  echo "--- stderr ($err)"
  cat "$err" 2>&1 || true
  exit 1
}

# A repo with a local identity, so the test needs no global git config.
mk_repo() { # dir
  mkdir -p "$1"
  (cd "$1" && git init -q -b main . && git config user.email rust-file-size@test \
    && git config user.name "rust-file-size test" && git config commit.gpgsign false)
}

tracked() { # dir; stages everything and commits, so the check sees an index
  (cd "$1" && git add -A && git commit -q -m setup)
}

# Runs the check with the repo as cwd; passes through its exit status.
run_check() { # dir
  (cd "$1" && sh "$check" > "$log" 2> "$err")
}

# Writes `n` numbered lines to a file — a cheap way to make it a known length.
lines() { # n path
  i=1
  while [ "$i" -le "$1" ]; do printf '# line %s\n' "$i"; i=$((i + 1)); done > "$2"
}

note "pass: a small source file and test files of any size are ignored"
good=$scratch/good
mk_repo "$good"
mkdir -p "$good/src" "$good/src/tests" "$good/scripts/ci"
lines 10 "$good/src/small.rs"
lines 5000 "$good/src/tests.rs"
lines 5000 "$good/src/foo_tests.rs"
lines 5000 "$good/src/tests/helper.rs"
: > "$good/scripts/ci/rust-file-size-allowlist.txt"
tracked "$good"
run_check "$good" || bad "the check failed on a clean tree"
grep -q "rust file sizes OK" "$log" || bad "the check passed without its OK line"

note "fail: a non-test file over the limit, named in the output"
big=$scratch/big
mk_repo "$big"
mkdir -p "$big/src" "$big/scripts/ci"
lines 2001 "$big/src/big.rs"
: > "$big/scripts/ci/rust-file-size-allowlist.txt"
tracked "$big"
if run_check "$big"; then
  bad "the check passed on an oversized non-test file"
fi
grep -q "src/big.rs" "$log" || bad "the failure did not name src/big.rs"
grep -q "2001 lines" "$log" || bad "the failure did not report the line count"

note "pass: the same file listed in the allowlist"
allow=$scratch/allow
mk_repo "$allow"
mkdir -p "$allow/src" "$allow/scripts/ci"
lines 2001 "$allow/src/big.rs"
printf '# a big module still to split\nsrc/big.rs\n' > "$allow/scripts/ci/rust-file-size-allowlist.txt"
tracked "$allow"
run_check "$allow" || bad "the check failed on an allowlisted oversized file"

note "fail: an allowlist entry for a file shrunk back under the limit"
shrunk=$scratch/shrunk
mk_repo "$shrunk"
mkdir -p "$shrunk/src" "$shrunk/scripts/ci"
lines 100 "$shrunk/src/big.rs"
printf 'src/big.rs\n' > "$shrunk/scripts/ci/rust-file-size-allowlist.txt"
tracked "$shrunk"
if run_check "$shrunk"; then
  bad "the check passed with a stale allowlist entry for a shrunk file"
fi
grep -q "src/big.rs" "$log" || bad "the stale failure did not name src/big.rs"
grep -q "remove it" "$log" || bad "the stale failure did not say to remove the line"

note "fail: an allowlist entry for a file that no longer exists"
gone=$scratch/gone
mk_repo "$gone"
mkdir -p "$gone/src" "$gone/scripts/ci"
lines 10 "$gone/src/keep.rs"
printf 'src/missing.rs\n' > "$gone/scripts/ci/rust-file-size-allowlist.txt"
tracked "$gone"
if run_check "$gone"; then
  bad "the check passed with an allowlist entry for a missing file"
fi
grep -q "no longer exists" "$log" || bad "the missing-file failure did not say so"

note "pass: a tracked file removed from the worktree is skipped, not an error"
removed=$scratch/removed
mk_repo "$removed"
mkdir -p "$removed/src" "$removed/scripts/ci"
lines 10 "$removed/src/keep.rs"
lines 2001 "$removed/src/gone.rs"
: > "$removed/scripts/ci/rust-file-size-allowlist.txt"
tracked "$removed"
rm "$removed/src/gone.rs"
run_check "$removed" || bad "the check failed on a tracked file missing from the worktree"
grep -q "rust file sizes OK" "$log" || bad "the check passed without its OK line"
if [ -s "$err" ]; then bad "the check wrote to stderr for a missing tracked file"; fi

note "all rust file size check cases passed"
