# Spend and cost history

Part of the [Colonizer protocol](../protocol.md).

## 6.8 Spend (per org and per day)

A UI can show an org what its colonies have spent, and history of that spend after a restart — even
after colonies are cleaned up or deleted. Two surfaces answer, from two sources that agree by
construction:

- Every org entry of `GET /api/orgs` carries a `spend` object, summed live over the org's sessions:
  whatever a session record currently holds is what the org reads as spent.
- `GET /api/spend/history` answers the same shape per day, from an append-only journal, so the
  picture survives the session records being cleaned up or deleted.

```json
"spend": {
  "cost_usd": 12.47,
  "routed_cost_usd": 0.03,
  "tokens": {"input": 482001, "output": 123477, "cache_read": 900233, "cache_write": 4412},
  "models": [
    {"model": "claude-opus-5", "tokens": 932190, "cost_usd": 11.80},
    {"model": "deepseek/deepseek-flash", "tokens": 577933, "cost_usd": null}
  ]
}
```

`cost_usd` adds the sessions' (or the day's) Claude-side estimates — `null` until any session (or
any journal row) measured one, never `0.0` for an unmeasured cost. `routed_cost_usd` is the same
addition over what the provider gateway routed and priced (§6.5). `tokens` sums `input_tokens`,
`output_tokens`, `cache_read_tokens` and `cache_write_tokens` over the sessions' `model_usage`,
`0` while nothing reported them. `models` lists every model the org (or day) used, sorted by tokens
descending (ties by name). Each entry's `tokens` is that model's four counts summed; its
`cost_usd` is the attributed cost, `null` when nothing attributed one.

The cost-attribution rule, on both surfaces. A session's Claude-reported cost (`cost_usd`) is
attributed to its model only when the session's `model_usage` contains exactly one model. A
multi-model session contributes its cost to the org totals but to no model's row, and the split
such a row would need is never fabricated. Gateway-routed dollars are never attributed per model:
the gateway prices whole responses and cannot say which of its models served one (the journal's
`routed` rows carry no model), so they reach the org totals' `routed_cost_usd` alone. A model whose
cost was never attributed reads `null`, never `0.0` — and never a routed dollar. The history
applies the same rule per turn, so the two surfaces agree. A subscription plan that reports no cost
at all reads as `null`, never as free.

## `GET /api/spend/history?days=30`

```json
{"days": [
  {"day": "2026-09-20", "orgs": [
    {"org": "acme",
     "cost_usd": 12.47, "routed_cost_usd": 0.03,
     "tokens": {"input": 482001, "output": 123477, "cache_read": 900233, "cache_write": 4412},
     "models": [{"model": "claude-opus-5", "tokens": 932190, "cost_usd": 11.80}],
     "launched": 2, "returned": 1, "scoring_ms": 0}
  ]}
]}
```

`days` is how far back to answer, default 30, clamped to 1–365. Days come back oldest first and only
days the journal mentions appear; each day's orgs are sorted by org name. Per day, `orgs` entries
carry the `spend` object above plus `launched` (colonies admitted that day, queued or starting),
`returned` (colonies that crossed into a terminal state that day — pull request opened, merged or
closed, nothing to push, or stopped/failed) and `scoring_ms` (the bench's scoring time journaled
that day, `0` where none). A colony counts as returned once per run, on the transition, never on
the later updates.

The journal behind it is `spend.jsonl` in the data dir, next to `sessions.json` and `routing.jsonl`:
append-only, one JSON line per event, never rewritten. Colony cleanup and deletion do not touch it,
so the history outlives the sessions that made it. A line that fails to parse, or a row from a newer
build whose extra keys this one does not know, is skipped rather than fatal. The rows, with the UTC
day each one is filed under and `cost_usd` omitted while nothing measured it. Colony-scoped rows
also name the colony (`session`) and the agent module that ran it (`agent`, the harness); chat rows
and rows from builds before those fields existed carry neither, and both still parse:

```json
{"ts": "…", "day": "2026-09-20", "org": "acme", "kind": "usage", "session": "clgay4wk", "agent": "claude-code", "model": "claude-opus-5",
 "input_tokens": 400, "output_tokens": 10, "cache_read_tokens": 0, "cache_write_tokens": 0, "cost_usd": 1.50}
{"ts": "…", "day": "2026-09-20", "org": "acme", "kind": "usage", "session": "clgay4wk", "agent": "claude-code", "cost_usd": 0.09}
{"ts": "…", "day": "2026-09-20", "org": "acme", "kind": "routed", "session": "clgay4wk", "agent": "claude-code", "cost_usd": 0.03}
{"ts": "…", "day": "2026-09-20", "org": "acme", "kind": "launched", "session": "clgay4wk", "agent": "claude-code"}
{"ts": "…", "day": "2026-09-20", "org": "acme", "kind": "returned", "session": "clgay4wk", "agent": "claude-code"}
{"ts": "…", "day": "2026-09-20", "org": "bench", "kind": "scoring", "scoring_ms": 9320}
```

Every row carries the four token fields, `0` where it has no tokens; a `scoring` row carries none
of them. `usage` rows are a turn's increment over the turn before it (the session record keeps the
cumulative; the journal gets the deltas). A one-model turn files its cost on that model's row; a
multi-model turn files per-model token rows and its cost on an un-modeled row, mirroring the
attribution rule. A failed append is reported through the app's sticky storage alert and leaves
the run unchanged: a lost row is a lost measurement, not a failed run.

A `scoring` row is the bench's ([bench.md](../bench.md)): when a `scripts/bench.mjs run` finishes it
files how long it spent scoring the run's pull requests under the `bench` org, the way chat files
under its pseudo-org — no colony, no tokens and no dollar, since scoring makes no model calls and
its only cost is time. The history sums those into the org entry's `scoring_ms` for the day, beside
the colonies' spend.

Which channel measured a row's `cost_usd` splits each colony's spend in two. A `usage` row's dollar
is the agent's own turn-end estimate: first-party traffic never passes the gateway — microsandbox
swaps the credential for `api.anthropic.com` at its TLS edge — so the only witness to that spend is
the agent itself, and the figure arrives once a turn, already an estimate. A `routed` row's dollar
is the gateway's metered price, computed from the provider's `pricing` rates as it counts the
response (§6.5). The split bounds the budget the same way: a routed provider without `pricing`
still counts its tokens but contributes $0, so a colony that spends only through one never reaches
`budget_usd` and is never stopped for spend — give such a provider its rates first (§6.5 lists the
five). Past `budget_usd` the mothership stops the colony and the gateway refuses its further routed
requests with `403` (§6.5); past `host_disk` it stops the colony the same way, the footprint
measured every five minutes. Either way the worktree is kept, so raising the limit and pressing
Resume continues the colony.

`scripts/colony-report.mjs --costs` reads the journal and answers per colony instead of per org:
each colony's rows grouped under its `session` id with the `agent` that ran them, `estimated` and
`metered` shown separately (a `–` is unmeasured, never $0), a colony with tokens but no measured
dollar labelled `unpriced — tokens only`, and rows that carry no `session` — chat, and every row
from a build before the field existed — reported as `unattributed`, so the totals still sum to the
whole window. A second table rolls the same rows up by harness × model. By default the report sums
the same window `GET /api/spend/history` answers — the last 30 days ending today UTC — keeping rows
by `day`: `--days` (clamped 1–365) changes the length, `--since` overrides the floor, rows dated
after today drop either way, the Total line names the window, and `--json` emits the same shape
with `window` on it; `--repo` keeps the colonies sessions.json names with that repository.
