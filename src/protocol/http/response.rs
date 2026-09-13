use super::{
    endpoint,
    form::{percent_encode, quote_header_value},
    public_base_url, APP_ICON, PROTECTED_RESOURCE_METADATA_PATH,
};
use crate::{
    core::{
        error::{Error, Result},
        oauth::OAuthError,
        state::AppState,
    },
    tooling::download::DownloadFile,
};
use serde_json::{json, Value};
use std::io::Read;
use tiny_http::{Header, Request, Response, StatusCode};

pub(crate) fn respond_json(request: Request, status: u16, value: Value) -> Result<()> {
    respond_bytes(request, status, serde_json::to_vec(&value)?)
}

pub(super) fn respond_json_with_cache_headers(
    request: Request,
    status: u16,
    value: Value,
) -> Result<()> {
    respond_bytes_with_headers(
        request,
        status,
        serde_json::to_vec(&value)?,
        vec![("Cache-Control", "no-store"), ("Pragma", "no-cache")],
    )
}

pub(super) fn respond_oauth_error(request: Request, status: u16, err: OAuthError) -> Result<()> {
    respond_json_with_cache_headers(
        request,
        status,
        json!({
            "error": err.error,
            "error_description": err.description,
        }),
    )
}

pub(super) fn respond_unauthorized(state: &AppState, request: Request) -> Result<()> {
    let mut response = Response::from_data(
        br#"{"error":{"code":"unauthorized","message":"A valid bearer token is required","retryable":false}}"#
            .to_vec(),
    )
    .with_status_code(StatusCode(401));
    response.add_header(header("Content-Type", "application/json"));

    if state.config.server.oauth_enabled {
        let base_url = public_base_url(state, &request);
        response.add_header(header(
            "WWW-Authenticate",
            &format!(
                "Bearer resource_metadata={}, scope={}, error=\"invalid_token\", error_description=\"OAuth access token required\"",
                quote_header_value(&endpoint(&base_url, PROTECTED_RESOURCE_METADATA_PATH)),
                quote_header_value(&state.config.server.oauth_scopes.join(" "))
            ),
        ));
    } else {
        response.add_header(header(
            "WWW-Authenticate",
            r#"Bearer realm="mcp-target-ops""#,
        ));
    }

    add_common_headers(&mut response);
    request.respond(response).map_err(Error::Io)
}

pub(super) fn respond_bytes(request: Request, status: u16, body: Vec<u8>) -> Result<()> {
    respond_bytes_with_headers(request, status, body, Vec::new())
}

pub(super) fn respond_bytes_with_headers(
    request: Request,
    status: u16,
    body: Vec<u8>,
    extra_headers: Vec<(&str, &str)>,
) -> Result<()> {
    let mut response = Response::from_data(body).with_status_code(StatusCode(status));
    response.add_header(header("Content-Type", "application/json"));
    for (name, value) in extra_headers {
        response.add_header(header(name, value));
    }
    add_common_headers(&mut response);
    request.respond(response).map_err(Error::Io)
}

pub(super) fn respond_html(request: Request, status: u16, body: String) -> Result<()> {
    respond_html_with_redirect(request, status, body, None)
}

// The caller must validate the client's redirect before rendering its consent page.
pub(super) fn respond_authorization_html(
    request: Request,
    status: u16,
    body: String,
    redirect_uri: &str,
) -> Result<()> {
    respond_html_with_redirect(request, status, body, Some(redirect_uri))
}

fn html_csp(redirect_uri: Option<&str>) -> String {
    // Chromium also checks form-action on the POST's cross-origin redirect.
    // Serialize only the parsed origin: never interpolate untrusted URL text into CSP.
    let origin = redirect_uri
        .and_then(|uri| reqwest::Url::parse(uri).ok())
        .filter(|url| matches!(url.scheme(), "https" | "http"))
        .map(|url| format!(" {}", url.origin().ascii_serialization()))
        .unwrap_or_default();
    format!("default-src 'none'; img-src 'self'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; form-action 'self'{origin}; base-uri 'none'; frame-ancestors 'none'")
}

fn respond_html_with_redirect(
    request: Request,
    status: u16,
    body: String,
    redirect_uri: Option<&str>,
) -> Result<()> {
    let mut response = Response::from_string(body).with_status_code(StatusCode(status));
    response.add_header(header("Content-Type", "text/html; charset=utf-8"));
    response.add_header(header("Cache-Control", "no-store"));
    response.add_header(header("Pragma", "no-cache"));
    response.add_header(header("Referrer-Policy", "no-referrer"));
    response.add_header(header("X-Content-Type-Options", "nosniff"));
    response.add_header(header("X-Frame-Options", "DENY"));
    response.add_header(header("Content-Security-Policy", &html_csp(redirect_uri)));
    add_common_headers(&mut response);
    request.respond(response).map_err(Error::Io)
}

pub(super) fn respond_icon(request: Request) -> Result<()> {
    let mut response = Response::from_data(APP_ICON.to_vec()).with_status_code(StatusCode(200));
    response.add_header(header("Content-Type", "image/x-icon"));
    response.add_header(header("Cache-Control", "public, max-age=86400"));
    add_common_headers(&mut response);
    request.respond(response).map_err(Error::Io)
}

pub(super) fn respond_download(request: Request, download: DownloadFile) -> Result<()> {
    let encoded_name = percent_encode(&download.file_name);
    let fallback_name = ascii_filename(&download.file_name);
    let disposition = format!(
        "attachment; filename={}; filename*=UTF-8''{}",
        quote_header_value(&fallback_name),
        encoded_name
    );
    let sha256 = download.sha256.clone();
    let mime_type = download.mime_type.clone();
    let mut response = Response::from_file(download.file).with_status_code(StatusCode(200));
    response.add_header(header("Content-Type", &mime_type));
    response.add_header(header("Content-Disposition", &disposition));
    response.add_header(header("Cache-Control", "private, no-store"));
    response.add_header(header("Pragma", "no-cache"));
    response.add_header(header("X-Content-Type-Options", "nosniff"));
    response.add_header(header("X-Content-SHA256", &sha256));
    add_common_headers(&mut response);
    request.respond(response).map_err(Error::Io)
}

fn ascii_filename(file_name: &str) -> String {
    let sanitized: String = file_name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | ' ') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.trim().is_empty() {
        "download".to_string()
    } else {
        sanitized
    }
}

pub(super) fn respond_empty(request: Request, status: u16) -> Result<()> {
    let mut response = Response::empty(StatusCode(status));
    add_common_headers(&mut response);
    request.respond(response).map_err(Error::Io)
}

pub(super) fn respond_empty_with_allow(request: Request, status: u16) -> Result<()> {
    let mut response = Response::empty(StatusCode(status));
    response.add_header(header("Allow", "GET, POST, DELETE, OPTIONS"));
    response.add_header(header(
        "Access-Control-Allow-Methods",
        "GET, POST, DELETE, OPTIONS",
    ));
    response.add_header(header(
        "Access-Control-Allow-Headers",
        "authorization, content-type, mcp-session-id",
    ));
    response.add_header(header("Access-Control-Max-Age", "86400"));
    add_common_headers(&mut response);
    request.respond(response).map_err(Error::Io)
}

pub(super) fn redirect_with_params(
    request: Request,
    redirect_uri: &str,
    params: Vec<(&str, &str)>,
) -> Result<()> {
    let mut location = redirect_uri.to_string();
    let mut first = !location.contains('?');
    for (name, value) in params {
        if value.is_empty() {
            continue;
        }
        location.push(if first { '?' } else { '&' });
        first = false;
        location.push_str(&percent_encode(name));
        location.push('=');
        location.push_str(&percent_encode(value));
    }

    let mut response = Response::empty(StatusCode(302));
    response.add_header(header("Location", &location));
    response.add_header(header("Cache-Control", "no-store"));
    response.add_header(header("Pragma", "no-cache"));
    response.add_header(header("Referrer-Policy", "no-referrer"));
    add_common_headers(&mut response);
    request.respond(response).map_err(Error::Io)
}

fn add_common_headers<R: Read>(response: &mut Response<R>) {
    response.add_header(header("Access-Control-Allow-Origin", "*"));
    response.add_header(header(
        "Access-Control-Expose-Headers",
        "Mcp-Session-Id, WWW-Authenticate",
    ));
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("static header is valid")
}

#[cfg(test)]
mod tests {
    use super::{ascii_filename, html_csp};

    #[test]
    fn download_filename_fallback_is_header_safe() {
        assert_eq!(ascii_filename("报告.zip"), "__.zip");
        assert_eq!(ascii_filename("a\r\nb.zip"), "a__b.zip");
        assert_eq!(ascii_filename("\n\r"), "__");
    }

    #[test]
    fn consent_policy_allows_only_callback_origin() {
        let policy = html_csp(Some(
            "https://chatgpt.com/connector/oauth/test?state=private",
        ));
        assert!(policy.contains("form-action 'self' https://chatgpt.com;"));
        assert!(!policy.contains("private"));
        assert!(!policy.contains("connector"));
        assert!(html_csp(Some("http://127.0.0.1:43123/callback"))
            .contains("form-action 'self' http://127.0.0.1:43123;"));
        assert!(html_csp(None).contains("form-action 'self';"));
        assert!(html_csp(Some("javascript:alert(1)")).contains("form-action 'self';"));
        let policy = html_csp(Some("https://example.com/path; form-action *"));
        assert!(policy.contains("form-action 'self' https://example.com;"));
        assert!(!policy.contains("form-action *"));
    }
}
