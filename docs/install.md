# Install

Colonizer runs on your own machine. Install a prebuilt release with one command, or build it from this
repository. This page is also the settings reference: everything the mothership reads from its
environment is under [Settings](#settings).

## What you need

- **A machine that can run microVMs**: Linux x86_64 with `/dev/kvm` readable and writable by your user,
  or an Apple Silicon Mac. On a stock Ubuntu, `/dev/kvm` is `root:kvm 0660`, so add yourself to the
  `kvm` group and log back in: `sudo usermod -aG kvm "$USER"`. An Intel Mac can't run Colonizer, because
  microsandbox's libkrun backend is aarch64-only. On Linux the host also needs glibc 2.28 or newer,
  which the pinned microsandbox binary requires.
- **Tools**: `git` and `gh`, which colonies use, and `curl` and `tar` (with xz support, for the
  Node.js runtime the installer unpacks). The installer also uses `gh`, when it is present, to verify
  a release's build provenance ([Install a release](#install-a-release)). A build from source also
  needs `npm`, Node.js 20.19 or newer (or 22.12 or newer; the web UI's Vite requires one of those),
  and a Rust toolchain of 1.98 or newer. Homebrew's `rust` can lag a long way behind, so `rustup` is
  the safe bet.
- **Claude Code**: on Linux, a native Claude Code install, which colonies use. On a Mac the installer
  fetches the Linux build a colony needs ([On a Mac](#on-a-mac)).

[microsandbox](https://docs.microsandbox.dev), Headscale and Tailscale ship with the app, pinned and
checked by sha256, so there is nothing else to install.

## Install a release

```sh
curl -fsSL https://colonizer.dev/install.sh | sh
```

Then run `colonizer`. It prints a sign-in link and opens it in your browser; `colonizer open` prints it
again. Opening <http://127.0.0.1:7878> without that link asks you to sign in.

The installer picks the app for your machine from the latest
[release](https://github.com/Colonizer-dev/harness/releases) and checks it against the release's
`SHA256SUMS`. It installs the app to `~/.local/share/colonizer/app`, a symlink to the directory the
installed version lives in (`app-a` or `app-b` beside it), and links `~/.local/bin/colonizer`; an
upgrade unpacks into the other slot and switches the symlink with one rename, so an upgrade cut short
leaves either the whole old app or the whole new one. The exception is the one-time move up from a
pre-symlink install: for a moment the old app is parked at `app.old`, and a hard kill in that window
leaves colonizer down until the next install puts it back. The script is `scripts/install-release.sh`,
published with each release as `install.sh`.

The checksums catch a corrupted download, not a rewritten release: whoever can replace the archive can
replace its checksums too. So the installer also verifies `SHA256SUMS` itself against the release's
build-provenance attestation: the release workflow signs the checksum file with Sigstore and logs the
signature in a public transparency log, which access to the release's assets alone cannot produce. That
second check needs `gh`, and when it cannot reach a verdict it is skipped with a note rather than
failing: no `gh` installed, a `gh` too old to have `gh attestation verify`, `COLONIZER_RELEASE_URL`
pointing somewhere other than the official release, or a release published before the workflow began
signing, which carries no attestation at all. A check that runs and fails stops the install.
`COLONIZER_REQUIRE_ATTESTATION=1` turns the skips into failures too.

To verify an artifact yourself:

```sh
gh attestation verify colonizer-linux-x86_64.tar.gz --repo Colonizer-dev/harness
```

`darwin-arm64` is the other platform. Releases are created as a draft and published only once every
asset is attached, the only order that works under GitHub's immutable releases, which freeze the assets
and the tag the moment a release is published. Turning immutability on is itself a repository setting
(Settings → Releases), not something a workflow can do.

A release contains no Anthropic code, which isn't ours to redistribute, and no guest runtime. So the
installer fetches these from their own channels and checks each one before it is used:

- the Claude Agent SDK, from the npm registry, against the checksum the release recorded from
  `package-lock.json`;
- on every host, the Linux Node.js runtime colonies run, from nodejs.org, against the version and
  checksum pinned in the release's `node.lock`;
- on a Mac, the Linux build of Claude Code that colonies run, the build pinned by version and checksum
  in the release's `claude-code.lock`, not whatever Anthropic's `stable` channel points at that day
  ([On a Mac](#on-a-mac)). A Linux host uses its own Claude Code install instead.

Run the same command again to update. Two variations:

```sh
# install a particular release instead of the latest
curl -fsSL https://colonizer.dev/install.sh | COLONIZER_VERSION=v0.1.0 sh

# also download the default colony image now, so the first colony boots straight away
curl -fsSL https://colonizer.dev/install.sh | sh -s -- --pull-image
```

The installer's other variables are listed under [Installer and build](#installer-and-build).

## Build from source

```sh
git clone https://github.com/Colonizer-dev/harness
cd harness
scripts/install.sh
dist/bin/colonizer
```

`dist/bin/colonizer` prints a sign-in link and opens it; `colonizer open` prints it again.

`scripts/install.sh` builds the whole app into `./dist`: the vendored binaries, `colonizer-agentd` and
`rtk` (each a static musl build inside a microVM), the Linux Node.js runtime colonies run, the agent
modules, the web UI and the harness. Two things are downloaded later, while Colonizer runs: the colony
image, which microsandbox pulls the first time a colony needs it, and the Headroom bundle, if you switch
Headroom on.

Two options:

- `--pull-image` downloads the default colony image, `node:24-bookworm` pinned by digest
  (`crates/colonizer/images.lock`), at install time. The first
  colony then boots straight away instead of waiting on a download of several gigabytes.
- `--install` copies the app to `~/.local/share/colonizer/app` (the same `app-a`/`app-b` symlink layout
  a release uses) and links `~/.local/bin/colonizer`, so `colonizer` runs from anywhere. Its swap logic
  is covered by `scripts/test/install.test.sh`; a full build followed by `--install` is not exercised
  in CI.

The image is the stock `node:24-bookworm`. A colony-node image — that base plus bun and pnpm, each
pinned and checksum-verified at build time — is built and scanned by
`.github/workflows/colony-image.yml`; the node preset keeps using the stock image until the built one
is published to `ghcr.io` and its digest is pinned in `crates/colonizer/images.lock`.

A third option, `--bundle`, is what the release workflow uses to build a release tarball; you don't
need it.

The crates are on crates.io (`colonizer-harness`, `colonizer-agentd`), but `cargo install` builds only
the `colonizer` binary, without the app assets it needs beside it. The installer or a build from
source is the way in.

## First run

In **Settings**, connect GitHub (your `gh` login is picked up automatically) and press **Log in with
Claude subscription**. Then **Launch** a colony on an issue, or on a repository with nothing but a
sentence of instructions. Launch as many as you like: past the parallel limit (Settings → Modules →
sandbox), a colony is queued and starts on its own when one ahead of it finishes.

## Commands you run on this machine

Most `colonizer` subcommands are clients of a running mothership ([docs/cli.md](cli.md)). A few act on
this machine and read the same environment the mothership does:

| Command | What it does |
| :--- | :--- |
| `colonizer` | Starts the mothership on `COLONIZER_BIND` |
| `colonizer open` | Prints the cockpit sign-in link and opens it in a browser. It reads the token file `<config dir>/api-token`, creating it if there is none, so it works whether or not the mothership is running |
| `colonizer version` | Prints the build: tag, commit, build time, and whether it is a development build ([docs/updates.md](updates.md)) |
| `colonizer update [--force]` | Asks the running mothership on `COLONIZER_BIND` to install the newest release and restart into it ([docs/updates.md](updates.md#updating-in-place)) |
| `colonizer login-item enable\|disable\|status` | Starts the mothership at login ([below](#desktop-install-the-cockpit-as-an-app-start-at-login)) |
| `colonizer telemetry show\|on\|off` | Shows or switches [usage data](usage-data.md); no network and no running mothership needed |
| `colonizer migrate-store --to DIR [--from DIR] [--dry-run]` | Copies this install's colonies into another local session store ([docs/session-store.md](session-store.md#migration-and-rollback)); `--from` defaults to `COLONIZER_DATA_DIR` |
| `colonizer fleet export [--out FILE] [--preview]`, `colonizer fleet import FILE [--preview]` | Writes this machine's colony history, logs and stats into a bundle, or reads another machine's into `fleet-imports/` ([docs/cli.md](cli.md#fleet-export-and-import)); no mothership or token needed |
| `colonizer completions <shell>` | Prints a completion script for `bash`, `zsh`, `fish`, `powershell` or `elvish` |
| `colonizer man` | Prints the man page to stdout |

```sh
echo 'source <(colonizer completions bash)' >> ~/.bashrc
colonizer man > colonizer.1 && man ./colonizer.1
```

More shells are in [docs/cli.md](cli.md#completions-and-the-man-page).

## On a Mac

The installer, for a release or a build from source, also fetches the `linux-arm64` build of Claude Code. The
release decides which build: the version and its sha256 come from `claude-code.lock`, so every install of
the same Colonizer release gets the same agent, checked before it is used. `scripts/update-runtime-pins.mjs`
watches Anthropic's `stable` channel daily and proposes a newer version by pull request, and the pin only
moves when a person merges that. This is needed because a colony is a Linux
microVM and the Mac's own binary is Mach-O. `colonizer-agentd` is built for the guest's architecture.

A colony has been taken end to end on Apple Silicon, from install to an open pull request
([#36](https://github.com/Colonizer-dev/harness/issues/36)).

The bundled private mesh comes with a Mac install too. That path is new: the build has been exercised
from Linux, but it has not yet been taken end to end on Apple hardware. Headscale ships a
`darwin-arm64` binary, just as it does for Linux. Tailscale publishes no macOS `tailscaled` anywhere
(its macOS release is a GUI app plus a system extension, with no command-line pair to extract), so
a build from source compiles
`tailscale` and `tailscaled` itself. The source is the same v1.102.4 tag the Linux binaries come
from, pinned and sha256-verified, and the build runs inside a `golang:1-alpine` microVM, the way
`rtk` is already built. That build takes a minute or so and needs a few GB of Go build and module
cache under `target/`, on the first install only; a re-run finds the stamp and does nothing. A
release install instead gets the binaries in the tarball and builds nothing. The host's `tailscaled`
runs with userspace networking and a SOCKS5 listener, so a Mac needs no TUN device, no root and no
special entitlements: Go's linker signs the binaries ad hoc, which is all Apple Silicon requires.
If the three mesh binaries are absent, colonies fall back to a loopback port, as before.

## Desktop: install the cockpit as an app, start at login

**The cockpit as an app.** The cockpit is an installable web app: its own window, a Dock or
taskbar icon, the same sign-in.

- **Chrome or Edge:** use the install icon in the address bar, or **Settings → Desktop → Install
  app**. On Android, Chrome offers the same install from the address-bar banner or the menu's
  **Add to Home screen**.
- **Safari:** **File → Add to Dock**.
- **iPhone or iPad:** open the cockpit in Safari and pick **Add to Home Screen** from the share
  sheet. When the device isn't installed yet, **Settings → Notifications** and **Settings →
  Desktop** show those steps in the app itself, next to the web push they unlock.

The installed app carries a few shortcuts — **Inbox**, **Colonize**, **Nest** — from the icon's
long-press menu (right-click on the taskbar/Dock icon). Sharing a GitHub issue or pull request link
to Colonizer (Android's share sheet) opens the colony holding it, or Colonize with that issue
prefilled; a pull request only ever matches a colony that recorded it as its own pull request.

A small service worker makes that work. It caches each build's hashed `/assets` files into a cache
named for that build — the previous build's stays one build longer, so a tab that hasn't reloaded
yet keeps working after an update — and caches the GitHub avatars the mothership proxies, answers
a short list of read-only views (repository details and package listings) from their last answer
while it fetches a fresh one, shows the mothership's
[Web Push](https://developer.mozilla.org/docs/Web/API/Push_API) notifications, and shows an offline
page when the mothership isn't running. It never caches pages, writes, the sign-in link or any other
`/api` call. When a new build has installed and is waiting, a **Colonizer updated** card offers
**Reload**; the running build keeps working until you do. The manifest, service worker, offline page
and icons load before sign-in; they contain nothing private.

**Start at login.**

```sh
colonizer login-item enable    # start the mothership when you log in
colonizer login-item status    # installed? enabled? which pid?
colonizer login-item disable   # stop doing that (the running mothership keeps running)
```

The same switch is at **Settings → Desktop → Start Colonizer at login**.

- **macOS:** a LaunchAgent at `~/Library/LaunchAgents/dev.colonizer.mothership.plist`.
- **Linux:** a systemd user unit at `~/.config/systemd/user/colonizer.service` (under
  `$XDG_CONFIG_HOME` when that is set). On a headless host, run `loginctl enable-linger` so it starts
  at boot, not only when you log in; `status` says so when linger is off.

Either way, it:

- runs `~/.local/bin/colonizer` (or, when that link does not exist, the binary you ran `enable` with)
  with `COLONIZER_NO_BROWSER=1`
- restarts it only after a crash
- appends to `mothership.out` in the data directory
- carries over this shell's `PATH` and `COLONIZER_*` settings, never anything whose name contains
  `KEY`, `TOKEN`, `SECRET`, `PASSWORD` or `PASS`, because the plist and unit are plain files

That last rule also leaves out `COLONIZER_MASTER_KEY`, `GH_TOKEN` and the other credential variables
below. A mothership started at login does not see them: keep those secrets in Settings (the keychain,
where one answers) rather than in your shell, or secrets encrypted with the master key will read as
missing.

Disabling removes the agent and never stops a running mothership, so it never interrupts a
colony.

**One mothership at a time.** A mothership binds its port before anything else. A second one,
say one started at login while another already runs by hand, says the port is taken and stops
without touching any colony. Started by the login agent, it exits cleanly, so the agent doesn't
retry it.

## Where things live

| What | Where | Change it with |
| :--- | :--- | :--- |
| The app | `~/.local/share/colonizer/app` for a release, a symlink to the slot the installed version lives in, so an upgrade is one rename; `./dist` for a build from source, or that same place after `--install` | `COLONIZER_APP` for a release, `COLONIZER_HOME` to point a binary at another app directory |
| Settings and credentials | `~/.config/colonizer` | `COLONIZER_CONFIG_DIR` |
| Colonies, clones, worktrees and everything they produce | `~/.local/share/colonizer` | `COLONIZER_DATA_DIR` |
| Unix sockets | `$XDG_RUNTIME_DIR/colonizer`, else `/tmp/colonizer-<uid>` | `XDG_RUNTIME_DIR` |
| The web UI and API | `127.0.0.1:7878` | `COLONIZER_BIND` |

What the mothership keeps in the config directory:

| File | What it holds |
| :--- | :--- |
| `modules.json` | Module settings, edited in Settings → Modules |
| `orgs.json`, `known-orgs.json` | Per-org overrides, and the orgs the cockpit has seen |
| `providers.json`, `provider-keys/<id>` | Model provider connections ([docs/providers.md](providers.md)) and their keys |
| `github-token`, `claude-accounts.json`, `claude-accounts/` | Saved GitHub and Claude credentials (a pre-accounts `claude-token` is migrated into `claude-accounts/default`) |
| `colony-secrets.json`, `colony-secrets/` | Secrets you hand to colonies |
| `voice-keys/`, `memory-keys/`, `notify-secret`, `push-vapid-key`, `push-subscriptions.json` | Speech-to-text keys, the mem0 key, the webhook signing secret, and Web Push |
| `secrets.json` | Where each saved secret lives (file or system keychain), for the Secrets page |
| `api-token`, `api-tokens.json` | The owner token behind the sign-in link, and scoped API tokens ([docs/cli.md](cli.md#scoped-api-tokens)) |
| `telemetry.json`, `usage.json`, `usage-last.json`, `usage-sent.json` | The [live map](telemetry.md) and [usage data](usage-data.md) answers, the last usage batch built, and when the last one was sent |
| `updates.json` | The update check switch ([docs/updates.md](updates.md)) |
| `loops.json`, `redteam-schedules.json` | Scheduled loops and red-team runs |
| `colonizer.toml` | Optional hand-written file; today it holds `[publish] co_author` |
| `fleet.json` | [Fleet](fleet.md) membership: pairings, members and this machine's own fleet token |
| `phones.json` | Paired phones: each phone's credential, stored as a SHA-256 |
| `remote/` | The remote-access identity key pair |
| `host_id` | This mothership's id |

Secret files are written with mode 0600, or encrypted with `COLONIZER_MASTER_KEY`, or moved to the
system keychain; the Secrets page shows which.

What it keeps in the data directory: `sessions.json` (the colony list, read back at every start) and
`sessions/<id>/` (each colony's logs and state), `repos/` and `worktrees/` (clones and each colony's
worktree), `mesh/`, `plugins/` (your own plugins), `memory/`, `chats/`, `drafts/`, `maps/`,
`archive/`, `cache/`, `deja/` (per-org transcript indexes), `fleet-imports/` (bundles read with
`colonizer fleet import`), `headroom/` and `hunters/` (downloaded on demand), the ledgers (`spend.jsonl`,
`routing.jsonl`, `activity.jsonl`, `ledger.json`, `provider-usage.json`, `provider-quota.json`), and
`mothership.out` when the mothership is started at login.

## Settings

Settings come from the environment, not flags. Module settings are edited in the cockpit and kept in
`modules.json`; the variables here are the ones a person sets. The mothership reads them when it
starts, so restart it after changing one. The local commands (`update`, `open`, `login-item`,
`telemetry`, `migrate-store`, `fleet export`, `fleet import`) read the same variables.

### The mothership

| Variable | Default | Meaning |
| :--- | :--- | :--- |
| `COLONIZER_BIND` | `127.0.0.1:7878` | Where the web UI and API listen. Keep it on loopback or a private interface, never `0.0.0.0` |
| `COLONIZER_ALLOWED_HOSTS` | – | Extra `Host` names to accept, comma separated, for example a name on your tailnet |
| `COLONIZER_DATA_DIR` | `~/.local/share/colonizer` | Colonies, clones, worktrees, mesh state. Must not contain `:` or `,` (it goes into microVM mount specs); startup refuses one that does |
| `COLONIZER_CONFIG_DIR` | `~/.config/colonizer` | Settings and saved credentials |
| `COLONIZER_HOME` | next to the binary, or `dist/` in a checkout | The app assets (`bin/`, `vendor/`, `modules/`, `web/`) |
| `COLONIZER_APP` | `~/.local/share/colonizer/app` | The symlink the installer moves and an [update](updates.md) follows |
| `COLONIZER_MSB` | the vendored `msb`, then `~/.local/bin/msb`, then `msb` on `PATH` | The microsandbox binary |
| `COLONIZER_CLAUDE_BIN` | `claude` on `PATH`, then `~/.local/share/mise/installs/claude/latest/claude`, `~/.local/bin/claude`, `~/.claude/local/claude` | The native Claude Code binary to mount into colonies |
| `COLONIZER_GATEWAY_BIND` | `127.0.0.1:41750` | The provider gateway; colonies reach it through `host.microsandbox.internal`. Must be an IP and port: a hostname such as `localhost:41750` refuses startup |
| `COLONIZER_FLEET_PEERS` | – | Base URLs of other motherships, comma separated, polled for the fleet view (`GET /api/hosts`). Nothing is exposed by setting it |
| `COLONIZER_FLEET_SYNC` | on | Set to `off` (or `0`, `false`, `no`) to stop a fleet member's background history push ([fleet.md](fleet.md#history-push)); `colonizer fleet sync` still drains on demand. Has no effect on a machine that has not joined a fleet |
| `COLONIZER_BENCH_POOL` | – | A bench pool directory ([docs/bench.md](bench.md#the-raid-set)): red-team runs read its `raid.json` and deal the injected bugs recorded for the raided repository out to the hunters' briefs |
| `COLONIZER_NO_BROWSER` | – | Set to anything, even empty, to skip opening the sign-in link in a browser |
| `COLONIZER_MASTER_KEY` | – (secrets saved in plaintext, 0600) | Encrypts the secrets the mothership saves, at rest ([below](#colonizer_master_key)) |
| `COLONIZER_NO_EXTERNAL_EFFECTS`, `COLONIZER_NO_WRITE` | – | A kill switch: set either to anything but `0`, `false`, `off` or `no`, and every write that leaves the harness (commits, pushes, pull requests, merges, comments, filed issues) refuses to run |
| `COLONIZER_SUMMARIES` | on | `0`, `false` or `off` turns colony summaries off whatever the agent module's `summaries` setting says |
| `COLONIZER_BOOT_RETRY_BUDGET_SECS` | `1200` | How long, in seconds, a colony's boot keeps retrying transient failures (network, GitHub 5xx) before the clone and worktree are in place |
| `COLONIZER_QUOTA_FALLBACK` | on | `0` or `false` stops every provider from failing over to its `fallback_model` when its plan runs out ([docs/providers.md](providers.md#plans-quotas-and-trust)) |
| `COLONIZER_RECLAIM` | on | `0`, `false`, `off` or `no` switches off the 5-minute sweep that reclaims finished colonies' worktrees once their work is pushed. Manual cleanup still works |
| `COLONIZER_RECLAIM_RETENTION_HOURS` | `12` | How long a finished colony's worktree is kept before the sweep may reclaim it |
| `COLONIZER_FLEET_INGEST_RETENTION_DAYS` | `90` | On a fleet owner, days a member's synced colony and its logs are kept after they arrive; `0` keeps them ([fleet.md](fleet.md#reading-it-on-the-owner)) |
| `COLONIZER_RECLAIM_MIN_FREE` | `5G` | The free-disk floor, used only when the sandbox module's `min_free_disk` setting has not been saved. Below it the queue pauses and the sweep reclaims pushed work without waiting |
| `MSB_HOME` | `~/.microsandbox` | Where microsandbox keeps its state and image cache, for the disk figures |
| `COLONIZER_HUNTER_INSTALL` | off | `1`, `true`, `on` or `yes` allows installing security hunters ([docs/security-hunters.md](security-hunters.md)) |
| `COLONIZER_UPDATE_CHECK` | on | `0`, `false` or `off` keeps the update check off whatever Settings says, and then no request is made at all |
| `COLONIZER_RELEASES_URL` | `https://api.github.com/repos/Colonizer-dev/harness/releases/latest` | Where the update check looks |
| `DO_NOT_TRACK` | – | Anything but empty, `0` or `false` keeps the [live map](telemetry.md) and [usage data](usage-data.md) off whatever Settings says |
| `COLONIZER_TELEMETRY` | – | `off`, `0`, `false` or `no` does the same |
| `CI` | – | Exactly `true` keeps [usage data](usage-data.md) off; the live map does not read it |
| `COLONIZER_TELEMETRY_URL` | `https://telemetry.colonizer.dev` | Where live map heartbeats go |
| `COLONIZER_TELEMETRY_ENDPOINT` | – (nothing is sent) | The collector URL usage data is posted to, at most once a day. No default: unset, no [usage data](usage-data.md) is ever sent, whatever the switch says |
| `COLONIZER_REMOTE_URL` | `wss://my.colonizer.dev` | The relay remote access dials when it is switched on ([docs/remote-tunnel.md](remote-tunnel.md)) |
| `COLONIZER_VAPID_SUBJECT` | `https://github.com/Colonizer-dev/harness` | The contact the mothership names to push services when it sends Web Push notifications |

### Upgrading across the microsandbox 0.7 pin

Since [#639](https://github.com/Colonizer-dev/harness/issues/639) the vendored `msb` is 0.7.x.
Microsandbox's home (`MSB_HOME`, default `~/.microsandbox`) is version-locked: the first 0.7
command migrates a 0.6-era home in place, and the migrated home still lists and removes microVMs a
0.6 `msb` created. The migration is one-way: after it, a 0.6 `msb` fails every command against the
home (`database schema is newer than this msb binary`). If you have to roll the harness itself back
to a 0.6-era release, downgrade the home first, with the 0.7 binary: `msb self downgrade 0.6.18
--yes`. It refuses while sandboxes are active (stop them first), backs the database up
(`db/msb.db.bak-…`), purges the image cache (re-downloaded on the next boot) and installs the
0.6.18 binaries, after which the old `msb` works again.

### Credentials read from the environment

Each of these is used only when nothing is saved for it in Settings. A saved value wins.

| Variable | Used for |
| :--- | :--- |
| `GH_TOKEN`, `GITHUB_TOKEN` | GitHub. With neither, and nothing saved, the `gh` CLI login is used |
| `CLAUDE_CODE_OAUTH_TOKEN`, `ANTHROPIC_API_KEY` | Claude, in that order, when no Claude account is saved |
| `MEM0_API_KEY` | The mem0 shared-memory provider |
| `JEV_API_KEY` | Jev compaction and the Jev second opinion (TypeSafe). Never saved; only the environment |
| `OPENAI_API_KEY`, `GROQ_API_KEY`, `DEEPGRAM_API_KEY`, `ELEVENLABS_API_KEY`, `COLONIZER_VOICE_API_KEY` | Speech to text in the composer, one per voice service (the last is the OpenAI-compatible one) |
| `OPENAI_API_KEY`, `XAI_API_KEY` | Also the vendor key the `codex` and `grok-build` agent modules get, when no `openai` or `xai-grok` provider has a saved key ([docs/runner-authoring.md](runner-authoring.md)) |
| `COLONIZER_NOTIFY_SECRET` | Signing outgoing notification webhooks |

A login item does not carry any of these ([above](#desktop-install-the-cockpit-as-an-app-start-at-login)).

### Client commands

`COLONIZER_TOKEN` is the API token the client commands (`list`, `launch`, `mcp` and the rest) send.
The order is `COLONIZER_TOKEN`, then `--token-file`, then the local `<config dir>/api-token`. Which
mothership they talk to is `--host`, else `COLONIZER_BIND`. See [docs/cli.md](cli.md).

### Installer and build

| Variable | Read by | Meaning |
| :--- | :--- | :--- |
| `COLONIZER_VERSION` | `install.sh` | Install that release tag instead of the latest |
| `COLONIZER_APP` | `install.sh` | Install the app symlink there instead of `~/.local/share/colonizer/app` |
| `COLONIZER_RELEASE_URL` | `install.sh` | Fetch the app from `<url>/<file>` instead of the GitHub release. The build attestation belongs to the official release, so it is skipped on that path |
| `COLONIZER_REQUIRE_ATTESTATION` | `install.sh` | `1` makes a provenance check that comes back without a verdict a failure instead of a note |
| `COLONIZER_KEEP_PREVIOUS` | `install.sh` | `1` keeps the slot being replaced; an in-place update sets it ([docs/updates.md](updates.md#the-previous-version-is-kept-for-a-while)) |
| `COLONIZER_IMAGE` | both scripts, with `--pull-image` | The image to pull instead of the pinned `node:24-bookworm` |
| `COLONIZER_MSB` | `scripts/install.sh` | A microsandbox binary to build with instead of the vendored one |
| `COLONIZER_CODESIGN_IDENTITY` | `scripts/install.sh` | On macOS, sign the binary with this identity so the Keychain keeps granting access across rebuilds |
| `COLONIZER_PREBUILT` | `scripts/install.sh` | A directory of prebuilt binaries to use instead of building them; the release workflow sets it |
| `COLONIZER_DESCRIBE`, `COLONIZER_COMMIT` | the Rust build | The version and commit to stamp into the binary when git is not available; the release workflow sets them ([docs/updates.md](updates.md#which-version-am-i-running)) |

### Not settings

Other `COLONIZER_*` names in the code are set by the mothership inside a colony, not read from your
shell: the model variables (`COLONIZER_MODEL`, `COLONIZER_SUBAGENT_MODEL`, `COLONIZER_MODEL_ROUTES` and
the rest), `COLONIZER_DISABLED_TOOLS`, `COLONIZER_RTK`, `COLONIZER_HEADROOM`, `COLONIZER_CAVEMAN`,
`COLONIZER_JEV_COMPACTION`, `COLONIZER_LOOP`, `COLONIZER_MESH_*`, `COLONIZER_MEMORY_DIR` and similar.
Each module setting that has one says which in its schema; change the setting in Settings → Modules,
not the variable. `COLONIZER_LOGIN_ITEM` is a marker the login item sets.

### `COLONIZER_MASTER_KEY`

`COLONIZER_MASTER_KEY` encrypts the secrets saved under the config directory (the GitHub and Claude
tokens, the model provider keys, the mem0 key and the webhook signing secret) with ChaCha20-Poly1305,
keyed by the SHA-256 of its value, and writes each one as a `.enc` file beside where the plaintext would
be, removing the plaintext. Unset or blank, secrets are written in plaintext (0600). It protects a
copied, synced or backed-up config directory, not a machine where something runs as you, since that can
read the variable too. Use a long random value (32 or more random bytes): the single SHA-256 does no key
stretching. Without the key, or with the wrong one, an encrypted secret counts as missing. [configuration.md](configuration.md#colonizer_master_key) has the rest: rotation,
the system keychain, and the per-colony budgets.

## Updating

A running mothership can update itself: Settings offers the newer release, installs it and restarts into
it without losing colonies, and `colonizer update` does the same from a terminal. That, and the
version check behind it, is [docs/updates.md](updates.md).

Two builds are refused: a development build, which holds work no release contains (update it from its
checkout with `git pull && scripts/install.sh --install`), and a release newer than the latest one, which
would be a downgrade. Either refusal takes `colonizer update --force`; what that risks is in
[docs/updates.md](updates.md#updating-in-place).

By hand: for a release, run the install command again. For a build from source:

```sh
git pull
scripts/install.sh --install
```

Either way, restart `colonizer` afterwards. Settings and colonies live outside the checkout, so a rebuild leaves them
alone, and the colony list is read back when the harness starts. If colonies are running, prefer
`colonizer update`: an installer run by hand removes the previous app slot straight away, and running
colonies mount plugins from it ([docs/updates.md](updates.md#the-previous-version-is-kept-for-a-while)).
