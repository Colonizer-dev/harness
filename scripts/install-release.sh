#!/bin/sh
# Installs a prebuilt Colonizer release. Published with every release as `install.sh`, and served at
# https://colonizer.dev/install.sh:
#
#   curl -fsSL https://colonizer.dev/install.sh | sh
#   curl -fsSL https://colonizer.dev/install.sh | sh -s -- --pull-image
#
# It downloads the app for this machine from the GitHub release, checks it against the release's
# SHA256SUMS, and links ~/.local/bin/colonizer to ~/.local/share/colonizer/app, a symlink to the
# directory the installed version lives in. Running it again points that symlink at the new version
# with one rename, so an install that stops halfway leaves either the whole old app or the whole new
# one. Settings (~/.config/colonizer) and colonies (~/.local/share/colonizer) are never touched.
#
# Anthropic's code is not in a release, because it is not ours to redistribute, so two things come from
# Anthropic's own channels instead, each checked before it is used:
# - the Claude Agent SDK the agent module runs, from the npm registry, against the checksum the release
#   recorded from package-lock.json (fetch-at-install);
# - on a Mac, the Linux build of Claude Code that colonies run, against Anthropic's manifest. A colony is
#   a Linux microVM, so the Mac's own binary cannot run in it.
#
#   COLONIZER_VERSION=v0.1.0   install that release instead of the latest
#   COLONIZER_APP=<dir>        install the app there instead of ~/.local/share/colonizer/app (the symlink)
#   --pull-image               also download the default colony image now (several gigabytes), so the
#                              first colony boots instead of waiting on it
#
# Everything is inside main(), so a download cut short by the network runs nothing.
set -eu

main() {
  repo="Colonizer-dev/harness"
  docs="https://colonizer.dev/docs/install"
  pull_image=0
  for arg in "$@"; do
    case "$arg" in
      --pull-image) pull_image=1 ;;
      *) fail "unknown option: $arg" ;;
    esac
  done

  # One build per platform, and nothing pretends to work where it cannot.
  case "$(uname -s)-$(uname -m)" in
    Linux-x86_64)
      platform=linux-x86_64
      { [ -r /dev/kvm ] && [ -w /dev/kvm ]; } ||
        fail "/dev/kvm is not readable and writable by $(id -un); colonies are KVM microVMs"
      ;;
    Darwin-arm64) platform=darwin-arm64 ;;
    Darwin-x86_64) fail "Apple Silicon only: microsandbox's libkrun backend has no Intel Mac support" ;;
    *) fail "no prebuilt Colonizer for $(uname -s) $(uname -m); see $docs" ;;
  esac

  for c in curl tar; do need "$c"; done
  command -v sha256sum >/dev/null 2>&1 || command -v shasum >/dev/null 2>&1 || fail "missing sha256sum or shasum"

  version=${COLONIZER_VERSION:-latest}
  if [ -n "${COLONIZER_RELEASE_URL:-}" ]; then
    base=$COLONIZER_RELEASE_URL
  elif [ "$version" = latest ]; then
    base="https://github.com/$repo/releases/latest/download"
  else
    base="https://github.com/$repo/releases/download/$version"
  fi
  app=${COLONIZER_APP:-$HOME/.local/share/colonizer/app}
  archive="colonizer-$platform.tar.gz"

  tmp=$(mktemp -d)
  parked=
  trap cleanup EXIT
  # dash runs a trap and then carries on, so a signal has to end the script here.
  trap 'cleanup; exit 1' INT TERM

  say "downloading Colonizer ($version, $platform)"
  fetch "$base/$archive" "$tmp/$archive"
  fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS"
  expected=$(awk -v f="$archive" '$2 == f || $2 == "*" f { print $1 }' "$tmp/SHA256SUMS")
  [ -n "$expected" ] || fail "$archive is not listed in the release's SHA256SUMS"
  [ "$(sha256_of "$tmp/$archive")" = "$expected" ] || fail "checksum mismatch for $archive; nothing was installed"

  mkdir -p "$tmp/unpack"
  tar -xzf "$tmp/$archive" -C "$tmp/unpack"
  [ -x "$tmp/unpack/colonizer/bin/colonizer" ] || fail "$archive has no colonizer/bin/colonizer"
  installed=$(cat "$tmp/unpack/colonizer/VERSION" 2>/dev/null || echo "$version")

  for record in "$tmp"/unpack/colonizer/modules/agents/*/fetch-at-install; do
    if [ -f "$record" ]; then fetch_at_install "$(dirname "$record")"; fi
  done
  if [ "$platform" = darwin-arm64 ]; then
    guest_claude "$tmp/unpack/colonizer/bin/claude-guest" "$app/bin/claude-guest"
  fi

  # $app is a symlink to the directory this version lives in, so installing it is a single rename:
  # whenever the script stops, $app is either the whole old app or the whole new one, never nothing.
  dir=$(dirname "$app")
  name=$(basename "$app")
  mkdir -p "$dir"
  restore_app
  rm -rf "$app.new" "$app.old"

  # Unpack beside the app that is running, into whichever of the two slots it is not using.
  previous=$(readlink "$app" || true)
  slot=$name-a
  [ "$previous" != "$name-a" ] || slot=$name-b
  rm -rf "${dir:?}/$slot"
  mv "$tmp/unpack/colonizer" "$dir/$slot"

  if [ -L "$app" ] || [ ! -e "$app" ]; then
    relink "$slot" "$app"
  else
    # An install from before the symlink layout left the app in the directory itself, and no rename
    # can replace a directory with a symlink. Park it, and let the traps put it back if we are killed.
    parked=$app.old
    mv "$app" "$parked"
    relink "$slot" "$app"
    parked=
    rm -rf "$app.old"
  fi
  # Colonies mount vendored plugins straight out of the slot the mothership was
  # started from (sessions.rs resolves its assets through current_exe, which
  # canonicalises the symlink away), so removing it under a running colony takes
  # its plugins with it. An update applied by a running mothership sets
  # COLONIZER_KEEP_PREVIOUS=1 and cleans the slot up itself, once nothing is
  # using it. A person running the installer by hand keeps today's behaviour.
  if [ "${COLONIZER_KEEP_PREVIOUS:-0}" = 1 ]; then
    case "$previous" in "$name-a" | "$name-b") say "keeping the previous version at $dir/$previous" ;; esac
  else
    case "$previous" in "$name-a" | "$name-b") rm -rf "${dir:?}/$previous" ;; esac
  fi

  mkdir -p "$HOME/.local/bin"
  relink "$app/bin/colonizer" "$HOME/.local/bin/colonizer"
  say "installed Colonizer $installed to $app"

  if [ "$pull_image" = 1 ]; then
    image=${COLONIZER_IMAGE:-node:24-bookworm}
    say "pulling colony image $image"
    "$app/vendor/microsandbox/bin/msb" pull "$image"
  fi

  echo
  missing=""
  for c in git gh; do command -v "$c" >/dev/null 2>&1 || missing="$missing $c"; done
  [ -z "$missing" ] || echo "Colonies also need:$missing (install them before launching one)."
  if [ "$platform" = linux-x86_64 ] && ! command -v claude >/dev/null 2>&1; then
    echo "Colonies run your native Claude Code install, and there is none on PATH: https://claude.com/claude-code"
  fi
  case ":$PATH:" in
    *":$HOME/.local/bin:"*) run="colonizer" ;;
    *) run="$HOME/.local/bin/colonizer" ;;
  esac
  echo "Run '$run' and open http://127.0.0.1:7878. If it was already running, restart it."
  echo "Guide: $docs"
}

say() { printf '==> %s\n' "$1"; }
fail() { printf 'colonizer install: %s\n' "$1" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"; }
fetch() { curl -fsSL --retry 3 -o "$2" "$1" || fail "could not download $1"; }

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

cleanup() {
  [ -z "$parked" ] || [ -e "$app" ] || mv "$parked" "$app"
  rm -rf "$tmp"
}

# An install killed between parking the old app and linking the new one (SIGKILL runs no traps, and
# installers before the symlink layout had no traps at all) leaves the only copy at $app.old, with
# either nothing or a dangling symlink at $app. [ -e ] follows links, so one condition covers both;
# a dangling link has to go first, since a rename cannot replace a directory with one.
restore_app() {
  if [ -e "$app.old" ] && [ ! -e "$app" ]; then
    rm -f "$app"
    mv "$app.old" "$app"
    say "put back the app an interrupted install left at $app.old"
  fi
}

# Points $2 at $1 with one rename, so nothing ever finds $2 missing. mv(1) follows a symlink to a
# directory and would move the new link inside it, so it needs GNU's -T or BSD's -h (macOS 13 and
# up); older macOS has neither, and ln -sf's unlink-then-symlink is the best it can do there.
relink() {
  rm -f "$2.new"
  ln -s "$1" "$2.new"
  mv -T "$2.new" "$2" 2>/dev/null || mv -h "$2.new" "$2" 2>/dev/null ||
    { rm -f "$2.new"; ln -sfn "$1" "$2"; }
}

# Packages a module's release left out (scripts/record-fetch-at-install.mjs): each line of fetch-at-install
# is `<path> <tarball url> <sha256>`, and the tarball's package/ directory becomes <module>/<path>.
fetch_at_install() {
  module=$1
  while read -r path url sha; do
    [ -n "$path" ] || continue
    name=$(basename "$path")
    say "downloading $name for the $(basename "$module") module"
    fetch "$url" "$tmp/$name.tgz"
    [ "$(sha256_of "$tmp/$name.tgz")" = "$sha" ] || fail "checksum mismatch for $url; nothing was installed"
    rm -rf "$tmp/$name.unpack" && mkdir -p "$tmp/$name.unpack"
    tar -xzf "$tmp/$name.tgz" -C "$tmp/$name.unpack"
    [ -d "$tmp/$name.unpack/package" ] || fail "$url has no package/ directory"
    mkdir -p "$(dirname "$module/$path")"
    rm -rf "${module:?}/$path"
    mv "$tmp/$name.unpack/package" "$module/$path"
  done < "$module/fetch-at-install"
}

# The Linux build of Claude Code for the guest, as scripts/fetch-agent-binary.sh fetches it for a source
# build: the `stable` channel, checked against the checksum in Anthropic's manifest. The copy from the
# previous install is reused when it is already that build. plutil reads the manifest, so a prebuilt
# install needs no Node.js.
guest_claude() {
  out=$1 previous=$2
  cc="https://downloads.claude.ai/claude-code-releases"
  cc_version=$(curl -fsSL --retry 3 "$cc/stable") || fail "could not read Claude Code's stable channel"
  case "$cc_version" in [0-9]*) ;; *) fail "unexpected Claude Code version: $cc_version" ;; esac
  fetch "$cc/$cc_version/manifest.json" "$tmp/claude-manifest.json"
  cc_sha=$(plutil -extract "platforms.linux-arm64.checksum" raw -o - "$tmp/claude-manifest.json" 2>/dev/null) ||
    fail "no linux-arm64 checksum in the Claude Code $cc_version manifest"
  if [ -f "$previous" ] && [ "$(sha256_of "$previous")" = "$cc_sha" ]; then
    cp "$previous" "$out"
    say "Claude Code $cc_version for colonies is already here"
  else
    say "downloading Claude Code $cc_version for colonies (linux-arm64)"
    fetch "$cc/$cc_version/linux-arm64/claude" "$out"
    [ "$(sha256_of "$out")" = "$cc_sha" ] || fail "checksum mismatch for Claude Code $cc_version; nothing was installed"
  fi
  chmod 755 "$out"
}

main "$@"
