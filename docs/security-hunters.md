# Security-hunter modules

What ships today: one `Manifest` per hunter, a `hunters.lock` pin per platform, an operator
opt-in (`COLONIZER_HUNTER_INSTALL=1`) guarding a Linux-only, race-free, checksum-verified
on-demand install, a capability probe, and a manifest-driven scan runner (`hunters::scan`) that
renders the `scan` template, runs the installed binary, and parses the artifacts back into
`Finding`s. A red-team run now drives Shannon, one colony per repository (see below); Strix is
installed and probed but no run drives its scans yet, and colonies have no Docker daemon.
In the cockpit's red-team wizard the swarm and Shannon are selectable cards — Shannon reads
"Ready · runs in a colony" — and Strix stays a disabled card marked "Coming soon"; the red-team API
accepts `"hunter": "shannon"` and refuses Strix with a 400 (see
[red-team.md](red-team.md#operating-it)). The colony swarm with the Security preset is still the
way to a security-focused raid that needs no external hunter: security focus areas, a deterministic
pre-scan and an operator checklist ([red-team.md](red-team.md#the-security-preset)).

## On demand, verified, never vendored

Hunter binaries download on demand at a pinned version and are checksum-verified before they run.
They are never vendored into the binary or the repo, which keeps licences clear: Strix is
Apache-2.0 and Shannon is AGPL-3.0, and the mothership shells out to both rather than linking
anything. Shannon is not even a download — it installs via `npx` as a Node package.

## The manifest

Each hunter is a `Manifest` in `crates/colonizer/src/hunters.rs` (`builtin()`), with one row per field:

| Field | What it is |
|---|---|
| `id` | Short stable id, used in routes, lock lookups and the on-disk cache |
| `name` | Display name |
| `description` | One line on what the hunter does |
| `homepage` | Project homepage |
| `logo` | Name of the intended inline-SVG icon, or the short name as a text fallback |
| `licence` | SPDX licence id |
| `pinned_version` | Hunter version this build drives |
| `runtime` | `binary` (downloaded) or `node` (via npx) |
| `needs_docker` | Whether a scan needs a Docker daemon |
| `install` | The only supported install path: the `POST /api/hunters/{id}/install` route, which installs the pinned, checksum-verified artifact from `hunters.lock` behind the `COLONIZER_HUNTER_INSTALL` opt-in |
| `scan` | Headless scan invocation template with `{target}`/`{mode}`/`{out}` placeholders; `scan` substitutes them as whole argv entries and runs the installed binary |
| `output` | Run-relative filenames the hunter writes; the first entry is the findings artifact `scan` parses |
| `findings_format` | Which parser reads the output: `strix_json` or `sarif` |
| `gateway_env` | Env var `scan` sets to the gateway's base URL for a run (the token goes to `LLM_API_KEY`); the gateway is required for scanning |
| `available` | True once a parser plus a checksum pin ship; false is a manifest-only stub |

## The two runtimes

`binary` hunters (Strix) download from `hunters.lock` into `<data_dir>/hunters/<id>/<version>/`
and run from there. `node` hunters (Shannon) run through `npx` with the pinned package version,
so there is nothing to download or verify — the install command is the whole story.

## Docker stays out of reach

Colonies are KVM microVMs without a Docker daemon, so a hunter with `needs_docker` (Strix) is
reported not ready and stays not runnable until Docker runs inside the colony microVM. The host's
daemon is never shared with a colony and never suggested: Docker socket access is host root, and
a hunter like Strix runs arbitrary proof-of-concept code, so pointing a colony — or anything a
hunter runs — at the host daemon would hand it the host. The capability probe
(`GET /api/hunters/{id}/probe`) reports the runtime plus Docker separately; a missing daemon says
plainly that the colony has none yet and the host's is never shared. A future run stage that
spawns Strix must target a Docker daemon inside the colony microVM.

## On-demand install

Binary artifacts pin in `crates/colonizer/hunters.lock` (id, version, platform, kind, sha256,
url — the same columns as `headroom.lock`), compiled into the binary and cached under
`<data_dir>/hunters/<id>/<version>/`. Installing is opt-in: `POST /api/hunters/{id}/install`
returns 403 unless `COLONIZER_HUNTER_INSTALL` holds a truthy value (`1`/`true`/`on`/`yes`,
case-insensitive). Pins are Linux-only, so macOS and Windows report the hunter as not
installable and not installed. Each hunter has its own async mutex held for the whole install,
so concurrent installs of the same hunter serialise and the second returns after re-checking
instead of downloading again. The download is bounded (256 MiB, roughly 3x the ~89 MB Strix 1.6.2
tarballs), refuses an oversized `Content-Length` before reading the body, follows redirects only
over https (at most ten hops), and times out after ten minutes. The sha256 is verified over the
downloaded bytes, which are piped straight into `tar` over stdin — no tarball file ever lands on
disk — and the binary lands atomically (a uniquely-named temp file in the version dir, chmodded
to `0o555`, then renamed over the final path), so a failed download never leaves a half-written
binary behind and readers never see one mid-write.

## Running a scan

`hunters::scan(manifest, request)` is the manifest-driven scan runner. It refuses a hunter that is
a manifest-only stub, checks the binary is installed and the target is a directory, renders the
`scan` template into argv (placeholders become whole argv entries — no shell), and runs the
installed binary with the working directory set to the request's `work_dir`, where it then looks
for the run's artifacts — at most three levels deep, never following symlinks. Strix writes
`strix_runs/<run-name>/vulnerabilities.json`, which that finds without knowing either name.

- **Gateway required.** A scan always routes the hunter's LLM traffic through the Colonizer
  gateway: `ScanRequest.gateway` (base URL plus token) is a required field, `gateway_env` carries
  the base URL, and the token goes to `LLM_API_KEY`. There is no "scan without the gateway" choice
  to make accidentally — a caller with no gateway has no scan — because hunter LLM spend that
  bypasses the gateway is spend nobody can see. Spend stays visible twice: the gateway meters
  every call, and `scan` also copies `llm_usage.cost` out of the hunter's own `run.json` into the
  `ScanOutcome` it returns.
- **Model and telemetry.** The request's model string lands in `STRIX_LLM` (Strix's LiteLLM model
  var) and `STRIX_TELEMETRY=0` is set so Strix does not phone home; those env names live as
  constants until a second hunter needs different ones.
- **Exit codes.** The runner takes Strix's headless convention as the rule: 0 clean, 2
  vulnerabilities found, anything else (or death by signal) a fatal `Err` carrying the stderr.
  Exit 2 with no readable primary artifact is also an error: a claimed vulnerability must have
  artifacts behind it.
- **Outcome.** `ScanOutcome` carries the status (`clean` or `findings`), the exit code, the parsed
  `Finding`s (first `output` entry through the manifest's parser), the run directory, and
  `cost_usd` when the hunter reported one.
- **Timeout.** A scan still running after four hours is killed and reported as an `Err` — long
  enough for a deep run, short enough that a wedged agent cannot hold the run stage forever.
- **Docker first.** `scan` does not itself check readiness: the run stage that drives it must
  refuse to start a hunter whose probe is not ready, and a `needs_docker` hunter (Strix) still
  needs a Docker daemon inside the colony microVM (see above).

## Planned, not implemented (tracked in [#933](https://github.com/Colonizer-dev/harness/issues/933))

[#216](https://github.com/Colonizer-dev/harness/issues/216) closed with these left, and so did its
breakdown [#336](https://github.com/Colonizer-dev/harness/issues/336) and its follow-ups
[#442](https://github.com/Colonizer-dev/harness/issues/442); none of them built the work, so it is
still open under #933.

- A red-team run driving Strix. `check_hunter` (`crates/colonizer/src/redteam.rs:1637`) answers any
  hunter other than the swarm or Shannon with a 400 — "cannot run as a red-team hunter in this build
  yet" — and the wizard's Strix card is disabled and reads "Coming soon"
  (`web/src/cockpit/RedTeamWizard.tsx:239`).
- The Docker daemon that Strix needs. Its scan is arbitrary proof-of-concept code that wants a
  daemon inside the colony microVM, which colonies do not have, so no run starts one. The host's
  daemon is never shared; see "Docker stays out of reach" above.
- A run stage that drives the host-side `hunters::scan` (`crates/colonizer/src/hunters.rs:941`). The
  runner is built and tested but nothing calls it outside the tests, so a host-side hunter's
  findings never reach `events::file_finding` — orchestrator validation, the
  `findings::MAX_PER_COLONY` cap (`crates/colonizer/src/findings.rs:27`) and issue filing. Shannon is
  not this gap: its in-colony SARIF is read back and filed through that path
  (see [red-team.md](red-team.md#operating-it)).

## How to add a hunter

1. Write a `Manifest` and add it to `builtin()`.
2. A SARIF-emitting hunter needs no new parser — reuse `parse_sarif`.
3. A non-SARIF hunter adds one parser function and a `FindingsFormat` variant, then dispatches
   it from `normalize`.
4. Pin binary artifacts in `hunters.lock` for both architectures.
5. Flip `available` once the parser and the pin are in.

## Current state

Strix is phase one: manifest, pinned lock, opt-in install, probe, two parsers (`parse_strix`,
`parse_sarif`), and a scan runner that wires them end to end — template to argv, artifacts to
`Finding`s, cost into the outcome. No run drives a host-side scan yet, and colonies have no Docker
daemon, so `hunters::scan` runs nothing today. Shannon is phase two: its manifest stays
`available: false`, so host-side install and `scan` still refuse it as a manifest-only stub, but a
`hunter: "shannon"` red-team run drives it inside a colony — the host writes the pinned, `npx`-based
command into the colony's brief, the colony starts the app and runs Shannon against it in its
microVM, and the host reads the SARIF it leaves behind back through orchestrator validation (see
[red-team.md](red-team.md#operating-it)). Because Shannon is AGPL, the mothership never fetches or
links it: only the in-colony command line is written. The cockpit module gallery will extend the
existing `/api/plugins` surface rather than adding a parallel one, and the `logo` field is a name
only until that gallery renders it.

In the cockpit, the red-team wizard calls the probe for both hunters and shows the swarm and
Shannon as selectable cards with bundled logo images, Strix disabled; the manifest's `logo` field
is not used there. There is no hunter gallery and no install button in the cockpit: installing
Strix is the `POST /api/hunters/strix/install` route above, with the opt-in set.
