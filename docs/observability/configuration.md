# Observability: environment configuration

The `observability` exporter reads the standard OpenTelemetry `OTEL_*` variables, so a machine
already set up for another OpenTelemetry program can be pointed at Colonizer without re-entering
every field, and an orchestrator can configure it per deployment. The variables are an **override**,
never a switch: they change what a configured exporter sends, and only the module or one master
variable decide whether anything sends at all.

The effective settings are on `GET /api/observability/status`, with `provenance` naming where each
field came from — `module`, `env:<VAR>` or `default`. The rest of the module is in
[settings and privacy](settings.md); what happens to the data once it leaves is in
[export policy](export-policy.md).

## Turning export on

**An environment variable never turns export on by itself.** Nothing is written and nothing is
spawned until either:

- the `observability` module is saved **and** enabled, or
- `COLONIZER_OBSERVABILITY=on` is set in the mothership's own environment.

So an `OTEL_EXPORTER_OTLP_ENDPOINT` already sitting in a systemd unit for another program, or a
Kubernetes ConfigMap full of `OTEL_*` keys, changes nothing until you opt in. That is deliberate: a
service-wide variable map is not consent to send an operator's logs somewhere.

Two variables can turn it back off, whatever else says:

| Variable | Effect |
| :--- | :--- |
| `OTEL_SDK_DISABLED` | Set to `true`, export is off. Checked first, so it wins over the module, over `COLONIZER_OBSERVABILITY=on` and over everything else. |
| `COLONIZER_OBSERVABILITY` | The master switch. `on`, `true`, `1` or `yes` (any case) enables an environment-only setup. **Any other value — `off`, `false`, `0`, `no`, or empty — turns export off even when a module is saved and enabled.** Set, but not truthy, is a switch off, not a mistake to be ignored. |

`GET /api/observability/status` reports `configured: false` and a `reason` in either case, and
`POST /api/observability/test` answers `409` rather than sending anything.

## The variables, and which wins

Every variable below overrides **one** field of the resolved settings. Precedence, highest first:

1. the **per-signal** variable (`OTEL_EXPORTER_OTLP_LOGS_ENDPOINT`, …),
2. the **base** `OTEL_EXPORTER_OTLP_*` variable,
3. the **Observability headers** secret (headers only),
4. the saved **module** setting,
5. the built-in **default**.

A variable set to an empty or whitespace-only value is treated as unset, so `OTEL_SERVICE_NAME=`
in a unit file does not blank the service name.

| Variable | Field | Default | What it is |
| :--- | :--- | :--- | :--- |
| `COLONIZER_OBSERVABILITY` | *(master switch)* | unset | `on` enables an environment-only setup; anything else turns export off. Not an override. |
| `OTEL_SDK_DISABLED` | *(master off)* | unset | `true` turns export off, whatever else says. |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | `endpoint` | *(module)* | The base OTLP/HTTP endpoint. Stored as given; the add-on appends `/v1/logs`, `/v1/traces` and `/v1/metrics` to it. Checked like a save: no credentials (`user:pass@`), no query string, and a plain `http://` only to a loopback or private address unless the module's **Allow plain http to a public host** is on. |
| `OTEL_EXPORTER_OTLP_PROTOCOL` | `protocol` | `http/protobuf` | `http/protobuf` or `http/json`; `grpc` is refused in this build. |
| `OTEL_EXPORTER_OTLP_COMPRESSION` | `compression` | `gzip` | `gzip` or `none`. |
| `OTEL_EXPORTER_OTLP_TIMEOUT` | `timeout_secs` | `10` | In **milliseconds**, as the specification has it, rounded up to whole seconds and clamped to 1–120. The module's field is in seconds. |
| `OTEL_EXPORTER_OTLP_HEADERS` | `headers` | *(secret)* | The `k=v,…` list below. Takes precedence over the **Observability headers** secret; the secret is read only when this is unset. |
| `OTEL_EXPORTER_OTLP_LOGS_ENDPOINT` | `logs_endpoint` | *(none)* | Sent **verbatim**: no `/v1/logs` is appended. Reaches `check_endpoint` like the base one. |
| `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` | `traces_endpoint` | *(none)* | Sent verbatim; no `/v1/traces` appended. |
| `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT` | `metrics_endpoint` | *(none)* | Sent verbatim; no `/v1/metrics` appended. |
| `OTEL_SERVICE_NAME` | `service_name` | `colonizer-mothership` | Also becomes the resource's `service.name`. |
| `OTEL_RESOURCE_ATTRIBUTES` | `resource_attributes` | *(see below)* | `k=v,…` pairs merged **underneath** the real resource, so it can add keys but never claim to be one of them. |

With no base endpoint and no per-signal endpoint, an enabled setup is a configuration error rather
than a silent no-op: the status says `no OTLP endpoint is configured` and names which one to set.

## Header lists

`OTEL_EXPORTER_OTLP_HEADERS` and the **Observability headers** secret take the same form, which is
the one the OpenTelemetry specification defines:

```sh
OTEL_EXPORTER_OTLP_HEADERS='Authorization=Basic%20<base64>,x-honeycomb-team=<team-key>'
```

- Pairs are separated by `,`, and whitespace around a pair, a name or a value is trimmed.
- The split is on the **first** `=`, so a value may contain `=` (base64 padding, a query string).
- Values are **percent-decoded**, which is what makes `Basic%20…` and a name with a space work.
- An **empty value** (`x-empty=`) is legal and is sent as an empty string.
- An empty pair (`,,`) is skipped, so a trailing comma is harmless.
- A pair with **no `=`**, a name that is empty or is not a legal HTTP token, a malformed `%`-escape,
  or a control character in a value is malformed. The **whole list is refused**: nothing is
  exported, and no partial credential is ever sent.
- A malformed pair is reported by its **1-based position** in the list, never by its text — a
  pair with no `=` *is* usually the credential (`Authorization: Basic …` typed with a colon, or a
  token pasted on its own), and it reaches the status page. A pair that does have a legal name is
  still named, so it can be found, and a very long name is truncated.

These are exactly the rules the add-on applies when it reads the same list off its stdin, so a
header list the status page calls valid never dies at spawn.

The values are handed to the add-on as one JSON line on its stdin and never written to
`<data>/observability/exporter.json`, a log line or an API answer. The status API reports the
header **names** and where they came from (`env`, `secret` or `none`), and that is all it can
report.

## The resource

Every export carries the same resource, so records from several machines are told apart:

| Attribute | Value |
| :--- | :--- |
| `service.name` | `OTEL_SERVICE_NAME` if set, else `colonizer-mothership`. |
| `service.version` | The mothership's version. |
| `service.instance.id` | This install's host id, the stable one in `<config_dir>/host_id`. |
| `host.name` | The machine's hostname, when it can be read. |
| `colonizer.fleet.id` | This install's host id, **when it is alone** — a lone mothership is a fleet of one. Omitted on a fleet member, which cannot yet know its owner's id; the exporter falls back to its own host id there. |
| `colonizer.fleet.role` | `owner` or `member`, **only when this mothership is in a fleet**. A lone mothership has no role attribute. |

`OTEL_RESOURCE_ATTRIBUTES` merges **underneath** that table. It can add keys
(`deployment.environment=prod`, `team=infra%20ops`) and they are all exported, but it can never
supply `service.name`, `service.instance.id`, `host.name` or anything starting with `colonizer.` —
those are removed from the environment's list and then set from what this mothership knows, so
neither the value nor the mere presence of one of them can survive. A member therefore carries no
`colonizer.fleet.id` even if the environment offers one, and a lone mothership carries no role. A
malformed pair is skipped and the rest is kept, and the status says so in `provenance`.

## Upgrading

**Nothing changes for an existing install until you opt in.** A mothership that has never saved an
`observability` module and has no `COLONIZER_OBSERVABILITY=on` in its environment exports nothing
after the upgrade, exactly as before — including when it runs somewhere with `OTEL_*` variables
already set for another program. Those variables are read; they are not consent.

To start exporting, do one of:

- save and enable the `observability` module in **Settings → Modules → Observability** (see
  [settings and privacy](settings.md)), or
- set `COLONIZER_OBSERVABILITY=on` in the mothership's environment and name an endpoint in
  `OTEL_EXPORTER_OTLP_ENDPOINT`.

Either way, `GET /api/observability/status` reports `configured: true` and shows the endpoint,
protocol, service name, the header names and where each field came from, so you can see what the
variables won before the first batch goes out.

The one visible change to a configured setup is the service name: a config with no
`OTEL_SERVICE_NAME` now exports as `colonizer-mothership` rather than `colonizer`. Set
`OTEL_SERVICE_NAME` if your backend groups on the old value.