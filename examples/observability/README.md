# Observability examples

The Grafana half of the observability epic ([#863](https://github.com/Colonizer-dev/harness/issues/863)):
the provisioned datasources and dashboards a local stack loads. The stack itself — the compose file,
Alloy, the fixture replay — is [#862](https://github.com/Colonizer-dev/harness/issues/862):
[grafana/README.md](grafana/README.md) runs it. Pointing your own Grafana at the pieces below
still works.

- `grafana/provisioning/datasources/datasources.yml` — Prometheus, Loki and Tempo. Their uids
  (`colonizer-prom`, `colonizer-loki`, `colonizer-tempo`) are the contract every dashboard queries,
  so a dashboard works unchanged against the local stack or a remote one.
- `grafana/provisioning/dashboards/dashboards.yml` — watches the dashboards directory.
- `grafana/dashboards/` — the dashboard JSONs.

Mount `provisioning/` at `/etc/grafana/provisioning` and `dashboards/` at
`/var/lib/grafana/dashboards`:

```yaml
volumes:
  - ./examples/observability/grafana/provisioning:/etc/grafana/provisioning
  - ./examples/observability/grafana/dashboards:/var/lib/grafana/dashboards
```

Loki sanitises label names on ingest (dots become underscores), so LogQL selectors use
`service_name` and `colonizer_stream`; `colonizer.colony.id` stays an attribute, filtered as
`| colonizer_colony_id="…"` rather than a label.

`node scripts/check-dashboards.mjs` is the gate: it fails a dashboard that does not parse, queries
a metric neither catalogue lists or a datasource uid nothing provisions, repeats a panel id or a
dashboard uid, or hard-codes a colony id instead of a dashboard variable.
