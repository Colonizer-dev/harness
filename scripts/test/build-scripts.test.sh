#!/bin/sh
# Drives scripts/build-agentd.sh and scripts/build-rtk.sh, unmodified, inside a fake repo, and checks
# the two ways a build can produce a binary no Linux colony can exec: a here-mode build off Linux,
# which the scripts must refuse before doing anything, and a cargo that yields a Mach-O, which must
# never be installed into dist/bin, stamped as built, or served back off a stamp. Also checks that
# rtk's build stamp names the build mode, so a binary built one way is not served after a run the
# other way.
#
# Runs offline, against stub uname, cargo and msb on PATH; the fake repo is what the scripts expect a
# checkout to look like (crates, Cargo.lock, the vendored rtk source). The ELF happy paths copy a real
# ELF binary from the host, so they need Linux; like install-release.test.sh, the test says so and
# moves on anywhere else, but the refusal and Mach-O cases run everywhere.
set -eu

repo=$(cd "$(dirname "$0")/../.." && pwd)

# Everything lives under one scratch directory: the fake repo, the PATH shims and the shims' call log.
# TMPDIR is exported so a here-mode rtk build (which deliberately builds under ${TMPDIR:-/tmp}) leaks
# here, not into /tmp.
scratch=$(mktemp -d)
mkdir -p "$scratch/tmp"
export TMPDIR="$scratch/tmp"
trap 'rm -rf "$scratch"' EXIT

fake_repo=$scratch/repo
shims=$scratch/shims
log=$scratch/run.log
err=$scratch/run.err
calls=$scratch/tool-calls

agentd_out=$fake_repo/dist/bin/colonizer-agentd
rtk_out=$fake_repo/dist/bin/rtk

# The host's own sh is the ELF the happy paths build "from cargo"; on a Mac it is a Mach-O, and that
# is the skip further down.
real_sh=$(command -v sh)
real_uname=$(command -v uname)

note() { printf '%s\n' "$*"; }

# The first failed assertion stops the test, and dumps what the run said on either stream.
bad() {
  printf 'FAIL: %s\n' "$1"
  echo "--- stdout ($log)"
  cat "$log" 2>&1 || true
  echo "--- stderr ($err)"
  cat "$err" 2>&1 || true
  exit 1
}

# Builds the skeleton the real scripts expect under a repo of their own: build-agentd.sh copies
# crates/colonizer-agentd and Cargo.lock; build-rtk.sh reads its rtk row out of vendor/vendor.lock
# (fields: name, version, any, kind, sha256, url) and untars vendor/cache/rtk-<url basename>, the
# extension-less cache name fetch-vendor.sh uses.
fresh_repo() {
  rm -rf "$fake_repo"
  mkdir -p "$fake_repo/scripts" "$fake_repo/crates/colonizer-agentd"
  cp "$repo/scripts/build-agentd.sh" "$repo/scripts/build-rtk.sh" "$fake_repo/scripts/"
  : > "$fake_repo/Cargo.lock"
  mkdir -p "$fake_repo/vendor/cache"
  printf 'rtk 0.49.0 any source 74b226ab00b8698d5084402893c76d93189493bd332b99d0b1e681d1ef860eb8 https://codeload.github.com/rtk-ai/rtk/tar.gz/refs/tags/v0.49.0\n' \
    > "$fake_repo/vendor/vendor.lock"
  mkdir -p "$scratch/pack/rtk-0.49.0"
  printf '[package]\nname = "rtk"\nversion = "0.49.0"\n' > "$scratch/pack/rtk-0.49.0/Cargo.toml"
  tar -C "$scratch/pack" -czf "$fake_repo/vendor/cache/rtk-v0.49.0" rtk-0.49.0
}

# One set of shims serves every case: they read their behaviour from the environment at run time, so
# a case just sets the variables it wants before calling run_build.
make_shims() {
  rm -rf "$shims"
  mkdir -p "$shims"
  cat > "$shims/uname" <<EOF
#!/bin/sh
case \${1:-} in
  -s) echo "\$UNAME_S" ;;
  -m) echo "\$UNAME_M" ;;
  *) exec "$real_uname" "\$@" ;;
esac
EOF
  # The fake cargo logs every call, then leaves the artefact where the build script installs from,
  # which is <target-dir>/release/<name>: an ELF copied from the host, or a Mach-O's magic bytes.
  cat > "$shims/cargo" <<EOF
#!/bin/sh
echo "cargo \$*" >> "\$CALLS"
dir=
prev=
for a in "\$@"; do
  [ "\$prev" = "--target-dir" ] && dir="\$a"
  prev="\$a"
done
mkdir -p "\$dir/release"
case "\$FAKE_ARTEFACT" in
  elf) cp "$real_sh" "\$dir/release/\$FAKE_NAME" ;;
  *) printf '\317\372\355\376Mach-O padding' > "\$dir/release/\$FAKE_NAME" ;;
esac
EOF
  # The fake msb stands in for the rust:1-alpine microVM: it finds the host directory mounted on
  # /build-target and leaves an ELF where the build script installs from.
  cat > "$shims/msb" <<EOF
#!/bin/sh
echo "msb \$*" >> "\$CALLS"
dir=
prev=
for a in "\$@"; do
  case \$prev in -v) case \$a in *:/build-target) dir=\${a%:/build-target} ;; esac ;; esac
  prev="\$a"
done
mkdir -p "\$dir/release"
cp "$real_sh" "\$dir/release/rtk"
EOF
  chmod 755 "$shims/uname" "$shims/cargo" "$shims/msb"
}

# Runs one build script in the fake repo with the shims first on PATH; what it prints and what it
# complains about land in separate files, because the refusal has to be on stderr in particular.
run_build() { # script
  rc=0
  PATH="$shims:$PATH" MSB="$shims/msb" CALLS=$calls \
    COLONIZER_BUILD_HERE=${HERE:-} UNAME_S=${UNAME_S:-Linux} UNAME_M=${UNAME_M:-x86_64} \
    FAKE_ARTEFACT=${FAKE_ARTEFACT:-} FAKE_NAME=${FAKE_NAME:-} \
    sh "$fake_repo/scripts/$1" > "$log" 2> "$err" || rc=$?
  return "$rc"
}

must_build() { # script label
  run_build "$1" || bad "$2: exited non-zero: $(cat "$err")"
}

must_refuse() { # script label
  rc=0
  run_build "$1" || rc=$?
  [ "$rc" = 1 ] || bad "$2: exited $rc, wanted the scripts' own refusal exit 1"
}

# The refusals have to say what is wrong, name the binary they would have built, and point at the fix.
expect_refusal() { # label binary
  grep -q "$2" "$err" || bad "$1: the refusal did not name $2: $(cat "$err")"
  grep -q "has to run in a Linux colony" "$err" || bad "$1: the refusal did not say the binary runs in a Linux colony: $(cat "$err")"
  grep -q "unset COLONIZER_BUILD_HERE to build it in the rust:1-alpine microVM" "$err" || bad "$1: the refusal did not point at the fix: $(cat "$err")"
}

expect_elf() { # path label
  [ "$(head -c 4 "$1" | od -An -tx1 | tr -d ' \n')" = 7f454c46 ] || bad "$2: $1 is not an ELF binary"
}

stamps() {
  ls "$fake_repo/target/rtk-build" 2>/dev/null || true
}

# Variable assignments prefixed to a function call persist in some shells and not others, so every
# case clears the shims' inputs first and states the ones it wants; run_build defaults the rest to
# this host's platform.
reset_env() {
  unset HERE FAKE_ARTEFACT FAKE_NAME UNAME_S UNAME_M
  rm -f "$calls"
}

# 1. Here mode off Linux is refused before the scripts have done anything at all: no build tree, no
#    dist, no cargo, for either script.
note "== here mode refused off Linux"
make_shims
for script in build-agentd.sh build-rtk.sh; do
  fresh_repo
  reset_env
  case $script in
    build-agentd.sh) binary=colonizer-agentd ;;
    build-rtk.sh) binary=rtk ;;
  esac
  HERE=1 UNAME_S=Darwin UNAME_M=arm64 must_refuse "$script" "$script: a Darwin here build"
  expect_refusal "$script" "$binary"
  [ ! -e "$fake_repo/target" ] || bad "$script: the refusal left a build tree behind: $(ls -R "$fake_repo/target")"
  [ ! -e "$fake_repo/dist" ] || bad "$script: the refusal installed into dist: $(ls -R "$fake_repo/dist")"
  [ ! -e "$calls" ] || bad "$script: the refusal built something first: $(cat "$calls")"
  note "ok: $script refused a Darwin here build and touched nothing"
done

# 2. A cargo that produces a Mach-O must be caught by the ELF magic check before the install, so
#    dist/bin never sees it and rtk never writes its stamp.
note "== a non-ELF artefact never reaches dist/bin"
fresh_repo
reset_env
HERE=1 FAKE_ARTEFACT=macho FAKE_NAME=colonizer-agentd must_refuse build-agentd.sh "agentd with a Mach-O artefact"
grep -q "colonizer-agentd is not an ELF binary" "$err" || bad "agentd: the ELF check said nothing about colonizer-agentd: $(cat "$err")"
[ ! -e "$agentd_out" ] || bad "agentd: dist/bin/colonizer-agentd exists anyway"
note "ok: agentd refused to install a Mach-O artefact"

fresh_repo
reset_env
HERE=1 FAKE_ARTEFACT=macho FAKE_NAME=rtk must_refuse build-rtk.sh "rtk with a Mach-O artefact"
grep -q "rtk is not an ELF binary" "$err" || bad "rtk: the ELF check said nothing about rtk: $(cat "$err")"
[ ! -e "$rtk_out" ] || bad "rtk: dist/bin/rtk exists anyway"
[ -z "$(stamps)" ] || bad "rtk: a stamp was written for a Mach-O artefact: $(stamps)"
note "ok: rtk refused to install a Mach-O artefact and wrote no stamp"

# 3. The ELF happy paths copy a real ELF binary from the host, so they need Linux; on any other host
#    the checks above have already run, and these would only re-test the Mach-O refusal.
if [ "$(head -c 4 "$real_sh" | od -An -tx1 | tr -d ' \n')" != 7f454c46 ]; then
  note "skip: the happy paths copy a real ELF from the host, and this host has none; the refusal and Mach-O cases passed"
  note "all checks passed"
  exit 0
fi

# 4. agentd's happy path: an ELF artefact is installed and executable.
note "== agentd installs an ELF artefact"
fresh_repo
reset_env
HERE=1 FAKE_ARTEFACT=elf FAKE_NAME=colonizer-agentd must_build build-agentd.sh "agentd with an ELF artefact"
[ -x "$agentd_out" ] || bad "agentd: dist/bin/colonizer-agentd is missing or not executable"
expect_elf "$agentd_out" "agentd happy path"
note "ok: agentd installed an executable ELF artefact"

# 5. rtk's stamp has to name the build mode: the same mode is served from the stamp, but a run in the
#    other mode must rebuild rather than short-circuit on the first mode's stamp.
note "== rtk's stamp names the build mode"
fresh_repo
reset_env
HERE=1 FAKE_ARTEFACT=elf FAKE_NAME=rtk must_build build-rtk.sh "rtk here-mode build with an ELF artefact"
[ -x "$rtk_out" ] || bad "rtk: dist/bin/rtk is missing or not executable"
expect_elf "$rtk_out" "rtk happy path"
[ "$(stamps | wc -l)" = 1 ] || bad "rtk: expected one stamp after the here-mode build, got: $(stamps)"
if grep -q "already built" "$log"; then bad "rtk: the first build claimed to be already built"; fi
cargo_calls=$(grep -c '^cargo' "$calls" || true)

# The same mode again: served from the stamp, without another cargo run.
HERE=1 FAKE_ARTEFACT=elf FAKE_NAME=rtk must_build build-rtk.sh "rtk here-mode build again"
grep -q "already built" "$log" || bad "rtk: the second here-mode build did not short-circuit: $(cat "$log")"
[ "$(grep -c '^cargo' "$calls" || true)" = "$cargo_calls" ] || bad "rtk: the short-circuited build ran cargo anyway: $(cat "$calls")"
here_stamp=$(stamps)

# The other mode must not serve the here-built binary: msb runs, and the stamp that is left names it.
reset_env
FAKE_ARTEFACT=elf FAKE_NAME=rtk must_build build-rtk.sh "rtk microVM build after a here-mode build"
if grep -q "already built" "$log"; then bad "rtk: the microVM build short-circuited on the here-mode stamp"; fi
grep -q '^msb' "$calls" || bad "rtk: the microVM build never ran msb: $(cat "$calls")"
[ "$(stamps | wc -l)" = 1 ] || bad "rtk: expected the microVM build to leave exactly its own stamp, got: $(stamps)"
[ "$(stamps)" != "$here_stamp" ] || bad "rtk: the microVM build reused the here-mode stamp: $(stamps)"
note "ok: rtk rebuilt when the build mode changed and short-circuited when it did not"

# 6. The stamp binds the source and the mode, not the bytes in dist/bin/rtk: a Mach-O sitting there
#    under a valid stamp (a stale or hand-copied artefact) must fail the same ELF check, not be
#    served as already built.
note "== a stamp never serves a non-ELF dist/bin/rtk"
reset_env
printf '\317\372\355\376Mach-O padding' > "$rtk_out"
if run_build build-rtk.sh; then bad "rtk: served a Mach-O dist/bin/rtk as already built"; fi
if grep -q "already built" "$log"; then bad "rtk: claimed already built over a Mach-O dist/bin/rtk: $(cat "$log")"; fi
grep -q "rtk is not an ELF binary" "$err" || bad "rtk: the ELF check said nothing about rtk: $(cat "$err")"
[ ! -e "$calls" ] || bad "rtk: the stamped Mach-O run rebuilt or ran a toolchain: $(cat "$calls")"
note "ok: rtk refused to serve a Mach-O dist/bin/rtk on a stamp"

note "all checks passed"
