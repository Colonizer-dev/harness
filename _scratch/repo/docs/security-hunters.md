# Security-hunter modules

Red-team runs drive external security hunters — AI pentesters like Strix and Shannon — against a
target, then file what they find through the normal findings flow. This page describes how hunters
plug into the mothership.

## On demand, verified, never vendored

Hunter binaries download on demand at a pinned version and are checksum-verified before they run.
They are never vendored into the binary or the repo, which keeps licences clear: Strix is
Apache-2.0 and Shannon is AGPL-3.0, and the mothership shells out to both rather than linking
anything. Shannon is not even a download — it installs via `npx` as a Node package.

## The manifest

Each hunter is a `Manifest` in `src/hunters.rs` (`builtin()`), with one row per field:

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
| `install` | How the operator installs on demand — a documented command |
| `scan` | Headless scan invocation template with `{target}`/`{mode}`/`{out}` placeholders |
| `output` | Run-relative filenames the hunter writes |
| `findings_format` | Which parser reads the output: `strix_json` or `sarif` |
| `gateway_env` | Env var pointing the hunter's LLM client at the Colonizer gateway |
| `available` | True once a parser plus a checksum pin ship; false is a manifest-only stub |

## The two runtimes

`binary` hunters (Strix) download from `hunters.lock` into `<data_dir>/hunters/<id>/<version>/`
and run from there. `node` hunters (Shannon) run through `npx` with the pinned package version,
so there is nothing to download or verify — the install command is the whole story.

## Docker in a microVM

Colonies are KVM microVMs without a Docker daemon, so hunters that need containers run with
`DOCKER_HOST` pointed at a host-side daemon — Strix supports this explicitly. The capability
probe (`GET /api/hunters/{id}/probe`) reports the runtime plus Docker separately and fails
loudly with an actionable message: a missing runtime names the runtime, a missing daemon names
`DOCKER_HOST`. A red-team run refuses to start a hunter whose probe is not ready.

## Gateway routing

`gateway_env` names the variable that points the hunter's LLM client at the Colonizer gateway
route — `LLM_API_BASE` for Strix, `SHANNON_AI_BASE_URL` for Shannon — so hunter tokens land in
`routed_cost_usd` like any other model spend. Strix's own model switch (`STRIX_LLM`) takes
`<provider-id>/<model>`.

## Findings flow

Hunter output is parsed into `Finding{title, body, evidence}` — never filed raw. From there the
existing orchestrator validation stage takes over and files confirmed findings as issues, subject
to the same `MAX_PER_COLONY` cap as any other finding. A Strix record carries its severity, CWE
and PoC/code locations into the finding's evidence; a SARIF result carries its rule, mapped
severity and location.

## On-demand download

Binary artifacts pin in `hunters.lock` (id, version, platform, kind, sha256, url — the same
columns as `headroom.lock`), compiled into the binary and cached under
`<data_dir>/hunters/<id>/<version>/`. A new pin re-downloads; the old directory stays until it
is replaced, so a failed download never leaves a half-written binary behind. On-demand install assumes a single in-flight install per hunter (the red-team run stage serialises installs); concurrent installs of the same hunter are not yet guarded.

## How to add a hunter

1. Write a `Manifest` and add it to `builtin()`.
2. A SARIF-emitting hunter needs no new parser — reuse `parse_sarif`.
3. A non-SARIF hunter adds one parser function and a `FindingsFormat` variant, then dispatches
   it from `normalize`.
4. Pin binary artifacts in `hunters.lock` for both architectures.
5. Flip `available` once the parser and the pin are in.

## Current state

Shannon is a manifest-only stub (phase 2): its manifest, install command, scan template and
SARIF format are recorded, but there is no download and no driving run yet. The cockpit module
gallery will extend the existing `/api/plugins` surface rather than adding a parallel one, and
the `logo` field is a name only until that gallery renders it.
