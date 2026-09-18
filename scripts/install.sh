#!/bin/sh
# Builds a self-contained Colonizer app directory. Nothing is downloaded at runtime.
#
#   scripts/install.sh              build everything into ./dist (run the harness from the checkout)
#   scripts/install.sh --install    also install to ~/.local/share/colonizer/versions/<version>, point
#                                   the ~/.local/share/colonizer/app symlink at it and link
#                                   ~/.local/bin/colonizer through the symlink
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
if [ "$(uname -s)" = "Darwin" ] && [ "$bundle" = 0 ]; then
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

# A prebuilt binary is used as it is; anything missing from $COLONIZER_PREBUILT is built here.
prebuilt_bin() {
  [ -n "$prebuilt" ] && [ -x "$prebuilt/$1" ] || return 1
  mkdir -p "$dist/bin"
  install -m 755 "$prebuilt/$1" "$dist/bin/$1"
  echo "using prebuilt $1 from $prebuilt"
}

# A version is used as a directory name under versions/, so only a plain v…/dev-… token is accepted:
# no slash or whitespace to escape versions/, no leading dot to hide among the staging names.
check_version_token() {
  case $1 in
    v[0-9]*|dev-*) ;;
    *) echo "'$1' is not a plain v…/dev-… version; refusing to use it as an install directory" >&2; exit 1 ;;
  esac
  case $1 in
    *[!A-Za-z0-9._-]*) echo "'$1' contains characters that are not safe in a directory name" >&2; exit 1 ;;
  esac
}

# mountinfo writes space, tab, newline and backslash in its path fields as \040, \011, \012 and
# \134; a field is only compared against a real path after those are decoded. \134 goes last, so
# the escaped form of a literal backslash cannot be decoded a second time out of what it decodes
# to: a file literally named \040 arrives as \134040, and must still be \040 afterwards.
iu_unescape() {
  printf '%s\n' "$1" |
    awk '{ gsub(/\\040/, " "); gsub(/\\011/, "\t"); gsub(/\\012/, "\n"); gsub(/\\134/, "\\\\"); print }'
}

# Whether anything can still reach into $1 — a version directory, a copy renamed aside, or the
# migrated legacy directory. The app link names it (or named it before this run repointed it), a
# colony still bind-mounts something under it, or a running process's own executable resolves into
# it. Renaming a directory keeps a bind mount and an open file alive — both follow the inode — so
# this is only ever asked before a delete, and the delete is what gets deferred. With no /proc to
# ask (macOS ships none) the answer is yes: keeping an old version a while longer costs disk, while
# a colony whose mounts went empty costs a restart.
dir_in_use() {
  iu_dir=$(cd "$1" 2>/dev/null && pwd -P) || return 1 # nothing resolves there, so nothing holds it
  if [ -e "$app" ]; then
    iu_app=$(cd "$app" 2>/dev/null && pwd -P) || iu_app=
    if [ "$iu_app" = "$iu_dir" ]; then return 0; fi
  fi
  if [ -n "$previous" ] && [ -d "$versions/$previous" ]; then
    iu_prev=$(cd "$versions/$previous" 2>/dev/null && pwd -P) || iu_prev=
    if [ "$iu_prev" = "$iu_dir" ]; then return 0; fi
  fi
  [ -d /proc/self ] || return 0 # no way to tell, so never delete
  # /proc/self/mountinfo, not /proc/self/mounts: only mountinfo says where a bind mount's tree is
  # rooted in its filesystem, which is the path a colony's read-only mounts were made from — and
  # that path follows a rename, because a mount follows the inode. It is relative to whatever
  # filesystem the directory sits on, so it is matched against the directory relative to the
  # deepest mount point over it. mountinfo cannot write space, tab, newline or backslash into a
  # path field and writes those four as \040, \011, \012 and \134, so each field is decoded before
  # it is compared: compared raw, every match under a path holding any of them would silently
  # miss, and a directory a colony still holds would be reported free — an over-deletion, the one
  # thing this must never do.
  iu_best=""
  while read -r _ _ _ _ iu_mp _; do
    case $iu_mp in *\\*) iu_mp=$(iu_unescape "$iu_mp") ;; esac
    case $iu_dir in
      "$iu_mp"/*)
        if [ -z "$iu_best" ] || [ "${#iu_mp}" -gt "${#iu_best}" ]; then iu_best=$iu_mp; fi
        ;;
    esac
  done < /proc/self/mountinfo
  iu_rel=$iu_dir
  if [ -n "$iu_best" ] && [ "$iu_best" != "/" ]; then iu_rel=${iu_dir#"$iu_best"}; fi
  while read -r _ _ _ iu_root iu_mp _; do
    case $iu_mp in *\\*) iu_mp=$(iu_unescape "$iu_mp") ;; esac
    case $iu_mp in
      "$iu_dir"|"$iu_dir"/*) return 0 ;; # something is mounted on or under the directory
    esac
    case $iu_root in *\\*) iu_root=$(iu_unescape "$iu_root") ;; esac
    case $iu_root in
      "$iu_rel"|"$iu_rel"/*) return 0 ;; # a colony bind-mounts out of it
    esac
  done < /proc/self/mountinfo
  # /proc/<pid>/exe is the kernel's own resolution of a process's binary, symlinks included; the
  # entry is gone by the time we read it only when the process is.
  for iu_exe in /proc/[0-9]*/exe; do
    iu_target=$(readlink "$iu_exe" 2>/dev/null) || continue
    case $iu_target in
      "$iu_dir"|"$iu_dir"/*) return 0 ;;
    esac
  done
  return 1
}

# Deletes a copy that a rename staged aside, or a migrated legacy directory, once nothing can reach
# it any more. The rename itself was safe — mounts and open files followed the inode — so this is
# the only step that has to wait, and a leftover it leaves behind is reclaimed by a later run of
# this installer or by the app's startup prune.
reclaim_dir() {
  if dir_in_use "$1"; then
    echo "==> $1 is still in use by a colony or a running app; it will be removed once nothing reads it"
  else
    rm -rf "$1"
  fi
}

# Reclaims the deferred leftovers of earlier runs: the copies renamed aside (versions/.<v>.old*) and
# the migrated legacy directory (.app.legacy*). Each goes only once nothing mounts or executes out
# of it any more, so a colony still holding one keeps it until a later pass.
reclaim_leftovers() {
  iu_reclaimed=""
  iu_kept=""
  for iu_stale in "${versions:?}"/.*.old* "${root:?}"/.app.legacy*; do
    [ -e "$iu_stale" ] || continue # the glob matched nothing
    if dir_in_use "$iu_stale"; then
      iu_kept="$iu_kept $(basename "$iu_stale")"
    else
      rm -rf "$iu_stale"
      iu_reclaimed="$iu_reclaimed $(basename "$iu_stale")"
    fi
  done
  [ -z "$iu_reclaimed" ] || echo "==> reclaimed leftovers from earlier runs:$iu_reclaimed"
  [ -z "$iu_kept" ] || echo "==> left for later, still in use:$iu_kept"
}

# A free name to move a doomed copy aside to: $1 itself when nothing has it, else $1-2, $1-3, … —
# the canonical name may still be held by a leftover an earlier run could not reclaim yet.
free_name() {
  iu_name=$1
  iu_n=2
  while [ -e "$iu_name" ]; do
    iu_name="$1-$iu_n"
    iu_n=$((iu_n + 1))
  done
  printf '%s\n' "$iu_name"
}

# Repoint $app at versions/<build_version>. Where mv understands -T (GNU), the new link is staged
# under a temp name and renamed over $app, so at every instant $app is either the old link or the new
# one, never missing and never dangling. Plain mv cannot do this: it follows $app (which points at a
# directory) and would file the new link inside the old version instead of replacing it. macOS's mv
# has no -T, so there the link is made directly with ln -sfn — unlink and re-link in quick
# succession rather than one rename. mv -T --help tells a mv that lacks -T from one whose rename
# failed for real: it exits 0 wherever the option exists, so only there is the failure returned and
# the old state left standing for the caller to restore.
link_app_to() {
  rm -rf "${root:?}/.app.new" # a leftover from an interrupted run; this name is only ever a link
  ln -sfn "versions/$build_version" "$root/.app.new"
  if mv -Tf "$root/.app.new" "$app" 2>/dev/null; then
    return 0
  elif mv -T --help >/dev/null 2>&1; then
    return 1 # this mv knows -T, so the rename above failed for real
  fi
  ln -sfn "versions/$build_version" "$app"
  rm -f "${root:?}/.app.new"
}

# Install $dist as versions/<build_version> and point $app at it. A version directory is written
# exactly once, by renaming a fully staged copy into place, and afterwards only ever renamed or
# deleted whole. A mothership holds the directory it booted from open (current_exe resolves through
# the symlink to versions/<version>), and colonies mount their agent, plugins and vendored tools
# read-only out of it, so nothing that any running process can still reach is ever modified or
# removed in place:
install_unpacked() {
  mkdir -p "$versions"
  # Staging happens inside versions/ so the final move is a same-filesystem rename, and a stage left
  # by an interrupted run is cleaned up first. The stage name starts with a dot and is never pointed
  # at, so it is always safe to remove. dist itself stays put (a --pull-image below still uses it),
  # so this is a copy rather than the move install-release.sh does. Before anything else, the
  # deferred leftovers of earlier runs go, each only if nothing holds it any more.
  stage="$versions/.$build_version.new"
  reclaim_leftovers
  rm -rf "$stage"
  cp -a "$dist" "$stage"
  target="$versions/$build_version"
  if [ ! -d "$target" ]; then
    # A previous run died between renaming this version aside and moving the staged copy in, which
    # leaves $app dangling; a copy renamed aside is the newest one there is, so put the newest
    # back — the highest generation, since free_name hands them out in order. A rename never
    # disturbs a mount or an open file, so this is safe whatever still holds the copy; any older
    # generation left where it is goes once reclaim_leftovers finds nothing holding it.
    aside=""
    aside_n=0
    for candidate in "${versions:?}/.$build_version".old*; do
      [ -d "$candidate" ] || continue # the glob matched nothing
      case $candidate in
        *.old) candidate_n=1 ;;
        *.old-*)
          candidate_n=${candidate##*.old-}
          case $candidate_n in
            ''|*[!0-9]*) continue ;; # not a name this installer hands out
          esac
          ;;
        *) continue ;;
      esac
      if [ "$candidate_n" -gt "$aside_n" ]; then
        aside=$candidate
        aside_n=$candidate_n
      fi
    done
    if [ -n "$aside" ]; then
      mv "$aside" "$target"
    fi
  fi
  if [ -d "$target" ]; then
    # Reinstalling a version that is already on disk — a normal flow, not an accident: the installer
    # is re-run to repair a release, and a dev rebuild of a dirty tree yields the same describe
    # token. The old copy is renamed aside — a rename keeps every colony mount and open file on it
    # alive — and the staged copy takes its name. The renamed copy is deleted only once nothing can
    # still reach it; otherwise it stays as a dot-prefixed leftover for a later run of this
    # installer, or the app's startup prune, to reclaim. If the new copy cannot be moved in, the
    # old one goes straight back, so only a kill between the two renames can leave $app dangling —
    # and the recovery above puts the renamed copy back on the next run.
    aside=$(free_name "${versions:?}/.$build_version.old")
    mv "$target" "$aside"
    if ! mv "$stage" "$target"; then
      mv "$aside" "$target"
      echo "could not move the new $build_version into place; the previous copy was kept" >&2
      exit 1
    fi
    reclaim_dir "$aside"
  else
    mv "$stage" "$target"
  fi
  if [ -d "$app" ] && [ ! -L "$app" ]; then
    # An install from before the versioned layout: $app is a real directory. The new version is
    # already safe under versions/, so the directory is moved aside and the link is renamed over
    # $app. A mothership running from the old directory keeps its open files through both renames,
    # and a colony's mounts follow the directory's inode to its new name; its recorded paths name
    # $app, which from the swap onwards resolve to the new version — the same break a normal update
    # makes, but without deleting anything that was open. The old directory is deleted only once
    # nothing can still reach it, and goes straight back if the swap fails.
    echo "==> migrating the old layout: $app becomes a link to versions/$build_version"
    legacy=$(free_name "${root:?}/.app.legacy")
    mv "$app" "$legacy"
    if ! link_app_to "$build_version"; then
      mv "$legacy" "$app"
      echo "could not point $app at versions/$build_version; the old install was kept" >&2
      exit 1
    fi
    reclaim_dir "$legacy"
  else
    link_app_to "$build_version"
  fi
  prune_old_versions
}

# Housekeeping for a manual install: keep the version just installed and the one before it (a
# mothership may still be running that one), delete the rest — except whatever is still in use. A
# colony bind-mounts its agent and plugins read-only out of its version directory, and "previous"
# is only a one-generation guess: two installs without a mothership restart can leave an older
# version mounted still. Deleting a version in use is deferred the same way a replaced copy is —
# it stays for a later run of this installer, or the app's startup prune, which knows which
# colonies still run which version; this pass only stops unattended installs piling up.
prune_old_versions() {
  pruned=""
  kept=""
  for dir in "$versions"/*/; do
    [ -d "$dir" ] || continue # nothing matched the glob
    name=$(basename "$dir")
    if [ "$name" = "$build_version" ]; then continue; fi
    if [ -n "$previous" ] && [ "$name" = "$previous" ]; then continue; fi
    if dir_in_use "$dir"; then
      kept="$kept $name"
      continue
    fi
    rm -rf "${versions:?}/$name"
    pruned="$pruned $name"
  done
  [ -z "$pruned" ] || echo "==> pruned old versions:$pruned"
  [ -z "$kept" ] || echo "==> left for later, still in use:$kept"
}

echo "==> colonizer-agentd (static musl build inside a microVM)"
prebuilt_bin colonizer-agentd || MSB="$msb" "$root/scripts/build-agentd.sh"

echo "==> rtk (static musl build inside a microVM, for colonies that switch on compact command output)"
prebuilt_bin rtk || MSB="$msb" "$root/scripts/build-rtk.sh"

echo "==> agent modules"
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

# Name this build, so --install can install it as versions/<name> and the same value can be recorded
# in dist/VERSION and in the binary itself (crates/colonizer/build.rs reads COLONIZER_BUILD_VERSION).
# The release workflow sets VERSION; a checkout names itself with git describe over version tags; a
# tarball download has no git history, so it falls back to the crate's own version, then a
# dev-unknown marker (check_version_token refuses a bare dev, so the marker keeps the dev- prefix
# like any other unnamed build). A describe that found no version tag answers with a commit id,
# which is still a dev build, so it takes the dev- prefix too.
build_version=${COLONIZER_BUILD_VERSION:-}
[ -n "$build_version" ] || build_version=${VERSION:-}
if [ -z "$build_version" ]; then
  build_version=$(git -C "$root" describe --tags --always --match 'v[0-9]*' 2>/dev/null) || build_version=""
fi
case $build_version in
  v[0-9]*|dev-*) ;;
  ?*) build_version="dev-$build_version" ;;
  *) build_version="" ;;
esac
if [ -z "$build_version" ]; then
  crate_version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/crates/colonizer/Cargo.toml" | head -1)
  case $crate_version in [0-9]*) build_version="v$crate_version" ;; esac
fi
[ -n "$build_version" ] || build_version=dev-unknown

echo "==> harness"
if ! prebuilt_bin colonizer; then
  # build.rs stamps the binary with this same version, so what the app prints matches dist/VERSION.
  COLONIZER_BUILD_VERSION="$build_version" cargo build --release -p colonizer-harness --manifest-path "$root/Cargo.toml"
  mkdir -p "$dist/bin"
  install -m 755 "$root/target/release/colonizer" "$dist/bin/colonizer"
fi

# The Rust side reads this to say what it is, and --install uses it as the version directory name.
printf '%s\n' "$build_version" > "$dist/VERSION"

if [ "$install_app" = 1 ]; then
  app="$HOME/.local/share/colonizer/app"
  root=$(dirname "$app")
  versions="$root/versions"
  check_version_token "$build_version"
  # Remember what $app points at now, before it is repointed: that version is spared by pruning as
  # the one a running mothership may still be. A real directory (the layout before versions/) is not
  # kept, because migration moves it aside and it is only deleted once nothing reads it any more.
  previous=""
  if [ -L "$app" ]; then
    previous=$(readlink "$app")
    previous=${previous##*/}
  fi
  case $previous in
    ''|"$build_version") previous="" ;;
    *) [ -d "$versions/$previous" ] || previous="" ;;
  esac
  echo "==> installing $build_version to $app"
  install_unpacked
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
