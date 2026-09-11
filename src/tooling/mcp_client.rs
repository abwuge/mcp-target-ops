use crate::{
    core::{
        error::{Error, Result},
        secret::SecretRef,
        state::AppState,
        target::{TargetId, TargetSource},
    },
    tooling::secret,
};
use reqwest::{
    blocking::{Client, Response},
    header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, CONTENT_TYPE},
    redirect::Policy,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, fs, io::Read, path::Path, str::FromStr, time::Duration};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
const SESSION_HEADER: &str = "mcp-session-id";

#[derive(Debug, Clone, Serialize)]
pub struct McpServerSummary {
    pub name: String,
    pub enabled: bool,
    pub config_source: String,
    pub secret_target: Option<String>,
    pub static_header_names: Vec<String>,
    pub secret_header_names: Vec<String>,
    pub timeout_ms: u64,
    pub max_response_bytes: usize,
}

pub fn list_servers(state: &AppState) -> Vec<McpServerSummary> {
    state
        .config
        .mcp_servers
        .iter()
        .map(|(name, config)| McpServerSummary {
            name: name.clone(),
            enabled: config.enabled,
            config_source: if config.config_file.is_some() {
                "file".to_string()
            } else if config.url_secret.is_some() {
                "secret_ref".to_string()
            } else {
                "inline".to_string()
            },
            secret_target: (config.config_file.is_none()).then(|| {
                config
                    .secret_target
                    .clone()
                    .unwrap_or_else(|| "local".to_string())
            }),
            static_header_names: config.headers.keys().cloned().collect(),
            secret_header_names: config.secret_headers.keys().cloned().collect(),
            timeout_ms: config.timeout_ms,
            max_response_bytes: config.max_response_bytes,
        })
        .collect()
}

pub fn tools_list(state: &AppState, server: &str) -> Result<Value> {
    let mut client = DownstreamMcpClient::connect(state, server)?;
    client.initialize()?;
    client.request("tools/list", json!({}))
}

pub fn tool_call(state: &AppState, server: &str, tool: &str, arguments: Value) -> Result<Value> {
    if tool.trim().is_empty() {
        return Err(Error::Tool(
            "downstream MCP tool name must not be empty".to_string(),
        ));
    }
    if !arguments.is_object() {
        return Err(Error::Tool(
            "downstream MCP tool arguments must be a JSON object".to_string(),
        ));
    }

    let mut client = DownstreamMcpClient::connect(state, server)?;
    client.initialize()?;
    client.request(
        "tools/call",
        json!({
            "name": tool,
            "arguments": arguments,
        }),
    )
}

#[derive(Debug, Deserialize)]
struct McpFileConfig {
    url: String,
    #[serde(default)]
    headers: BTreeMap<String, String>,
}

struct DownstreamMcpClient {
    name: String,
    url: String,
    http: Client,
    headers: HeaderMap,
    session_id: Option<String>,
    max_response_bytes: usize,
    next_id: u64,
}

impl DownstreamMcpClient {
    fn connect(state: &AppState, name: &str) -> Result<Self> {
        let config = state.config.mcp_servers.get(name).ok_or_else(|| {
            Error::Tool(format!("downstream MCP server {name:?} is not configured"))
        })?;
        if !config.enabled {
            return Err(Error::Policy(format!(
                "downstream MCP server {name:?} is disabled"
            )));
        }

        let timeout = Duration::from_millis(config.timeout_ms);
        let (url, file_headers) = if let Some(path) = &config.config_file {
            let file_config = load_file_config(path, name)?;
            (file_config.url, file_config.headers)
        } else {
            let secret_target = config.secret_target.as_deref().unwrap_or("local");
            let target = TargetId::from_str(secret_target)?;
            let target_config = state.get_target_config(&target)?;
            let url = match (&config.url, &config.url_secret) {
                (Some(url), None) => url.clone(),
                (None, Some(secret_ref)) => {
                    resolve_secret(state, &target, target_config, secret_ref, timeout)?
                }
                _ => {
                    return Err(Error::Config(format!(
                        "mcp_servers.{name} must configure exactly one endpoint source"
                    )))
                }
            };
            (url, BTreeMap::new())
        };
        validate_url(&url, name)?;

        let mut headers = HeaderMap::new();
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/json, text/event-stream"),
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        for (header_name, value) in &file_headers {
            insert_header(&mut headers, header_name, value)?;
        }
        for (header_name, value) in &config.headers {
            insert_header(&mut headers, header_name, value)?;
        }
        if !config.secret_headers.is_empty() {
            let secret_target = config.secret_target.as_deref().unwrap_or("local");
            let target = TargetId::from_str(secret_target)?;
            let target_config = state.get_target_config(&target)?;
            for (header_name, secret_ref) in &config.secret_headers {
                let value = resolve_secret(state, &target, target_config, secret_ref, timeout)?;
                insert_header(&mut headers, header_name, &value)?;
            }
        }

        let http = Client::builder()
            .timeout(timeout)
            .redirect(Policy::none())
            .build()
            .map_err(|err| Error::Tool(format!("failed to build downstream MCP client: {err}")))?;

        Ok(Self {
            name: name.to_string(),
            url,
            http,
            headers,
            session_id: None,
            max_response_bytes: config.max_response_bytes,
            next_id: 1,
        })
    }

    fn initialize(&mut self) -> Result<()> {
        let result = self.request(
            "initialize",
            json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {
                    "name": "mcp-target-ops",
                    "version": env!("CARGO_PKG_VERSION"),
                }
            }),
        )?;

        let selected = result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or(MCP_PROTOCOL_VERSION);
        if selected.trim().is_empty() {
            return Err(Error::Tool(format!(
                "downstream MCP server {:?} returned an empty protocol version",
                self.name
            )));
        }

        self.notify("notifications/initialized", json!({}))
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let response = self.post(
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            }),
            true,
        )?;

        let message = response.ok_or_else(|| {
            Error::Tool(format!(
                "downstream MCP server {:?} returned no JSON-RPC response for {method}",
                self.name
            ))
        })?;
        if let Some(error) = message.get("error") {
            return Err(Error::Tool(format!(
                "downstream MCP server {:?} returned an error for {method}: {}",
                self.name,
                compact_json(error)
            )));
        }
        message.get("result").cloned().ok_or_else(|| {
            Error::Tool(format!(
                "downstream MCP server {:?} response for {method} has no result",
                self.name
            ))
        })
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.post(
            json!({
                "jsonrpc": "2.0",
                "method": method,
                "params": params,
            }),
            false,
        )?;
        Ok(())
    }

    fn post(&mut self, payload: Value, expect_reply: bool) -> Result<Option<Value>> {
        let mut request = self
            .http
            .post(&self.url)
            .headers(self.headers.clone())
            .json(&payload);
        if let Some(session_id) = &self.session_id {
            request = request.header(SESSION_HEADER, session_id);
        }

        let response = request.send().map_err(|err| {
            Error::Tool(format!(
                "downstream MCP request to {:?} failed: {err}",
                self.name
            ))
        })?;
        self.handle_response(response, expect_reply)
    }

    fn handle_response(&mut self, response: Response, expect_reply: bool) -> Result<Option<Value>> {
        let status = response.status();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        if let Some(session_id) = response
            .headers()
            .get(SESSION_HEADER)
            .and_then(|value| value.to_str().ok())
        {
            self.session_id = Some(session_id.to_string());
        }

        let body = read_bounded(response, self.max_response_bytes)?;
        if !status.is_success() {
            return Err(Error::Tool(format!(
                "downstream MCP server {:?} returned HTTP {}: {}",
                self.name,
                status.as_u16(),
                String::from_utf8_lossy(&body).trim()
            )));
        }
        if !expect_reply {
            return Ok(None);
        }
        if body.is_empty() {
            return Ok(None);
        }

        if content_type.contains("application/json") {
            return serde_json::from_slice(&body)
                .map(Some)
                .map_err(|err| Error::Tool(format!("invalid downstream MCP JSON: {err}")));
        }
        if content_type.contains("text/event-stream") {
            let text = String::from_utf8(body)?;
            return parse_sse_message(&text).map(Some);
        }

        Err(Error::Tool(format!(
            "downstream MCP server {:?} returned unsupported content-type {:?}",
            self.name, content_type
        )))
    }
}

fn resolve_secret(
    state: &AppState,
    target: &TargetId,
    target_config: &crate::core::config::TargetConfig,
    secret_ref: &SecretRef,
    timeout: Duration,
) -> Result<String> {
    secret::resolve_ref(
        state,
        target,
        target_config,
        TargetSource::Explicit,
        secret_ref,
        timeout,
    )
}

fn load_file_config(path: &Path, name: &str) -> Result<McpFileConfig> {
    #[cfg(unix)]
    {
        let mode = fs::metadata(path)?.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(Error::Policy(format!(
                "downstream MCP config {} for {name:?} must not be group/world accessible; use mode 0600",
                path.display()
            )));
        }
    }

    let text = fs::read_to_string(path).map_err(|err| {
        Error::Config(format!(
            "failed to read downstream MCP config {} for {name:?}: {err}",
            path.display()
        ))
    })?;
    let config: McpFileConfig = toml::from_str(&text).map_err(|err| {
        Error::Config(format!(
            "failed to parse downstream MCP config {} for {name:?}: {err}",
            path.display()
        ))
    })?;
    if config.url.trim().is_empty() || config.url.trim() != config.url {
        return Err(Error::Config(format!(
            "downstream MCP config {} for {name:?} has an empty or padded url",
            path.display()
        )));
    }
    Ok(config)
}

fn insert_header(headers: &mut HeaderMap, name: &str, value: &str) -> Result<()> {
    let name = HeaderName::from_bytes(name.as_bytes()).map_err(|err| {
        Error::Config(format!(
            "invalid downstream MCP header name {name:?}: {err}"
        ))
    })?;
    let value = HeaderValue::from_str(value)
        .map_err(|err| Error::Config(format!("invalid downstream MCP header value: {err}")))?;
    headers.insert(name, value);
    Ok(())
}

fn validate_url(url: &str, name: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|err| Error::Config(format!("mcp_servers.{name} has invalid URL: {err}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(Error::Config(format!(
            "mcp_servers.{name} URL must use http or https"
        )));
    }
    if parsed.username() != "" || parsed.password().is_some() {
        return Err(Error::Config(format!(
            "mcp_servers.{name} URL must not contain userinfo credentials"
        )));
    }
    Ok(())
}

fn read_bounded(mut response: Response, max_bytes: usize) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    let mut limited = response.by_ref().take(max_bytes.saturating_add(1) as u64);
    limited.read_to_end(&mut body)?;
    if body.len() > max_bytes {
        return Err(Error::Tool(format!(
            "downstream MCP response exceeded max_response_bytes ({max_bytes})"
        )));
    }
    Ok(body)
}

fn parse_sse_message(text: &str) -> Result<Value> {
    let normalized = text.replace("\r\n", "\n");
    for block in normalized.split("\n\n") {
        let data: Vec<&str> = block
            .lines()
            .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
            .collect();
        if data.is_empty() {
            continue;
        }
        let message: Value = serde_json::from_str(&data.join("\n"))
            .map_err(|err| Error::Tool(format!("invalid downstream MCP SSE data: {err}")))?;
        if message.get("result").is_some() || message.get("error").is_some() {
            return Ok(message);
        }
    }
    Err(Error::Tool(
        "downstream MCP SSE response contained no JSON-RPC result".to_string(),
    ))
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<unserializable error>".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sse_json_rpc_response() {
        let value = parse_sse_message(
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n\n",
        )
        .unwrap();
        assert_eq!(value["result"]["ok"], true);
    }

    #[test]
    fn rejects_non_http_urls() {
        assert!(validate_url("file:///tmp/x", "bad").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn file_config_requires_private_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.toml");
        fs::write(
            &path,
            "url = \"https://example.com/mcp\"\n[headers]\nAuthorization = \"Bearer hidden\"\n",
        )
        .unwrap();

        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let loaded = load_file_config(&path, "memory").unwrap();
        assert_eq!(loaded.url, "https://example.com/mcp");
        assert!(loaded.headers.contains_key("Authorization"));

        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load_file_config(&path, "memory").is_err());
    }
}
