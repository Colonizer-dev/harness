#!/bin/sh
# Builds the graft skillset bundle for one architecture: the plugin (manifest, skill, bin/graft wrapper) and a
# runtime/ with @nanonets/graft and its production dependencies installed by `npm ci` from vendor/graft's lock,
# every package integrity-checked. The mothership downloads it when someone asks for the graft skillset
# (docs/skill-packs.md, "Downloadable skillsets") and unpacks it to <data>/plugins/graft.
#
# The bundle carries its own Node 22 (vendor/graft/node.lock, sha256-checked) at node/bin/node, and the
# native grammars are compiled against it: graft 0.19's tree-sitter 0.21 core does not build on Node 24
# (node-guest's major), and several grammars publish no linux-arm64 prebuild (tree-sitter-kotlin none at all).
# Run inside Debian bookworm (the colony image's glibc) with a C++ toolchain and python3 for node-gyp:
# .github/workflows/graft-bundle.yml runs it in node:22-bookworm on native runners,
# scripts/build-graft-bundle.sh in a local microVM.
#
#   scripts/graft-bundle/build.sh <x86_64|aarch64> <release, e.g. 0.19.0-1> <output directory>
set -eu
arch=$1 release=$2 out=$3
root=$(cd "$(dirname "$0")/../.." && pwd)
case "$arch" in x86_64|aarch64) ;; *) echo "unknown architecture $arch" >&2; exit 1 ;; esac
[ "$(uname -m)" = "$arch" ] || { echo "building $arch on $(uname -m): the native grammars would not load" >&2; exit 1; }
case "$arch" in x86_64) platform=linux-x64 ;; aarch64) platform=linux-arm64 ;; esac
line=$(awk -v p="$platform" '$1 == "node" && $3 == p { print $2, $5, $6 }' "$root/vendor/graft/node.lock")
[ -n "$line" ] || { echo "vendor/graft/node.lock has no $platform entry" >&2; exit 1; }
set -- $line
nodeversion=$1 nodesha=$2 nodeurl=$3

work=$(mktemp -d "${TMPDIR:-/var/tmp}/graft-bundle.XXXXXX")
trap 'rm -rf "$work"' EXIT
mkdir -p "$out"
cp -R "$root/scripts/graft-bundle/plugin" "$work/graft"
mkdir -p "$work/graft/runtime"
cp "$root/vendor/graft/package.json" "$root/vendor/graft/package-lock.json" "$work/graft/runtime/"

echo "==> Node $nodeversion ($platform)"
curl -fsSL --retry 3 -o "$work/node.tar.xz" "$nodeurl"
echo "$nodesha  $work/node.tar.xz" | sha256sum -c - >/dev/null || { echo "Node checksum mismatch" >&2; exit 1; }
mkdir -p "$work/node" "$work/graft/node/bin"
tar -xJf "$work/node.tar.xz" -C "$work/node" --strip-components=1
cp "$work/node/bin/node" "$work/graft/node/bin/node"
cp "$work/node/LICENSE" "$work/graft/node/LICENSE"
# npm and node-gyp compile the grammars against this exact Node, whatever node the image has.
export PATH="$work/node/bin:$PATH"
[ "$(node -p process.versions.node)" = "$nodeversion" ] || { echo "the pinned Node is not the one on PATH" >&2; exit 1; }

echo "==> @nanonets/graft and dependencies (npm ci, integrity-checked)"
# CI=1 and DO_NOT_TRACK=1: graft's postinstall records an install event unless either is set.
(cd "$work/graft/runtime" && CI=1 DO_NOT_TRACK=1 npm ci --omit=dev --no-audit --no-fund --loglevel=error)
graft=$(node -p 'require(process.argv[1]).version' "$work/graft/runtime/node_modules/@nanonets/graft/package.json")
cp "$work/graft/runtime/node_modules/@nanonets/graft/LICENSE" "$work/graft/LICENSE"

cat > "$work/graft/BUNDLE.json" <<JSON
{
  "release": "$release",
  "arch": "$arch",
  "graft": "$graft",
  "node": "$nodeversion",
  "lock_sha256": "$(sha256sum "$root/vendor/graft/package-lock.json" | cut -d' ' -f1)",
  "built_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}
JSON

name="graft-$release-linux-$arch.tar.gz"
tar -czf "$out/$name" -C "$work" graft
echo "==> $out/$name"
echo "size $(du -h "$out/$name" | cut -f1)  sha256 $(sha256sum "$out/$name" | cut -d' ' -f1)"
