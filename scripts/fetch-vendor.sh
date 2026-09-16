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
  # `any` entries are platform-independent (source, not binaries).
  [ "$plat" = "$platform" ] || [ "$plat" = "any" ] || continue
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
    ecc)
      # Staged as a Claude Code plugin directory, not a binary.
      #
      # The whole hooks/ directory is dropped. ECC's plugin manifest sets
      # userConfig.hooks_enabled default true and Claude Code discovers
      # hooks/hooks.json by convention, so "skills and agents only" cannot be
      # expressed as a flag: every ECC hook is a `node -e` bootstrap that spawns
      # first and checks ECC_HOOKS_ENABLED second. Removing the files is the
      # only version of this that is true by construction.
      #
      # Also dropped: docs/ and the per-harness copies under .kiro, .cursor,
      # .opencode and .agents, which duplicate the same skills for other tools.
      tmp=$(mktemp -d)
      tar -xzf "$file" -C "$tmp"
      src=$(echo "$tmp"/ECC-*)
      dest="$out/plugins/ecc"
      rm -rf "$dest"
      mkdir -p "$dest"
      for keep in .claude-plugin skills agents commands scripts LICENSE; do
        [ -e "$src/$keep" ] || { echo "ecc $version has no $keep" >&2; exit 1; }
        cp -R "$src/$keep" "$dest/$keep"
      done
      # Fail loudly rather than shipping hooks by accident.
      if [ -e "$dest/hooks" ]; then echo "ecc staging leaked hooks/" >&2; exit 1; fi
      rm -rf "$tmp"
      ;;
    microsandbox)
      tmp=$(mktemp -d)
      tar -xzf "$file" -C "$tmp"
      mkdir -p "$out/microsandbox/bin" "$out/microsandbox/lib"
      install -m 755 "$tmp/msb" "$out/microsandbox/bin/msb"
      # msb loads libkrunfw by soname from the lib directory beside it, so keep upstream's links.
      lib=$(cd "$tmp" && ls libkrunfw.so.*.*.* libkrunfw.*.dylib 2>/dev/null | head -1)
      [ -n "$lib" ] || { echo "the microsandbox bundle has no libkrunfw" >&2; exit 1; }
      install -m 755 "$tmp/$lib" "$out/microsandbox/lib/$lib"
      case "$lib" in
        *.dylib)
          ln -sf "$lib" "$out/microsandbox/lib/libkrunfw.dylib"
          ;;
        *)
          abi=${lib#libkrunfw.so.}
          abi=${abi%%.*}
          ln -sf "$lib" "$out/microsandbox/lib/libkrunfw.so.$abi"
          ln -sf "libkrunfw.so.$abi" "$out/microsandbox/lib/libkrunfw.so"
          ;;
      esac
      rm -rf "$tmp"
      ;;
  esac
  echo "installed $name $version"
done < "$root/vendor/vendor.lock"

[ "$found" -gt 0 ] || { echo "no vendored binaries pinned for $platform" >&2; exit 1; }

# The DERP relay map is committed (scripts/update-derpmap.sh), so the mesh never fetches it at runtime.
install -m 644 "$root/vendor/derpmap.yaml" "$out/derpmap.yaml"
echo "installed DERP map"
