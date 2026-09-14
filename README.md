# claude-harness

A small Rust harness with a web UI: connect GitHub, browse the open issues of any repository you can
access, and send an issue to Claude Code. Each run:

1. creates a fresh **git worktree** on a new `claude/issue-<n>-<id>` branch from the default branch,
2. boots a **[microsandbox](https://microsandbox.dev) microVM** (libkrun/KVM, rootless) with that worktree
   mounted at `/workspace`,
3. runs Claude Code headless inside the VM (`--dangerously-skip-permissions`, streamed live to the UI),
4. commits the result on the host, pushes the branch and **opens a pull request** that closes the issue.

## Trust model

| What | Where it lives |
| --- | --- |
| GitHub token | Host only. Commit, push and `gh pr create` run on the host after the VM is gone. |
| Claude token | Host only. The guest env holds a placeholder (`$MSB_CLAUDE_CODE_OAUTH_TOKEN`); microsandbox's TLS proxy substitutes the real value only for requests to `api.anthropic.com`. |
| Worktree | Mounted read-write. |
| Git objects & worktree metadata | Mounted **read-only** (so `git status/diff/log` work in the VM, but commits don't). |
| VM outputs | Treated as untrusted: `.git` is rewritten, nested `.git` dirs are removed, host git runs with hooks and fsmonitor disabled, and `pr.md` is read only if it is a regular file. |

The web API has no login. It binds to `127.0.0.1` by default, rejects unexpected `Host` headers
(DNS rebinding) and cross-origin writes.

## Requirements

- Linux with KVM (`/dev/kvm` readable and writable by your user)
- [microsandbox](https://docs.microsandbox.dev) installed as `msb` (`curl -fsSL https://get.microsandbox.dev | sh`)
- `git`, `gh`, and a native Claude Code binary on the host (it is mounted read-only into the VM, so
  the sandbox image must be glibc-based)
- Rust toolchain to build

## Run

```sh
cargo build --release
./target/release/claude-harness
# open http://127.0.0.1:7878
```

Then open **Settings**:

- **GitHub** – uses your `gh auth login` session automatically, or paste a token.
- **Claude** – run `claude setup-token` and paste the token (an `sk-ant-api…` key works too).

## Configuration

All optional, via environment variables:

| Variable | Default | Meaning |
| --- | --- | --- |
| `HARNESS_BIND` | `127.0.0.1:7878` | Listen address |
| `HARNESS_ALLOWED_HOSTS` | – | Extra `Host` names to accept, comma separated (e.g. a Tailscale name) |
| `HARNESS_IMAGE` | `node:24-bookworm` | OCI image for the microVM |
| `HARNESS_CPUS` / `HARNESS_MEMORY` / `HARNESS_ROOT_DISK` | `4` / `8G` / `16G` | VM resources |
| `HARNESS_MAX_DURATION` | `2h` | Hard limit per run |
| `HARNESS_MAX_PARALLEL` | `3` | Concurrent microVMs; further runs queue |
| `HARNESS_MODEL` | Claude Code default | `--model` passed to Claude Code |
| `HARNESS_CLAUDE_BIN` | auto-detected | Native Claude Code binary to mount |
| `HARNESS_DATA_DIR` | `~/.local/share/claude-harness` | Bare clones, worktrees, job logs |
| `HARNESS_CONFIG_DIR` | `~/.config/claude-harness` | Saved tokens (mode 0600) |

`CLAUDE_CODE_OAUTH_TOKEN`, `ANTHROPIC_API_KEY`, `GH_TOKEN` and `GITHUB_TOKEN` are honoured when no token
is saved in Settings.

## Layout on disk

```
~/.local/share/claude-harness/
  repos/<owner>/<repo>.git          bare clone shared by all runs of a repository
  worktrees/<owner>/<repo>/issue-*  one worktree per run (remove with "Clean up")
  jobs/<id>/in/{prompt.md,run.sh}   mounted read-only at /harness/in
  jobs/<id>/out/pr.md               written by Claude: PR title + body
  jobs/<id>/log.jsonl               harness events + Claude stream-json
  jobs.json                         run history
```

## Run as a user service

```ini
# ~/.config/systemd/user/claude-harness.service
[Unit]
Description=claude-harness

[Service]
ExecStart=%h/Projects/claude-harness/target/release/claude-harness
Environment=PATH=%h/.local/bin:%h/.local/share/mise/installs/claude/latest:/usr/bin
Restart=on-failure

[Install]
WantedBy=default.target
```

```sh
systemctl --user daemon-reload && systemctl --user enable --now claude-harness
```
