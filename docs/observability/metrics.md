# Metrics (`GET /metrics`)

The mothership can serve a Prometheus catalogue at `GET /metrics`, collected from its own in-memory
state at scrape time. It is part of the [`observability`](../observability.md) module kind and sits
behind the module's `prometheus` switch, so an install that never asked for it has no such endpoint.

This page covers the catalogue, how to scrape it, what a series is allowed to name, and who may
read it. The wire contract is in [the protocol page](../protocol/observability.md); the design
decisions are in [the ADR](../design/observability.md).

## Turning it on

The `prometheus` setting of any `observability` module (`otlp` or `file` — it is one of the
settings they share, see [Settings and privacy](settings.md)). With it on and the module enabled:

```sh
curl -H "Authorization: Bearer $(cat ~/.config/colonizer/api-token)" \
  http://127.0.0.1:7878/metrics
```

The response is `text/plain; version=0.0.4; charset=utf-8` — the Prometheus text exposition format.
Nothing is buffered between scrapes: each one reads the live state, and nothing in the catalogue is
a mirror of a file.

## The catalogue

| Metric | Type | Labels | What it is |
| --- | --- | --- | --- |
| `colonizer_colonies` | gauge | `status`, `org`, `agent` | Colonies right now, by status (the snake_case wire name), organisation and agent module. |
| `colonizer_colonies_started_total` | counter | — | Colonies started since this install's counters were seeded. |
| `colonizer_colonies_finished_total` | counter | `outcome` | Colonies finished, by outcome: `pr_opened`, `merged`, `closed`, `no_changes`, `stopped`, `failed`, `question`, `suspended`, `restored`. |
| `colonizer_gateway_requests_total` | counter | `provider`, `outcome` | Gateway requests, split `ok` and `error` (`ok` is requests minus failures, never below zero). |
| `colonizer_gateway_fallbacks_total` | counter | `provider` | Gateway answers that will fall back to another model. |
| `colonizer_gateway_in_flight` | gauge | `provider` | Requests streaming right now. |
| `colonizer_gateway_queued` | gauge | `provider` | Requests waiting for a provider slot. |
| `colonizer_gateway_request_duration_seconds` | histogram | `provider` | Request duration, buckets at 0.1, 0.25, 0.5, 1, 2.5, 5, 10, 30, 60, 120 and 300 seconds, plus `+Inf`, `_sum` and `_count`. A request slower than 300 s lands in no finite bucket: it is in `+Inf`, in `_count` and in `_sum` only. A `rate()` over a bucket therefore answers "how many finished inside this bound", not "how many were still running at it". |
| `colonizer_tokens_total` | counter | `org`, `model`, `type` | Tokens spent. Two shapes: `model="all"` with `type` in `input`, `output`, `cache_read`, `cache_write`, and a named model with `type="total"`. |
| `colonizer_cost_usd_total` | counter | `org`, `model` | Cost in US dollars. A model nothing has priced yet has no series at all. |
| `colonizer_questions_open` | gauge | — | Colonies waiting for an answer. |
| `colonizer_attention` | gauge | `reason` | Colonies flagged for attention, by reason. The set is closed — a reason outside it is reported as `other`, because the flag itself is free-form JSON and a sentence must never become a series. |
| `colonizer_queue_depth` | gauge | — | Colonies waiting for a free slot. |
| `colonizer_storage_alert` | gauge | — | `1` while a write or config-read alert is showing, else `0`. |
| `colonizer_disk_free_bytes` | gauge | — | Free bytes on the data volume. **No sample at all** while it has never been measured — an absent series, not a zero. |

Every metric is declared with `# HELP` and `# TYPE` on every scrape, even when it has no series
that scrape, so a dashboard that has never seen a sample still knows the metric exists.

### The two token shapes

`colonizer_tokens_total` describes the same tokens twice, deliberately:

- `model="<name>", type="total"` is one model's total, for "which model is this install spending on".
- `model="all", type="input"|"output"|"cache_read"|"cache_write"` is the org's four totals, for
  "where do the tokens go".

**The two shapes must not be summed together**, and neither may be compared to another as if they
were disjoint: `model="all"` already contains every named model. To get one number per org, use the
`model="all"` series alone; to break it down, use the named-model series alone.

Both are snapshots of the live colony set, not lifetime counters, so they can *fall* when a colony
is deleted. A dashboard that charts them as a rate will show a step down; that is the install
getting smaller, not tokens coming back.

## Scraping it

### Prometheus

```yaml
scrape_configs:
  - job_name: colonizer
    scrape_interval: 30s
    static_configs:
      - targets: ["127.0.0.1:7878"]
        labels:
          install: workstation
    authorization:
      # A scoped `read` token with no org or repo limits. Never the install's own API token in a
      # repo, and never the `observability-headers` secret — a scraper should not be holding either.
      type: Bearer
      credentials_file: /etc/colonizer/prometheus.token
```

### Grafana Alloy

Alloy reads the same token from a file, so the credential never sits in the config:

```alloy
prometheus.scrape "colonizer" {
  targets       = [{"__address__" = "127.0.0.1:7878", "install" = "workstation"}]
  scrape_interval = "30s"
  authorization {
    type     = "Bearer"
    credentials_file = "/etc/colonizer/prometheus.token"
  }
  forward_to = [prometheus.remote_write.grafana.receiver]
}
```

The token is a scoped API token of scope `read` with **no** `orgs` and no `repos`, created with
`POST /api/tokens` (see [Accounts, tokens and install settings](../protocol/auth.md)). Give it a
name a person will recognise later, such as `prometheus`, and revoke it like any other credential.

## Who may read it

`/metrics` is install-wide, so it is guarded like the install rather than like a colony:

| Caller | Answer |
| --- | --- |
| The owner (the install's API token, or the cockpit's cookie) | `200` and the catalogue |
| A `read`, `operate` or `launch` token with no org or repo limits | `200` and the catalogue |
| A token with any `orgs` or `repos` limit | `403` |
| A `fleet` token | `403` |
| A paired phone | `403` |
| No token, or an invalid one | `401` |
| No `observability` module, a disabled one, or `prometheus` off | `404` |
| `repo_names = hashed` on, and the hash key can be read neither at start nor now | `503` |

A limited token is refused rather than served a filtered view. A filtered catalogue is a lie in a
way a filtered colony list is not: the series that were dropped are exactly the ones an operator
needs to see, and their absence would read as "no traffic" rather than "not yours".

A paired phone is refused too, though it gets as far as the cockpit: `host_guard` authenticates a
phone as the owner and lets it make any `GET`. The install's own numbers are the owner's, and a
phone is a revocable credential that lives on someone's device, so `/metrics` is one of the few
things it is not for. Scrape from the computer.

The `503` is the same rule in a different costume. With `repo_names = hashed` and no key to hash
with, the alternative is a scrape in which every org reads `unknown` — a real number, a plausible
one, and one a dashboard would cache as though it were the install. The key is retried per scrape,
so a start that merely raced the data directory does not cost the whole install its metrics.

The auth is the install-wide read token, deliberately not the `observability-headers` secret (ADR
auth rule 5). An OTLP collector holds a backend credential; a Prometheus scraper holds a Colonizer
credential. Keeping them separate means revoking one does not silently stop the other.

## What a series may name

The rules are in the code and enforced at render time; this is the reasoning.

- **No colony id.** One series per colony is unbounded and tells an operator nothing a status gauge
  does not. Colony-level questions are what the [activity log](../protocol/activity.md) and the
  sessions API are for.
- **No `repo` label, and no free text anywhere.** A repository name is a person and a project; a
  failure message, an issue title or an attention flag's `detail` is a sentence. Neither belongs in
  a label, and the [ADR's cardinality rule](../design/observability.md#cardinality-and-loki-labels)
  keeps per-repo series off Loki labels for the same reason. What a label may carry is a code: a
  status, a provider, a model, an agent, an outcome, an attention reason.
- **Every value is sanitised and capped.** Anything outside `[A-Za-z0-9_.:-]` becomes `_`, and a
  value is cut at 64 characters. The three characters the exposition escapes anyway (`\`, `"`,
  newline) are escaped regardless, on the day the safe set is widened.
- **Every family is capped.** A family stops at 200 distinct label sets; past that every further set
  folds into one series whose labels all read `other` (for a histogram, by adding the counters up).
  Ten thousand orgs cost 201 series, not ten thousand.
- **`repo_names = hashed` replaces the org, too.** An org is a customer name, and the metrics
  endpoint is the surface a backend watches most closely, so the setting applies here as well as to
  the exporter: the `org` label becomes `hmac-sha256(key, org)[..12]` in hex — 24 characters. The key
  is 32 random bytes at `<data>/observability/hash.key`, mode 0600, created on first use; a fleet
  copies it between members so the same org hashes the same everywhere. The repository is never
  labelled, hashed or otherwise — it is not in the catalogue at all.

When `repo_names = hashed` is on and the key can be neither read nor created, the endpoint answers
`503` rather than reporting every org as `unknown`, and it retries the key on the next scrape. The
switch asks for a name a backend cannot learn; an `unknown` in every series is a number that says
nothing, and sending it anyway would be worse than sending none.
