- **A quota probe per provider.** A provider can now carry `quota: {url, pointer}` — a `GET` and an
  RFC 6901 JSON pointer into its answer — fetched with the provider's own credential whenever
  `GET /api/providers/{id}/health` runs, so the cockpit shows what is left in a prepaid token plan. The
  probe rides along on the reachability check and never changes its verdict; its URL must sit on the
  base URL's origin — scheme, host and port, because the credential is sent there — and its pointer
  must start with `/`. Saving with `quota` omitted keeps the stored
  probe, an empty URL clears it, and with no probe the `quota` field stays out of the health answer.
  ([#199])

[#199]: https://github.com/Colonizer-dev/harness/issues/199
