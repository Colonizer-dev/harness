# Updates

How a mothership knows which version it is, how it finds out that a newer one
exists, and what installing it does to the colonies that are running.

Installing for the first time is [docs/install.md](install.md). What changed in
each release is [CHANGELOG.md](../CHANGELOG.md).

## Which version am I running

```sh
colonizer version
```

```
v0.1.4 (1367191, built 2026-09-17T17:21:32Z)
```

The same three facts — the tag, the commit and the build time — are in
**Settings → Updates**, and at `GET /api/version`. They are stamped into the
binary at build time from git, not read from a file next to it, so a binary
cannot be made to claim a version it is not. The release workflow passes the
tag and commit in as `COLONIZER_DESCRIBE` and `COLONIZER_COMMIT`, because the
Linux harness is built in a container without git; a build that sets them is
stamped with them rather than asking git.

A build that is not a release says so:

```
v0.1.4-12-gabc1234 (abc1234, modified tree, built 2026-09-18T09:03:11Z) — development build
```

That happens for a build after a tag, a build from a modified tree, and a build
from a source package with no git history at all. Settings shows the same thing
as a badge, and does not offer to replace your own build with a release.

## The check

On by default. A minute after start, then every six hours, the mothership asks
GitHub for the repository's latest release and compares its tag with the release
this build came from. A draft or a prerelease is ignored.

It is one `GET` to `api.github.com`, carrying nothing but a `colonizer/<version>`
user agent — no id, no machine, no colonies. That is a different thing from the
[live map](telemetry.md), which is off until you switch it on.

| To | Do |
| :--- | :--- |
| Turn the check off | **Settings → Updates**, or `COLONIZER_UPDATE_CHECK=0` |
| Point it somewhere else | `COLONIZER_RELEASES_URL` |

Switched off, it makes no request at all rather than making one and discarding
the answer. `COLONIZER_UPDATE_CHECK` counts as off when it is exactly `0`,
`false` or `off`. Set in the environment, the switch in Settings is disabled and
says which variable is holding it off, and `PUT /api/update` answers `409`.

A failed check keeps the last answer it had, so a flaky network does not hide a
release you were already told about.

## Checking without a mothership

`colonizer update` needs a running mothership to install anything, but
`colonizer update --check` does not: it asks GitHub there and then, honouring
`COLONIZER_RELEASES_URL`, and prints one line. It installs nothing, needs no
token, and ignores `--host` and `--token-file`. It is what a CI release health
check runs against an installed build.

```sh
$ colonizer update --check
v0.2.2 is available (this is v0.2.1); run `colonizer update` to install it.
```

The line is one of three: `{installed} is the newest release.` when there is
nothing newer, the above when there is, and
`this is a development build; the newest release is {latest}.` for a build that
is not a release. Either non-error answer exits `0`; a failed fetch exits
non-zero with the error. `--check` cannot be combined with `--force`.

## Updating in place

**Settings → Updates** offers the newer release with its notes, and a button
that installs it and restarts into it. From a terminal, against a mothership
that is already running:

```sh
colonizer update
```

`colonizer update` talks to the mothership on `COLONIZER_BIND` — or the one
`--host` names — with the local token file (`<config dir>/api-token`), or the
token `COLONIZER_TOKEN` or `--token-file` gives. It needs the check to have run:
with the check switched off, or before its first answer (a minute after start),
it says so and installs nothing.

Two builds are refused, and the refusal names both versions: a development
build, which holds work no release contains, and a release newer than the
latest one, which would be a downgrade rather than an update. Either refusal
takes `--force`:

```sh
colonizer update --force
```

That installs the latest release anyway. It prints a warning naming both
versions first, backs `sessions.json` up and prints where, and still steps back
rather than restarting if a colony is publishing when the drain runs out: a
publish is waited for first, and the update is refused rather than cutting the
push off. Forcing needs a known latest release, like any update.

Both do the same thing, because the command is a client of the same two routes
the pane uses — `GET /api/update` and `POST /api/update/apply`. Neither
downloads anything itself: the mothership runs `scripts/install-release.sh`,
shipped inside the app, which is the same installer the one-line install command
runs. The download is checked against the release's `SHA256SUMS`, and against
the build attestation when `gh` can reach a verdict — `gh` must be installed
and logged in; the mothership passes the installer the GitHub token saved in
settings when the environment carries none. The installer gets 20 minutes; past
that the update is marked failed and the running version is left as it was.

What happens, in order:

1. **The backup is taken first.** `sessions.json` is copied to
   `sessions.json.pre-update-<unix-timestamp>` beside it; if that copy fails,
   the update is marked failed and nothing is installed. The copies are not
   pruned, and are safe to delete. The copy's path is reported on the update
   progress (`GET /api/update` answers it as `apply.backup`), and `colonizer
   update` prints it as `sessions.json backed up to <path>` while it waits. The mothership also logs it
   to its own output, so the path survives the restart for an update started from Settings, whose
   progress is in memory.
2. **The mothership drains.** It stops admitting new boots — a launch or a resume
   asked for while it drains is queued rather than started, so no boot begins a
   microVM the restart would strand — and waits for what is already in flight: a
   colony still booting or publishing gets up to five minutes
   (`COLONIZER_DRAIN_TIMEOUT_SECS`) to finish. Scripts can drive the same flag
   through `GET`/`POST /api/admin/drain` (owner token; the routes are in the
   table [below](#the-routes)) and poll until `ready`. A boot the wait gives up
   on is requeued on the next start with `interrupted_by_restart` on its log,
   not left stopped. A publish the wait gives up on is different: its microVM is
   already gone and the host is committing and pushing, and interrupting that
   leaves a colony `failed` with its pull request unopened — so the update stops
   there instead. It clears the drain, marks the apply failed and says which
   colony is still publishing, and the pane asks you to try again once it
   finishes.
3. **The release is unpacked beside the running app**, into whichever of the two
   slots — `app-a`, `app-b` — the running version is not using. A failure
   part-way leaves the running version exactly as it was. The installer refuses
   to stage into a slot a process is still running from (naming the pid), so a
   hand-run install cannot pull a boot's `msb` out from under it either.
4. **The `app` symlink is moved with one rename.** There is no moment at which
   it points at half an install.
5. **The process replaces itself** with the new binary, `<app>/bin/colonizer`,
   started with the same arguments. Before it does, it takes the mothership off
   the [live map](telemetry.md) if that is on and stops the mesh, so the new
   process can take its ports. Colonies are detached microVMs, so each live one
   is reconnected and its event stream carries on from the sequence number it
   had. The pane lists every colony and what happened to it.

The browser reconnects on its own; a colony's chat continues where it stopped.

On macOS the Keychain ties each saved secret to the binary that wrote it, so the
new binary is re-signed before the switch when this host knows an identity:
`COLONIZER_CODESIGN_IDENTITY` in the environment, or the identity recorded beside
the app, in `~/.local/share/colonizer/codesign-identity`, by the install that set
it. A release install records an identity it was run with, so an update started
from Settings — whose installer child has no environment of its own — re-signs
too, and no update loses the Keychain access its owner already granted. If
`codesign` fails the update stops before the symlink moves and the running
version is left as it was, so nothing is switched to a binary the Keychain would
not recognise. Delete the recorded file to stop re-signing; an identity in the
environment still signs. See [The system
keychain](configuration.md#the-system-keychain).

## Notices and affected colonies

Most releases are routine, and the cockpit says so quietly: a dot in the rail and the release in
**Settings → Updates**. A release that fixes something an operator may be hitting right now says so
in its notes. A changelog fragment can carry a `critical` or `fixes-running` line written for the
operator ([changelog.d/README.md](../changelog.d/README.md#a-fix-the-operator-should-hear-about-before-updating)),
and the release body carries those lines in a block `GET /api/update` reads, together with the
ones from the nine releases before it, so a mothership a few releases behind still hears about the
fixes in between. The ones newer than the running build come back as `notices`, and the cockpit
shows them as a banner above every view: *Update to v0.2.7: Fixes colonies failing with
UND_ERR_SOCKET (sandbox credential scanner). 4 of your colonies are affected.*

**Affected here.** A notice can name a probe: a read-only check, compiled into the harness, that
the mothership runs on its own disk to count the colonies the fix is for. A release cannot send code
to run, only the id of a probe the running build already has; a notice naming one this build does
not know is shown without a count (`affected: null`). Nothing a probe reads leaves the machine. Each
answer is reused for 30 seconds.

| Probe | What it reads | Matches |
| :--- | :--- | :--- |
| `msb-body-secret-violation` | The last MiB of each running colony's `$MSB_HOME/sandboxes/<sandbox>/logs/runtime.log` (`~/.microsandbox` by default) | A `secret violation` line with `location=body`: msb 0.7.3's credential scanner blocking the colony's own request ([#1096](https://github.com/Colonizer-dev/harness/issues/1096)) |

**After the update.** A colony's microVM keeps the vendored components it booted with (msb, the
plugins, the agent modules), from the app slot it started from, until it is stopped and resumed. An
update reconnects running colonies rather than restarting them, so a colony stuck on a bug the
update fixed stays stuck. `GET /api/update` lists the colonies still running on a previous slot as
`behind`, each with the notice lines whose probe matched it (`affected_by`). **Settings → Updates**
offers **Restart on the new version** for each of them and for all of them. The cockpit banners the
affected ones, with the same button. A restart is the stop and resume the colony's own buttons do:
the microVM is taken down and booted again from the current slot, and the worktree and the
conversation are kept. Restarts run one at a time in the background; `restarts` on `GET /api/update`
follows them, and names any that failed.

## The previous version is kept for a while

An update applied in place, from Settings or with `colonizer update`, passes
`COLONIZER_KEEP_PREVIOUS=1` to the installer, so the slot it replaced stays on
disk. Colonies mount vendored plugin directories straight
out of the slot their mothership started from, and taking that away while a
colony is reading it breaks the colony, not the upgrade.

The next start sweeps it: once colonies have been recovered, any slot that is
neither the one this process is running from nor mounted by a live colony is
removed. Nothing accumulates beyond the two slots.

**Take care:** an installer run by hand still replaces the app symlink
immediately and does not wait for a drain, which is right when nothing is
running and wrong when something is. It will not, though, delete a slot a
process is executing from: it refuses when the slot it would stage into is in
use, and keeps a previous slot a running mothership or colony still reads from
for the next start to sweep. It finds those processes even when they were
started through a symlink outside the slot (the way `~/.local/bin/colonizer`
starts the mothership): by each process's real executable — `/proc/<pid>/exe` on
Linux, `lsof` when it is installed on macOS — as well as by its command line. If
colonies are running, update from Settings or with `colonizer update`: it drains
first.

## When it cannot be applied from here

The pane says why instead of offering a button, and `colonizer update` prints
the same reason:

| Reason | What to do |
| :--- | :--- |
| This install has no `scripts/install-release.sh` — it did not come from a release | Update the way you installed: `git pull && scripts/install.sh --install` for a checkout |
| The app path is not the symlink the installer maintains | Install once from a release, or set `COLONIZER_APP` to the symlink |
| Running without an installed app directory | Same |
| This is a development build (`v0.1.5-60-gd62bfb2`, a modified tree, or no tag): a release would replace work it does not contain | Update it from its checkout: `git pull && scripts/install.sh --install` |
| Running `v0.1.6`, newer than the latest release `v0.1.5`: installing it would be a downgrade | Wait for a newer release, or pass `--force` to install `v0.1.5` anyway |
| Provenance could not be checked because `gh` is not logged in (a note, or an update failure under `COLONIZER_REQUIRE_ATTESTATION=1`) | Run `gh auth login`, or set `GH_TOKEN`; the mothership passes the GitHub token saved in settings to the installer when the environment carries none |

A source checkout is meant to be updated with git. Saying so is better than
half-applying something. **Settings → Updates** also says how to switch such a
build to releases (`switch_to_releases` on `GET /api/update`): the one-line
installer for a build with no installed app, or `colonizer update --force` for a
development build that sits in one. Updating a development install from `main`
in place is not built.

## By hand

For a release, run the install command again:

```sh
curl -fsSL https://colonizer.dev/install.sh | sh
```

For a checkout, `git pull && scripts/install.sh --install`. Either way, restart
`colonizer` afterwards. Settings, credentials and colonies live outside the app
directory, so a rebuild leaves them alone and the colony list is read back at
start — see [Where things live](install.md#where-things-live).

The restart drains too. `systemctl --user restart colonizer` — or any systemd
stop, or a `kill` on the process — sends SIGTERM, and the mothership then stops
admitting new boots and waits up to five minutes for the colonies that are
booting or publishing before it exits — `COLONIZER_DRAIN_TIMEOUT_SECS`, the same
budget an update uses. (Ctrl-C at a terminal sends SIGINT and does not drain; it
stops at once.) While it drains on SIGTERM the HTTP API is no longer served: the
server shuts down with the signal and the drain runs in its place, so the log
the mothership writes is the only progress. A script that needs to watch a drain
should poll `GET /api/admin/drain` and only signal once `ready` is true.

The user unit is written with `KillMode=mixed` and `TimeoutStopSec=330` (the
LaunchAgent's `ExitTimeOut`). `KillMode=mixed` sends SIGTERM to the main process
alone, never to its children, so nothing tears an `msb`, `git-remote-http` or
`gh` out from under a colony that is still draining; `TimeoutStopSec=330` — the
five-minute drain plus slack — bounds how long the main process may take. Once
the main process has exited, drained or out of time, systemd SIGKILLs whatever is
left in the cgroup, and that is how the leftover helper children go. Raising
`COLONIZER_DRAIN_TIMEOUT_SECS` above about 300 s therefore also needs a longer
`TimeoutStopSec` (and `ExitTimeOut` in the plist), or the service manager kills
the mothership part-way through its drain; re-write the unit with
`colonizer login-item enable` after changing it. After the restart, check the
old service left nothing running:

```sh
systemctl --user status colonizer              # the CGroup tree should name only the new main process
systemd-cgls --user-unit colonizer.service     # the same tree, as processes
```

## The routes

`GET /api/version` accepts any token with the `read` scope; the rest need the
owner token (the sign-in link's).

| Route | What it answers |
| :--- | :--- |
| `GET /api/version` | The build: version, commit, dirty, built at, the release it descends from, whether it is a development build |
| `GET /api/update` | `installed` (the above), plus `enabled` and `blocked_by` (the check's switch and the variable holding it off), `latest`, `available`, `last_checked`, `error`, `can_apply` (`{ok, reason}`), `apply`, how an update in flight is getting on (`phase`, `version`, `started_at`, `error`, `log`, `colonies`, `backup`), `notices` (the [notices](#notices-and-affected-colonies) newer than this build: `version`, `severity`, `line`, `probe`, `issue`, `affected` — `{count, colonies}` or `null`), `behind` (colonies still on a previous app slot: `id`, `repo`, `status`, `slot`, `affected_by`), `restarts` (`{restarting, failed}`) and `switch_to_releases` (`{reason, command, then}`, or `null` for a release install) |
| `PUT /api/update` | `{"enabled": true\|false}` — the check. Answers the same body as `GET`, or `409` while the environment keeps the check off |
| `POST /api/update/apply` | Install the newer release and restart into it; an optional `{"force": true}` body installs the latest release over a development build or a newer release instead (no body means no force, anything else that is not JSON is a 400) |
| `POST /api/update/restart` | `{"ids": [...]}` or `{"all": true}`: stop and resume the colonies in `behind` so they boot on this version. Answers `{restarting, skipped}` at once (an id that is not behind, or already restarting, is skipped with the reason); `400` with neither, `409` while an update is being applied |
| `GET /api/admin/drain` · `POST /api/admin/drain` | The drain, for a script that updates or restarts on its own. `POST` (no body, or `{"draining": false}` to cancel) starts or cancels it and both answer `{draining, since, in_flight, ready}`: `ready` is what a script polls for before installing or killing the process, and `in_flight` counts the colonies still booting or publishing |

Designed in [#45](https://github.com/Colonizer-dev/harness/issues/45); the
version stamp is [#110](https://github.com/Colonizer-dev/harness/pull/110), the
symlinked install [#104](https://github.com/Colonizer-dev/harness/pull/104) and
applying in place [#112](https://github.com/Colonizer-dev/harness/pull/112).
