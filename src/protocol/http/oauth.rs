use super::{
    endpoint,
    form::{parse_query, parse_urlencoded, required_param},
    public_base_url,
    response::{
        redirect_with_params, respond_html, respond_json, respond_json_with_cache_headers,
        respond_oauth_error,
    },
    Params, AUTHORIZATION_SERVER_METADATA_PATH, AUTHORIZE_PATH, FAVICON_PATH,
    PROTECTED_RESOURCE_METADATA_PATH, REGISTER_PATH, TOKEN_PATH,
};
use crate::{
    core::{
        error::{Error, Result},
        oauth::{AuthorizationCodeRequest, OAuthError, TokenLifetimes, TokenResponse},
        state::AppState,
    },
    protocol::{html::escape, oauth_page},
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use tiny_http::{Method, Request};

pub(super) fn handle_request(
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

    let response_type = match required_param(&params, "response_type") {
        Ok(value) => value,
        Err(err) => return respond_oauth_error(request, 400, err),
    };
    if response_type != "code" {
        return redirect_authorize_error(request, &params, "unsupported_response_type");
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

    if params.get("decision").map(String::as_str) == Some("deny") {
        return redirect_authorize_error(request, &params, "access_denied");
    }

    if state.config.server.oauth_authorization_password.is_some()
        && !params.contains_key("password")
    {
        return respond_html(
            request,
            200,
            oauth_page::render(&state, &params, None, FAVICON_PATH, AUTHORIZE_PATH),
        );
    }

    if let Some(expected_password) = state.config.server.oauth_authorization_password.as_deref() {
        if params.get("password").map(String::as_str) != Some(expected_password) {
            return respond_html(
                request,
                401,
                oauth_page::render(
                    &state,
                    &params,
                    Some("Incorrect authorization password. Please try again."),
                    FAVICON_PATH,
                    AUTHORIZE_PATH,
                ),
            );
        }
    }
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

fn redirect_authorize_error(request: Request, params: &Params, error: &str) -> Result<()> {
    let Some(redirect_uri) = params.get("redirect_uri").map(String::as_str) else {
        return respond_html(
            request,
            400,
            format!(
                "<!doctype html><title>OAuth error</title><h1>OAuth error</h1><p>{}</p>",
                escape(error)
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

impl From<OAuthError> for Error {
    fn from(err: OAuthError) -> Self {
        Error::Tool(format!("{}: {}", err.error, err.description))
    }
}

#[cfg(test)]
mod tests {
    use super::redirect_uri_allowed;

    #[test]
    fn redirect_uri_policy_allows_https_and_loopback() {
        assert!(redirect_uri_allowed(
            "https://chatgpt.com/connector/oauth/abc"
        ));
        assert!(redirect_uri_allowed("http://localhost:3000/callback"));
        assert!(redirect_uri_allowed("http://127.0.0.1:3000/callback"));
        assert!(redirect_uri_allowed("http://[::1]:3000/callback"));
        assert!(!redirect_uri_allowed("http://example.com/callback"));
    }
}
