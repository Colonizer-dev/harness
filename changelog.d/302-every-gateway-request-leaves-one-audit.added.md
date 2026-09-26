- **Every gateway request leaves one audit line.** The provider gateway now appends a per-request
  record to the colony's `gateway.jsonl`: what was asked for (provider, wire, method, path, the
  requested and sent model), how it ended (status, a typed failure code, whether the Claude fallback
  was licensed, queue and total duration, bytes, token counts) — and nothing else. The record is a
  fixed struct that is the whole allowlist, so keys, tokens and request bodies never reach the log,
  and the colony's own credential headers are never forwarded upstream. `colony-report` counts each
  colony's gateway requests and failures, Settings shows a provider's last failure code beside its
  failure rate, and the provider-degraded notification carries it. ([#302])

[#302]: https://github.com/Colonizer-dev/harness/issues/302
