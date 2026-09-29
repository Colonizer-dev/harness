# UHP conformance

The [Unified Harness Protocol](https://unifiedharnessprotocol.org/) (UHP) is an open wire contract
for agent harnesses, extending the OpenAI Responses API so a Responses SDK could drive a harness
unchanged. Colonizer's own wire names came first; [protocol.md](protocol.md) §7 maps them onto UHP
and is proposal-only. This page is the measured state: what the protocol's conformance suite says
about this repository as it is. The spec — `2026-09-12`, draft — and the suite live in
[HarnessRouter/harnessrouter](https://github.com/HarnessRouter/harnessrouter)'s `protocol/` dir;
the suite, `uhp-conformance` `2026.9.12.post2`, is not on PyPI and is installed pinned to commit
`27656efd` — moving that pin changes results, so it re-measures like a code change.

## The claim

> Colonizer is **not conformant** (no class) at spec 2026-09-12, measured 2026-09-28 with suite
> 2026.9.12.post2.

The target claim is the **core** class, then **extended**; **full is not targeted**. Classes are
cumulative — core 40 checks, extended 49, full 75 — and a skip is never a pass.

| Class | Checks | Pass | Fail | Skip | Error |
| :--- | ---: | ---: | ---: | ---: | ---: |
| core | 40 | 3 | 12 | 25 | 0 |
| extended (cumulative) | 49 | 6 | 12 | 31 | 0 |
| full (cumulative) | 75 | 6 | 13 | 56 | 0 |

Per-check outcomes are in [uhp-conformance.json](uhp-conformance.json) — machine-readable, written
by the script below and compared check by check in CI. Measured 2026-09-28, build commit `54264fa`,
Debian 12, Linux 6.12.99 x86_64, rustc 1.98.1, a debug `cargo build -p colonizer-harness --locked`.
The mothership was booted on loopback without KVM and without agent credentials, so no colony ever
started — none was needed, since every task check skips before it reaches a task. A rerun gives
identical counts in about 0.1 s. This run re-measures after #651 landed the first §7 surface: the
§7.2 session list and the §7.5 artifact reads answer under `/uhp/v1/…`, and every other protocol
path answers a JSON 404 instead of the cockpit's page.

## What fails

| Check | Spec chapter | What the suite saw |
| :--- | :--- | :--- |
| D-01 | lifecycle §2 | `GET /v1/uhp` answered 401, expected 200 — there is no discovery document |
| D-02 | lifecycle §2 | discovery requires authentication; a client must be able to probe first |
| D-03–D-05 | lifecycle §2, schema.md | no discovery document; nothing to validate or advertise |
| V-01 | lifecycle §1 | the responses the suite saw (all refusals) carry no `UHP-Version` header |
| V-03 | lifecycle §1 | an unsupported version hits the 401 wall, expected a JSON 400 |
| A-02 | architecture §5 | the 401 body is Colonizer's error JSON, not the UHP envelope (no `error.type`) |
| E-02 | errors §3.1 | an unknown harness reports `not_found`, expected `harness_not_found` |
| E-03 | errors §3.1 | an unknown response reports `not_found`, expected `response_not_found` |
| H-01 | harnesses §1 | `GET /v1/harnesses` answers 404 — the route does not exist |
| H-03 | harnesses §3 | `GET /v1/models` answers 404 — no model catalogue |
| F-02 | harnesses §4.1 | harness creation 404s, so an unsupported base cannot be refused with 400 (full class, not targeted) |

The remaining failures share one cause: the capability does not exist at the protocol path. There
is a `/uhp` surface now — sessions and artifacts answer there, and any other `/uhp` path answers a
JSON 404 (never the SPA page, which is why E-01 and X-08 pass) — but discovery (D), version
negotiation (V), the token wall's envelope shape (A-02), harnesses and models (H), and
harness/response-specific error codes (E-02, E-03) have no route behind them yet, and a refusal
with the wrong code is still a failure. The passes are A-01 (missing credential refused with 401),
E-01 (errors use the structured envelope), E-04 (no stack traces in error bodies), X-01 (the
session listing is served and well formed), X-02 (the listing reports its end with `next_cursor`)
and X-08 (artifact ids do not traverse outside their container).

The skips, by family: V-02, because discovery advertised no version to negotiate. H-02 and H-04,
X-05, X-06 and X-09, F-01 and F-03–F-08, and R-01–R-08 skip because the harness listing they build
on does not exist. X-07 skips because no task ever ran, so its session produced no artifacts to
download. X-03 and X-04 have no session id from an earlier task, for the same reason. T-01–T-10,
S-01–S-09 and C-01–C-03 all need a harness to run a task against. P-01–P-10 skip on their face:
the plugins capability is absent, and the Plugins chapter is optional at every class.

## The gaps, in Colonizer terms

**Core** — discovery/lifecycle, versioning, auth/errors, harnesses, tasks, streaming,
continuation/cancellation. The `/uhp` surface exists as a routing family with the version header
and the §7.7 envelope, but only sessions and artifacts live in it: there is no discovery document,
no harness or model listing, no responses/tasks endpoint, no streaming. Follow-up:
*UHP core class: implement the /uhp/v1 surface proposed in docs/protocol.md §7 (discovery,
harnesses, responses, streaming, errors)*.

The SPA-fallback half of this gap closed alongside #651: unmatched `/uhp` paths now answer the
same JSON 404 unmatched `/api/*` paths have answered since #641, so a protocol miss no longer
reads as the cockpit's page. What remains is the surface itself.

**Extended** — sessions, files, artifacts. Landed with #651: `GET /api/sessions` takes `limit`
and `cursor` (and still answers the bare array without them), and the artifact reads — list,
single download capped at 16 MiB, plain-tar archive — answer at both their `/api/sessions/{id}/files…`
names and their `/uhp/v1` aliases with traversal refused. Still unbuilt: input-file uploads
(§7.5's `POST /api/files`, probed by X-05 and X-09), and X-06/X-07 need a task-bearing run — the
manual gate below — to exercise artifacts of a session that actually produced some.

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
all skip until the core surface exists, so there is nothing to cross-check yet.

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
/tmp/uhp-venv/bin/uhp-conformance --base-url http://127.0.0.1:7878/uhp \
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
