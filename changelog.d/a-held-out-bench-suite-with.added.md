- **A held-out bench suite with gap reporting.** `scripts/bench/heldout.mjs` gives each bench task family a
  companion check kept outside the repository (a set inside it is refused). `run --heldout <dir>` resolves
  every family's companion before any colony launches, scores each pull request against it on a fresh,
  guarded clone keeping only the pass bit, retires companions after three scoring decisions, and fails the
  run naming any family whose visible-vs-held-out gap beats `--max-gap` (0.25). See [docs/bench.md](docs/bench.md).
