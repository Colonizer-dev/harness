- **External calibration against SWE-bench.** `scripts/swebench.mjs` runs the colonies on work nobody
  here chose: each instance becomes a private single-commit snapshot of the upstream repo (no history, no
  eval artifacts, no remote), runs under a required budget envelope that stops cleanly without
  extrapolating unpaid tasks, and is scored by the official SWE-bench harness, with raw and clean rates —
  a patch that edits the hidden tests is flagged and kept out of the clean count. Stages: Lite, then
  Verified, then Multilingual. Runs stay labeled uncalibrated until the remaining controls (#330's gold
  sanity gate, network and trajectory monitoring) land. ([#331])

[#331]: https://github.com/Colonizer-dev/harness/issues/331
