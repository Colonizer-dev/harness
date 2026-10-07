#!/bin/sh
# Downloads the Linux Node.js runtime for the guest and installs bin/node to dist/bin/node-guest.
#
# A colony is a Linux microVM whose preset image is node:24-bookworm, and the guest runtime mounted
# into it has to be a Linux build of the same Node 24 LTS line. On Linux that could be the host's
# own node, but a Mac's is Mach-O and cannot execute in the guest, so the Linux build is fetched
# here — at install time, never at runtime, and verified against the sha256 pinned beside its
# version before anything is extracted.
#
# The version and checksum come from vendor/node.lock, so every install of the same Colonizer
# release gets the same runtime. The lock pins the nodejs.org tarball, and the tarball's checksum
# is verified before bin/node is extracted from it — the extracted file itself has no pin.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
out="$root/dist/bin/node-guest"

# The guest's architecture, which on a Mac is the host's: an Apple Silicon microVM is aarch64.
case "$(uname -m)" in
  arm64|aarch64) arch="arm64" ;;
  x86_64|amd64) arch="x64" ;;
  *) echo "no Node.js runtime pinned for $(uname -m)" >&2; exit 1 ;;
esac
# The colony images are glibc-based (see README), and nodejs.org publishes glibc-linked Linux builds.
platform="linux-$arch"

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# One row per guest platform. The lock names platforms the way nodejs.org does (linux-arm64,
# linux-x64), not the way Colonizer's release platforms are named — see the header there. Reading it
# needs nothing but the shell.
version="" expected="" url=""
while read -r name ver plat kind sha pinned_url; do
  case "$name" in ''|'#'*) continue ;; esac
  [ "$plat" = "$platform" ] || continue
  [ "$kind" = "runtime" ] || continue
  # A row with too few columns matches here but pins no url; skip it so a
  # well-formed row for the same platform further down still gets found.
  [ -n "$pinned_url" ] || continue
  version=$ver expected=$sha url=$pinned_url
  break
done < "$root/vendor/node.lock"
[ -n "$url" ] || { echo "no Node.js runtime pinned for $platform in vendor/node.lock" >&2; exit 1; }

mkdir -p "$(dirname "$out")"
tmp_tar="$out.tar.xz.part"
echo "fetching Node.js $version for $platform"
curl -fsSL --retry 3 -o "$tmp_tar" "$url"
actual=$(sha256_of "$tmp_tar")
if [ "$actual" != "$expected" ]; then
  rm -f "$tmp_tar"
  echo "checksum mismatch for Node.js $version ($platform)" >&2
  exit 1
fi
# The tarball is verified above, before anything below trusts its contents. Only bin/node leaves
# the unpack directory; the rest of the tarball (npm, headers, docs) never reaches the guest.
tmpdir="$out.unpack"
rm -rf "$tmpdir" && mkdir -p "$tmpdir"
entry=$(tar -tf "$tmp_tar" | grep '/bin/node$' | head -n 1)
[ -n "$entry" ] || { rm -rf "$tmpdir" "$tmp_tar"; echo "Node.js $version ($platform) has no bin/node" >&2; exit 1; }
tar -xJf "$tmp_tar" -C "$tmpdir" "$entry"
tmp="$out.part"
mv -f "$tmpdir/$entry" "$tmp"
rm -rf "$tmpdir" "$tmp_tar"
chmod 755 "$tmp"
mv -f "$tmp" "$out"
echo "installed Node.js $version for the guest ($platform)"
