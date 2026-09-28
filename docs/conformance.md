# UHP conformance

The [Unified Harness Protocol](https://unifiedharnessprotocol.org/) (UHP) is an open wire contract
for agent harnesses, extending the OpenAI Responses API so a Responses SDK could drive a harness
unchanged. Colonizer's own wire names came first; [protocol.md](protocol.md) §7 maps them onto UHP
and is partly served. This page is the measured state: what the protocol's conformance suite says
about this repository as it is. The spec — `2026-09-12`, draft — and the suite live in
[HarnessRouter/harnessrouter](https://github.com/HarnessRouter/harnessrouter)'s `protocol/` dir;
the suite, `uhp-conformance` `2026.9.12.post2`, is not on PyPI and is installed pinned to commit
`27656efd` — moving that pin changes results, so it re-measures like a code change.

## The claim

> Colonizer is **not conformant** (no class) at spec 2026-09-12, measured 2026-09-28 with suite
> 2026.9.12.post2. Every hermetic core check passes except D-05 — by choice: streaming and
> cancellation are the follow-up, and the discovery document reports them false rather than
> claiming them. The core's other skips are the task-bearing checks, which need a harness to run
> against and stay the manual gate; a class claim needs those too.

The target claim is the **core** class, then **extended**; **full is not targeted**. Classes are
cumulative — core 40 checks, extended 49, full 75 — and a skip is never a pass.

| Class | Checks | Pass | Fail | Skip | Error |
| :--- | ---: | ---: | ---: | ---: | ---: |
| core | 40 | 15 | 1 | 24 | 0 |
| extended (cumulative) | 49 | 18 | 1 | 30 | 0 |
| full (cumulative) | 75 | 18 | 2 | 55 | 0 |

Per-check outcomes are in [uhp-conformance.json](uhp-conformance.json) — machine-readable, written
by the script below and compared check by check in CI. Measured 2026-09-28, build commit `54264fa`,
Debian 12, Linux 6.12.99 x86_64, rustc 1.98.1, a debug `cargo build -p colonizer-harness --locked`.
The mothership was booted on loopback without KVM and without agent credentials, so no colony ever
started — none was needed, since every task check skips before it reaches a task. The read-side
UHP surface of issue #650 is what the core checks now measure: discovery, versioning, the error
envelope, harnesses and session listing (`crates/colonizer/src/uhp.rs`), probed under `/uhp`.

## What fails

| Check | Spec chapter | What the suite saw |
| :--- | :--- | :--- |
| D-05 | lifecycle §2 | the discovery document claims class core but reports `streaming` and `cancellation` false — honest, and the follow-up flips it |
| F-02 | harnesses §4.1 | `POST /v1/harnesses` is not served (405), so creating a harness with an unsupported base is not refused with 400/422 (full class, not targeted) |

Everything else that fails is fixed: D-01–D-04, V-01–V-03 and A-02 pass since the discovery
document, the `UHP-Version` negotiation and the 401 envelope exist (`crates/colonizer/src/uhp.rs`);
E-01–E-03 pass because an unknown harness or response is a 404 envelope with its resource-specific
code, no longer the SPA fallback's 200 `text/html`; H-01 and H-03 pass on the harness list and the
(empty) model catalogue; X-01, X-02 and X-08 pass on the session listing and the refused traversal
probe. F-02 used to fail the other way — the create probe fell through to the SPA and was accepted
with 200; the surface now answers it 405, which is the honest "not implemented".

The skips, by family: T-01–T-10, S-01–S-09 and C-01–C-03 all need a harness to run a task against,
and the hermetic boot has none (no bundled assets, so the harness list is empty). H-02 and H-04
skip the same way — they fetch a harness the listing must name. X-03–X-07 and X-09 have no session
id from an earlier task, and X-09 skips on its face because discovery reports `files_input` false.
F-01 and F-03–F-08 build on harness CRUD; R-01–R-08 on session sharing, reported false. P-01–P-10
skip on their face: the plugins capability is absent, and the Plugins chapter is optional at every
class.

## The gaps, in Colonizer terms

**Core** — discovery/lifecycle, versioning, auth/errors, harnesses and session listing are served
under `/uhp/v1` since #650 (`crates/colonizer/src/uhp.rs`, `crates/colonizer/routes.snap`). What
keeps the core class is D-05, and what keeps D-05 is the missing half of the surface: creating and
continuing responses (`POST /uhp/v1/responses`), SSE streaming (§7.4) and cancellation (§7.6) are
still the follow-up, and the discovery document says so in its capabilities. Follow-up:
*UHP core class: responses, streaming and cancellation on /uhp/v1 (docs/protocol.md §7.3, §7.4,
§7.6)*.

Unknown paths inside `/uhp` no longer fall through to the SPA fallback — `api_not_found` in
`crates/colonizer/src/server.rs` answers them as 404 envelopes since #650, the way it has answered
unmatched `/api/*` since #641. Paths outside both prefixes still serve the cockpit's page, which
is the SPA's job and stays.

**Extended** — sessions, files, artifacts. The session listing is served and paginated (X-01,
X-02, X-08 pass); what remains is the artifact API: there is no `/v1/files` and no container read —
`publish.rs` speaks worktree/git/PR, not files; §7.5 proposes the routes. Follow-up: *UHP extended
class: a colony file/artifact API (docs/protocol.md §7.5)*.

**Full**, accepted as not targeted:

- **Harness CRUD (F).** Harnesses are agent modules discovered from the shipped assets at startup
  (`crates/colonizer/src/modules.rs`); creating them over HTTP is not a Colonizer goal.
- **Session sharing (R).** A single-operator local install has no multi-user sharing model.
- **Plugins (P).** Optional at every class, and skipped on its face: Colonizer's plugins are
  vendored agent plugins mounted into colonies, not UHP plugin packages.

## The CI gate

`.github/workflows/ci.yml` runs a `conformance` job: it builds the mothership, boots it on loopback
with throwaway config and data dirs (the script sets `COLONIZER_CONFIG_DIR`, `COLONIZER_DATA_DIR`,
`COLONIZER_BIND`, `COLONIZER_NO_BROWSER`, `COLONIZER_UPDATE_CHECK`, `COLONIZER_TELEMETRY`), runs
the pinned suite at `--class full`, and fails if any check's outcome differs from
[uhp-conformance.json](uhp-conformance.json). A protocol-affecting change cannot land silently:
`sh scripts/ci/uhp-conformance.sh --update`, review the diff, and update this page's date and
counts in the same change. The date on this page is never newer than the last run — the
expectation file's `measured` and the date under The claim must match.

The script takes the binary from `$COLONIZER_BIN` or its first argument (default
`target/debug/colonizer`); `UHP_VENV` relocates the suite's virtualenv, `UHP_JSON_OUT` keeps the
full JSON report, `UHP_SUITE_REF` overrides the pinned install ref. It needs `python3` with
`venv` — network once, to build it; reuse needs none.

Second hermetic gate: the same script validates every line of the Claude Code runner's event
fixture (`modules/agents/claude-code/test/fixtures/events.jsonl`) against
[agent-events.schema.json](agent-events.schema.json) (draft 2020-12). That schema is the internal
runner→agentd contract; UHP's bundled one describes the client-facing Responses-style stream,
overlapping only semantically (text deltas, reasoning, tool calls). The UHP stream checks (S-*)
still all skip — streaming is the part of the surface not served yet (see D-05 above), so there is
nothing to cross-check yet.

## The manual gate

Once the core surface lists harnesses, the task-bearing checks (T, S and C; later X and R) execute
real agent tasks in colonies. They need `/dev/kvm` and agent credentials, and cost tokens and
minutes, so the hermetic CI job must not run them; they are a manual gate:

- **Who:** whoever cuts the release — the maintainer who assembles the changelog fragments and
  pushes the version tag ([changelog.d/README.md](../changelog.d/README.md), "Cutting a release").
- **Where:** a Linux host with `/dev/kvm`, the kind `scripts/build-agentd.sh --smoke` needs; hosted
  runners have it too, but this gate runs against a mothership with signed-in agent credentials.
- **When:** before every release tag, on every UHP spec-version bump, on any protocol-affecting
  change.
- **How:** the same suite, against a mothership that has a signed-in agent:

```sh
cargo build -p colonizer-harness --locked
python3 -m venv /tmp/uhp-venv
/tmp/uhp-venv/bin/pip install "git+https://github.com/HarnessRouter/harnessrouter.git@27656efd34629bd1fba4d00f66924d48a2c820bc#subdirectory=protocol/conformance"
/tmp/uhp-venv/bin/uhp-conformance --base-url http://127.0.0.1:7878 \
  --api-key "$(cat ~/.config/colonizer/api-token)" --class full --plain
```

Today the manual gate adds nothing — every task check skips either way — so the hermetic run is the
complete measurement, and the report above is not provisional.

## Reproducing

```sh
cargo build -p colonizer-harness --locked
sh scripts/ci/uhp-conformance.sh           # boots a throwaway mothership, compares against the expectation file
UHP_JSON_OUT=/tmp/uhp-full.json sh scripts/ci/uhp-conformance.sh   # keep the suite's per-check JSON report
```

The script creates its config and data dirs under `mktemp -d` and removes them on the way out; the
mothership never sees real config. A second person on the same commit gets the same counts.

## What this is not

Conformance is interoperability, not a security verdict. Passing UHP checks says nothing about
isolation, credentials or publish safety — that assessment is [audit.md](audit.md), the one that
gates unattended work.
