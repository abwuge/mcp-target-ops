use crate::{
    core::{
        activity::{self, Caller, Event},
        error::{Error, Result},
        state::AppState,
    },
    tooling::tools,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    io::{self, BufRead, Write},
    sync::Arc,
    time::{Instant, UNIX_EPOCH},
};

#[derive(Debug, Deserialize)]
struct RpcRequest {
    #[serde(default, rename = "jsonrpc")]
    _jsonrpc: Option<String>,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

#[derive(Debug, Serialize)]
struct RpcResponse {
    jsonrpc: &'static str,
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
struct RpcError {
    code: i64,
    message: String,
}

pub fn serve_stdio(state: Arc<AppState>) -> Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        if let Some(response) = handle_json_bytes(Arc::clone(&state), line.as_bytes())? {
            stdout.write_all(&response)?;
            stdout.write_all(b"\n")?;
            stdout.flush()?;
        }
    }

    Ok(())
}

pub fn handle_json_bytes(state: Arc<AppState>, bytes: &[u8]) -> Result<Option<Vec<u8>>> {
    handle_json_bytes_for_caller(state, bytes, "direct")
}

pub fn handle_json_bytes_for_caller(
    state: Arc<AppState>,
    bytes: &[u8],
    caller_key: &str,
) -> Result<Option<Vec<u8>>> {
    let caller = caller_for_key(&state, caller_key);
    handle_json_bytes_for_context(state, bytes, &caller)
}

fn caller_for_key(state: &AppState, key: &str) -> Caller {
    let epoch = state
        .started_at()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    Caller::from_key(key, &format!("{}-{epoch}", std::process::id()))
}

pub fn handle_json_bytes_for_context(
    state: Arc<AppState>,
    bytes: &[u8],
    caller: &Caller,
) -> Result<Option<Vec<u8>>> {
    let value = match serde_json::from_slice::<Value>(bytes) {
        Ok(value) => value,
        Err(err) => {
            let response = parse_error(format!("parse error: {err}"));
            return Ok(Some(serde_json::to_vec(&response)?));
        }
    };

    handle_json_value_for_context(state, value, caller)?
        .map(|response| serde_json::to_vec(&response))
        .transpose()
        .map_err(Error::Json)
}

#[cfg(test)]
fn handle_json_value_for_caller(
    state: Arc<AppState>,
    value: Value,
    caller_key: &str,
) -> Result<Option<Value>> {
    let caller = caller_for_key(&state, caller_key);
    handle_json_value_for_context(state, value, &caller)
}

fn handle_json_value_for_context(
    state: Arc<AppState>,
    value: Value,
    caller: &Caller,
) -> Result<Option<Value>> {
    if let Value::Array(requests) = value {
        if requests.is_empty() {
            return serde_json::to_value(parse_error("parse error: empty batch".to_string()))
                .map(Some)
                .map_err(Error::Json);
        }

        let mut responses = Vec::new();
        for request in requests {
            if let Some(response) = handle_request_value(Arc::clone(&state), request, caller)? {
                responses.push(response);
            }
        }

        if responses.is_empty() {
            Ok(None)
        } else {
            Ok(Some(Value::Array(responses)))
        }
    } else {
        handle_request_value(state, value, caller)
    }
}

fn handle_request_value(
    state: Arc<AppState>,
    value: Value,
    caller: &Caller,
) -> Result<Option<Value>> {
    let response = match serde_json::from_value::<RpcRequest>(value) {
        Ok(request) => handle_request(state, request, caller),
        Err(err) => Some(parse_error(format!("parse error: {err}"))),
    };

    response
        .map(serde_json::to_value)
        .transpose()
        .map_err(Error::Json)
}

fn parse_error(message: String) -> RpcResponse {
    RpcResponse {
        jsonrpc: "2.0",
        id: None,
        result: None,
        error: Some(RpcError {
            code: -32700,
            message,
        }),
    }
}

fn handle_request(
    state: Arc<AppState>,
    request: RpcRequest,
    caller: &Caller,
) -> Option<RpcResponse> {
    if request.method.starts_with("notifications/") {
        return None;
    }
    let started_ms = activity::now_ms();
    let start = Instant::now();
    let mut observed_caller = caller.clone();
    if request.method == "initialize" {
        let info = request.params.as_ref().and_then(|p| p.get("clientInfo"));
        observed_caller.client_name = info
            .and_then(|v| v.get("name"))
            .and_then(Value::as_str)
            .map(str::to_string);
        observed_caller.client_version = info
            .and_then(|v| v.get("version"))
            .and_then(Value::as_str)
            .map(str::to_string);
    }
    let tool_params = (request.method == "tools/call")
        .then_some(request.params.as_ref())
        .flatten();
    let name = tool_params
        .and_then(|p| p.get("name"))
        .and_then(Value::as_str)
        .unwrap_or(&request.method)
        .to_string();
    let arguments = tool_params.and_then(|p| p.get("arguments"));
    let mut keys: Vec<String> = arguments
        .and_then(Value::as_object)
        .map(|obj| {
            obj.keys()
                .take(32)
                .map(|key| key.chars().take(64).collect::<String>())
                .collect()
        })
        .unwrap_or_default();
    keys.sort();
    let requested_target = arguments
        .and_then(|a| a.get("target"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let kind = if request.method == "tools/call" {
        "tool"
    } else {
        "protocol"
    };

    let id = request.id.clone();
    let result = match request.method.as_str() {
        "initialize" => initialize(&state, request.params.unwrap_or_else(|| json!({}))),
        "tools/list" => Ok(json!({ "tools": tools::list_tools(
            state.config.server.oauth_enabled
                .then_some(state.config.server.oauth_scopes.as_slice())
        ) })),
        "tools/call" => tools_call(
            Arc::clone(&state),
            request.params.unwrap_or_else(|| json!({})),
            &caller.key,
        ),
        "ping" => Ok(json!({})),
        other => Err(Error::Tool(format!("unsupported method: {other}"))),
    };
    let response = match result {
        Ok(value) => RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(value),
            error: None,
        },
        Err(err) => RpcResponse {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(RpcError {
                code: err.json_rpc_code(),
                message: err.to_string(),
            }),
        },
    };
    if let Some(store) = &state.activity {
        let errored = response.error.is_some()
            || response
                .result
                .as_ref()
                .and_then(|v| v.get("isError"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
        let response_target = response
            .result
            .as_ref()
            .and_then(|v| v.get("structuredContent"))
            .and_then(|v| v.get("resolved_target"))
            .and_then(|v| v.get("target"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let event = Event {
            caller: observed_caller,
            started_ms,
            duration_ms: start.elapsed().as_secs_f64() * 1000.0,
            kind,
            name,
            status: if errored { "error" } else { "ok" },
            target: requested_target.or(response_target),
            argument_keys: keys,
            rpc_error_code: response.error.as_ref().map(|err| err.code),
        };
        if let Err(err) = store.record(event) {
            eprintln!("mcp-target-ops activity log failed: {err}");
        }
    }
    Some(response)
}

fn initialize(state: &AppState, params: Value) -> Result<Value> {
    let requested_protocol = params
        .get("protocolVersion")
        .and_then(|v| v.as_str())
        .unwrap_or("2025-06-18");

    Ok(json!({
        "protocolVersion": requested_protocol,
        "capabilities": {
            "tools": { "listChanged": false }
        },
        "serverInfo": {
            "name": state.config.server.name.clone(),
            "version": state.config.server.version.clone(),
        }
    }))
}

fn tools_call(state: Arc<AppState>, params: Value, caller_key: &str) -> Result<Value> {
    #[derive(Deserialize)]
    struct ToolCallParams {
        name: String,
        #[serde(default)]
        arguments: Option<Value>,
    }

    let params: ToolCallParams = serde_json::from_value(params)?;
    let attach_completed = !matches!(params.name.as_str(), "job_poll" | "job_output" | "job_wait");
    match tools::call_tool_with_context(
        Arc::clone(&state),
        &params.name,
        params.arguments.unwrap_or_else(|| json!({})),
        caller_key,
    ) {
        Ok(mut value) => {
            let completed_jobs = if attach_completed {
                state.jobs.take_completed_for_caller(caller_key)
            } else {
                Vec::new()
            };
            if !completed_jobs.is_empty() {
                if let Some(object) = value.as_object_mut() {
                    object.insert(
                        "completed_jobs".to_string(),
                        serde_json::to_value(&completed_jobs)?,
                    );
                }
            }
            // COMPAT(COMPAT-009): Explicit attachment delivery keeps the original
            // embedded-resource result for clients that still need host materialization.
            let content = if params.name == "file_export"
                && value.get("delivery").and_then(Value::as_str) == Some("attachment")
            {
                let file = value
                    .get_mut("file")
                    .and_then(Value::as_object_mut)
                    .ok_or_else(|| {
                        Error::Tool(
                            "attachment file_export result is missing file metadata".to_string(),
                        )
                    })?;
                let blob = file
                    .remove("data_base64")
                    .and_then(|value| value.as_str().map(str::to_string))
                    .ok_or_else(|| {
                        Error::Tool(
                            "attachment file_export result is missing encoded file data"
                                .to_string(),
                        )
                    })?;
                let mime_type = file
                    .get("mime_type")
                    .and_then(Value::as_str)
                    .unwrap_or("application/octet-stream");
                let file_name = file
                    .get("file_name")
                    .and_then(Value::as_str)
                    .unwrap_or("export.bin");
                let sha256 = file
                    .get("sha256")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                vec![json!({
                    "type": "resource",
                    "resource": {
                        "uri": format!("target-ops://export/{sha256}/{file_name}"),
                        "mimeType": mime_type,
                        "blob": blob,
                    }
                })]
            } else {
                Vec::new()
            };

            let mut content = content;
            if !completed_jobs.is_empty() {
                content.push(json!({
                    "type": "text",
                    "text": format!(
                        "{} background job{} completed; full results are in structuredContent.completed_jobs.",
                        completed_jobs.len(),
                        if completed_jobs.len() == 1 { "" } else { "s" }
                    ),
                }));
            }

            Ok(json!({
                "content": content,
                "structuredContent": value,
                "isError": false,
            }))
        }
        Err(err) => {
            let completed_jobs = if attach_completed {
                state.jobs.take_completed_for_caller(caller_key)
            } else {
                Vec::new()
            };
            let mut content = vec![json!({
                "type": "text",
                "text": err.to_string(),
            })];
            if !completed_jobs.is_empty() {
                content.push(json!({
                    "type": "text",
                    "text": format!(
                        "{} background job{} completed; full results are in structuredContent.completed_jobs.",
                        completed_jobs.len(),
                        if completed_jobs.len() == 1 { "" } else { "s" }
                    ),
                }));
            }
            let mut response = json!({
                "content": content,
                "isError": true,
            });
            if !completed_jobs.is_empty() {
                response["structuredContent"] = json!({ "completed_jobs": completed_jobs });
            }
            Ok(response)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::{Config, TargetConfig};
    use crate::tooling::job::JobPollRequest;
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
    use std::time::Duration;
    use tempfile::tempdir;

    fn test_state() -> Arc<AppState> {
        let temp = tempdir().unwrap();
        let path = temp.keep();
        let mut config = Config::default();
        if let Some(TargetConfig::Local(local)) = config.targets.get_mut("local") {
            local.enabled = true;
            local.policy.allow_exec = true;
            local.policy.allow_file_read = true;
            local.policy.allowed_roots = vec![path.to_string_lossy().to_string()];
        }
        config.server.default_target = Some("local".to_string());
        config.server.public_base_url = Some("https://files.example.test".to_string());
        config.server.oauth_state_file = None;
        config.server.runtime_dir = path.join("runtime");
        config.activity.database_path = path.join("activity/calls.sqlite3");
        Arc::new(AppState::new(config).unwrap())
    }

    fn call_tool_for(
        state: Arc<AppState>,
        caller_key: &str,
        id: u64,
        name: &str,
        arguments: Value,
    ) -> Value {
        handle_json_value_for_caller(
            state,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": { "name": name, "arguments": arguments }
            }),
            caller_key,
        )
        .unwrap()
        .unwrap()
    }

    #[test]
    fn activity_correlates_client_and_mcp_session_without_storing_values() {
        let state = test_state();
        let init = handle_json_value_for_caller(
            Arc::clone(&state),
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":"2025-06-18",
                "clientInfo":{"name":"Client A","version":"2.1"}
            }}),
            "session:observable-a",
        )
        .unwrap()
        .unwrap();
        assert_eq!(init["error"], Value::Null);
        let successful = call_tool_for(
            Arc::clone(&state),
            "session:observable-a",
            2,
            "target_current",
            json!({"password":"NEVER_SAVE_THIS_VALUE"}),
        );
        assert_eq!(successful["result"]["isError"], false);
        let failed = call_tool_for(
            Arc::clone(&state),
            "session:observable-a",
            3,
            "no_such_tool",
            json!({}),
        );
        assert_eq!(failed["result"]["isError"], true);

        let data = state
            .activity
            .as_ref()
            .unwrap()
            .query(Default::default())
            .unwrap();
        assert_eq!(data["summary"]["count"], 3);
        assert_eq!(data["summary"]["sessions"], 1);
        assert_eq!(data["summary"]["errors"], 1);
        assert_eq!(data["sessions"][0]["client"], "Client A");
        assert_eq!(data["sessions"][0]["client_version"], "2.1");
        assert_eq!(data["sessions"][0]["grouping"], "mcp_session");
        assert_eq!(data["calls"][1]["argument_keys"], json!(["password"]));
        assert!(!data.to_string().contains("NEVER_SAVE_THIS_VALUE"));
    }

    #[test]
    fn protocol_exposes_tools_without_app_resources() {
        let state = test_state();
        let initialized = initialize(&state, json!({})).unwrap();
        assert!(initialized["capabilities"].get("resources").is_none());
        for method in ["resources/list", "resources/read"] {
            let response = handle_json_value_for_caller(
                Arc::clone(&state),
                json!({"jsonrpc":"2.0","id":1,"method":method,"params":{"uri":"ui://target-ops/panel/v1.html"}}),
                "direct",
            ).unwrap().unwrap();
            assert!(response["error"].is_object());
        }
        for name in ["panel_open", "panel_read", "result_read"] {
            let response = call_tool_for(Arc::clone(&state), "direct", 1, name, json!({}));
            assert_eq!(response["result"]["isError"], true);
        }
        let response = call_tool_for(state, "direct", 1, "target_list", json!({}));
        assert_eq!(response["result"]["isError"], false);
        assert!(response["result"]["structuredContent"]
            .get("result_id")
            .is_none());
    }

    #[test]
    fn target_operations_ignore_adjacent_agents_file() {
        let original = test_state();
        let root = original.config.server.runtime_dir.parent().unwrap();
        let config_path = root.join("config.toml");
        let agents = root.join("AGENTS.md");
        let content = b"RULE-SENTINEL: target operations previously required reading this file.\n";
        std::fs::write(&agents, content).unwrap();
        std::fs::write(&config_path, toml::to_string(&original.config).unwrap()).unwrap();
        let state = Arc::new(AppState::new(Config::load_from_path(&config_path).unwrap()).unwrap());

        let initialized = initialize(&state, json!({})).unwrap();
        assert!(initialized.get("instructions").is_none());
        for caller in ["session:a", "session:b"] {
            let result = call_tool_for(
                Arc::clone(&state),
                caller,
                1,
                "file_list",
                json!({ "target": "local", "path": root }),
            );
            assert_eq!(result["result"]["isError"], false);
        }
        let removed = call_tool_for(state, "session:a", 2, "target_instructions", json!({}));
        assert_eq!(removed["result"]["isError"], true);
        assert!(removed["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("unknown tool: target_instructions"));
        assert_eq!(std::fs::read(&agents).unwrap(), content);
    }

    #[test]
    fn link_file_export_has_no_embedded_resource() {
        let state = test_state();
        let source = state
            .config
            .server
            .runtime_dir
            .parent()
            .unwrap()
            .join("export.txt");
        std::fs::write(&source, b"link export").unwrap();

        let response = call_tool_for(
            Arc::clone(&state),
            "session:export-link",
            1,
            "file_export",
            json!({ "target": "local", "path": source }),
        );
        let result = &response["result"];
        assert_eq!(result["structuredContent"]["delivery"], "link");
        assert!(result["structuredContent"].get("file").is_none());
        assert!(result["structuredContent"]["download"]["url"]
            .as_str()
            .unwrap()
            .starts_with("https://files.example.test/downloads/"));
        assert!(result["content"].as_array().unwrap().is_empty());
    }

    #[test]
    fn attachment_file_export_keeps_embedded_resource_compatibility() {
        let state = test_state();
        let source = state
            .config
            .server
            .runtime_dir
            .parent()
            .unwrap()
            .join("export.txt");
        std::fs::write(&source, b"attachment export").unwrap();

        let response = call_tool_for(
            Arc::clone(&state),
            "session:export-attachment",
            1,
            "file_export",
            json!({ "target": "local", "path": source, "delivery": "attachment" }),
        );
        let result = &response["result"];
        assert_eq!(result["structuredContent"]["delivery"], "attachment");
        assert!(result["structuredContent"]["download"].is_null());
        assert!(result["structuredContent"]["file"]
            .get("data_base64")
            .is_none());
        assert_eq!(result["content"][0]["type"], "resource");
        assert_eq!(
            result["content"][0]["resource"]["blob"],
            BASE64.encode(b"attachment export")
        );
    }

    #[test]
    fn completed_jobs_attach_to_next_non_result_tool_call_once() {
        let state = test_state();
        let started = call_tool_for(
            Arc::clone(&state),
            "session:a",
            1,
            "exec_start",
            json!({
                "target": "local",
                "command": "sleep 0.02; printf queued",
                "timeout_ms": 1000
            }),
        );
        let job_id = started["result"]["structuredContent"]["job_id"]
            .as_str()
            .unwrap()
            .to_string();
        while state
            .jobs
            .poll_for_caller(
                JobPollRequest {
                    job_id: job_id.clone(),
                },
                "session:a",
            )
            .unwrap()
            .status
            == "running"
        {
            std::thread::sleep(Duration::from_millis(5));
        }

        let other = call_tool_for(
            Arc::clone(&state),
            "session:b",
            2,
            "target_current",
            json!({}),
        );
        assert!(other["result"]["structuredContent"]
            .get("completed_jobs")
            .is_none());

        let delivered = call_tool_for(
            Arc::clone(&state),
            "session:a",
            3,
            "target_current",
            json!({}),
        );
        let completed = delivered["result"]["structuredContent"]["completed_jobs"]
            .as_array()
            .unwrap();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0]["job_id"], job_id);
        assert_eq!(completed[0]["stdout"], "queued");

        let drained = call_tool_for(
            Arc::clone(&state),
            "session:a",
            4,
            "target_current",
            json!({}),
        );
        assert!(drained["result"]["structuredContent"]
            .get("completed_jobs")
            .is_none());
    }

    #[test]
    fn job_wait_claims_result_before_completion_inbox() {
        let state = test_state();
        let started = call_tool_for(
            Arc::clone(&state),
            "session:a",
            1,
            "exec_start",
            json!({
                "target": "local",
                "command": "sleep 0.02; printf waited",
                "timeout_ms": 1000
            }),
        );
        let job_id = started["result"]["structuredContent"]["job_id"]
            .as_str()
            .unwrap()
            .to_string();
        let waited = call_tool_for(
            Arc::clone(&state),
            "session:a",
            2,
            "job_wait",
            json!({ "job_id": job_id, "wait_timeout_ms": 1000 }),
        );
        assert_eq!(waited["result"]["structuredContent"]["stdout"], "waited");

        let next = call_tool_for(state, "session:a", 3, "target_current", json!({}));
        assert!(next["result"]["structuredContent"]
            .get("completed_jobs")
            .is_none());
    }
}
