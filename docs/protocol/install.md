# Updates, downloads and telemetry

Part of the [Colonizer protocol](../protocol.md).

## `GET /api/version`

What this mothership was built from, stamped in at build time by `crates/colonizer/build.rs`:

```json
{"version":"v0.1.4","commit":"1367191…","dirty":false,"built_at":"2026-09-17T17:21:32Z","release":"v0.1.4","development":false}
```

`version` is `git describe --tags --always --dirty`, so a build after a tag reads `v0.1.4-12-gabc1234`.
`release` is the last release tag the build contains, which is what an update is compared against. A
build from a source package with no git history reports the crate version and no commit.
`development` is true for anything but an exact release tag: commits after a tag, a dirty tree, or no
tag at all. `built_at`
honours `SOURCE_DATE_EPOCH`, so a release can still be built reproducibly.

## `GET /api/update` and `PUT /api/update`

Whether a newer release exists. **On by default**; `PUT {"enabled": false}` turns it off, and
`COLONIZER_UPDATE_CHECK=0` (or `false`, `off`) keeps it off from the environment (reported as
`blocked_by`; a `PUT` is then a **409**). Switching it on checks at once.

The check asks GitHub for the latest release of `Colonizer-dev/harness` a minute after start and every
six hours after that, and only while it is on: switched off, the mothership makes no request for it,
and forgets the last answer so no banner lingers. Drafts and prereleases are ignored. The request
carries a user agent and nothing about the install: the live map is separate, and off until switched
on (`telemetry.md`). `COLONIZER_RELEASES_URL` points the check elsewhere, for a fork or a test.

```json
{"enabled":true,"blocked_by":null,"installed":{…},"latest":{"version":"v0.1.5","url":"…","notes":"…","published_at":"…"},
 "available":true,"last_checked":"…","error":null}
```

`available` is true only when `latest` parses as a release newer than `installed.release`. A build
whose version cannot be placed is never told it is behind.

`apply` reports an update being installed: `{phase, version, started_at, error, log, colonies: [{id,
repo, outcome}], backup}`, `phase` one of `idle`, `draining`, `installing`, `restarting` or `failed`
(`draining` is the queue held back while the colonies still booting or publishing finish — the same
flag `GET`/`POST /api/admin/drain` drives). `can_apply` is
`{ok, reason}`: whether this install can update itself at all — a source checkout or a development build
cannot, and says so.

## `POST /api/update/apply`

Installs the latest release and restarts into it. Answers `{"started": true}` as soon as the work
starts; the installer is given 20 minutes. An optional body `{"force": true}` installs over a
development build or a build newer than the latest release.

It runs `scripts/install-release.sh` from inside the app (the same installer a person would run) so the
download, its checksum and the symlink swap are not reimplemented. A failure leaves the running version
untouched, because the installer unpacks beside it and moves the symlink last. Before it runs,
`sessions.json` (when there is one) is copied to `sessions.json.pre-update-<unix-timestamp>` beside it;
if that copy fails, nothing is installed and `apply.phase` is `failed`.

Refused with `409` when this is not a release install (no `scripts/install-release.sh`), the latest
release is not known yet, this is already the latest, this build is newer than the latest release (no
downgrade without `force`), or an update is already installing or restarting.

Refused with `409` when this mothership is a development build (`installed.development`) and `force` is
not set: a release could replace changes it does not contain, so the answer points at `git pull && scripts/install.sh --install`.

Refused with `409` when a colony is `publishing`: its microVM is already gone and the host is committing
and pushing, and interrupting that leaves the colony failed with its pull request unopened. A colony that
is merely working does not hold an update: it is detached, and `lifecycle::recover` reconnects it.

The installer is run with `COLONIZER_KEEP_PREVIOUS=1`, because colonies mount vendored plugins out of the
app directory this mothership started from (`resolve_assets` in `config.rs` canonicalises the symlink
away), and taking it out from under them would take their plugins too. Each session records that
directory as `app_slot`; at the next start, once recovery has settled, a kept directory is removed if no
live colony still names it.

## `POST /api/sandbox/pull` and `GET /api/sandbox/pull`

Downloads the configured colony image (after the stack preset) into microsandbox's cache, so a launch
boots instead of waiting on a registry. Settings calls `POST` when the sandbox module is saved, which
is the moment a stack is chosen. The image is the preset's reference pinned by digest in
`crates/colonizer/images.lock` and compiled into the mothership, so the cache ends up with the exact
bytes the release was tested with. An image set by hand with no lock row boots as written.

Under `auto`, the preset's default, that image is the Node stack's — `auto`'s fallback — because the
stack a colony actually boots is decided per repository, when its worktree is checked out: the
repository's marker files name it (`Cargo.toml` Rust, `go.mod` Go, `pyproject.toml`,
`requirements.txt`, `setup.py` or `Pipfile` Python, `package.json` Node), a marker at the repository
root beats one in a subdirectory, and a repository with none falls back to Node. Anything set
explicitly still wins.

`POST` returns at once (a cold pull of `node:24-bookworm` measured 108 s, too long to hold a request
open) and the download runs in the background. Calling it again while the same image is pulling
returns the running pull rather than starting a second. `GET` returns the most recent status:

```json
{"image": "python:3.13-bookworm@sha256:933b46a0…", "state": "pulling", "started_at": "…", "finished_at": null, "error": null}
```

`state` is `idle`, `cached` (already local, nothing done), `pulling`, `done` or `failed`. `POST` is a
`400` when no colony image is configured.

**There is no progress percentage.** `msb pull` draws its progress bar only on a terminal; piped, it
prints one line when it has finished, and `--info` adds only migration logs. Scraping the bar through a
pty would mean parsing an undocumented format that can change with any msb release, so the API reports
what is actually known: the image, when it started, and how it ended.

A launch still pulls a cold image itself if nothing got to it first, announcing it in the log and
recording it as the `image-pull` phase.

## `POST /api/headroom/download` and `GET /api/headroom`

Downloads the Headroom bundle pinned for this machine (see Token savings). Settings calls `POST` when the
agent module is saved with `headroom` switched on, and offers it as "Download now" while the switch is on
and nothing is downloaded.

`POST` returns at once and the download runs in the background. While the bundle is installed,
downloading or unpacking, `POST` returns that status without starting another download, and it returns
`409` when no bundle is pinned for this architecture. `GET` returns the status:

```json
{"release": "0.37.0-1", "state": "downloading", "bytes": 104857600, "total": 231330241, "started_at": "…", "finished_at": null, "error": null}
```

`state` is `idle` (not downloaded), `installed`, `downloading`, `unpacking`, `failed` (with `error`), or
`unavailable` (no bundle for this architecture, and `release` is null). Unlike the image pull, this reports
progress: `bytes` of `total`, updated about every megabyte.

Nothing appears at `<data>/headroom/<release>` until the archive's sha256 has matched and it has unpacked
completely. A mismatch or an interrupted download ends `failed` and leaves no partial files behind.

## `GET /api/telemetry` and `PUT /api/telemetry`

The live map on colonizer.dev (`docs/telemetry.md`). It is off until the user switches it on, and
the web UI asks once while `enabled` is `null`. `GET` returns:

```json
{
  "enabled": true, "blocked_by": null,
  "endpoint": "https://telemetry.colonizer.dev", "map_url": "https://colonizer.dev/live",
  "last_sent_at": "…", "last_error": null,
  "heartbeat": {"install_id": "0b0c9a8e-…", "version": "0.1.3", "platform": "darwin-arm64", "colonies": 2}
}
```

`heartbeat` is exactly what the next heartbeat will send; `install_id` is `null` until the map is first
switched on. `COLONIZER_TELEMETRY_URL` points `endpoint` elsewhere. `blocked_by` names `DO_NOT_TRACK` or `COLONIZER_TELEMETRY` when the environment keeps it
off, and `enabled` is then `false`.

`PUT` with `{"enabled": true|false}` saves the answer to `<config>/telemetry.json` and returns the same
status. Switching on creates a random `install_id` and sends a heartbeat within a second or two.
Switching off sends `{"install_id", "online": false}` and forgets the id. `PUT` returns `409` while the
environment keeps it off.

## `GET /api/telemetry/usage` and `PUT /api/telemetry/usage`

Anonymous usage reporting, separate from the live map ([usage-data.md](../usage-data.md)). The mothership
builds the batch — Cratefield's `module-telemetry` payload ([usage-data.md](../usage-data.md)) — so you
can read exactly what a send carries, here or with `colonizer telemetry show`. The batch is sent at
most once a day, and only when `COLONIZER_TELEMETRY_ENDPOINT` names a collector in the mothership's
environment: unset, nothing is sent, ever. The choice is kept in `<config>/usage.json` and is on
unless switched off.

```json
{"enabled": true, "blocked_by": null, "payload_version": 1, "batch": {"schema": 1, "install": "…",
 "client": {"kind": "server", "version": "…", "platform": "linux", "arch": "x86-64"},
 "modules": ["mothership"], "events": [{"name": "colonies.parallel_now.2-3", "outcome": "ok",
 "error": "none", "duration": "unknown", "count": 1}, {"name": "boot.git.1-2s", …}]}}
```

Counts and durations are bucket labels, settings are names without values. `blocked_by` names
`COLONIZER_TELEMETRY`, `DO_NOT_TRACK` or `CI` when the environment holds reporting off. `PUT
{"enabled": bool}` saves the choice (off forgets the id behind `install`, on makes a new one) and
answers the same status; **409** while the environment holds it off.
