#!/bin/sh
# Builds a self-contained Colonizer app directory. Nothing is downloaded at runtime.
#
#   scripts/install.sh             build everything into ./dist (run the harness from the checkout)
#   scripts/install.sh --install   also install to ~/.local/share/colonizer/app and
#                                  link ~/.local/bin/colonizer
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
dist="$root/dist"
install_app=0
[ "${1:-}" = "--install" ] && install_app=1

need() { command -v "$1" >/dev/null 2>&1 || { echo "missing required command: $1" >&2; exit 1; }; }
for c in cargo npm node git gh curl tar sha256sum; do need "$c"; done
msb="${COLONIZER_MSB:-$HOME/.local/bin/msb}"
[ -x "$msb" ] || { echo "microsandbox not found at $msb; install it: curl -fsSL https://get.microsandbox.dev | sh" >&2; exit 1; }
[ -r /dev/kvm ] && [ -w /dev/kvm ] || { echo "/dev/kvm is not accessible; microVMs need KVM" >&2; exit 1; }

echo "==> vendored binaries (pinned, sha256-verified)"
"$root/scripts/fetch-vendor.sh"

echo "==> colonizer-agentd (static musl build inside a microVM)"
"$root/scripts/build-agentd.sh"

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
