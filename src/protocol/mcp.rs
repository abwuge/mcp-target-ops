use crate::{
    core::{
        activity::{self, Caller, Event},
        audit::CallDetails,
        error::{Error, Result},
        state::AppState,
    },
    tooling::{mcp_client, tools},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    io::{self, BufRead, Write},
    sync::Arc,
    time::{Instant, UNIX_EPOCH},
};

pub(crate) const MODERN_PROTOCOL: &str = "2026-07-28";
const LEGACY_PROTOCOL: &str = "2025-06-18";
const SUPPORTED_PROTOCOLS: [&str; 2] = [MODERN_PROTOCOL, LEGACY_PROTOCOL];

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

#[derive(Debug, Serialize, Clone)]
struct RpcError {
    code: i64,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
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
            data: None,
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
    let info = if request.method == "initialize" {
        request.params.as_ref().and_then(|p| p.get("clientInfo"))
    } else {
        request
            .params
            .as_ref()
            .and_then(|p| p.get("_meta"))
            .and_then(|meta| meta.get("io.modelcontextprotocol/clientInfo"))
    };
    observed_caller.client_name = info
        .and_then(|v| v.get("name"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or(observed_caller.client_name);
    observed_caller.client_version = info
        .and_then(|v| v.get("version"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or(observed_caller.client_version);
    let tool_params = (request.method == "tools/call")
        .then_some(request.params.as_ref())
        .flatten();
    let name = tool_params
        .and_then(|p| p.get("name"))
        .and_then(Value::as_str)
        .unwrap_or(&request.method)
        .to_string();
    let arguments = tool_params.and_then(|p| p.get("arguments"));
    let request_details = tool_params.map(|_| CallDetails::from_arguments(&name, arguments));
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
    let mut audit_result = None;
    let protocol = check_protocol(&request);
    let result = match protocol.as_ref() {
        Err(error) => Err(error.clone()),
        Ok(modern) => {
            let response = match request.method.as_str() {
                "initialize" => initialize(&state, request.params.unwrap_or_else(|| json!({}))),
                "server/discover" if *modern => Ok(discover(&state)),
                "tools/list" => {
                    let mut result = json!({ "tools": tools::list_tools(
                        state.config.server.oauth_enabled
                            .then_some(state.config.server.oauth_scopes.as_slice()),
                        &mcp_client::enabled_servers(&state.config),
                    ) });
                    if *modern {
                        result["resultType"] = json!("complete");
                        result["ttlMs"] = json!(0);
                        result["cacheScope"] = json!("private");
                    }
                    Ok(result)
                }
                "tools/call" => tools_call(
                    Arc::clone(&state),
                    request.params.unwrap_or_else(|| json!({})),
                    &caller.job_owner_key,
                )
                .map(|output| {
                    audit_result = Some(output.audit);
                    let mut result = output.response;
                    if *modern {
                        result["resultType"] = json!("complete");
                    }
                    result
                }),
                "ping" if !*modern => Ok(json!({})),
                other => Err(Error::Tool(format!("unsupported method: {other}"))),
            };
            response.map_err(|error| RpcError {
                code: if matches!(&error, Error::Tool(message) if message.starts_with("unsupported method:")) {
                    -32601
                } else {
                    error.json_rpc_code()
                },
                message: error.to_string(),
                data: None,
            })
        }
    };
    let response = match result {
        Ok(value) => RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(value),
            error: None,
        },
        Err(error) => RpcResponse {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(error),
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
        let response_target = audit_result
            .as_ref()
            .or(response.result.as_ref())
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
            details: request_details.map(|details| {
                details.with_response(
                    audit_result.as_ref().or(response.result.as_ref()),
                    response.error.as_ref().map(|err| err.message.as_str()),
                )
            }),
        };
        if let Err(err) = store.record(event) {
            eprintln!("mcp-target-ops activity log failed: {err}");
        }
    }
    Some(response)
}

/// Modern MCP requests are self-describing and do not establish protocol sessions.
/// Legacy initialize remains available for the clients still using it.
fn check_protocol(request: &RpcRequest) -> std::result::Result<bool, RpcError> {
    if request.method == "initialize" {
        let requested = request
            .params
            .as_ref()
            .and_then(|p| p.get("protocolVersion"))
            .and_then(Value::as_str)
            .unwrap_or(LEGACY_PROTOCOL);
        if requested != LEGACY_PROTOCOL {
            return Err(RpcError {
                code: -32022,
                message: "Unsupported initialize protocol version".to_string(),
                data: Some(json!({"supported": SUPPORTED_PROTOCOLS, "requested": requested})),
            });
        }
        return Ok(false);
    }
    let meta = request.params.as_ref().and_then(|p| p.get("_meta"));
    let modern = request.method == "server/discover"
        || meta.and_then(Value::as_object).is_some_and(|fields| {
            fields
                .keys()
                .any(|name| name.starts_with("io.modelcontextprotocol/"))
        });
    if !modern {
        return Ok(false);
    }
    let version = meta
        .and_then(|meta| meta.get("io.modelcontextprotocol/protocolVersion"))
        .and_then(Value::as_str);
    let Some(version) = version else {
        return Err(rpc_error(-32602, "missing params._meta protocolVersion"));
    };
    if version != MODERN_PROTOCOL {
        return Err(RpcError {
            code: -32022,
            message: "Unsupported protocol version".to_string(),
            data: Some(json!({ "supported": SUPPORTED_PROTOCOLS, "requested": version })),
        });
    }
    if request._jsonrpc.as_deref() != Some("2.0") || request.id.as_ref().is_none_or(Value::is_null)
    {
        return Err(rpc_error(-32600, "invalid JSON-RPC request"));
    }
    if !meta
        .and_then(|meta| meta.get("io.modelcontextprotocol/clientCapabilities"))
        .is_some_and(Value::is_object)
    {
        return Err(rpc_error(
            -32602,
            "missing or invalid params._meta clientCapabilities",
        ));
    }
    Ok(true)
}

fn rpc_error(code: i64, message: &str) -> RpcError {
    RpcError {
        code,
        message: message.to_string(),
        data: None,
    }
}

fn discover(state: &AppState) -> Value {
    json!({
        "resultType": "complete",
        "supportedVersions": SUPPORTED_PROTOCOLS,
        "capabilities": {
            "tools": { "listChanged": false },
        },
        "_meta": {
            "io.modelcontextprotocol/serverInfo": {
                "name": state.config.server.name.clone(),
                "version": state.config.server.version.clone(),
            }
        },
        "instructions": mcp_client::discovery_instructions(&mcp_client::enabled_servers(&state.config)),
        "ttlMs": 0,
        "cacheScope": "private",
    })
}

fn initialize(state: &AppState, params: Value) -> Result<Value> {
    let requested_protocol = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(LEGACY_PROTOCOL);
    if requested_protocol != LEGACY_PROTOCOL {
        return Err(Error::Tool(format!(
            "unsupported initialize protocol version {requested_protocol}; supported: {LEGACY_PROTOCOL}"
        )));
    }
    Ok(json!({
        "protocolVersion": LEGACY_PROTOCOL,
        "capabilities": {
            "tools": { "listChanged": false }
        },
        "serverInfo": {
            "name": state.config.server.name.clone(),
            "version": state.config.server.version.clone(),
        },
        "instructions": mcp_client::discovery_instructions(&mcp_client::enabled_servers(&state.config)),
    }))
}

struct ToolCallOutput {
    response: Value,
    audit: Value,
}

fn tools_call(state: Arc<AppState>, params: Value, caller_key: &str) -> Result<ToolCallOutput> {
    #[derive(Deserialize)]
    struct ToolCallParams {
        name: String,
        #[serde(default)]
        arguments: Option<Value>,
    }

    let params: ToolCallParams = serde_json::from_value(params)?;
    let arguments = params.arguments.unwrap_or_else(|| json!({}));
    let options = tools::ResultOptions::from_arguments(&arguments);
    let attach_completed = !matches!(params.name.as_str(), "job");
    match tools::call_tool_with_context(Arc::clone(&state), &params.name, arguments, caller_key) {
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
            let mut content = Vec::new();
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

            let mut compact = tools::compact_result(&params.name, &value, &options);
            if let Some(completed) = value.get("completed_jobs") {
                if let Some(object) = compact.as_object_mut() {
                    object.insert("completed_jobs".to_string(), completed.clone());
                }
            }
            Ok(ToolCallOutput {
                response: json!({
                    "content": content,
                    "structuredContent": compact,
                    "isError": false,
                }),
                audit: json!({"structuredContent": value}),
            })
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
            Ok(ToolCallOutput {
                audit: response.clone(),
                response,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::{Config, TargetConfig};
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
            local.policy.allow_file_write = true;
            local.policy.allowed_roots = vec![path.to_string_lossy().to_string()];
        }
        config.server.default_target = Some("local".to_string());
        config.server.public_base_url = Some("https://files.example.test".to_string());
        config.server.oauth_state_file = None;
        config.server.runtime_dir = path.join("runtime");
        config.activity.database_path = path.join("activity/calls.sqlite3");
        Arc::new(AppState::new(config).unwrap())
    }

    fn modern_request(method: &str, id: u64, params: Value) -> Value {
        let mut params = params;
        params["_meta"] = json!({
            "io.modelcontextprotocol/protocolVersion": MODERN_PROTOCOL,
            "io.modelcontextprotocol/clientCapabilities": {},
            "io.modelcontextprotocol/clientInfo": {"name":"test-modern-client", "version":"1"},
        });
        json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params})
    }

    #[test]
    fn modern_protocol_supports_stateless_discovery_catalog_and_calls() {
        let state = test_state();
        let send = |message| {
            handle_json_value_for_caller(Arc::clone(&state), message, "bearer:one")
                .unwrap()
                .unwrap()
        };
        // Every operation can be the first request: no initialize or session is needed.
        let discovery = send(modern_request("server/discover", 1, json!({})));
        assert_eq!(discovery["result"]["resultType"], "complete");
        assert_eq!(discovery["result"]["supportedVersions"][0], MODERN_PROTOCOL);
        assert_eq!(discovery["result"]["cacheScope"], "private");
        assert_eq!(
            discovery["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
            state.config.server.name
        );
        assert!(discovery["result"]["instructions"].is_string());

        let listed = send(modern_request("tools/list", 2, json!({})));
        assert_eq!(listed["result"]["resultType"], "complete");
        assert!(listed["result"]["tools"].as_array().unwrap().len() >= 20);
        assert_eq!(listed["result"]["ttlMs"], 0);
        assert_eq!(listed["result"]["cacheScope"], "private");

        let call = send(modern_request(
            "tools/call",
            3,
            json!({
                "name":"target_list", "arguments":{}
            }),
        ));
        assert_eq!(call["result"]["resultType"], "complete");
        assert_eq!(call["result"]["isError"], false);
        assert!(call["result"]["structuredContent"]["targets"].is_array());

        let unknown = send(modern_request("does/not-exist", 4, json!({})));
        assert_eq!(unknown["error"]["code"], -32601);

        // Existing legacy clients remain usable while they migrate.
        let legacy = send(json!({"jsonrpc":"2.0","id":5,"method":"initialize",
            "params":{"protocolVersion":"2025-06-18"}}));
        assert_eq!(legacy["result"]["protocolVersion"], LEGACY_PROTOCOL);
        assert!(legacy["result"].get("resultType").is_none());
    }

    #[test]
    fn modern_protocol_rejects_unsupported_versions_and_missing_capabilities() {
        let state = test_state();
        let send = |message| {
            handle_json_value_for_caller(Arc::clone(&state), message, "bearer:two")
                .unwrap()
                .unwrap()
        };
        let mut unsupported = modern_request(
            "tools/call",
            1,
            json!({
                "name":"target_list","arguments":{}
            }),
        );
        unsupported["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"] =
            json!("2099-01-01");
        let rejected = send(unsupported);
        assert_eq!(rejected["error"]["code"], -32022);
        assert_eq!(rejected["error"]["data"]["requested"], "2099-01-01");
        assert!(rejected["error"]["data"]["supported"]
            .as_array()
            .unwrap()
            .contains(&json!(MODERN_PROTOCOL)));

        let mut missing = modern_request(
            "tools/call",
            2,
            json!({
                "name":"target_list","arguments":{}
            }),
        );
        missing["params"]["_meta"]
            .as_object_mut()
            .unwrap()
            .remove("io.modelcontextprotocol/clientCapabilities");
        assert_eq!(send(missing)["error"]["code"], -32602);

        let invalid_initialize = send(json!({"jsonrpc":"2.0","id":3,"method":"initialize",
            "params":{"protocolVersion":"2026-07-28"}}));
        assert_eq!(invalid_initialize["error"]["code"], -32022);
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

    fn call_tool_for_owner(
        state: Arc<AppState>,
        session_key: &str,
        owner_key: &str,
        id: u64,
        name: &str,
        arguments: Value,
    ) -> Value {
        let mut caller = caller_for_key(&state, session_key);
        caller.job_owner_key = owner_key.to_string();
        handle_json_value_for_context(
            state,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": { "name": name, "arguments": arguments }
            }),
            &caller,
        )
        .unwrap()
        .unwrap()
    }

    #[test]
    fn jobs_can_be_read_from_another_session_with_same_grant() {
        let state = test_state();
        let owner = "oauth-grant:first";
        let other_owner = "oauth-grant:second";
        let started = call_tool_for_owner(
            Arc::clone(&state),
            "session:one",
            owner,
            1,
            "exec",
            json!({
                "target":"local",
                "command":"sleep 0.05; printf cross-session",
                "yield_time_ms":0
            }),
        );
        let id = started["result"]["structuredContent"]["job_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(
            started["result"]["structuredContent"]["auto_backgrounded"],
            true
        );

        let denied = call_tool_for_owner(
            Arc::clone(&state),
            "session:other",
            other_owner,
            2,
            "job",
            json!({"job_id":id,"action":"status"}),
        );
        assert_eq!(denied["result"]["isError"], true);
        assert!(denied["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("not found"));
        let hidden = call_tool_for_owner(
            Arc::clone(&state),
            "session:other",
            other_owner,
            3,
            "server_info",
            json!({}),
        );
        assert_eq!(hidden["result"]["structuredContent"]["job_ids"], json!([]));

        let visible = call_tool_for_owner(
            Arc::clone(&state),
            "session:two",
            owner,
            4,
            "server_info",
            json!({}),
        );
        assert!(visible["result"]["structuredContent"]["job_ids"]
            .as_array()
            .unwrap()
            .contains(&json!(id)));

        let waited = call_tool_for_owner(
            Arc::clone(&state),
            "session:three",
            owner,
            5,
            "job",
            json!({"job_id":id,"action":"wait","wait_ms":5000}),
        );
        assert_eq!(waited["result"]["isError"], false);
        assert_eq!(
            waited["result"]["structuredContent"]["stdout"],
            "cross-session"
        );

        let no_duplicate =
            call_tool_for_owner(state, "session:four", owner, 6, "target_current", json!({}));
        assert!(no_duplicate["result"]["structuredContent"]
            .get("completed_jobs")
            .is_none());
    }

    #[test]
    fn completed_job_inbox_follows_authorization_grant_across_sessions() {
        let state = test_state();
        let owner = "oauth-grant:inbox";
        let started = call_tool_for_owner(
            Arc::clone(&state),
            "session:start",
            owner,
            1,
            "exec",
            json!({
                "target": "local",
                "command": "sleep 0.02; printf delivered-across-sessions",
                "yield_time_ms": 0
            }),
        );
        let id = started["result"]["structuredContent"]["job_id"]
            .as_str()
            .unwrap()
            .to_string();
        let start = std::time::Instant::now();
        while !state
            .jobs
            .output_for_caller(
                crate::tooling::job::JobOutputRequest {
                    job_id: id.clone(),
                    stdout_since_seq: None,
                    stderr_since_seq: None,
                    max_bytes: None,
                },
                owner,
            )
            .unwrap()
            .eof
        {
            assert!(start.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(10));
        }

        let unrelated = call_tool_for_owner(
            Arc::clone(&state),
            "session:unrelated",
            "oauth-grant:other",
            2,
            "target_current",
            json!({}),
        );
        assert!(unrelated["result"]["structuredContent"]
            .get("completed_jobs")
            .is_none());

        let delivered = call_tool_for_owner(
            Arc::clone(&state),
            "session:new",
            owner,
            3,
            "target_current",
            json!({}),
        );
        let completed = delivered["result"]["structuredContent"]["completed_jobs"]
            .as_array()
            .unwrap();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0]["job_id"], id);
        assert_eq!(completed[0]["stdout"], "delivered-across-sessions");

        let once = call_tool_for_owner(
            state,
            "session:another",
            owner,
            4,
            "target_current",
            json!({}),
        );
        assert!(once["result"]["structuredContent"]
            .get("completed_jobs")
            .is_none());
    }

    #[test]
    fn batch_jobs_follow_authorization_grant_across_sessions() {
        let state = test_state();
        let owner = "oauth-grant:batch";
        let started = call_tool_for_owner(
            Arc::clone(&state),
            "session:batch-start",
            owner,
            1,
            "exec",
            json!({
                "target":"local",
                "commands":["sleep 0.02; printf batch-ok"],
                "yield_time_ms":0
            }),
        );
        let id = started["result"]["structuredContent"]["job_id"]
            .as_str()
            .unwrap()
            .to_string();

        let hidden = call_tool_for_owner(
            Arc::clone(&state),
            "session:batch-foreign",
            "oauth-grant:foreign",
            2,
            "server_info",
            json!({}),
        );
        assert_eq!(hidden["result"]["structuredContent"]["job_ids"], json!([]));
        let denied = call_tool_for_owner(
            Arc::clone(&state),
            "session:batch-foreign",
            "oauth-grant:foreign",
            3,
            "job",
            json!({"job_id":id, "action":"read"}),
        );
        assert_eq!(denied["result"]["isError"], true);

        let completed = call_tool_for_owner(
            state,
            "session:batch-reconnect",
            owner,
            4,
            "job",
            json!({"job_id":id,"action":"wait","wait_ms":5000}),
        );
        assert_eq!(completed["result"]["isError"], false);
        assert_eq!(
            completed["result"]["structuredContent"]["results"][0]["stdout"],
            "batch-ok"
        );
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
    fn real_exec_command_output_and_search_are_persisted() {
        let state = test_state();
        let command = "printf 'audit-command-visible-2026'";
        let response = call_tool_for(
            Arc::clone(&state),
            "session:command-audit",
            101,
            "exec",
            json!({"target":"local","command":command}),
        );
        assert_eq!(response["result"]["isError"], false);
        assert!(response["result"]["structuredContent"]
            .get("command")
            .is_none());
        assert_eq!(
            response["result"]["structuredContent"]["stdout"],
            "audit-command-visible-2026"
        );
        let history = state
            .activity
            .as_ref()
            .unwrap()
            .query(crate::core::activity::Query {
                search: Some("audit-command-visible-2026".to_string()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(history["summary"]["count"], 1);
        assert_eq!(history["calls"][0]["command_preview"], command);
        assert_eq!(history["calls"][0]["has_details"], true);
        let id = history["calls"][0]["id"].as_i64().unwrap();
        let details = state
            .activity
            .as_ref()
            .unwrap()
            .call_details(id)
            .unwrap()
            .unwrap();
        assert_eq!(details["available"], true);
        assert_eq!(details["command"], command);
        assert_eq!(details["arguments"]["command"], command);
        assert_eq!(details["result"]["stdout"], "audit-command-visible-2026");
        assert_eq!(details["result"]["exit_code"], 0);
    }

    #[test]
    fn file_write_and_patch_do_not_repeat_submitted_payloads() {
        let state = test_state();
        let root = state.config.server.runtime_dir.parent().unwrap();
        let file = root.join("clean-input.txt");
        let path = file.to_string_lossy().to_string();
        let written = call_tool_for(
            Arc::clone(&state),
            "session:file-clean",
            1,
            "write",
            json!({"target":"local","path":path,"content":"before\n"}),
        );
        let output = &written["result"]["structuredContent"];
        assert_eq!(written["result"]["isError"], false);
        assert!(output.get("content").is_none());
        assert_eq!(output["written"], true);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "before\n");

        let patch = "--- a/clean-input.txt\n+++ b/clean-input.txt\n@@ -1 +1 @@\n-before\n+after\n";
        let patched = call_tool_for(
            Arc::clone(&state),
            "session:file-clean",
            2,
            "write",
            json!({"target":"local","path":path,"patch":patch}),
        );
        assert_eq!(patched["result"]["isError"], false, "{patched}");
        let result = &patched["result"]["structuredContent"];
        assert_eq!(result["changed"], true);
        assert!(result.get("diff").is_none());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "after\n");

        let read = call_tool_for(
            Arc::clone(&state),
            "session:file-clean",
            3,
            "read",
            json!({"target":"local","path":path}),
        );
        assert_eq!(read["result"]["structuredContent"]["content"], "after\n");
        let audit = state
            .activity
            .as_ref()
            .unwrap()
            .query(crate::core::activity::Query {
                search: Some("+after".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(audit["summary"]["count"], 1);
    }

    #[test]
    fn file_results_are_small_and_audit_retains_full_details() {
        let state = test_state();
        let root = state.config.server.runtime_dir.parent().unwrap();
        let file = root.join("compact-edit.txt");
        let path = file.to_string_lossy().to_string();
        std::fs::write(&file, "before\n").unwrap();

        let read = call_tool_for(
            Arc::clone(&state),
            "session:compact",
            1,
            "read",
            json!({"target":"local","path":path}),
        );
        let read_output = &read["result"]["structuredContent"];
        let current_revision = read_output["revision"].as_str().unwrap();
        assert_eq!(current_revision.len(), 7);
        assert!(read_output.get("sha256").is_none());
        assert_eq!(read_output["content"], "before\n");
        assert!(read_output.get("path").is_none());
        assert!(read_output.get("bytes").is_none());

        let edit = call_tool_for(
            Arc::clone(&state),
            "session:compact",
            2,
            "file_edit",
            json!({"target":"local","path":path,"expected_revision":current_revision,
                "edits":[{"old":"before","new":"after"}]}),
        );
        let returned = &edit["result"]["structuredContent"];
        assert_eq!(returned["changed"], true);
        assert_eq!(returned["written"], true);
        assert_eq!(returned["revision"].as_str().unwrap().len(), 7);
        assert!(returned.get("diff").is_none());
        assert!(returned.get("new_sha256").is_none());
        let next_revision = returned["revision"].as_str().unwrap().to_string();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "after\n");

        let query = state
            .activity
            .as_ref()
            .unwrap()
            .query(crate::core::activity::Query {
                tool: Some("file_edit".into()),
                ..Default::default()
            })
            .unwrap();
        let id = query["calls"][0]["id"].as_i64().unwrap();
        let details = state
            .activity
            .as_ref()
            .unwrap()
            .call_details(id)
            .unwrap()
            .unwrap();
        assert_eq!(details["arguments"]["edits"][0]["new"], "after");
        assert!(details["result"]["diff"]
            .as_str()
            .unwrap()
            .contains("-before"));
        assert!(details["result"]["diff"]
            .as_str()
            .unwrap()
            .contains("+after"));
        assert_eq!(details["result"]["new_sha256"].as_str().unwrap().len(), 64);
        assert!(
            serde_json::to_vec(returned).unwrap().len()
                < serde_json::to_vec(&details["result"]).unwrap().len() / 3
        );

        let expanded = call_tool_for(
            Arc::clone(&state),
            "session:compact",
            3,
            "file_edit",
            json!({"target":"local","path":path,"expected_revision":next_revision,"include_diff":true,
                "include_sha256":true,"edits":[{"old":"after","new":"third"}]}),
        );
        let expanded = &expanded["result"]["structuredContent"];
        assert!(expanded["diff"].as_str().unwrap().contains("+third"));
        assert_eq!(expanded["old_sha256"].as_str().unwrap().len(), 64);
        assert_eq!(expanded["new_sha256"].as_str().unwrap().len(), 64);
        assert_eq!(
            expanded["revision"].as_str().unwrap(),
            &expanded["new_sha256"].as_str().unwrap()[..7]
        );

        let preview = call_tool_for(
            Arc::clone(&state),
            "session:compact",
            4,
            "file_edit",
            json!({"target":"local","path":path,"dry_run":true,
                "edits":[{"old":"third","new":"fourth"}]}),
        );
        assert_eq!(preview["result"]["structuredContent"]["written"], false);
        assert!(preview["result"]["structuredContent"]["diff"]
            .as_str()
            .unwrap()
            .contains("+fourth"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "third\n");
        assert!(preview["result"]["structuredContent"]
            .get("revision")
            .is_none());

        let overwritten = call_tool_for(
            Arc::clone(&state),
            "session:compact",
            5,
            "write",
            json!({"target":"local","path":path,"content":"replacement\n",
                "expected_revision":expanded["revision"]}),
        );
        assert_eq!(overwritten["result"]["structuredContent"]["created"], false);
        assert_eq!(overwritten["result"]["structuredContent"]["written"], true);
        assert_eq!(
            overwritten["result"]["structuredContent"]["revision"]
                .as_str()
                .unwrap()
                .len(),
            7
        );
        let other = root.join("compact-created.txt");
        let created = call_tool_for(
            Arc::clone(&state),
            "session:compact",
            6,
            "write",
            json!({"target":"local","path":other,"content":"private text\n"}),
        );
        assert_eq!(created["result"]["structuredContent"]["created"], true);
        assert_eq!(created["result"]["structuredContent"]["written"], true);
        assert_eq!(
            created["result"]["structuredContent"]["revision"]
                .as_str()
                .unwrap()
                .len(),
            7
        );

        let deleted = call_tool_for(
            Arc::clone(&state),
            "session:compact",
            7,
            "file_delete",
            json!({"target":"local","path":other}),
        );
        let public = &deleted["result"]["structuredContent"];
        assert_eq!(public, &json!({"deleted":true,"written":true}));
        let records = state
            .activity
            .as_ref()
            .unwrap()
            .query(crate::core::activity::Query {
                tool: Some("file_delete".into()),
                ..Default::default()
            })
            .unwrap();
        let id = records["calls"][0]["id"].as_i64().unwrap();
        let details = state
            .activity
            .as_ref()
            .unwrap()
            .call_details(id)
            .unwrap()
            .unwrap();
        assert_eq!(details["result"]["content"], "private text\n");
        assert_eq!(details["result"]["old_sha256"].as_str().unwrap().len(), 64);
        assert!(std::fs::metadata(&other).is_err());

        std::fs::write(&other, "visible on request\n").unwrap();
        let requested = call_tool_for(
            Arc::clone(&state),
            "session:compact",
            8,
            "file_delete",
            json!({"target":"local","path":other,"include_content":true}),
        );
        assert_eq!(
            requested["result"]["structuredContent"]["content"],
            "visible on request\n"
        );

        let backup = call_tool_for(
            Arc::clone(&state),
            "session:compact",
            9,
            "file_backup",
            json!({"target":"local","path":path}),
        );
        let public = &backup["result"]["structuredContent"];
        let backup_id = public["backup"]["backup_id"].as_str().unwrap();
        assert!(public["backup"].get("sha256").is_none());
        assert!(public["backup"]["expires_at_unix_secs"].as_u64().unwrap() > 0);
        let listing = call_tool_for(
            Arc::clone(&state),
            "session:compact",
            10,
            "file_restore",
            json!({"target":"local","path":path}),
        );
        assert_eq!(
            listing["result"]["structuredContent"]["backups"][0]["backup_id"],
            backup_id
        );
        assert!(listing["result"]["structuredContent"]["backups"][0]
            .get("sha256")
            .is_none());
        let restored = call_tool_for(
            Arc::clone(&state),
            "session:compact",
            11,
            "file_restore",
            json!({"target":"local","backup_id":backup_id,"overwrite":true}),
        );
        assert_eq!(restored["result"]["structuredContent"]["restored"], true);
        assert_eq!(
            restored["result"]["structuredContent"]["revision"]
                .as_str()
                .unwrap()
                .len(),
            7
        );
        assert!(restored["result"]["structuredContent"]
            .get("restored_sha256")
            .is_none());

        let exported = call_tool_for(
            Arc::clone(&state),
            "session:compact",
            12,
            "file_export",
            json!({"target":"local","path":path}),
        );
        assert!(exported["result"]["structuredContent"]["download"]["url"]
            .as_str()
            .unwrap()
            .starts_with("https://"));
        assert!(exported["result"]["structuredContent"]["download"]
            .get("sha256")
            .is_none());

        let removed = call_tool_for(
            Arc::clone(&state),
            "session:compact",
            13,
            "file_read",
            json!({"target":"local","path":path}),
        );
        assert_eq!(removed["result"]["isError"], true);
        assert!(removed["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("unknown tool"));
    }

    #[test]
    fn short_revisions_guard_serial_edits_and_reject_stale_changes() {
        let state = test_state();
        let file = state
            .config
            .server
            .runtime_dir
            .parent()
            .unwrap()
            .join("revision.txt");
        let path = file.to_string_lossy().to_string();
        std::fs::write(&file, "v1\n").unwrap();
        let first_read = call_tool_for(
            Arc::clone(&state),
            "session:revision",
            1,
            "read",
            json!({"target":"local","path":path}),
        );
        let rev1 = first_read["result"]["structuredContent"]["revision"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(rev1.len(), 7);
        assert!(first_read["result"]["structuredContent"]
            .get("sha256")
            .is_none());
        let edit = call_tool_for(
            Arc::clone(&state),
            "session:revision",
            2,
            "file_edit",
            json!({"target":"local","path":path,"expected_revision":rev1,
                "edits":[{"old":"v1","new":"v2"}]}),
        );
        assert_eq!(edit["result"]["isError"], false);
        let rev2 = edit["result"]["structuredContent"]["revision"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_ne!(rev1, rev2);

        let stale = call_tool_for(
            Arc::clone(&state),
            "session:revision",
            3,
            "file_edit",
            json!({"target":"local","path":path,"expected_revision":rev1,
                "edits":[{"old":"v2","new":"incorrect"}]}),
        );
        assert_eq!(stale["result"]["isError"], true);
        assert!(stale["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("revision"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "v2\n");

        let overwrite = call_tool_for(
            Arc::clone(&state),
            "session:revision",
            4,
            "write",
            json!({"target":"local","path":path,"expected_revision":rev2,"content":"v3\n"}),
        );
        assert_eq!(overwrite["result"]["isError"], false);
        let rev3 = overwrite["result"]["structuredContent"]["revision"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_ne!(rev2, rev3);
        let stale_write = call_tool_for(
            Arc::clone(&state),
            "session:revision",
            5,
            "write",
            json!({"target":"local","path":path,"expected_revision":rev2,"content":"incorrect\n"}),
        );
        assert_eq!(stale_write["result"]["isError"], true);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "v3\n");

        let stale_delete = call_tool_for(
            Arc::clone(&state),
            "session:revision",
            6,
            "file_delete",
            json!({"target":"local","path":path,"expected_revision":rev2}),
        );
        assert_eq!(stale_delete["result"]["isError"], true);
        let read_full = call_tool_for(
            Arc::clone(&state),
            "session:revision",
            7,
            "read",
            json!({"target":"local","path":path,"include_sha256":true}),
        );
        assert_eq!(
            read_full["result"]["structuredContent"]["sha256"]
                .as_str()
                .unwrap()
                .len(),
            64
        );
        assert_eq!(read_full["result"]["structuredContent"]["revision"], rev3);
        assert_eq!(
            read_full["result"]["structuredContent"]["sha256"]
                .as_str()
                .unwrap()[..7],
            rev3
        );

        let deleted = call_tool_for(
            Arc::clone(&state),
            "session:revision",
            8,
            "file_delete",
            json!({"target":"local","path":path,"expected_revision":rev3}),
        );
        assert_eq!(
            deleted["result"]["structuredContent"],
            json!({"deleted":true,"written":true})
        );
        assert!(std::fs::metadata(&file).is_err());
    }

    #[test]
    fn unified_exec_handles_batches_background_wait_and_cancel() {
        let state = test_state();
        let owner = "session:unified";
        let launched = call_tool_for(
            Arc::clone(&state),
            owner,
            100,
            "exec",
            json!({
                "target":"local", "commands":[
                    "sleep 0.04; printf first",
                    {"command":"sleep 0.03; printf second"}
                ],
                "mode":"parallel", "yield_time_ms":0
            }),
        );
        assert_eq!(launched["result"]["isError"], false);
        let group_id = launched["result"]["structuredContent"]["job_id"]
            .as_str()
            .unwrap();
        assert!(group_id.starts_with("batch_"));
        let ready = call_tool_for(
            Arc::clone(&state),
            owner,
            101,
            "job",
            json!({"job_id":group_id, "action":"wait", "wait_ms":5000}),
        );
        let report = &ready["result"]["structuredContent"];
        assert_eq!(report["status"], "completed");
        assert_eq!(report["succeeded"], 2);
        assert_eq!(report["results"][0]["stdout"], "first");
        assert_eq!(report["results"][1]["stdout"], "second");
        assert_eq!(report["results"][0]["index"], 0);
        assert_eq!(report["results"][1]["index"], 1);

        let unrelated = call_tool_for(
            Arc::clone(&state),
            "session:other",
            102,
            "job",
            json!({"job_id":group_id, "action":"read"}),
        );
        assert_eq!(unrelated["result"]["isError"], true);

        let stopped = call_tool_for(
            Arc::clone(&state),
            owner,
            103,
            "exec",
            json!({"target":"local","commands":["false", "printf should-not-run"],
                "mode":"sequential", "stop_on_error":true, "yield_time_ms":0}),
        );
        let failed_group = stopped["result"]["structuredContent"]["job_id"]
            .as_str()
            .unwrap();
        let stopped_result = call_tool_for(
            Arc::clone(&state),
            owner,
            104,
            "job",
            json!({"job_id":failed_group, "action":"wait","wait_ms":5000}),
        );
        assert_eq!(
            stopped_result["result"]["structuredContent"]["status"],
            "failed"
        );
        assert_eq!(
            stopped_result["result"]["structuredContent"]["executed_count"],
            1
        );
        assert_eq!(
            stopped_result["result"]["structuredContent"]["stopped_early"],
            true
        );

        let long = call_tool_for(
            Arc::clone(&state),
            owner,
            105,
            "exec",
            json!({"target":"local", "commands":["sleep 3; printf no","sleep 3; printf no"],
                "mode":"parallel","yield_time_ms":0}),
        );
        let long_id = long["result"]["structuredContent"]["job_id"]
            .as_str()
            .unwrap();
        let cancel = call_tool_for(
            Arc::clone(&state),
            owner,
            106,
            "job",
            json!({"job_id":long_id,"action":"cancel"}),
        );
        assert_eq!(cancel["result"]["isError"], false);
        let cancellation = call_tool_for(
            Arc::clone(&state),
            owner,
            107,
            "job",
            json!({"job_id":long_id,"action":"wait","wait_ms":5000}),
        );
        assert_eq!(
            cancellation["result"]["structuredContent"]["status"],
            "cancelled"
        );

        let single = call_tool_for(
            Arc::clone(&state),
            owner,
            108,
            "exec",
            json!({"target":"local","command":"sleep 0.03; printf single","yield_time_ms":0}),
        );
        let single_id = single["result"]["structuredContent"]["job_id"]
            .as_str()
            .unwrap();
        let single_result = call_tool_for(
            Arc::clone(&state),
            owner,
            109,
            "job",
            json!({"job_id":single_id,"action":"wait","wait_ms":5000}),
        );
        assert_eq!(
            single_result["result"]["structuredContent"]["stdout"],
            "single"
        );

        let audit = state
            .activity
            .as_ref()
            .unwrap()
            .query(crate::core::activity::Query {
                search: Some("printf second".into()),
                ..Default::default()
            })
            .unwrap();
        assert!(audit["summary"]["count"].as_i64().unwrap() >= 1);
    }

    #[test]
    fn unified_read_search_write_patch_and_restore() {
        let state = test_state();
        let root = state.config.server.runtime_dir.parent().unwrap();
        let folder = root.join("workspace");
        std::fs::create_dir_all(&folder).unwrap();
        let file = folder.join("example.rs");
        let path = file.to_string_lossy().to_string();
        let owner = "session:file-unified";

        let write = call_tool_for(
            Arc::clone(&state),
            owner,
            201,
            "write",
            json!({
                "target":"local","path":path,"content":"alpha_12\nbeta\n"
            }),
        );
        assert_eq!(write["result"]["isError"], false);
        let read = call_tool_for(
            Arc::clone(&state),
            owner,
            202,
            "read",
            json!({"target":"local","path":path,"start_line":1,"end_line":1}),
        );
        assert_eq!(read["result"]["structuredContent"]["content"], "alpha_12\n");
        let searched = call_tool_for(
            Arc::clone(&state),
            owner,
            203,
            "read",
            json!({
                "target":"local","path":folder.to_string_lossy(),
                "pattern":"alpha_[0-9]+","glob":["*.rs"],"context_lines":1
            }),
        );
        assert_eq!(searched["result"]["isError"], false, "{searched}");
        assert_eq!(
            searched["result"]["structuredContent"]["match_count"], 1,
            "{searched}"
        );
        assert!(
            searched["result"]["structuredContent"]["results"][0]["text"]
                .as_str()
                .unwrap()
                .contains("alpha_12")
        );

        let snapshot = call_tool_for(
            Arc::clone(&state),
            owner,
            204,
            "file_backup",
            json!({"target":"local","path":path}),
        );
        assert_eq!(snapshot["result"]["isError"], false);
        let id = snapshot["result"]["structuredContent"]["backup"]["backup_id"]
            .as_str()
            .unwrap();
        let backup_listing = call_tool_for(
            Arc::clone(&state),
            owner,
            205,
            "file_restore",
            json!({"target":"local","path":path}),
        );
        assert_eq!(backup_listing["result"]["isError"], false);
        assert_eq!(
            backup_listing["result"]["structuredContent"]["backups"][0]["backup_id"],
            id
        );

        let patch =
            "--- a/example.rs\n+++ b/example.rs\n@@ -1,2 +1,2 @@\n-alpha_12\n+alpha_34\n beta\n";
        let patched = call_tool_for(
            Arc::clone(&state),
            owner,
            206,
            "write",
            json!({"target":"local","path":path,"patch":patch}),
        );
        assert_eq!(patched["result"]["isError"], false, "{patched}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "alpha_34\nbeta\n");

        let restored = call_tool_for(
            Arc::clone(&state),
            owner,
            207,
            "file_restore",
            json!({"target":"local","backup_id":id,"overwrite":true}),
        );
        assert_eq!(restored["result"]["isError"], false, "{restored}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "alpha_12\nbeta\n");
    }

    #[test]
    fn initialize_and_tool_schemas_advertise_only_enabled_downstream_servers() {
        let mut config = test_state().config.clone();
        config.activity.enabled = false;
        config.mcp_servers.insert(
            "nocturne_memory".into(),
            toml::from_str("enabled = true\ndescription = 'Personal memory search'\nurl = 'https://private.example.invalid/mcp'").unwrap(),
        );
        config.mcp_servers.insert(
            "disabled_search".into(),
            toml::from_str("enabled = false\ndescription = 'Disabled search'\nurl = 'https://disabled.example.invalid/mcp'").unwrap(),
        );
        let state = Arc::new(AppState::new(config).unwrap());
        let initialized = handle_json_value_for_caller(
            Arc::clone(&state),
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}),
            "session:discovery",
        ).unwrap().unwrap();
        let instructions = initialized["result"]["instructions"].as_str().unwrap();
        assert!(instructions.contains("nocturne_memory (Personal memory search)"));
        assert!(instructions.contains("mcp_tools_list(server)"));
        assert!(!instructions.contains("disabled_search"));
        assert!(!instructions.contains("private.example.invalid"));
        let listed = handle_json_value_for_caller(
            Arc::clone(&state),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            "session:discovery",
        )
        .unwrap()
        .unwrap();
        let tools = listed["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 24);
        for name in ["mcp_tools_list", "mcp_tool_call"] {
            let descriptor = tools.iter().find(|tool| tool["name"] == name).unwrap();
            assert_eq!(
                descriptor["inputSchema"]["properties"]["server"]["enum"],
                json!(["nocturne_memory"])
            );
            assert!(!descriptor.to_string().contains("disabled_search"));
            assert!(!descriptor.to_string().contains("private.example.invalid"));
        }
        let inventory = call_tool_for(state, "session:discovery", 3, "mcp_server_list", json!({}));
        let servers = inventory["result"]["structuredContent"]["servers"]
            .as_array()
            .unwrap();
        assert_eq!(servers.len(), 2);
        let memory = servers
            .iter()
            .find(|server| server["name"] == "nocturne_memory")
            .unwrap();
        assert_eq!(memory["description"], "Personal memory search");
        assert_eq!(memory["enabled"], true);
        assert!(inventory["result"]["structuredContent"]
            .to_string()
            .contains("disabled_search"));
        assert!(!inventory["result"]["structuredContent"]
            .to_string()
            .contains("private.example.invalid"));
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
        let mut config = Config::load_from_path(&config_path).unwrap();
        config.server.oauth_state_file = Some(root.join("oauth-state.json"));
        let state = Arc::new(AppState::new(config).unwrap());

        let initialized = initialize(&state, json!({})).unwrap();
        assert!(initialized["instructions"]
            .as_str()
            .unwrap()
            .contains("downstream MCP servers"));
        assert!(!initialized["instructions"]
            .as_str()
            .unwrap()
            .contains("RULE-SENTINEL"));
        for caller in ["session:a", "session:b"] {
            let result = call_tool_for(
                Arc::clone(&state),
                caller,
                1,
                "read",
                json!({ "target": "local", "path": config_path }),
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
        assert!(result["structuredContent"].get("file").is_none());
        assert!(result["structuredContent"]["download"]["url"]
            .as_str()
            .unwrap()
            .starts_with("https://files.example.test/downloads/"));
        assert!(result["content"].as_array().unwrap().is_empty());
    }

    #[test]
    fn completed_jobs_attach_to_next_non_result_tool_call_once() {
        let state = test_state();
        let started = call_tool_for(
            Arc::clone(&state),
            "session:a",
            1,
            "exec",
            json!({
                "target": "local",
                "command": "sleep 0.02; printf queued",
                "timeout_ms": 5000,
                "yield_time_ms": 0
            }),
        );
        let job_id = started["result"]["structuredContent"]["job_id"]
            .as_str()
            .unwrap()
            .to_string();
        // Process completion and pipe EOF are separate events on busy CI runners.
        // The completion inbox requires EOF, so wait for that rather than a
        // process state transition that may arrive before output is drained.
        let started_wait = std::time::Instant::now();
        loop {
            let peek = state
                .jobs
                .output_for_caller(
                    crate::tooling::job::JobOutputRequest {
                        job_id: job_id.clone(),
                        stdout_since_seq: None,
                        stderr_since_seq: None,
                        max_bytes: None,
                    },
                    "session:a",
                )
                .unwrap();
            if peek.eof {
                break;
            }
            assert!(started_wait.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(10));
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
        assert!(completed[0].get("command").is_none());
        assert!(completed[0].get("cwd").is_none());

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
            "exec",
            json!({
                "target": "local",
                "command": "sleep 0.02; printf waited",
                "timeout_ms": 5000,
                "yield_time_ms": 0
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
            "job",
            json!({ "job_id": job_id, "action": "wait", "wait_ms": 5000 }),
        );
        assert_eq!(waited["result"]["structuredContent"]["stdout"], "waited");

        let next = call_tool_for(state, "session:a", 3, "target_current", json!({}));
        assert!(next["result"]["structuredContent"]
            .get("completed_jobs")
            .is_none());
    }
}
