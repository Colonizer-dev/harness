# The v0.1.3 audit

An external source-code audit read Colonizer v0.1.3 (commit `d89ce76`) on 17 September 2026. Its
verdict, in plain words: not ready for unattended work on sensitive repositories with real
credentials. It comes first here on purpose, and this page is edited as findings close.

What the harness is good for today is attended work. You launch the colony, you watch it, and a
human reads the pull request before anything happens next. The audit's tracking issue is
[#91](https://github.com/Colonizer-dev/harness/issues/91).

## What it credits

These held when the audit checked them against the code.

- **The mount split.** The worktree and `/harness/out` are writable; the bare repository, the agent,
  the plugins, the memory scopes and the binaries are mounted read-only.
- **Secrets on the host.** The GitHub token, the provider keys and the mem0 key are written 0600
  under the mothership's config directory, and never enter a colony. The Claude credential is handed
  to the sandbox as a host-scoped secret and swapped in by the TLS proxy for `api.anthropic.com`;
  the guest environment holds a placeholder.
- **Per-colony gateway tokens.** Each colony gets its own random token, written 0600 on the host and
  compared in constant time at the gateway.
- **The publish step.** The branch must carry the `colonizer/` prefix and must not be the base
  branch. There is no force-push, and nothing merges automatically.
- **Untrusted colony output.** The worktree's `.git` is rewritten from the value recorded before the
  VM ran, nested `.git` directories are removed, and `pr.md` must be a regular file within a size
  limit.
- **Memory review.** Proposals arrive pending, and only an approved note is mounted into another
  colony. Review is on unless an operator turns it off.
- **Host and Origin checks.** The API binds to loopback, rejects unknown `Host` values, and requires
  a matching `Origin` on non-GET requests and WebSocket upgrades.
- **Pinned inputs.** The vendored artefacts, the agent binary and the Headroom bundles are verified
  against recorded sha256 digests, and the GitHub Actions are pinned by commit.

## Findings

Four findings describe ways a colony could cross into the host. They are filed as draft security
advisories, visible to maintainers only, and are not described here until they are fixed.

| Finding | Severity | Detail |
| :--- | :--- | :--- |
| F01 | High | Withheld until fixed. |
| F02 | High | Withheld until fixed. |
| F03 | High | Withheld until fixed. |
| F04 | Medium | Withheld until fixed. |

The public ones, each with its issue:

| Finding | Issue |
| :--- | :--- |
| **F05.** No fail-closed gate for pushes, PRs and issues | [#84](https://github.com/Colonizer-dev/harness/issues/84) |
| **F06.** A publish that fails after the commit can't be completed | [#85](https://github.com/Colonizer-dev/harness/issues/85) |
| **F07.** The parallel limit isn't atomic, and costs aren't a budget | [#86](https://github.com/Colonizer-dev/harness/issues/86) |
| **F08.** Failed writes of state and events are ignored | [#87](https://github.com/Colonizer-dev/harness/issues/87) |
| **F09.** Telemetry retention isn't guaranteed | [#88](https://github.com/Colonizer-dev/harness/issues/88) |
| **F10.** Release provenance and pinned inputs | [#89](https://github.com/Colonizer-dev/harness/issues/89), with CI and real-colony tests in [#73](https://github.com/Colonizer-dev/harness/issues/73) |
| The installer's app swap isn't atomic | [#90](https://github.com/Colonizer-dev/harness/issues/90) |

## Checkpoints

Four gates have to pass before unattended work is on the table.

- **G1, boundaries.** F01–F04 fixed, each with a negative test on a real colony. That covers host
  access, a writer that stays behind, and credential use beyond the colony's routes.
- **G2, external effects.** The no-write policy for publishing and issues, and gates bound to the
  exact tree, work ([#84](https://github.com/Colonizer-dev/harness/issues/84)).
- **G3, recovery.** Failures injected into storage, VM stop, commit, push, PR, concurrency and
  updates; recovery is idempotent and keeps an accurate record
  ([#85](https://github.com/Colonizer-dev/harness/issues/85),
  [#86](https://github.com/Colonizer-dev/harness/issues/86),
  [#87](https://github.com/Colonizer-dev/harness/issues/87),
  [#90](https://github.com/Colonizer-dev/harness/issues/90)).
- **G4, release.** Full CI and native end-to-end tests on a pinned runtime; privacy retention and
  supply-chain evidence meet the requirements
  ([#73](https://github.com/Colonizer-dev/harness/issues/73),
  [#88](https://github.com/Colonizer-dev/harness/issues/88),
  [#89](https://github.com/Colonizer-dev/harness/issues/89)).

## Where this stands

Nothing is checked off yet, and no gate is cleared. The roadmap is the issue tracker, and this page
is the audit's view of it. One fact worth stating plainly: the Rust test suites are not run by CI
today — there is no test workflow, and `.github/workflows/` holds only `headroom-bundle.yml`,
`release.yml` and `vendored-plugin-updates.yml`. That is part of what G4 asks for.
