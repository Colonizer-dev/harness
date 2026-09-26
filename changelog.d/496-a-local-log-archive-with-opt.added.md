- **A local log archive, with opt-in retention.** When a colony ends, its session directory — `events.jsonl`, the
  rotated logs, `out/pr.md`, `egress.json` — is tarred, zstd-compressed and filed under
  `<data_dir>/archive/<org>/<repo>/<yyyy>/<mm>/`, one revision per real change (`<id>.tar.zst`, then `<id>.r2`,
  …) and never overwriting the bundle before it. Deleting a colony archives its logs first, so a delete moves them
  into the archive instead of destroying them; `DELETE /api/sessions/{id}?purge_logs=true` is the only way the
  bundles go with it. `GET /api/archive` lists every revision with its index record (cost, tokens, models, PR,
  mothership), and `POST /api/archive/retention` is the preview/dry-run pair: a pure plan over the index (bundles
  older than `keep_days`, then oldest-first until `max_gb`), and an apply that refuses to remove anything unless
  `expect` repeats exactly the preview's list — and nothing at all while a bundle is the only copy, unless the
  request explicitly allows it. First slice of #496. ([#496])

[#496]: https://github.com/Colonizer-dev/harness/issues/496
