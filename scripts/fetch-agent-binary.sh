#!/bin/sh
# Downloads the Linux build of Claude Code for the guest and installs it to dist/bin/claude-guest.
#
# A colony is a Linux microVM. On Linux the harness mounts the host's own `claude` binary into it, but
# a Mac's is Mach-O and cannot execute in the guest, so the Linux build of the same agent is fetched
# here — at install time, never at runtime, and verified against the sha256 pinned beside its version.
#
# The version and checksum come from vendor/claude-code.lock, so every install of the same Colonizer
# release gets the same agent. scripts/update-runtime-pins.mjs checks Anthropic's stable channel daily
# and proposes a new version by pull request; a human merges it. That bounds how stale the agent can
# get without giving up a reproducible release — the same trade the vendored plugins make.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
out="$root/dist/bin/claude-guest"

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

# One row per guest platform. The lock names platforms the way Anthropic does (linux-arm64,
# linux-x64), not the way Colonizer's release platforms are named — see the header there. Reading it
# needs nothing but the shell, where the manifest parse this replaces pulled in node.
version="" expected="" url=""
while read -r name ver plat kind sha pinned_url; do
  case "$name" in ''|'#'*) continue ;; esac
  [ "$plat" = "$platform" ] || continue
  [ "$kind" = "agent" ] || continue
  # A row with too few columns matches here but pins no url; skip it so a
  # well-formed row for the same platform further down still gets found.
  [ -n "$pinned_url" ] || continue
  version=$ver expected=$sha url=$pinned_url
  break
done < "$root/vendor/claude-code.lock"
[ -n "$url" ] || { echo "no Claude Code build pinned for $platform in vendor/claude-code.lock" >&2; exit 1; }

mkdir -p "$(dirname "$out")"
tmp="$out.part"
echo "fetching Claude Code $version for $platform"
curl -fsSL --retry 3 -o "$tmp" "$url"
actual=$(sha256_of "$tmp")
if [ "$actual" != "$expected" ]; then
  rm -f "$tmp"
  echo "checksum mismatch for Claude Code $version ($platform)" >&2
  exit 1
fi
chmod 755 "$tmp"
mv -f "$tmp" "$out"
echo "installed Claude Code $version for the guest ($platform)"
