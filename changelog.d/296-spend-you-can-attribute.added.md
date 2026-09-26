- **Spend you can attribute.** Every colony-scoped row of the spend journal (`<data>/spend.jsonl`)
  now names the colony (`session`) and the agent module — the harness — that ran it, and the routing
  decision recorded at boot carries the same `agent`, so a routing choice can be joined with what the
  colony went on to spend. `node scripts/colony-report.mjs --costs` reads the journal grouped per
  colony: the agent's own turn-end estimate against what the gateway metered for routed providers,
  shown separately (a `–` is unmeasured, never $0), colonies with tokens but no measured dollar
  labelled `unpriced — tokens only`, rows with no colony — chat, and older rows — under
  `unattributed` so the totals still sum to the whole window, and a harness × model rollup for the
  question the per-colony table cannot answer. The report defaults to the spend history's own
  window — the last 30 days ending today, `--days` changing the length and `--since` the floor —
  and names it in its Total line and `--json`. Bench comparisons gain a Harness · model column per
  task. See [docs/protocol.md](docs/protocol.md) §6.8 and [docs/bench.md](docs/bench.md). ([#296])

[#296]: https://github.com/Colonizer-dev/harness/issues/296
