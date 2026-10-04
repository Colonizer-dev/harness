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
# Checksums travel with the release they check, so they catch a corrupted download but not a release
# rewritten in place. When `gh` is installed the installer therefore also verifies SHA256SUMS against
# the build attestation the release workflow signed with Sigstore and logged publicly — something a
# rewritten release cannot produce — and a failed check aborts the install. When the check cannot
# reach a verdict it is skipped with a note instead: no `gh`; a gh too old for `gh attestation
# verify`; COLONIZER_RELEASE_URL pointing away from the official release; or a release published
# before the workflow began signing them, which carries no attestation to find. Skips become fatal
# under COLONIZER_REQUIRE_ATTESTATION=1.
#
# Anthropic's code is not in a release, because it is not ours to redistribute, so two things come from
# Anthropic's own channels instead, each checked before it is used:
# - the Claude Agent SDK the agent module runs, from the npm registry, against the checksum the release
#   recorded from package-lock.json (fetch-at-install);
# - on a Mac, the Linux build of Claude Code that colonies run: the build pinned by checksum in the
#   release (claude-code.lock, which scripts/update-runtime-pins.mjs refreshes by pull request), not
#   whatever Anthropic's `stable` channel points at that day. A colony is a Linux microVM, so the
#   Mac's own binary cannot run in it. (A Linux host reuses its own Claude Code install instead.)
# - on every host, the Linux Node.js runtime colonies run (node.lock, fetched by the guest_node step
#   below): unlike Claude Code there is no host install to reuse, and sessions.rs fails the boot
#   without bin/node-guest, so a Linux install fetches it just like a Mac one does.
#
#   COLONIZER_VERSION=v0.1.0   install that release instead of the latest
#   COLONIZER_APP=<dir>        install the app there instead of ~/.local/share/colonizer/app (the symlink)
#   COLONIZER_RELEASE_URL=<url>
#                              fetch the app from <url>/<file> instead of the GitHub release; the
#                              build attestation belongs to the official release, so it is skipped
#   COLONIZER_REQUIRE_ATTESTATION=1
#                              make a provenance check that comes back without a verdict a
#                              failure, not a note
#   --pull-image               also download the default colony image now (several gigabytes), so the
#                              first colony boots instead of waiting on it
#
# Everything is inside main(), so a download cut short by the network runs nothing.
set -eu

main() {
  repo="Colonizer-dev/harness"
  docs="https://colonizer.dev/docs/install"
  release_workflow="$repo/.github/workflows/release.yml"
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
  verify_provenance "$tmp/SHA256SUMS"

  mkdir -p "$tmp/unpack"
  tar -xzf "$tmp/$archive" -C "$tmp/unpack"
  [ -x "$tmp/unpack/colonizer/bin/colonizer" ] || fail "$archive has no colonizer/bin/colonizer"
  installed=$(cat "$tmp/unpack/colonizer/VERSION" 2>/dev/null || echo "$version")

  for record in "$tmp"/unpack/colonizer/modules/agents/*/fetch-at-install; do
    if [ -f "$record" ]; then fetch_at_install "$(dirname "$record")"; fi
  done
  if [ "$platform" = darwin-arm64 ]; then
    guest_claude "$tmp/unpack/colonizer/bin/claude-guest" "$app/bin/claude-guest" "$tmp/unpack/colonizer/claude-code.lock"
    guest_node "$tmp/unpack/colonizer/bin/node-guest" "$tmp/unpack/colonizer/node.lock" "linux-arm64"
  elif [ "$platform" = linux-x86_64 ]; then
    # No host fallback for node: unlike claude-guest, which a Linux host reuses from its own
    # install, sessions.rs fails the boot without bin/node-guest — so Linux installs fetch it too.
    guest_node "$tmp/unpack/colonizer/bin/node-guest" "$tmp/unpack/colonizer/node.lock" "linux-x64"
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
  inuse=$(slot_pids "$dir/$slot")
  [ -z "$inuse" ] ||
    fail "$dir/$slot is in use by pid(s) $inuse; restart colonizer (or wait for 'colonizer update') then install again"
  rm -rf "${dir:?}/$slot"
  mv "$tmp/unpack/colonizer" "$dir/$slot"

  # The macOS Keychain ties each saved secret to the binary that wrote it, so an ad-hoc signed app
  # loses that access the moment an update replaces it — and a mothership updating in place passes its
  # own environment, which a cockpit-started update usually does not carry COLONIZER_CODESIGN_IDENTITY
  # in. When an identity is known — the variable, or the one a previous install recorded beside the
  # app — the new binary is signed before the switch, so the Keychain grant survives. A signing
  # failure removes the new slot and stops before the switch: the old version, which the Keychain
  # knows, stays installed and $app keeps pointing at it.
  identity=${COLONIZER_CODESIGN_IDENTITY:-}
  from_env=0
  if [ -n "$identity" ]; then
    from_env=1
  elif [ -f "$dir/codesign-identity" ]; then
    identity=$(cat "$dir/codesign-identity" 2>/dev/null || true)
  fi
  signed=0
  if [ -n "$identity" ] && [ "$(uname -s)" = Darwin ]; then
    say "signing colonizer as $identity"
    if ! codesign --force --sign "$identity" --identifier dev.colonizer.mothership \
         --timestamp=none "$dir/$slot/bin/colonizer"; then
      rm -rf "${dir:?}/$slot"
      fail "codesign failed for '$identity'; without it the macOS Keychain would ask for every saved secret again. Nothing was changed: the previous version is still installed."
    fi
    signed=1
  fi

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

  # Remember an identity that came in the environment, so an update started from the cockpit — whose
  # installer child has no environment of its own — re-signs too. Written beside the app and renamed
  # into place, so a reader never sees a half-written name. An identity that was only read back from
  # this file is already in it. Delete the file to stop re-signing.
  if [ "$from_env" = 1 ] && [ "$signed" = 1 ]; then
    printf '%s\n' "$identity" > "$dir/codesign-identity.new"
    mv -f "$dir/codesign-identity.new" "$dir/codesign-identity"
  fi

  # Colonies mount vendored plugins straight out of the slot the mothership was
  # started from (sessions.rs resolves its assets through current_exe, which
  # canonicalises the symlink away), so removing it under a running colony takes
  # its plugins with it. An update applied by a running mothership sets
  # COLONIZER_KEEP_PREVIOUS=1 and cleans the slot up itself, once nothing is
  # using it. A person running the installer by hand keeps today's behaviour,
  # except that a slot a process is still running from is never removed either:
  # the mothership's own sweep knows when colonies have let go of it.
  if [ "${COLONIZER_KEEP_PREVIOUS:-0}" = 1 ]; then
    case "$previous" in "$name-a" | "$name-b") say "keeping the previous version at $dir/$previous" ;; esac
  else
    case "$previous" in
      "$name-a" | "$name-b")
        prev_inuse=$(slot_pids "$dir/$previous")
        if [ -n "$prev_inuse" ]; then
          say "keeping the previous version at $dir/$previous: pid(s) $prev_inuse still run from it (swept on the next start)"
        else
          rm -rf "${dir:?}/$previous"
        fi
        ;;
    esac
  fi

  mkdir -p "$HOME/.local/bin"
  relink "$app/bin/colonizer" "$HOME/.local/bin/colonizer"
  say "installed Colonizer $installed to $app"

  if [ "$pull_image" = 1 ]; then
    # The image the release was tested with, pulled by digest so the bits cannot drift under it
    # (images.lock rides in the app; the reference is <url>@sha256:<sha256>). A missing lock or row
    # falls back to the bare tag: an install that is otherwise done beats a failed one.
    pinned=$(awk '$1 !~ /^#/ && $6 == "node:24-bookworm" && $4 == "image" { print $6 "@sha256:" $5; exit }' "$app/images.lock" 2>/dev/null)
    image=${COLONIZER_IMAGE:-${pinned:-node:24-bookworm}}
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
  echo "Run '$run'; it prints and opens a sign-in link ('colonizer open' reprints it). If it was already running, restart it."
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

# The pids whose command line, or whose real executable, lives under $1 (used as a path prefix, so
# pass the slot directory). A slot is only safe to delete when nothing executes from it — the
# mothership, or a boot's msb. The mothership is started through a symlink outside the slot
# (~/.local/bin/colonizer, the unit's ExecStart, or $dir/app/bin/colonizer after an update restarts
# itself), so `ps` shows the symlink, not the slot: on Linux every /proc/<pid>/exe link is resolved
# with readlink, and on macOS the text segment lsof reports is mapped back to pids when lsof is
# installed. Errors (a vanished pid, a kernel thread, another user's process) are ignored. The argv
# match stays, for anything that execs from the slot directly. Both use a trailing-slash prefix so
# `app-a` does not match `app-ab`; our own shell is dropped so the walk cannot report the installer
# itself. `ps -ww -axo` is Linux and macOS. Mirrors the same helper in scripts/install.sh.
slot_pids() {
  slot=${1%/}/
  # The ps table is read before awk matches it, so awk is not in its own snapshot.
  ps_out=$(ps -ww -axo pid=,command= 2>/dev/null || true)
  argv_pids=$(printf '%s\n' "$ps_out" | awk -v p="$slot" 'index($0, p) { print $1 }')
  exe_pids=
  if [ -d /proc ]; then
    exe_pids=$(for link in /proc/[0-9]*/exe; do
      real=$(readlink "$link" 2>/dev/null) || continue
      case "$real" in
        ("$slot"*) pid=${link#/proc/}; printf '%s\n' "${pid%/exe}" ;;
      esac
    done)
  elif command -v lsof >/dev/null 2>&1; then
    exe_pids=$(lsof -nP -d txt -Fpn 2>/dev/null | awk -v p="$slot" '
      /^p/ { pid = substr($0, 2) }
      /^n/ { if (index(substr($0, 2), p) == 1) print pid }')
  fi
  printf '%s\n%s\n' "$argv_pids" "$exe_pids" |
    awk -v self="$$" 'NF && $1 != self && !seen[$1]++ { printf "%s%s", (n++ ? " " : ""), $1 }'
}

# The checksums say the archive matches SHA256SUMS, and SHA256SUMS comes from the same release as the
# archive, so together they only rule out a corrupted download: whoever can rewrite the release assets
# can rewrite both. The release workflow also signs SHA256SUMS itself with Sigstore and logs that in a
# public transparency log, which asset access alone cannot forge, so verifying the one checksums file
# puts every digest the check above compared against under the attestation — one gh round trip instead
# of one per artifact.
#
# Plain sh cannot verify a Sigstore bundle, so this needs gh, and when the check cannot reach a
# verdict it is skipped with a note and the checksum-verified install goes on: gh missing, a gh from
# before `gh attestation verify` existed, a gh installed but not logged in (the common case on a
# headless host, which would otherwise fail every update), a COLONIZER_RELEASE_URL download, which the
# official repo's attestation says nothing about, or a release that carries no attestation at all —
# every release published before this check existed, which gh answers with "no attestations found".
# COLONIZER_REQUIRE_ATTESTATION=1 turns those skips into failures. A check that ran and found
# something wrong is a different thing and is fatal whatever the variable says: a logged-in gh
# answered, and its answer is that these checksums are not the ones the workflow signed. The downloads
# only just succeeded over this network, so any other gh failure the auth probe below does not explain
# is read as that answer, not as the network being down.
#
# "No attestations found" is a skip, not that failure: it means gh found no provenance to check, not
# that the provenance is wrong — the state of every release published before this step existed. Nor
# is it a state an attacker who swaps a release asset can reach on purpose. The lookup runs on the
# file's digest in the public transparency log, so a swapped file digests differently and its lookup
# legitimately finds nothing, landing in this same skip; that is harmless only because the checksum
# check is mandatory and runs first, so a file that does not match the release's SHA256SUMS is
# rejected before verify_provenance is ever called — nothing gets here except checksum-verified
# files. Matching gh by wording is the fragile part: a future gh that rewords the message no longer
# matches, the install fails again, and that is the right direction to break in — far rarer than
# certainly failing every gh user today.
verify_provenance() {
  file=$1

  if [ -n "${COLONIZER_RELEASE_URL:-}" ]; then
    why="the download comes from COLONIZER_RELEASE_URL, not the $repo release"
  elif ! command -v gh >/dev/null 2>&1; then
    why="gh is not installed (the checksum check above still applies)"
  elif ! gh_help=$(gh attestation verify --help 2>&1); then
    why="this gh predates 'gh attestation verify'"
  fi

  if [ -z "${why:-}" ]; then
    # --repo ties the signature to this repository; where the gh in use also knows --signer-workflow,
    # pin the workflow, so anything else the repository can sign with does not count.
    set -- gh attestation verify "$file" --repo "$repo"
    case "$gh_help" in
      *--signer-workflow*) set -- "$@" --signer-workflow "$release_workflow" ;;
    esac
    if out=$("$@" 2>&1); then
      say "provenance verified: $(basename "$file") is what $release_workflow signed"
      return
    elif printf '%s\n' "$out" | grep -qi 'no attestations'; then
      # An unattested release, not a wrong one — see the comment above the function for why this is
      # not a downgrade an attacker can steer a tampered file into.
      why="$(basename "$file") carries no build provenance; that is expected for releases published before the release workflow began signing, and would mean something was wrong on a current one"
    elif printf '%s\n' "$out" | grep -qi 'gh auth login' || ! gh auth status >/dev/null 2>&1; then
      # gh ran but could not reach a verdict because it has no credentials — the common case on a
      # headless host, and a skip, not a rejection: it never fetched the attestation to reject it. gh
      # points at `gh auth login` when it is not logged in; `gh auth status` confirms it when a gh
      # words the failure differently. Only a gh that could have looked and disagreed — the branch
      # below — is the wrong-signature failure.
      why="gh is not logged in, so it cannot fetch the attestation; run 'gh auth login' or set GH_TOKEN to verify provenance"
    else
      printf '%s\n' "$out" >&2
      fail "the build attestation over $(basename "$file") does not verify: gh checked, and it is wrong — these checksums are not what $release_workflow signed; nothing was installed"
    fi
  fi

  if [ "${COLONIZER_REQUIRE_ATTESTATION:-0}" = 1 ]; then
    fail "provenance could not be checked: $why — COLONIZER_REQUIRE_ATTESTATION=1 installs only attested bits; nothing was installed"
  fi
  say "provenance not checked: $why"
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
# build: the build pinned in the claude-code.lock that shipped inside this release, so the release and
# not Anthropic's `stable` channel decides what a colony runs. The copy from the previous install is
# reused when it is already that pinned build. The lock is plain columns, so this needs no jq — and
# neither Node.js nor plutil, which the manifest it replaced needed one of.
guest_claude() {
  out=$1 previous=$2 lock=$3
  [ -f "$lock" ] || fail "the release has no $lock; nothing was installed"
  cc_version="" cc_sha="" cc_url=""
  while read -r lock_name lock_version lock_plat lock_kind lock_sha lock_url; do
    case "$lock_name" in ''|'#'*) continue ;; esac
    [ "$lock_plat" = "linux-arm64" ] || continue
    [ "$lock_kind" = "agent" ] || continue
    # A row with too few columns matches here but pins no url; skip it so a
    # well-formed row for the same platform further down still gets found.
    [ -n "$lock_url" ] || continue
    cc_version=$lock_version cc_sha=$lock_sha cc_url=$lock_url
    break
  done < "$lock"
  [ -n "$cc_url" ] || fail "no linux-arm64 Claude Code build pinned in $lock"
  if [ -f "$previous" ] && [ "$(sha256_of "$previous")" = "$cc_sha" ]; then
    cp "$previous" "$out"
    say "Claude Code $cc_version for colonies is already here"
  else
    say "downloading Claude Code $cc_version for colonies (linux-arm64)"
    fetch "$cc_url" "$out"
    [ "$(sha256_of "$out")" = "$cc_sha" ] || fail "checksum mismatch for Claude Code $cc_version; nothing was installed"
  fi
  chmod 755 "$out"
}

# The Linux Node.js runtime for the guest, as scripts/fetch-node-binary.sh fetches it for a source
# build: the tarball pinned in the node.lock that shipped inside this release. The lock pins the
# tarball, so its checksum is verified before bin/node is extracted — the installed file itself has
# no pin, and a previous copy is never reused sight unseen.
guest_node() {
  out=$1 lock=$2 want=$3
  [ -f "$lock" ] || fail "the release has no $lock; nothing was installed"
  node_version="" node_sha="" node_url=""
  while read -r lock_name lock_version lock_plat lock_kind lock_sha lock_url; do
    case "$lock_name" in ''|'#'*) continue ;; esac
    [ "$lock_plat" = "$want" ] || continue
    [ "$lock_kind" = "runtime" ] || continue
    [ -n "$lock_url" ] || continue
    node_version=$lock_version node_sha=$lock_sha node_url=$lock_url
    break
  done < "$lock"
  [ -n "$node_url" ] || fail "no $want Node.js runtime pinned in $lock"
  say "downloading Node.js $node_version for colonies ($want)"
  fetch "$node_url" "$tmp/node.tar.xz"
  [ "$(sha256_of "$tmp/node.tar.xz")" = "$node_sha" ] || fail "checksum mismatch for Node.js $node_version; nothing was installed"
  rm -rf "$tmp/node.unpack" && mkdir -p "$tmp/node.unpack"
  entry=$(tar -tf "$tmp/node.tar.xz" | grep '/bin/node$' | head -n 1)
  [ -n "$entry" ] || fail "Node.js $node_version for colonies has no bin/node"
  tar -xJf "$tmp/node.tar.xz" -C "$tmp/node.unpack" "$entry"
  mv -f "$tmp/node.unpack/$entry" "$out"
  chmod 755 "$out"
}

main "$@"
