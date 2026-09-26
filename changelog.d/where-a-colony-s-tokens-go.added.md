- **Where a colony's tokens go.** The colony report files every turn's token spend under one of six
  categories — read, search, command_output, edit, reasoning, replay — from the tools the turn called,
  a subagent's calls included (edit beats command_output beats search beats read beats reasoning; cache
  reads and writes are always replay). Each `turn_end`'s cumulative `model_usage` is diffed against the
  previous snapshot and floored at zero exactly as the spend journal does, so a colony's categories add
  up to its recorded usage; a `turn_end` without `model_usage` leaves the baseline for the next measured
  turn instead of re-counting it. The report gains a "Token categories" table, `--json` carries
  `tokenCategories`/`tokenCategoriesByModel`, and `bench-<label>.json` records `token_categories` per
  task and run so a change to where tokens go becomes measurable. A first slice of #469: the Rust
  journal and the cockpit chart are still to come.
