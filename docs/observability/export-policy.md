# The export policy and OTLP encoding

The foundation of the observability add-on (issue #844), in `crates/colonizer-observability`: the
OTLP message types and their encoding, size-bounded batching, the export policy every exported
string goes through, the hash key behind `repo_names = hashed`, and the secret-canary test kit. It
is a library of its own that nothing links yet; the readers and mappers that will fill it come
later. The mothership never links it, and CI checks that `colonizer-harness` pulls in none of
OpenTelemetry, prost, tonic or flate2. This note is for maintainers. The rules it implements are
P2–P10 in the [design](../design/observability.md#privacy).

## The policy is the only way in

A mapper never fills an OTLP message itself. It asks a `Policy` for a builder — `log(source)`,
`span(kind, subject)` or `metric(source, name, unit)` — and gives it values, each with a tier
(`Structure` or `Content`). The builder applies these steps to every value, in order:

1. **Allowlist.** A key that is not on that record kind's allowlist is dropped. That is a mapper
   bug, so a debug build panics on it (`debug_assert!`). The allowlists are the design's source
   inventory and span tree, plus the keys every record shares (`colonizer.record.id`,
   `colonizer.colony.id`, `colonizer.org`, `colonizer.repo`, …) and `colonizer.truncated`, which
   the policy sets itself. A key ending in `.*` (`model_usage.*`) allows any name under that
   prefix, but only name characters, at most 64 of them. Keys are sent as they are, so a key under
   a prefix that the redactor would change is dropped with its value.
2. **Tier.** A value is content if either the caller or the allowlist says so. For example,
   `activity.detail`, findings' `title` and `reason`, issue and PR titles, and the events `path`
   are content whatever the caller passes. Content is dropped unless the content gate allows it.
   The gate is always closed until its conditions are wired in. Content is never a span or
   metric-point attribute, whatever the gate says (P5).
3. **Names.** Under `repo_names = hashed`, a repository, org or path is replaced by its hash, and
   a branch, a URL naming the repository (PR, issue and duplicate links) or a title is dropped. An
   `invoke_agent` span's name carries its repository, so that is hashed too. If there is no hash
   key, these values are dropped, never sent in plain. The activity log's `target` is hashed too,
   because a `workspace.*` entry names its org there. An event name and the subject in any other
   span's name are not hashed, so a mapper must give them only fixed names (a tool, a model, a
   step), never a repository, org, branch or path.
4. **Scrub.** Image and base64 payloads become a placeholder. Then the string is redacted again
   with `colonizer-redact` (P7). An attribute is redacted as a JSON field of that name, as it is on
   disk. A secret-named key such as `password` loses its value. An identifier key such as
   `tool_call_id` or `colonizer.record.id` skips only the high-entropy check, so a long provider id
   stays joinable while a known token shape is still caught. Then the string is capped: attributes
   and span names at `max_attribute_bytes`, log bodies at `max_content_bytes`.

A structured body goes through the same steps field by field (`redact_value`), and its object keys
are redacted too. If its compact JSON is over the cap, it is sent as truncated JSON text instead.

### Truncation

A string over its cap is cut on a character boundary and ends with `…(N more bytes)`, where N is
the number of bytes cut. The whole result, marker included, is never longer than the cap. The
record then carries `colonizer.truncated = true`.

### Placeholders

A payload is replaced by `[image <mime> <n> bytes]` if it is declared as an image, or by
`[blob <mime> <n> bytes]` otherwise. `<n>` is the decoded size. The mime is the one declared, or
`application/octet-stream` if there is none; the policy does not guess a format from the bytes.
These count as payloads:

- a `data:<mime>;base64,` URL;
- the `data`, `base64` or `bytes` field of an inline binary block, such as an Anthropic image
  block, or any object naming an image, audio, video or PDF media type;
- any bare base64 run of 256 characters or more that mixes upper case, lower case and digits and is
  not a word-like path.

## Hash key

`repo_names = hashed` hashes names with a key kept at `<data>/observability/hash.key`: 32 random
bytes, mode 0600. The key is created on first use. A key file that others can read, such as a
copy made at 0644, is narrowed to 0600 when it is loaded, and is refused if that fails. It is written to a private file first and then
linked into place, so two processes starting at once agree on one key. After that it is reused. A
fleet copies this file so that its members' hashes agree. A key file of the wrong length is an
error and is never replaced, because replacing it would change every hash a backend already holds.
A name hashes to the first 12 bytes of HMAC-SHA256(key, name), written as 24 lowercase hex
characters.

## Batching

A `Batcher` packs items into requests, one signal per request. Every request carries the single
resource and the scope `colonizer` at the crate version. A request holds at most `max_items`
records (default 2000). Its encoded size is at most `max_request_bytes` (default 1 MiB, never more
than 4 MiB) in the chosen encoding, protobuf or JSON.

Sizes are computed as items are added: exactly for JSON, and for protobuf to within the growth of
two length prefixes. Each built request is checked again and halved if it is over. A record too big
for a request on its own has its longest strings cut, with the same marker and flag. If it still
cannot fit, it is dropped and counted in `oversized`.

## Encoding

Requests encode as OTLP protobuf or OTLP/JSON, and either can be gzipped. The JSON follows the
OTLP mapping:

- lowerCamelCase field names;
- lowercase hex trace and span ids;
- 64-bit integers, including `asInt` points, as strings;
- enums as integers;
- absent messages omitted rather than `null`.

`opentelemetry-proto`'s `with-serde` does most of this; `encode.rs` fixes the last two. The goldens
in `crates/colonizer-observability/tests/fixtures/otlp/` pin the output, with the scope's version
written as `<crate version>` so that a release does not change them. To regenerate them after an
intended change, run `UPDATE_GOLDEN=1 cargo test -p colonizer-observability json_matches_the_golden`.

## The secret-canary kit

`testkit.rs` (tests only) builds a fresh fake credential for each redactor detector kind at run
time, so no fake secret ever appears in the source. The kinds are GitHub, Anthropic, OpenAI, an
AWS key pair, Stripe, Slack, a PEM block, a JWT, Bearer, Basic, a URI with a password, a postgres
connection string, `PASSWORD=`, a high-entropy string, and an `observability-headers` value. It
also builds a content canary.

`assert_absent` takes exported bytes, gunzips them if needed, and parses them as OTLP/JSON or as
any of the three protobuf requests. It then looks for each secret in every string, and in the raw
bytes, in these forms: as is, percent-encoded, and base64 (standard and URL-safe) at every
alignment.

The acceptance tests use the kit to put every canary in every string position of logs, spans,
metric points and the resource. They then check that no canary survives in protobuf, JSON or gzip
of either, and that the `[REDACTED:<kind>]` marks are there instead.
