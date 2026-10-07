<p align="center">
  <a href="https://github.com/Colonizer-dev/harness">
    <img src="https://raw.githubusercontent.com/Colonizer-dev/harness/main/assets/banners/colonizer-harness.png" alt="colonizer-harness — The mothership." width="100%">
  </a>
</p>

<p align="center">
  <a href="https://crates.io/crates/colonizer-harness"><img src="https://img.shields.io/crates/v/colonizer-harness.svg?style=flat-square&labelColor=0A0A0B&color=FF6B35" alt="colonizer-harness on crates.io"></a>
  <a href="https://github.com/Colonizer-dev/harness/releases/latest"><img src="https://img.shields.io/github/v/release/Colonizer-dev/harness?style=flat-square&labelColor=0A0A0B&color=EDEBE6&label=RELEASE" alt="Latest release"></a>
  <a href="https://colonizer.dev/docs/"><img src="https://img.shields.io/badge/DOCS-COLONIZER.DEV-EDEBE6?style=flat-square&labelColor=0A0A0B" alt="Documentation"></a>
  <a href="https://github.com/Colonizer-dev/harness/blob/main/LICENSE"><img src="https://img.shields.io/badge/LICENSE-MIT-FF6B35?style=flat-square&labelColor=0A0A0B" alt="MIT"></a>
</p>

# `colonizer-harness`

The mothership of [Colonizer](https://colonizer.dev): the `colonizer` command.

Every task gets a **colony**: its own KVM microVM with a fresh git worktree and a coding agent inside. The
agent can do anything in there. The colony is linked back to your machine, the **mothership**, so its chat
and a real terminal are one hop away. When the agent needs you, it asks with **choices**. When the work is
done, the mothership publishes it: it commits, pushes and opens the pull request, each only when not
already done.

## Install

```sh
cargo install colonizer-harness --locked
colonizer setup   # installs this version's release, with the app assets, beside the binary
```

This crate is the harness's Rust source. `cargo install colonizer-harness` builds only the `colonizer`
binary, which has no app assets beside it — microsandbox, the in-VM daemon, the agent module and the
web UI all live in the release. `colonizer setup` installs this version's release over that binary,
checking the release's installer against its `SHA256SUMS` first; `curl -fsSL
https://colonizer.dev/install.sh | sh` does the same without a Rust toolchain. The
[install guide](https://colonizer.dev/docs/install) covers both and building from source. Linux
x86_64 with KVM, or an Apple Silicon Mac.

## What it does

- **A microVM per task.** Not a shared-kernel container: a colony that goes rogue can wreck its own
  worktree, and that is all.
- **A private mesh home.** Colonies join a private network with the mothership, never your own tailnet,
  and can't reach each other. If the mesh binaries are absent, colonies are reached on a loopback
  port instead.
- **Choices, not walls of text.** Questions arrive as cards with an "Other…" answer, in a web UI that shows
  each colony's chat and terminal side by side.
- **Placeholders only.** The GitHub token never enters a colony, and the agent's API credential is swapped
  in by the sandbox's host-side TLS proxy, for one host, on the way out.
- **The host publishes.** The microVM is gone before the mothership commits, pushes and opens the pull
  request. Each step runs only if it is still needed, so a publish that failed part-way can just be
  retried.

## The crates

| Crate | What it is |
| :--- | :--- |
| [`colonizer-harness`](https://crates.io/crates/colonizer-harness) | The mothership: the `colonizer` command, its API and the web UI's server |
| [`colonizer-agentd`](https://crates.io/crates/colonizer-agentd) | The daemon inside every colony |

## Documentation

The [install guide](https://colonizer.dev/docs/install), the [vision](https://colonizer.dev/docs/vision),
the [architecture](https://colonizer.dev/docs/architecture) and the
[protocol](https://colonizer.dev/docs/protocol) are on colonizer.dev, mirrored from the repository's
`docs/`. Code, issues and releases: [Colonizer-dev/harness](https://github.com/Colonizer-dev/harness).

MIT.
