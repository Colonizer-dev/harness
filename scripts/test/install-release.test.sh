#!/bin/sh
# Drives scripts/install-release.sh, unmodified, against fake releases served over file://, and checks
# what GitHub issue #90 asks for: an install interrupted at any point must leave a working colonizer,
# and the next install must recover from whatever the interrupted one left behind.
#
# Runs offline. A fake release has no modules/agents/*/fetch-at-install markers and its node.lock
# points at a fake runtime tarball over file://, so the installer downloads nothing but local files.
# A macOS fake release also carries a fake guest Claude Code build, so guest_claude runs against that
# too; the macOS signing cases below shim `uname` so the installer takes its Darwin path on Linux.
set -eu

repo=$(cd "$(dirname "$0")/../.." && pwd)
installer=$repo/scripts/install-release.sh

# Needs Linux x86_64 (sha256sum, /proc, and the installer's own platform gate). The Linux-install
# cases also need /dev/kvm readable and writable, because that is the installer's own platform gate;
# the macOS signing cases shim `uname` so the installer takes its Darwin path, which needs no kvm, so
# they still run on a host without it.
if [ "$(uname -s)-$(uname -m)" != "Linux-x86_64" ]; then
  echo "skip: this test drives the installer on Linux x86_64 (sha256sum, /proc)"
  exit 0
fi
have_kvm=0
if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then have_kvm=1; fi

# Everything lives under one scratch directory: the fake releases, the fake HOME, the PATH shims and
# the installer's own temp dirs. TMPDIR is exported so a killed install leaks here, not into /tmp.
scratch=$(mktemp -d)
mkdir -p "$scratch/tmp"
export TMPDIR="$scratch/tmp"
# A background process started from a slot stands in for a running mothership or a colony's msb; it
# has to be killed before the scratch dir it runs from is removed.
bg_pids=
trap 'stop_bg; rm -rf "$scratch"' EXIT

# The default install path under the fake HOME is what we want to exercise, so an inherited
# COLONIZER_APP must not move it, nor an inherited signing identity drive the macOS cases.
unset COLONIZER_APP COLONIZER_CODESIGN_IDENTITY

home=$scratch/home
app=$home/.local/share/colonizer/app
versions=$scratch/versions
shims=$scratch/shims
darwin_shims=$scratch/darwin-shims
codesign_log=$scratch/codesign.log
counter=$scratch/shim-count
log=$scratch/install.log

# The real mv and ln, captured before any shim directory goes on PATH, so the shims can exec the real
# tools instead of recursing into themselves.
real_mv=$(command -v mv)
real_ln=$(command -v ln)

note() { printf '%s\n' "$*"; }

# The first failed assertion stops the test, and dumps enough state to see what the installer left.
bad() {
  printf 'FAIL: %s\n' "$1"
  echo "--- $home/.local/share/colonizer"
  ls -la "$home/.local/share/colonizer" 2>&1 || true
  echo "--- $home/.local/bin"
  ls -la "$home/.local/bin" 2>&1 || true
  echo "--- installer log ($log)"
  cat "$log" 2>&1 || true
  exit 1
}

# Builds a release the installer accepts: the archive name and the sha256sum-style SHA256SUMS are the
# real ones, and the fake app is a script that says which version it is. Since the guest_node step,
# every release also carries a node.lock pinning the colony Node.js runtime, and the installer
# refuses an archive without one — so the fake carries a lock pointing at a fake runtime tarball
# over file://, keeping the test offline. A darwin-arm64 release carries, in addition, the guest
# Claude Code build and the arm64 Node runtime the installer fetches on that platform (guest_claude,
# guest_node), both faked over file:// the same way.
make_fake_node() { # dir
  dir=$1
  mkdir -p "$dir/node-payload/node-fake/bin"
  printf '#!/bin/sh\necho node-fake\n' > "$dir/node-payload/node-fake/bin/node"
  chmod 755 "$dir/node-payload/node-fake/bin/node"
  tar -C "$dir/node-payload" -cJf "$dir/node.tar.xz" node-fake
  node_sha=$(sha256sum "$dir/node.tar.xz" | cut -d' ' -f1)
  node_url="file://$dir/node.tar.xz"
  printf '#!/bin/sh\necho claude-guest-fake\n' > "$dir/claude-guest-fake"
  claude_sha=$(sha256sum "$dir/claude-guest-fake" | cut -d' ' -f1)
  claude_url="file://$dir/claude-guest-fake"
}

fake_release() { # version dir [platform]
  version=$1
  dir=$2
  platform=${3:-linux-x86_64}
  mkdir -p "$dir/colonizer/bin"
  printf '#!/bin/sh\necho colonizer %s\n' "$version" > "$dir/colonizer/bin/colonizer"
  chmod 755 "$dir/colonizer/bin/colonizer"
  printf '%s\n' "$version" > "$dir/colonizer/VERSION"
  printf 'node 99 linux-x64 runtime %s %s\n' "$node_sha" "$node_url" > "$dir/colonizer/node.lock"
  if [ "$platform" = darwin-arm64 ]; then
    printf 'node 99 linux-arm64 runtime %s %s\n' "$node_sha" "$node_url" >> "$dir/colonizer/node.lock"
    printf 'claude-code 99 linux-arm64 agent %s %s\n' "$claude_sha" "$claude_url" > "$dir/colonizer/claude-code.lock"
  fi
  tar -C "$dir" -czf "$dir/colonizer-$platform.tar.gz" colonizer
  (cd "$dir" && sha256sum "colonizer-$platform.tar.gz" > SHA256SUMS)
}

install_from_release() { # release-url
  HOME="$home" COLONIZER_RELEASE_URL="file://$1" sh "$installer" > "$log" 2>&1
}

must_install() { # release-url label
  install_from_release "$1" || bad "$2: the installer exited non-zero"
}

# A release whose tarball no longer matches the SHA256SUMS it ships: one byte appended after the
# sums are written, exactly what a truncated or tampered download looks like to the installer.
corrupt_release() { # version dir
  fake_release "$1" "$2"
  printf 'x' >> "$2/colonizer-linux-x86_64.tar.gz"
}

# Runs the installer expecting it to refuse, then insists the log says why and nothing was installed.
refuse_install() { # release-url label
  if install_from_release "$1"; then
    bad "$2: the installer exited zero on a release its own SHA256SUMS do not match"
  fi
  grep -q "checksum mismatch" "$log" ||
    bad "$2: expected the installer to report a checksum mismatch; its log says: $(cat "$log")"
}

# The macOS signing cases shadow uname so the installer takes its Darwin path on this Linux host
# (Darwin -s, arm64 -m; anything else delegates to the real uname), and codesign so no signature is
# made: the shim appends its arguments to a log, and exits 1 when CODESIGN_FAIL is set. PATH carries
# these two shims only, so the real mv/ln/tar/curl run.
make_darwin_shims() {
  rm -rf "$darwin_shims"
  mkdir -p "$darwin_shims"
  real_uname=$(command -v uname)
  cat > "$darwin_shims/uname" <<EOF
#!/bin/sh
case "\$1" in
  -s) echo Darwin ;;
  -m) echo arm64 ;;
  *) exec "$real_uname" "\$@" ;;
esac
EOF
  cat > "$darwin_shims/codesign" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >> "$CODESIGN_LOG"
if [ -n "${CODESIGN_FAIL:-}" ]; then exit 1; fi
exit 0
EOF
  chmod 755 "$darwin_shims/uname" "$darwin_shims/codesign"
}

# A darwin-shimmed install. $2 is the identity in the environment (empty means none); $codesign_fail,
# set by the caller, makes the codesign shim fail. The log starts empty each run, so a grep sees only
# this run's calls.
install_darwin() { # release-url identity
  : > "$codesign_log"
  HOME="$home" COLONIZER_RELEASE_URL="file://$1" \
    COLONIZER_CODESIGN_IDENTITY="${2:-}" CODESIGN_LOG="$codesign_log" CODESIGN_FAIL="${codesign_fail:-}" \
    PATH="$darwin_shims:$PATH" sh "$installer" > "$log" 2>&1
}

# An install run with mv and ln shadowed by shims that share one counter file: every call is counted,
# and the k-th call (SHIM_KILL_AT) kills the installer before exec-ing the real tool. The run exits
# non-zero; the caller decides whether that is a problem.
interrupt_install() { # release-url
  rc=0
  HOME="$home" COLONIZER_RELEASE_URL="file://$1" PATH="$shims:$PATH" \
    SHIM_COUNT=$counter SHIM_KILL_AT=$kill_at SHIM_SIG=$signal \
    sh "$installer" > "$log" 2>&1 || rc=$?
  return "$rc"
}

# Installs the shims. SHIM_KILL_AT and SHIM_SIG are read at run time, so one set of shims serves every
# interruption point; make_shims just resets the counter.
make_shims() {
  rm -rf "$shims"
  mkdir -p "$shims"
  : > "$counter"
  for tool in mv ln; do
    case $tool in
      mv) real=$real_mv ;;
      ln) real=$real_ln ;;
    esac
    cat > "$shims/$tool" <<EOF
#!/bin/sh
n=\$(cat "\$SHIM_COUNT" 2>/dev/null || echo 0)
n=\$((n + 1))
echo "\$n" > "\$SHIM_COUNT"
[ "\$n" = "\$SHIM_KILL_AT" ] && kill -s "\$SHIM_SIG" "\$PPID"
exec "$real" "\$@"
EOF
    chmod 755 "$shims/$tool"
  done
}

# Runs colonizer the way a user would, through ~/.local/bin. A dangling bin link fails here, which is
# exactly what we want to see.
colonizer_output() {
  "$home/.local/bin/colonizer" 2>&1 || true
}

expect_colonizer() { # label wanted [wanted...]
  label=$1
  shift
  got=$(colonizer_output)
  for want in "$@"; do
    if [ "$got" = "$want" ]; then
      return 0
    fi
  done
  bad "$label: expected colonizer to print one of ($*), but it printed ($got)"
}

expect_symlink_app() { # label
  [ -L "$app" ] || bad "$1: expected $app to be a symlink, but ls says: $(ls -ld "$app" 2>&1)"
}

# Neither the parked old app nor a half-renamed link may survive a completed install.
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

# The same helper the installer uses, lifted out of it verbatim, so the test starts a process the
# installer will see as running out of a slot. Each helper is one contiguous `name() {` ... `}` block,
# so awk can lift it out; the count keeps a refactor that breaks that shape from silently testing
# nothing.
slot_func=$(awk '/^slot_pids\(\) \{/,/^\}/' "$installer")
[ "$(printf '%s\n' "$slot_func" | grep -c '() {')" = 1 ] ||
  { echo "FAIL: expected the slot_pids helper in $installer, got: $(printf '%s\n' "$slot_func" | grep '() {' || true)"; exit 1; }
eval "$slot_func"

src_sleep=$(command -v sleep || true)
[ -n "$src_sleep" ] && [ -x "$src_sleep" ] || src_sleep=/bin/sleep

start_in_slot() { # slot
  mkdir -p "$1/vendor/microsandbox/bin"
  cp "$src_sleep" "$1/vendor/microsandbox/bin/msb"
  "$1/vendor/microsandbox/bin/msb" 600 &
  bg_pids="$bg_pids $!"
  i=0
  while [ -z "$(slot_pids "$1")" ] && [ "$i" -lt 50 ]; do
    sleep 0.1
    i=$((i + 1))
  done
  [ -n "$(slot_pids "$1")" ] || bad "could not start a process from $1"
}

# The same process, but started through a symlink outside the slot — exactly how the mothership runs
# (`~/.local/bin/colonizer`, or `$dir/app/bin/colonizer` after an update restarts itself). `ps` shows
# the symlink, not the slot, so only the real-executable check (`/proc/<pid>/exe` on Linux, `lsof` on
# macOS) finds it; finding it is what makes the installer refuse the slot.
start_in_slot_via_symlink() { # slot
  mkdir -p "$scratch/bin" "$1/vendor/microsandbox/bin"
  cp "$src_sleep" "$1/vendor/microsandbox/bin/msb"
  ln -sf "$1/vendor/microsandbox/bin/msb" "$scratch/bin/colonizer"
  "$scratch/bin/colonizer" 600 &
  bg_pids="$bg_pids $!"
  i=0
  while [ -z "$(slot_pids "$1")" ] && [ "$i" -lt 50 ]; do
    sleep 0.1
    i=$((i + 1))
  done
  [ -n "$(slot_pids "$1")" ] || bad "could not start a process from $1 through a symlink"
}

# Kills and reaps what start_in_slot left running, so a later case does not see the previous case's
# slot as still in use.
stop_bg() {
  for p in $bg_pids; do kill "$p" 2>/dev/null || true; done
  for p in $bg_pids; do wait "$p" 2>/dev/null || true; done
  bg_pids=
}

make_fake_node "$versions"
fake_release 1 "$versions/v1"
fake_release 2 "$versions/v2"
fake_release 1 "$versions/v1-darwin" darwin-arm64
fake_release 2 "$versions/v2-darwin" darwin-arm64

# 0. The macOS signing cases. uname is shimmed to Darwin, so the installer takes its macOS path and
#    re-signs the new host binary with the identity it knows, before switching slots. They need no
#    /dev/kvm — only the Linux cases below do — so they run even on a host where those are skipped.
note "== macOS: the new binary is re-signed before the switch"
make_darwin_shims

expect_signed() { # label identity
  grep -q -- "--force --sign $2 --identifier dev.colonizer.mothership" "$codesign_log" ||
    bad "$1: expected codesign to be called with --sign $2 --identifier dev.colonizer.mothership; the log says: $(cat "$codesign_log" 2>&1)"
}

# The identity in the environment signs the new slot's binary, the switch happens, and the identity is
# recorded beside the app for later cockpit-started updates, which carry no environment of their own.
fresh_home
codesign_fail=
install_darwin "$versions/v1-darwin" "my-darwin-id" || bad "macOS fresh install: the installer exited non-zero"
expect_signed "macOS fresh install" "my-darwin-id"
grep -q "app-a/bin/colonizer" "$codesign_log" ||
  bad "macOS fresh install: expected the new slot's binary signed, the log says: $(cat "$codesign_log")"
expect_colonizer "macOS fresh install" "colonizer 1"
[ "$(readlink "$app")" = "app-a" ] ||
  bad "macOS fresh install: expected the app symlink at app-a, got $(readlink "$app" 2>&1)"
[ "$(cat "$home/.local/share/colonizer/codesign-identity" 2>/dev/null)" = "my-darwin-id" ] ||
  bad "macOS fresh install: expected the identity recorded in codesign-identity"
note "ok: an environment identity signed the new slot and was recorded"

# No identity in the environment, but one recorded beside the app: the install re-signs with it, and
# leaves the recorded file as it is (it did not come from the environment this time).
fresh_home
mkdir -p "$home/.local/share/colonizer"
printf 'recorded-darwin-id\n' > "$home/.local/share/colonizer/codesign-identity"
install_darwin "$versions/v2-darwin" || bad "macOS recorded-identity install: the installer exited non-zero"
expect_signed "macOS recorded-identity install" "recorded-darwin-id"
expect_colonizer "macOS recorded-identity install" "colonizer 2"
[ "$(cat "$home/.local/share/colonizer/codesign-identity")" = "recorded-darwin-id" ] ||
  bad "macOS recorded-identity install: expected the recorded file left untouched"
note "ok: a recorded identity re-signed the new slot with no environment variable"

# codesign fails: the installer exits non-zero, the app symlink still points at the previous slot, the
# new slot is removed, and the previous version still runs.
fresh_home
codesign_fail=
install_darwin "$versions/v1-darwin" "keep-me-id" || bad "macOS v1 install before a failing re-sign: the installer exited non-zero"
expect_colonizer "macOS before a failing re-sign" "colonizer 1"
codesign_fail=1
rc=0
install_darwin "$versions/v2-darwin" "keep-me-id" || rc=$?
[ "$rc" -ne 0 ] || bad "macOS failing re-sign: expected the installer to exit non-zero"
[ "$(readlink "$app" 2>&1)" = "app-a" ] ||
  bad "macOS failing re-sign: expected the app symlink still at app-a, got $(readlink "$app" 2>&1)"
[ ! -d "$home/.local/share/colonizer/app-b" ] ||
  bad "macOS failing re-sign: expected the new slot app-b removed"
expect_colonizer "macOS failing re-sign" "colonizer 1"
grep -q "codesign failed" "$log" ||
  bad "macOS failing re-sign: expected the installer to say codesign failed; it said: $(cat "$log")"
note "ok: a failed re-sign left the previous version installed and running"
codesign_fail=
note "macOS signing checks passed"

if [ "$have_kvm" != 1 ]; then
  echo "skip: the Linux-install cases below need /dev/kvm readable and writable; the macOS cases above ran"
  exit 0
fi

# 1. A fresh install: the app is a symlink, colonizer runs through it, nothing is left over.
note "== fresh install of v1"
fresh_home
must_install "$versions/v1" "fresh install"
expect_colonizer "fresh install" "colonizer 1"
expect_symlink_app "fresh install"
expect_no_leftovers "fresh install"
expect_one_slot "fresh install"
note "ok: fresh install prints colonizer 1 through a symlinked app"

# 2. An upgrade: the new version takes over and the old slot goes away.
note "== upgrade from v1 to v2"
must_install "$versions/v2" "upgrade"
expect_colonizer "upgrade" "colonizer 2"
expect_symlink_app "upgrade"
expect_no_leftovers "upgrade"
expect_one_slot "upgrade"
note "ok: upgrade prints colonizer 2 and leaves one slot"

# 3. The issue's own scenario: an upgrade killed at each mv or ln call. Whatever state the dead
#    installer leaves, ~/.local/bin/colonizer must still run, and the next install must recover.
note "== interrupted upgrades (SIGKILL runs no traps, SIGTERM runs the installer's cleanup traps)"
kill_at=1
while [ "$kill_at" -le 6 ]; do
  for signal in KILL TERM; do
    fresh_home
    must_install "$versions/v1" "clean v1 before interrupting call $kill_at"
    make_shims
    interrupt_install "$versions/v2" || true
    expect_colonizer "$signal at call $kill_at: after the interrupted upgrade" "colonizer 1" "colonizer 2"
    must_install "$versions/v2" "recovery after $signal at call $kill_at"
    expect_colonizer "recovery after $signal at call $kill_at" "colonizer 2"
    expect_no_leftovers "recovery after $signal at call $kill_at"
    expect_one_slot "recovery after $signal at call $kill_at"
    note "ok: upgrade interrupted with SIG$signal at mv/ln call $kill_at: colonizer still ran, next install recovered"
  done
  kill_at=$((kill_at + 1))
done

# 4. The same harness against the layout the pre-symlink installers left behind: $app a real directory
#    with the bin link pointing into it. The difference in expectations is the point of the case:
#    with SIGTERM the new traps run, so colonizer must survive immediately at every kill point; with
#    SIGKILL no trap runs, and an upgrade killed while the old app is parked leaves nothing at $app,
#    so colonizer may be down until the next install. Even then no version may be lost: if $app is
#    missing, the old copy must still be parked at $app.old for restore_app to put back.
note "== upgrades from the old directory layout (real directory at app, not a symlink)"
legacy_home() {
  fresh_home
  mkdir -p "$app/bin" "$home/.local/bin"
  printf '#!/bin/sh\necho colonizer 0\n' > "$app/bin/colonizer"
  chmod 755 "$app/bin/colonizer"
  printf '0\n' > "$app/VERSION"
  ln -s "$app/bin/colonizer" "$home/.local/bin/colonizer"
}

kill_at=1
while [ "$kill_at" -le 6 ]; do
  for signal in KILL TERM; do
    legacy_home
    make_shims
    interrupt_install "$versions/v2" || true
    if [ "$signal" = TERM ]; then
      expect_colonizer "SIGTERM at call $kill_at: traps should have restored the old app" "colonizer 0" "colonizer 2"
    else
      got=$(colonizer_output)
      case $got in
        "colonizer 0" | "colonizer 2") ;;
        *)
          [ -e "$app.old" ] || bad "SIGKILL at call $kill_at: colonizer is down and app.old is gone; no version would be left"
          note "   colonizer was down after SIGKILL at call $kill_at, old app parked at app.old (SIGKILL runs no traps)"
          ;;
      esac
    fi
    must_install "$versions/v2" "recovery after $signal at call $kill_at from the legacy layout"
    expect_colonizer "recovery after $signal at call $kill_at from the legacy layout" "colonizer 2"
    expect_no_leftovers "recovery after $signal at call $kill_at from the legacy layout"
    expect_one_slot "recovery after $signal at call $kill_at from the legacy layout"
    note "ok: legacy-layout upgrade interrupted with SIG$signal at mv/ln call $kill_at: no version lost, next install recovered"
  done
  kill_at=$((kill_at + 1))
done

# 5. The recovery rule on its own, not only as a side effect of the interruption cases: an install
#    that finds $app.old holding the last working copy and $app not resolving -- gone, or a dangling
#    symlink -- must put the copy back rather than delete it, and then install over it.
note "== recovery from app.old with app gone or a dangling symlink"
abandoned_home() {
  fresh_home
  mkdir -p "$app.old/bin" "$home/.local/bin"
  printf '#!/bin/sh\necho colonizer 1\n' > "$app.old/bin/colonizer"
  chmod 755 "$app.old/bin/colonizer"
  ln -s "$app/bin/colonizer" "$home/.local/bin/colonizer"
}

# restore_app says so on stdout, so the log is the direct evidence the rule fired.
expect_restored() { # label
  grep -q "put back the app an interrupted install left at" "$log" ||
    bad "$1: expected the installer to put the parked app back; its log says: $(cat "$log")"
}

abandoned_home
must_install "$versions/v2" "restore with app.old alone and nothing at app"
expect_restored "restore with app.old alone and nothing at app"
expect_colonizer "restore with app.old alone and nothing at app" "colonizer 2"
expect_no_leftovers "restore with app.old alone and nothing at app"
expect_one_slot "restore with app.old alone and nothing at app"
note "ok: app.old with nothing at app was put back, and the install finished over it"

abandoned_home
ln -s app-a "$app"
must_install "$versions/v2" "restore with app.old behind a dangling app symlink"
expect_restored "restore with app.old behind a dangling app symlink"
expect_colonizer "restore with app.old behind a dangling app symlink" "colonizer 2"
expect_no_leftovers "restore with app.old behind a dangling app symlink"
expect_one_slot "restore with app.old behind a dangling app symlink"
note "ok: app.old behind a dangling app symlink was put back, and the install finished over it"

# 6. A running process in the slot an install would replace: the installer refuses, naming the slot
#    and pid(s), and leaves the slot's binary alone — the boot that failed when a restart ran an
#    installer over the slot its msb was still running from. The process is started through a symlink
#    outside the slot, the way the mothership itself runs, so this only passes with the
#    real-executable check and not the command line.
note "== install refused when the target slot is in use"
fresh_home
must_install "$versions/v1" "clean v1 before the in-use target test"
start_in_slot_via_symlink "$home/.local/share/colonizer/app-b"
rc=0
install_from_release "$versions/v2" || rc=$?
[ "$rc" -ne 0 ] || bad "in-use target slot: expected the installer to refuse app-b in use"
grep -q "in use by pid(s)" "$log" ||
  bad "in-use target slot: expected the refusal to name the slot and pid(s); it said: $(cat "$log")"
[ -f "$home/.local/share/colonizer/app-b/vendor/microsandbox/bin/msb" ] ||
  bad "in-use target slot: expected the in-use slot's binary to be left untouched"
expect_colonizer "in-use target slot" "colonizer 1"
note "ok: an install whose target slot is in use was refused and left it untouched"
stop_bg

# 7. A running process in the slot an install would remove: the slot is kept, not deleted, and a note
#    says so. The mothership's own sweep removes it later. Started from the slot directly, so the
#    command-line match sees it — the other half of the check, next to the symlinked case above.
note "== install keeps a previous slot still in use"
fresh_home
must_install "$versions/v1" "clean v1 before the in-use previous test"
start_in_slot "$home/.local/share/colonizer/app-a"
must_install "$versions/v2" "install over an in-use previous slot"
expect_colonizer "keep in-use previous slot" "colonizer 2"
grep -q "keeping the previous version at" "$log" ||
  bad "keep in-use previous slot: expected a note that app-a is kept; it said: $(cat "$log")"
[ -d "$home/.local/share/colonizer/app-a" ] ||
  bad "keep in-use previous slot: expected the in-use app-a to be kept"
expect_symlink_app "keep in-use previous slot"
expect_no_leftovers "keep in-use previous slot"
note "ok: an install kept the previous slot a process was still running from"

# 8. A download that does not match the checksums travels with the release must install nothing. It
#    is the release's own SHA256SUMS that turns a corrupted or swapped download into a refusal rather
#    than a bad install, so the refusal is tested where nothing is installed yet, and over a working
#    install that must be left exactly as it was.
note "== a tarball that does not match SHA256SUMS installs nothing"
stop_bg

corrupt_release 1 "$versions/corrupt-fresh"
fresh_home
refuse_install "$versions/corrupt-fresh" "checksum mismatch on a fresh home"
if [ -e "$app" ] || [ -L "$app" ]; then
  bad "checksum mismatch on a fresh home: an app was left at $app"
fi
if [ -e "$home/.local/bin/colonizer" ] || [ -L "$home/.local/bin/colonizer" ]; then
  bad "checksum mismatch on a fresh home: the bin link was left behind"
fi
[ "$(count_slots)" = 0 ] ||
  bad "checksum mismatch on a fresh home: a version directory was left beside the app"
note "ok: a corrupted download on a fresh home was refused, and no app or link was left"

fresh_home
must_install "$versions/v1" "working install before a corrupted download"
before=$(readlink "$app")
corrupt_release 3 "$versions/corrupt-over"
refuse_install "$versions/corrupt-over" "checksum mismatch over a working install"
expect_colonizer "after a refused upgrade" "colonizer 1"
[ "$(readlink "$app")" = "$before" ] ||
  bad "checksum mismatch over a working install: the app symlink moved to $(readlink "$app")"
expect_symlink_app "checksum mismatch over a working install"
expect_no_leftovers "checksum mismatch over a working install"
expect_one_slot "checksum mismatch over a working install"
note "ok: a corrupted download over a working install left that install exactly as it was"

note "all checks passed"
