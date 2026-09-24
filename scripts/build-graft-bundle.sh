#!/bin/sh
# Builds the graft skillset bundle for this machine's architecture inside a node:22-bookworm microVM (Debian
# bookworm, the colony image's glibc, with the toolchain node-gyp needs), then smoke-tests it there. For development: the published bundles come from
# .github/workflows/graft-bundle.yml, which runs the same two scripts on native runners.
#
#   scripts/build-graft-bundle.sh [release]    default release: dev
set -eu
REPO=$(cd "$(dirname "$0")/.." && pwd)
MSB=${MSB:-$REPO/dist/vendor/microsandbox/bin/msb}
release=${1:-dev}
case "$(uname -m)" in arm64|aarch64) arch=aarch64 ;; *) arch=x86_64 ;; esac
out="$REPO/dist/graft"
mkdir -p "$out"
echo "building the graft bundle ($arch, release $release) in a node:22-bookworm microVM..."
"$MSB" run --no-tty -m 4G -c 4 --root-disk 8G \
  -v "$REPO:/repo:ro" -v "$out:/out" \
  node:22-bookworm -- sh -c "
    set -e
    export TMPDIR=/var/tmp
    sh /repo/scripts/graft-bundle/build.sh $arch $release /out
    mkdir -p /var/tmp/smoke && tar -xzf /out/graft-$release-linux-$arch.tar.gz -C /var/tmp/smoke
    sh /repo/scripts/graft-bundle/smoke.sh /var/tmp/smoke/graft
  "
ls -lh "$out/graft-$release-linux-$arch.tar.gz"
