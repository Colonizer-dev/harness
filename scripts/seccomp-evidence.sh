#!/bin/sh
# Runs CMD under strace and reports which syscalls colonizer-agentd's runner seccomp profile would
# have rejected outright: the no-regression evidence for the in-guest runner hardening
# (crates/colonizer-agentd/src/harden.rs), meant for a test colony terminal on every
# vendor/claude-code.lock, images.lock or runner change. Exits 1 when such a call was made, so a
# lock bump that starts probing io_uring again cannot land unnoticed. The arg-gated syscalls
# (clone/prctl/ioctl) and the clone3 ENOSYS override are excluded: every multithreaded process
# makes those, and the profile lets them through unless the dangerous argument is present.
#
#   scripts/seccomp-evidence.sh -- node --version
#   scripts/seccomp-evidence.sh --agentd dist/bin/colonizer-agentd -- npm ci
set -eu

agentd=colonizer-agentd
if [ "${1:-}" = "--agentd" ]; then
  agentd=$2
  shift 2
fi
[ "${1:-}" = "--" ] && shift

trace=$(mktemp)
denied=$(mktemp)
made=$(mktemp)
trap 'rm -f "$trace" "$denied" "$made"' EXIT INT TERM

status=0
strace -f -qq -o "$trace" -- "$@" || status=$?

"$agentd" --seccomp-profile | python3 -c 'import json, sys
profile = json.load(sys.stdin)
gated = {r["syscall"] for r in profile["conditional"]}
gated |= {r["syscall"] for r in profile["errno_overrides"]}
for name in sorted(set(profile["deny"]) - gated):
    print(name)' > "$denied"
# Under -f every line starts with the pid; keep the syscall name of each entered call.
sed -E 's/^[0-9]+ +//' "$trace" | grep -oE '^[a-zA-Z0-9_]+\(' | tr -d '(' | sort -u > "$made" || true

hits=$(comm -12 "$denied" "$made")
if [ -n "$hits" ]; then
  echo "seccomp-evidence: rejected syscalls made by: $*"
  echo "$hits" | sed 's/^/  /'
else
  echo "seccomp-evidence: no rejected syscalls made by: $*"
fi
[ "$status" -eq 0 ] || echo "seccomp-evidence: note: CMD exited $status" >&2
[ -z "$hits" ]
