use crate::{
    core::{
        error::{Error, Result},
        state::AppState,
    },
    protocol::apps,
    tooling::tools,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    io::{self, BufRead, Write},
    sync::Arc,
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
    let value = match serde_json::from_slice::<Value>(bytes) {
        Ok(value) => value,
        Err(err) => {
            let response = parse_error(format!("parse error: {err}"));
            return Ok(Some(serde_json::to_vec(&response)?));
        }
    };

    handle_json_value_for_caller(state, value, caller_key)?
        .map(|response| serde_json::to_vec(&response))
        .transpose()
        .map_err(Error::Json)
}

fn handle_json_value_for_caller(
    state: Arc<AppState>,
    value: Value,
    caller_key: &str,
) -> Result<Option<Value>> {
    if let Value::Array(requests) = value {
        if requests.is_empty() {
            return serde_json::to_value(parse_error("parse error: empty batch".to_string()))
                .map(Some)
                .map_err(Error::Json);
        }

        let mut responses = Vec::new();
        for request in requests {
            if let Some(response) = handle_request_value(Arc::clone(&state), request, caller_key)? {
                responses.push(response);
            }
        }

        if responses.is_empty() {
            Ok(None)
        } else {
            Ok(Some(Value::Array(responses)))
        }
    } else {
        handle_request_value(state, value, caller_key)
    }
}

fn handle_request_value(
    state: Arc<AppState>,
    value: Value,
    caller_key: &str,
) -> Result<Option<Value>> {
    state.results.touch_session(caller_key)?;
    let response = match serde_json::from_value::<RpcRequest>(value) {
        Ok(request) => handle_request(state, request, caller_key),
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
    caller_key: &str,
) -> Option<RpcResponse> {
    if request.method.starts_with("notifications/") {
        return None;
    }

    let id = request.id.clone();
    let result = match request.method.as_str() {
        "initialize" => initialize(&state, request.params.unwrap_or_else(|| json!({}))),
        "tools/list" => Ok(json!({ "tools": tools::list_tools(
            state
                .config
                .server
                .oauth_enabled
                .then_some(state.config.server.oauth_scopes.as_slice())
        ) })),
        "tools/call" => tools_call(
            state,
            id.as_ref(),
            request.params.unwrap_or_else(|| json!({})),
            caller_key,
        ),
        "resources/list" => Ok(apps::list_resources(
            state.config.server.public_base_url.as_deref(),
        )),
        "resources/read" => resources_read(&state, request.params.unwrap_or_else(|| json!({}))),
        "ping" => Ok(json!({})),
        other => Err(Error::Tool(format!("unsupported method: {other}"))),
    };

    match result {
        Ok(value) => Some(RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(value),
            error: None,
        }),
        Err(err) => Some(RpcResponse {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(RpcError {
                code: err.json_rpc_code(),
                message: err.to_string(),
            }),
        }),
    }
}

fn resources_read(state: &AppState, params: Value) -> Result<Value> {
    #[derive(Deserialize)]
    struct ResourceReadParams {
        uri: String,
    }

    let params: ResourceReadParams = serde_json::from_value(params)?;
    apps::read_resource(
        &params.uri,
        state.config.server.public_base_url.as_deref(),
        &state.config.runtime,
    )
    .ok_or_else(|| Error::Tool(format!("unknown resource: {}", params.uri)))
}

fn initialize(state: &AppState, params: Value) -> Result<Value> {
    let requested_protocol = params
        .get("protocolVersion")
        .and_then(|v| v.as_str())
        .unwrap_or("2025-06-18");

    let mut result = json!({
        "protocolVersion": requested_protocol,
        "capabilities": {
            "tools": { "listChanged": false },
            "resources": { "subscribe": false, "listChanged": false }
        },
        "serverInfo": {
            "name": state.config.server.name.clone(),
            "version": state.config.server.version.clone(),
        }
    });
    if let Some(prompt) = state
        .config
        .server
        .startup_prompt
        .as_deref()
        .filter(|prompt| !prompt.trim().is_empty())
    {
        result["instructions"] = Value::String(prompt.to_string());
    }
    Ok(result)
}

fn tools_call(
    state: Arc<AppState>,
    request_id: Option<&Value>,
    params: Value,
    caller_key: &str,
) -> Result<Value> {
    #[derive(Deserialize)]
    struct ToolCallParams {
        name: String,
        #[serde(default)]
        arguments: Option<Value>,
    }

    let params: ToolCallParams = serde_json::from_value(params)?;
    let attach_completed = !matches!(
        params.name.as_str(),
        "job_poll" | "job_output" | "job_wait" | "result_read" | "exec_stream"
    );
    match tools::call_tool_with_context(
        Arc::clone(&state),
        &params.name,
        params.arguments.unwrap_or_else(|| json!({})),
        request_id,
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
            if caches_app_result(&params.name) {
                let result_id = state.results.store(caller_key, &value)?;
                if let Some(object) = value.as_object_mut() {
                    object.insert("result_id".to_string(), Value::String(result_id));
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

fn caches_app_result(name: &str) -> bool {
    matches!(
        name,
        "target_list"
            | "mcp_server_list"
            | "exec"
            | "exec_batch"
            | "exec_start"
            | "file_read"
            | "file_edit"
            | "file_write"
            | "file_delete"
            | "file_import"
            | "file_patch"
            | "file_move"
    )
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
    fn initialize_omits_unconfigured_startup_prompt() {
        let state = test_state();
        let result = initialize(&state, json!({})).unwrap();
        assert!(result.get("instructions").is_none());
    }

    #[test]
    fn initialize_exposes_configured_startup_prompt() {
        let temp = tempdir().unwrap();
        let path = temp.keep();
        let mut config = Config::default();
        config.server.runtime_dir = path.join("runtime");
        config.server.oauth_state_file = None;
        config.server.startup_prompt =
            Some("Treat this repository like AGENTS.md instructions.".to_string());
        let state = AppState::new(config).unwrap();

        let result = initialize(&state, json!({})).unwrap();
        assert_eq!(
            result["instructions"],
            "Treat this repository like AGENTS.md instructions."
        );
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
