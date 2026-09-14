#!/bin/sh
# Downloads the pinned third-party binaries from vendor/vendor.lock, verifies their sha256 and
# installs them into dist/vendor. Downloads are cached in vendor/cache.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
platform="$(uname -s | tr '[:upper:]' '[:lower:]')-$(uname -m)"
cache="$root/vendor/cache"
out="$root/dist/vendor"
mkdir -p "$cache" "$out/tailscale"

found=0
while read -r name version plat kind sha url; do
  case "$name" in ''|'#'*) continue ;; esac
  [ "$plat" = "$platform" ] || continue
  found=$((found + 1))
  file="$cache/$(basename "$url")"
  if [ ! -f "$file" ] || ! echo "$sha  $file" | sha256sum -c --quiet - 2>/dev/null; then
    echo "fetching $name $version"
    curl -fsSL --retry 3 -o "$file.part" "$url"
    mv "$file.part" "$file"
  fi
  echo "$sha  $file" | sha256sum -c --quiet - || { echo "checksum mismatch for $name" >&2; rm -f "$file"; exit 1; }
  case "$name" in
    headscale) install -m 755 "$file" "$out/headscale" ;;
    tailscale)
      tmp=$(mktemp -d)
      tar -xzf "$file" -C "$tmp"
      install -m 755 "$tmp"/tailscale_*/tailscale "$out/tailscale/tailscale"
      install -m 755 "$tmp"/tailscale_*/tailscaled "$out/tailscale/tailscaled"
      rm -rf "$tmp"
      ;;
  esac
  echo "installed $name $version"
done < "$root/vendor/vendor.lock"

[ "$found" -gt 0 ] || { echo "no vendored binaries pinned for $platform" >&2; exit 1; }

# The DERP relay map is committed (scripts/update-derpmap.sh), so the mesh never fetches it at runtime.
install -m 644 "$root/vendor/derpmap.yaml" "$out/derpmap.yaml"
echo "installed DERP map"
