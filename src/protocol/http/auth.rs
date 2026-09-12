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

pub(super) fn caller_key(request: &Request) -> String {
    if let Some(session_id) = mcp_session_id(request) {
        return format!("session:{session_id}");
    }
    if let Some(token) = bearer_token(request) {
        let digest = Sha256::digest(token.as_bytes());
        return format!("bearer:{}", hex_bytes(&digest));
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
