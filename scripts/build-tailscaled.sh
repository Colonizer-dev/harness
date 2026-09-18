#!/bin/sh
# Builds tailscale and tailscaled for darwin-arm64 from the source pinned in vendor/vendor.lock and
# installs them to dist/vendor/tailscale/, completing the mesh on a Mac: headscale is a plain download,
# but Tailscale publishes no macOS tailscaled anywhere, so building from source is the only way to get
# one. Go cross-compiles cleanly to darwin/arm64 from any host, and its linker applies the ad-hoc code
# signature (CS_ADHOC|CS_LINKER_SIGNED) that Apple Silicon requires, so the binaries run unsigned.
#
#   scripts/build-tailscaled.sh    build (skipped when dist/vendor/tailscale is already built from this source)
#
# COLONIZER_BUILD_HERE=1 builds in the current environment instead of a microVM, as build-agentd.sh
# and build-rtk.sh do.
set -eu

REPO=$(cd "$(dirname "$0")/.." && pwd)
MSB=${MSB:-$HOME/.local/bin/msb}
OUT="$REPO/dist/vendor/tailscale"
line=$(awk '$1 == "tailscale" && $4 == "source" { print $2, $5, $6 }' "$REPO/vendor/vendor.lock")
[ -n "$line" ] || { echo "vendor/vendor.lock has no tailscale source entry" >&2; exit 1; }
set -- $line "$@"
version=$1 sha=$2
shift 3
archive="$REPO/vendor/cache/tailscale-$(basename "$(awk '$1 == "tailscale" && $4 == "source" { print $6 }' "$REPO/vendor/vendor.lock")")"
[ -f "$archive" ] || { echo "tailscale source missing from $archive: run scripts/fetch-vendor.sh first" >&2; exit 1; }

# The pinned tarball is git-less, so tailscale's own build_dist.sh and mkversion cannot run from it and
# `tailscaled --version` would print the useless "1.102.4-ERR-BuildInfo". These are the stamps
# upstream's own build gives tag v1.102.4, commit bbcd7d1fc2054b9189ebc1531acf74bd880ca0c8; update the
# commit when the pin moves.
short_commit=bbcd7d1fc205
ldflags="-X tailscale.com/version.longStamp=$version-t$short_commit -X tailscale.com/version.shortStamp=$version"

target="darwin-arm64"
# The stamp names the source and the target, so a new pin or another machine rebuilds.
stamp="$REPO/target/tailscale-build/$sha-$target"
if [ -x "$OUT/tailscale" ] && [ -x "$OUT/tailscaled" ] && [ -f "$stamp" ]; then
  echo "tailscale $version ($target) already built"
else
  SRC="$REPO/target/tailscale-src"
  # As build-rtk.sh: when building here the source is extracted outside the repository, so nothing in
  # the checkout can end up inside the build.
  [ "${COLONIZER_BUILD_HERE:-}" != 1 ] || SRC="${TMPDIR:-/tmp}/colonizer-tailscale-src"
  # The module cache (~640MB), build cache (~2.6GB) and go's scratch space all live on real disk under
  # target/ and are mounted in: the microVM's /tmp is a small tmpfs, and keeping the caches there makes
  # a re-run warm, as build-rtk.sh does for cargo's registry.
  rm -rf "$SRC" && mkdir -p "$SRC" "$REPO/target/tailscale-build" "$REPO/target/alpine-go-modcache" \
    "$REPO/target/alpine-go-buildcache" "$REPO/target/alpine-go-tmp" "$REPO/target/alpine-tailscale" "$OUT"
  tar -xzf "$archive" -C "$SRC" --strip-components 1
  if [ "${COLONIZER_BUILD_HERE:-}" = 1 ]; then
    echo "building tailscale $version ($target) here..."
    for cmd in tailscale tailscaled; do
      (cd "$SRC" && CGO_ENABLED=0 GOOS=darwin GOARCH=arm64 \
        GOMODCACHE="$REPO/target/alpine-go-modcache" GOCACHE="$REPO/target/alpine-go-buildcache" \
        GOTMPDIR="$REPO/target/alpine-go-tmp" \
        go build -trimpath -ldflags "$ldflags" -o "$REPO/target/alpine-tailscale/$cmd" "./cmd/$cmd")
    done
  else
    echo "building tailscale $version ($target) in a golang:1-alpine microVM..."
    "$MSB" run --no-tty -q -m 4G -c 8 \
      -v "$SRC:/src" \
      -v "$REPO/target/alpine-tailscale:/build-target" \
      -v "$REPO/target/alpine-go-modcache:/go/pkg/mod" \
      -v "$REPO/target/alpine-go-buildcache:/root/.cache/go-build" \
      -v "$REPO/target/alpine-go-tmp:/gotmp" \
      -w /src \
      golang:1-alpine -- sh -c "
        export CGO_ENABLED=0 GOOS=darwin GOARCH=arm64
        export GOMODCACHE=/go/pkg/mod GOCACHE=/root/.cache/go-build GOTMPDIR=/gotmp
        go build -trimpath -o /build-target/tailscale -ldflags '$ldflags' ./cmd/tailscale &&
        go build -trimpath -o /build-target/tailscaled -ldflags '$ldflags' ./cmd/tailscaled"
  fi
  install -m 755 "$REPO/target/alpine-tailscale/tailscale" "$OUT/tailscale"
  install -m 755 "$REPO/target/alpine-tailscale/tailscaled" "$OUT/tailscaled"
  rm -f "$REPO/target/tailscale-build/"*
  touch "$stamp"
fi
for cmd in tailscale tailscaled; do
  file "$OUT/$cmd"
  ls -lh "$OUT/$cmd" | awk '{print "size:", $5}'
done
