#!/bin/sh
# Builds a fully static (musl) rtk from the source pinned in vendor/vendor.lock, inside a rust:1-alpine
# microVM, and installs it to dist/bin/rtk for colonies that switch on compact command output.
#
# Built rather than downloaded because upstream's aarch64 Linux release is linked against glibc 2.39,
# newer than the colony image's (Debian bookworm, 2.36), so it would not start in a colony on Apple
# Silicon or any ARM host. A static musl build runs in any colony image, on either architecture.
#
#   scripts/build-rtk.sh           build (skipped when dist/bin/rtk is already built from this source)
#   scripts/build-rtk.sh --smoke   build, then run it inside a node:24-bookworm microVM
#
# COLONIZER_BUILD_HERE=1 builds in the current environment instead of a microVM, as build-agentd.sh does.
set -eu

REPO=$(cd "$(dirname "$0")/.." && pwd)
MSB=${MSB:-$HOME/.local/bin/msb}
OUT="$REPO/dist/bin/rtk"
line=$(awk '$1 == "rtk" && $4 == "source" { print $2, $5, $6 }' "$REPO/vendor/vendor.lock")
[ -n "$line" ] || { echo "vendor/vendor.lock has no rtk source entry" >&2; exit 1; }
set -- $line "$@"
version=$1 sha=$2
shift 3
archive="$REPO/vendor/cache/rtk-$(basename "$(awk '$1 == "rtk" { print $6 }' "$REPO/vendor/vendor.lock")")"
[ -f "$archive" ] || { echo "rtk source missing from $archive: run scripts/fetch-vendor.sh first" >&2; exit 1; }

case "$(uname -m)" in
  arm64|aarch64) target="aarch64-unknown-linux-musl" ;;
  *) target="x86_64-unknown-linux-musl" ;;
esac
# The stamp names the source and the target, so a new pin or another machine rebuilds.
stamp="$REPO/target/rtk-build/$sha-$target"
if [ -x "$OUT" ] && [ -f "$stamp" ]; then
  echo "rtk $version ($target) already built"
else
  SRC="$REPO/target/rtk-src"
  # Built in place, the source has to sit outside this repository: under it, cargo would take rtk for a
  # member of the harness workspace and refuse to build it.
  [ "${COLONIZER_BUILD_HERE:-}" != 1 ] || SRC="${TMPDIR:-/tmp}/colonizer-rtk-src"
  rm -rf "$SRC" && mkdir -p "$SRC" "$REPO/target/rtk-build" "$REPO/target/alpine-rtk" "$REPO/target/alpine-cargo-registry" "$REPO/dist/bin"
  tar -xzf "$archive" -C "$SRC" --strip-components 1
  if [ "${COLONIZER_BUILD_HERE:-}" = 1 ]; then
    echo "building rtk $version ($target) here..."
    (cd "$SRC" && cargo build --release --locked --target-dir "$REPO/target/alpine-rtk")
  else
    echo "building rtk $version ($target) in a rust:1-alpine microVM..."
    "$MSB" run --no-tty -q -m 4G -c 8 \
      -v "$SRC:/src" \
      -v "$REPO/target/alpine-rtk:/build-target" \
      -v "$REPO/target/alpine-cargo-registry:/usr/local/cargo/registry" \
      -w /src \
      rust:1-alpine -- sh -c 'apk add --no-cache musl-dev >/dev/null && cargo build --release --locked --target-dir /build-target'
  fi
  install -m 755 "$REPO/target/alpine-rtk/release/rtk" "$OUT"
  rm -f "$REPO/target/rtk-build/"*
  touch "$stamp"
fi
file "$OUT"
ls -lh "$OUT" | awk '{print "size:", $5}'

if [ "${1:-}" = "--smoke" ]; then
  echo "smoke test in node:24-bookworm..."
  "$MSB" run --no-tty -q -m 1G -v "$OUT:/opt/colonizer/bin/rtk:ro" node:24-bookworm -- sh -c '
    /opt/colonizer/bin/rtk --version
    printf "rewrite(git status) -> "; /opt/colonizer/bin/rtk rewrite "git status"; echo " [exit $?]"
    printf "rewrite(echo hi)    -> "; /opt/colonizer/bin/rtk rewrite "echo hi"; echo " [exit $?]"'
fi
