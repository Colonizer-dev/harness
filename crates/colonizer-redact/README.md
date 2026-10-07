<p align="center">
  <a href="https://crates.io/crates/colonizer-redact"><img src="https://img.shields.io/crates/v/colonizer-redact.svg?style=flat-square&labelColor=0A0A0B&color=FF6B35" alt="colonizer-redact on crates.io"></a>
  <a href="https://docs.rs/colonizer-redact"><img src="https://img.shields.io/docsrs/colonizer-redact?style=flat-square&labelColor=0A0A0B&color=EDEBE6" alt="docs.rs"></a>
  <a href="https://github.com/Colonizer-dev/harness/blob/main/LICENSE"><img src="https://img.shields.io/badge/LICENSE-MIT-FF6B35?style=flat-square&labelColor=0A0A0B" alt="MIT"></a>
</p>

# `colonizer-redact`

Secret redaction for [Colonizer](https://colonizer.dev) logs.

Coding agents print things. A token an agent echoes, a tool prints or a request carries gets
replaced with `[REDACTED:<kind>]` before the line reaches disk, an archive or an export. The
Colonizer mothership ([`colonizer-harness`](https://crates.io/crates/colonizer-harness)) and its
observability add-on share this one set of detectors. It depends only on `serde_json`, so you can
use it in any Rust program that writes logs.

## What it catches

Detectors are layered, most specific first. Overlapping matches merge.

1. **Provider tokens** by their documented prefixes: GitHub, Anthropic, OpenAI, AWS, Stripe, Slack.
2. **Other well-known credential shapes:** more token prefixes, PEM private keys, JWTs,
   `Bearer`/`Basic` credentials, webhook URLs.
3. **URIs with inline credentials** (`scheme://user:pass@host`): the password goes, the user and
   host stay.
4. **Connection strings**, in URI form and as `Password=…;` / `AccountKey=…;`.
5. **`KEY=value`, `key: value` and JSON fields** whose key names a secret (`password`, `token`,
   `api_key`, …). Placeholders (`${VAR}`, `<token>`) and code (`password = read_password()`)
   are left alone.
6. **High-entropy strings** as a last resort. Git SHAs, digests, UUIDs, lockfile hashes and
   base64 images are never candidates.

JSON lines are redacted field by field, so a line stays valid JSON with its shape intact. A line
with nothing to redact comes back untouched, byte for byte. Each detector is a single linear pass
with no backtracking.

## Use

```toml
[dependencies]
colonizer-redact = "0.2"
```

```rust
use colonizer_redact::{redact_line, redact_text};

// Free text: each secret becomes [REDACTED:<kind>].
let clean = redact_text(&agent_output);

// One JSON-lines log line; borrowed (no copy) when nothing matched.
let line = redact_line(&log_line);
```

| Function | What it does |
|---|---|
| `redact_text` | Free text |
| `redact_line` | One log line: JSON field by field, otherwise as text |
| `redact_jsonl` | A whole JSON-lines file, line endings kept |
| `redact_value` | A `serde_json::Value`, in place |
| `marks` | Counts the `[REDACTED:<kind>]` marks in already-redacted text |
| `redaction_note` | A one-line note for the operator when redaction changed a file |

## Licence

MIT. Part of [Colonizer-dev/harness](https://github.com/Colonizer-dev/harness).
