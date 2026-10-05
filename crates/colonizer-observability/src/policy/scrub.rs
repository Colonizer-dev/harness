//! What happens to every exported string, in order: redaction (again — P7), image and base64
//! payloads replaced by a placeholder (P8), and the byte cap with its truncation marker (P10).

use serde_json::Value;
use std::borrow::Cow;

/// A base64 run at least this long is a payload, not text: an image, a file, an archive. The
/// redactor's entropy layer stops at 1024 characters and lets image data through by design, so
/// this catches both.
pub(crate) const BLOB_MIN_CHARS: usize = 256;

/// `s` capped at `cap` bytes: cut on a character boundary and ended with `…(N more bytes)`, N being
/// the bytes cut, the whole result (marker included) at most `cap` bytes. Borrowed when it fits.
pub fn truncate(s: &str, cap: usize) -> Cow<'_, str> {
    if s.len() <= cap {
        return Cow::Borrowed(s);
    }
    // The marker is sized for the most bytes it could ever name, so the real one never runs over.
    let room = cap.saturating_sub(marker(s.len()).len());
    let cut = floor_char_boundary(s, room);
    let marked = format!("{}{}", &s[..cut], marker(s.len() - cut));
    if marked.len() <= cap {
        Cow::Owned(marked)
    } else {
        // A cap too small for any marker: the characters that fit, unmarked.
        Cow::Owned(s[..floor_char_boundary(s, cap)].to_string())
    }
}

fn marker(cut: usize) -> String {
    format!("…({cut} more bytes)")
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Every image or base64 payload in free text — a `data:` URL, or a long bare base64 run —
/// replaced with [`placeholder`]. Borrowed when there is none.
pub(crate) fn blobs(s: &str) -> Cow<'_, str> {
    let b = s.as_bytes();
    let mut out = String::new();
    let mut copied = 0;
    let mut i = 0;
    while i < b.len() {
        if let Some((end, mime, data)) = data_url(b, i) {
            out.push_str(&s[copied..i]);
            out.push_str(&placeholder(Some(mime), data));
            copied = end;
            i = end;
            continue;
        }
        if base64_byte(b[i]) && (i == 0 || !base64_byte(b[i - 1])) {
            let end = run_end(b, i);
            if is_blob(&s[i..end]) {
                out.push_str(&s[copied..i]);
                out.push_str(&placeholder(None, &s[i..end]));
                copied = end;
            }
            i = end.max(i + 1);
            continue;
        }
        i += 1;
    }
    if copied == 0 {
        return Cow::Borrowed(s);
    }
    out.push_str(&s[copied..]);
    Cow::Owned(out)
}

/// A `data:<mime>[;param…];base64,<data>` URL starting at `i`: where it ends, its mime type and
/// its base64 payload.
fn data_url(b: &[u8], i: usize) -> Option<(usize, &str, &str)> {
    let rest = b.get(i..)?;
    if rest.len() < 5 || !rest[..5].eq_ignore_ascii_case(b"data:") {
        return None;
    }
    let header_end = i + 5 + rest[5..].iter().take(128).position(|&c| c == b',')?;
    let header = std::str::from_utf8(&b[i + 5..header_end]).ok()?;
    let (mime, params) = header.split_once(';')?;
    if !params.split(';').any(|p| p.eq_ignore_ascii_case("base64")) || !is_mime(mime) {
        return None;
    }
    let start = header_end + 1;
    let end = run_end(b, start);
    let data = std::str::from_utf8(&b[start..end]).ok()?;
    Some((end, mime, data))
}

fn is_mime(m: &str) -> bool {
    let Some((kind, sub)) = m.split_once('/') else { return false };
    !kind.is_empty()
        && !sub.is_empty()
        && m.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'/' | b'.' | b'+' | b'-'))
}

/// Standard and URL-safe base64 alphabets together.
fn base64_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'+' | b'/' | b'-' | b'_')
}

/// The end of a base64 run from `i`, its `=` padding included.
fn run_end(b: &[u8], i: usize) -> usize {
    let mut end = i;
    while end < b.len() && base64_byte(b[end]) {
        end += 1;
    }
    let body = end;
    while end < b.len() && end - body < 2 && b[end] == b'=' {
        end += 1;
    }
    end
}

/// A run long and varied enough to be encoded data rather than words, digits or a path.
fn is_blob(run: &str) -> bool {
    let c = run.trim_end_matches('=').as_bytes();
    if c.len() < BLOB_MIN_CHARS {
        return false;
    }
    if !(c.iter().any(u8::is_ascii_digit) && c.iter().any(u8::is_ascii_uppercase) && c.iter().any(u8::is_ascii_lowercase)) {
        return false;
    }
    // A long path (`src/Foo/bar_baz/…`) has words between its separators; base64 rarely does.
    let words = c
        .split(|&b| matches!(b, b'/' | b'-' | b'_'))
        .filter(|seg| seg.len() >= 3 && seg.iter().all(u8::is_ascii_lowercase))
        .count();
    words < 2
}

/// The text standing in for a payload: `[image <mime> <n> bytes]` for a declared image, `[blob
/// <mime> <n> bytes]` for anything else, `n` its decoded size. The mime is the one declared, else
/// `application/octet-stream`: P8 needs the payload's kind and size, not a guess at its format.
pub fn placeholder(declared: Option<&str>, base64: &str) -> String {
    let mime = declared.map_or_else(|| "application/octet-stream".to_string(), str::to_ascii_lowercase);
    let kind = if mime.starts_with("image/") { "image" } else { "blob" };
    format!("[{kind} {mime} {} bytes]", decoded_len(base64))
}

/// The bytes a base64 string decodes to, from its length alone.
fn decoded_len(base64: &str) -> usize {
    let chars = base64.trim_end_matches('=').bytes().filter(|&c| base64_byte(c)).count();
    chars * 3 / 4
}

/// Payloads in a JSON value replaced in place: the `data` of an inline binary block (Anthropic's
/// `{"type":"image","source":{"type":"base64","media_type":…,"data":…}}`, or any object naming an
/// image, audio, video or PDF media type) becomes [`placeholder`], and every string has its
/// `data:` URLs and long base64 runs replaced.
pub(crate) fn json_blobs(value: &mut Value) {
    match value {
        Value::String(s) => {
            if let Cow::Owned(replaced) = blobs(s) {
                *s = replaced;
            }
        }
        Value::Array(items) => items.iter_mut().for_each(json_blobs),
        Value::Object(map) => {
            let declared = ["media_type", "mime_type", "mimeType", "content_type"]
                .iter()
                .find_map(|k| map.get(*k).and_then(Value::as_str))
                .map(str::to_string);
            let binary = map.get("type").and_then(Value::as_str) == Some("base64")
                || declared
                    .as_deref()
                    .is_some_and(|t| ["image/", "audio/", "video/"].iter().any(|p| t.starts_with(p)) || t == "application/pdf");
            for (k, v) in map.iter_mut() {
                if binary && matches!(k.as_str(), "data" | "base64" | "bytes") {
                    if let Value::String(data) = v {
                        *data = placeholder(declared.as_deref(), data);
                    }
                    continue;
                }
                json_blobs(v);
            }
        }
        _ => {}
    }
}
