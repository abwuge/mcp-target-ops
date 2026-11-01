mod auth;
mod dashboard;
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
        activity::Caller,
        error::{Error, Result},
        state::AppState,
    },
    protocol::mcp,
};
use base64::Engine as _;
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

    if path == "/dashboard" || path.starts_with("/dashboard/") {
        return dashboard::handle(state, request, method, &path);
    }

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
        (Method::Post, MCP_PATH) => {
            let user_agent = header_value(&request, "user-agent").map(str::to_string);
            let (oauth_client_id, oauth_client_name) =
                auth::oauth_client_metadata(&state, &request);
            let mut body = Vec::new();
            request.as_reader().read_to_end(&mut body)?;
            let parsed = serde_json::from_slice::<Value>(&body).ok();
            let modern = parsed.as_ref().is_some_and(|value| {
                value.get("method").and_then(Value::as_str) == Some("server/discover")
                    || value
                        .pointer("/params/_meta/io.modelcontextprotocol~1protocolVersion")
                        .is_some()
            }) || header_value(&request, "MCP-Protocol-Version")
                == Some(mcp::MODERN_PROTOCOL);

            if modern {
                let header_error = validate_modern_headers(
                    parsed.as_ref(),
                    header_value(&request, "MCP-Protocol-Version"),
                    header_value(&request, "Mcp-Method"),
                    header_value(&request, "Mcp-Name"),
                );
                if let Err(message) = header_error {
                    return respond_mcp_error(request, parsed.as_ref(), 400, -32020, &message);
                }
            }

            // Modern requests have no protocol session. Stable application ownership
            // (e.g. jobs and terminals) is derived from the authenticated grant.
            let incoming_session = (!modern).then(|| auth::mcp_session_id(&request)).flatten();
            let fallback_caller_key = if modern {
                auth::credential_caller_key(&request)
            } else {
                auth::caller_key(&request)
            };
            let new_session = (!modern && incoming_session.is_none() && is_initialize_body(&body))
                .then(new_mcp_session_id);
            let caller_key = new_session
                .as_ref()
                .or(incoming_session.as_ref())
                .map(|session| format!("session:{session}"))
                .unwrap_or(fallback_caller_key);
            let mut caller = Caller::from_key(&caller_key, "http");
            caller.job_owner_key = auth::job_owner_key(&state, &request);
            caller.user_agent = user_agent;
            caller.oauth_client_id = oauth_client_id;
            caller.oauth_client_name = oauth_client_name;
            match mcp::handle_json_bytes_for_context(state, &body, &caller)? {
                Some(response) => {
                    let status = if modern {
                        let value: Value = serde_json::from_slice(&response)?;
                        match value.pointer("/error/code").and_then(Value::as_i64) {
                            Some(-32601) => 404,
                            Some(-32022 | -32020 | -32602 | -32600 | -32700) => 400,
                            _ => 200,
                        }
                    } else {
                        200
                    };
                    if let Some(session_id) = new_session.as_deref() {
                        respond_bytes_with_headers(
                            request,
                            status,
                            response,
                            vec![("Mcp-Session-Id", session_id)],
                        )
                    } else {
                        respond_bytes(request, status, response)
                    }
                }
                None => respond_empty(request, 202),
            }
        }
        (Method::Delete, MCP_PATH) => {
            if let (Some(store), Some(session_id)) =
                (state.activity.as_ref(), auth::mcp_session_id(&request))
            {
                if let Err(err) = store.end_session(&format!("session:{session_id}")) {
                    eprintln!("mcp-target-ops activity session close failed: {err}");
                }
            }
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

/// The HTTP envelope is routing metadata. Verify it against the JSON-RPC body
/// before executing anything, so a proxy and Target Ops cannot disagree.
fn validate_modern_headers(
    value: Option<&Value>,
    protocol: Option<&str>,
    method: Option<&str>,
    name: Option<&str>,
) -> std::result::Result<(), String> {
    let value = value.ok_or("invalid modern JSON-RPC body")?;
    if !value.is_object() {
        return Err("modern MCP requires one JSON-RPC request per POST".into());
    }
    let body_version = value
        .pointer("/params/_meta/io.modelcontextprotocol~1protocolVersion")
        .and_then(Value::as_str)
        .ok_or("missing params._meta protocolVersion")?;
    if protocol != Some(body_version) {
        return Err("MCP-Protocol-Version header does not match params._meta".into());
    }
    let body_method = value
        .get("method")
        .and_then(Value::as_str)
        .ok_or("missing JSON-RPC method")?;
    if method != Some(body_method) {
        return Err("Mcp-Method header does not match method".into());
    }
    if body_method == "tools/call"
        || body_method == "prompts/get"
        || body_method == "resources/read"
    {
        let body_name = value
            .pointer(if body_method == "resources/read" {
                "/params/uri"
            } else {
                "/params/name"
            })
            .and_then(Value::as_str)
            .ok_or("missing MCP name in request parameters")?;
        let header_name = name
            .ok_or_else(|| "missing Mcp-Name header".to_string())
            .and_then(decode_header_value)?;
        if header_name != body_name {
            return Err("Mcp-Name header does not match request parameters".into());
        }
    }
    Ok(())
}

fn decode_header_value(value: &str) -> std::result::Result<String, String> {
    if let Some(encoded) = value
        .strip_prefix("=?base64?")
        .and_then(|encoded| encoded.strip_suffix("?="))
    {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| "invalid Base64 Mcp-Name header")?;
        String::from_utf8(bytes).map_err(|_| "invalid UTF-8 Mcp-Name header".into())
    } else {
        Ok(value.to_owned())
    }
}

fn respond_mcp_error(
    request: Request,
    body: Option<&Value>,
    status: u16,
    code: i64,
    message: &str,
) -> Result<()> {
    let id = body
        .and_then(|body| body.get("id"))
        .cloned()
        .unwrap_or(Value::Null);
    respond_bytes(
        request,
        status,
        serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": code, "message": message},
        }))?,
    )
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
    fn modern_http_routing_headers_must_match_request_body() {
        use super::{decode_header_value, validate_modern_headers};
        use serde_json::json;
        let body = json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{
            "name":"target_list","arguments":{},"_meta":{
                "io.modelcontextprotocol/protocolVersion":"2026-07-28",
                "io.modelcontextprotocol/clientCapabilities":{}
            }
        }});
        let check =
            |protocol, method, name| validate_modern_headers(Some(&body), protocol, method, name);
        assert!(check(Some("2026-07-28"), Some("tools/call"), Some("target_list")).is_ok());
        assert!(check(None, Some("tools/call"), Some("target_list")).is_err());
        assert!(check(Some("2025-06-18"), Some("tools/call"), Some("target_list")).is_err());
        assert!(check(Some("2026-07-28"), Some("tools/list"), Some("target_list")).is_err());
        assert!(check(Some("2026-07-28"), Some("tools/call"), None).is_err());
        assert!(check(Some("2026-07-28"), Some("tools/call"), Some("other")).is_err());

        let encoded = "=?base64?dGFyZ2V0X2xpc3Q=?=";
        assert_eq!(decode_header_value(encoded).unwrap(), "target_list");
        assert!(check(Some("2026-07-28"), Some("tools/call"), Some(encoded)).is_ok());
        assert!(decode_header_value("=?base64?not-base64?=").is_err());
    }

    #[test]
    fn embedded_icon_is_an_ico_file() {
        assert_eq!(&APP_ICON[..4], &[0, 0, 1, 0]);
    }
}
