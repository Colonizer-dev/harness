//! Chat images, stored on the Mothership so a conversation keeps them: regenerate, edit and resend,
//! branch and compare send them to the model again, and the thread shows them after a reload.
//!
//! Each image is stored once, content-addressed, as `<data>/chats/attachments/<sha256>.<ext>` (0600):
//! the same picture pasted into two conversations is one file. Only PNG, JPEG, GIF and WebP are
//! accepted, recognised by their leading bytes, never by the name or the declared type. Metadata
//! that can carry a location is dropped before the image is hashed and stored: JPEG APP segments
//! other than JFIF, ICC and Adobe (EXIF keeps only its orientation), comments and anything after
//! the image; PNG text, time and EXIF chunks; WebP EXIF and XMP chunks.
//!
//! A message refers to an image by `{kind:"image", sha, mime, width, height, bytes}`. Deleting a
//! conversation removes the images no remaining message refers to (see [`collect_garbage`]).

use crate::{ApiResult, App, Shared, client_error};
use axum::{
    Json,
    body::{Body, Bytes},
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::Response,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    path::PathBuf,
    time::{Duration, SystemTime},
};

/// One image, after its metadata is stripped.
pub const MAX_BYTES: usize = 10 * 1024 * 1024;
/// Images on one message.
pub const MAX_PER_MESSAGE: usize = 8;
/// The upload body: a raw image, or one as base64 in JSON with room for the wrapper.
pub const UPLOAD_BODY_LIMIT: usize = MAX_BYTES / 3 * 4 + 64 * 1024;
/// The Anthropic API refuses larger images; a stored one over this is shown but not sent.
pub const MODEL_IMAGE_LIMIT: u64 = 5 * 1024 * 1024;
/// An upload no message took up within this long is swept with the next deletion.
const ORPHAN_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// What a message records of an image.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ImageRef {
    pub sha: String,
    pub mime: String,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
}

pub(crate) fn dir(app: &App) -> PathBuf {
    app.cfg.data_dir.join("chats").join("attachments")
}

/// A stored image's name: 64 lowercase hex characters, nothing that could be a path.
pub fn valid_sha(sha: &str) -> bool {
    sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn ext(mime: &str) -> Option<&'static str> {
    match mime {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        _ => None,
    }
}

const EXTS: [(&str, &str); 4] = [
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
];

/// The image type from the leading bytes, or `None` when they are not one we accept. Pure, for the tests.
pub fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn le24(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at + 3)?;
    Some(s[0] as u32 | (s[1] as u32) << 8 | (s[2] as u32) << 16)
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// Width and height from the image's header. Pure, for the tests.
pub fn dimensions(mime: &str, b: &[u8]) -> Option<(u32, u32)> {
    let (w, h) = match mime {
        "image/png" => {
            if b.get(12..16)? != b"IHDR" {
                return None;
            }
            (be32(b, 16)?, be32(b, 20)?)
        }
        "image/gif" => (le16(b, 6)? as u32, le16(b, 8)? as u32),
        "image/jpeg" => jpeg_dimensions(b)?,
        "image/webp" => webp_dimensions(b)?,
        _ => return None,
    };
    (w > 0 && h > 0).then_some((w, h))
}

fn jpeg_dimensions(b: &[u8]) -> Option<(u32, u32)> {
    let mut at = 2;
    while at + 4 <= b.len() {
        if b[at] != 0xFF {
            return None;
        }
        let marker = b[at + 1];
        if marker == 0xFF {
            at += 1;
            continue;
        }
        if (0xD0..=0xD9).contains(&marker) || marker == 0x01 {
            at += 2;
            continue;
        }
        let len = be16(b, at + 2)? as usize;
        let is_sof = (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
        if is_sof {
            return Some((be16(b, at + 7)? as u32, be16(b, at + 5)? as u32));
        }
        if marker == 0xDA || len < 2 {
            return None;
        }
        at += 2 + len;
    }
    None
}

fn webp_dimensions(b: &[u8]) -> Option<(u32, u32)> {
    let kind = b.get(12..16)?;
    let data = 20;
    match kind {
        b"VP8 " => {
            (b.get(data + 3..data + 6)? == [0x9D, 0x01, 0x2A]).then_some(())?;
            Some(((le16(b, data + 6)? & 0x3FFF) as u32, (le16(b, data + 8)? & 0x3FFF) as u32))
        }
        b"VP8L" => {
            (*b.get(data)? == 0x2F).then_some(())?;
            let bits = le32(b, data + 1)?;
            Some(((bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1))
        }
        b"VP8X" => Some((le24(b, data + 4)? + 1, le24(b, data + 7)? + 1)),
        _ => None,
    }
}

/// The EXIF orientation (tag 0x0112) in an APP1 payload that starts `Exif\0\0`, when it is set.
fn exif_orientation(payload: &[u8]) -> Option<u16> {
    let tiff = payload.strip_prefix(b"Exif\0\0")?;
    let little = match tiff.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let r16 = |at: usize| if little { le16(tiff, at) } else { be16(tiff, at) };
    let r32 = |at: usize| if little { le32(tiff, at) } else { be32(tiff, at) };
    let ifd = r32(4)? as usize;
    let count = r16(ifd)? as usize;
    (0..count.min(512)).find_map(|i| {
        let entry = ifd + 2 + i * 12;
        (r16(entry)? == 0x0112 && r16(entry + 2)? == 3)
            .then(|| r16(entry + 8))
            .flatten()
    })
}

/// An APP1 segment holding only an EXIF orientation, so a phone photo still displays upright.
fn orientation_segment(orientation: u16) -> Vec<u8> {
    let mut tiff = Vec::with_capacity(26);
    tiff.extend_from_slice(b"MM\0\x2A");
    tiff.extend_from_slice(&8u32.to_be_bytes());
    tiff.extend_from_slice(&1u16.to_be_bytes());
    tiff.extend_from_slice(&0x0112u16.to_be_bytes());
    tiff.extend_from_slice(&3u16.to_be_bytes());
    tiff.extend_from_slice(&1u32.to_be_bytes());
    tiff.extend_from_slice(&orientation.to_be_bytes());
    tiff.extend_from_slice(&[0, 0]);
    tiff.extend_from_slice(&0u32.to_be_bytes());
    let mut seg = vec![0xFF, 0xE1];
    seg.extend_from_slice(&((2 + 6 + tiff.len()) as u16).to_be_bytes());
    seg.extend_from_slice(b"Exif\0\0");
    seg.extend_from_slice(&tiff);
    seg
}

/// The JPEG without metadata: only the JFIF (APP0), ICC (APP2) and Adobe (APP14) segments are kept
/// of the application segments, EXIF is cut to its orientation, comments go, and so does anything
/// after the end of the image (the thumbnails a camera appends). `None` when it does not parse.
fn strip_jpeg(b: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(b.len());
    out.extend_from_slice(&b[..2]);
    let mut at = 2;
    loop {
        // Between segments: a marker, possibly after fill bytes.
        while b.get(at) == Some(&0xFF) && b.get(at + 1) == Some(&0xFF) {
            at += 1;
        }
        if *b.get(at)? != 0xFF {
            return None;
        }
        let marker = *b.get(at + 1)?;
        if marker == 0xD9 {
            out.extend_from_slice(&[0xFF, 0xD9]);
            return Some(out);
        }
        if (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            out.extend_from_slice(&b[at..at + 2]);
            at += 2;
            continue;
        }
        let len = be16(b, at + 2)? as usize;
        if len < 2 {
            return None;
        }
        let end = at + 2 + len;
        let segment = b.get(at..end)?;
        let payload = &segment[4..];
        match marker {
            0xE1 => {
                if let Some(o) = exif_orientation(payload).filter(|o| (2..=8).contains(o)) {
                    out.extend_from_slice(&orientation_segment(o));
                }
            }
            0xE2 if payload.starts_with(b"MPF\0") => {}
            0xE0 | 0xE2 | 0xEE => out.extend_from_slice(segment),
            0xE3..=0xEF | 0xFE => {}
            _ => out.extend_from_slice(segment),
        }
        at = end;
        if marker == 0xDA {
            // Entropy-coded data runs to the next marker that is not a stuffed 0xFF00 or a restart.
            let start = at;
            loop {
                let byte = *b.get(at)?;
                if byte == 0xFF {
                    let next = *b.get(at + 1)?;
                    if next != 0x00 && !(0xD0..=0xD7).contains(&next) {
                        break;
                    }
                    at += 2;
                } else {
                    at += 1;
                }
            }
            out.extend_from_slice(&b[start..at]);
        }
    }
}

/// The PNG without text, time or EXIF chunks and anything after `IEND`. `None` when it does not parse.
fn strip_png(b: &[u8]) -> Option<Vec<u8>> {
    let mut out = b[..8].to_vec();
    let mut at = 8;
    loop {
        let len = be32(b, at)? as usize;
        let end = at.checked_add(12)?.checked_add(len)?;
        let chunk = b.get(at..end)?;
        let kind = &chunk[4..8];
        if !matches!(kind, b"tEXt" | b"zTXt" | b"iTXt" | b"eXIf" | b"tIME") {
            out.extend_from_slice(chunk);
        }
        if kind == b"IEND" {
            return Some(out);
        }
        at = end;
    }
}

/// The WebP without its EXIF and XMP chunks, the extended header's flags for them cleared.
fn strip_webp(b: &[u8]) -> Option<Vec<u8>> {
    let riff_end = (le32(b, 4)? as usize).checked_add(8)?.min(b.len());
    let mut out = b[..12].to_vec();
    let mut at = 12;
    while at + 8 <= riff_end {
        let len = le32(b, at + 4)? as usize;
        let end = at.checked_add(8)?.checked_add(len + (len & 1))?.min(riff_end);
        let chunk = b.get(at..end)?;
        match &chunk[..4] {
            b"EXIF" | b"XMP " => {}
            b"VP8X" if chunk.len() > 8 => {
                let mut c = chunk.to_vec();
                c[8] &= !(0x08 | 0x04);
                out.extend_from_slice(&c);
            }
            _ => out.extend_from_slice(chunk),
        }
        at = end;
    }
    let size = (out.len() - 8) as u32;
    out[4..8].copy_from_slice(&size.to_le_bytes());
    Some(out)
}

/// Checks and cleans an uploaded image: its real type, size, dimensions, and metadata stripped.
/// Answers the bytes to store and what a message records. Pure, for the tests.
pub fn prepare(bytes: &[u8]) -> Result<(Vec<u8>, ImageRef), String> {
    if bytes.is_empty() {
        return Err("an empty image".into());
    }
    if bytes.len() > MAX_BYTES {
        return Err(format!("an image is over {} MB", MAX_BYTES / 1024 / 1024));
    }
    let mime = sniff(bytes).ok_or("not a PNG, JPEG, GIF or WebP image")?;
    let clean = match mime {
        "image/jpeg" => strip_jpeg(bytes),
        "image/png" => strip_png(bytes),
        "image/webp" => strip_webp(bytes),
        _ => Some(bytes.to_vec()),
    }
    .ok_or_else(|| format!("the {} image is damaged", &mime[6..]))?;
    let (width, height) = dimensions(mime, &clean).ok_or_else(|| format!("the {} image has no readable size", &mime[6..]))?;
    let sha = crate::util::hex(ring::digest::digest(&ring::digest::SHA256, &clean).as_ref());
    let image = ImageRef {
        sha,
        mime: mime.to_string(),
        width,
        height,
        bytes: clean.len() as u64,
    };
    Ok((clean, image))
}

/// Stores an image (once: an identical one already stored is kept) and answers its reference.
pub async fn store(app: &App, bytes: &[u8]) -> Result<ImageRef, crate::AppError> {
    let (clean, image) = prepare(bytes).map_err(|e| client_error(StatusCode::UNSUPPORTED_MEDIA_TYPE, &e))?;
    let dir = dir(app);
    let path = dir.join(format!("{}.{}", image.sha, ext(&image.mime).unwrap_or("bin")));
    let tmp = dir.join(format!("{}.{}.tmp", image.sha, crate::util::short_id()));
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        std::fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
        if path.exists() {
            return Ok(());
        }
        crate::util::write_private(&tmp, &clean)?;
        std::fs::rename(&tmp, &path).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })?;
        Ok(())
    })
    .await
    .map_err(anyhow::Error::from)??;
    Ok(image)
}

/// Where a stored image is and its type, or `None` when there is no such image.
async fn locate(app: &App, sha: &str) -> Option<(PathBuf, &'static str)> {
    if !valid_sha(sha) {
        return None;
    }
    for (ext, mime) in EXTS {
        let path = dir(app).join(format!("{sha}.{ext}"));
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            return Some((path, mime));
        }
    }
    None
}

/// A stored image's bytes and reference.
pub async fn load(app: &App, sha: &str) -> Option<(Vec<u8>, ImageRef)> {
    let (path, mime) = locate(app, sha).await?;
    let bytes = tokio::fs::read(&path).await.ok()?;
    let (width, height) = dimensions(mime, &bytes).unwrap_or((0, 0));
    let image = ImageRef {
        sha: sha.to_string(),
        mime: mime.to_string(),
        width,
        height,
        bytes: bytes.len() as u64,
    };
    Some((bytes, image))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Upload {
    /// Base64, or a `data:` URL.
    data: String,
}

/// `POST /api/chat/attachments`: stores one image and answers its reference. The body is the image
/// itself (any `Content-Type` but JSON), or JSON `{"data": "<base64 or data: URL>"}`.
pub async fn upload(State(app): State<Shared>, headers: HeaderMap, body: Bytes) -> ApiResult<ImageRef> {
    let json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim_start().starts_with("application/json"));
    let bytes = if json {
        let req: Upload =
            serde_json::from_slice(&body).map_err(|e| client_error(StatusCode::BAD_REQUEST, &format!("invalid JSON: {e}")))?;
        let data = req.data.split_once(";base64,").map_or(req.data.as_str(), |(_, d)| d);
        crate::util::b64_decode(data).ok_or_else(|| client_error(StatusCode::BAD_REQUEST, "the image is not base64"))?
    } else {
        body.to_vec()
    };
    if bytes.len() > MAX_BYTES {
        return Err(client_error(StatusCode::PAYLOAD_TOO_LARGE, "an image is over 10 MB"));
    }
    Ok(Json(store(&app, &bytes).await?))
}

/// `GET /api/chat/attachments/{sha}`: a stored image, served as its real type and never sniffed.
pub async fn serve(State(app): State<Shared>, Path(sha): Path<String>) -> Result<Response, crate::AppError> {
    let (path, mime) = locate(&app, &sha)
        .await
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such image"))?;
    let bytes = tokio::fs::read(&path).await?;
    Ok(image_response(mime, &sha, bytes))
}

fn image_response(mime: &str, sha: &str, bytes: Vec<u8>) -> Response {
    Response::builder()
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_LENGTH, bytes.len())
        // The name is the content's hash: it never changes, so the browser keeps it.
        .header(header::CACHE_CONTROL, "private, max-age=31536000, immutable")
        .header(header::ETAG, format!("\"{sha}\""))
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::CONTENT_SECURITY_POLICY, "default-src 'none'; sandbox")
        .header(header::CONTENT_DISPOSITION, "inline")
        .body(Body::from(bytes))
        .expect("a fixed response")
}

/// The images every stored conversation's messages refer to.
pub(crate) async fn referenced(app: &App) -> HashSet<String> {
    let mut out = HashSet::new();
    let Ok(mut entries) = tokio::fs::read_dir(app.cfg.data_dir.join("chats")).await else {
        return out;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".jsonl") {
            continue;
        }
        if let Ok(text) = tokio::fs::read_to_string(entry.path()).await {
            out.extend(crate::chat::image_shas(&crate::chat::parse_messages(&text)));
        }
    }
    out
}

/// After a conversation is deleted: removes the images it referred to that no remaining message
/// refers to, and uploads nothing took up for a day. Answers how many files went.
pub(crate) async fn collect_garbage(app: &App, released: &HashSet<String>) -> usize {
    let keep = referenced(app).await;
    let Ok(mut entries) = tokio::fs::read_dir(dir(app)).await else {
        return 0;
    };
    let mut removed = 0;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(sha) = name.split('.').next().filter(|s| valid_sha(s)) else {
            continue;
        };
        if keep.contains(sha) {
            continue;
        }
        let stale = entry
            .metadata()
            .await
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age > ORPHAN_AGE);
        if (released.contains(sha) || stale) && tokio::fs::remove_file(entry.path()).await.is_ok() {
            removed += 1;
        }
    }
    removed
}

/// The API routes this module serves. `server::api_routes` merges them into the cockpit's router,
/// behind the activity log's route layer and `host_guard`.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new()
        .route(
            "/api/chat/attachments",
            routing::post(upload).layer(axum::extract::DefaultBodyLimit::max(UPLOAD_BODY_LIMIT)),
        )
        .route("/api/chat/attachments/{sha}", routing::get(serve))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A 1×1 PNG with a tEXt chunk that says where it was taken.
    pub(crate) fn png() -> Vec<u8> {
        fn chunk(kind: &[u8], data: &[u8]) -> Vec<u8> {
            let mut c = (data.len() as u32).to_be_bytes().to_vec();
            c.extend_from_slice(kind);
            c.extend_from_slice(data);
            c.extend_from_slice(&[0, 0, 0, 0]);
            c
        }
        let mut b = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = 1u32.to_be_bytes().to_vec();
        ihdr.extend_from_slice(&1u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        b.extend(chunk(b"IHDR", &ihdr));
        b.extend(chunk(b"tEXt", b"Location\0Reykjavik 64.1466,-21.9426"));
        b.extend(chunk(b"IDAT", &[0x78, 0x9C, 0x63, 0, 0, 0, 1, 0, 1]));
        b.extend(chunk(b"IEND", &[]));
        b
    }

    /// A JPEG with an EXIF segment (GPS text and orientation 6), a comment, a frame header of
    /// 640×480, a tiny scan, and a thumbnail after the end of the image.
    pub(crate) fn jpeg() -> Vec<u8> {
        let mut tiff = b"II\x2A\0".to_vec();
        tiff.extend_from_slice(&8u32.to_le_bytes());
        tiff.extend_from_slice(&2u16.to_le_bytes());
        // Orientation = 6.
        tiff.extend_from_slice(&0x0112u16.to_le_bytes());
        tiff.extend_from_slice(&3u16.to_le_bytes());
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&6u16.to_le_bytes());
        tiff.extend_from_slice(&[0, 0]);
        // GPS IFD pointer, and the text a reader would find.
        tiff.extend_from_slice(&0x8825u16.to_le_bytes());
        tiff.extend_from_slice(&4u16.to_le_bytes());
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&0u32.to_le_bytes());
        tiff.extend_from_slice(&0u32.to_le_bytes());
        tiff.extend_from_slice(b"GPS 64.1466N 21.9426W");
        let seg = |marker: u8, payload: &[u8]| {
            let mut s = vec![0xFF, marker];
            s.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
            s.extend_from_slice(payload);
            s
        };
        let mut b = vec![0xFF, 0xD8];
        b.extend(seg(0xE0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0"));
        let mut exif = b"Exif\0\0".to_vec();
        exif.extend_from_slice(&tiff);
        b.extend(seg(0xE1, &exif));
        b.extend(seg(0xFE, b"shot at home"));
        // SOF0: precision 8, height 480, width 640, one component.
        b.extend(seg(0xC0, &[8, 0x01, 0xE0, 0x02, 0x80, 1, 1, 0x11, 0]));
        b.extend(seg(0xDA, &[1, 1, 0, 0, 0x3F, 0]));
        b.extend_from_slice(&[0x12, 0xFF, 0x00, 0x34, 0xFF, 0xD0, 0x56]);
        b.extend_from_slice(&[0xFF, 0xD9]);
        b.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xE1, 0, 8, b'G', b'P', b'S', b'!', 0, 0, 0xFF, 0xD9]);
        b
    }

    #[test]
    fn types_come_from_the_bytes_not_the_name() {
        assert_eq!(sniff(&png()), Some("image/png"));
        assert_eq!(sniff(&jpeg()), Some("image/jpeg"));
        assert_eq!(sniff(b"GIF89a\x02\0\x03\0"), Some("image/gif"));
        assert_eq!(sniff(b"RIFF\x10\0\0\0WEBPVP8 "), Some("image/webp"));
        // What an attacker would label image/png: markup, a script, an SVG.
        for spoof in [
            &b"<svg xmlns='http://www.w3.org/2000/svg'/>"[..],
            b"<html><script>",
            b"%PDF-1.7",
            b"\x89PNX",
        ] {
            assert_eq!(sniff(spoof), None, "{spoof:?}");
            assert!(prepare(spoof).is_err());
        }
    }

    #[test]
    fn sizes_are_read_from_headers() {
        assert_eq!(dimensions("image/png", &png()), Some((1, 1)));
        assert_eq!(dimensions("image/jpeg", &jpeg()), Some((640, 480)));
        assert_eq!(dimensions("image/gif", b"GIF89a\x02\0\x03\0"), Some((2, 3)));
        let mut vp8l = b"RIFF\0\0\0\0WEBPVP8L\x05\0\0\0\x2F".to_vec();
        vp8l.extend_from_slice(&((9u32) | (4u32 << 14)).to_le_bytes());
        assert_eq!(dimensions("image/webp", &vp8l), Some((10, 5)));
        assert_eq!(dimensions("image/gif", b"GIF89a\0\0\0\0"), None, "a zero size is refused");
    }

    #[test]
    fn metadata_that_can_carry_a_location_is_stripped() {
        let (clean, image) = prepare(&jpeg()).unwrap();
        let has = |hay: &[u8], needle: &[u8]| hay.windows(needle.len()).any(|w| w == needle);
        assert!(!has(&clean, b"GPS"), "EXIF GPS and the trailing thumbnail are gone");
        assert!(!has(&clean, b"shot at home"), "comments are gone");
        assert!(has(&clean, b"JFIF"), "JFIF stays");
        assert_eq!(
            exif_orientation(&clean[clean.windows(6).position(|w| w == b"Exif\0\0").unwrap()..]),
            Some(6),
            "orientation kept"
        );
        assert!(clean.ends_with(&[0x56, 0xFF, 0xD9]), "the scan survives, the end marker last");
        assert_eq!((image.mime.as_str(), image.width, image.height), ("image/jpeg", 640, 480));
        assert_eq!(image.bytes, clean.len() as u64);

        let (clean, image) = prepare(&png()).unwrap();
        assert!(!has(&clean, b"Reykjavik"));
        assert!(has(&clean, b"IDAT") && clean.ends_with(b"IEND\0\0\0\0"));
        assert_eq!((image.width, image.height), (1, 1));
    }

    #[test]
    fn size_caps_hold() {
        assert!(prepare(&[]).is_err());
        let mut big = png();
        big.resize(MAX_BYTES + 1, 0);
        assert!(prepare(&big).unwrap_err().contains("over 10 MB"));
    }

    #[test]
    fn hashes_are_the_only_names() {
        assert!(valid_sha(&"a".repeat(64)));
        for bad in [
            "",
            "../etc/passwd",
            &"A".repeat(64),
            &"a".repeat(63),
            &format!("{}.png", "a".repeat(60)),
        ] {
            assert!(!valid_sha(bad), "{bad:?}");
        }
    }

    #[tokio::test]
    async fn identical_images_are_stored_once_private_and_served_with_safe_headers() {
        let root = std::env::temp_dir().join(format!("colonizer-chat-images-{}", uuid::Uuid::new_v4()));
        let app = crate::tests::test_app(&root);
        let a = store(&app, &jpeg()).await.unwrap();
        let b = store(&app, &jpeg()).await.unwrap();
        assert_eq!(a, b, "the same picture is one stored file");
        let files: Vec<_> = std::fs::read_dir(dir(&app)).unwrap().collect();
        assert_eq!(files.len(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = dir(&app).join(format!("{}.jpg", a.sha));
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }

        let response = serve(State(app.clone()), Path(a.sha.clone())).await.unwrap();
        let h = response.headers();
        assert_eq!(h[header::CONTENT_TYPE], "image/jpeg");
        assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert!(h[header::CACHE_CONTROL].to_str().unwrap().contains("immutable"));
        assert!(h[header::CONTENT_SECURITY_POLICY].to_str().unwrap().contains("sandbox"));
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body.len() as u64, a.bytes);
        assert!(serve(State(app.clone()), Path("../../config".into())).await.is_err());
        assert!(serve(State(app.clone()), Path("f".repeat(64))).await.is_err());

        // Through the handler: raw bytes, JSON base64 and a data: URL all land on the same file;
        // a spoofed type is refused whatever the headers claim.
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, "image/png".parse().unwrap());
        let raw = upload(State(app.clone()), headers.clone(), Bytes::from(png()))
            .await
            .unwrap()
            .0;
        let mut json_headers = HeaderMap::new();
        json_headers.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
        let data = format!("data:image/png;base64,{}", crate::util::b64_encode(&png()));
        let body = Bytes::from(serde_json::to_vec(&serde_json::json!({ "data": data })).unwrap());
        let via_json = upload(State(app.clone()), json_headers, body).await.unwrap().0;
        assert_eq!(raw, via_json);
        let spoof = upload(State(app.clone()), headers, Bytes::from_static(b"<svg onload=alert(1)>")).await;
        assert_eq!(spoof.err().map(|e| e.status()), Some(StatusCode::UNSUPPORTED_MEDIA_TYPE));
        let _ = std::fs::remove_dir_all(root);
    }
}
