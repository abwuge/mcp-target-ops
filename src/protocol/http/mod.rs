mod auth;
mod form;
mod oauth;
mod response;

pub(crate) use self::response::respond_json;
use self::response::{
    respond_bytes, respond_bytes_with_headers, respond_download, respond_empty,
    respond_empty_with_allow, respond_icon,
};
use crate::{
    core::{
        error::{Error, Result},
        state::AppState,
    },
    protocol::mcp,
};
use rand::{rngs::OsRng, RngCore};
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Arc, thread};
use tiny_http::{Method, Request, Server};

const MCP_PATH: &str = "/mcp";
const FAVICON_PATH: &str = "/favicon.ico";
const PROTECTED_RESOURCE_METADATA_PATH: &str = "/.well-known/oauth-protected-resource";
const AUTHORIZATION_SERVER_METADATA_PATH: &str = "/.well-known/oauth-authorization-server";
const AUTHORIZE_PATH: &str = "/oauth/authorize";
const TOKEN_PATH: &str = "/oauth/token";
const REGISTER_PATH: &str = "/oauth/register";
const DOWNLOAD_PREFIX: &str = "/downloads/";
const APP_ICON: &[u8] = include_bytes!("../../../assets/mcp-target-ops.ico");

pub(super) type Params = BTreeMap<String, String>;

pub fn serve_http(state: Arc<AppState>, addr: &str) -> Result<()> {
    let server = Server::http(addr)
        .map_err(|err| Error::Config(format!("failed to bind HTTP server on {addr}: {err}")))?;

    eprintln!("mcp-target-ops listening on http://{addr}/mcp");
    for request in server.incoming_requests() {
        let state = Arc::clone(&state);
        thread::spawn(move || {
            if let Err(err) = handle_request(state, request) {
                eprintln!("mcp-target-ops HTTP request failed: {err}");
            }
        });
    }

    Ok(())
}

fn handle_request(state: Arc<AppState>, mut request: Request) -> Result<()> {
    let method = request.method().clone();
    let path = request_path(request.url());

    if method == Method::Options {
        return respond_empty_with_allow(request, 204);
    }

    if is_public_endpoint(&method, path.as_str()) {
        return handle_public_request(state, request, method, path);
    }

    if !auth::request_authorized(&state, &request) {
        return response::respond_unauthorized(&state, request);
    }

    match (method, path.as_str()) {
        // COMPAT(COMPAT-008): Keep accepting POST / for older clients that were
        // configured with the public origin before /mcp became canonical.
        (Method::Post, "/" | MCP_PATH) => {
            let incoming_session = auth::mcp_session_id(&request);
            let fallback_caller_key = auth::caller_key(&request);
            let mut body = Vec::new();
            request.as_reader().read_to_end(&mut body)?;
            let new_session =
                (incoming_session.is_none() && is_initialize_body(&body)).then(new_mcp_session_id);
            let caller_key = new_session
                .as_ref()
                .or(incoming_session.as_ref())
                .map(|session| format!("session:{session}"))
                .unwrap_or(fallback_caller_key);
            match mcp::handle_json_bytes_for_caller(state, &body, &caller_key)? {
                Some(response) => {
                    if let Some(session_id) = new_session.as_deref() {
                        respond_bytes_with_headers(
                            request,
                            200,
                            response,
                            vec![("Mcp-Session-Id", session_id)],
                        )
                    } else {
                        respond_bytes(request, 200, response)
                    }
                }
                None => respond_empty(request, 202),
            }
        }
        (Method::Delete, MCP_PATH) => {
            let caller_key = auth::caller_key(&request);
            state.clear_target_instructions_for_caller(&caller_key);
            respond_empty(request, 204)
        }
        (Method::Get, MCP_PATH) => respond_json(
            request,
            405,
            json!({
                "error": "method not allowed",
                "endpoints": ["POST /mcp"]
            }),
        ),
        _ => respond_json(
            request,
            404,
            json!({
                "error": "not found",
                "endpoints": [
                    "GET /health",
                    "POST /mcp"
                ]
            }),
        ),
    }
}

fn is_initialize_body(body: &[u8]) -> bool {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("method")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .as_deref()
        == Some("initialize")
}

fn new_mcp_session_id() -> String {
    let mut bytes = [0_u8; 24];
    OsRng.fill_bytes(&mut bytes);
    let mut value = String::with_capacity(8 + bytes.len() * 2);
    value.push_str("session_");
    for byte in bytes {
        value.push_str(&format!("{byte:02x}"));
    }
    value
}

fn is_public_endpoint(method: &Method, path: &str) -> bool {
    if method == &Method::Get && path.starts_with(DOWNLOAD_PREFIX) {
        return true;
    }
    matches!(
        (method, path),
        (Method::Get, "/")
            | (Method::Get, FAVICON_PATH)
            | (Method::Get, "/health")
            | (Method::Get, PROTECTED_RESOURCE_METADATA_PATH)
            | (Method::Get, AUTHORIZATION_SERVER_METADATA_PATH)
            | (Method::Get, AUTHORIZE_PATH)
            | (Method::Post, AUTHORIZE_PATH)
            | (Method::Post, TOKEN_PATH)
            | (Method::Post, REGISTER_PATH)
    )
}

fn handle_public_request(
    state: Arc<AppState>,
    request: Request,
    method: Method,
    path: String,
) -> Result<()> {
    if method == Method::Get {
        if let Some(token) = path.strip_prefix(DOWNLOAD_PREFIX) {
            return match state.downloads.open(token)? {
                Some(download) => respond_download(request, download),
                None => respond_json(request, 404, json!({ "error": "download not found" })),
            };
        }
    }

    match (method.clone(), path.as_str()) {
        (Method::Get, FAVICON_PATH) => respond_icon(request),
        (Method::Get, "/") => {
            let base_url = public_base_url(&state, &request);
            respond_json(
                request,
                200,
                json!({
                    "name": state.config.server.name.clone(),
                    "version": state.config.server.version.clone(),
                    "endpoints": {
                        "health": endpoint(&base_url, "/health"),
                        "favicon": endpoint(&base_url, FAVICON_PATH),
                        "mcp": endpoint(&base_url, MCP_PATH),
                        "oauth_protected_resource": endpoint(&base_url, PROTECTED_RESOURCE_METADATA_PATH),
                        "oauth_authorization_server": endpoint(&base_url, AUTHORIZATION_SERVER_METADATA_PATH)
                    }
                }),
            )
        }
        (Method::Get, "/health") => respond_json(
            request,
            200,
            json!({
                "ok": true,
                "name": state.config.server.name.clone(),
                "version": state.config.server.version.clone(),
                "oauth_enabled": state.config.server.oauth_enabled,
            }),
        ),
        (Method::Get, PROTECTED_RESOURCE_METADATA_PATH)
        | (Method::Get, AUTHORIZATION_SERVER_METADATA_PATH)
        | (Method::Get, AUTHORIZE_PATH)
        | (Method::Post, AUTHORIZE_PATH)
        | (Method::Post, TOKEN_PATH)
        | (Method::Post, REGISTER_PATH) => {
            if !state.config.server.oauth_enabled {
                return respond_json(
                    request,
                    404,
                    json!({
                        "error": "not found",
                        "message": "OAuth is not enabled for this server"
                    }),
                );
            }

            oauth::handle_request(state, request, method, path)
        }
        _ => respond_json(request, 404, json!({ "error": "not found" })),
    }
}

pub(crate) fn public_base_url(state: &AppState, request: &Request) -> String {
    if let Some(base_url) = &state.config.server.public_base_url {
        return base_url.trim_end_matches('/').to_string();
    }

    let host = header_value(request, "x-forwarded-host")
        .or_else(|| header_value(request, "host"))
        .unwrap_or("localhost");
    let proto = header_value(request, "x-forwarded-proto").unwrap_or_else(|| {
        if host.starts_with("localhost") || host.starts_with("127.0.0.1") {
            "http"
        } else {
            "https"
        }
    });

    format!("{proto}://{host}")
        .trim_end_matches('/')
        .to_string()
}

fn endpoint(base_url: &str, path: &str) -> String {
    format!("{}{}", base_url.trim_end_matches('/'), path)
}

fn header_value<'a>(request: &'a Request, name: &'static str) -> Option<&'a str> {
    request.headers().iter().find_map(|header| {
        if header.field.equiv(name) {
            Some(header.value.as_str())
        } else {
            None
        }
    })
}

fn request_path(url: &str) -> String {
    url.split('?').next().unwrap_or("/").to_string()
}

#[cfg(test)]
mod tests {
    use super::{is_public_endpoint, APP_ICON, DOWNLOAD_PREFIX, FAVICON_PATH};
    use tiny_http::Method;

    #[test]
    fn public_http_surface_is_limited_to_mcp_support_endpoints() {
        assert!(is_public_endpoint(&Method::Get, FAVICON_PATH));
        assert!(is_public_endpoint(
            &Method::Get,
            &format!("{DOWNLOAD_PREFIX}{}", "a".repeat(64))
        ));
        assert!(!is_public_endpoint(&Method::Get, "/openapi.json"));
        assert!(!is_public_endpoint(&Method::Get, "/actions/v1/targets"));
    }

    #[test]
    fn embedded_icon_is_an_ico_file() {
        assert_eq!(&APP_ICON[..4], &[0, 0, 1, 0]);
    }
}
