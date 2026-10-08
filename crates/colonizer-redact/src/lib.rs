//! Secret redaction for colony logs (issue #761, part of the fleet epic #691): a credential that an
//! agent echoes, a tool prints or a request carries is replaced with `[REDACTED:<kind>]` before the
//! line is written to `events.jsonl`, `harness.jsonl` or `gateway.jsonl`, and again before a session
//! directory is archived — so neither the disk, an archive bundle nor a fleet export carries it.
//!
//! The detectors are layered, most specific first, and every match becomes a byte span; overlapping
//! spans merge and the kind of the earliest wins:
//!
//! 1. **Provider tokens** by their documented prefixes: GitHub, Anthropic, OpenAI, AWS (and an AWS
//!    secret key sitting next to an access key id), Stripe, Slack.
//! 2. **A rule corpus** of other well-known credential shapes: more token prefixes, PEM private key
//!    blocks, JWTs, `Bearer`/`Basic` credentials and webhook URLs. The rules are written here; the
//!    list of services covered took its ideas from public secret-scanning rule sets.
//! 3. **URIs with inline credentials** (`scheme://user:pass@host`): the password goes, the user and
//!    host stay so the line still says where it pointed.
//! 4. **Connection strings**: the same URI form for database and broker schemes, and the
//!    `Password=…;` / `AccountKey=…;` key-value form, which layer 5 handles.
//! 5. **`KEY=value`, `key: value` and JSON fields** whose key names a secret (`password`, `secret`,
//!    `token`, `api_key`, `private_key`, …), skipping placeholders (`${VAR}`, `<token>`, `****`) and
//!    code (`token: String`, `password = read_password()`).
//! 6. **High-entropy strings** as a last resort: 32 to 1024 characters of the base64/URL-safe
//!    alphabet mixing upper case, lower case and digits, at 4.2 bits per character or more. Hex runs
//!    (git SHAs, sha256 digests), UUIDs, lockfile integrity hashes (`sha512-…`, `h1:…`) and base64
//!    image data are never candidates.
//!
//! The JSON path ([`redact_value`], [`redact_line`]) redacts field by field, so a line stays valid
//! JSON with its shape intact; a line with nothing to redact is returned untouched, byte for byte.
//! Everything is a single linear pass per detector over the bytes, with no backtracking.
//!
//! A crate of its own, depending only on `serde_json`, so the mothership (as `crate::redact`) and
//! the observability add-on share one set of detectors (docs/design/observability.md).

use serde_json::Value;
use std::borrow::Cow;

/// The replacement for one secret of the given kind.
fn mark(kind: &str) -> String {
    format!("[REDACTED:{kind}]")
}

/// A secret's bytes in the input, and what it was.
#[derive(Clone, Copy, Debug)]
struct Span {
    start: usize,
    end: usize,
    kind: &'static str,
}

// ── Public surface ──

/// Every secret in free text replaced with `[REDACTED:<kind>]`. Borrowed when nothing matched.
pub fn redact_text(input: &str) -> Cow<'_, str> {
    redact_text_with(input, true)
}

/// Redacts every string in a JSON value in place, field by field; `true` if anything changed.
pub fn redact_value(value: &mut Value) -> bool {
    walk(value, None)
}

/// One log line: a JSON line is redacted field by field and re-serialised only when something
/// changed; anything else is redacted as text.
pub fn redact_line(line: &str) -> Cow<'_, str> {
    let trimmed = line.trim_start();
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && let Ok(mut value) = serde_json::from_str::<Value>(line)
    {
        return if redact_value(&mut value) {
            Cow::Owned(value.to_string())
        } else {
            Cow::Borrowed(line)
        };
    }
    redact_text(line)
}

/// A whole JSON-lines file (or any line-oriented log): each UTF-8 line through [`redact_line`],
/// line endings kept. A line that is not UTF-8 is kept as it is. Borrowed when nothing matched.
pub fn redact_jsonl(bytes: &[u8]) -> Cow<'_, [u8]> {
    let mut out: Option<Vec<u8>> = None;
    let mut at = 0;
    while at < bytes.len() {
        let end = bytes[at..].iter().position(|&b| b == b'\n').map_or(bytes.len(), |p| at + p);
        if let Ok(line) = std::str::from_utf8(&bytes[at..end])
            && let Cow::Owned(redacted) = redact_line(line)
        {
            let buf = out.get_or_insert_with(|| bytes[..at].to_vec());
            buf.extend_from_slice(redacted.as_bytes());
        } else if let Some(buf) = out.as_mut() {
            buf.extend_from_slice(&bytes[at..end]);
        }
        if end < bytes.len()
            && let Some(buf) = out.as_mut()
        {
            buf.push(b'\n');
        }
        at = end + 1;
    }
    match out {
        Some(buf) => Cow::Owned(buf),
        None => Cow::Borrowed(bytes),
    }
}

// ── JSON ──

/// The `[REDACTED:<kind>]` marks in already-redacted text, counted per kind in first-seen order.
/// How the publish, review and finding paths tell that redaction changed what they are about to
/// send, so the operator hears that a colony exposed a secret instead of it vanishing silently.
pub fn marks(text: &str) -> Vec<(String, usize)> {
    const OPEN: &str = "[REDACTED:";
    let mut out: Vec<(String, usize)> = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(OPEN) {
        rest = &rest[at + OPEN.len()..];
        let Some(end) = rest.find(']') else { break };
        let kind = &rest[..end];
        if !kind.is_empty() && kind.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            match out.iter_mut().find(|(k, _)| k == kind) {
                Some((_, n)) => *n += 1,
                None => out.push((kind.to_string(), 1)),
            }
            rest = &rest[end + 1..];
        }
    }
    out
}

/// Where redaction would change `text`: the 1-based line and the kind of each secret found, in
/// order. Never carries a value, so it is safe to put in a message to the agent that wrote it (the
/// pr.md rewrite nudge, issue #1175). A secret that spans lines (a PEM block) is reported on the
/// line it starts at when that line alone shows it, and otherwise not at all.
pub fn findings(text: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if let Cow::Owned(redacted) = redact_text(line) {
            for (kind, n) in marks(&redacted) {
                out.extend(std::iter::repeat_n((i + 1, kind), n));
            }
        }
    }
    out
}

/// The operator-facing line for a file that redaction changed, e.g. `pr.md contained 1 secret
/// (github token), redacted before publishing`; `None` when `text` carries no mark.
pub fn redaction_note(file: &str, text: &str, before: &str) -> Option<String> {
    let found = marks(text);
    let total: usize = found.iter().map(|(_, n)| n).sum();
    if total == 0 {
        return None;
    }
    let kinds = found
        .iter()
        .map(|(kind, n)| {
            let label = kind.replace('_', " ");
            if *n > 1 { format!("{label} ×{n}") } else { label }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let noun = if total == 1 { "secret" } else { "secrets" };
    Some(format!("{file} contained {total} {noun} ({kinds}), redacted before {before}"))
}

fn walk(value: &mut Value, key: Option<&str>) -> bool {
    match value {
        Value::String(s) => {
            if let Some(k) = key
                && is_secret_key(k)
                && is_secret_value(s, true)
            {
                *s = mark(key_kind(k));
                return true;
            }
            let entropy = key.is_none_or(|k| !is_entropy_exempt_key(k));
            match redact_text_with(s, entropy) {
                Cow::Owned(redacted) => {
                    *s = redacted;
                    true
                }
                Cow::Borrowed(_) => false,
            }
        }
        Value::Array(items) => items.iter_mut().fold(false, |changed, item| walk(item, None) | changed),
        Value::Object(map) => {
            let binary = is_binary_blob(map);
            let mut changed = false;
            for (k, v) in map.iter_mut() {
                if binary && matches!(k.as_str(), "data" | "base64" | "bytes") {
                    continue;
                }
                changed |= walk(v, Some(k));
            }
            changed
        }
        _ => false,
    }
}

/// An inline binary block — an image or document a transcript carries as base64 — whose payload
/// is data, not a credential.
fn is_binary_blob(map: &serde_json::Map<String, Value>) -> bool {
    map.get("type").and_then(Value::as_str) == Some("base64")
        || ["media_type", "mime_type", "mimeType", "content_type"].iter().any(|k| {
            map.get(*k).and_then(Value::as_str).is_some_and(|t| {
                t.starts_with("image/") || t.starts_with("audio/") || t.starts_with("video/") || t == "application/pdf"
            })
        })
}

/// The words that name an identifier or a digest field (`user_id`, `commitSha`, `etag`).
const EXEMPT_WORDS: &[&str] = &[
    "id",
    "ids",
    "uuid",
    "guid",
    "sha",
    "hash",
    "digest",
    "checksum",
    "integrity",
    "signature",
    "etag",
    "fingerprint",
    "commit",
];

/// Identifier words commonly written run together as one lower-case word (`commitsha`), which
/// word splitting cannot take apart. A trailing digit run is dropped before the lookup, so `sha256`,
/// `sha1` and `sha512` count as `sha` without being listed here.
const EXEMPT_COMPOUNDS: &[&str] = &["commitsha", "commithash", "md5"];

/// Words that say a field carries a credential. One anywhere in a key overrides the exemption, so
/// `api_key_id` or `session_cookie_id` keeps the entropy layer.
const SECRET_WORDS: &[&str] = &[
    "secret",
    "secrets",
    "token",
    "tokens",
    "password",
    "passwd",
    "pwd",
    "key",
    "keys",
    "apikey",
    "auth",
    "cookie",
    "cookies",
    "credential",
    "credentials",
    "private",
    "session",
    "bearer",
    "jwt",
];

/// A key split into lower-case words at separators (`_`, `-`, `.`, anything not alphanumeric),
/// camelCase and PascalCase humps (`commitSha`, `HTTPServerID` → `http`, `server`, `id`) and a digit
/// run followed by a letter (`v2Hash` → `v2`, `hash`). A digit run stays on the word it ends, so
/// `sha256` is one word.
fn key_words(key: &str) -> Vec<String> {
    let mut words = Vec::new();
    for part in key.split(|c: char| !c.is_ascii_alphanumeric()) {
        let b = part.as_bytes();
        let mut start = 0;
        for i in 1..b.len() {
            let (prev, cur) = (b[i - 1], b[i]);
            let next_lower = b.get(i + 1).is_some_and(u8::is_ascii_lowercase);
            let boundary = (prev.is_ascii_lowercase() && cur.is_ascii_uppercase())
                || (prev.is_ascii_digit() && cur.is_ascii_alphabetic())
                || (prev.is_ascii_uppercase() && cur.is_ascii_uppercase() && next_lower);
            if boundary {
                words.push(part[start..i].to_ascii_lowercase());
                start = i;
            }
        }
        if start < b.len() {
            words.push(part[start..].to_ascii_lowercase());
        }
    }
    words
}

fn is_secret_word(word: &str) -> bool {
    SECRET_WORDS.contains(&word) || is_secret_key(word)
}

/// Fields that hold identifiers and digests by design, where a long random-looking value is the
/// point and not a secret. Only the entropy layer is skipped; a known token shape is still caught.
///
/// The key's last word, or the whole key run together, must be an identifier word
/// ([`EXEMPT_WORDS`], [`EXEMPT_COMPOUNDS`], with a trailing digit run ignored), so `user_id`,
/// `toolUseId`, `commit_sha`, `sha256` and `etag` are exempt but `did`, `paid`, `valid` and
/// `android` are not. A secret word anywhere in the key ([`SECRET_WORDS`], or a long one run into
/// other letters) wins, so `api_key_id` and `session_cookie_id` are not exempt either.
fn is_entropy_exempt_key(key: &str) -> bool {
    let words = key_words(key);
    let Some(last) = words.last() else {
        return false;
    };
    // A long secret word also counts run into its neighbours (`SECRETV2ID`, `xtokenid`); the short
    // ones (`key`, `pwd`, `jwt`, `auth`) only as whole words, or `monkey_id` would lose its exemption.
    let whole = compact(key);
    if words.iter().any(|w| is_secret_word(w)) || SECRET_WORDS.iter().any(|s| s.len() >= 5 && whole.contains(s)) {
        return false;
    }
    let identifier =
        |w: &str| EXEMPT_COMPOUNDS.contains(&w) || EXEMPT_WORDS.contains(&w.trim_end_matches(|c: char| c.is_ascii_digit()));
    // The whole key as one word too, for a mixed-case spelling the hump rule splits (`ETag`).
    identifier(last) || identifier(&whole)
}

// ── Text ──

fn redact_text_with(input: &str, entropy: bool) -> Cow<'_, str> {
    let bytes = input.as_bytes();
    let mut spans = Vec::new();
    provider_tokens(bytes, &mut spans);
    corpus(bytes, &mut spans);
    uri_credentials(bytes, &mut spans);
    key_values(bytes, &mut spans);
    if entropy {
        high_entropy(bytes, &mut spans);
    }
    apply(input, spans)
}

/// Replaces the merged spans. Every span starts and ends next to an ASCII byte, so both ends are
/// char boundaries; one that somehow is not is dropped rather than split a character.
fn apply(input: &str, mut spans: Vec<Span>) -> Cow<'_, str> {
    spans.retain(|s| s.start < s.end && input.is_char_boundary(s.start) && input.is_char_boundary(s.end));
    spans.retain(|s| !is_documentation_example(s.kind, &input[s.start..s.end]));
    if spans.is_empty() {
        return Cow::Borrowed(input);
    }
    spans.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
    let mut out = String::with_capacity(input.len());
    let mut at = 0;
    let mut current: Option<Span> = None;
    for span in spans {
        match current.as_mut() {
            Some(c) if span.start < c.end => c.end = c.end.max(span.end),
            _ => {
                if let Some(c) = current.take() {
                    out.push_str(&input[at..c.start]);
                    out.push_str(&mark(c.kind));
                    at = c.end;
                }
                current = Some(span);
            }
        }
    }
    if let Some(c) = current {
        out.push_str(&input[at..c.start]);
        out.push_str(&mark(c.kind));
        at = c.end;
    }
    out.push_str(&input[at..]);
    Cow::Owned(out)
}

// ── Published documentation examples ──

/// The credentials AWS publishes in its own documentation. They are public and grant nothing, and
/// agents quote them constantly (a redaction crate's tests, a README), so a span whose text is
/// exactly one of these is not a secret (issue #1175). Matched whole and exactly, the way Gitleaks
/// and trufflehog allowlist them: a real key next to one, or one with anything glued on, still goes.
const DOCUMENTATION_EXAMPLES: &[&str] = &["AKIAIOSFODNN7EXAMPLE", "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"];

/// Whether `text`, a span of `kind`, is a documentation example: one of [`DOCUMENTATION_EXAMPLES`],
/// or an AWS access key id (20 characters) ending in `EXAMPLE`, the shape AWS's other samples take.
fn is_documentation_example(kind: &str, text: &str) -> bool {
    DOCUMENTATION_EXAMPLES.contains(&text) || (kind == "aws_access_key" && text.len() == 20 && text.ends_with("EXAMPLE"))
}

// ── Byte classes ──

fn alnum(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}
fn alnum_us(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}
fn alnum_dash(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-'
}
fn alnum_dash_us(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}
fn alnum_dash_us_dot(b: u8) -> bool {
    alnum_dash_us(b) || b == b'.'
}
fn upper_digit(b: u8) -> bool {
    b.is_ascii_uppercase() || b.is_ascii_digit()
}
fn hex(b: u8) -> bool {
    b.is_ascii_hexdigit()
}
fn base64ish(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'_' | b'-')
}
fn token68(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'~' | b'+' | b'/' | b'=' | b'-')
}

/// True when `i` starts a word: the byte before it cannot continue an identifier or token.
fn word_start(s: &[u8], i: usize) -> bool {
    i == 0 || !(s[i - 1].is_ascii_alphanumeric() || s[i - 1] == b'_' || s[i - 1] == b'-')
}

/// The end of the run of `class` bytes starting at `from`.
fn run(s: &[u8], from: usize, class: fn(u8) -> bool) -> usize {
    s[from..].iter().position(|&b| !class(b)).map_or(s.len(), |p| from + p)
}

fn find(s: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from >= s.len() || needle.is_empty() {
        return None;
    }
    s[from..].windows(needle.len()).position(|w| w == needle).map(|p| from + p)
}

// ── 1 and 2: prefixed tokens ──

/// A token recognised by its prefix: the body after it is a run of `body` bytes of `min..=max`
/// length, and the whole token (prefix included) is redacted.
struct Prefix {
    prefix: &'static str,
    body: fn(u8) -> bool,
    min: usize,
    max: usize,
    kind: &'static str,
}

const fn p(prefix: &'static str, body: fn(u8) -> bool, min: usize, kind: &'static str) -> Prefix {
    Prefix {
        prefix,
        body,
        min,
        max: usize::MAX,
        kind,
    }
}
const fn exact(prefix: &'static str, body: fn(u8) -> bool, len: usize, kind: &'static str) -> Prefix {
    Prefix {
        prefix,
        body,
        min: len,
        max: len,
        kind,
    }
}

/// Layer 1: the providers a colony talks to most, by their documented token prefixes.
const PROVIDERS: &[Prefix] = &[
    p("ghp_", alnum, 30, "github_token"),
    p("gho_", alnum, 30, "github_token"),
    p("ghu_", alnum, 30, "github_token"),
    p("ghs_", alnum, 30, "github_token"),
    p("ghr_", alnum, 30, "github_token"),
    p("github_pat_", alnum_us, 40, "github_token"),
    p("sk-ant-", alnum_dash_us, 20, "anthropic_key"),
    p("sk-proj-", alnum_dash_us, 20, "openai_key"),
    p("sk-svcacct-", alnum_dash_us, 20, "openai_key"),
    p("sk-admin-", alnum_dash_us, 20, "openai_key"),
    p("sk-", alnum, 20, "openai_key"),
    exact("AKIA", upper_digit, 16, "aws_access_key"),
    exact("ASIA", upper_digit, 16, "aws_access_key"),
    exact("ABIA", upper_digit, 16, "aws_access_key"),
    exact("ACCA", upper_digit, 16, "aws_access_key"),
    p(concat!("sk_", "live_"), alnum, 16, "stripe_key"),
    p("sk_test_", alnum, 16, "stripe_key"),
    p("rk_live_", alnum, 16, "stripe_key"),
    p("rk_test_", alnum, 16, "stripe_key"),
    p("whsec_", alnum, 24, "stripe_webhook_secret"),
    p("xoxb-", alnum_dash, 10, "slack_token"),
    p("xoxp-", alnum_dash, 10, "slack_token"),
    p("xoxa-", alnum_dash, 10, "slack_token"),
    p("xoxr-", alnum_dash, 10, "slack_token"),
    p("xoxs-", alnum_dash, 10, "slack_token"),
    p("xoxe-", alnum_dash, 10, "slack_token"),
    p("xapp-", alnum_dash, 10, "slack_token"),
];

/// Layer 2, prefixed half: other services' credential shapes.
const CORPUS_PREFIXES: &[Prefix] = &[
    exact("AIza", alnum_dash_us, 35, "google_api_key"),
    p(concat!("glp", "at-"), alnum_dash_us, 20, "gitlab_token"),
    p("glptt-", alnum_dash_us, 20, "gitlab_token"),
    p("GR1348941", alnum_dash_us, 20, "gitlab_token"),
    exact("npm_", alnum, 36, "npm_token"),
    p("hf_", alnum, 30, "huggingface_token"),
    exact("dop_v1_", hex, 64, "digitalocean_token"),
    exact("doo_v1_", hex, 64, "digitalocean_token"),
    p("pypi-AgEIcHlwaS5vcmc", alnum_dash_us, 50, "pypi_token"),
    exact("shpat_", hex, 32, "shopify_token"),
    exact("shpss_", hex, 32, "shopify_token"),
    exact("shpca_", hex, 32, "shopify_token"),
    p("SG.", alnum_dash_us_dot, 60, "sendgrid_key"),
    p("sq0atp-", alnum_dash_us, 22, "square_token"),
    p("sq0csp-", alnum_dash_us, 40, "square_token"),
    p("lin_api_", alnum, 40, "linear_key"),
    p("dckr_pat_", alnum_dash_us, 27, "docker_token"),
    p("figd_", alnum_dash_us, 40, "figma_token"),
    p("gsk_", alnum, 40, "groq_key"),
    p("xai-", alnum, 40, "xai_key"),
    p("pplx-", alnum, 40, "perplexity_key"),
    p("r8_", alnum, 30, "replicate_token"),
    p("glc_", alnum_dash_us, 32, "grafana_token"),
    p("glsa_", alnum_us, 32, "grafana_token"),
    p("AGE-SECRET-KEY-1", upper_digit, 50, "age_key"),
    p("tskey-", alnum_dash, 20, "tailscale_key"),
    p("vault:v1:", base64ish, 20, "vault_token"),
    p("hvs.", alnum_dash_us, 24, "vault_token"),
];

fn prefixed(s: &[u8], rules: &[Prefix], spans: &mut Vec<Span>) {
    for i in 0..s.len() {
        let first = s[i];
        if !first.is_ascii_alphanumeric() || !word_start(s, i) {
            continue;
        }
        for rule in rules {
            let pre = rule.prefix.as_bytes();
            if pre[0] != first || !s[i..].starts_with(pre) {
                continue;
            }
            let body = i + pre.len();
            let end = run(s, body, rule.body);
            let len = end - body;
            if len >= rule.min && len <= rule.max {
                spans.push(Span {
                    start: i,
                    end,
                    kind: rule.kind,
                });
                break;
            }
        }
    }
}

fn provider_tokens(s: &[u8], spans: &mut Vec<Span>) {
    let before = spans.len();
    prefixed(s, PROVIDERS, spans);
    // An AWS secret access key has no prefix of its own; it is a 40-character base64 run, and the
    // one place it can be told from any other is right next to its access key id.
    let ids: Vec<usize> = spans[before..]
        .iter()
        .filter(|s| s.kind == "aws_access_key")
        .map(|s| s.end)
        .collect();
    for from in ids {
        let window = (from + 256).min(s.len());
        let mut i = from;
        while i < window {
            if !(s[i].is_ascii_alphanumeric() || s[i] == b'+' || s[i] == b'/') {
                i += 1;
                continue;
            }
            let end = run(s, i, |b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/');
            let candidate = &s[i..end];
            if candidate.len() == 40
                && candidate.iter().any(u8::is_ascii_digit)
                && candidate.iter().any(u8::is_ascii_uppercase)
                && candidate.iter().any(u8::is_ascii_lowercase)
            {
                spans.push(Span {
                    start: i,
                    end,
                    kind: "aws_secret_key",
                });
                break;
            }
            i = end;
        }
    }
}

fn corpus(s: &[u8], spans: &mut Vec<Span>) {
    prefixed(s, CORPUS_PREFIXES, spans);
    private_keys(s, spans);
    jwts(s, spans);
    auth_schemes(s, spans);
    webhooks(s, spans);
}

/// PEM private key blocks, header to footer (the whole block, to the end of the text when the
/// footer was cut off).
fn private_keys(s: &[u8], spans: &mut Vec<Span>) {
    let mut from = 0;
    while let Some(begin) = find(s, from, b"-----BEGIN ") {
        let header_end = find(s, begin + 11, b"-----").filter(|&e| e - begin < 80);
        let Some(header_end) = header_end else {
            from = begin + 11;
            continue;
        };
        if !s[begin..header_end].ends_with(b"PRIVATE KEY") && !s[begin..header_end].ends_with(b"PRIVATE KEY BLOCK") {
            from = header_end + 5;
            continue;
        }
        let end = match find(s, header_end + 5, b"-----END ") {
            Some(footer) => find(s, footer + 9, b"-----").map_or(s.len(), |e| e + 5),
            None => s.len(),
        };
        spans.push(Span {
            start: begin,
            end,
            kind: "private_key",
        });
        from = end;
    }
}

/// JSON Web Tokens: three base64url segments, the first two JSON objects (`eyJ…`).
fn jwts(s: &[u8], spans: &mut Vec<Span>) {
    let mut from = 0;
    while let Some(i) = find(s, from, b"eyJ") {
        from = i + 3;
        if !word_start(s, i) {
            continue;
        }
        let end = run(s, i, alnum_dash_us_dot);
        let token = &s[i..end];
        let parts: Vec<&[u8]> = token.split(|&b| b == b'.').collect();
        if parts.len() >= 3 && parts[0].len() >= 10 && parts[1].starts_with(b"eyJ") && parts[1].len() >= 10 {
            spans.push(Span {
                start: i,
                end,
                kind: "jwt",
            });
            from = end;
        }
    }
}

/// `Bearer <token>` and `Basic <credentials>`, as an Authorization header or anywhere in text.
fn auth_schemes(s: &[u8], spans: &mut Vec<Span>) {
    for (scheme, kind, min) in [(&b"bearer"[..], "bearer_token", 16), (&b"basic"[..], "basic_auth", 16)] {
        let mut i = 0;
        while i + scheme.len() < s.len() {
            if s[i..i + scheme.len()].eq_ignore_ascii_case(scheme) && word_start(s, i) && s[i + scheme.len()] == b' ' {
                let start = i + scheme.len() + run(&s[i + scheme.len()..], 0, |b| b == b' ');
                let end = run(s, start, token68);
                let token = &s[start..end];
                if token.len() >= min && token.iter().any(u8::is_ascii_digit) && !token.starts_with(b"[REDACTED") {
                    spans.push(Span { start, end, kind });
                }
                i = end.max(i + 1);
            } else {
                i += 1;
            }
        }
    }
}

/// Webhook URLs whose path is the credential.
fn webhooks(s: &[u8], spans: &mut Vec<Span>) {
    for (prefix, kind) in [
        (&b"hooks.slack.com/services/"[..], "slack_webhook"),
        (&b"hooks.slack.com/workflows/"[..], "slack_webhook"),
        (&b"discord.com/api/webhooks/"[..], "discord_webhook"),
        (&b"discordapp.com/api/webhooks/"[..], "discord_webhook"),
        (&b"outlook.office.com/webhook/"[..], "teams_webhook"),
    ] {
        let mut from = 0;
        while let Some(i) = find(s, from, prefix) {
            let start = i + prefix.len();
            let end = run(s, start, |b| alnum_dash_us(b) || b == b'/' || b == b'@');
            if end - start >= 20 {
                spans.push(Span { start, end, kind });
            }
            from = end.max(start);
        }
    }
}

// ── 3 and 4: URIs with credentials ──

const CONNECTION_SCHEMES: &[&str] = &[
    "postgres",
    "postgresql",
    "mysql",
    "mariadb",
    "mongodb",
    "mongodb+srv",
    "redis",
    "rediss",
    "amqp",
    "amqps",
    "kafka",
    "nats",
    "mssql",
    "sqlserver",
    "clickhouse",
    "cassandra",
    "cockroachdb",
    "jdbc:postgresql",
    "jdbc:mysql",
];

/// `scheme://user:password@host`: the password is redacted, the user and host stay. A userinfo
/// with no password is redacted whole only when it is long enough to be a token.
fn uri_credentials(s: &[u8], spans: &mut Vec<Span>) {
    let mut from = 0;
    while let Some(sep) = find(s, from, b"://") {
        from = sep + 3;
        let mut scheme_start = sep;
        while scheme_start > 0
            && (s[scheme_start - 1].is_ascii_alphanumeric() || matches!(s[scheme_start - 1], b'+' | b'.' | b'-' | b':'))
        {
            scheme_start -= 1;
        }
        if sep - scheme_start < 2 || !s[scheme_start].is_ascii_alphabetic() {
            continue;
        }
        let scheme = String::from_utf8_lossy(&s[scheme_start..sep]).to_ascii_lowercase();
        let authority_start = sep + 3;
        let mut at = None;
        let mut i = authority_start;
        while i < s.len() {
            match s[i] {
                b'@' => at = Some(i),
                b'/' | b'?' | b'#' | b'"' | b'\'' | b'<' | b'>' | b'`' | b'\\' => break,
                b if b.is_ascii_whitespace() => break,
                _ => {}
            }
            i += 1;
        }
        let Some(at) = at else { continue };
        let userinfo = &s[authority_start..at];
        let kind = if CONNECTION_SCHEMES.contains(&scheme.as_str()) {
            "connection_string"
        } else {
            "uri_credentials"
        };
        match userinfo.iter().position(|&b| b == b':') {
            Some(colon) => {
                let password = &userinfo[colon + 1..];
                if let Ok(text) = std::str::from_utf8(password)
                    && is_secret_value(text, true)
                {
                    spans.push(Span {
                        start: authority_start + colon + 1,
                        end: at,
                        kind,
                    });
                }
            }
            None if userinfo.len() >= 16 => {
                if let Ok(text) = std::str::from_utf8(userinfo)
                    && is_secret_value(text, true)
                {
                    spans.push(Span {
                        start: authority_start,
                        end: at,
                        kind,
                    });
                }
            }
            None => {}
        }
        from = at + 1;
    }
}

// ── 5: KEY=value ──

/// A key name, reduced to lower-case letters and digits (`DB_PASSWORD` → `dbpassword`).
fn compact(key: &str) -> String {
    key.bytes()
        .filter(u8::is_ascii_alphanumeric)
        .map(|b| b.to_ascii_lowercase() as char)
        .collect()
}

const SECRET_KEY_SUFFIXES: &[&str] = &[
    "password",
    "passwd",
    "passphrase",
    "secret",
    "token",
    "apikey",
    "secretkey",
    "privatekey",
    "accesskey",
    "accountkey",
    "sharedaccesskey",
    "masterkey",
    "encryptionkey",
    "signingkey",
    "sessionkey",
    "authkey",
    "authorization",
    "credential",
    "credentials",
    "cookie",
];

/// Whether a field or variable named `key` holds a secret.
fn is_secret_key(key: &str) -> bool {
    let k = compact(key);
    k == "pwd" || SECRET_KEY_SUFFIXES.iter().any(|suffix| k.ends_with(suffix))
}

fn key_kind(key: &str) -> &'static str {
    let k = compact(key);
    if k.contains("pass") || k == "pwd" {
        "password"
    } else if k.ends_with("token") {
        "token"
    } else if k.ends_with("privatekey") {
        "private_key"
    } else if k.ends_with("key") {
        "api_key"
    } else if k.ends_with("authorization") {
        "authorization"
    } else if k.contains("credential") {
        "credential"
    } else if k.ends_with("cookie") {
        "cookie"
    } else {
        "secret"
    }
}

/// Whether a value next to a secret-named key is a literal worth redacting, rather than a
/// placeholder, a path, a number or — unquoted — a piece of code.
fn is_secret_value(value: &str, quoted: bool) -> bool {
    let v = value.trim();
    if v.len() < if quoted { 3 } else { 4 } || v.starts_with("[REDACTED") {
        return false;
    }
    let lower = v.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "null" | "none" | "nil" | "undefined" | "true" | "false" | "redacted" | "bearer" | "basic" | "token"
    ) || v.starts_with(['$', '<', '%', '*', '&', '{'])
        || v.starts_with("~/")
        || v.starts_with("./")
        || v.starts_with('/')
        || lower.starts_with("file:")
        || lower.starts_with("env:")
        || v.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        || v.bytes().all(|b| b == v.as_bytes()[0])
    {
        return false;
    }
    if !quoted {
        // `token: String`, `password = read_password()`, `api_key=config.api_key`: code, not a value.
        if v.contains('(') || v.contains('[') || v.contains('<') || v.ends_with(':') {
            return false;
        }
        // Letters only, dotted or hyphenated: `config.api_key`, `status?.github`, `owner-only`.
        let identifier_path = v.split(['.', ':']).filter(|part| !part.is_empty()).all(|part| {
            part.bytes().next().is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphabetic() || matches!(b, b'_' | b'-' | b'?' | b'!'))
        });
        if identifier_path {
            return false;
        }
        // A comparison between two identifiers (`session>=read`, `repo>=launch`): code, not a value.
        if v.split_once(">=").is_some_and(|(a, b)| {
            let operand = |p: &str| {
                !p.is_empty()
                    && p.bytes().next().is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
                    && p.bytes()
                        .all(|c| c.is_ascii_alphabetic() || matches!(c, b'_' | b'-' | b'?' | b'!' | b'>'))
            };
            operand(a) && operand(b)
        }) {
            return false;
        }
    }
    true
}

/// `key=value`, `key: value`, `"key": "value"` and `key := value` in free text, with the key
/// naming a secret. Only the value is redacted.
fn key_values(s: &[u8], spans: &mut Vec<Span>) {
    for i in 0..s.len() {
        let sep = s[i];
        if sep != b'=' && sep != b':' {
            continue;
        }
        let next = s.get(i + 1).copied();
        let prev = if i > 0 { Some(s[i - 1]) } else { None };
        if sep == b'=' && (matches!(next, Some(b'=' | b'>')) || matches!(prev, Some(b'=' | b'!' | b'<' | b'>' | b':'))) {
            continue;
        }
        if sep == b':' && (next == Some(b':') || prev == Some(b':') || next == Some(b'/')) {
            continue;
        }
        // The key: back over spaces and an optional closing quote to a run of name bytes.
        let mut j = i;
        while j > 0 && s[j - 1] == b' ' && i - j < 2 {
            j -= 1;
        }
        // A quoted key sits right against its separator (`"key": …`); `"a" : "b"` is a ternary.
        let key_quote = (j > 0 && matches!(s[j - 1], b'"' | b'\'')).then(|| s[j - 1]);
        if key_quote.is_some() && j < i {
            continue;
        }
        if key_quote.is_some() {
            j -= 1;
        }
        let key_end = j;
        while j > 0 && (s[j - 1].is_ascii_alphanumeric() || matches!(s[j - 1], b'_' | b'-' | b'.')) {
            j -= 1;
        }
        // A quoted key opens with the same quote, or the "key" is the last word of some string.
        if j == key_end || key_quote.is_some_and(|q| j == 0 || s[j - 1] != q) {
            continue;
        }
        let Ok(key) = std::str::from_utf8(&s[j..key_end]) else {
            continue;
        };
        if !is_secret_key(key) {
            continue;
        }
        // The value: past `:=` and spaces, then a quoted string or a run up to a delimiter.
        let mut k = i + 1;
        if sep == b':' && next == Some(b'=') {
            k += 1;
        }
        while k < s.len() && (s[k] == b' ' || s[k] == b'\t') {
            k += 1;
        }
        if k >= s.len() {
            continue;
        }
        let (start, end, quoted) = if matches!(s[k], b'"' | b'\'') {
            let quote = s[k];
            let mut e = k + 1;
            while e < s.len() && s[e] != quote && s[e] != b'\n' {
                if s[e] == b'\\' {
                    e += 1;
                }
                e += 1;
            }
            if e >= s.len() || s[e] != quote {
                continue;
            }
            (k + 1, e, true)
        } else {
            let e = s[k..]
                .iter()
                .position(|&b| {
                    b.is_ascii_whitespace() || matches!(b, b',' | b';' | b'&' | b')' | b'}' | b']' | b'"' | b'\'' | b'`')
                })
                .map_or(s.len(), |p| k + p);
            (k, e, false)
        };
        let Ok(value) = std::str::from_utf8(&s[start..end]) else {
            continue;
        };
        if is_secret_value(value, quoted) {
            spans.push(Span {
                start,
                end,
                kind: key_kind(key),
            });
        }
    }
}

// ── 6: entropy ──

const ENTROPY_MIN_LEN: usize = 32;
const ENTROPY_MAX_LEN: usize = 1024;
const ENTROPY_BITS: f64 = 4.2;

/// Shannon entropy in bits per byte.
fn shannon(bytes: &[u8]) -> f64 {
    let mut counts = [0u32; 256];
    for &b in bytes {
        counts[b as usize] += 1;
    }
    let n = bytes.len() as f64;
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / n;
            -p * p.log2()
        })
        .sum()
}

fn is_uuid(c: &[u8]) -> bool {
    c.len() == 36
        && c.iter().enumerate().all(|(i, &b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

/// The last resort, so it only claims bytes no other layer has: a run that overlaps an earlier
/// span is left to that span's kind. `=` is base64 padding, so it may only end a run; that keeps a
/// `KEY=value` from reading as one long token.
fn high_entropy(s: &[u8], spans: &mut Vec<Span>) {
    let taken: Vec<(usize, usize)> = spans.iter().map(|sp| (sp.start, sp.end)).collect();
    let body = |b: u8| base64ish(b) && b != b'=';
    let mut i = 0;
    while i < s.len() {
        if !body(s[i]) {
            i += 1;
            continue;
        }
        let mut end = run(s, i, body);
        while end < s.len() && s[end] == b'=' && end - i < ENTROPY_MAX_LEN {
            end += 1;
        }
        if is_entropy_secret(s, i, end) && !taken.iter().any(|&(a, b)| a < end && i < b) {
            spans.push(Span {
                start: i,
                end,
                kind: "high_entropy",
            });
        }
        i = end;
    }
}

fn is_entropy_secret(s: &[u8], start: usize, end: usize) -> bool {
    let c = &s[start..end];
    if c.len() < ENTROPY_MIN_LEN || c.len() > ENTROPY_MAX_LEN {
        return false;
    }
    // Lockfile integrity (`sha512-…`, go.sum `h1:…`) and data URIs (`;base64,…`).
    let before = &s[start.saturating_sub(8)..start];
    if ["sha1-", "sha256-", "sha384-", "sha512-"]
        .iter()
        .any(|p| c.starts_with(p.as_bytes()))
        || before.ends_with(b"h1:")
        || before.ends_with(b"base64,")
        || before.ends_with(b"sha256:")
        || before.ends_with(b"sha512:")
    {
        return false;
    }
    // Base64 image data by its magic bytes: PNG, JPEG, GIF, WebP, SVG.
    if ["iVBORw0KGgo", "/9j/", "R0lGOD", "UklGR", "PHN2Zy", "PD94bWwg"]
        .iter()
        .any(|m| c.starts_with(m.as_bytes()))
    {
        return false;
    }
    // Git SHAs, digests and UUIDs: hex with or without dashes.
    if is_uuid(c) || c.iter().all(|&b| b.is_ascii_hexdigit() || b == b'-') {
        return false;
    }
    if !(c.iter().any(u8::is_ascii_digit) && c.iter().any(u8::is_ascii_uppercase) && c.iter().any(u8::is_ascii_lowercase)) {
        return false;
    }
    // A path (`src/Foo/Bar2Baz/…`) has words between its slashes; random base64 rarely does.
    if c.contains(&b'/')
        && c.split(|&b| b == b'/')
            .filter(|seg| seg.len() >= 3 && seg.iter().all(u8::is_ascii_lowercase))
            .count()
            >= 2
    {
        return false;
    }
    // An alphabet or a counting run (`ABCD…xyz0123…`) is maximally varied and not a secret.
    let ascending = c.windows(2).filter(|w| w[1] == w[0] + 1).count();
    if ascending * 2 > c.len() {
        return false;
    }
    shannon(c) >= ENTROPY_BITS
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Issue #1175: AWS's documented examples are public, so quoting them is not a leak.
    #[test]
    fn the_aws_documentation_examples_are_not_secrets() {
        for text in [
            "the id is AKIAIOSFODNN7EXAMPLE",
            "key wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            "AKIAIOSFODNN7EXAMPLE wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            "aws_secret_access_key = wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            "AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE",
            "an id ending EXAMPLE: AKIAI44QH8DHBEXAMPLE",
            "temporary ASIAIOSFODNN7EXAMPLE",
        ] {
            assert_eq!(redact_text(text), text, "{text}");
        }
        assert_eq!(
            redaction_note("pr.md", &redact_text("AKIAIOSFODNN7EXAMPLE"), "publishing"),
            None
        );
    }

    /// The allowlist is exact: a lookalike, a longer token, or a real key beside an example still goes.
    #[test]
    fn the_allowlist_is_exact_and_a_real_key_beside_an_example_still_goes() {
        let real = concat!("AKIA", "Y34FZKBOKMUTVV7A");
        assert_eq!(redact_text(real), "[REDACTED:aws_access_key]");
        // One character off, or glued to a longer run: not the example.
        assert_eq!(redact_text(concat!("AKIA", "IOSFODNN7EXAMPLF")), "[REDACTED:aws_access_key]");
        // An example id does not excuse the secret key that follows it.
        let pair = concat!("AKIAIOSFODNN7EXAMPLE wJalrXUtnFEMI/K7MDENG/", "bPxRfiCYEXAMPLEKEZ");
        assert_eq!(redact_text(pair), "AKIAIOSFODNN7EXAMPLE [REDACTED:aws_secret_key]");
        let both = format!("{real} and AKIAIOSFODNN7EXAMPLE");
        assert_eq!(redact_text(&both), "[REDACTED:aws_access_key] and AKIAIOSFODNN7EXAMPLE");
    }

    #[test]
    fn findings_name_the_line_and_kind_never_the_value() {
        let secret = "ghp_aB3dE5gH7jK9mN1pQ3sT5vX7zA9cE1gH3jK5";
        let text = format!("# Title\n\nfine\nthe token was {secret}\nAKIAIOSFODNN7EXAMPLE");
        let found = findings(&text);
        assert_eq!(found, vec![(4, "github_token".to_string())]);
        assert!(!format!("{found:?}").contains(secret));
        assert!(findings("nothing here").is_empty());
    }

    #[test]
    fn a_redaction_note_names_the_count_and_the_kinds() {
        let text = "a [REDACTED:github_token] b [REDACTED:aws_access_key] c [REDACTED:github_token]";
        assert_eq!(
            redaction_note("pr.md", text, "publishing").as_deref(),
            Some("pr.md contained 3 secrets (github token ×2, aws access key), redacted before publishing")
        );
        assert_eq!(
            redaction_note(
                "pr.md",
                &redact_text("GH=ghp_aB3dE5gH7jK9mN1pQ3sT5vX7zA9cE1gH3jK5"),
                "publishing"
            )
            .as_deref(),
            Some("pr.md contained 1 secret (github token), redacted before publishing")
        );
        assert_eq!(redaction_note("pr.md", "nothing [REDACTED: here] or [x]", "publishing"), None);
    }

    fn r(s: &str) -> String {
        redact_text(s).into_owned()
    }

    /// Asserts `secret` is gone from `input` once redacted, and a mark of `kind` stands in its place.
    fn check(table: &[(&str, &str, &str)]) {
        for (input, secret, kind) in table {
            let out = r(input);
            assert!(!out.contains(secret), "{kind}: {secret:?} survived in {out:?}");
            assert!(
                out.contains(&format!("[REDACTED:{kind}]")),
                "{kind}: expected its mark in {out:?}"
            );
        }
    }

    #[test]
    fn provider_tokens_are_redacted_by_prefix() {
        check(&[
            (
                "export GH=ghp_aB3dE5gH7jK9mN1pQ3sT5vX7zA9cE1gH3jK5",
                "ghp_aB3dE5gH7jK9mN1pQ3sT5vX7zA9cE1gH3jK5",
                "github_token",
            ),
            (
                "remote: gho_16C7e42F292c6912E7710c838347Ae178B4a ok",
                "gho_16C7e42F292c6912E7710c838347Ae178B4a",
                "github_token",
            ),
            (
                "token github_pat_11ABCDEFG0123456789_abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOP",
                "github_pat_11ABCDEFG0123456789_abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOP",
                "github_token",
            ),
            (
                "key sk-ant-api03-AbCdEf123456_GhIjKl-789012MnOpQr",
                "sk-ant-api03-AbCdEf123456_GhIjKl-789012MnOpQr",
                "anthropic_key",
            ),
            (
                "OPENAI sk-proj-Ab12Cd34Ef56Gh78Ij90Kl12_mn-op",
                "sk-proj-Ab12Cd34Ef56Gh78Ij90Kl12_mn-op",
                "openai_key",
            ),
            (
                "legacy sk-Ab12Cd34Ef56Gh78Ij90Kl12Mn34Op56",
                "sk-Ab12Cd34Ef56Gh78Ij90Kl12Mn34Op56",
                "openai_key",
            ),
            (
                concat!("id AKIA", "IOSFODNN7EXAMPLF here"),
                concat!("AKIA", "IOSFODNN7EXAMPLF"),
                "aws_access_key",
            ),
            (
                concat!("sts ASIA", "Y34FZKBOKMUTVV7A"),
                concat!("ASIA", "Y34FZKBOKMUTVV7A"),
                "aws_access_key",
            ),
            (
                concat!("AKIA", "IOSFODNN7EXAMPLF wJalrXUtnFEMI/K7MDENG/", "bPxRfiCYEXAMPLEKEZ"),
                concat!("wJalrXUtnFEMI/K7MDENG/", "bPxRfiCYEXAMPLEKEZ"),
                "aws_secret_key",
            ),
            (
                concat!("stripe sk_", "live_4eC39HqLyjWDarjtT1zdp7dc"),
                concat!("sk_", "live_4eC39HqLyjWDarjtT1zdp7dc"),
                "stripe_key",
            ),
            (
                "restricted rk_live_51H8abcDEFghiJKLmno",
                "rk_live_51H8abcDEFghiJKLmno",
                "stripe_key",
            ),
            (
                "slack xoxb-123456789012-1234567890123-AbCdEfGhIjKl",
                "xoxb-123456789012-1234567890123-AbCdEfGhIjKl",
                "slack_token",
            ),
        ]);
    }

    #[test]
    fn the_rule_corpus_catches_other_credential_shapes() {
        check(&[
            (
                concat!("maps AIza", "SyA1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q"),
                concat!("AIza", "SyA1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q"),
                "google_api_key",
            ),
            (
                concat!("gl glp", "at-xYz123AbC456dEf789gH"),
                concat!("glp", "at-xYz123AbC456dEf789gH"),
                "gitlab_token",
            ),
            (
                "npm npm_abcdefghijklmnopqrstuvwxyz0123456789",
                "npm_abcdefghijklmnopqrstuvwxyz0123456789",
                "npm_token",
            ),
            (
                "hf hf_AbCdEfGhIjKlMnOpQrStUvWxYz01234567",
                "hf_AbCdEfGhIjKlMnOpQrStUvWxYz01234567",
                "huggingface_token",
            ),
            (
                "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA1b2c3\n-----END RSA PRIVATE KEY-----\nafter",
                "MIIEpAIBAAKCAQEA1b2c3",
                "private_key",
            ),
            (
                "jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U",
                "eyJzdWIiOiIxMjM0NTY3ODkwIn0",
                "jwt",
            ),
            (
                "Authorization: Bearer abc123def456ghi789jkl",
                "abc123def456ghi789jkl",
                "bearer_token",
            ),
            (
                "Authorization: Basic dXNlcjpwYXNzd29yZDEyMw==",
                "dXNlcjpwYXNzd29yZDEyMw==",
                "basic_auth",
            ),
            (
                concat!(
                    "post https://hooks.slack",
                    ".com/services/T00000000/B00000000/XXXXXXXXXXXXXXXXXXXXXXXX"
                ),
                "T00000000/B00000000/XXXXXXXXXXXXXXXXXXXXXXXX",
                "slack_webhook",
            ),
        ]);
    }

    #[test]
    fn inline_uri_credentials_keep_the_user_and_host() {
        check(&[
            (
                "clone https://nick:s3cretPass@example.com/repo.git",
                "s3cretPass",
                "uri_credentials",
            ),
            ("proxy http://u:p4ssw0rd!@10.0.0.1:3128 up", "p4ssw0rd!", "uri_credentials"),
        ]);
        assert_eq!(
            r("https://nick:s3cretPass@example.com/x"),
            "https://nick:[REDACTED:uri_credentials]@example.com/x"
        );
    }

    #[test]
    fn database_and_broker_connection_strings() {
        check(&[
            (
                "DATABASE_URL is postgres://app:hunter22@db:5432/chi",
                "hunter22",
                "connection_string",
            ),
            (
                "mongodb+srv://admin:Zx9!qw@cluster0.mongodb.net/db",
                "Zx9!qw",
                "connection_string",
            ),
            ("redis://:r3dispass@cache:6379/0", "r3dispass", "connection_string"),
            ("amqp://guest:rabbitPw1@mq:5672/", "rabbitPw1", "connection_string"),
            (
                "Server=db;User Id=sa;Password=Sup3r$ecret;Database=x",
                "Sup3r$ecret",
                "password",
            ),
            (
                "DefaultEndpointsProtocol=https;AccountName=acct;AccountKey=Zm9vYmFyYmF6cXV4MTIz==;",
                "Zm9vYmFyYmF6cXV4MTIz==",
                "api_key",
            ),
        ]);
    }

    #[test]
    fn secret_named_keys_and_fields() {
        check(&[
            ("DB_PASSWORD=hunter22 next", "hunter22", "password"),
            ("export API_KEY='abc-def-123'", "abc-def-123", "api_key"),
            ("client_secret: \"s0me-client-secret\"", "s0me-client-secret", "secret"),
            (r#"{"access_token": "opaque-9f8e7d"}"#, "opaque-9f8e7d", "token"),
            ("private_key = \"notreally\"", "notreally", "private_key"),
            ("GET /cb?code=1&token=abcd1234efgh HTTP/1.1", "abcd1234efgh", "token"),
            ("secret := \"gopherPass9\"", "gopherPass9", "secret"),
        ]);
        let mut v = json!({"user": "nick", "password": "hunter", "nested": {"apiKey": "k-123-abc"}, "tokens": 12});
        assert!(redact_value(&mut v));
        assert_eq!(
            v,
            json!({"user": "nick", "password": "[REDACTED:password]", "nested": {"apiKey": "[REDACTED:api_key]"}, "tokens": 12})
        );
    }

    #[test]
    fn high_entropy_strings_are_a_last_resort() {
        check(&[
            (
                "opaque Xk9pL2mQ8vR4tY7wZ1aB3cD5eF6gH0jK here",
                "Xk9pL2mQ8vR4tY7wZ1aB3cD5eF6gH0jK",
                "high_entropy",
            ),
            (
                "cookie=\"x\" ; val q7W2e9R4t1Y8u3I6o0P5aS2dF7gH4jK1lZ9x",
                "q7W2e9R4t1Y8u3I6o0P5aS2dF7gH4jK1lZ9x",
                "high_entropy",
            ),
        ]);
    }

    #[test]
    fn identifiers_digests_images_and_code_are_left_alone() {
        let untouched = [
            "commit 3f786850e387550fdab836ed7e6dc881de23001b merged",
            "sha256 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
            "session 550e8400-e29b-41d4-a716-446655440000 started",
            "integrity sha512-z4PhNX7vuL3xVChQ1m2AB9Yg5AULVxXcg/SpIdNs6c5H0NE8XYXysP+DGNKHfuwvY7kxvUR8wJhL0DXn4E4d5g==",
            "github.com/pkg/errors v0.9.1 h1:FEBLx1zS214owpjy7qsBeixbURkuhQAwrK5UwLGTwt4=",
            "![x](data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==)",
            "The quick brown fox jumps over the lazy dog, and the password policy is documented.",
            "fn login(password: &str, token: String) -> Result<Token, Error> { let api_key = config.api_key; }",
            "let token = read_token()?; if password == expected { return Ok(()) }",
            "see crates/colonizer/src/sessions/runtime.rs:120 for the ThisIsAVeryLongCamelCaseIdentifier",
            "PWD=/home/nick/work max_tokens=4096 input_tokens: 1200 token: ${GITHUB_TOKEN}",
            "https://github.com/Colonizer-dev/harness/pull/761 and ssh://git@github.com/x.git",
            "task-runner sk-short and ghp_short",
            "password: [REDACTED:password] already done",
            r#"label={auth === "bearer" ? "Token" : "API key"} and the token is a secret: owner-only"#,
            "const B64: &[u8] = b\"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/\";",
            "tailscale up --auth-key=file:/colonizer/mesh-authkey",
            "/api/sessions/{id}/publish  POST  unauth=401  token=session>=launch  activity=colony.publish",
            "token=session>=read unauth=401 activity=sessions.read",
            "token=session>=operate token=response>=read",
            "token=repo>=launch token=colony>=operate",
        ];
        for line in untouched {
            assert_eq!(r(line), line, "left alone");
        }
        let mut image = json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "Xk9pL2mQ8vR4tY7wZ1aB3cD5eF6gH0jKXk9pL2mQ8vR4tY7w"}});
        let before = image.clone();
        assert!(!redact_value(&mut image));
        assert_eq!(image, before);
        let mut ids =
            json!({"id": "toolu_01Xk9pL2mQ8vR4tY7wZ1aB3cD5e", "signature": "EqoBCkgIBxABGAIiQXk9pL2mQ8vR4tY7wZ1aB3cD5eF6gH0j"});
        assert!(
            !redact_value(&mut ids),
            "identifier and signature fields skip the entropy layer: {ids}"
        );
    }

    #[test]
    fn a_comparison_suffix_does_not_exempt_a_credential() {
        check(&[
            ("token=abcdefghabcdefghabcdefgh==", "abcdefghabcdefghabcdefgh==", "token"),
            ("token=hunter22>=read", "hunter22>=read", "token"),
            // The key rule wins over the AWS prefix here, and it takes `>=read` with it.
            ("token=AKIAIOSFODNN7EXAMPLF>=read", "AKIAIOSFODNN7EXAMPLF>=read", "token"),
            ("token=dXNlcjpwYXNzd29yZA==>=read", "dXNlcjpwYXNzd29yZA==>=read", "token"),
        ]);
    }

    #[test]
    fn the_entropy_exemption_matches_whole_words_only() {
        let table = [
            // Genuine identifiers and digests, in every key style.
            ("id", true),
            ("ID", true),
            ("user_id", true),
            ("userId", true),
            ("UserID", true),
            ("user-id", true),
            ("colonizer.record.id", true),
            ("gen_ai.tool.call.id", true),
            ("tool_call_id", true),
            ("toolUseIds", true),
            ("uuid", true),
            ("request_guid", true),
            ("commit_sha", true),
            ("commitSha", true),
            ("commitsha", true),
            ("head.commit", true),
            ("sha256", true),
            ("file_sha1", true),
            ("blobSHA256", true),
            ("content-hash", true),
            ("md5", true),
            ("etag", true),
            ("ETag", true),
            ("integrity", true),
            ("signature", true),
            ("cert.fingerprint", true),
            ("payload_digest", true),
            ("checksum", true),
            // A word that merely ends in an identifier word is not one.
            ("did", false),
            ("paid", false),
            ("valid", false),
            ("android", false),
            ("avoid", false),
            ("rehash_count", false),
            ("smash", false),
            ("monkey_id", true),
            ("keyboard.id", true),
            ("user_id_note", false),
            ("", false),
            ("_", false),
            // A secret word anywhere wins over an identifier ending.
            ("api_key_id", false),
            ("apiKeyId", false),
            ("session_cookie_id", false),
            ("session-id", false),
            ("sessionId", false),
            ("auth.token.sha", false),
            ("private_key_fingerprint", false),
            ("bearerHash", false),
            ("jwt_signature", false),
            ("password_hash", false),
            ("PWD_DIGEST", false),
            ("credentials.id", false),
            ("secretKeyId", false),
            ("access_token_id", false),
        ];
        for (key, exempt) in table {
            assert_eq!(is_entropy_exempt_key(key), exempt, "{key:?} → {:?}", key_words(key));
        }
    }

    #[test]
    fn a_random_value_is_kept_only_under_an_identifier_key() {
        let random = concat!("Xk9pL2mQ8vR4tY7wZ1aB3cD5", "eF6gH0jKq7W2e9R4");
        assert_eq!(random.len(), 40);
        for key in ["did", "paid", "api_key_id", "session_cookie_id"] {
            let mut field = json!({ key: random });
            assert!(redact_value(&mut field), "{key} keeps a random value: {field}");
            assert!(!field.to_string().contains(random), "{key}: {field}");
        }
        for key in ["id", "user_id", "commit_sha", "etag"] {
            let mut field = json!({ key: random });
            assert!(!redact_value(&mut field), "{key} redacts an identifier: {field}");
            assert_eq!(field[key], random);
        }
    }

    /// Every key built from identifier words and filler words, in every style, with a secret word
    /// somewhere in it, keeps the entropy layer.
    #[test]
    fn no_key_with_a_secret_word_is_exempt() {
        let fillers = ["", "user", "tool", "v2", "Request"];
        let joins: [fn(&[&str]) -> String; 4] = [
            |w| w.join("_"),
            |w| w.join("-"),
            |w| w.join("."),
            |w| {
                w.iter()
                    .enumerate()
                    .map(|(i, x)| {
                        let mut c = x.chars();
                        match (i, c.next()) {
                            (0, Some(f)) => f.to_ascii_lowercase().to_string() + c.as_str(),
                            (_, Some(f)) => f.to_ascii_uppercase().to_string() + c.as_str(),
                            (_, None) => String::new(),
                        }
                    })
                    .collect()
            },
        ];
        let mut checked = 0;
        for secret in SECRET_WORDS {
            for filler in fillers {
                for suffix in EXEMPT_WORDS.iter().chain(EXEMPT_COMPOUNDS) {
                    for words in [
                        vec![*secret, *suffix],
                        vec![filler, secret, suffix],
                        vec![secret, filler, suffix],
                        vec![*secret],
                    ] {
                        let words: Vec<&str> = words.into_iter().filter(|w| !w.is_empty()).collect();
                        for join in joins {
                            let key = join(&words);
                            assert!(!is_entropy_exempt_key(&key), "{key:?} is exempt");
                            // SCREAMING_CASE, where a separator still marks the words (an
                            // upper-cased camelCase key has no word boundaries left to find).
                            if words.len() == 1 || !key.bytes().all(|b| b.is_ascii_alphanumeric()) {
                                let upper = key.to_ascii_uppercase();
                                assert!(!is_entropy_exempt_key(&upper), "{upper:?} is exempt");
                            }
                            checked += 1;
                        }
                    }
                }
            }
        }
        assert!(checked > 1000);
    }

    #[test]
    fn redaction_is_idempotent_and_keeps_json_valid() {
        let line = r#"{"seq":3,"type":"tool_result","text":"export OPENAI_API_KEY=sk-proj-Ab12Cd34Ef56Gh78Ij90Kl12_mn-op\nok"}"#;
        let once = redact_line(line).into_owned();
        let v: Value = serde_json::from_str(&once).expect("still JSON");
        assert_eq!(v["seq"], 3);
        assert!(v["text"].as_str().unwrap().contains("[REDACTED:"), "{once}");
        assert_eq!(
            redact_line(&once),
            once.as_str(),
            "a redacted line has nothing left to redact"
        );
        let clean = r#"{"seq":1,"type":"status","state":"working"}"#;
        assert!(
            matches!(redact_line(clean), Cow::Borrowed(_)),
            "a clean line is not re-serialised"
        );
    }

    #[test]
    fn a_jsonl_file_is_redacted_line_by_line() {
        let file = b"{\"a\":\"ghp_aB3dE5gH7jK9mN1pQ3sT5vX7zA9cE1gH3jK5\"}\nplain line\n\xff\xfe\n";
        let out = redact_jsonl(file);
        let text = String::from_utf8_lossy(&out);
        assert!(
            text.starts_with("{\"a\":\"[REDACTED:github_token]\"}\nplain line\n"),
            "{text}"
        );
        assert!(out.ends_with(b"\xff\xfe\n"), "a non-UTF-8 line is kept as it was");
        assert!(matches!(redact_jsonl(b"{\"a\":1}\n"), Cow::Borrowed(_)));
    }

    /// A megabyte of realistic log in well under a second, even in a debug build's worst case the
    /// bound stays loose enough not to flake; release runs it in tens of milliseconds.
    #[test]
    fn a_megabyte_of_log_redacts_quickly() {
        let line = r#"{"seq":12,"type":"tool_result","origin":"agent","text":"Compiling colonizer v0.1.10 (crates/colonizer)\n  commit 3f786850e387550fdab836ed7e6dc881de23001b, id 550e8400-e29b-41d4-a716-446655440000, token: ${TOKEN}"}"#;
        let mut log = String::new();
        while log.len() < 1 << 20 {
            log.push_str(line);
            log.push('\n');
        }
        log.push_str(r#"{"text":"leaked ghp_aB3dE5gH7jK9mN1pQ3sT5vX7zA9cE1gH3jK5"}"#);
        let started = std::time::Instant::now();
        let out = redact_jsonl(log.as_bytes());
        let elapsed = started.elapsed();
        assert!(String::from_utf8_lossy(&out).ends_with(r#"{"text":"leaked [REDACTED:github_token]"}"#));
        let bound = if cfg!(debug_assertions) { 20 } else { 1 };
        assert!(elapsed.as_secs() < bound, "1 MiB took {elapsed:?}");
    }
}
