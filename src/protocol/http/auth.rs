use crate::core::state::AppState;
use sha2::{Digest, Sha256};
use tiny_http::Request;

pub(super) fn request_authorized(state: &AppState, request: &Request) -> bool {
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
        .is_some_and(|expected| token == expected)
    {
        return true;
    }

    oauth_configured && state.oauth.lock().unwrap().access_token_valid(token)
}

pub(super) fn oauth_client_metadata(
    state: &AppState,
    request: &Request,
) -> (Option<String>, Option<String>) {
    let Some(token) = bearer_token(request) else {
        return (None, None);
    };
    let Some(client) = state.oauth.lock().unwrap().access_token_client(token) else {
        return (None, None);
    };
    (Some(client.client_id), client.client_name)
}

// A job belongs to the authenticated authorization grant, not a transient MCP
// session. Refreshing an OAuth access token retains the same grant ID.
pub(super) fn job_owner_key(state: &AppState, request: &Request) -> String {
    let Some(token) = bearer_token(request) else {
        return "http:anonymous".to_string();
    };
    if state.config.server.http_bearer_token.as_deref() == Some(token) {
        return format!("static-bearer:{}", token_fingerprint(token));
    }
    if state.config.server.oauth_enabled {
        if let Some(grant_id) = state.oauth.lock().unwrap().access_token_grant_id(token) {
            return format!("oauth-grant:{grant_id}");
        }
    }
    // Validated earlier; a just-expired credential must not inherit another
    // grant's jobs, even if its MCP session header is reused.
    format!("bearer:{}", token_fingerprint(token))
}

pub(super) fn caller_key(request: &Request) -> String {
    if let Some(session_id) = mcp_session_id(request) {
        return format!("session:{session_id}");
    }
    // Current OAuth clients may omit the optional MCP session ID; group their
    // requests by the credential fingerprint without exposing the token.
    if let Some(token) = bearer_token(request) {
        return format!("bearer:{}", token_fingerprint(token));
    }
    "http:anonymous".to_string()
}

pub(super) fn mcp_session_id(request: &Request) -> Option<String> {
    request.headers().iter().find_map(|header| {
        if !header.field.equiv("Mcp-Session-Id") {
            return None;
        }
        let value = header.value.as_str();
        (!value.is_empty()
            && value.len() <= 256
            && value.bytes().all(|byte| byte.is_ascii_graphic()))
        .then(|| value.to_string())
    })
}

fn bearer_token(request: &Request) -> Option<&str> {
    request.headers().iter().find_map(|header| {
        header
            .field
            .equiv("Authorization")
            .then(|| bearer_value(header.value.as_str()))
            .flatten()
    })
}

fn token_fingerprint(token: &str) -> String {
    hex_bytes(&Sha256::digest(token.as_bytes()))
}

fn hex_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn bearer_value(value: &str) -> Option<&str> {
    let (scheme, token) = value.split_once(' ')?;
    (scheme.eq_ignore_ascii_case("Bearer") && !token.is_empty() && !token.starts_with(' '))
        .then_some(token)
}

#[cfg(test)]
mod tests {
    use super::bearer_value;

    #[test]
    fn accepts_bearer_tokens_case_insensitively() {
        assert_eq!(bearer_value("Bearer secret-token"), Some("secret-token"));
        assert_eq!(bearer_value("bearer secret-token"), Some("secret-token"));
    }

    #[test]
    fn rejects_malformed_authorization_values() {
        assert_eq!(bearer_value("Basic secret-token"), None);
        assert_eq!(bearer_value("Bearer"), None);
        assert_eq!(bearer_value("Bearer "), None);
        assert_eq!(bearer_value("Bearer  secret-token"), None);
    }
}
