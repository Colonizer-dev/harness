- **The Overview's workspace card shows whole again.** Hovering a row of "Share by workspace" beside
  the merged-PRs chart opens a card for that workspace, centred on the row; for the top rows it
  reaches above the chart's top rule, and the chart body's `overflow: hidden` (there so the KPI
  strip's hairline grid does not poke past its edges) cut the card's top, rounded border and all,
  off at that rule. The chart section now clips sideways only, so the card rises over the heading
  intact; the KPI strip and every other ruled section still clip both ways. ([#565])

[#565]: https://github.com/Colonizer-dev/harness/pull/565
