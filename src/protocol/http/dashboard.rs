use super::form;
use crate::core::{
    activity::Query,
    error::{Error, Result},
    state::AppState,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tiny_http::{Header, Method, Request, Response, StatusCode};

const HTML: &str = include_str!("../../../assets/dashboard.html");
const CSS: &str = include_str!("../../../assets/dashboard.css");
const JS: &str = include_str!("../../../assets/dashboard.js");

pub(super) fn handle(
    state: Arc<AppState>,
    request: Request,
    method: Method,
    path: &str,
) -> Result<()> {
    let Some(password) = dashboard_password(&state.config) else {
        return respond(
            request,
            404,
            "application/json",
            br#"{"error":"not found"}"#.to_vec(),
            None,
        );
    };
    if state.activity.is_none() {
        return respond(
            request,
            404,
            "application/json",
            br#"{"error":"not found"}"#.to_vec(),
            None,
        );
    }
    if !authorized(&request, password) {
        return respond(
            request,
            401,
            "application/json",
            br#"{"error":"dashboard authentication required"}"#.to_vec(),
            Some((
                "WWW-Authenticate",
                "Basic realm=\"Target Ops Dashboard\", charset=\"UTF-8\"",
            )),
        );
    }
    if method != Method::Get {
        return respond(
            request,
            405,
            "application/json",
            br#"{"error":"method not allowed"}"#.to_vec(),
            None,
        );
    }

    match path {
        "/dashboard" | "/dashboard/" => respond(
            request,
            200,
            "text/html; charset=utf-8",
            HTML.as_bytes().to_vec(),
            None,
        ),
        "/dashboard/app.css" => respond(
            request,
            200,
            "text/css; charset=utf-8",
            CSS.as_bytes().to_vec(),
            None,
        ),
        "/dashboard/app.js" => respond(
            request,
            200,
            "text/javascript; charset=utf-8",
            JS.as_bytes().to_vec(),
            None,
        ),
        "/dashboard/api" => {
            let params = match form::parse_query(request.url()) {
                Ok(params) => params,
                Err(_) => {
                    return respond(
                        request,
                        400,
                        "application/json",
                        br#"{"error":"invalid query string"}"#.to_vec(),
                        None,
                    )
                }
            };
            let query = Query {
                hours: params.get("hours").and_then(|s| s.parse().ok()),
                limit: params.get("limit").and_then(|s| s.parse().ok()),
                before: params.get("before").and_then(|s| s.parse().ok()),
                session: params.get("session").cloned(),
                client: params.get("client").cloned(),
                tool: params.get("tool").cloned(),
                status: params.get("status").cloned(),
                search: params.get("search").cloned(),
            };
            match state
                .activity
                .as_ref()
                .expect("checked enabled")
                .query(query)
            {
                Ok(data) => respond(
                    request,
                    200,
                    "application/json",
                    serde_json::to_vec(&data)?,
                    None,
                ),
                Err(err) => {
                    eprintln!("mcp-target-ops dashboard query failed: {err}");
                    respond(
                        request,
                        500,
                        "application/json",
                        br#"{"error":"dashboard query failed"}"#.to_vec(),
                        None,
                    )
                }
            }
        }
        _ => respond(
            request,
            404,
            "application/json",
            br#"{"error":"not found"}"#.to_vec(),
            None,
        ),
    }
}

// One password is enough for single-user deployments: if no dashboard-specific
// password is configured, reuse the OAuth consent password when OAuth is enabled.
// Authentication still happens separately via HTTP Basic, never with MCP tokens.
fn dashboard_password(config: &crate::core::config::Config) -> Option<&str> {
    config.activity.dashboard_password.as_deref().or_else(|| {
        config
            .server
            .oauth_enabled
            .then_some(config.server.oauth_authorization_password.as_deref())
            .flatten()
    })
}

// Browser-native Basic authentication avoids putting a credential in URL, JS or storage.
fn authorized(request: &Request, password: &str) -> bool {
    let encoded = request
        .headers()
        .iter()
        .find(|header| header.field.equiv("Authorization"))
        .and_then(|header| header.value.as_str().split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("Basic"))
        .map(|(_, encoded)| encoded);
    let Some(encoded) = encoded else {
        return false;
    };
    let Ok(provided) = STANDARD.decode(encoded) else {
        return false;
    };
    let expected = format!("admin:{password}");
    let actual_hash = Sha256::digest(provided);
    let expected_hash = Sha256::digest(expected.as_bytes());
    actual_hash
        .iter()
        .zip(expected_hash.iter())
        .fold(0u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}

fn respond(
    request: Request,
    status: u16,
    mime: &str,
    data: Vec<u8>,
    extra: Option<(&str, &str)>,
) -> Result<()> {
    let mut response = Response::from_data(data).with_status_code(StatusCode(status));
    let header = |name: &str, value: &str| -> Header {
        Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid dashboard header")
    };
    response.add_header(header("Content-Type", mime));
    response.add_header(header("Cache-Control", "private, no-store"));
    response.add_header(header("Pragma", "no-cache"));
    response.add_header(header("X-Content-Type-Options", "nosniff"));
    response.add_header(header("Referrer-Policy", "no-referrer"));
    response.add_header(header("X-Frame-Options", "DENY"));
    response.add_header(header("Content-Security-Policy",
        "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"));
    if let Some((name, value)) = extra {
        response.add_header(header(name, value));
    }
    request.respond(response).map_err(Error::Io)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The HTTP integration tests exercise unauthorized access and API data.
    #[test]
    fn dashboard_reuses_existing_oauth_consent_password_by_default() {
        let mut config = crate::core::config::Config::default();
        config.activity.dashboard_password = None;
        config.server.oauth_enabled = true;
        config.server.oauth_authorization_password = Some("same-password".into());

        assert_eq!(dashboard_password(&config), Some("same-password"));

        config.activity.dashboard_password = Some("admin-override".into());
        assert_eq!(dashboard_password(&config), Some("admin-override"));

        config.activity.dashboard_password = None;
        config.server.oauth_enabled = false;
        assert_eq!(dashboard_password(&config), None);

        config.server.oauth_enabled = true;
        config.server.oauth_authorization_password = None;
        assert_eq!(dashboard_password(&config), None);
    }

    #[test]
    fn dashboard_assets_are_embedded() {
        assert!(HTML.contains("id=\"callsTable\""));
        assert!(CSS.contains(".sidebar"));
        assert!(JS.contains("fetch("));
    }
}
