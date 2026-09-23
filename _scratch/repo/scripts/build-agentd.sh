#!/bin/sh
# Builds a fully static (musl) colonizer-agentd inside a rust:1-alpine microVM and installs it to
# dist/bin/colonizer-agentd. The crate is built from an isolated copy so concurrent edits to other
# workspace members can't break or rewrite this build.
#
#   scripts/build-agentd.sh           build
#   scripts/build-agentd.sh --smoke   build, then run it inside a node:24-bookworm microVM
#
# COLONIZER_BUILD_HERE=1 builds in the current environment instead of a microVM, which has to be
# Linux, since this binary runs in a Linux colony; on a non-musl Linux the result links against the
# host's libc, which may be newer than the colony image's (Debian bookworm, glibc 2.36) and fail to
# start there. The release workflow sets it when it is already running inside rust:1-alpine, where no
# microVM can start.
set -eu

# Here mode follows the host, so off Linux it would silently produce a binary no colony can exec, and
# every colony would die on it at the readiness timeout. Refuse before anything is built or moved.
if [ "${COLONIZER_BUILD_HERE:-}" = 1 ] && [ "$(uname -s)" != Linux ]; then
  echo "COLONIZER_BUILD_HERE=1 builds colonizer-agentd for the current environment, but the binary has to run in a Linux colony; unset COLONIZER_BUILD_HERE to build it in the rust:1-alpine microVM instead" >&2
  exit 1
fi

REPO=$(cd "$(dirname "$0")/.." && pwd)
MSB=${MSB:-$HOME/.local/bin/msb}
SRC="$REPO/target/agentd-src"
OUT="$REPO/dist/bin/colonizer-agentd"

rm -rf "$SRC"
mkdir -p "$SRC/crates" "$REPO/target/alpine" "$REPO/target/alpine-cargo-registry" "$REPO/dist/bin"
cp -R "$REPO/crates/colonizer-agentd" "$SRC/crates/"
cp "$REPO/Cargo.lock" "$SRC/Cargo.lock"
cat > "$SRC/Cargo.toml" <<'EOF'
[workspace]
resolver = "3"
members = ["crates/colonizer-agentd"]

[profile.release]
strip = true
EOF

# The build runs inside the microVM, which has the host's architecture, so the target follows the
# machine: x86_64 on a Linux box, aarch64 on Apple Silicon. cargo picks it up from the guest itself.
case "$(uname -m)" in
  arm64|aarch64) target="aarch64-unknown-linux-musl" ;;
  *) target="x86_64-unknown-linux-musl" ;;
esac
if [ "${COLONIZER_BUILD_HERE:-}" = 1 ]; then
  echo "building colonizer-agentd ($target) here..."
  (cd "$SRC" && cargo build --release -p colonizer-agentd --target-dir "$REPO/target/alpine")
else
  echo "building colonizer-agentd ($target) in a rust:1-alpine microVM..."
  "$MSB" run --no-tty -q -m 4G -c 8 \
    -v "$SRC:/src" \
    -v "$REPO/target/alpine:/build-target" \
    -v "$REPO/target/alpine-cargo-registry:/usr/local/cargo/registry" \
    -w /src \
    rust:1-alpine -- sh -c 'apk add --no-cache musl-dev >/dev/null && cargo build --release -p colonizer-agentd --target-dir /build-target'
fi

# Either branch must produce a Linux binary, and rust:1-alpine ships no file(1) to say so, so compare
# the ELF magic directly, which needs only the head and printf busybox always has, before a
# wrong-host artefact can land in dist/bin.
elf_or_die() {
  [ "$(head -c 4 "$1")" = "$(printf '\177ELF')" ] || {
    echo "$1 is not an ELF binary, so it cannot run in a colony" >&2
    exit 1
  }
}
elf_or_die "$REPO/target/alpine/release/colonizer-agentd"

install -m 755 "$REPO/target/alpine/release/colonizer-agentd" "$OUT"
file "$OUT"
ls -lh "$OUT" | awk '{print "size:", $5}'

if [ "${1:-}" = "--smoke" ]; then
  SMOKE="$REPO/target/agentd-smoke"
  rm -rf "$SMOKE" && mkdir -p "$SMOKE"
  printf 'smoke-token\n' > "$SMOKE/token"
  cat > "$SMOKE/session.json" <<'EOF'
{"session_id":"smoke","workspace":"/tmp","listen":"127.0.0.1:7070",
 "agent":{"module":"smoke","command":["sh","-c","echo '{\"type\":\"status\",\"state\":\"idle\"}'; cat >/dev/null"],"env":{}}}
EOF
  echo "smoke test in node:24-bookworm..."
  "$MSB" run --no-tty -q -m 1G -v "$OUT:/opt/colonizer/bin/colonizer-agentd:ro" -v "$SMOKE:/colonizer:ro" node:24-bookworm -- sh -c '
    /opt/colonizer/bin/colonizer-agentd --version
    /opt/colonizer/bin/colonizer-agentd >/tmp/agentd.log 2>&1 </dev/null &
    pid=$!
    for i in $(seq 50); do curl -sf -H "Authorization: Bearer smoke-token" http://127.0.0.1:7070/v1/health && break; sleep 0.1; done
    echo
    echo "unauthenticated: $(curl -s -o /dev/null -w %{http_code} http://127.0.0.1:7070/v1/health)"
    kill "$pid"; wait "$pid" 2>/dev/null; true'
fi
