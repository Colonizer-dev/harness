#!/bin/sh
# Downloads the Linux build of Claude Code for the guest and installs it to dist/bin/claude-guest.
#
# A colony is a Linux microVM. On Linux the harness mounts the host's own `claude` binary into it, but
# a Mac's is Mach-O and cannot execute in the guest, so the Linux build of the same agent is fetched
# here — at install time, never at runtime, and verified against Anthropic's own release manifest.
#
# The version is whatever the `stable` channel points at when you install. It is deliberately not
# pinned in vendor.lock: the agent talks to a moving service, and shipping colonies a stale agent is
# worse than following the channel the vendor publishes.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
out="$root/dist/bin/claude-guest"
base="https://downloads.claude.ai/claude-code-releases"

# The guest's architecture, which on a Mac is the host's: an Apple Silicon microVM is aarch64.
case "$(uname -m)" in
  arm64|aarch64) arch="arm64" ;;
  x86_64|amd64) arch="x64" ;;
  *) echo "no Claude Code build for $(uname -m)" >&2; exit 1 ;;
esac
# The colony images are glibc-based (see README), so the glibc build, not the musl one.
platform="linux-$arch"

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

version=$(curl -fsSL --retry 3 "$base/stable")
case "$version" in
  [0-9]*) ;;
  *) echo "unexpected version from $base/stable: $version" >&2; exit 1 ;;
esac

# node is already a prerequisite, and a JSON parser beats a regex over someone else's manifest.
expected=$(curl -fsSL --retry 3 "$base/$version/manifest.json" | node -e '
  let raw = "";
  process.stdin.on("data", (d) => (raw += d)).on("end", () => {
    const entry = JSON.parse(raw).platforms?.[process.argv[1]];
    process.stdout.write(entry?.checksum ?? "");
  });
' "$platform")
case "$expected" in
  [0-9a-f]*) [ ${#expected} -eq 64 ] || expected="" ;;
  *) expected="" ;;
esac
[ -n "$expected" ] || { echo "no checksum for $platform in the $version manifest" >&2; exit 1; }

mkdir -p "$(dirname "$out")"
tmp="$out.part"
echo "fetching Claude Code $version for $platform"
curl -fsSL --retry 3 -o "$tmp" "$base/$version/$platform/claude"
actual=$(sha256_of "$tmp")
if [ "$actual" != "$expected" ]; then
  rm -f "$tmp"
  echo "checksum mismatch for Claude Code $version ($platform)" >&2
  exit 1
fi
chmod 755 "$tmp"
mv -f "$tmp" "$out"
echo "installed Claude Code $version for the guest ($platform)"
