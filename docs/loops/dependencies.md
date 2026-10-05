# Dependencies & supply chain

Part of the [Colonizer loops](../loops.md).

A built-in loop that checks the dependencies of the repositories you opt in and opens pull requests
that fix what it finds. It is **off** until you switch it on *and* add an org (`acme`) or a
repository (`acme/app`) to its allowlist; both start empty. It is at the top of the Loops page, and
its settings are in `<config_dir>/supply-chain-loop.json`.

**When it runs.** Daily by default (06:17 UTC); hourly, every 6 hours or weekly from the page, and
never more often than hourly. **Dry run** runs the whole check and lists what it would dispatch,
without starting a colony or saving anything; **Run now** does a real run.

**What it checks.** The manifests and lockfiles at each repository's default branch, read from the
mothership's own mirror, never inside a colony: `Cargo.lock`, `package-lock.json`, `bun.lock`,
`pnpm-lock.yaml`, `yarn.lock`, `poetry.lock`, `uv.lock`, `requirements.txt` and `go.mod`. Each
lockfile goes to the first scanner installed on the host that reads it:

| Lockfile | Scanners, best first |
| :--- | :--- |
| `Cargo.lock` | `cargo-audit`, then `osv-scanner` |
| `package-lock.json` | `npm audit`, then `osv-scanner` |
| `pnpm-lock.yaml`, `yarn.lock`, `poetry.lock`, `uv.lock`, `requirements.txt`, `go.mod` | `osv-scanner` |
| `deny.toml` (a licence policy) | `cargo-deny` (`check licenses bans`) |

A lockfile no installed scanner reads, `bun.lock` included, is checked with the mothership's own OSV
lookup (the one behind the Packages tab), unless you switch that off. The report says which scanner
was missing and how to install it; Colonizer never downloads or runs a tool you have not installed.

It collects known vulnerabilities with their severity and fixed version, yanked versions,
unmaintained (RustSec) and deprecated packages, and licence-policy violations when the repository
has a `deny.toml`. **Outdated direct dependencies** (a major version or more behind) are reported
when you tick that setting; they are off by default and never dispatched.

**How it fixes.** The fixable findings (a vulnerability with a fixed version, or a yanked release)
at or above the dispatch threshold (moderate by default) are grouped into one *supply-chain target*
per repository and ecosystem, and one colony gets the whole group, never one per package. Its brief
lists the exact findings and says: make the minimal bump to each fixed version, update the
lockfiles, run the repository's checks, make no unrelated upgrades, and make no major bump unless
it is the only way to a fix (and then explain it in the pull request). Its origin is
`supply-chain:<ecosystem>`.

A target is **not** dispatched, and the report says why, when:

- `COLONIZER_NO_EXTERNAL_EFFECTS` (or `COLONIZER_NO_WRITE`) is set: the run reports only;
- a colony of the same repository is still live, queued, parked or has its pull request open on
  any of the same findings — another loop colony, a Packages-tab hand-off, or a `colonizer launch
  --package --advisory` — by the one rule every launch uses
  ([duplicates](../protocol/duplicates.md)): the duplicate target is refused, naming that colony;
- the repository is cooling down: 12 hours after its last dispatch by default;
- the run has reached its caps: 1 colony per repository and 3 per run by default. Repositories are
  checked one at a time, so a run never bursts.

**The report.** Every run records its findings by severity, what it dispatched and what it skipped
and why. The last report and the history of runs are on the Loops page, and each run and each
dispatch is a `loop.supply_chain` line in the activity log. A critical or high vulnerability with
no fixed version raises an attention item on the page, since no colony can bump past it.
