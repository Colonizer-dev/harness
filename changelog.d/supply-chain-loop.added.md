**A built-in "Dependencies & supply chain" loop.** Off by default and opt-in per org or repository,
it checks each opted-in repository's lockfiles on the mothership's mirror (never inside a colony)
with the scanners already on the host — `cargo-audit`, `cargo-deny` for a `deny.toml` licence
policy, `npm audit` and `osv-scanner` — or the mothership's own OSV lookup, and says which scanner
to install when none reads a lockfile. Fixable findings become one colony per repository and
ecosystem with a brief asking for minimal bumps and nothing else; duplicates of an open target,
the per-repository cooldown, the per-run caps and `COLONIZER_NO_EXTERNAL_EFFECTS` hold a dispatch
back. Every run is reported on the Loops page and in the activity log, a critical or high finding
with no fix raises an attention item, and a dry run writes nothing. See
[docs/loops.md](docs/loops.md#dependencies--supply-chain).
