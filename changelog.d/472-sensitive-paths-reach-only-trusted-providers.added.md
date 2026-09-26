- **Sensitive paths reach only trusted providers.** At boot, the file paths a task names are now
  classified for sensitivity — `open` (docs, vendored code), `standard` (ordinary app code),
  `custom` and `restricted` (secrets: `.env` files, private keys, cloud credentials, infra config)
  — from built-in defaults, extendable per repository with `.colonizer/sensitivity.toml`, and the
  strictest class is recorded on the colony. The gateway refuses a `restricted` colony any provider
  not marked `trusted` in providers.json: configured is not vetted, and cheap is not private.
  Other classes change nothing yet; a vetted tier and org-level policy are still to come. ([#472])

[#472]: https://github.com/Colonizer-dev/harness/issues/472
