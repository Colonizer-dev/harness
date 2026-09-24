#!/bin/sh
# Builds a self-contained Colonizer app directory. Nothing is downloaded at runtime.
#
#   scripts/install.sh              build everything into ./dist (run the harness from the checkout)
#   scripts/install.sh --install    also install to ~/.local/share/colonizer/app and
#                                   link ~/.local/bin/colonizer
#   scripts/install.sh --pull-image also download the default colony image now, so the
#                                   first colony boots instead of waiting on a download
#   scripts/install.sh --bundle     build ./dist for a prebuilt release (.github/workflows/release.yml)
#
# A bundle is built on one machine and run on another, so --bundle skips the KVM check and the Claude Code
# fetch (scripts/install-release.sh fetches it where the app is installed). Binaries already in
# $COLONIZER_PREBUILT (colonizer, colonizer-agentd, rtk) are used as they are instead of being built:
# the release workflow builds the Linux ones as static musl binaries inside rust:1-alpine.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
dist="$root/dist"
install_app=0
pull_image=0
bundle=0
prebuilt=${COLONIZER_PREBUILT:-}
# Opt-in on purpose: the colony image is gigabytes, and an installer that
# downloads that much without being asked is not a good guest on a laptop.
for arg in "$@"; do
  case "$arg" in
    --install) install_app=1 ;;
    --pull-image) pull_image=1 ;;
    --bundle) bundle=1 ;;
    *) echo "unknown option: $arg" >&2; exit 1 ;;
  esac
done

need() { command -v "$1" >/dev/null 2>&1 || { echo "missing required command: $1" >&2; exit 1; }; }
# The build needs these; gh is for running the app, and a bundle is not run where it is built.
for c in npm node git curl tar; do need "$c"; done
[ "$bundle" = 1 ] || need gh
[ -n "$prebuilt" ] && [ -x "$prebuilt/colonizer" ] || need cargo
# GNU calls it sha256sum, macOS ships shasum; either will do.
command -v sha256sum >/dev/null 2>&1 || command -v shasum >/dev/null 2>&1 ||
  { echo "missing required command: sha256sum or shasum" >&2; exit 1; }

# One rule per platform, and nothing pretends to work where it cannot.
case "$(uname -s)" in
  Linux)
    [ "$bundle" = 1 ] || { [ -r /dev/kvm ] && [ -w /dev/kvm ]; } || { echo "/dev/kvm is not accessible; microVMs need KVM" >&2; exit 1; }
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
# (There is no host fallback for it on a Mac, and none needed on Linux.)
if [ "$(uname -s)" = "Darwin" ] && [ "$bundle" = 0 ]; then
  echo "==> Claude Code for the guest (Linux build, sha256-verified)"
  "$root/scripts/fetch-agent-binary.sh"
fi

# The Node runtime has no host fallback on any host — unlike the agent binary above, which Linux
# reuses from the host install. boot.rs mounts dist/bin/node-guest into every colony whose agent
# command starts with `node` and fails the boot when it is missing, so the pinned Linux build is
# fetched on Darwin and Linux alike: at install time, never at runtime.
if [ "$bundle" = 0 ]; then
  echo "==> Node.js for the guest (Linux build, sha256-verified)"
  "$root/scripts/fetch-node-binary.sh"
fi

# Both locks ride in dist/: the installed app reads them back at install time (install-release.sh
# fetches the guest agent against claude-code.lock and the guest runtime against node.lock) and at --pull-image time (the image digest in
# images.lock), so a release has to carry its pins to stay reproducible.
mkdir -p "$dist"
install -m 644 "$root/vendor/claude-code.lock" "$dist/claude-code.lock"
install -m 644 "$root/vendor/node.lock" "$dist/node.lock"
install -m 644 "$root/crates/colonizer/images.lock" "$dist/images.lock"

# microsandbox ships with the app, so there is nothing to install separately. COLONIZER_MSB still
# wins, for a host that would rather run its own build.
msb="${COLONIZER_MSB:-$dist/vendor/microsandbox/bin/msb}"
[ -x "$msb" ] || { echo "microsandbox is missing from $msb after vendoring" >&2; exit 1; }
# Every vendored plugin must land where the mothership resolves it (<app>/plugins/<name>).
for plugin in $(awk '$1 !~ /^#/ && $4 == "plugin" { print $1 }' "$root/vendor/vendor.lock"); do
  [ -d "$dist/plugins/$plugin" ] || { echo "vendored plugin $plugin is missing from $dist/plugins after vendoring" >&2; exit 1; }
done

# A prebuilt binary is used as it is; anything missing from $COLONIZER_PREBUILT is built here.
prebuilt_bin() {
  [ -n "$prebuilt" ] && [ -x "$prebuilt/$1" ] || return 1
  mkdir -p "$dist/bin"
  install -m 755 "$prebuilt/$1" "$dist/bin/$1"
  echo "using prebuilt $1 from $prebuilt"
}

# $app is a symlink to the slot directory beside it (app-a/app-b), so installing is a single rename:
# whenever the script stops, $app is either the whole old app or the whole new one, never nothing.
# Same layout as scripts/install-release.sh, whose relink/restore_app/cleanup_install these mirror.
relink() {
  rm -f "$2.new"
  ln -s "$1" "$2.new"
  mv -T "$2.new" "$2" 2>/dev/null || mv -h "$2.new" "$2" 2>/dev/null ||
    { rm -f "$2.new"; ln -sfn "$1" "$2"; }
}

# An install killed between parking the old app and linking the new one (SIGKILL runs no traps, and
# installers before the symlink layout had none) leaves the only copy at $app.old, with either
# nothing or a dangling symlink at $app. [ -e ] follows links, so one condition covers both; a
# dangling link has to go first, since a rename cannot replace a directory with one. The old
# --install path staged its copy at $app.new instead, so a copy parked there is moved into place too.
restore_app() {
  if [ -e "$app.old" ] && [ ! -e "$app" ]; then
    rm -f "$app"
    mv "$app.old" "$app"
    echo "put back the app an interrupted install left at $app.old"
  elif [ -e "$app.new" ] && [ ! -e "$app" ]; then
    mv "$app.new" "$app"
    echo "put back the app an interrupted install left at $app.new"
  fi
}

cleanup_install() {
  [ -z "$parked" ] || [ -e "$app" ] || mv "$parked" "$app"
}

# Stages $1 beside $app, into whichever of the two slots $app is not using, and points $app at it
# with one rename. A legacy install left the app in the directory itself, and no rename can replace
# a directory with a symlink, so it is parked at $app.old first, and the traps put it back if we are
# killed before the new link lands. Reads and writes the $app/$parked globals.
swap_app() {
  src=$1
  dir=$(dirname "$app")
  name=$(basename "$app")
  mkdir -p "$dir"
  restore_app
  rm -rf "$app.new" "$app.old"
  previous=$(readlink "$app" || true)
  slot=$name-a
  [ "$previous" != "$name-a" ] || slot=$name-b
  rm -rf "${dir:?}/$slot"
  cp -a "$src" "$dir/$slot"
  if [ -L "$app" ] || [ ! -e "$app" ]; then
    relink "$slot" "$app"
  else
    parked=$app.old
    mv "$app" "$parked"
    relink "$slot" "$app"
    parked=
    rm -rf "$app.old"
  fi
  case "$previous" in "$name-a" | "$name-b") rm -rf "${dir:?}/$previous" ;; esac
}

echo "==> colonizer-agentd (static musl build inside a microVM)"
prebuilt_bin colonizer-agentd || MSB="$msb" "$root/scripts/build-agentd.sh"

echo "==> rtk (static musl build inside a microVM, for colonies that switch on compact command output)"
prebuilt_bin rtk || MSB="$msb" "$root/scripts/build-rtk.sh"

# The host's own mesh needs a tailscaled, and Tailscale publishes no macOS build of it: on a Mac the
# vendored source is built into the app. Linux's tailscaled came from the vendored tgz above.
if [ "$(uname -s)" = "Darwin" ]; then
  echo "==> tailscale for the host (built from pinned source inside a microVM)"
  MSB="$msb" "$root/scripts/build-tailscaled.sh"
fi

echo "==> agent modules"
# Shipped so an installed mothership can apply an update with the same verified
# installer a person would run, instead of downloading a script to execute.
mkdir -p "$dist/scripts"
install -m 755 "$root/scripts/install-release.sh" "$dist/scripts/install-release.sh"

mkdir -p "$dist/modules/agents"
for module in "$root"/modules/agents/*/; do
  id=$(basename "$module")
  target="$dist/modules/agents/$id"
  rm -rf "$target"
  mkdir -p "$target"
  (cd "$module" && tar --exclude=./node_modules --exclude=./test -cf - .) | tar -xf - -C "$target"
  if [ -f "$target/package.json" ] && [ "$bundle" = 1 ]; then
    # A release carries no Anthropic code. The Agent SDK is "all rights reserved", and its optional
    # platform packages are Claude Code itself. Colonies run the Claude Code binary the install provides
    # (pathToClaudeCodeExecutable in the runner), so the platform packages are left out, and the SDK is
    # recorded in fetch-at-install for scripts/install-release.sh to fetch from the npm registry.
    (cd "$target" && npm ci --omit=dev --omit=optional --no-audit --no-fund --silent)
    (cd "$target" && node "$root/scripts/record-fetch-at-install.mjs" node_modules/@anthropic-ai/claude-agent-sdk)
  elif [ -f "$target/package.json" ]; then
    (cd "$target" && npm ci --omit=dev --no-audit --no-fund --silent)
  fi
  echo "installed agent module $id"
done

echo "==> web UI"
(cd "$root/web" && npm ci --no-audit --no-fund --silent && npm run build --silent)
rm -rf "$dist/web"
cp -r "$root/web/dist" "$dist/web"

echo "==> harness"
if ! prebuilt_bin colonizer; then
  # --locked builds exactly what the release does; a lockfile that no longer resolves fails loudly.
  cargo build --release --locked -p colonizer-harness --manifest-path "$root/Cargo.toml"
  mkdir -p "$dist/bin"
  install -m 755 "$root/target/release/colonizer" "$dist/bin/colonizer"
fi

if [ "$install_app" = 1 ]; then
  app="$HOME/.local/share/colonizer/app"
  echo "==> installing to $app"
  parked=
  trap cleanup_install EXIT
  # dash runs a trap and then carries on, so a signal has to end the script here.
  trap 'cleanup_install; exit 1' INT TERM
  swap_app "$dist"
  mkdir -p "$HOME/.local/bin"
  relink "$app/bin/colonizer" "$HOME/.local/bin/colonizer"
  echo "installed: run 'colonizer'; it prints and opens a sign-in link ('colonizer open' reprints it)"
else
  echo "built: run '$dist/bin/colonizer'; it prints and opens a sign-in link ('colonizer open' reprints it)"
fi

# The colony image is the one large thing that otherwise arrives lazily, during
# the first launch, with nothing on screen to explain the wait.
if [ "$pull_image" = 1 ]; then
  # The image the release was tested with, pulled by digest so the bits cannot drift under it
  # (crates/colonizer/images.lock; the reference is <url>@sha256:<sha256>). A missing lock or row
  # falls back to the bare tag: an install that is otherwise done beats a failed one.
  pinned=$(awk '$1 !~ /^#/ && $6 == "node:24-bookworm" && $4 == "image" { print $6 "@sha256:" $5; exit }' "$root/crates/colonizer/images.lock" 2>/dev/null)
  image=${COLONIZER_IMAGE:-${pinned:-node:24-bookworm}}
  echo "==> pulling colony image $image"
  "$dist/vendor/microsandbox/bin/msb" pull "$image"
fi
