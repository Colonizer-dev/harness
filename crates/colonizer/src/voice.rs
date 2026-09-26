//! Voice: speech-to-text for the cockpit's composer, through a service the operator connects.
//!
//! The `voice` module picks the service. `browser`, the default, is nothing on this side: the
//! cockpit uses the browser's own recogniser. Every other provider is a transcription API the
//! Mothership calls: the browser records a clip, posts it to `POST /api/voice/transcribe`, and this
//! module forwards it with the key and returns the text. The key therefore stays here, like the model
//! provider keys: saved 0600 (encrypted under `COLONIZER_MASTER_KEY` when that is set), never written
//! to modules.json, never returned by the API, never sent into a colony.
//!
//! Audio goes browser → Mothership → provider and is not kept: it lives in memory for the one request.
//!
//! The requests are built by pure functions ([`build_request`], [`parse_transcript`]) so every
//! provider's URL, headers and form fields are pinned by tests without a network.

use crate::{
    ApiResult, App, Shared, client_error,
    config::{ModuleChoice, setting_str},
    modules::schema_for,
    util::{delete_secret, env_nonempty, read_secret, truncate, write_secret},
};
use axum::{
    Json,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode, header},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};

pub const BROWSER: &str = "browser";
/// The largest clip the Mothership forwards: OpenAI's and Groq's own upload limit.
pub const MAX_BYTES: usize = 25 * 1024 * 1024;
/// The longest clip the composer records before it stops by itself.
pub const MAX_SECONDS: u64 = 120;
const TIMEOUT: Duration = Duration::from_secs(90);

/// One transcription service: what it is called, where it lives, how it wants its key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Service {
    pub id: &'static str,
    pub name: &'static str,
    pub default_model: &'static str,
    /// Empty for `openai_compatible`, whose base URL is a setting.
    pub base_url: &'static str,
    /// The environment variable a key may come from when none is saved.
    pub env: &'static str,
    /// The host of a configured model provider whose key may be reused (Settings → Model providers).
    pub provider_host: Option<&'static str>,
    /// Whether a request without a key makes sense (a local whisper server usually wants none).
    pub key_optional: bool,
    wire: Wire,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wire {
    /// `POST {base}/audio/transcriptions`, multipart `file` + `model` (+ `language`), bearer key.
    OpenAi,
    /// `POST {base}/v1/listen?model=…`, the raw audio as the body, `Authorization: Token`.
    Deepgram,
    /// `POST {base}/v1/speech-to-text`, multipart `file` + `model_id` (+ `language_code`), `xi-api-key`.
    ElevenLabs,
}

pub const SERVICES: [Service; 5] = [
    Service {
        id: "openai",
        name: "OpenAI",
        default_model: "gpt-4o-mini-transcribe",
        base_url: "https://api.openai.com/v1",
        env: "OPENAI_API_KEY",
        provider_host: Some("api.openai.com"),
        key_optional: false,
        wire: Wire::OpenAi,
    },
    Service {
        id: "groq",
        name: "Groq",
        default_model: "whisper-large-v3-turbo",
        base_url: "https://api.groq.com/openai/v1",
        env: "GROQ_API_KEY",
        provider_host: Some("api.groq.com"),
        key_optional: false,
        wire: Wire::OpenAi,
    },
    Service {
        id: "deepgram",
        name: "Deepgram",
        default_model: "nova-3",
        base_url: "https://api.deepgram.com",
        env: "DEEPGRAM_API_KEY",
        provider_host: None,
        key_optional: false,
        wire: Wire::Deepgram,
    },
    Service {
        id: "elevenlabs",
        name: "ElevenLabs",
        default_model: "scribe_v1",
        base_url: "https://api.elevenlabs.io",
        env: "ELEVENLABS_API_KEY",
        provider_host: None,
        key_optional: false,
        wire: Wire::ElevenLabs,
    },
    Service {
        id: "openai_compatible",
        name: "OpenAI-compatible",
        default_model: "whisper-1",
        base_url: "",
        env: "COLONIZER_VOICE_API_KEY",
        provider_host: None,
        key_optional: true,
        wire: Wire::OpenAi,
    },
];

pub fn service(id: &str) -> Option<&'static Service> {
    SERVICES.iter().find(|s| s.id == id)
}

/// The Settings → Modules providers for the `voice` kind: the browser plus every service.
pub fn module_providers() -> Vec<(&'static str, &'static str, &'static str, Value)> {
    let language = json!({"type": "string", "title": "Language", "description": "An ISO-639-1 code such as en or nl. Blank lets the service detect it", "default": ""});
    let mut out = vec![(
        BROWSER,
        "Browser",
        "The browser's own speech recognition. Nothing to connect; Chrome sends the audio to Google, Safari can recognise on the device",
        json!({"type": "object", "properties": {"language": language.clone()}}),
    )];
    for s in &SERVICES {
        let mut properties = serde_json::Map::new();
        if s.base_url.is_empty() {
            properties.insert(
                "base_url".into(),
                json!({"type": "string", "title": "Base URL", "description": "The server's OpenAI-style API root, e.g. http://127.0.0.1:8000/v1 for a local whisper server or a LiteLLM proxy; requests go to {base}/audio/transcriptions", "default": ""}),
            );
        }
        properties.insert(
            "model".into(),
            json!({"type": "string", "title": "Model", "description": model_hint(s.id), "default": s.default_model}),
        );
        properties.insert("language".into(), language.clone());
        let description = match s.id {
            "openai" => "OpenAI's transcription API. Reuses an OpenAI model provider's key when you have one",
            "groq" => "Groq's hosted Whisper, fast and inexpensive. Reuses a Groq model provider's key when you have one",
            "deepgram" => "Deepgram's speech-to-text API",
            "elevenlabs" => "ElevenLabs Scribe speech-to-text",
            _ => "Any server speaking OpenAI's /audio/transcriptions: a local whisper server, LiteLLM, vLLM",
        };
        out.push((s.id, s.name, description, json!({"type": "object", "properties": properties})));
    }
    out
}

fn model_hint(id: &str) -> &'static str {
    match id {
        "openai" => "gpt-4o-mini-transcribe (default), gpt-4o-transcribe or whisper-1",
        "groq" => "whisper-large-v3-turbo (default) or whisper-large-v3",
        "deepgram" => "nova-3 (default) or nova-2",
        "elevenlabs" => "scribe_v1",
        _ => "Whatever the server names its model; whisper-1 for most",
    }
}

// ---------------------------------------------------------------------------
// Configuration and keys
// ---------------------------------------------------------------------------

/// What the `voice` module says, resolved: provider (browser when off or unset), model, language, base URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoiceConfig {
    pub provider: String,
    pub model: String,
    pub language: String,
    pub base_url: String,
}

pub fn config_of(choice: Option<&ModuleChoice>, agents: &[crate::modules::AgentModule]) -> VoiceConfig {
    let Some(choice) = choice.filter(|c| c.enabled && service(&c.provider).is_some()) else {
        let language = choice
            .map(|c| setting_str(c, &schema_for("voice", BROWSER, agents), "language"))
            .unwrap_or_default();
        return VoiceConfig {
            provider: BROWSER.into(),
            model: String::new(),
            language,
            base_url: String::new(),
        };
    };
    let schema = schema_for("voice", &choice.provider, agents);
    let svc = service(&choice.provider).expect("filtered above");
    let model = setting_str(choice, &schema, "model");
    let base_url = setting_str(choice, &schema, "base_url");
    VoiceConfig {
        provider: choice.provider.clone(),
        model: if model.trim().is_empty() {
            svc.default_model.to_string()
        } else {
            model.trim().to_string()
        },
        language: setting_str(choice, &schema, "language").trim().to_string(),
        base_url: if svc.base_url.is_empty() {
            base_url.trim().trim_end_matches('/').to_string()
        } else {
            svc.base_url.to_string()
        },
    }
}

fn key_file(app: &App, provider: &str) -> PathBuf {
    app.cfg.config_dir.join("voice-keys").join(provider)
}

/// The key for `provider` and where it came from: saved here, else its environment variable, else a
/// configured model provider on the same host. Never logged, never returned.
fn resolve_key(app: &App, provider: &str) -> Option<(String, String)> {
    let svc = service(provider)?;
    if let Some(key) = read_secret(&key_file(app, provider)) {
        return Some((key, "saved".into()));
    }
    if let Some(key) = env_nonempty(svc.env) {
        return Some((key, svc.env.into()));
    }
    let host = svc.provider_host?;
    app.providers()
        .into_iter()
        .filter(|p| crate::providers::split_url(&p.base_url).is_some_and(|(_, h, _, _)| h.eq_ignore_ascii_case(host)))
        .find_map(|p| app.provider_key(&p.id).map(|key| (key, format!("provider:{}", p.id))))
}

fn current(app: &App, modules: &crate::config::ModulesConfig) -> VoiceConfig {
    config_of(modules.get("voice"), &app.agents)
}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

/// A clip's media type as the services want it, and the file extension they infer the codec from.
/// `None` for anything that is not audio the composer records.
pub fn audio_kind(content_type: &str) -> Option<(&'static str, &'static str)> {
    let base = content_type.split(';').next().unwrap_or_default().trim().to_ascii_lowercase();
    match base.as_str() {
        "audio/webm" => Some(("audio/webm", "webm")),
        "audio/ogg" => Some(("audio/ogg", "ogg")),
        "audio/mp4" | "audio/m4a" | "audio/x-m4a" => Some(("audio/mp4", "m4a")),
        "audio/mpeg" | "audio/mp3" => Some(("audio/mpeg", "mp3")),
        "audio/wav" | "audio/x-wav" | "audio/wave" => Some(("audio/wav", "wav")),
        _ => None,
    }
}

/// A request ready to send: everything a test needs to pin, nothing that needs a network.
#[derive(Debug)]
pub struct Prepared {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// A multipart/form-data body: text fields, then one file part.
fn multipart(boundary: &str, fields: &[(&str, &str)], file: (&str, &str, &str, &[u8])) -> Vec<u8> {
    let mut body = Vec::new();
    for (name, value) in fields {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes(),
        );
    }
    let (name, filename, mime, bytes) = file;
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"{filename}\"\r\nContent-Type: {mime}\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

/// The request that transcribes `audio` (of media type `mime`, extension `ext`) with `cfg`'s service.
pub fn build_request(
    cfg: &VoiceConfig,
    key: Option<&str>,
    audio: &[u8],
    mime: &str,
    ext: &str,
    boundary: &str,
) -> Result<Prepared, String> {
    let svc = service(&cfg.provider).ok_or("voice is set to the browser; nothing to send")?;
    if cfg.base_url.is_empty() {
        return Err("set the voice service's base URL in Settings → Modules → Voice".into());
    }
    let key = key.filter(|k| !k.is_empty());
    if key.is_none() && !svc.key_optional {
        return Err(format!("add the {} API key in Settings → Modules → Voice", svc.name));
    }
    let filename = format!("speech.{ext}");
    let mut headers = Vec::new();
    Ok(match svc.wire {
        Wire::OpenAi => {
            if let Some(key) = key {
                headers.push(("authorization".into(), format!("Bearer {key}")));
            }
            headers.push(("content-type".into(), format!("multipart/form-data; boundary={boundary}")));
            let mut fields = vec![("model", cfg.model.as_str()), ("response_format", "json")];
            if !cfg.language.is_empty() {
                fields.push(("language", cfg.language.as_str()));
            }
            Prepared {
                url: format!("{}/audio/transcriptions", cfg.base_url),
                headers,
                body: multipart(boundary, &fields, ("file", &filename, mime, audio)),
            }
        }
        Wire::Deepgram => {
            headers.push(("authorization".into(), format!("Token {}", key.unwrap_or_default())));
            headers.push(("content-type".into(), mime.to_string()));
            let mut query = format!("model={}&smart_format=true&punctuate=true", urlencode(&cfg.model));
            if cfg.language.is_empty() {
                query.push_str("&detect_language=true");
            } else {
                query.push_str(&format!("&language={}", urlencode(&cfg.language)));
            }
            Prepared {
                url: format!("{}/v1/listen?{query}", cfg.base_url),
                headers,
                body: audio.to_vec(),
            }
        }
        Wire::ElevenLabs => {
            headers.push(("xi-api-key".into(), key.unwrap_or_default().to_string()));
            headers.push(("content-type".into(), format!("multipart/form-data; boundary={boundary}")));
            let mut fields = vec![("model_id", cfg.model.as_str())];
            if !cfg.language.is_empty() {
                fields.push(("language_code", cfg.language.as_str()));
            }
            Prepared {
                url: format!("{}/v1/speech-to-text", cfg.base_url),
                headers,
                body: multipart(boundary, &fields, ("file", &filename, mime, audio)),
            }
        }
    })
}

fn urlencode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The transcript in a service's answer.
pub fn parse_transcript(provider: &str, body: &Value) -> Result<String, String> {
    let text = match service(provider).map(|s| s.wire) {
        Some(Wire::Deepgram) => body["results"]["channels"][0]["alternatives"][0]["transcript"].as_str(),
        Some(_) => body["text"].as_str(),
        None => None,
    };
    text.map(|t| t.trim().to_string())
        .ok_or_else(|| "the voice service answered without a transcript".into())
}

/// A failed answer, said plainly. The body is quoted short and scrubbed of the key, in case a
/// service echoes the request.
pub fn failure(name: &str, status: u16, body: &str, key: Option<&str>) -> String {
    match status {
        401 | 403 => format!("{name} refused the API key ({status}); check it in Settings → Modules → Voice"),
        413 => format!("{name} says the recording is too large"),
        429 => format!("{name} is rate limiting or out of credit (429); try again shortly"),
        _ => {
            let mut detail = truncate(body.trim(), 200);
            if let Some(key) = key.filter(|k| k.len() >= 4) {
                detail = detail.replace(key, "[key]");
            }
            if detail.is_empty() {
                format!("{name} answered {status}")
            } else {
                format!("{name} answered {status}: {detail}")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

/// `GET /api/voice`: the active service and whether it can be used. Never the key.
pub async fn status(State(app): State<Shared>) -> Json<Value> {
    let cfg = current(&app, &*app.modules.read().await);
    Json(describe(&app, &cfg))
}

fn describe(app: &App, cfg: &VoiceConfig) -> Value {
    let svc = service(&cfg.provider);
    let key = svc.and_then(|s| resolve_key(app, s.id));
    let configured = match svc {
        None => true,
        Some(s) => !cfg.base_url.is_empty() && (key.is_some() || s.key_optional),
    };
    json!({
        "provider": cfg.provider,
        "name": svc.map_or("Browser", |s| s.name),
        "model": cfg.model,
        "language": cfg.language,
        "configured": configured,
        "has_key": key.is_some(),
        "source": key.map(|(_, source)| source),
        "key_optional": svc.is_some_and(|s| s.key_optional),
        "max_seconds": MAX_SECONDS,
        "max_bytes": MAX_BYTES,
    })
}

#[derive(Deserialize)]
pub struct VoiceKey {
    provider: String,
    api_key: String,
}

/// `PUT /api/voice/key`: saves the key for one service, or removes it when empty. Answers with the
/// status of the active service.
pub async fn put_key(State(app): State<Shared>, Json(req): Json<VoiceKey>) -> ApiResult<Value> {
    let Some(svc) = service(&req.provider) else {
        return Err(client_error(StatusCode::BAD_REQUEST, "unknown voice service"));
    };
    let key = req.api_key.trim();
    let path = key_file(&app, svc.id);
    if key.is_empty() {
        delete_secret(&path);
    } else if key.len() > 512 || !key.chars().all(|c| c.is_ascii_graphic()) {
        return Err(client_error(StatusCode::BAD_REQUEST, "that doesn't look like an API key"));
    } else {
        write_secret(&path, key)?;
    }
    Ok(status(State(app)).await)
}

/// `POST /api/voice/transcribe`: the raw clip in, `{text}` out.
pub async fn transcribe(State(app): State<Shared>, headers: HeaderMap, body: Bytes) -> ApiResult<Value> {
    let cfg = current(&app, &*app.modules.read().await);
    let Some(svc) = service(&cfg.provider) else {
        return Err(client_error(
            StatusCode::CONFLICT,
            "voice is set to the browser's own recognition; connect a service in Settings → Modules → Voice",
        ));
    };
    if body.is_empty() {
        return Err(client_error(StatusCode::BAD_REQUEST, "the recording is empty"));
    }
    if body.len() > MAX_BYTES {
        return Err(client_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "the recording is larger than 25 MB",
        ));
    }
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let Some((mime, ext)) = audio_kind(content_type) else {
        return Err(client_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "send audio/webm, audio/ogg, audio/mp4, audio/mpeg or audio/wav",
        ));
    };
    let key = resolve_key(&app, svc.id).map(|(key, _)| key);
    let boundary = format!("colonizer-{}", crate::util::short_id());
    let prepared = build_request(&cfg, key.as_deref(), &body, mime, ext, &boundary)
        .map_err(|message| client_error(StatusCode::CONFLICT, &message))?;
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(TIMEOUT)
        .user_agent(concat!("colonizer/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let mut request = client.post(&prepared.url);
    for (name, value) in &prepared.headers {
        request = request.header(name, value);
    }
    let response = request.body(prepared.body).send().await.map_err(|e| {
        let why = if e.is_timeout() {
            "timed out".to_string()
        } else {
            "could not be reached".to_string()
        };
        client_error(StatusCode::BAD_GATEWAY, &format!("{} {why}", svc.name))
    })?;
    let code = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    if !(200..300).contains(&code) {
        return Err(client_error(
            StatusCode::BAD_GATEWAY,
            &failure(svc.name, code, &text, key.as_deref()),
        ));
    }
    let parsed: Value = serde_json::from_str(&text).map_err(|_| {
        client_error(
            StatusCode::BAD_GATEWAY,
            &format!("{} answered with something that is not JSON", svc.name),
        )
    })?;
    let transcript = parse_transcript(svc.id, &parsed).map_err(|message| client_error(StatusCode::BAD_GATEWAY, &message))?;
    Ok(Json(json!({"text": transcript, "provider": svc.id})))
}

/// The API routes this module serves. `server::api_routes` merges them into the cockpit's router,
/// behind the activity log's route layer and `host_guard`.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new()
        .route("/api/voice", routing::get(status))
        .route("/api/voice/key", routing::put(put_key))
        // A clip is larger than axum's 2 MB default body limit; the handler checks the cap itself too.
        .route(
            "/api/voice/transcribe",
            routing::post(transcribe).layer(axum::extract::DefaultBodyLimit::max(MAX_BYTES + 1)),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(provider: &str) -> VoiceConfig {
        let svc = service(provider).unwrap();
        VoiceConfig {
            provider: provider.into(),
            model: svc.default_model.into(),
            language: String::new(),
            base_url: if svc.base_url.is_empty() {
                "http://127.0.0.1:8000/v1".into()
            } else {
                svc.base_url.into()
            },
        }
    }

    fn header<'a>(p: &'a Prepared, name: &str) -> Option<&'a str> {
        p.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    fn body_text(p: &Prepared) -> String {
        String::from_utf8_lossy(&p.body).into_owned()
    }

    #[test]
    fn openai_posts_a_multipart_form_with_the_model_and_a_bearer_key() {
        let p = build_request(&cfg("openai"), Some("sk-test"), b"OPUS", "audio/webm", "webm", "B").unwrap();
        assert_eq!(p.url, "https://api.openai.com/v1/audio/transcriptions");
        assert_eq!(header(&p, "authorization"), Some("Bearer sk-test"));
        assert_eq!(header(&p, "content-type"), Some("multipart/form-data; boundary=B"));
        let body = body_text(&p);
        assert!(body.contains("name=\"model\"\r\n\r\ngpt-4o-mini-transcribe\r\n"));
        assert!(body.contains("name=\"file\"; filename=\"speech.webm\"\r\nContent-Type: audio/webm\r\n\r\nOPUS\r\n--B--\r\n"));
        assert!(!body.contains("name=\"language\""), "blank language is auto-detect, not sent");
    }

    #[test]
    fn groq_speaks_the_openai_wire_at_its_own_root() {
        let mut c = cfg("groq");
        c.language = "nl".into();
        let p = build_request(&c, Some("gsk"), b"x", "audio/mp4", "m4a", "B").unwrap();
        assert_eq!(p.url, "https://api.groq.com/openai/v1/audio/transcriptions");
        assert!(body_text(&p).contains("name=\"language\"\r\n\r\nnl\r\n"));
        assert!(body_text(&p).contains("whisper-large-v3-turbo"));
    }

    #[test]
    fn deepgram_sends_the_raw_clip_with_a_token_header_and_query_options() {
        let p = build_request(&cfg("deepgram"), Some("dg"), b"RAW", "audio/webm", "webm", "B").unwrap();
        assert_eq!(
            p.url,
            "https://api.deepgram.com/v1/listen?model=nova-3&smart_format=true&punctuate=true&detect_language=true"
        );
        assert_eq!(header(&p, "authorization"), Some("Token dg"));
        assert_eq!(header(&p, "content-type"), Some("audio/webm"));
        assert_eq!(p.body, b"RAW");
        let mut c = cfg("deepgram");
        c.language = "en-US".into();
        let p = build_request(&c, Some("dg"), b"RAW", "audio/webm", "webm", "B").unwrap();
        assert!(p.url.ends_with("&language=en-US"));
    }

    #[test]
    fn elevenlabs_uses_its_own_field_names_and_header() {
        let p = build_request(&cfg("elevenlabs"), Some("el"), b"x", "audio/wav", "wav", "B").unwrap();
        assert_eq!(p.url, "https://api.elevenlabs.io/v1/speech-to-text");
        assert_eq!(header(&p, "xi-api-key"), Some("el"));
        assert!(body_text(&p).contains("name=\"model_id\"\r\n\r\nscribe_v1\r\n"));
    }

    #[test]
    fn an_openai_compatible_server_needs_a_base_url_but_not_a_key() {
        let p = build_request(&cfg("openai_compatible"), None, b"x", "audio/webm", "webm", "B").unwrap();
        assert_eq!(p.url, "http://127.0.0.1:8000/v1/audio/transcriptions");
        assert_eq!(header(&p, "authorization"), None);
        let mut c = cfg("openai_compatible");
        c.base_url = String::new();
        assert!(
            build_request(&c, None, b"x", "audio/webm", "webm", "B")
                .unwrap_err()
                .contains("base URL")
        );
    }

    #[test]
    fn a_hosted_service_without_a_key_says_where_to_add_one() {
        let err = build_request(&cfg("openai"), None, b"x", "audio/webm", "webm", "B").unwrap_err();
        assert_eq!(err, "add the OpenAI API key in Settings → Modules → Voice");
        assert!(build_request(&cfg("groq"), Some(""), b"x", "audio/webm", "webm", "B").is_err());
    }

    #[test]
    fn only_audio_the_composer_records_is_accepted() {
        assert_eq!(audio_kind("audio/webm;codecs=opus"), Some(("audio/webm", "webm")));
        assert_eq!(audio_kind("audio/mp4"), Some(("audio/mp4", "m4a")));
        assert_eq!(audio_kind("audio/x-wav"), Some(("audio/wav", "wav")));
        assert_eq!(audio_kind("text/plain"), None);
        assert_eq!(audio_kind(""), None);
    }

    #[test]
    fn transcripts_are_read_from_each_service_shape() {
        assert_eq!(parse_transcript("openai", &json!({"text": " hello "})).unwrap(), "hello");
        assert_eq!(
            parse_transcript("elevenlabs", &json!({"text": "hi", "words": []})).unwrap(),
            "hi"
        );
        let dg = json!({"results": {"channels": [{"alternatives": [{"transcript": "fix the bug"}]}]}});
        assert_eq!(parse_transcript("deepgram", &dg).unwrap(), "fix the bug");
        assert!(parse_transcript("openai", &json!({"error": "x"})).is_err());
    }

    #[test]
    fn failures_are_plain_and_never_echo_the_key() {
        assert!(failure("OpenAI", 401, "", Some("sk-secret")).contains("refused the API key"));
        assert!(failure("Groq", 429, "", None).contains("rate limiting"));
        let echoed = failure("Local", 500, "bad request for key sk-secret-123", Some("sk-secret-123"));
        assert!(!echoed.contains("sk-secret-123"));
        assert!(echoed.contains("[key]"));
        assert_eq!(failure("Deepgram", 502, "  ", None), "Deepgram answered 502");
    }

    #[test]
    fn the_module_resolves_to_the_browser_when_off_unset_or_unknown() {
        assert_eq!(config_of(None, &[]).provider, BROWSER);
        let mut off = ModuleChoice {
            provider: "openai".into(),
            enabled: false,
            settings: Default::default(),
        };
        assert_eq!(config_of(Some(&off), &[]).provider, BROWSER);
        off.enabled = true;
        let on = config_of(Some(&off), &[]);
        assert_eq!(
            (on.provider.as_str(), on.model.as_str(), on.base_url.as_str()),
            ("openai", "gpt-4o-mini-transcribe", "https://api.openai.com/v1")
        );
        let mut compat = ModuleChoice {
            provider: "openai_compatible".into(),
            enabled: true,
            settings: Default::default(),
        };
        compat.settings.insert("base_url".into(), json!("http://localhost:9000/v1/"));
        assert_eq!(config_of(Some(&compat), &[]).base_url, "http://localhost:9000/v1");
    }

    #[test]
    fn every_service_is_a_module_provider_after_the_browser() {
        let ids: Vec<_> = module_providers().into_iter().map(|(id, ..)| id).collect();
        assert_eq!(
            ids,
            ["browser", "openai", "groq", "deepgram", "elevenlabs", "openai_compatible"]
        );
    }
}
