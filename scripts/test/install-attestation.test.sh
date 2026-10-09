#!/bin/sh
# Checks verify_provenance() in scripts/install-release.sh, the provenance step of every install: a gh
# that fails because it is not logged in must be a skip — a note, and fatal only under
# COLONIZER_REQUIRE_ATTESTATION=1, with a message naming the auth problem — while a logged-in gh that
# rejects the attestation is the "wrong" failure, and a transient failure (an HTTP 5xx or a rate limit
# from the attestation API, like the network errors) is retried, then refused as "service
# unavailable", never as "wrong" (issue #1235). The function is lifted out of the installer verbatim
# (the awk trick scripts/test/install-release.test.sh uses for slot_pids) and driven against a stub gh
# first on PATH, so this runs anywhere, offline, without the installer's Linux + /dev/kvm gate.
set -eu

repo_dir=$(cd "$(dirname "$0")/../.." && pwd)
installer=$repo_dir/scripts/install-release.sh

# The installer's own helpers, copied verbatim so the notes and the failure text are the real ones.
say() { printf '==> %s\n' "$1"; }
fail() { printf 'colonizer install: %s\n' "$1" >&2; exit 1; }

# One contiguous `name() {` ... `}` block, so awk can lift it out; the count keeps a refactor that
# breaks that shape from silently testing nothing.
verify_func=$(awk '/^verify_provenance\(\) \{/,/^\}/' "$installer")
[ "$(printf '%s\n' "$verify_func" | grep -c '() {')" = 1 ] ||
  { echo "FAIL: expected the verify_provenance helper in $installer, got: $(printf '%s\n' "$verify_func" | grep '() {' || true)"; exit 1; }
eval "$verify_func"

# The repository and workflow the function names in its messages.
repo=Colonizer-dev/harness
export release_workflow="$repo/.github/workflows/release.yml"

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
shims=$scratch/shims
gh_log=$scratch/gh.log
mkdir -p "$shims"

# A stub gh first on PATH. GH_STUB_TAG picks its answer, and every call is logged so a case can prove
# gh was reached. `--help` succeeds and offers --signer-workflow, so the function takes the
# pinned-workflow path it uses against a current gh.
cat > "$shims/gh" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >> "$GH_STUB_LOG"
if [ "$1" = attestation ] && [ "$2" = verify ] && [ "$3" = --help ]; then
  printf '  --signer-workflow string   Workflow to verify against\n'
  exit 0
fi
if [ "$1" = attestation ] && [ "$2" = verify ]; then
  case ${GH_STUB_TAG:-} in
    unauthed)
      printf 'To get started with GitHub CLI, please run:  gh auth login\n' >&2
      printf 'Alternatively, populate the GH_TOKEN environment variable with a GitHub API token.\n' >&2
      exit 4 ;;
    unauthed_quiet)
      printf 'error authenticating to api.github.com\n' >&2
      exit 1 ;;
    mismatch)
      printf 'Error: the artifact subject digest does not match the attestation\n' >&2
      exit 1 ;;
    tuf_timeout)
      printf 'Error: failed to refresh TUF metadata: Get "https://tuf-repo-cdn.sigstore.dev/timestamp.json": context deadline exceeded\n' >&2
      exit 1 ;;
    gh503)
      printf 'gh: failed to fetch attestations from Colonizer-dev/harness: HTTP 503: trust-metadata-api service unavailable\n' >&2
      exit 1 ;;
    ratelimit)
      printf 'gh: API rate limit exceeded for installation ID 1234567: HTTP 403: You have exceeded a secondary rate limit\n' >&2
      exit 1 ;;
    flaky_503)
      # The same 503 on the first two verify attempts, then the verified answer: the transient error
      # cleared. The count includes this call, so <= 2 means "this is one of the first two".
      if [ "$(grep -c '^attestation verify [^-]' "$GH_STUB_LOG")" -le 2 ]; then
        printf 'gh: failed to fetch attestations from Colonizer-dev/harness: HTTP 503: trust-metadata-api service unavailable\n' >&2
        exit 1
      fi
      printf 'The following attestation verified\n'
      exit 0 ;;
    noatt)
      printf 'Error: no attestations found for artifact\n' >&2
      exit 1 ;;
    verified)
      printf 'The following attestation verified\n'
      exit 0 ;;
    *)
      printf 'stub gh: unknown GH_STUB_TAG %s\n' "${GH_STUB_TAG:-}" >&2
      exit 2 ;;
  esac
fi
if [ "$1" = auth ] && [ "$2" = status ]; then
  case ${GH_STUB_TAG:-} in
    unauthed | unauthed_quiet) exit 1 ;;
    *) exit 0 ;;
  esac
fi
exit 0
EOF
chmod 755 "$shims/gh"
export PATH="$shims:$PATH"
export GH_STUB_LOG="$gh_log"
export GH_STUB_TAG=

note() { printf '%s\n' "$*"; }

bad() {
  printf 'FAIL: %s\n' "$1"
  echo "--- gh calls ($gh_log)"
  cat "$gh_log" 2>&1 || true
  exit 1
}

contains() { # label haystack needle
  case $2 in
    *"$3"*) return 0 ;;
  esac
  bad "$1: expected the output to contain '$3'; it said: $2"
}

lacks() { # label haystack needle
  case $2 in
    *"$3"*) bad "$1: the output must not contain '$3'; it said: $2" ;;
  esac
}

# Calls the real function with an isolated `why` — the installer calls it once and never resets it —
# and a released-source environment, returning its output and exit status together. $1 is the file
# under check; $2, when given, is COLONIZER_REQUIRE_ATTESTATION.
run_verify() {
  unset why
  ( COLONIZER_RELEASE_URL='' COLONIZER_REQUIRE_ATTESTATION=${2:-} verify_provenance "$1" ) 2>&1
}

file=$scratch/SHA256SUMS

# 1. A gh that is not logged in is a skip: a note and exit 0, not the "wrong" failure.
note "== a gh that is not logged in is a skip"
GH_STUB_TAG=unauthed
out=$(run_verify "$file") && rc=0 || rc=$?
[ "$rc" -eq 0 ] || bad "unauthed: expected exit 0 (a skip), got $rc: $out"
contains "unauthed" "$out" "provenance not checked"
contains "unauthed" "$out" "gh is not logged in"
contains "unauthed" "$out" "gh auth login"
contains "unauthed" "$out" "GH_TOKEN"
lacks "unauthed" "$out" "it is wrong"
note "ok: not-logged-in gh skipped the check with a note"

# 2. The same, under COLONIZER_REQUIRE_ATTESTATION=1: fatal, and the failure names the auth problem.
note "== COLONIZER_REQUIRE_ATTESTATION=1 makes the auth skip fatal, naming it"
out=$(run_verify "$file" 1) && rc=0 || rc=$?
[ "$rc" -ne 0 ] || bad "unauthed require: expected a non-zero exit, got 0: $out"
contains "unauthed require" "$out" "provenance could not be checked"
contains "unauthed require" "$out" "gh is not logged in"
contains "unauthed require" "$out" "GH_TOKEN"
contains "unauthed require" "$out" "COLONIZER_REQUIRE_ATTESTATION=1"
note "ok: the auth skip became a fatal error that names gh auth login / GH_TOKEN"

# 3. A logged-in gh that rejects the attestation is the "wrong" failure, whatever the variable says,
#    and must not be reported as an auth problem.
note "== a logged-in gh that rejects the attestation is the wrong failure"
GH_STUB_TAG=mismatch
: > "$gh_log"
out=$(run_verify "$file") && rc=0 || rc=$?
[ "$rc" -ne 0 ] || bad "mismatch: expected a non-zero exit, got 0: $out"
contains "mismatch" "$out" "does not verify"
contains "mismatch" "$out" "it is wrong"
lacks "mismatch" "$out" "not logged in"
grep -q 'auth status' "$gh_log" ||
  bad "mismatch: expected the failure branch to consult 'gh auth status' before deciding"
note "ok: a rejection by an authenticated gh is still the wrong-signature failure"

# 3b. The same rejection is fatal even without COLONIZER_REQUIRE_ATTESTATION (a check that ran and
#     found something wrong is not a skip), which case 3 already covers; here the quiet variant proves
#     the second probe, `gh auth status`, and not only the wording match, classifies a silent failure.
note "== an unauthenticated gh that does not print 'gh auth login' is still a skip"
GH_STUB_TAG=unauthed_quiet
out=$(run_verify "$file") && rc=0 || rc=$?
[ "$rc" -eq 0 ] || bad "unauthed_quiet: expected exit 0 (a skip), got $rc: $out"
contains "unauthed_quiet" "$out" "gh is not logged in"
lacks "unauthed_quiet" "$out" "it is wrong"
note "ok: 'gh auth status' alone recognised the unauthenticated gh"

# 4. The states that were already skips stay skips, and a verified attestation still says so.
note "== no attestations is still a skip"
GH_STUB_TAG=noatt
out=$(run_verify "$file") && rc=0 || rc=$?
[ "$rc" -eq 0 ] || bad "noatt: expected exit 0 (a skip), got $rc: $out"
contains "noatt" "$out" "no build provenance"
note "ok: 'no attestations found' skipped the check with a note"

note "== a verified attestation is still reported as verified"
GH_STUB_TAG=verified
out=$(run_verify "$file") && rc=0 || rc=$?
[ "$rc" -eq 0 ] || bad "verified: expected exit 0, got $rc: $out"
contains "verified" "$out" "provenance verified"
note "ok: a verified attestation says so"

# 5. A Sigstore/TUF network failure is retried, then reported as the service being unavailable,
#    never as "wrong".
note "== a TUF timeout is retried, then reported as a service-unavailable failure"
GH_STUB_TAG=tuf_timeout
: > "$gh_log"
out=$(COLONIZER_ATTESTATION_BACKOFF=0 run_verify "$file") && rc=0 || rc=$?
[ "$rc" -ne 0 ] || bad "tuf_timeout: expected a non-zero exit, got 0: $out"
contains "tuf_timeout" "$out" "unavailable"
contains "tuf_timeout" "$out" "retry later"
contains "tuf_timeout" "$out" "context deadline exceeded"
contains "tuf_timeout" "$out" "nothing was installed"
lacks "tuf_timeout" "$out" "it is wrong"
lacks "tuf_timeout" "$out" "COLONIZER_SKIP_ATTESTATION"
[ "$(grep -c '^attestation verify [^-]' "$gh_log")" -eq 3 ] ||
  bad "tuf_timeout: expected 3 verify attempts"
note "ok: retried 3 times, then the service-unavailable message"

note "== a real mismatch is not retried"
GH_STUB_TAG=mismatch
: > "$gh_log"
out=$(COLONIZER_ATTESTATION_BACKOFF=0 run_verify "$file") && rc=0 || rc=$?
[ "$rc" -ne 0 ] || bad "mismatch: expected a non-zero exit"
contains "mismatch" "$out" "it is wrong"
lacks "mismatch" "$out" "service is unavailable"
lacks "mismatch" "$out" "retry later"
[ "$(grep -c '^attestation verify [^-]' "$gh_log")" -eq 1 ] || bad "mismatch: expected a single verify attempt"
note "ok: no retry for a real mismatch"

# 6. An HTTP 5xx from the attestation API is verification that could not run, not a verification that
#    failed (issue #1235): retried with the same backoff, then refused as "service unavailable",
#    never with the wrong-checksum message.
note "== an HTTP 503 is retried, then reported as the service being unavailable"
GH_STUB_TAG=gh503
: > "$gh_log"
out=$(COLONIZER_ATTESTATION_BACKOFF=0 run_verify "$file") && rc=0 || rc=$?
[ "$rc" -ne 0 ] || bad "gh503: expected a non-zero exit, got 0: $out"
contains "gh503" "$out" "unavailable"
contains "gh503" "$out" "retry later"
contains "gh503" "$out" "nothing was installed"
contains "gh503" "$out" "HTTP 503" # gh's own output is echoed above the refusal, for diagnosis
lacks "gh503" "$out" "does not verify"
lacks "gh503" "$out" "it is wrong"
lacks "gh503" "$out" "checksums are not what"
[ "$(grep -c '^attestation verify [^-]' "$gh_log")" -eq 3 ] ||
  bad "gh503: expected 3 verify attempts"
# A fatal `fail` either way, but under COLONIZER_REQUIRE_ATTESTATION=1 too.
out=$(COLONIZER_ATTESTATION_BACKOFF=0 run_verify "$file" 1) && rc=0 || rc=$?
[ "$rc" -ne 0 ] || bad "gh503 require: expected a non-zero exit, got 0: $out"
contains "gh503 require" "$out" "unavailable"
note "ok: retried 3 times, then the service-unavailable message"

note "== a rate-limited attestation API is the same unavailable refusal"
GH_STUB_TAG=ratelimit
: > "$gh_log"
out=$(COLONIZER_ATTESTATION_BACKOFF=0 run_verify "$file") && rc=0 || rc=$?
[ "$rc" -ne 0 ] || bad "ratelimit: expected a non-zero exit, got 0: $out"
contains "ratelimit" "$out" "unavailable"
contains "ratelimit" "$out" "retry later"
contains "ratelimit" "$out" "nothing was installed"
lacks "ratelimit" "$out" "does not verify"
lacks "ratelimit" "$out" "it is wrong"
[ "$(grep -c '^attestation verify [^-]' "$gh_log")" -eq 3 ] ||
  bad "ratelimit: expected 3 verify attempts"
note "ok: a rate limit is unavailable, not wrong"

note "== a transient 503 that clears verifies on a retry"
GH_STUB_TAG=flaky_503
: > "$gh_log"
out=$(COLONIZER_ATTESTATION_BACKOFF=0 run_verify "$file") && rc=0 || rc=$?
[ "$rc" -eq 0 ] || bad "flaky_503: expected exit 0 once the retries got through, got $rc: $out"
contains "flaky_503" "$out" "provenance verified"
lacks "flaky_503" "$out" "unavailable"
lacks "flaky_503" "$out" "it is wrong"
[ "$(grep -c '^attestation verify [^-]' "$gh_log")" -eq 3 ] ||
  bad "flaky_503: expected 3 verify attempts (two 503s, then success)"
note "ok: two 503s, then verified on the third attempt"

note "all checks passed"
