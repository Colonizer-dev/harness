#!/bin/sh
# Hermetic Unified Harness Protocol conformance run (issue #297): boots the mothership this checkout
# builds on loopback with throwaway config/data dirs, points the commit-pinned UHP conformance suite
# at it, and fails when any check's outcome moved from docs/uhp-conformance.json — a moved outcome is
# a protocol-behaviour change, re-measured deliberately with --update, never absorbed. Also validates
# the claude-code runner's event fixture against docs/agent-events.schema.json. Moving the suite pin
# (suite_commit) changes results and goes through the same re-measure flow. Usage:
#   sh scripts/ci/uhp-conformance.sh [--update] [<path-to-colonizer>]
# Binary defaults to $COLONIZER_BIN, else target/debug/colonizer. Env: UHP_VENV (venv dir, default
# under $RUNNER_TEMP or the run's temp dir), UHP_JSON_OUT (keep the report), UHP_SUITE_REF (pip ref).
set -eu

suite_commit=27656efd34629bd1fba4d00f66924d48a2c820bc
suite_version=2026.9.12.post2
suite_ref=${UHP_SUITE_REF:-"git+https://github.com/HarnessRouter/harnessrouter.git@${suite_commit}#subdirectory=protocol/conformance"}
expectation=docs/uhp-conformance.json
fixture=modules/agents/claude-code/test/fixtures/events.jsonl
schema=docs/agent-events.schema.json

update=0
[ "${1:-}" = "--update" ] && { update=1; shift; }
bin=${1:-${COLONIZER_BIN:-target/debug/colonizer}}
cd "$(git rev-parse --show-toplevel)"
[ -x "$bin" ] || {
  echo "colonizer binary not executable: $bin (cargo build -p colonizer-harness --locked, or pass a path)" >&2
  exit 1
}

work=$(mktemp -d "${TMPDIR:-/tmp}/uhp-conformance.XXXXXX")
server_pid=
trap 'if [ -n "$server_pid" ]; then kill "$server_pid" 2>/dev/null || true; wait "$server_pid" 2>/dev/null || true; fi; rm -rf "$work"' EXIT INT TERM
die() { echo "$1 — last lines of $work/serve.log (dir kept):" >&2; tail -20 "$work/serve.log" >&2; trap - EXIT INT TERM; exit 1; }

# A free port on loopback, so a locally running mothership cannot collide with the run. The suite
# builds its URLs as base_url + "/v1/...", and the UHP surface lives under /uhp (issue #650), so
# the base URL carries the prefix: an SDK's `<mothership>/uhp/v1` lands on the same routes.
port=$(python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')
base_url=http://127.0.0.1:$port/uhp
mkdir -p "$work/cfg" "$work/data"
COLONIZER_BIND=127.0.0.1:$port COLONIZER_CONFIG_DIR="$work/cfg" COLONIZER_DATA_DIR="$work/data" \
  COLONIZER_NO_BROWSER=1 COLONIZER_UPDATE_CHECK=0 COLONIZER_TELEMETRY=off DO_NOT_TRACK=1 \
  "$bin" >"$work/serve.log" 2>&1 &
server_pid=$!

# Ready when the owner token exists and the port answers; bail early if the server died.
i=0
until [ -s "$work/cfg/api-token" ] && python3 -c "import socket; s = socket.socket(); s.settimeout(0.5); s.connect(('127.0.0.1', $port)); s.close()" 2>/dev/null; do
  kill -0 "$server_pid" 2>/dev/null || die "mothership exited before serving $base_url"
  [ "$i" -le 200 ] || die "mothership not ready after 60s"
  i=$((i + 1))
  sleep 0.3
done

# Reuse the venv when it already holds the pinned suite version; recreate it otherwise (a moved pin,
# a half-built venv). Creation needs network once; reuse needs none.
venv=${UHP_VENV:-${RUNNER_TEMP:-$work}/uhp-conformance-venv}
have=$("$venv/bin/python" -c "import importlib.metadata as m; print(m.version('uhp-conformance'))" 2>/dev/null || true)
if [ "$have" != "$suite_version" ]; then
  echo "installing uhp-conformance $suite_version into $venv ..."
  rm -rf "$venv" && python3 -m venv "$venv" && "$venv/bin/pip" --quiet install "$suite_ref"
fi

report=${UHP_JSON_OUT:-$work/uhp-report.json}
echo "running the UHP conformance suite (--class full) against $base_url ..."
rc=0
"$venv/bin/uhp-conformance" --base-url "$base_url" --api-key "$(cat "$work/cfg/api-token")" --class full \
  --json "$report" --plain --label "Colonizer $(git rev-parse --short HEAD)" || rc=$?
# A non-zero suite exit is the honest "not conformant" the expectation file records; only a missing report is a harness bug.
[ -s "$report" ] || die "the suite produced no report (exit $rc)"
[ -z "${UHP_JSON_OUT:-}" ] || echo "JSON report kept at $report"
kill "$server_pid" 2>/dev/null || true; wait "$server_pid" 2>/dev/null || true; server_pid=

# Fixture validation, then compare against (or, with --update, rewrite) the expectation file: one
# python block, so the expectation format and its enforcement live in one place. PYTHONUTF8 anchors
# the open() calls, which would otherwise follow the locale.
PYTHONUTF8=1 "$venv/bin/python" - "$fixture" "$schema" "$expectation" "$report" "$suite_commit" \
  "$(date -u +%Y-%m-%d)" "$(git rev-parse HEAD)" "$update" <<'PY'
import json, sys
from jsonschema.validators import validator_for

fixture, schema, expectation, report, suite_commit, measured, commit, update = sys.argv[1:9]
update = update == "1"

s = json.load(open(schema)); v = validator_for(s)(s); v.check_schema(s)
bad = 0
for n, line in enumerate(open(fixture), 1):
    if not line.strip():
        continue
    try:
        event = json.loads(line)
    except ValueError as e:
        print(f"  {fixture}:{n}: not JSON: {e}"); bad += 1; continue
    for e in v.iter_errors(event):
        print(f"  {fixture}:{n}: invalid at {'/'.join(map(str, e.absolute_path)) or 'root'}: {e.message}"); bad += 1
if bad:
    sys.exit(f"FAIL: {fixture} does not validate against {schema} ({bad} error(s))")
print(f"events fixture OK: every line of {fixture} validates against {schema}")

rep = json.load(open(report))
if update:
    exp = {"spec": rep["suite_protocol_version"], "suite": rep["suite_version"], "suite_commit": suite_commit,
           "class": rep["requested_class"], "measured": measured, "commit": commit, "summary": rep["summary"],
           "outcomes": {c["id"]: c["outcome"] for c in rep["checks"]}}
    with open(expectation, "w") as f:
        json.dump(exp, f, indent=2); f.write("\n")
    print(f"wrote {expectation}: {len(exp['outcomes'])} checks, summary {exp['summary']} — review the diff")
    sys.exit(0)

try:
    exp = json.load(open(expectation))
except FileNotFoundError:
    sys.exit(f"no expectation file at {expectation} — create it from a fresh run: sh scripts/ci/uhp-conformance.sh --update")

problems = [f"{label}: measured {now!r}, expected {exp.get(key)!r}" + (" — the suite pin moved" if key == "suite" else "")
            for key, label, now in [("spec", "protocol version", rep.get("suite_protocol_version")),
                                    ("suite", "suite version", rep.get("suite_version")),
                                    ("class", "class", rep.get("requested_class")),
                                    ("summary", "summary", rep.get("summary"))] if now != exp.get(key)]
got = {c["id"]: c["outcome"] for c in rep.get("checks", [])}
want = exp.get("outcomes", {})
problems += [f"{i}: check gone from the suite" if i not in got else f"{i}: {was} -> {got[i]}"
             for i, was in want.items() if i not in got or got[i] != was]
problems += [f"{i}: new check, outcome {o}" for i, o in got.items() if i not in want]

if problems:
    print(f"UHP conformance drifted from {expectation}:")
    print("\n".join(f"  {p}" for p in problems))
    print("hint: re-measure with sh scripts/ci/uhp-conformance.sh --update and update docs/conformance.md")
    sys.exit(1)
print(f"UHP conformance matches {expectation}: {len(got)} checks, summary {rep['summary']}")
PY
