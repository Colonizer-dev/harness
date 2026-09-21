#!/bin/sh
# Drives the --install swap logic in scripts/install.sh, unmodified, with a fake HOME and fake dist
# trees, and checks what GitHub issue #102 asks for: installing must never leave
# ~/.local/share/colonizer/app missing, a legacy directory install must migrate to the symlink
# layout, and the next install must recover from whatever an interrupted one left behind.
#
# scripts/install.sh builds the world before its --install block, so the test cannot run the script
# itself without a full end-to-end build. Instead it extracts the swap helpers (relink, restore_app,
# cleanup_install, swap_app) from the real file and drives them the way the --install block does.
# Runs offline, with no platform gate: the swap is plain cp/mv/ln.
set -eu

repo=$(cd "$(dirname "$0")/../.." && pwd)
installer=$repo/scripts/install.sh

# The swap helpers, verbatim from the installer. Each helper is one contiguous
# `name() {` ... `}` block, so awk can lift them out; the count below keeps a
# refactor that breaks that shape from silently testing nothing.
funcs=$(awk '/^(relink|restore_app|cleanup_install|swap_app)\(\) \{/,/^\}/' "$installer")
[ "$(printf '%s\n' "$funcs" | grep -c '() {')" = 4 ] ||
  { echo "FAIL: expected 4 swap helpers in $installer, got: $(printf '%s\n' "$funcs" | grep '() {' || true)"; exit 1; }
eval "$funcs"

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

home=$scratch/home
app=$home/.local/share/colonizer/app
log=$scratch/swap.log

note() { printf '%s\n' "$*"; }

bad() {
  printf 'FAIL: %s\n' "$1"
  echo "--- $home/.local/share/colonizer"
  ls -la "$home/.local/share/colonizer" 2>&1 || true
  echo "--- $home/.local/bin"
  ls -la "$home/.local/bin" 2>&1 || true
  exit 1
}

# A fake dist tree: a colonizer that says which version it is.
fake_dist() { # version dir
  mkdir -p "$2/bin"
  printf '#!/bin/sh\necho colonizer %s\n' "$1" > "$2/bin/colonizer"
  chmod 755 "$2/bin/colonizer"
  printf '%s\n' "$1" > "$2/VERSION"
}

# The --install block's own sequence: fresh traps, the swap, then the bin link.
do_install() { # dist
  parked=
  swap_app "$1" > "$log" 2>&1
  mkdir -p "$home/.local/bin"
  relink "$app/bin/colonizer" "$home/.local/bin/colonizer"
}

colonizer_output() {
  "$home/.local/bin/colonizer" 2>&1 || true
}

expect_colonizer() { # label wanted
  got=$(colonizer_output)
  [ "$got" = "$2" ] || bad "$1: expected colonizer to print ($2), but it printed ($got)"
}

expect_symlink_app() { # label
  [ -L "$app" ] || bad "$1: expected $app to be a symlink, but ls says: $(ls -ld "$app" 2>&1)"
}

expect_no_leftovers() { # label
  if [ -e "$app.new" ] || [ -L "$app.new" ] || [ -e "$app.old" ] || [ -L "$app.old" ]; then
    bad "$1: expected no app.new or app.old beside the app, found: $(ls -la "$home/.local/share/colonizer" 2>&1)"
  fi
}

count_slots() {
  n=0
  for d in "$home/.local/share/colonizer/app-a" "$home/.local/share/colonizer/app-b"; do
    [ -d "$d" ] && n=$((n + 1))
  done
  echo "$n"
}

expect_one_slot() { # label
  [ "$(count_slots)" = 1 ] ||
    bad "$1: expected one version directory beside the app symlink, found $(count_slots)"
}

fresh_home() {
  rm -rf "$home"
  mkdir -p "$home"
}

fake_dist 1 "$scratch/dist1"
fake_dist 2 "$scratch/dist2"

# 1. A fresh install: the app is a symlink, colonizer runs through it, nothing is left over.
note "== fresh install of v1"
fresh_home
do_install "$scratch/dist1"
expect_colonizer "fresh install" "colonizer 1"
expect_symlink_app "fresh install"
expect_no_leftovers "fresh install"
expect_one_slot "fresh install"
note "ok: fresh install prints colonizer 1 through a symlinked app"

# 2. An upgrade: the new version takes over and the old slot goes away.
note "== upgrade from v1 to v2"
do_install "$scratch/dist2"
expect_colonizer "upgrade" "colonizer 2"
expect_symlink_app "upgrade"
expect_no_leftovers "upgrade"
expect_one_slot "upgrade"
note "ok: upgrade prints colonizer 2 and leaves one slot"

# 3. The legacy layout: a real directory at $app migrates onto a symlink, keeping no backup.
note "== upgrade from a legacy directory install"
fresh_home
mkdir -p "$app/bin"
printf '#!/bin/sh\necho colonizer 0\n' > "$app/bin/colonizer"
chmod 755 "$app/bin/colonizer"
do_install "$scratch/dist2"
expect_colonizer "legacy migration" "colonizer 2"
expect_symlink_app "legacy migration"
expect_no_leftovers "legacy migration"
expect_one_slot "legacy migration"
note "ok: legacy directory migrated to a symlink printing colonizer 2"

# 4. Recovery: $app.old holding the last working copy and $app not resolving -- gone, or a dangling
#    symlink -- must be put back first, and then installed over.
note "== recovery from app.old with app gone or a dangling symlink"
abandoned_home() {
  fresh_home
  mkdir -p "$app.old/bin" "$home/.local/bin"
  printf '#!/bin/sh\necho colonizer 1\n' > "$app.old/bin/colonizer"
  chmod 755 "$app.old/bin/colonizer"
  ln -s "$app/bin/colonizer" "$home/.local/bin/colonizer"
}

abandoned_home
do_install "$scratch/dist2"
grep -q "put back the app an interrupted install left at" "$log" ||
  bad "restore with app.old alone: expected the swap to put the parked app back"
expect_colonizer "restore with app.old alone" "colonizer 2"
expect_no_leftovers "restore with app.old alone"
expect_one_slot "restore with app.old alone"
note "ok: app.old with nothing at app was put back, and the install finished over it"

abandoned_home
ln -s app-a "$app"
do_install "$scratch/dist2"
grep -q "put back the app an interrupted install left at" "$log" ||
  bad "restore behind a dangling symlink: expected the swap to put the parked app back"
expect_colonizer "restore behind a dangling symlink" "colonizer 2"
expect_no_leftovers "restore behind a dangling symlink"
expect_one_slot "restore behind a dangling symlink"
note "ok: app.old behind a dangling app symlink was put back, and the install finished over it"

# 5. Recovery from the old scheme's staging: an install killed between `rm -rf $app` and
#    `mv $app.new $app` leaves the new tree at $app.new with nothing at $app.
note "== recovery from a stale app.new left by the old scheme"
fresh_home
mkdir -p "$(dirname "$app")" "$home/.local/bin"
fake_dist 1 "$app.new"
ln -s "$app/bin/colonizer" "$home/.local/bin/colonizer"
do_install "$scratch/dist2"
grep -q "put back the app an interrupted install left at" "$log" ||
  bad "restore from stale app.new: expected the swap to put the staged copy back"
expect_colonizer "restore from stale app.new" "colonizer 2"
expect_symlink_app "restore from stale app.new"
expect_no_leftovers "restore from stale app.new"
expect_one_slot "restore from stale app.new"
note "ok: stale app.new was moved into place, and the install finished over it"

# 6. The traps' own rule, without signals: a parked backup is restored when $app is missing, and
#    left alone when $app is already there.
note "== cleanup_install restores a parked app only when app is missing"
fresh_home
mkdir -p "$(dirname "$app")"
fake_dist 1 "$app.old"
parked=$app.old
cleanup_install
[ -d "$app" ] || bad "cleanup with app missing: expected the parked copy back at $app"
[ ! -e "$app.old" ] || bad "cleanup with app missing: expected the parked copy moved, not copied"
note "ok: cleanup put the parked app back when app was missing"

fake_dist 1 "$app"
mkdir -p "$app.old/bin"
parked=$app.old
cleanup_install
[ -d "$app.old" ] || bad "cleanup with app present: expected the parked copy left alone"
note "ok: cleanup left the parked app alone when app was present"

note "all checks passed"
