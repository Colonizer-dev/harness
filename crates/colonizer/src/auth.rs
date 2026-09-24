//! The cockpit API token (issue #405): the `Host` and `Origin` checks stop browsers, not
//! scripts, so every request to the cockpit HTTP API also needs this per-install secret. It lives
//! at `<config_dir>/api-token`, created on first use and loaded into `App`. Clis and scripts send
//! it as `Authorization: Bearer`; the browser uses the `colonizer_token` cookie (see `host_guard`
//! for how the two differ on `Origin`).

use anyhow::{Context, Result};
use axum::http::{HeaderMap, header};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

/// The cookie that carries the token for the browser UI.
pub const COOKIE_NAME: &str = "colonizer_token";
/// How long the browser keeps the cookie: about a year, so signing in is a rare event.
const COOKIE_MAX_AGE_SECS: u64 = 365 * 24 * 60 * 60;
/// What an unauthenticated `/api` caller is told: plain text, pointing at the fix.
pub const UNAUTHORIZED_BODY: &str = "missing or invalid API token; run `colonizer open`";

/// Whether `host_guard` authenticated the request; handlers serving a reduced body read it from
/// the request extensions.
#[derive(Clone, Copy, Debug)]
pub struct Authenticated(pub bool);

/// How an authenticated request reached the API, for the activity log's `via`: the browser's
/// cookie (the cockpit) or an `Authorization: Bearer` token (the CLI or a script). Set by
/// `host_guard` next to [`Authenticated`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    Cockpit,
    Api,
}

/// The token file: `<config_dir>/api-token`, next to the other saved secrets.
pub fn token_file(config_dir: &Path) -> PathBuf {
    config_dir.join("api-token")
}

/// Loads the install's token, creating it first when this is a first run. Creation mirrors
/// `util::write_secret`'s parent handling (the directory is 0700) and writes the token itself with
/// `util::write_private` (0600). The file stays plaintext on purpose — the CLI and the bench script
/// read it straight off disk — so it deliberately bypasses the `COLONIZER_MASTER_KEY` envelope
/// `write_secret` would apply. An empty file counts as missing and is replaced.
pub fn load_or_create(config_dir: &Path) -> Result<String> {
    let path = token_file(config_dir);
    if let Some(token) = crate::util::read_trimmed(&path)
        && !token.is_empty()
    {
        return Ok(token);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let token = crate::util::random_token();
    crate::util::write_private(&path, token.as_bytes()).with_context(|| format!("could not write {}", path.display()))?;
    Ok(token)
}

/// Constant-time token comparison (the gateway's `constant_time_eq`).
pub fn tokens_match(presented: &str, expected: &str) -> bool {
    crate::gateway::constant_time_eq(presented.as_bytes(), expected.as_bytes())
}

/// The `Authorization: Bearer <token>` header, if one is present and non-empty.
pub fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    value
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
}

/// The `colonizer_token` cookie, if one is present and non-empty.
pub fn cookie_token(headers: &HeaderMap) -> Option<String> {
    for values in headers.get_all(header::COOKIE) {
        let Ok(cookies) = values.to_str() else { continue };
        for part in cookies.split(';') {
            let Some((name, value)) = part.split_once('=') else { continue };
            if name.trim() == COOKIE_NAME {
                let token = value.trim();
                if !token.is_empty() {
                    return Some(token.to_string());
                }
            }
        }
    }
    None
}

/// The `token` query parameter of e.g. `/?token=…`, percent-decoded. Only the sign-in link uses
/// it; API requests use the header or the cookie instead, so tokens stay out of logs.
pub fn query_token(query: Option<&str>) -> Option<String> {
    for pair in query?.split('&') {
        let (name, value) = pair.split_once('=')?;
        if name == "token" {
            let token = percent_decode(value);
            if !token.is_empty() {
                return Some(token);
            }
        }
    }
    None
}

/// Enough percent-decoding for a query value: `%XX` escapes only (the token is hex, so neither
/// those nor `+` occur in practice). Anything else, including truncated escapes, is left as-is
/// rather than rejected — and `get` keeps a trailing `%` from indexing out of bounds.
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = String::with_capacity(value.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(pair) = bytes.get(i + 1..i + 3)
            && let Ok(hex) = std::str::from_utf8(pair)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte as char);
            i += 3;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

/// The `Set-Cookie` value: `HttpOnly` against page JS, `SameSite=Strict` against cross-site sends.
pub fn set_cookie_header(token: &str) -> String {
    format!("{COOKIE_NAME}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={COOKIE_MAX_AGE_SECS}")
}

/// The browser-facing sign-in URL for this install's bind address. A wildcard bind answers on
/// every interface, so the link points at loopback — the operator opens it on the host itself.
pub fn login_url(bind: &str, token: &str) -> String {
    let (host, port) = bind.rsplit_once(':').unwrap_or((bind, "7878"));
    let host = match host {
        "0.0.0.0" => "127.0.0.1",
        "::" | "[::]" => "[::1]",
        host => host,
    };
    format!("http://{host}:{port}/?token={token}")
}

/// Whether the bind answers to loopback only (anything else reaches the network).
pub fn bind_is_loopback(bind: &str) -> bool {
    let host = bind.rsplit_once(':').map_or(bind, |(host, _)| host);
    matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]") || host.starts_with("127.")
}

/// Opens the sign-in link in the local browser (`open` on macOS, `xdg-open` on Linux with a
/// display), spawning without waiting. Anything else — no browser, no display,
/// `COLONIZER_NO_BROWSER`, a failed spawn — skips silently: the link is already printed, and
/// startup never hinges on this.
pub fn open_browser(url: &str) {
    if std::env::var_os("COLONIZER_NO_BROWSER").is_some() {
        return;
    }
    fn launch(tool: &str, url: &str) {
        let _ = std::process::Command::new(tool)
            .arg(url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(target_os = "macos")]
    launch("open", url);
    #[cfg(target_os = "linux")]
    if std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some() {
        launch("xdg-open", url);
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let _ = url;
}

/// What a valid sign-in link answers: the cookie plus a page whose script replaces the address
/// with the same path minus the `token` query. A plain redirect would arrive without the
/// `SameSite=Strict` cookie when the link was followed cross-site (e.g. clicked in a chat app).
pub fn login_page() -> String {
    include_str!("pages/signed_in.html").to_string()
}

/// What an unauthenticated page load answers. The script reloads once — session storage plus a
/// timestamp, so it cannot loop — because a `SameSite=Strict` cookie is not sent on a cross-site
/// navigation: the page-initiated reload is same-site, so a cookie set earlier rides along.
pub fn locked_page() -> String {
    include_str!("pages/locked.html").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("colonizer-auth-{}", crate::util::short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_token_file_is_created_private_and_reused() {
        let root = temp_dir();
        let first = load_or_create(&root).unwrap();
        assert_eq!(first.len(), 64, "a 64-hex-char token, like util::random_token");
        let mode = std::fs::metadata(token_file(&root)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the token is a secret: owner-only");
        let dir_mode = std::fs::metadata(&root).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700, "the config dir holds secrets");
        assert_eq!(load_or_create(&root).unwrap(), first, "a second load reuses the file");
        // An empty file counts as missing and is replaced.
        std::fs::write(token_file(&root), "  \n").unwrap();
        assert_eq!(load_or_create(&root).unwrap().len(), 64, "whitespace is not a token");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn token_comparison_is_exact() {
        assert!(tokens_match("abc", "abc"));
        assert!(!tokens_match("abc", "abd"));
        assert!(!tokens_match("abc", "abcd"), "length matters too");
        assert!(!tokens_match("", "abc"));
    }

    #[test]
    fn the_sign_in_pages_keep_their_safety_scripts() {
        let locked = locked_page();
        assert!(locked.contains("colonizer open"), "the locked page names the command");
        assert!(
            locked.contains("colonizer_auth_retry"),
            "the one-shot reload that picks up a SameSite cookie"
        );
        let signed_in = login_page();
        assert!(
            signed_in.contains("searchParams.delete('token')"),
            "the token leaves the address"
        );
        assert!(
            signed_in.contains("history.replaceState"),
            "the address is cleaned before the delay"
        );
        // Served before sign-in, so it must load nothing from anywhere else.
        for page in [&locked, &signed_in] {
            for external in ["<link rel=\"stylesheet\"", "<script src", "@import", "url(http"] {
                assert!(!page.contains(external), "external asset {external:?} in a sign-in page");
            }
        }
    }

    #[test]
    fn bearer_and_cookie_headers_parse() {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, "Bearer tok123".parse().unwrap());
        headers.insert(header::COOKIE, "other=1; colonizer_token=cook456".parse().unwrap());
        assert_eq!(bearer_token(&headers).as_deref(), Some("tok123"));
        assert_eq!(cookie_token(&headers).as_deref(), Some("cook456"));

        let empty = HeaderMap::new();
        assert!(bearer_token(&empty).is_none());
        assert!(cookie_token(&empty).is_none());

        let mut basic = HeaderMap::new();
        basic.insert(header::AUTHORIZATION, "Basic dXNlcg==".parse().unwrap());
        assert!(bearer_token(&basic).is_none(), "only the Bearer scheme counts");
    }

    #[test]
    fn query_token_finds_the_param_and_decodes_it() {
        assert_eq!(query_token(Some("token=abc")).as_deref(), Some("abc"));
        assert_eq!(query_token(Some("a=1&token=abc&b=2")).as_deref(), Some("abc"));
        assert_eq!(query_token(Some("token=%61%62%63")).as_deref(), Some("abc"));
        // Truncated and dangling escapes are left as-is, never indexed out of bounds.
        assert_eq!(query_token(Some("token=a%b")).as_deref(), Some("a%b"));
        assert_eq!(query_token(Some("token=a%4")).as_deref(), Some("a%4"));
        assert_eq!(query_token(Some("token=%")).as_deref(), Some("%"));
        assert!(query_token(Some("a=1")).is_none());
        assert!(query_token(Some("token=")).is_none());
        assert!(query_token(None).is_none());
    }

    #[test]
    fn the_login_url_points_at_loopback_for_a_wildcard_bind() {
        let token = "t";
        assert_eq!(login_url("127.0.0.1:7878", token), "http://127.0.0.1:7878/?token=t");
        assert_eq!(login_url("0.0.0.0:7878", token), "http://127.0.0.1:7878/?token=t");
        assert_eq!(login_url("[::]:7878", token), "http://[::1]:7878/?token=t");
        assert!(bind_is_loopback("127.0.0.1:7878"));
        assert!(bind_is_loopback("localhost:7878"));
        assert!(!bind_is_loopback("0.0.0.0:7878"));
        assert!(!bind_is_loopback("192.168.1.5:7878"));
    }

    #[test]
    fn the_cookie_is_httponly_and_strict() {
        let header = set_cookie_header("tok");
        assert!(header.starts_with("colonizer_token=tok;"), "{header}");
        assert!(header.contains("HttpOnly"), "{header}");
        assert!(header.contains("SameSite=Strict"), "{header}");
        assert!(header.contains("Path=/"), "{header}");
    }
}
