# Install

Colonizer runs on your own machine. Install a prebuilt release with one command, or build it from this
repository.

## What you need

- **A machine that can run microVMs**: Linux x86_64 with `/dev/kvm` readable and writable by your user,
  or an Apple Silicon Mac. An Intel Mac can't run Colonizer, because microsandbox's libkrun backend is
  aarch64-only. On Linux the host also needs glibc 2.28 or newer, which the pinned microsandbox
  binary requires.
- **Tools**: `git` and `gh`, which colonies use, and `curl` and `tar`. The installer also uses `gh`,
  when it is present, to verify a release's build provenance ([Install a release](#install-a-release)).
  A build from source also needs
  Node.js 20 or newer and a Rust toolchain of 1.88 or newer. Homebrew's `rust` can lag a long way
  behind, so `rustup` is the safe bet.
- **Claude Code**: on Linux, a native Claude Code install, which colonies use. On a Mac the installer
  fetches the Linux build a colony needs ([On a Mac](#on-a-mac)).

[microsandbox](https://docs.microsandbox.dev), Headscale and Tailscale ship with the app, pinned and
checked by sha256, so there is nothing else to install.

## Install a release

```sh
curl -fsSL https://colonizer.dev/install.sh | sh
```

Then run `colonizer` and open <http://127.0.0.1:7878>.

The installer picks the app for your machine from the latest
[release](https://github.com/Colonizer-dev/harness/releases) and checks it against the release's
`SHA256SUMS`. It installs the app to `~/.local/share/colonizer/app`, a symlink to the directory the
installed version lives in, and links `~/.local/bin/colonizer`; an upgrade switches the symlink with
one rename, so an upgrade cut short leaves either the whole old app or the whole new one. The
exception is the one-time move up from a pre-symlink install: for a moment the old app is parked at
`app.old`, and a hard kill in that window leaves colonizer down until the next install puts it back.
The script is `scripts/install-release.sh`, published with each release as `install.sh`.

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

A release contains no Anthropic code, which isn't ours to redistribute. So the installer fetches two
things from Anthropic's own channels and checks each one:

- the Claude Agent SDK, from the npm registry, against the checksum the release recorded from
  `package-lock.json`;
- on a Mac, the Linux build of Claude Code that colonies run, the build pinned by version and checksum
  in the release's `claude-code.lock`, not whatever Anthropic's `stable` channel points at that day
  ([On a Mac](#on-a-mac)).

Run the same command again to update. Two variations:

```sh
# install a particular release instead of the latest
curl -fsSL https://colonizer.dev/install.sh | COLONIZER_VERSION=v0.1.0 sh

# also download the default colony image now, so the first colony boots straight away
curl -fsSL https://colonizer.dev/install.sh | sh -s -- --pull-image
```

Two more variables change where the app comes from and how strictly it is checked:
`COLONIZER_RELEASE_URL` fetches the app from `<url>/<file>` instead of the GitHub release. The build
attestation belongs to the official release, so it is skipped on that path, and
`COLONIZER_REQUIRE_ATTESTATION=1` makes a provenance check that comes back without a verdict a failure
instead of a note.

## Build from source

```sh
git clone https://github.com/Colonizer-dev/harness
cd harness
scripts/install.sh
dist/bin/colonizer
```

Then open <http://127.0.0.1:7878>.

`scripts/install.sh` builds the whole app into `./dist`: the vendored binaries, `colonizer-agentd` and
`rtk` (each a static musl build inside a microVM), the agent modules, the web UI and the harness.
Two things are downloaded later, while Colonizer runs: the colony image, which microsandbox pulls the
first time a colony needs it, and the Headroom bundle, if you switch Headroom on.

Two options:

- `--pull-image` downloads the default colony image, `node:24-bookworm` pinned by digest
  (`crates/colonizer/images.lock`), at install time. The first
  colony then boots straight away instead of waiting on a download of several gigabytes.
- `--install` copies the app to `~/.local/share/colonizer/app` and links `~/.local/bin/colonizer`, so
  `colonizer` runs from anywhere. It is implemented but hasn't been run end to end yet.

The crates are on crates.io (`colonizer-harness`, `colonizer-agentd`), but `cargo install` builds only
the `colonizer` binary, without the app assets it needs beside it. The installer or a build from
source is the way in.

## First run

In **Settings**, connect GitHub (your `gh` login is picked up automatically) and press **Log in with
Claude subscription**. Then **Launch** a colony on an issue, or on a repository with nothing but a
sentence of instructions. Launch as many as you like: past the parallel limit (Settings → Modules →
sandbox), a colony is queued and starts on its own when one ahead of it finishes.

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

## Where things live

| What | Where | Change it with |
| :--- | :--- | :--- |
| The app | `~/.local/share/colonizer/app` for a release, a symlink to the directory the installed version lives in, so an upgrade is one rename; `./dist` for a build from source, or that same place after `--install` | `COLONIZER_APP` for a release |
| Settings, org settings, providers, and the GitHub, Claude and provider credentials | `~/.config/colonizer` | `COLONIZER_CONFIG_DIR` |
| Colonies and their worktrees, shared memory, your own plugins, the Headroom bundle | `~/.local/share/colonizer` | `COLONIZER_DATA_DIR` |
| The web UI | `127.0.0.1:7878` | `COLONIZER_BIND` |

Every setting is listed under [Configuration](https://github.com/Colonizer-dev/harness#configuration) in the README.

## Updating

A running mothership can update itself: Settings offers the newer release, installs it and restarts into
it without losing colonies, and `colonizer update` does the same from a terminal. A development build is
refused: update it from its checkout with `git pull && scripts/install.sh --install`. That, and the
version check behind it, is [docs/updates.md](updates.md).

By hand: for a release, run the install command again. For a build from source:

```sh
git pull
scripts/install.sh
```

Either way, restart `colonizer` afterwards. Settings and colonies live outside the checkout, so a rebuild leaves them
alone, and the colony list is read back when the harness starts.
