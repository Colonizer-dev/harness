# Trust model

Colonies only ever hold placeholders: the GitHub token never enters a colony, and the agent's API credential is swapped in by the sandbox's host-side TLS proxy, for one host, on the way out. What that promise is worth today is in [audit.md](audit.md): an external audit of v0.1.3 found four ways past the wall that keeps secrets on the host, and they are not fixed yet. Which of these controls are enforced boundaries and which are guidance is in [boundaries.md](boundaries.md).

| What | Where it lives |
| :--- | :--- |
| GitHub token | Mothership only. Commit, push and `gh pr create` run on the host after the colony is gone. |
| Claude token | Mothership only (system keychain, or a 0600 file). The colony sees a placeholder; microsandbox's TLS proxy substitutes the real value for `api.anthropic.com` only. |
| Model provider keys | Mothership only (system keychain, or a 0600 file). Colonies send provider requests to the gateway with a per-colony token; the gateway adds the key. |
| Worktree | Mounted read-write at `/workspace`. |
| Colony secrets you name | Mothership only; microsandbox substitutes the value on TLS to the hosts you allowed, so the colony sees a placeholder. |
| Git objects and worktree metadata | Mounted read-only: `git status`, `diff` and `log` work in the colony, commits don't. |
| Colony output | Untrusted until published: `.git` rewritten, nested `.git` removed, no hooks or fsmonitor, `pr.md` must be a regular file. |
| Prompt screening (screen module) | Off until you configure it. When on, it reads the colony's diff and PR body at publish time, classifies hidden code points, and holds (`block`) or annotates (`warn`) the publish. It sees colony output, holds no credentials, and sends nothing anywhere — no network, no model ([docs/prompt-screening.md](prompt-screening.md)). |
| What a colony runs | Pinned, not floating: the image by OCI digest (`crates/colonizer/images.lock`), the guest Claude Code build by sha256 (`crates/colonizer/claude-code.lock`), the vendored tools by sha256 (`vendor/vendor.lock`). Pins move only through a reviewed pull request. |
| Release downloads | Checked against the release's `SHA256SUMS`, which itself carries a build-provenance attestation the installer verifies whenever `gh` can reach a verdict ([docs/install.md](install.md)). |
| Mesh | Own Headscale and userspace `tailscaled`, own state and socket, `--no-logs-no-support`. Mothership reaches colonies; colonies can't reach each other. A [fleet](fleet.md) member reaches only the mothership's own node, never a colony. |
| colonizer-agentd | Per-colony bearer token, even inside the mesh. |
| Live map | Off until you switch it on. When on, a heartbeat every 5 minutes: a random id, version, platform and colony count. No code, repositories or names ([docs/telemetry.md](telemetry.md)). |
| Usage data | On by default: an anonymous batch of counts, shown in full before anything is sent. Sent at most once a day, and only when `COLONIZER_TELEMETRY_ENDPOINT` names a collector — unset, nothing is sent at all. A different random id from the live map's; `colonizer telemetry off` switches it off ([docs/usage-data.md](usage-data.md)). |

Colonies are detached: they keep running when the mothership restarts, and it reconnects to them.

An external audit read this table against the code at v0.1.3. What it confirmed, what it found
instead, and what has to be true before unattended work: [docs/audit.md](audit.md).
