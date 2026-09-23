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
the answer. Set in the environment, the switch in Settings is disabled and says
which variable is holding it off.

## Updating in place

**Settings → Updates** offers the newer release with its notes, and a button
that installs it and restarts into it. From a terminal, against a mothership
that is already running:

```sh
colonizer update
```

Both do the same thing, because the command is a client of the same two routes
the pane uses — `GET /api/update` and `POST /api/update/apply`. Neither
downloads anything itself: the mothership runs `scripts/install-release.sh`,
shipped inside the app, which is the same installer the one-line install command
runs. The download is checked against the release's `SHA256SUMS`, and against
the build attestation when `gh` can reach a verdict.

What happens, in order:

1. **Colonies are looked at first.** A colony that is publishing holds the
   update: its microVM is already gone and the host is committing and pushing,
   and interrupting that leaves a colony `failed` with its pull request
   unopened. The pane says which colony, and you try again when it is done.
   As the install starts, `sessions.json` is copied to
   `sessions.json.pre-update-<unix-timestamp>` beside it; if that copy fails,
   the update is marked failed and nothing is installed. The copies are not
   pruned, and are safe to delete.
2. **The release is unpacked beside the running app**, into whichever of the two
   slots — `app-a`, `app-b` — the running version is not using. A failure
   part-way leaves the running version exactly as it was.
3. **The `app` symlink is moved with one rename.** There is no moment at which
   it points at half an install.
4. **The process replaces itself** with the new binary. Colonies are detached
   microVMs, so each live one is reconnected and its event stream carries on
   from the sequence number it had. The pane lists every colony and what
   happened to it.

The browser reconnects on its own; a colony's chat continues where it stopped.

## The previous version is kept for a while

An update applied from Settings passes `COLONIZER_KEEP_PREVIOUS=1`, so the slot
it replaced stays on disk. Colonies mount vendored plugin directories straight
out of the slot their mothership started from, and taking that away while a
colony is reading it breaks the colony, not the upgrade.

The next start sweeps it: once colonies have been recovered, any slot that is
neither the one this process is running from nor mounted by a live colony is
removed. Nothing accumulates beyond the two slots.

**Take care:** an installer run by hand does not do this. It replaces the app
directory immediately, which is right when nothing is running and wrong when
something is. If colonies are running, update from Settings or with `colonizer
update`.

## When it cannot be applied from here

The pane says why instead of offering a button, and `colonizer update` prints
the same reason:

| Reason | What to do |
| :--- | :--- |
| This install has no `scripts/install-release.sh` — it did not come from a release | Update the way you installed: `git pull && scripts/install.sh` for a checkout |
| The app path is not the symlink the installer maintains | Install once from a release, or set `COLONIZER_APP` to the symlink |
| Running without an installed app directory | Same |
| This is a development build (`v0.1.5-60-gd62bfb2`, a modified tree, or no tag): a release would replace work it does not contain | Update it from its checkout: `git pull && scripts/install.sh --install` |

A source checkout is meant to be updated with git. Saying so is better than
half-applying something.

## By hand

For a release, run the install command again:

```sh
curl -fsSL https://colonizer.dev/install.sh | sh
```

For a checkout, `git pull && scripts/install.sh`. Either way, restart
`colonizer` afterwards. Settings, credentials and colonies live outside the app
directory, so a rebuild leaves them alone and the colony list is read back at
start — see [Where things live](install.md#where-things-live).

## The routes

| Route | What it answers |
| :--- | :--- |
| `GET /api/version` | The build: version, commit, dirty, built at, the release it descends from, whether it is a development build |
| `GET /api/update` | The above, plus the latest release, whether one is available, when it was last checked, whether it can be applied here, and how an update in flight is getting on |
| `PUT /api/update` | `{"enabled": true\|false}` — the check |
| `POST /api/update/apply` | Install the newer release and restart into it |

Designed in [#45](https://github.com/Colonizer-dev/harness/issues/45); the
version stamp is [#110](https://github.com/Colonizer-dev/harness/pull/110), the
symlinked install [#104](https://github.com/Colonizer-dev/harness/pull/104) and
applying in place [#112](https://github.com/Colonizer-dev/harness/pull/112).
