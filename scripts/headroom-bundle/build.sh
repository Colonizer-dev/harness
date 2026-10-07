#!/bin/sh
# Builds the Headroom bundle for one architecture: a standalone CPython with headroom-ai[proxy] and every
# dependency installed from vendor/headroom/requirements.txt, hash-checked. Colonies mount it read-only at
# /opt/colonizer/headroom when Headroom is switched on (docs/protocol.md, "Token savings").
#
# Run inside Debian bookworm, the colony image's glibc (2.36), so pip picks wheels a colony can load.
# .github/workflows/headroom-bundle.yml runs it in a container; scripts/build-headroom-bundle.sh in a microVM.
#
#   scripts/headroom-bundle/build.sh <x86_64|aarch64> <release, e.g. 0.37.0-1> <output directory>
set -eu
arch=$1 release=$2 out=$3
root=$(cd "$(dirname "$0")/../.." && pwd)
case "$arch" in x86_64|aarch64) ;; *) echo "unknown architecture $arch" >&2; exit 1 ;; esac
[ "$(uname -m)" = "$arch" ] || { echo "building $arch on $(uname -m): pip would pick the wrong wheels" >&2; exit 1; }

line=$(awk -v a="$arch" '$1 == a { print $2, $3, $4 }' "$root/vendor/headroom/python.lock")
[ -n "$line" ] || { echo "vendor/headroom/python.lock has no $arch entry" >&2; exit 1; }
set -- $line
pyversion=$1 pysha=$2 pyurl=$3

work=$(mktemp -d "${TMPDIR:-/var/tmp}/headroom-bundle.XXXXXX")
trap 'rm -rf "$work"' EXIT
export TMPDIR="$work/tmp"
mkdir -p "$TMPDIR" "$work/headroom" "$out"

echo "==> CPython $pyversion ($arch)"
curl -fsSL --retry 3 -o "$work/python.tar.gz" "$pyurl"
echo "$pysha  $work/python.tar.gz" | sha256sum -c - >/dev/null || { echo "CPython checksum mismatch" >&2; exit 1; }
tar -xzf "$work/python.tar.gz" -C "$work/headroom"

echo "==> headroom-ai and dependencies (hash-checked)"
"$work/headroom/python/bin/python3" -m pip install --no-cache-dir --disable-pip-version-check --no-warn-script-location \
  --require-hashes --no-deps -r "$root/vendor/headroom/requirements.txt"
headroom=$("$work/headroom/python/bin/python3" -c 'import importlib.metadata as m; print(m.version("headroom-ai"))')

cat > "$work/headroom/BUNDLE.json" <<JSON
{
  "release": "$release",
  "arch": "$arch",
  "headroom": "$headroom",
  "python": "$pyversion",
  "requirements_sha256": "$(sha256sum "$root/vendor/headroom/requirements.txt" | cut -d' ' -f1)",
  "built_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}
JSON

name="headroom-$release-linux-$arch.tar.gz"
tar -czf "$out/$name" -C "$work" headroom
echo "==> $out/$name"
echo "size $(du -h "$out/$name" | cut -f1)  sha256 $(sha256sum "$out/$name" | cut -d' ' -f1)"
