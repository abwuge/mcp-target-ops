use super::{
    endpoint,
    form::{percent_encode, quote_header_value},
    public_base_url, APP_ICON, PROTECTED_RESOURCE_METADATA_PATH,
};
use crate::core::{
    error::{Error, Result},
    oauth::OAuthError,
    state::AppState,
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

fn respond_bytes_with_headers(
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
    let mut response = Response::from_string(body).with_status_code(StatusCode(status));
    response.add_header(header("Content-Type", "text/html; charset=utf-8"));
    response.add_header(header("Cache-Control", "no-store"));
    response.add_header(header("Pragma", "no-cache"));
    response.add_header(header("Referrer-Policy", "no-referrer"));
    response.add_header(header("X-Content-Type-Options", "nosniff"));
    response.add_header(header("X-Frame-Options", "DENY"));
    response.add_header(header(
        "Content-Security-Policy",
        "default-src 'none'; img-src 'self'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'",
    ));
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
