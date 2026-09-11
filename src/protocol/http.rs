use crate::{
    core::{
        error::{Error, Result},
        oauth::{AuthorizationCodeRequest, OAuthError, TokenLifetimes, TokenResponse},
        state::AppState,
    },
    protocol::{gpts, mcp},
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::BTreeMap, io::Read, sync::Arc, thread};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

const MCP_PATH: &str = "/mcp";
const FAVICON_PATH: &str = "/favicon.ico";
const PROTECTED_RESOURCE_METADATA_PATH: &str = "/.well-known/oauth-protected-resource";
const AUTHORIZATION_SERVER_METADATA_PATH: &str = "/.well-known/oauth-authorization-server";
const AUTHORIZE_PATH: &str = "/oauth/authorize";
const TOKEN_PATH: &str = "/oauth/token";
const REGISTER_PATH: &str = "/oauth/register";
const APP_ICON: &[u8] = include_bytes!("../../assets/mcp-target-ops.ico");

type Params = BTreeMap<String, String>;

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

    if !request_authorized(&state, &request) {
        return respond_unauthorized(&state, request);
    }

    if gpts::is_action_path(&path) {
        return gpts::handle_request(state, request, method, &path);
    }

    match (method, path.as_str()) {
        (Method::Post, "/" | MCP_PATH) => {
            let mut body = Vec::new();
            request.as_reader().read_to_end(&mut body)?;
            match mcp::handle_json_bytes(state, &body)? {
                Some(response) => respond_bytes(request, 200, response),
                None => respond_empty(request, 202),
            }
        }
        (Method::Delete, MCP_PATH) => respond_empty(request, 204),
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
                    "GET /openapi.json",
                    "POST /mcp",
                    "GET|POST /actions/v1/*"
                ]
            }),
        ),
    }
}

fn is_public_endpoint(method: &Method, path: &str) -> bool {
    matches!(
        (method, path),
        (Method::Get, "/")
            | (Method::Get, FAVICON_PATH)
            | (Method::Get, "/health")
            | (Method::Get, gpts::OPENAPI_PATH)
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
                        "gpts_openapi_schema": endpoint(&base_url, gpts::OPENAPI_PATH),
                        "gpts_actions_prefix": endpoint(&base_url, gpts::ACTIONS_PREFIX),
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
        (Method::Get, gpts::OPENAPI_PATH) => {
            let base_url = public_base_url(&state, &request);
            let document = gpts::openapi_document(&state, &base_url);
            respond_json(request, 200, document)
        }
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

            handle_oauth_request(state, request, method, path)
        }
        _ => respond_json(request, 404, json!({ "error": "not found" })),
    }
}

fn handle_oauth_request(
    state: Arc<AppState>,
    request: Request,
    method: Method,
    path: String,
) -> Result<()> {
    match (method.clone(), path.as_str()) {
        (Method::Get, PROTECTED_RESOURCE_METADATA_PATH) => {
            let base_url = public_base_url(&state, &request);
            respond_json(request, 200, protected_resource_metadata(&state, &base_url))
        }
        (Method::Get, AUTHORIZATION_SERVER_METADATA_PATH) => {
            let base_url = public_base_url(&state, &request);
            respond_json(
                request,
                200,
                authorization_server_metadata(&state, &base_url),
            )
        }
        (Method::Get, AUTHORIZE_PATH) | (Method::Post, AUTHORIZE_PATH) => {
            handle_authorize(state, request, method)
        }
        (Method::Post, TOKEN_PATH) => handle_token(state, request),
        (Method::Post, REGISTER_PATH) => handle_register(state, request),
        _ => respond_json(request, 404, json!({ "error": "not found" })),
    }
}

fn protected_resource_metadata(state: &AppState, base_url: &str) -> Value {
    json!({
        "resource": base_url,
        "authorization_servers": [base_url],
        "scopes_supported": state.config.server.oauth_scopes.clone(),
        "resource_documentation": base_url,
        "token_endpoint_auth_methods_supported": ["none"],
    })
}

fn authorization_server_metadata(state: &AppState, base_url: &str) -> Value {
    let mut value = json!({
        "issuer": base_url,
        "authorization_endpoint": endpoint(base_url, AUTHORIZE_PATH),
        "token_endpoint": endpoint(base_url, TOKEN_PATH),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": state.config.server.oauth_scopes.clone(),
    });

    if state.config.server.oauth_allow_dynamic_client_registration {
        value
            .as_object_mut()
            .expect("metadata is an object")
            .insert(
                "registration_endpoint".to_string(),
                json!(endpoint(base_url, REGISTER_PATH)),
            );
    }

    value
}

fn handle_register(state: Arc<AppState>, mut request: Request) -> Result<()> {
    if !state.config.server.oauth_allow_dynamic_client_registration {
        return respond_oauth_error(
            request,
            404,
            OAuthError::new("invalid_request", "dynamic client registration is disabled"),
        );
    }

    #[derive(Debug, Deserialize)]
    struct RegisterRequest {
        #[serde(default)]
        client_name: Option<String>,
        #[serde(default)]
        redirect_uris: Vec<String>,
        #[serde(default)]
        token_endpoint_auth_method: Option<String>,
        #[serde(default)]
        grant_types: Vec<String>,
        #[serde(default)]
        response_types: Vec<String>,
    }

    let mut body = Vec::new();
    request.as_reader().read_to_end(&mut body)?;
    let registration = match serde_json::from_slice::<RegisterRequest>(&body) {
        Ok(value) => value,
        Err(err) => {
            return respond_oauth_error(
                request,
                400,
                OAuthError::new("invalid_client_metadata", err.to_string()),
            )
        }
    };

    if registration.redirect_uris.is_empty() {
        return respond_oauth_error(
            request,
            400,
            OAuthError::new("invalid_client_metadata", "redirect_uris is required"),
        );
    }

    if let Some(method) = registration.token_endpoint_auth_method.as_deref() {
        if method != "none" {
            return respond_oauth_error(
                request,
                400,
                OAuthError::new(
                    "invalid_client_metadata",
                    "only token_endpoint_auth_method=none is supported",
                ),
            );
        }
    }

    if !registration.grant_types.is_empty()
        && !registration
            .grant_types
            .iter()
            .any(|grant_type| grant_type == "authorization_code")
    {
        return respond_oauth_error(
            request,
            400,
            OAuthError::new(
                "invalid_client_metadata",
                "grant_types must include authorization_code",
            ),
        );
    }

    if !registration.response_types.is_empty()
        && !registration
            .response_types
            .iter()
            .any(|response_type| response_type == "code")
    {
        return respond_oauth_error(
            request,
            400,
            OAuthError::new(
                "invalid_client_metadata",
                "response_types must include code",
            ),
        );
    }

    if let Some(redirect_uri) = registration
        .redirect_uris
        .iter()
        .find(|uri| !redirect_uri_allowed(uri))
    {
        return respond_oauth_error(
            request,
            400,
            OAuthError::new(
                "invalid_redirect_uri",
                format!("redirect_uri is not allowed: {redirect_uri}"),
            ),
        );
    }

    let client = match state
        .oauth
        .lock()
        .unwrap()
        .register_client(registration.client_name, registration.redirect_uris)
    {
        Ok(client) => client,
        Err(err) => return respond_oauth_error(request, 500, err),
    };

    respond_json_with_cache_headers(
        request,
        201,
        json!({
            "client_id": client.client_id,
            "client_name": client.client_name,
            "client_id_issued_at": client.issued_at_unix,
            "redirect_uris": client.redirect_uris,
            "token_endpoint_auth_method": "none",
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "scope": state.config.server.oauth_scopes.join(" "),
        }),
    )
}

fn handle_authorize(state: Arc<AppState>, mut request: Request, method: Method) -> Result<()> {
    let base_url = public_base_url(&state, &request);
    let params = match method {
        Method::Get => match parse_query(request.url()) {
            Ok(params) => params,
            Err(err) => return respond_oauth_error(request, 400, err),
        },
        Method::Post => {
            let mut body = Vec::new();
            request.as_reader().read_to_end(&mut body)?;
            match parse_urlencoded(std::str::from_utf8(&body).unwrap_or_default()) {
                Ok(params) => params,
                Err(err) => return respond_oauth_error(request, 400, err),
            }
        }
        _ => Params::new(),
    };

    if state.config.server.oauth_authorization_password.is_some()
        && !params.contains_key("password")
    {
        return respond_html(request, 200, consent_page(&state, &params));
    }

    if let Some(expected_password) = state.config.server.oauth_authorization_password.as_deref() {
        if params.get("password").map(String::as_str) != Some(expected_password) {
            return respond_html(
                request,
                401,
                "<!doctype html><title>Unauthorized</title><h1>Unauthorized</h1><p>Invalid password.</p>"
                    .to_string(),
            );
        }
    }

    let response_type = match required_param(&params, "response_type") {
        Ok(value) => value,
        Err(err) => return respond_oauth_error(request, 400, err),
    };
    if response_type != "code" {
        return redirect_authorize_error(request, &params, "unsupported_response_type");
    }

    let client_id = match required_param(&params, "client_id") {
        Ok(value) => value,
        Err(err) => return respond_oauth_error(request, 400, err),
    };
    let redirect_uri = match required_param(&params, "redirect_uri") {
        Ok(value) => value,
        Err(err) => return respond_oauth_error(request, 400, err),
    };
    if !client_redirect_allowed(&state, client_id, redirect_uri) {
        return respond_html(
            request,
            400,
            "<!doctype html><title>Invalid redirect_uri</title><h1>Invalid redirect_uri</h1>"
                .to_string(),
        );
    }

    let code_challenge = match required_param(&params, "code_challenge") {
        Ok(value) => value,
        Err(err) => return redirect_authorize_error(request, &params, err.error),
    };
    let code_challenge_method = params
        .get("code_challenge_method")
        .map(String::as_str)
        .unwrap_or("plain");
    if code_challenge_method != "S256" {
        return redirect_authorize_error(request, &params, "invalid_request");
    }

    let resource = params
        .get("resource")
        .map(String::as_str)
        .unwrap_or(base_url.as_str());
    if resource != base_url {
        return redirect_authorize_error(request, &params, "invalid_target");
    }

    let scopes = match requested_scopes(&state, params.get("scope").map(String::as_str)) {
        Ok(scopes) => scopes,
        Err(err) => return redirect_authorize_error(request, &params, err.error),
    };
    let code = state.oauth.lock().unwrap().issue_authorization_code(
        AuthorizationCodeRequest {
            client_id: client_id.to_string(),
            redirect_uri: redirect_uri.to_string(),
            code_challenge: code_challenge.to_string(),
            code_challenge_method: code_challenge_method.to_string(),
            scopes,
            resource: resource.to_string(),
        },
        state.config.server.oauth_authorization_code_ttl_secs,
    );

    redirect_with_params(
        request,
        redirect_uri,
        vec![
            ("code", code.as_str()),
            (
                "state",
                params.get("state").map(String::as_str).unwrap_or(""),
            ),
        ],
    )
}

fn handle_token(state: Arc<AppState>, mut request: Request) -> Result<()> {
    let mut body = Vec::new();
    request.as_reader().read_to_end(&mut body)?;
    let params = match parse_urlencoded(std::str::from_utf8(&body).unwrap_or_default()) {
        Ok(params) => params,
        Err(err) => return respond_oauth_error(request, 400, err),
    };

    let grant_type = match required_param(&params, "grant_type") {
        Ok(value) => value,
        Err(err) => return respond_oauth_error(request, 400, err),
    };
    let client_id = match required_param(&params, "client_id") {
        Ok(value) => value,
        Err(err) => return respond_oauth_error(request, 400, err),
    };

    let exchange = match grant_type {
        "authorization_code" => {
            let code = match required_param(&params, "code") {
                Ok(value) => value,
                Err(err) => return respond_oauth_error(request, 400, err),
            };
            let redirect_uri = match required_param(&params, "redirect_uri") {
                Ok(value) => value,
                Err(err) => return respond_oauth_error(request, 400, err),
            };
            let code_verifier = match required_param(&params, "code_verifier") {
                Ok(value) => value,
                Err(err) => return respond_oauth_error(request, 400, err),
            };

            state.oauth.lock().unwrap().exchange_authorization_code(
                code,
                client_id,
                redirect_uri,
                code_verifier,
                params.get("resource").map(String::as_str),
                token_lifetimes(&state),
            )
        }
        "refresh_token" => {
            let refresh_token = match required_param(&params, "refresh_token") {
                Ok(value) => value,
                Err(err) => return respond_oauth_error(request, 400, err),
            };

            state.oauth.lock().unwrap().exchange_refresh_token(
                refresh_token,
                client_id,
                params.get("resource").map(String::as_str),
                params.get("scope").map(String::as_str),
                token_lifetimes(&state),
            )
        }
        _ => {
            return respond_oauth_error(
                request,
                400,
                OAuthError::new(
                    "unsupported_grant_type",
                    "only authorization_code and refresh_token are supported",
                ),
            )
        }
    };

    respond_token_exchange(request, exchange)
}

fn token_lifetimes(state: &AppState) -> TokenLifetimes {
    TokenLifetimes {
        access_token_secs: state.config.server.oauth_access_token_ttl_secs,
        refresh_token_secs: state.config.server.oauth_refresh_token_ttl_secs,
    }
}

fn respond_token_exchange(
    request: Request,
    exchange: std::result::Result<TokenResponse, OAuthError>,
) -> Result<()> {
    match exchange {
        Ok(token) => respond_json_with_cache_headers(
            request,
            200,
            json!({
                "access_token": token.access_token,
                "refresh_token": token.refresh_token,
                "token_type": "Bearer",
                "expires_in": token.expires_in,
                "scope": token.scope,
            }),
        ),
        Err(err) => respond_oauth_error(request, 400, err),
    }
}

fn requested_scopes(
    state: &AppState,
    requested: Option<&str>,
) -> std::result::Result<Vec<String>, OAuthError> {
    let scopes = requested
        .map(|scope| {
            scope
                .split_whitespace()
                .filter(|scope| !scope.is_empty())
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|scopes| !scopes.is_empty())
        .unwrap_or_else(|| state.config.server.oauth_scopes.clone());

    for scope in &scopes {
        if !state.config.server.oauth_scopes.contains(scope) {
            return Err(OAuthError::new(
                "invalid_scope",
                format!("unsupported scope: {scope}"),
            ));
        }
    }

    Ok(scopes)
}

fn client_redirect_allowed(state: &AppState, client_id: &str, redirect_uri: &str) -> bool {
    let oauth = state.oauth.lock().unwrap();
    if oauth.has_client(client_id) {
        return oauth.client_allows_redirect(client_id, redirect_uri);
    }

    redirect_uri_allowed(redirect_uri)
}

fn redirect_uri_allowed(uri: &str) -> bool {
    uri.starts_with("https://")
        || uri.starts_with("http://localhost:")
        || uri.starts_with("http://127.0.0.1:")
        || uri.starts_with("http://[::1]:")
}

fn request_authorized(state: &AppState, request: &Request) -> bool {
    let static_bearer_configured = state.config.server.http_bearer_token.is_some();
    let oauth_configured = state.config.server.oauth_enabled;
    if !static_bearer_configured && !oauth_configured {
        return true;
    }

    let Some(token) = bearer_token(request) else {
        return false;
    };

    if state
        .config
        .server
        .http_bearer_token
        .as_deref()
        .map(|expected| token == expected)
        .unwrap_or(false)
    {
        return true;
    }

    if state.config.server.oauth_enabled {
        return state.oauth.lock().unwrap().access_token_valid(token);
    }

    false
}

fn bearer_token(request: &Request) -> Option<&str> {
    request.headers().iter().find_map(|header| {
        if !header.field.equiv("Authorization") {
            return None;
        }

        let (scheme, token) = header.value.as_str().split_once(' ')?;
        if scheme.eq_ignore_ascii_case("Bearer") {
            Some(token)
        } else {
            None
        }
    })
}

pub(crate) fn respond_json(request: Request, status: u16, value: Value) -> Result<()> {
    respond_bytes(request, status, serde_json::to_vec(&value)?)
}

fn respond_json_with_cache_headers(request: Request, status: u16, value: Value) -> Result<()> {
    respond_bytes_with_headers(
        request,
        status,
        serde_json::to_vec(&value)?,
        vec![("Cache-Control", "no-store"), ("Pragma", "no-cache")],
    )
}

fn respond_oauth_error(request: Request, status: u16, err: OAuthError) -> Result<()> {
    respond_json_with_cache_headers(
        request,
        status,
        json!({
            "error": err.error,
            "error_description": err.description,
        }),
    )
}

fn respond_unauthorized(state: &AppState, request: Request) -> Result<()> {
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

fn respond_bytes(request: Request, status: u16, body: Vec<u8>) -> Result<()> {
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

fn respond_html(request: Request, status: u16, body: String) -> Result<()> {
    let mut response = Response::from_string(body).with_status_code(StatusCode(status));
    response.add_header(header("Content-Type", "text/html; charset=utf-8"));
    add_common_headers(&mut response);
    request.respond(response).map_err(Error::Io)
}

fn respond_icon(request: Request) -> Result<()> {
    let mut response = Response::from_data(APP_ICON.to_vec()).with_status_code(StatusCode(200));
    response.add_header(header("Content-Type", "image/x-icon"));
    response.add_header(header("Cache-Control", "public, max-age=86400"));
    add_common_headers(&mut response);
    request.respond(response).map_err(Error::Io)
}

fn respond_empty(request: Request, status: u16) -> Result<()> {
    let mut response = Response::empty(StatusCode(status));
    add_common_headers(&mut response);
    request.respond(response).map_err(Error::Io)
}

fn respond_empty_with_allow(request: Request, status: u16) -> Result<()> {
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

fn redirect_authorize_error(request: Request, params: &Params, error: &str) -> Result<()> {
    let Some(redirect_uri) = params.get("redirect_uri").map(String::as_str) else {
        return respond_html(
            request,
            400,
            format!(
                "<!doctype html><title>OAuth error</title><h1>OAuth error</h1><p>{}</p>",
                html_escape(error)
            ),
        );
    };

    if !redirect_uri_allowed(redirect_uri) {
        return respond_html(
            request,
            400,
            "<!doctype html><title>OAuth error</title><h1>Invalid redirect_uri</h1>".to_string(),
        );
    }

    redirect_with_params(
        request,
        redirect_uri,
        vec![
            ("error", error),
            (
                "state",
                params.get("state").map(String::as_str).unwrap_or(""),
            ),
        ],
    )
}

fn redirect_with_params(
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

fn parse_query(url: &str) -> std::result::Result<Params, OAuthError> {
    parse_urlencoded(url.split_once('?').map(|(_, query)| query).unwrap_or(""))
}

fn parse_urlencoded(input: &str) -> std::result::Result<Params, OAuthError> {
    let mut params = Params::new();
    if input.is_empty() {
        return Ok(params);
    }

    for pair in input.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        params.insert(percent_decode(key)?, percent_decode(value)?);
    }

    Ok(params)
}

fn required_param<'a>(
    params: &'a Params,
    name: &'static str,
) -> std::result::Result<&'a str, OAuthError> {
    params
        .get(name)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| OAuthError::new("invalid_request", format!("missing {name}")))
}

fn percent_decode(value: &str) -> std::result::Result<String, OAuthError> {
    let mut bytes = Vec::with_capacity(value.len());
    let mut iter = value.as_bytes().iter().copied();

    while let Some(byte) = iter.next() {
        match byte {
            b'+' => bytes.push(b' '),
            b'%' => {
                let hi = iter.next().ok_or_else(|| {
                    OAuthError::new("invalid_request", "incomplete percent encoding")
                })?;
                let lo = iter.next().ok_or_else(|| {
                    OAuthError::new("invalid_request", "incomplete percent encoding")
                })?;
                let decoded = hex_value(hi)
                    .zip(hex_value(lo))
                    .map(|(hi, lo)| (hi << 4) | lo)
                    .ok_or_else(|| {
                        OAuthError::new("invalid_request", "invalid percent encoding")
                    })?;
                bytes.push(decoded);
            }
            other => bytes.push(other),
        }
    }

    String::from_utf8(bytes)
        .map_err(|_| OAuthError::new("invalid_request", "urlencoded value is not UTF-8"))
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(*byte as char)
            }
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    encoded
}

fn quote_header_value(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn consent_page(state: &AppState, params: &Params) -> String {
    let hidden_inputs = params
        .iter()
        .filter(|(key, _)| key.as_str() != "password")
        .map(|(key, value)| {
            format!(
                r#"<input type="hidden" name="{}" value="{}">"#,
                html_escape(key),
                html_escape(value)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let client_id = params.get("client_id").map(String::as_str).unwrap_or("");
    let scope = params
        .get("scope")
        .cloned()
        .unwrap_or_else(|| state.config.server.oauth_scopes.join(" "));

    format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <link rel="icon" href="{favicon_path}" type="image/x-icon">
  <title>Authorize {name}</title>
  <style>
    body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; margin: 3rem auto; max-width: 36rem; line-height: 1.5; padding: 0 1rem; }}
    .app-icon {{ display: block; width: 5rem; height: 5rem; margin-bottom: 1rem; }}
    label, input, button {{ display: block; width: 100%; box-sizing: border-box; }}
    input {{ margin: .35rem 0 1rem; padding: .65rem; }}
    button {{ padding: .75rem; }}
    code {{ overflow-wrap: anywhere; }}
  </style>
</head>
<body>
  <img class="app-icon" src="{favicon_path}" alt="">
  <h1>Authorize {name}</h1>
  <p>Client <code>{client_id}</code> is requesting access to scope <code>{scope}</code>.</p>
  <form method="post" action="{authorize_path}">
    {hidden_inputs}
    <label>Password
      <input name="password" type="password" autocomplete="current-password" autofocus>
    </label>
    <button type="submit">Authorize</button>
  </form>
</body>
</html>"#,
        name = html_escape(&state.config.server.name),
        client_id = html_escape(client_id),
        scope = html_escape(&scope),
        favicon_path = FAVICON_PATH,
        authorize_path = AUTHORIZE_PATH,
        hidden_inputs = hidden_inputs,
    )
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
fn authorization_matches(value: &str, expected_token: &str) -> bool {
    let Some((scheme, token)) = value.split_once(' ') else {
        return false;
    };

    scheme.eq_ignore_ascii_case("Bearer") && token == expected_token
}

impl From<OAuthError> for Error {
    fn from(err: OAuthError) -> Self {
        Error::Tool(format!("{}: {}", err.error, err.description))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        authorization_matches, is_public_endpoint, parse_urlencoded, percent_decode,
        percent_encode, redirect_uri_allowed, APP_ICON, FAVICON_PATH,
    };
    use crate::protocol::gpts;
    use tiny_http::Method;

    #[test]
    fn authorization_matches_bearer_token() {
        assert!(authorization_matches("Bearer secret-token", "secret-token"));
        assert!(authorization_matches("bearer secret-token", "secret-token"));
    }

    #[test]
    fn authorization_rejects_missing_or_wrong_token() {
        assert!(!authorization_matches("Basic secret-token", "secret-token"));
        assert!(!authorization_matches("Bearer wrong-token", "secret-token"));
        assert!(!authorization_matches("Bearer", "secret-token"));
        assert!(!authorization_matches(
            "Bearer  secret-token",
            "secret-token"
        ));
    }

    #[test]
    fn parses_urlencoded_values() {
        let params = parse_urlencoded("scope=mcp%3Atools+other&state=a%2Bb").unwrap();
        assert_eq!(params.get("scope").unwrap(), "mcp:tools other");
        assert_eq!(params.get("state").unwrap(), "a+b");
    }

    #[test]
    fn percent_codec_round_trips_reserved_values() {
        let encoded = percent_encode("https://example.com/cb?x=a b&y=1");
        assert_eq!(
            percent_decode(&encoded).unwrap(),
            "https://example.com/cb?x=a b&y=1"
        );
    }

    #[test]
    fn redirect_uri_policy_allows_https_and_localhost() {
        assert!(redirect_uri_allowed(
            "https://chatgpt.com/connector/oauth/abc"
        ));
        assert!(redirect_uri_allowed("http://localhost:3000/callback"));
        assert!(redirect_uri_allowed("http://127.0.0.1:3000/callback"));
        assert!(!redirect_uri_allowed("http://example.com/callback"));
    }

    #[test]
    fn gpts_schema_is_public_but_actions_require_http_auth() {
        assert!(is_public_endpoint(&Method::Get, FAVICON_PATH));
        assert!(is_public_endpoint(&Method::Get, gpts::OPENAPI_PATH));
        assert!(!is_public_endpoint(&Method::Get, "/actions/v1/targets"));
        assert!(!is_public_endpoint(&Method::Post, "/actions/v1/files/read"));
    }

    #[test]
    fn embedded_icon_is_an_ico_file() {
        assert_eq!(&APP_ICON[..4], &[0, 0, 1, 0]);
    }
}
