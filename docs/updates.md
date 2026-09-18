# Updates

The mothership notices when a newer release is out, tells you in Settings, and can install it without
losing colonies: the new release is installed beside the running one, one symlink is swapped, and the
mothership replaces itself on the way through. Colonies keep running and reconnect on their own.

**The check is on by default.** Unlike the live map there is no question to answer first: checking is on
until someone turns it off. You can switch it off at any time in Settings, under Updates.

## What the mothership knows about itself

Every build records what it is. `colonizer version` prints it, Settings → Updates shows it, and
`GET /api/version` returns it:

```json
{"version": "v0.1.3", "release": "v0.1.3", "commit": "d89ce76f8e0d884b997d8392d5c0adbde6a3202c",
 "dirty": false, "built_at": "2026-09-17T12:00:00Z", "development": false}
```

| Field | What it is |
| :--- | :--- |
| `version` | What `git describe` said when the binary was built, e.g. `v0.1.3` or `v0.1.3-12-gabc1234`. A build with no git answer — a crate build from crates.io — reports the crate version. |
| `release` | The release tag this build contains, `v0.1.3` even for a build twelve commits past it. This is what a newer release is compared against. |
| `commit` | The commit the binary was built from, or `null` when it could not be recorded. |
| `dirty` | Whether anything was uncommitted at build time. |
| `built_at` | RFC 3339 UTC. Release builds pin it with `SOURCE_DATE_EPOCH`, so the same source stamps the same time on every building machine. |
| `development` | True for anything that is not exactly a tagged release: ahead of its tag, built from a dirty tree, or never on a tag. |

The build script asks git for all of this and git may decline — a crates.io build has no `.git` to ask —
and a missing answer is an empty field, never a build failure.

## The update check

While the mothership runs it asks `https://api.github.com/repos/Colonizer-dev/harness/releases/latest`,
starting a minute after start and again every 6 hours. A check that failed, or that was skipped while the
switch was off, is retried after 30 minutes instead. The request is one anonymous `GET`, with no token
and no query, and it carries nothing about the install — not the version, not the platform, not how many
colonies run here. GitHub sees what any visitor sees: the machine's IP address and a
`User-Agent: colonizer/<version>` header, which its API requires of every client. Sending anything more —
a version, an id, a count — is issue #14's business, not this check's.

GitHub answers with the repository's latest release: tag, name, notes and publish date. GitHub's `latest`
is never a draft or a prerelease, so an `rc` tag waits for the release it names. The answer is kept for
Settings and the web UI; a failed check keeps the last good answer and records the error. Turning the
switch on checks again at once.

Three overrides move the whole thing: `COLONIZER_UPDATE_API` points at another GitHub API root and
`COLONIZER_UPDATE_REPO` at another repository — what a test, or an air-gapped mirror, uses.
`COLONIZER_UPDATE_DOWNLOAD_URL` moves where release bundles are downloaded from when one is applied.

## Keeping it off for good

The Settings switch is kept in `~/.config/colonizer/update.json`. `COLONIZER_UPDATE_CHECK=off` (also
`0`, `false` or `no`, any case) in the mothership's environment keeps the check off whatever the switch
says — the same escape hatch `DO_NOT_TRACK` is for the live map, for a machine that must not contact
GitHub at all. Settings shows the switch as off and will not change it, the API answers 409, and no
request is made at all.

## What a development build is told

A development build is not exactly a tagged release, and it is only told about releases newer than the
tag it contains: a build of `v0.1.3-12-gabc1234` hears about `v0.2.0`, but is never told that `v0.1.3` —
which it already contains — is an update. A build with no tag at all takes any tagged release as newer.
The tag a development build stands on never shows up as its own update.

## Applying an update

Settings → Updates shows what the last check found, and an **Apply** button when there is something to
apply; `colonizer update` (below) does the same from a terminal. An update is refused, with the reason
and nothing changed, while a colony is publishing — its pull request is filed by this very process,
which the restart would replace — or while another update is already being applied. It also needs the
check on: the check is what found the release.

The walk, all of it done by the mothership:

1. **Download and verify.** The release's `SHA256SUMS` and the bundle for this platform,
   `colonizer-<platform>.tar.gz`, are downloaded from
   `https://github.com/Colonizer-dev/harness/releases/download/<tag>`. The bundle is hashed as it
   streams and must match the release's sums before anything is installed; a mismatch, or an
   interrupted download, fails the update and leaves nothing behind.
2. **Reassemble what a release leaves out.** A release carries no Anthropic code, so the Claude Agent
   SDK is fetched from the npm registry against the checksum the release recorded, the way the
   installer does at first install. On a Mac, the Linux build of Claude Code that colonies run is
   copied forward from the running install.
3. **Install beside, not over.** The result lands in `versions/<tag>`, next to the version that is
   running. Nothing anything is still reading is touched: a colony's read-only mounts follow the
   directory it booted from, and that directory keeps its name.
4. **Swap the link.** `app` is repointed at the new version by renaming a staged link over it, a rename
   that is atomic on POSIX, so nothing reading through `app` — this process's own path, a colony's
   mount table, `~/.local/bin/colonizer` — ever sees it missing or dangling. Until the swap lands, a
   failure reports `failed` with the old release still running.
5. **Replace the process.** Sessions are saved, the mesh is shut down cleanly, the live map is told the
   mothership is going away, and the new binary is execed through the link with the same arguments.

The UI follows along: `downloading` (with `bytes` of `total`), `verifying`, `unpacking`, `installing`,
`restarting`.

## What an operator actually sees

- **Colonies are not stopped.** Their microVMs are deliberately left running while the mothership
  replaces itself. On the way back up, every colony that was running is reconnected to its microVM: the
  chat and terminal come back where they were, because the event log lives in the colony and replays.
  A colony whose log says `harness restarted: reconnecting to the running microVM` has been through an
  update and come out the other side.
- **The gap is seconds.** The UI shows `restarting` for about two seconds before the connection drops;
  that drop is the update succeeding, and the mothership comes back on the new version.
- **A model request in flight may be retried once.** A routed request that reaches the gateway while
  the mothership is being replaced sees it vanish; the colony's router resends that request once, to
  Anthropic with the route's `fallback_model`. With no fallback configured the request fails visibly
  and Claude Code reports the error. Requests to unrouted models go straight to Anthropic and do not
  notice.
- **Nothing in progress is thrown away.** A colony that was mid-publish blocks the update rather than
  being lost. If the harness restarts some other way while a colony is publishing, that colony is
  marked failed with its worktree intact, and it can publish again.

## Where it all lands

```text
~/.local/share/colonizer/
  versions/v0.1.3/   bin/ vendor/ plugins/ modules/ web/ VERSION LICENSE NOTICE
  versions/v0.1.4/
  app -> versions/v0.1.4
```

`~/.local/bin/colonizer` links through `app`. A version directory is written once, by renaming a fully
staged copy into place, and afterwards only ever renamed or deleted whole — never modified where it
stands.

Old versions are removed on the next start — and nothing still in use ever is. Kept: the version the
mothership runs from, whichever one `app` names now, and the version of every live colony — its mounts
reach into that directory, so deleting it would reach into a running colony. Everything else is only
disk. The installers keep the version they install and the one before it; the mothership prunes again
with knowledge of which colonies still run which. Nothing is deleted in place, either: a directory
being replaced or retired is renamed aside first — a bind mount follows the inode, so a colony reading
from it keeps working — and the delete waits until nothing holds it any more: the `app` link or the
previous release no longer resolving into it, no colony bind-mounting out of it, no running process
executing out of it. With no `/proc` to ask (macOS) a directory counts as in use and is kept — and
since nothing there can ever be shown to be free, no deferred delete ever finishes: versions and
leftovers simply accumulate until they are removed by hand. A delete
that has to wait leaves a dot-prefixed leftover (`.v0.1.3.old`, `.app.legacy`) — the installer says so —
which the next installer run and the startup prune both reclaim once nothing holds it. Pruning never
follows a symlink out of `versions/`, and it leaves the dot-prefixed staging names of an install in
progress alone.

## `colonizer update`

```sh
colonizer update
```

All the work happens in the mothership; the command is only its command line. It asks the mothership on
`COLONIZER_BIND` to check, applies what it finds, and follows the progress — download counts, then the
states above — until the mothership restarts onto the new version, gives up, or stops answering. With
nothing newer it says so and exits. With no mothership running it says to start one first. The check
has to be on: the command is refused otherwise, like the button in Settings.

## What this does not do

- **It applies published release bundles, and only for the platforms that have them.** Linux x86_64 and
  Apple Silicon. Anything else is told so in Settings: there are no release bundles for it.
- **It will not update a source checkout.** A mothership running from `./dist` is told so; `git pull`
  and `scripts/install.sh` remain the way.
- **It will not update an install that predates the versioned layout.** One with a real `app` directory
  where the symlink now goes is refused: run the release installer once and it migrates the install to
  the layout, and updates in place work from then on.
- **There is no rollback.** To go back, run the release installer with `COLONIZER_VERSION=v0.1.3`: it
  installs that release beside the rest and points `app` at it.
- **On a Mac, the guest's Claude Code is carried forward, not refreshed.** The update copies the
  running install's Linux build instead of fetching the one the release pins, so it can lag until the
  next run of the release installer.
- **It watches one repository's latest release.** `COLONIZER_UPDATE_REPO` names another one; there is
  no choosing a version, no channel and no staging rings.
