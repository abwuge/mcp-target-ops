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
    let value = match serde_json::from_slice::<Value>(bytes) {
        Ok(value) => value,
        Err(err) => {
            let response = parse_error(format!("parse error: {err}"));
            return Ok(Some(serde_json::to_vec(&response)?));
        }
    };

    handle_json_value(state, value)?
        .map(|response| serde_json::to_vec(&response))
        .transpose()
        .map_err(Error::Json)
}

pub fn handle_json_value(state: Arc<AppState>, value: Value) -> Result<Option<Value>> {
    if let Value::Array(requests) = value {
        if requests.is_empty() {
            return serde_json::to_value(parse_error("parse error: empty batch".to_string()))
                .map(Some)
                .map_err(Error::Json);
        }

        let mut responses = Vec::new();
        for request in requests {
            if let Some(response) = handle_request_value(Arc::clone(&state), request)? {
                responses.push(response);
            }
        }

        if responses.is_empty() {
            Ok(None)
        } else {
            Ok(Some(Value::Array(responses)))
        }
    } else {
        handle_request_value(state, value)
    }
}

fn handle_request_value(state: Arc<AppState>, value: Value) -> Result<Option<Value>> {
    let response = match serde_json::from_value::<RpcRequest>(value) {
        Ok(request) => handle_request(state, request),
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

fn handle_request(state: Arc<AppState>, request: RpcRequest) -> Option<RpcResponse> {
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
        "tools/call" => tools_call(state, request.params.unwrap_or_else(|| json!({}))),
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
    apps::read_resource(&params.uri, state.config.server.public_base_url.as_deref())
        .ok_or_else(|| Error::Tool(format!("unknown resource: {}", params.uri)))
}

fn initialize(state: &AppState, params: Value) -> Result<Value> {
    let requested_protocol = params
        .get("protocolVersion")
        .and_then(|v| v.as_str())
        .unwrap_or("2025-06-18");

    Ok(json!({
        "protocolVersion": requested_protocol,
        "capabilities": {
            "tools": { "listChanged": false },
            "resources": { "subscribe": false, "listChanged": false }
        },
        "serverInfo": {
            "name": state.config.server.name.clone(),
            "version": state.config.server.version.clone(),
        }
    }))
}

fn tools_call(state: Arc<AppState>, params: Value) -> Result<Value> {
    #[derive(Deserialize)]
    struct ToolCallParams {
        name: String,
        #[serde(default)]
        arguments: Option<Value>,
    }

    let params: ToolCallParams = serde_json::from_value(params)?;
    match tools::call_tool(
        state,
        &params.name,
        params.arguments.unwrap_or_else(|| json!({})),
    ) {
        Ok(mut value) => {
            let content = if params.name == "file_export" {
                let file = value
                    .get_mut("file")
                    .and_then(Value::as_object_mut)
                    .ok_or_else(|| {
                        Error::Tool("file_export result is missing file metadata".to_string())
                    })?;
                let blob = file
                    .remove("data_base64")
                    .and_then(|value| value.as_str().map(str::to_string))
                    .ok_or_else(|| {
                        Error::Tool("file_export result is missing encoded file data".to_string())
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

            Ok(json!({
                "content": content,
                "structuredContent": value,
                "isError": false,
            }))
        }
        Err(err) => Ok(json!({
            "content": [{
                "type": "text",
                "text": err.to_string(),
            }],
            "isError": true,
        })),
    }
}
