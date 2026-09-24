# Security-hunter modules

What ships today: one `Manifest` per hunter, a `hunters.lock` pin per platform, an operator
opt-in (`COLONIZER_HUNTER_INSTALL=1`) guarding a Linux-only, race-free, checksum-verified
on-demand install, and a capability probe. No scan runs yet: the Strix and SARIF parsers exist
but nothing calls them, hunter LLM traffic is not routed anywhere, and no run stage drives
these modules.

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
| `install` | The only supported install path: the `POST /api/hunters/{id}/install` route, which installs the pinned, checksum-verified artifact from `hunters.lock` behind the `COLONIZER_HUNTER_INSTALL` opt-in |
| `scan` | Headless scan invocation template with `{target}`/`{mode}`/`{out}` placeholders |
| `output` | Run-relative filenames the hunter writes |
| `findings_format` | Which parser reads the output: `strix_json` or `sarif` |
| `gateway_env` | Env var that would point the hunter's LLM client at the Colonizer gateway, once routing exists (planned, not implemented) |
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

Binary artifacts pin in `hunters.lock` (id, version, platform, kind, sha256, url — the same
columns as `headroom.lock`), compiled into the binary and cached under
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

## Planned, not implemented (tracked in #216)

- Hunter LLM traffic routed through the gateway (`gateway_env`, landing in `routed_cost_usd`).
- Findings flowing through orchestrator validation subject to `MAX_PER_COLONY`.
- A red-team run driving hunters: refusing to start one whose probe is not ready, or serialising
  installs (installs already serialise themselves per hunter; see above).

## How to add a hunter

1. Write a `Manifest` and add it to `builtin()`.
2. A SARIF-emitting hunter needs no new parser — reuse `parse_sarif`.
3. A non-SARIF hunter adds one parser function and a `FindingsFormat` variant, then dispatches
   it from `normalize`.
4. Pin binary artifacts in `hunters.lock` for both architectures.
5. Flip `available` once the parser and the pin are in.

## Current state

Strix is phase one: manifest, pinned lock, opt-in install, probe, and two parsers
(`parse_strix`, `parse_sarif`) that exist but are not wired to anything — no scan runs, no
findings flow. Shannon is a manifest-only stub (phase 2): its manifest, install command, scan
template and SARIF format are recorded, but there is no download and no driving run yet. The
cockpit module gallery will extend the existing `/api/plugins` surface rather than adding a
parallel one, and the `logo` field is a name only until that gallery renders it.
