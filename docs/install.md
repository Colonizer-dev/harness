# Install

Colonizer runs on your own machine, and today you build it from this repository. There are no prebuilt
releases yet.

## What you need

- **A machine that can run microVMs**: Linux x86_64 with `/dev/kvm` readable and writable by your user,
  or an Apple Silicon Mac. An Intel Mac can't run Colonizer, because microsandbox's libkrun backend is
  aarch64-only.
- **Tools**: `git`, `gh`, `curl`, `tar`, Node.js 20 or newer, and a Rust toolchain of 1.88 or newer.
  Homebrew's `rust` can lag a long way behind, so `rustup` is the safe bet.
- **Claude Code**: on Linux, a native Claude Code install, which colonies use. On a Mac the installer
  fetches the Linux build a colony needs ([On a Mac](#on-a-mac)).

[microsandbox](https://docs.microsandbox.dev), Headscale and Tailscale ship with the app, pinned and
checked by sha256, so there is nothing else to install.

## Build and run

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

- `--pull-image` downloads the default colony image, `node:24-bookworm`, at install time. The first
  colony then boots straight away instead of waiting on a download of several gigabytes.
- `--install` copies the app to `~/.local/share/colonizer/app` and links `~/.local/bin/colonizer`, so
  `colonizer` runs from anywhere. It is implemented but hasn't been run end to end yet.

## First run

In **Settings**, connect GitHub (your `gh` login is picked up automatically) and press **Log in with
Claude subscription**. Then **Launch** a colony on an issue, or on a repository with nothing but a
sentence of instructions. Launch as many as you like: past the parallel limit (Settings → Modules →
sandbox), a colony is queued and starts on its own when one ahead of it finishes.

## On a Mac

`scripts/install.sh` also fetches the `linux-arm64` build of Claude Code. It follows the `stable`
channel and is checked against Anthropic's own manifest. This is needed because a colony is a Linux
microVM and the Mac's own binary is Mach-O. `colonizer-agentd` is built for the guest's architecture.

A colony has been taken end to end on Apple Silicon, from install to an open pull request
([#36](https://github.com/Colonizer-dev/harness/issues/36)). The one gap is the bundled private mesh.
Tailscale publishes no macOS `tailscaled` to vendor, so on a Mac colonies are reached on a loopback
port instead ([#32](https://github.com/Colonizer-dev/harness/issues/32)).

## Where things live

| What | Where | Change it with |
| :--- | :--- | :--- |
| The app | `./dist`, or `~/.local/share/colonizer/app` after `--install` | |
| Settings, org settings, providers, and the GitHub, Claude and provider credentials | `~/.config/colonizer` | `COLONIZER_CONFIG_DIR` |
| Colonies and their worktrees, shared memory, your own plugins, the Headroom bundle | `~/.local/share/colonizer` | `COLONIZER_DATA_DIR` |
| The web UI | `127.0.0.1:7878` | `COLONIZER_BIND` |

Every setting is listed under [Configuration](https://github.com/Colonizer-dev/harness#configuration) in the README.

## Updating

```sh
git pull
scripts/install.sh
```

Then restart `colonizer`. Settings and colonies live outside the checkout, so a rebuild leaves them
alone, and the colony list is read back when the harness starts.
