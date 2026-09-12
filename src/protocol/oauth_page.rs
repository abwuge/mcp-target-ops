use crate::core::state::AppState;
use std::collections::BTreeMap;

use super::html::escape;

const TEMPLATE: &str = include_str!("../../assets/oauth-authorize.html");
const UNKNOWN_CLIENT: &str = "MCP client";

pub(super) fn render(
    state: &AppState,
    params: &BTreeMap<String, String>,
    error: Option<&str>,
    favicon_path: &str,
    authorize_path: &str,
) -> String {
    let hidden_inputs = params
        .iter()
        .filter(|(key, _)| !matches!(key.as_str(), "password" | "decision"))
        .map(|(key, value)| {
            format!(
                r#"<input type="hidden" name="{}" value="{}">"#,
                escape(key),
                escape(value)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let client_id = params
        .get("client_id")
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("Client ID not provided");
    let client_name = {
        let oauth = state.oauth.lock().unwrap();
        oauth
            .client_name(client_id)
            .filter(|value| !value.trim().is_empty())
            .map(ToOwned::to_owned)
    }
    .unwrap_or_else(|| UNKNOWN_CLIENT.to_string());
    let redirect_uri = params
        .get("redirect_uri")
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("Redirect URI not provided");
    let scope = params
        .get("scope")
        .cloned()
        .unwrap_or_else(|| state.config.server.oauth_scopes.join(" "));
    let scope_chips = render_scope_chips(&scope);
    let error_block = error
        .map(|message| {
            format!(
                r#"<div class="error" role="alert">{}</div>"#,
                escape(message)
            )
        })
        .unwrap_or_default();

    TEMPLATE
        .replace("@@SERVER_NAME@@", &escape(&state.config.server.name))
        .replace("@@CLIENT_NAME@@", &escape(&client_name))
        .replace("@@CLIENT_ID@@", &escape(client_id))
        .replace("@@REDIRECT_URI@@", &escape(redirect_uri))
        .replace("@@FAVICON_PATH@@", favicon_path)
        .replace("@@SCOPE_CHIPS@@", &scope_chips)
        .replace("@@AUTHORIZE_PATH@@", authorize_path)
        .replace("@@HIDDEN_INPUTS@@", &hidden_inputs)
        .replace("@@ERROR_BLOCK@@", &error_block)
        .replace(
            "@@ARIA_INVALID@@",
            if error.is_some() { "true" } else { "false" },
        )
}

fn render_scope_chips(scope: &str) -> String {
    let chips = scope
        .split_whitespace()
        .map(|item| format!(r#"<span class="scope-chip">{}</span>"#, escape(item)))
        .collect::<Vec<_>>();

    if chips.is_empty() {
        r#"<span class="scope-chip">No scopes requested</span>"#.to_string()
    } else {
        chips.join("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::Config;
    use tempfile::tempdir;

    #[test]
    fn renders_registered_client_and_error_without_echoing_secrets() {
        let temp = tempdir().expect("temporary directory");
        let mut config = Config::default();
        config.server.name = "Target <Ops>".to_string();
        config.server.runtime_dir = temp.path().join("runtime");
        config.server.oauth_state_file = None;
        let state = AppState::new(config).expect("state initializes");
        let client = state
            .oauth
            .lock()
            .unwrap()
            .register_client(
                Some("ChatGPT <Desktop>".to_string()),
                vec!["https://chatgpt.com/connector/oauth/callback".to_string()],
            )
            .expect("client registers");

        let mut params = BTreeMap::new();
        params.insert("client_id".to_string(), client.client_id);
        params.insert("scope".to_string(), "mcp:tools files:read".to_string());
        params.insert(
            "redirect_uri".to_string(),
            "https://chatgpt.com/connector/oauth/callback".to_string(),
        );
        params.insert("password".to_string(), "do-not-render".to_string());
        params.insert("decision".to_string(), "approve".to_string());

        let page = render(
            &state,
            &params,
            Some("Invalid <password>. Please try again."),
            "/favicon.ico",
            "/oauth/authorize",
        );

        assert!(page.contains("Connect ChatGPT &lt;Desktop&gt;"));
        assert!(page.contains("Target &lt;Ops&gt;"));
        assert!(page.contains("class=\"scope-chip\">mcp:tools"));
        assert!(page.contains("class=\"scope-chip\">files:read"));
        assert!(page.contains("https://chatgpt.com/connector/oauth/callback"));
        assert!(page.contains("role=\"alert\">Invalid &lt;password&gt;. Please try again."));
        assert!(page.contains("aria-invalid=\"true\""));
        assert!(page.contains("name=\"decision\" value=\"deny\""));
        assert!(page.contains("name=\"decision\" value=\"approve\""));
        assert!(!page.contains("do-not-render"));
        assert!(!page.contains("type=\"hidden\" name=\"decision\""));
        assert!(!page.contains("@@"));
    }
}
