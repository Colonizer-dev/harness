#!/bin/sh
# Builds the Headroom bundle for this machine's architecture inside a node:24-bookworm microVM (the colony
# image's glibc), then smoke-tests it there. For development: the published bundles come from
# .github/workflows/headroom-bundle.yml, which runs the same two scripts on native runners.
#
#   scripts/build-headroom-bundle.sh [release]    default release: dev
set -eu
REPO=$(cd "$(dirname "$0")/.." && pwd)
MSB=${MSB:-$REPO/dist/vendor/microsandbox/bin/msb}
release=${1:-dev}
case "$(uname -m)" in arm64|aarch64) arch=aarch64 ;; *) arch=x86_64 ;; esac
out="$REPO/dist/headroom"
mkdir -p "$out"
echo "building the Headroom bundle ($arch, release $release) in a node:24-bookworm microVM..."
# /tmp in a microVM is a 512 MB tmpfs; the build and the smoke test work under /var/tmp.
"$MSB" run --no-tty -m 6G -c 8 --root-disk 16G \
  -v "$REPO:/repo:ro" -v "$out:/out" \
  node:24-bookworm -- sh -c "
    set -e
    export TMPDIR=/var/tmp
    sh /repo/scripts/headroom-bundle/build.sh $arch $release /out
    mkdir -p /var/tmp/smoke && tar -xzf /out/headroom-$release-linux-$arch.tar.gz -C /var/tmp/smoke
    /var/tmp/smoke/headroom/python/bin/python3 /repo/scripts/headroom-bundle/smoke.py /var/tmp/smoke/headroom
  "
ls -lh "$out/headroom-$release-linux-$arch.tar.gz"
