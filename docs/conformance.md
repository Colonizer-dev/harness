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

> Colonizer is **not conformant** (no class) at spec 2026-09-12, measured 2026-09-27 with suite
> 2026.9.12.post2.

The target claim is the **core** class, then **extended**; **full is not targeted**. Classes are
cumulative — core 40 checks, extended 49, full 75 — and a skip is never a pass.

| Class | Checks | Pass | Fail | Skip | Error |
| :--- | ---: | ---: | ---: | ---: | ---: |
| core | 40 | 2 | 13 | 25 | 0 |
| extended (cumulative) | 49 | 2 | 16 | 31 | 0 |
| full (cumulative) | 75 | 2 | 17 | 56 | 0 |

Per-check outcomes are in [uhp-conformance.json](uhp-conformance.json) — machine-readable, written
by the script below and compared check by check in CI. Measured 2026-09-27, build commit `4147180`,
Debian 12, Linux 6.12.99 x86_64, rustc 1.98.1, a debug `cargo build -p colonizer-harness --locked`.
The mothership was booted on loopback without KVM and without agent credentials, so no colony ever
started — none was needed, since every task check skips before it reaches a task. A rerun gives
identical counts in about 0.1 s. This is the baseline before the §7 aliases (#293 landed as docs
only): no `/uhp` route exists in the code yet.

## What fails

| Check | Spec chapter | What the suite saw |
| :--- | :--- | :--- |
| D-01 | lifecycle §2 | `GET /v1/uhp` answered 401 HTML, expected 200 |
| D-02 | lifecycle §2 | discovery requires authentication; a client must be able to probe first |
| D-03–D-05 | lifecycle §2, schema.md | no discovery document; nothing to validate or advertise |
| V-01 | lifecycle §1 | responses carry no `UHP-Version` header |
| V-03 | lifecycle §1 | an unsupported version hits the 401 wall, expected a JSON 400 |
| A-02 | architecture §5 | the 401 body is Colonizer's error JSON, not the UHP envelope (no `error.type`) |
| E-01, E-03 | errors §1, §3.1 | an unknown harness or response answered 200, expected 404 |
| E-02 | errors §3.1 | no `harness_not_found` code |
| H-01 | harnesses §1 | the response has no `harnesses` array |
| H-03 | harnesses §3 | no model catalogue |
| X-01 | sessions §2 | the listing answers 200 with no array of sessions |
| X-02 | sessions §2 | no pagination marker; a client must guess the end from a short page |
| X-08 | files §5 | a path-traversal probe against an artifact id was answered 200 instead of refused |
| F-02 | harnesses §4.1 | an unsupported base was accepted with 200 (full class, not targeted) |

The D, V, A and H rows share one cause: there is no UHP surface, so `/v1/uhp` and every other
protocol path is an unknown path behind the API-token wall. The E and X rows add a second cause
that is Colonizer's own: unknown paths fall through to the SPA fallback in
`crates/colonizer/src/server.rs` and answer 200 `text/html`, so a probe for a harness, a response
or an artifact looks like a success. The passes are A-01 (missing credential refused with 401) and
E-04 (no stack traces in error bodies).

The skips, by family: V-02, because discovery advertised no version to negotiate. H-02 and H-04,
X-05–X-07 and X-09, F-01 and F-03–F-08, and R-01–R-08 skip because the harness listing (or, for
X-07, an artifact download) they build on does not exist. X-03 and X-04 have no session id from an
earlier task, because no task ever ran. T-01–T-10, S-01–S-09 and C-01–C-03 all need a harness to
run a task against. P-01–P-10 skip on their face: the plugins capability is absent, and the
Plugins chapter is optional at every class.

## The gaps, in Colonizer terms

**Core** — discovery/lifecycle, versioning, auth/errors, harnesses, tasks, streaming,
continuation/cancellation. There is no UHP surface: every route is `/api/*`
(`crates/colonizer/routes.snap`) and §7 is proposal-only. Follow-up:
*UHP core class: implement the /uhp/v1 surface proposed in docs/protocol.md §7 (discovery,
harnesses, responses, streaming, errors)*.

That will not fix the 200s on its own: unknown `/api/*` paths fall through to the SPA fallback and
answer `index.html`, which drives E-01–E-03 and X-08 today and would swallow UHP 404s tomorrow.
Follow-up: *Unknown `/api/*` paths return 200 text/html (SPA fallback) instead of a JSON 404*.

**Extended** — sessions, files, artifacts. `GET /api/sessions` returns a bare, unpaginated array
(`crates/colonizer/src/sessions/api.rs`); there is no artifact API — `publish.rs` speaks
worktree/git/PR, not files; §7.5 proposes the routes. Follow-up: *UHP extended class: sessions
pagination and a colony file/artifact API (docs/protocol.md §7.5)*.

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
