#!/bin/sh
# Drives scripts/ci/check-exec-bits.sh, unmodified, against throwaway git repos: it passes on a
# clean tree, fails naming a 100644 shebang script (including one whose path has a space), and
# ignores files no shell would exec — a .mjs without a shebang under scripts/, and a .sh without
# one. Runs offline and needs nothing but git and sh.
set -eu

# A colony sandbox exports GIT_DIR/GIT_WORK_TREE/GIT_INDEX_FILE for its own worktree; a test
# driving throwaway repos must not inherit them.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE

repo=$(cd "$(dirname "$0")/../.." && pwd)
check=$repo/scripts/ci/check-exec-bits.sh

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
  (cd "$1" && git init -q -b main . && git config user.email exec-bits@test && git config user.name "exec-bits test" \
    && git config commit.gpgsign false)
}

tracked() { # dir; stages everything and commits, so the check sees an index
  (cd "$1" && git add -A && git commit -q -m setup)
}

# Runs the check with the repo as cwd; passes through its exit status.
run_check() { # dir
  (cd "$1" && sh "$check" > "$log" 2> "$err")
}

note "pass: a clean tree (755 shebang script, 644 non-shebang files)"
good=$scratch/good
mk_repo "$good"
mkdir -p "$good/scripts"
printf '#!/bin/sh\necho ok\n' > "$good/scripts/ok.sh"
chmod 755 "$good/scripts/ok.sh"
printf 'console.log("run via node, no shebang");\n' > "$good/scripts/lib.mjs"
printf 'notes\n' > "$good/README.md"
tracked "$good"
run_check "$good" || bad "the check failed on a clean tree"
grep -q "exec bits OK" "$log" || bad "the check passed without its OK line"

note "pass: a .sh without a shebang may stay 644"
noshebang=$scratch/noshebang
mk_repo "$noshebang"
printf 'echo sourced, never execed\n' > "$noshebang/snippet.sh"
tracked "$noshebang"
run_check "$noshebang" || bad "the check failed on a shebang-less .sh at 644"

note "fail: a 100644 shebang script, named in the output"
badd=$scratch/bad
mk_repo "$badd"
mkdir -p "$badd/scripts"
printf '#!/bin/sh\necho hi\n' > "$badd/scripts/tool.sh"
chmod 644 "$badd/scripts/tool.sh"
printf '#!/usr/bin/env node\nconsole.log("x");\n' > "$badd/scripts/run.mjs"
chmod 644 "$badd/scripts/run.mjs"
tracked "$badd"
if run_check "$badd"; then
  bad "the check passed on a repo with 644 shebang scripts"
fi
grep -q "scripts/tool.sh" "$log" || bad "the failure did not name scripts/tool.sh"
grep -q "scripts/run.mjs" "$log" || bad "the failure did not name scripts/run.mjs"

note "fail: a path with a space, then pass once it is +x"
spaced=$scratch/spaced
mk_repo "$spaced"
mkdir -p "$spaced/scripts"
printf '#!/bin/sh\necho hi\n' > "$spaced/scripts/my tool.sh"
chmod 644 "$spaced/scripts/my tool.sh"
tracked "$spaced"
if run_check "$spaced"; then
  bad "the check passed on a 644 shebang script with a space in its path"
fi
grep -q "my tool.sh" "$log" || bad "the failure did not name the spaced path"
(cd "$spaced" && git update-index --chmod=+x -- "scripts/my tool.sh" && git commit -q -m fix)
run_check "$spaced" || bad "the check failed after the spaced path went 755"

note "all exec-bit check cases passed"
