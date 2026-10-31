use super::form;
use crate::{
    core::{
        activity::Query,
        config::Config,
        error::{Error, Result},
        state::AppState,
    },
    protocol::{html::escape, oauth_page},
};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::Read,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use tiny_http::{Header, Method, Request, Response, StatusCode};

const HTML: &str = include_str!("../../../assets/dashboard.html");
const LOGIN: &str = include_str!("../../../assets/dashboard-login.html");
const CSS: &str = include_str!("../../../assets/dashboard.css");
const JS: &str = include_str!("../../../assets/dashboard.js");
const COOKIE_NAME: &str = "target_ops_dashboard";
const COOKIE_LIFETIME_SECS: u64 = 12 * 60 * 60;
const MAX_LOGIN_BODY: u64 = 4096;
const MAX_FAILED_PER_WINDOW: usize = 12;
const FAILURE_WINDOW: Duration = Duration::from_secs(5 * 60);

static FAILED_LOGINS: OnceLock<Mutex<HashMap<String, (Instant, usize)>>> = OnceLock::new();

pub(super) fn handle(
    state: Arc<AppState>,
    mut request: Request,
    method: Method,
    path: &str,
) -> Result<()> {
    let Some(password) = dashboard_password(&state.config) else {
        return respond(
            request,
            404,
            "application/json",
            br#"{"error":"not found"}"#.to_vec(),
            false,
        );
    };
    let Some(store) = state.activity.as_ref() else {
        return respond(
            request,
            404,
            "application/json",
            br#"{"error":"not found"}"#.to_vec(),
            false,
        );
    };

    if method == Method::Post && path == "/dashboard/login" {
        let mut body = Vec::new();
        request
            .as_reader()
            .take(MAX_LOGIN_BODY + 1)
            .read_to_end(&mut body)?;
        if body.len() as u64 > MAX_LOGIN_BODY {
            return respond(
                request,
                413,
                "text/plain",
                b"Request too large".to_vec(),
                false,
            );
        }
        let params = form::parse_urlencoded(std::str::from_utf8(&body).unwrap_or_default());
        let submitted = params
            .ok()
            .and_then(|params| params.get("password").cloned());
        let right_password = submitted
            .as_deref()
            .is_some_and(|candidate| constant_time_eq(candidate.as_bytes(), password.as_bytes()));
        let ip = request
            .remote_addr()
            .map(|addr| addr.ip().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        if !right_password {
            let limited = record_failed_attempt(&ip);
            return login_page(
                request,
                &state.config.server.name,
                Some(if limited {
                    "尝试次数过多，请稍后重试。"
                } else {
                    "密码不正确，请重试。"
                }),
                if limited { 429 } else { 401 },
            );
        }
        clear_failed_attempts(&ip);
        let token = store.issue_dashboard_session(password)?;
        return redirect(
            request,
            "/dashboard",
            Some(cookie_header(&token, &state.config)),
        );
    }

    let session_token = browser_cookie(&request);
    let logged_in = match session_token.as_deref() {
        Some(token) => store.dashboard_session_valid(token, password)?,
        None => false,
    };

    if method == Method::Get && matches!(path, "/dashboard" | "/dashboard/") && !logged_in {
        return login_page(request, &state.config.server.name, None, 200);
    }
    if !logged_in {
        return respond(
            request,
            401,
            "application/json",
            br#"{"error":"dashboard login required"}"#.to_vec(),
            false,
        );
    }
    if method == Method::Post && path == "/dashboard/logout" {
        if let Some(token) = session_token {
            store.revoke_dashboard_session(&token)?;
        }
        return redirect(
            request,
            "/dashboard",
            Some(cookie_header("", &state.config)),
        );
    }
    if method != Method::Get {
        return respond(
            request,
            405,
            "application/json",
            br#"{"error":"method not allowed"}"#.to_vec(),
            false,
        );
    }

    match path {
        "/dashboard" | "/dashboard/" => respond(
            request,
            200,
            "text/html; charset=utf-8",
            HTML.as_bytes().to_vec(),
            false,
        ),
        "/dashboard/app.css" => respond(
            request,
            200,
            "text/css; charset=utf-8",
            CSS.as_bytes().to_vec(),
            false,
        ),
        "/dashboard/app.js" => respond(
            request,
            200,
            "text/javascript; charset=utf-8",
            JS.as_bytes().to_vec(),
            false,
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
                        false,
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
            match store.query(query) {
                Ok(data) => respond(
                    request,
                    200,
                    "application/json",
                    serde_json::to_vec(&data)?,
                    false,
                ),
                Err(err) => {
                    eprintln!("mcp-target-ops dashboard query failed: {err}");
                    respond(
                        request,
                        500,
                        "application/json",
                        br#"{"error":"dashboard query failed"}"#.to_vec(),
                        false,
                    )
                }
            }
        }
        _ => respond(
            request,
            404,
            "application/json",
            br#"{"error":"not found"}"#.to_vec(),
            false,
        ),
    }
}

fn dashboard_password(config: &Config) -> Option<&str> {
    config.activity.dashboard_password.as_deref().or_else(|| {
        config
            .server
            .oauth_enabled
            .then_some(config.server.oauth_authorization_password.as_deref())
            .flatten()
    })
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    // Compare hashes, so verification time is not affected by password length.
    let x = Sha256::digest(a);
    let y = Sha256::digest(b);
    x.iter()
        .zip(y.iter())
        .fold(0u8, |diff, (&a, &b)| diff | (a ^ b))
        == 0
}

fn failed_logins() -> &'static Mutex<HashMap<String, (Instant, usize)>> {
    FAILED_LOGINS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn record_failed_attempt(ip: &str) -> bool {
    let mut attempts = failed_logins().lock().unwrap();
    let now = Instant::now();
    attempts.retain(|_, (start, _)| now.duration_since(*start) < FAILURE_WINDOW);
    let entry = attempts.entry(ip.to_string()).or_insert((now, 0));
    entry.1 += 1;
    entry.1 > MAX_FAILED_PER_WINDOW
}

fn clear_failed_attempts(ip: &str) {
    failed_logins().lock().unwrap().remove(ip);
}

fn browser_cookie(request: &Request) -> Option<String> {
    for header in request.headers() {
        if !header.field.equiv("Cookie") {
            continue;
        }
        for part in header.value.as_str().split(';') {
            let Some((name, token)) = part.trim().split_once('=') else {
                continue;
            };
            if name == COOKIE_NAME
                && token.len() == 43
                && token
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            {
                return Some(token.to_owned());
            }
        }
    }
    None
}

fn cookie_header(token: &str, config: &Config) -> String {
    let secure = if config
        .server
        .public_base_url
        .as_ref()
        .is_some_and(|url| url.starts_with("https://"))
    {
        "; Secure"
    } else {
        ""
    };
    let lifetime = if token.is_empty() {
        0
    } else {
        COOKIE_LIFETIME_SECS
    };
    format!("{COOKIE_NAME}={token}; Path=/dashboard; HttpOnly; SameSite=Strict; Max-Age={lifetime}{secure}")
}

fn login_page(request: Request, name: &str, error: Option<&str>, status: u16) -> Result<()> {
    let error_block = error
        .map(|error| {
            format!(
                "<div class=\"error\" role=\"alert\">{}</div>",
                escape(error)
            )
        })
        .unwrap_or_default();
    let html = LOGIN
        .replace("@@SERVER_NAME@@", &escape(name))
        .replace("@@AUTH_STYLES@@", oauth_page::shared_auth_styles())
        .replace("@@ERROR_BLOCK@@", &error_block)
        .replace(
            "@@ARIA_INVALID@@",
            if error.is_some() { "true" } else { "false" },
        );
    respond(
        request,
        status,
        "text/html; charset=utf-8",
        html.into_bytes(),
        true,
    )
}

fn redirect(request: Request, path: &str, cookie: Option<String>) -> Result<()> {
    let mut response = Response::empty(StatusCode(303));
    add_headers(&mut response, false);
    response.add_header(header("Location", path));
    if let Some(cookie) = cookie {
        response.add_header(header("Set-Cookie", &cookie));
    }
    request.respond(response).map_err(Error::Io)
}

fn respond(request: Request, status: u16, mime: &str, data: Vec<u8>, login: bool) -> Result<()> {
    let mut response = Response::from_data(data).with_status_code(StatusCode(status));
    response.add_header(header("Content-Type", mime));
    add_headers(&mut response, login);
    request.respond(response).map_err(Error::Io)
}

fn add_headers<R: Read>(response: &mut Response<R>, login: bool) {
    response.add_header(header("Cache-Control", "private, no-store"));
    response.add_header(header("Pragma", "no-cache"));
    response.add_header(header("X-Content-Type-Options", "nosniff"));
    response.add_header(header("Referrer-Policy", "no-referrer"));
    response.add_header(header("X-Frame-Options", "DENY"));
    response.add_header(header("Content-Security-Policy", if login {
        "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'"
    } else {
        "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'"
    }));
}
fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid dashboard header")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dashboard_reuses_existing_oauth_consent_password_by_default() {
        let mut config = Config::default();
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
    fn login_page_reuses_oauth_styles_without_usernames() {
        assert!(LOGIN.contains("name=\"password\""));
        assert!(!LOGIN.contains("name=\"username\""));
        assert!(oauth_page::shared_auth_styles().contains(".password-field"));
        assert!(HTML.contains("id=\"callsTable\""));
        assert!(CSS.contains(".sidebar"));
        assert!(JS.contains("fetch("));
    }

    #[test]
    fn cookies_are_host_only_and_https_aware() {
        let mut config = Config::default();
        config.server.public_base_url = Some("https://service.example.com".into());
        let cookie = cookie_header("abc", &config);
        assert!(cookie.contains("HttpOnly; SameSite=Strict"));
        assert!(cookie.contains("; Secure"));
        assert!(cookie.contains("Path=/dashboard"));
        assert!(!cookie.contains("Domain="));
        assert!(cookie_header("", &config).contains("Max-Age=0"));
    }
}
