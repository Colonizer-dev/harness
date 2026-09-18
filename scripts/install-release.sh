#!/bin/sh
# Installs a prebuilt Colonizer release. Published with every release as `install.sh`, and served at
# https://colonizer.dev/install.sh:
#
#   curl -fsSL https://colonizer.dev/install.sh | sh
#   curl -fsSL https://colonizer.dev/install.sh | sh -s -- --pull-image
#
# It downloads the app for this machine from the GitHub release, checks it against the release's
# SHA256SUMS, installs it as ~/.local/share/colonizer/versions/<version> and points the
# ~/.local/share/colonizer/app symlink at it; ~/.local/bin/colonizer links through the symlink. Every
# version installs into its own directory and is never modified again, so updating repoints a symlink
# instead of replacing the directory a running mothership (and the colonies that mount their agent,
# plugins and vendored tools read-only from it) booted from. Running it again installs the new version
# alongside the old one; afterwards all versions are pruned except the current one, the one before
# it, and anything a colony still has mounted. A directory that has to go while something still
# reads it — the copy a reinstall replaces, a version in use, an old app directory — is renamed
# aside (which a mount survives) and its deletion is deferred to a later run of this installer, or
# to the app's startup prune. An install made by an older installer (a real app directory) is
# migrated to the layout. Settings (~/.config/colonizer) and colonies (~/.local/share/colonizer)
# are never touched.
#
# Anthropic's code is not in a release, because it is not ours to redistribute, so two things come from
# Anthropic's own channels instead, each checked before it is used:
# - the Claude Agent SDK the agent module runs, from the npm registry, against the checksum the release
#   recorded from package-lock.json (fetch-at-install);
# - on a Mac, the Linux build of Claude Code that colonies run, against Anthropic's manifest. A colony is
#   a Linux microVM, so the Mac's own binary cannot run in it.
#
#   COLONIZER_VERSION=v0.1.0   install that release instead of the latest
#   COLONIZER_APP=<dir>        install the app there instead of ~/.local/share/colonizer/app
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
  trap 'rm -rf "$tmp"' EXIT INT TERM

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
  # The version names the directory the app installs into, so it is checked before anything else
  # downloads: only a plain v…/dev-… token gets past this.
  check_version_token "$installed"
  root=$(dirname "$app")
  versions="$root/versions"

  for record in "$tmp"/unpack/colonizer/modules/agents/*/fetch-at-install; do
    if [ -f "$record" ]; then fetch_at_install "$(dirname "$record")"; fi
  done
  if [ "$platform" = darwin-arm64 ]; then
    # The previous install's copy is read through $app, which still names the last install at this
    # point: the old real directory, or the symlink into versions/ from an earlier run of this script.
    guest_claude "$tmp/unpack/colonizer/bin/claude-guest" "$app/bin/claude-guest"
  fi

  # Remember what $app points at now, before it is repointed: that version is spared by pruning as the
  # one a running mothership may still be. A real directory (the layout before versions/) is not kept,
  # because migration moves it aside and it is only deleted once nothing reads it any more.
  previous=""
  if [ -L "$app" ]; then
    previous=$(readlink "$app")
    previous=${previous##*/}
  fi
  case $previous in
    ''|"$installed") previous="" ;;
    *) [ -d "$versions/$previous" ] || previous="" ;;
  esac

  install_unpacked "$tmp/unpack/colonizer"

  mkdir -p "$HOME/.local/bin"
  ln -sf "$app/bin/colonizer" "$HOME/.local/bin/colonizer"
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

# A version is used as a directory name under versions/, so only a plain v…/dev-… token is accepted:
# no slash or whitespace to escape versions/, no leading dot to hide among the staging names. Colonies
# mount read-only out of the directory this names, so it is worth being strict about it.
check_version_token() {
  case $1 in
    v[0-9]*|dev-*) ;;
    *) fail "'$1' is not a plain v…/dev-… version; refusing to use it as an install directory" ;;
  esac
  case $1 in
    *[!A-Za-z0-9._-]*) fail "'$1' contains characters that are not safe in a directory name" ;;
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
    say "$1 is still in use by a colony or a running app; it will be removed once nothing reads it"
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
  [ -z "$iu_reclaimed" ] || say "reclaimed leftovers from earlier runs:$iu_reclaimed"
  [ -z "$iu_kept" ] || say "left for later, still in use:$iu_kept"
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

# Repoint $app at versions/<version>. Where mv understands -T (GNU), the new link is staged under a
# temp name and renamed over $app, so at every instant $app is either the old link or the new one,
# never missing and never dangling. Plain mv cannot do this: it follows $app (which points at a
# directory) and would file the new link inside the old version instead of replacing it. macOS's mv
# has no -T, so there the link is made directly with ln -sfn — unlink and re-link in quick
# succession rather than one rename. mv -T --help tells a mv that lacks -T from one whose rename
# failed for real: it exits 0 wherever the option exists, so only there is the failure returned and
# the old state left standing for the caller to restore.
link_app_to() {
  rm -rf "${root:?}/.app.new" # a leftover from an interrupted run; this name is only ever a link
  ln -sfn "versions/$1" "$root/.app.new"
  if mv -Tf "$root/.app.new" "$app" 2>/dev/null; then
    return 0
  elif mv -T --help >/dev/null 2>&1; then
    return 1 # this mv knows -T, so the rename above failed for real
  fi
  ln -sfn "versions/$1" "$app"
  rm -f "${root:?}/.app.new"
}

# Install <source> as versions/<version> and point $app at it. A version directory is written exactly
# once, by renaming a fully staged copy into place, and afterwards only ever renamed or deleted whole.
# A mothership holds the directory it booted from open (current_exe resolves through the symlink to
# versions/<version>), and colonies mount their agent, plugins and vendored tools read-only out of it,
# so nothing that any running process can still reach is ever modified or removed in place:
install_unpacked() {
  src=$1
  mkdir -p "$versions"
  # Staging happens inside versions/ so the final move is a same-filesystem rename, and a stage left
  # by an interrupted run is cleaned up first. The stage name starts with a dot and is never pointed
  # at, so it is always safe to remove. Before anything else, the deferred leftovers of earlier runs
  # go, each only if nothing holds it any more.
  stage="$versions/.$installed.new"
  reclaim_leftovers
  rm -rf "$stage"
  mv "$src" "$stage"
  target="$versions/$installed"
  if [ ! -d "$target" ]; then
    # A previous run died between renaming this version aside and moving the staged copy in, which
    # leaves $app dangling; a copy renamed aside is the newest one there is, so put the newest
    # back — the highest generation, since free_name hands them out in order. A rename never
    # disturbs a mount or an open file, so this is safe whatever still holds the copy; any older
    # generation left where it is goes once reclaim_leftovers finds nothing holding it.
    aside=""
    aside_n=0
    for candidate in "${versions:?}/.$installed".old*; do
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
    # is re-run to repair a release. The old copy is renamed aside — a rename keeps every colony
    # mount and open file on it alive — and the staged copy takes its name. The renamed copy is
    # deleted only once nothing can still reach it; otherwise it stays as a dot-prefixed leftover
    # for a later run of this installer, or the app's startup prune, to reclaim. If the new copy
    # cannot be moved in, the old one goes straight back, so only a kill between the two renames
    # can leave $app dangling — and the recovery above puts the renamed copy back on the next run.
    aside=$(free_name "${versions:?}/.$installed.old")
    mv "$target" "$aside"
    if ! mv "$stage" "$target"; then
      mv "$aside" "$target"
      fail "could not move the new $installed into place; the previous copy was kept"
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
    say "migrating the old layout: $app becomes a link to versions/$installed"
    legacy=$(free_name "${root:?}/.app.legacy")
    mv "$app" "$legacy"
    if ! link_app_to "$installed"; then
      mv "$legacy" "$app"
      fail "could not point $app at versions/$installed; the old install was kept"
    fi
    reclaim_dir "$legacy"
  else
    link_app_to "$installed"
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
    if [ "$name" = "$installed" ]; then continue; fi
    if [ -n "$previous" ] && [ "$name" = "$previous" ]; then continue; fi
    if dir_in_use "$dir"; then
      kept="$kept $name"
      continue
    fi
    rm -rf "${versions:?}/$name"
    pruned="$pruned $name"
  done
  [ -z "$pruned" ] || say "pruned old versions:$pruned"
  [ -z "$kept" ] || say "left for later, still in use:$kept"
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
