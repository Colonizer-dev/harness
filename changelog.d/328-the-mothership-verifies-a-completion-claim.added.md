- **The mothership verifies a completion claim before publishing it.** When a colony's turn ends
  having written `pr.md`, the host snapshots its work without touching the worktree, reads the git
  state itself (commits ahead of base, changed files, the paths the PR description names) and re-runs
  the repository's test command in a fresh one-shot microVM over a git archive of the snapshot —
  never on the host, never trusting the agent's own logs. A confirmed claim publishes as before, an
  unverifiable one publishes too (said so, never counted as confirmed), and a contradicted one holds
  autopilot with `autopilot_held` and the contradictions stated. What runs comes from the `publish`
  module's new `verify` setting — `auto` (the default) reads the repository's own test declaration
  from the base branch (package.json, Cargo.toml, Makefile), `none` opts out per colony or globally,
  or an explicit command — and the verdict lands in the colony's event log as a `verification` host
  event. ([#328])

[#328]: https://github.com/Colonizer-dev/harness/issues/328
