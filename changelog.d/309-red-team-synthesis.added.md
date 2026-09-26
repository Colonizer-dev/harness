- **Red-team synthesis.** A red-team run that finds something launches one more colony at `done` —
  the synthesis judge — which merges the hunters' findings into a single ranked report, tracked by
  the run's `synthesis` object and the tally's `merged` count. It fires exactly once, publishes
  nothing, and is retried via `POST /api/redteam/runs/{id}/synthesize`. ([#309])

[#309]: https://github.com/Colonizer-dev/harness/issues/309
