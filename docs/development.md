# Development

The test suites, the reports and the local UI. How to open a pull request, and the full list of
checks CI runs, is in [CONTRIBUTING.md](../CONTRIBUTING.md).

```sh
cargo test --workspace                          # mothership and agentd, including agentd's no-KVM smoke test
cargo clippy --workspace --all-targets -- -D warnings
(cd modules/agents/claude-code && npm test)
(cd modules/agents/pi && npm test)
(cd modules/agents/codex && npm test)
(cd modules/agents/acp && npm test)
(cd services/telemetry && npm test)             # the live map's receiver
(cd services/relay && npm test)                 # remote access relay
(cd web && npm run build && npm test)           # tsc, vite, and the UI's own tests
node --test scripts/test/colony-report.test.mjs
node scripts/colony-report.mjs                  # how colonies went, from what they already log
node scripts/colony-e2e.mjs                     # boots a real colony against a stub model (CI's colony-e2e job)
node scripts/colony-report.mjs --transcript <id> # one colony, step by step
node scripts/colony-report.mjs --costs          # spend.jsonl by colony, last 30 days: estimated vs metered, harness × model
node --test scripts/test/bench.test.mjs
node scripts/bench.mjs run --repo owner/bench --label before   # the fixed tasks, scored (docs/bench.md)
node scripts/trajectory-monitor.mjs --session <id>  # post-hoc: was a resolved colony clean? (docs/trajectory-monitor.md)
node scripts/evolve.mjs diagnose --bench bench-after.json  # offline evolver: failures → classes → proposals → retain (docs/evolver.md)
sh scripts/test/build-scripts.test.sh           # the build scripts: here mode refused off Linux, no non-ELF artefact installed or served
sh scripts/test/install-release.test.sh         # the installer: an install interrupted at any point leaves a working colonizer, and the next one recovers
(cd web && npm run dev)                         # UI dev server; proxies /api to 127.0.0.1:7878
# http://127.0.0.1:5173/?mock=1                 # the UI against an in-browser mock backend
node scripts/require-ci-checks.mjs             # dry run: the ruleset that makes CI required on main
node scripts/require-ci-checks.mjs --apply     # send it (idempotent; needs a repository-admin token)
```

CI is meant to gate merges on `main`: the ruleset that makes the six always-running CI jobs required
checks is printed by the dry run above, and [docs/audit.md](audit.md) ("Required checks, issue
#367") says which checks those are, which are deliberately left optional, and the two repository
settings that travel with them.
