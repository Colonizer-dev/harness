#!/bin/sh
# Builds a self-contained Colonizer app directory. Nothing is downloaded at runtime.
#
#   scripts/install.sh              build everything into ./dist (run the harness from the checkout)
#   scripts/install.sh --install    also install to ~/.local/share/colonizer/app and
#                                   link ~/.local/bin/colonizer
#   scripts/install.sh --pull-image also download the default colony image now, so the
#                                   first colony boots instead of waiting on a download
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
dist="$root/dist"
install_app=0
pull_image=0
# Opt-in on purpose: the colony image is gigabytes, and an installer that
# downloads that much without being asked is not a good guest on a laptop.
for arg in "$@"; do
  case "$arg" in
    --install) install_app=1 ;;
    --pull-image) pull_image=1 ;;
    *) echo "unknown option: $arg" >&2; exit 1 ;;
  esac
done

need() { command -v "$1" >/dev/null 2>&1 || { echo "missing required command: $1" >&2; exit 1; }; }
for c in cargo npm node git gh curl tar; do need "$c"; done
# GNU calls it sha256sum, macOS ships shasum; either will do.
command -v sha256sum >/dev/null 2>&1 || command -v shasum >/dev/null 2>&1 ||
  { echo "missing required command: sha256sum or shasum" >&2; exit 1; }

# One rule per platform, and nothing pretends to work where it cannot.
case "$(uname -s)" in
  Linux)
    [ -r /dev/kvm ] && [ -w /dev/kvm ] || { echo "/dev/kvm is not accessible; microVMs need KVM" >&2; exit 1; }
    ;;
  Darwin)
    [ "$(uname -m)" = "arm64" ] ||
      { echo "Apple Silicon only: microsandbox's libkrun backend has no x86_64 macOS support" >&2; exit 1; }
    ;;
  *)
    echo "unsupported platform: $(uname -s)" >&2
    exit 1
    ;;
esac

echo "==> vendored binaries (pinned, sha256-verified)"
"$root/scripts/fetch-vendor.sh"

# A colony is a Linux microVM, so the agent binary mounted into it has to be a Linux one. On Linux that
# is the host's own install; a Mac's is Mach-O and cannot run in the guest, so fetch the Linux build.
if [ "$(uname -s)" = "Darwin" ]; then
  echo "==> Claude Code for the guest (Linux build, sha256-verified)"
  "$root/scripts/fetch-agent-binary.sh"
fi

# microsandbox ships with the app, so there is nothing to install separately. COLONIZER_MSB still
# wins, for a host that would rather run its own build.
msb="${COLONIZER_MSB:-$dist/vendor/microsandbox/bin/msb}"
[ -x "$msb" ] || { echo "microsandbox is missing from $msb after vendoring" >&2; exit 1; }
# Every vendored plugin must land where the mothership resolves it (<app>/plugins/<name>).
for plugin in $(awk '$1 !~ /^#/ && $4 == "plugin" { print $1 }' "$root/vendor/vendor.lock"); do
  [ -d "$dist/plugins/$plugin" ] || { echo "vendored plugin $plugin is missing from $dist/plugins after vendoring" >&2; exit 1; }
done

echo "==> colonizer-agentd (static musl build inside a microVM)"
MSB="$msb" "$root/scripts/build-agentd.sh"

echo "==> rtk (static musl build inside a microVM, for colonies that switch on compact command output)"
MSB="$msb" "$root/scripts/build-rtk.sh"

echo "==> agent modules"
mkdir -p "$dist/modules/agents"
for module in "$root"/modules/agents/*/; do
  id=$(basename "$module")
  target="$dist/modules/agents/$id"
  rm -rf "$target"
  mkdir -p "$target"
  (cd "$module" && tar --exclude=./node_modules --exclude=./test -cf - .) | tar -xf - -C "$target"
  if [ -f "$target/package.json" ]; then
    (cd "$target" && npm ci --omit=dev --no-audit --no-fund --silent)
  fi
  echo "installed agent module $id"
done

echo "==> web UI"
(cd "$root/web" && npm ci --no-audit --no-fund --silent && npm run build --silent)
rm -rf "$dist/web"
cp -r "$root/web/dist" "$dist/web"

echo "==> harness"
cargo build --release -p colonizer --manifest-path "$root/Cargo.toml"
mkdir -p "$dist/bin"
install -m 755 "$root/target/release/colonizer" "$dist/bin/colonizer"

if [ "$install_app" = 1 ]; then
  app="$HOME/.local/share/colonizer/app"
  echo "==> installing to $app"
  mkdir -p "$(dirname "$app")"
  rm -rf "$app.new"
  cp -a "$dist" "$app.new"
  rm -rf "$app"
  mv "$app.new" "$app"
  mkdir -p "$HOME/.local/bin"
  ln -sf "$app/bin/colonizer" "$HOME/.local/bin/colonizer"
  echo "installed: run 'colonizer' and open http://127.0.0.1:7878"
else
  echo "built: run '$dist/bin/colonizer' and open http://127.0.0.1:7878"
fi

# The colony image is the one large thing that otherwise arrives lazily, during
# the first launch, with nothing on screen to explain the wait.
if [ "$pull_image" = 1 ]; then
  image=${COLONIZER_IMAGE:-node:24-bookworm}
  echo "==> pulling colony image $image"
  "$dist/vendor/microsandbox/bin/msb" pull "$image"
fi
