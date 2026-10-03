# Colonizer decisions

A record of the things Colonizer has decided against or deferred, so the reasoning is not lost. An
entry here is not a vow: each one states what would change it.

---

## ChatGPT subscriptions are not a Colonizer credential

Decided 2026-09-17 · status: stands ([#30](https://github.com/Colonizer-dev/harness/issues/30))

Colonizer will not support using a ChatGPT subscription as a colony credential. OpenAI-compatible
providers take an API key. Revisit if the conditions at the end change.

### It is a different API, not a different credential

ChatGPT sign-in tokens — what the Codex CLI's sign-in flow obtains — are honoured by
`chatgpt.com/backend-api/codex/responses`, the Responses API, not by `api.openai.com/v1/chat/completions`.
The gateway's `openai` wire translates the Anthropic Messages API to Chat Completions only, and only for
`/v1/messages` (`crates/colonizer/src/openai.rs:22-26`, a translator of about 1,100 lines). Supporting a
ChatGPT plan means a second translator, Anthropic Messages to Responses API items and events. That is a
parallel project, not a follow-up. The provider catalog carried a `codex` preset until this entry; with
`wire: "openai"` it sent Chat Completions requests to an endpoint that only answers the Responses API, so
it could not work.

### It is not a legal refusal

OpenAI's current Terms of Use (effective 1 January 2026) contain no clause reserving ChatGPT access to
OpenAI's own interfaces. The nearest clauses prohibit programmatically extracting data or Output, and
reverse engineering — neither is aimed at a user driving their own subscription. The reports that
third-party harnesses reusing the Codex OAuth client were cut off do not hold up on inspection: the one
first-hand report ([openai/codex#14215](https://github.com/openai/codex/issues/14215)) is an HTTP 403
whose error code is `unsupported_country_region_territory`, with no statement from OpenAI. Meanwhile
OpenAI documents [`codex app-server`](https://developers.openai.com/codex/app-server) as a supported way
to embed Codex in a third-party product, Codex owning the ChatGPT OAuth flow, and third-party tools ship
ChatGPT-plan sign-in (Roo Code, January 2026; OpenClaw, announced by OpenAI's CEO in May 2026).

What has no documented path is rolling your own OAuth client against Codex's hardcoded `client_id`;
there is no registration programme for that. Unclear and unsupported, then, not prohibited. The decision
rests on the engineering argument above, not on this.

### The Claude analogy is mechanically wrong

Claude subscription auth never touches the gateway. `claude_login.rs` drives `claude setup-token` on the
mothership; the token is injected as a microsandbox `--secret` scoped to `api.anthropic.com`
(`crates/colonizer/src/boot.rs:1562-1566`, `crates/colonizer/src/sandbox.rs:62-67`); the in-colony
router forwards unrouted models to Anthropic with Claude Code's own headers
(`modules/agents/claude-code/router.mjs:178-179`). The gateway only ever attaches a provider key, as
`x-api-key` or `bearer` (`crates/colonizer/src/gateway.rs:1207-1213`, `credential_header`). Mothership-side judging holds to
the same rule ([#143](https://github.com/Colonizer-dev/harness/issues/143)): the `autonomy` module
sends a model to the provider configured for it and spends that provider's key — a plain id resolves
only where a configured provider sits on Anthropic's API — and the mothership's Claude login is
never spent answering a colony's questions.

### If it is ever revisited

[`modules/agents/codex`](../modules/agents/codex) now drives `codex exec` headlessly on an OpenAI API
key (a `CODEX_API_KEY` colony secret, not a plan). For ChatGPT-plan access the shape is unchanged: an
agent module that bundles `codex app-server` and owns its own sign-in — not a new credential class in
the gateway. Conditions that would change the decision:

- OpenAI documents third-party access to ChatGPT plans, or opens OAuth client registration; or
- the gateway gains a Responses API wire for other reasons.

---

## No OpenTelemetry SDK in the export pipeline

Decided 2026-10-03 · status: stands ([#839](https://github.com/Colonizer-dev/harness/issues/839))

The observability add-on (#840–#865) speaks OTLP as a wire format only. It encodes
`ExportLogsServiceRequest`/`ExportTraceServiceRequest`/`ExportMetricsServiceRequest` itself from
records it derives from the mothership's own jsonl files, and POSTs them — no `TracerProvider`, no
`SpanProcessor`, no in-process exporter registry, and nothing instrumented with `#[instrument]`
anywhere in the tree. Full dependency and transport tables:
[design/observability.md § No OpenTelemetry SDK in the export pipeline](design/observability.md#no-opentelemetry-sdk-in-the-export-pipeline).

### Why

Spans here are reconstructed after the fact from files that already carry everything causality needs
(`agent_ref` for subagents, `tool_call_id` for tool calls, ordering for turns); nothing is live-emitted,
so there is no running pipeline for an SDK to drive. An SDK pipeline would also pull
`opentelemetry`/`opentelemetry_sdk`/`tonic` into `colonizer-harness`'s own dependency tree — exactly what
the add-on's separate-crate packaging exists to avoid (see the entry below). The CI gate is
`cargo tree -p colonizer-harness -e normal` containing none of `opentelemetry`, `opentelemetry_sdk`,
`opentelemetry-proto`, `prost`, `tonic`, `flate2`.

### If it is ever revisited

- a future signal (e.g. live, in-process trace propagation across the gateway) needs span context to
  cross process boundaries while a colony runs, which derived-after-the-fact spans cannot give it; or
- `opentelemetry-proto`'s hand-rolled encode path becomes a maintenance burden an SDK's own exporter
  would remove.

---

## Conversation content is per-org opt-in

Decided 2026-10-03 · status: stands ([#839](https://github.com/Colonizer-dev/harness/issues/839))

Prompts, completions, tool arguments/results, question/answer text and other content-tier fields are
exported only when the install switch `conversation_content` is on, the colony's org has itself set
`settings.observability.content = true` in `orgs.json`, and the colony's sensitivity is known and not
`Restricted`. All three are required; any one missing or unrecognised fails closed. Full rule as P3/P4:
[design/observability.md § Conversation content is per-org opt-in](design/observability.md#conversation-content-is-per-org-opt-in).

### Why not inherit from the install switch

`effective_deja_enabled` (`crates/colonizer/src/orgs.rs:625`) is the repo's existing pattern for an
org-level switch that defaults to whatever the install chose. Content export deliberately does not
follow it: deja recall only ever affects the colony itself, while content export sends an org's words
to a backend outside the machine. An install switch turning every org's content on by default would
make the install owner's choice bind orgs that never agreed to it.

### If it is ever revisited

- an org-management feature needs a fleet-wide or install-wide content default or for
  provisioning at scale, with its own explicit confirmation step per org; or
- `orgs.json` grows a general inheritance mechanism that this is folded into rather than special-cased.

---

## No native Loki connector

Decided 2026-10-03 · status: stands ([#839](https://github.com/Colonizer-dev/harness/issues/839),
deferred to [#865](https://github.com/Colonizer-dev/harness/issues/865))

The observability add-on speaks OTLP only. A Loki push-API connector is not built now: the `file`
provider, plus running Alloy, Vector or Fluent Bit over those files (or over the OTLP/JSON file sink),
already gets data into Loki without the harness carrying a second wire format. Details and what else is
deferred alongside it: [design/observability.md § No native Loki
connector](design/observability.md#no-native-loki-connector).

### Why

A native Loki connector would be a second export protocol maintained alongside OTLP/HTTP for one
backend, when OTLP already reaches Loki through a one-hop collector most operators running Loki already
have reason to run (for retention, multi-tenancy, or fan-out).

### If it is ever revisited

- a measured case shows the collector hop meaningfully hurting latency or memory on a
  resource-constrained host;
- enough operators ask for one-binary Loki push without a collector in front; or
- Loki adopts OTLP ingestion natively widely enough that "native connector" stops meaning "a second
  wire format" at all.

---

## The exporter is a downloadable add-on, not part of the mothership

Decided 2026-10-03 · status: stands ([#839](https://github.com/Colonizer-dev/harness/issues/839))

The observability exporter — tailer, id/span mapping, OTLP transports, metrics rendering, backfill,
preview/offline-export — ships as its own crate and binary (`colonizer-observability`), built,
checksummed and attested in its own per-platform tarball alongside every release, not compiled into the
mothership binary or included in the default install bundle. The mothership downloads, verifies and
supervises it as a child process over a versioned JSON contract. Full packaging, install, supervision
and contract design: [design/observability.md § The exporter is a downloadable add-on, not part of the
mothership](design/observability.md#the-exporter-is-a-downloadable-add-on-not-part-of-the-mothership).

### Why

Install size and attack surface: no OTel/protobuf/gRPC code paths ship in an install that never turns
this on. Crash isolation: a malformed OTLP response or a runaway encode cannot take the mothership down
with it. The cost — a second release artifact, a version handshake at spawn, and preview/test needing
the add-on installed — was accepted in preference to a Cargo feature (one-size release binaries), a
dynamic library plugin (no stable Rust ABI across a mothership upgrade), or a WASM module (no
filesystem/network access without a host shim, which is most of the work anyway).

### If it is ever revisited

- release binary size stops being a concern operators raise, removing the main reason to keep it
  separate; or
- the version-handshake/contract overhead between mothership and add-on proves harder to keep in sync
  than compiling the exporter in would have been.
