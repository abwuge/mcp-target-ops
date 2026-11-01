use super::compact;
use serde_json::{json, Value};

pub(super) fn output_schema(name: &str) -> Value {
    if let Some(mut schema) = compact::output_schema(name) {
        schema["properties"]["completed_jobs"] = json!({
            "type":"array", "items":completed_job_schema()
        });
        return schema;
    }
    let mut schema = match name {
        "server_info" => json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "version": { "type": "string" },
                "active_target": nullable_string_schema(),
                "ssh_session_ids": string_array_schema(),
                "ssh_master_ids": string_array_schema(),
                "terminal_ids": string_array_schema(),
                "job_ids": string_array_schema(),
                "downstream_mcp_servers": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string" },
                            "enabled": { "type": "boolean" }
                        },
                        "required": ["name", "enabled"],
                        "additionalProperties": false
                    }
                },
                "runtime_dir": { "type": "string" },
                "started_at_debug": { "type": "string" },
                "notes": string_array_schema()
            },
            "required": ["name", "version", "active_target", "ssh_session_ids", "ssh_master_ids", "terminal_ids", "job_ids", "downstream_mcp_servers", "runtime_dir", "started_at_debug", "notes"],
            "additionalProperties": false
        }),
        "target_list" => json!({
            "type": "object",
            "properties": {
                "targets": {
                    "type": "array",
                    "items": target_summary_schema()
                }
            },
            "required": ["targets"],
            "additionalProperties": false
        }),
        "target_current" => json!({
            "type": "object",
            "properties": { "active_target": nullable_string_schema() },
            "required": ["active_target"],
            "additionalProperties": false
        }),
        "target_select" => json!({
            "type": "object",
            "properties": {
                "active_target": { "type": "string" },
                "previous_target": nullable_string_schema()
            },
            "required": ["active_target", "previous_target"],
            "additionalProperties": false
        }),
        "target_connect" => connection_schema("connected"),
        "target_disconnect" => connection_schema("disconnected"),
        "mcp_server_list" => json!({
            "type": "object",
            "properties": {
                "servers": {
                    "type": "array",
                    "items": mcp_server_summary_schema()
                }
            },
            "required": ["servers"],
            "additionalProperties": false
        }),
        "mcp_tools_list" => json!({
            "type": "object",
            "properties": {
                "server": { "type": "string" },
                "result": {}
            },
            "required": ["server", "result"],
            "additionalProperties": false
        }),
        "mcp_tool_call" => json!({
            "type": "object",
            "properties": {
                "server": { "type": "string" },
                "tool": { "type": "string" },
                "result": {}
            },
            "required": ["server", "tool", "result"],
            "additionalProperties": false
        }),
        "exec" => json!({
            "type": "object",
            "properties": {
                "resolved_target": resolved_target_schema(),
                "job_id": { "type": "string" },
                "status": { "type": "string", "enum": ["running"] },
                "auto_backgrounded": { "type": "boolean" },
                "next_action": { "type": "string" },
                "exit_code": nullable_integer_schema(),
                "stdout": { "type": "string" },
                "stderr": { "type": "string" },
                "stdout_truncated": { "type": "boolean" },
                "stderr_truncated": { "type": "boolean" },
                "timed_out": { "type": "boolean" }
            },
            "required": ["resolved_target"],
            "additionalProperties": false
        }),
        "terminal_open" => json!({
            "type": "object",
            "properties": {
                "resolved_target": resolved_target_schema(),
                "terminal_id": { "type": "string" },
                "rows": { "type": "integer", "minimum": 0 },
                "cols": { "type": "integer", "minimum": 0 }
            },
            "required": ["resolved_target", "terminal_id", "rows", "cols"],
            "additionalProperties": false
        }),
        "terminal_send" => json!({
            "type": "object",
            "properties": {
                "terminal_id": { "type": "string" },
                "bytes_written": { "type": "integer", "minimum": 0 }
            },
            "required": ["terminal_id", "bytes_written"],
            "additionalProperties": false
        }),
        "terminal_read" => json!({
            "type": "object",
            "properties": {
                "terminal_id": { "type": "string" },
                "target": { "type": "string" },
                "from_seq": { "type": "integer", "minimum": 0 },
                "next_seq": { "type": "integer", "minimum": 0 },
                "output": { "type": "string" },
                "truncated": { "type": "boolean" },
                "eof": { "type": "boolean" }
            },
            "required": ["terminal_id", "target", "from_seq", "next_seq", "output", "truncated", "eof"],
            "additionalProperties": false
        }),
        "terminal_close" => json!({
            "type": "object",
            "properties": {
                "terminal_id": { "type": "string" },
                "closed": { "type": "boolean" }
            },
            "required": ["terminal_id", "closed"],
            "additionalProperties": false
        }),
        "job" => job_result_schema(),
        _ => unreachable!("output schema missing for tool {name}"),
    };

    match name {
        "exec" => {
            let batch = batch_result_schema();
            for (key, value) in batch["properties"].as_object().unwrap() {
                schema["properties"][key] = value.clone();
            }
            schema["properties"]["job_id"] = json!({"type":"string"});
            schema["properties"]["status"] = json!({"type":"string"});
            schema["properties"]["auto_backgrounded"] = json!({"type":"boolean"});
            schema["properties"]["timed_out"] =
                json!({"oneOf":[{"type":"boolean"},{"type":"integer"}]});
            schema["properties"]["kind"] = json!({"type":"string"});
            schema["properties"]["target"] = json!({"type":"string"});
            schema["properties"]["done"] = json!({"type":"boolean"});
            schema["properties"]["cancel_requested"] = json!({"type":"boolean"});
            schema["properties"]["completed_count"] = json!({"type":"integer"});
            schema["properties"]["elapsed_ms"] = json!({"type":"integer"});
        }
        "job" => {
            schema = job_result_schema();
            schema["properties"]["results"] =
                json!({"type":"array","items":exec_batch_item_schema()});
            schema["properties"]["kind"] = json!({"type":"string"});
            schema["properties"]["done"] = json!({"type":"boolean"});
            schema["properties"]["completed_count"] = json!({"type":"integer"});
            schema["properties"]["requested_count"] = json!({"type":"integer"});
            schema["properties"]["executed_count"] = json!({"type":"integer"});
            schema["properties"]["cancel_requested"] = json!({"type":"boolean"});
            schema["properties"]["resolved_target"] = resolved_target_schema();
            schema["properties"]["mode"] = json!({"type":"string"});
            schema["properties"]["succeeded"] = json!({"type":"integer"});
            schema["properties"]["failed"] = json!({"type":"integer"});
            schema["properties"]["stopped_early"] = json!({"type":"boolean"});
            schema["properties"]["timed_out"] =
                json!({"oneOf":[{"type":"boolean"},{"type":"integer"}]});
            schema["required"] = json!(["job_id", "status"]);
        }
        _ => {}
    }

    schema["properties"]["completed_jobs"] = json!({
        "type": "array",
        "items": completed_job_schema()
    });
    schema
}

fn batch_result_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "resolved_target": resolved_target_schema(),
            "mode": { "type": "string", "enum": ["sequential", "parallel"] },
            "requested_count": { "type": "integer", "minimum": 1 },
            "executed_count": { "type": "integer", "minimum": 0 },
            "succeeded": { "type": "integer", "minimum": 0 },
            "failed": { "type": "integer", "minimum": 0 },
            "timed_out": { "type": "integer", "minimum": 0 },
            "stopped_early": { "type": "boolean" },
            "results": {
                "type": "array",
                "items": exec_batch_item_schema()
            }
        },
        "required": ["resolved_target", "mode", "requested_count", "executed_count", "succeeded", "failed", "timed_out", "stopped_early", "results"],
        "additionalProperties": false
    })
}

fn job_result_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "job_id": { "type": "string" },
            "target": { "type": "string" },
            "status": { "type": "string", "enum": ["running", "completed", "failed", "cancelled", "timed_out"] },
            "exit_code": nullable_integer_schema(),
            "elapsed_ms": { "type": "integer", "minimum": 0 },
            "timed_out": { "type": "boolean" },
            "stdout_from_seq": { "type": "integer", "minimum": 0 },
            "stdout_next_seq": { "type": "integer", "minimum": 0 },
            "stdout": { "type": "string" },
            "stdout_truncated": { "type": "boolean" },
            "stderr_from_seq": { "type": "integer", "minimum": 0 },
            "stderr_next_seq": { "type": "integer", "minimum": 0 },
            "stderr": { "type": "string" },
            "stderr_truncated": { "type": "boolean" },
            "eof": { "type": "boolean" },
            "wait_timed_out": { "type": "boolean" },
            "next_action": { "type": "string" }
        },
        "required": ["job_id", "target", "status", "elapsed_ms", "timed_out", "stdout_from_seq", "stdout_next_seq", "stdout", "stdout_truncated", "stderr_from_seq", "stderr_next_seq", "stderr", "stderr_truncated", "eof", "wait_timed_out"],
        "additionalProperties": false
    })
}

fn completed_job_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "job_id": { "type": "string" },
            "target": { "type": "string" },
            "status": { "type": "string", "enum": ["completed", "failed", "cancelled", "timed_out"] },
            "exit_code": nullable_integer_schema(),
            "elapsed_ms": { "type": "integer", "minimum": 0 },
            "timed_out": { "type": "boolean" },
            "stdout": { "type": "string" },
            "stderr": { "type": "string" },
            "stdout_truncated": { "type": "boolean" },
            "stderr_truncated": { "type": "boolean" }
        },
        "required": ["job_id", "target", "status", "elapsed_ms", "timed_out", "stdout_truncated", "stderr_truncated"],
        "additionalProperties": false
    })
}

fn exec_batch_item_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "index": { "type": "integer", "minimum": 0 },
            "status": { "type": "string" },
            "stdout_next_seq": { "type": "integer" },
            "stderr_next_seq": { "type": "integer" },
            "exit_code": nullable_integer_schema(),
            "stdout": { "type": "string" },
            "stderr": { "type": "string" },
            "stdout_truncated": { "type": "boolean" },
            "stderr_truncated": { "type": "boolean" },
            "timed_out": { "type": "boolean" },
            "error": { "type": "string" },
            "elapsed_ms": { "type": "integer", "minimum": 0 },
            "success": { "type": "boolean" }
        },
        "required": ["index", "elapsed_ms"],
        "additionalProperties": false
    })
}

fn resolved_target_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target": { "type": "string" },
            "source": { "type": "string", "enum": ["explicit", "active", "default"] }
        },
        "required": ["target", "source"],
        "additionalProperties": false
    })
}

fn target_summary_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": { "type": "string" },
            "kind": { "type": "string", "enum": ["local", "ssh"] },
            "config_key": { "type": "string" },
            "enabled": { "type": "boolean" },
            "active": { "type": "boolean" },
            "policy": {
                "type": "object",
                "properties": {
                    "allow_exec": { "type": "boolean" },
                    "allow_terminal": { "type": "boolean" },
                    "allow_file_read": { "type": "boolean" },
                    "allow_file_write": { "type": "boolean" },
                    "allow_select_active": { "type": "boolean" },
                    "require_explicit_target_for_write": { "type": "boolean" },
                    "allowed_roots": string_array_schema()
                },
                "required": ["allow_exec", "allow_terminal", "allow_file_read", "allow_file_write", "allow_select_active", "require_explicit_target_for_write", "allowed_roots"],
                "additionalProperties": false
            }
        },
        "required": ["id", "kind", "config_key", "enabled", "active", "policy"],
        "additionalProperties": false
    })
}

fn mcp_server_summary_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "enabled": { "type": "boolean" },
            "config_source": { "type": "string", "enum": ["file", "secret_ref", "inline"] },
            "secret_target": nullable_string_schema(),
            "static_header_names": string_array_schema(),
            "secret_header_names": string_array_schema(),
            "timeout_ms": { "type": "integer", "minimum": 1 },
            "max_response_bytes": { "type": "integer", "minimum": 1 }
        },
        "required": ["name", "enabled", "config_source", "secret_target", "static_header_names", "secret_header_names", "timeout_ms", "max_response_bytes"],
        "additionalProperties": false
    })
}

fn connection_schema(status_field: &str) -> Value {
    let mut properties = serde_json::Map::new();
    properties.insert("resolved_target".to_string(), resolved_target_schema());
    properties.insert(status_field.to_string(), json!({ "type": "boolean" }));
    properties.insert("message".to_string(), json!({ "type": "string" }));
    properties.insert("exit_code".to_string(), nullable_integer_schema());
    properties.insert("stdout".to_string(), json!({ "type": "string" }));
    properties.insert("stderr".to_string(), json!({ "type": "string" }));
    properties.insert("timed_out".to_string(), json!({ "type": "boolean" }));
    json!({
        "type": "object",
        "properties": properties,
        "required": ["resolved_target", status_field],
        "additionalProperties": false
    })
}

fn nullable_string_schema() -> Value {
    json!({ "type": ["string", "null"] })
}

fn nullable_integer_schema() -> Value {
    json!({ "type": ["integer", "null"] })
}

fn string_array_schema() -> Value {
    json!({ "type": "array", "items": { "type": "string" } })
}
