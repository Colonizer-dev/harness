#!/bin/sh
# Drives scripts/install-release.sh, unmodified, against fake releases served over file://, and checks
# what GitHub issue #90 asks for: an install interrupted at any point must leave a working colonizer,
# and the next install must recover from whatever the interrupted one left behind.
#
# Runs offline. A fake release has no modules/agents/*/fetch-at-install markers, so the installer
# downloads nothing but the archive, and guest_claude only runs on a Mac.
#
# Needs Linux x86_64 with /dev/kvm readable and writable, because that is the installer's own platform
# gate and this test drives the real script. It skips with a message anywhere else.
set -eu

repo=$(cd "$(dirname "$0")/../.." && pwd)
installer=$repo/scripts/install-release.sh

if [ "$(uname -s)-$(uname -m)" != "Linux-x86_64" ] || [ ! -r /dev/kvm ] || [ ! -w /dev/kvm ]; then
  echo "skip: the installer only runs on Linux x86_64 with /dev/kvm, and this test drives the real installer"
  exit 0
fi

# Everything lives under one scratch directory: the fake releases, the fake HOME, the PATH shims and
# the installer's own temp dirs. TMPDIR is exported so a killed install leaks here, not into /tmp.
scratch=$(mktemp -d)
mkdir -p "$scratch/tmp"
export TMPDIR="$scratch/tmp"
trap 'rm -rf "$scratch"' EXIT

# The default install path under the fake HOME is what we want to exercise, so an inherited
# COLONIZER_APP must not move it.
unset COLONIZER_APP

home=$scratch/home
app=$home/.local/share/colonizer/app
versions=$scratch/versions
shims=$scratch/shims
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
# real ones, and the fake app is a script that says which version it is.
fake_release() { # version dir
  version=$1
  dir=$2
  mkdir -p "$dir/colonizer/bin"
  printf '#!/bin/sh\necho colonizer %s\n' "$version" > "$dir/colonizer/bin/colonizer"
  chmod 755 "$dir/colonizer/bin/colonizer"
  printf '%s\n' "$version" > "$dir/colonizer/VERSION"
  tar -C "$dir" -czf "$dir/colonizer-linux-x86_64.tar.gz" colonizer
  (cd "$dir" && sha256sum colonizer-linux-x86_64.tar.gz > SHA256SUMS)
}

install_from_release() { # release-url
  HOME="$home" COLONIZER_RELEASE_URL="file://$1" sh "$installer" > "$log" 2>&1
}

must_install() { # release-url label
  install_from_release "$1" || bad "$2: the installer exited non-zero"
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

fake_release 1 "$versions/v1"
fake_release 2 "$versions/v2"

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

note "all checks passed"
