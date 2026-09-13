use serde_json::{json, Value};

pub(super) fn output_schema(name: &str) -> Value {
    let mut schema = match name {
        "server_info" => json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "version": { "type": "string" },
                "active_target": nullable_string_schema(),
                "ssh_session_ids": string_array_schema(),
                "terminal_ids": string_array_schema(),
                "job_ids": string_array_schema(),
                "runtime_dir": { "type": "string" },
                "started_at_debug": { "type": "string" },
                "notes": string_array_schema()
            },
            "required": ["name", "version", "active_target", "ssh_session_ids", "terminal_ids", "job_ids", "runtime_dir", "started_at_debug", "notes"],
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
                "command": { "type": "string" },
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
            "required": ["command", "resolved_target"],
            "additionalProperties": false
        }),
        "exec_stream" => json!({
            "type": "object",
            "properties": {
                "attached": { "type": "boolean" },
                "session_id": nullable_string_schema(),
                "target": nullable_string_schema(),
                "status": nullable_string_schema(),
                "exit_code": nullable_integer_schema(),
                "elapsed_ms": { "type": ["integer", "null"], "minimum": 0 },
                "stdout_from_seq": { "type": "integer", "minimum": 0 },
                "stdout_next_seq": { "type": "integer", "minimum": 0 },
                "stdout": { "type": "string" },
                "stdout_truncated": { "type": "boolean" },
                "stderr_from_seq": { "type": "integer", "minimum": 0 },
                "stderr_next_seq": { "type": "integer", "minimum": 0 },
                "stderr": { "type": "string" },
                "stderr_truncated": { "type": "boolean" },
                "eof": { "type": "boolean" }
            },
            "required": ["attached", "stdout_from_seq", "stdout_next_seq", "stdout_truncated", "stderr_from_seq", "stderr_next_seq", "stderr_truncated", "eof"],
            "additionalProperties": false
        }),
        "result_read" => json!({
            "type": "object",
            "properties": {
                "found": { "type": "boolean" },
                "data": {}
            },
            "required": ["found"],
            "additionalProperties": false
        }),
        "exec_batch" => json!({
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
        }),
        "exec_start" => json!({
            "type": "object",
            "properties": {
                "resolved_target": resolved_target_schema(),
                "job_id": { "type": "string" },
                "status": { "type": "string", "enum": ["running"] }
            },
            "required": ["resolved_target", "job_id", "status"],
            "additionalProperties": false
        }),
        "job_poll" => json!({
            "type": "object",
            "properties": {
                "job_id": { "type": "string" },
                "target": { "type": "string" },
                "status": { "type": "string", "enum": ["running", "completed", "failed", "cancelled", "timed_out"] },
                "exit_code": nullable_integer_schema(),
                "elapsed_ms": { "type": "integer", "minimum": 0 },
                "timed_out": { "type": "boolean" },
                "cancel_requested": { "type": "boolean" }
            },
            "required": ["job_id", "target", "status", "elapsed_ms", "timed_out", "cancel_requested"],
            "additionalProperties": false
        }),
        "job_output" => json!({
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
                "eof": { "type": "boolean" }
            },
            "required": ["job_id", "target", "status", "elapsed_ms", "timed_out", "stdout_from_seq", "stdout_next_seq", "stdout", "stdout_truncated", "stderr_from_seq", "stderr_next_seq", "stderr", "stderr_truncated", "eof"],
            "additionalProperties": false
        }),
        "job_wait" => json!({
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
                "wait_timed_out": { "type": "boolean" }
            },
            "required": ["job_id", "target", "status", "elapsed_ms", "timed_out", "stdout_from_seq", "stdout_next_seq", "stdout", "stdout_truncated", "stderr_from_seq", "stderr_next_seq", "stderr", "stderr_truncated", "eof", "wait_timed_out"],
            "additionalProperties": false
        }),
        "job_cancel" => json!({
            "type": "object",
            "properties": {
                "job_id": { "type": "string" },
                "cancel_requested": { "type": "boolean" },
                "status": { "type": "string", "enum": ["running", "completed", "failed", "cancelled", "timed_out"] }
            },
            "required": ["job_id", "cancel_requested", "status"],
            "additionalProperties": false
        }),
        "file_read" => json!({
            "type": "object",
            "properties": {
                "resolved_target": resolved_target_schema(),
                "path": { "type": "string" },
                "encoding": { "type": "string", "enum": ["utf-8", "base64"] },
                "content": { "type": "string" },
                "sha256": { "type": "string" },
                "bytes": { "type": "integer", "minimum": 0 },
                "truncated": { "type": "boolean" },
                "start_line": { "type": "integer", "minimum": 1 },
                "end_line": { "type": "integer", "minimum": 0 },
                "requested_count": { "type": "integer", "minimum": 1 },
                "succeeded": { "type": "integer", "minimum": 0 },
                "failed": { "type": "integer", "minimum": 0 },
                "files": {
                    "type": "array",
                    "items": file_read_batch_item_schema()
                }
            },
            "required": ["resolved_target", "truncated"],
            "additionalProperties": false
        }),
        "file_list" => json!({
            "type": "object",
            "properties": {
                "resolved_target": resolved_target_schema(),
                "path": { "type": "string" },
                "entries": {
                    "type": "array",
                    "items": file_entry_schema()
                }
            },
            "required": ["resolved_target", "path", "entries"],
            "additionalProperties": false
        }),
        "file_edit" => json!({
            "type": "object",
            "properties": {
                "resolved_target": resolved_target_schema(),
                "path": { "type": "string" },
                "changed": { "type": "boolean" },
                "written": { "type": "boolean" },
                "old_sha256": { "type": "string" },
                "new_sha256": { "type": "string" },
                "diff": { "type": "string" }
            },
            "required": ["resolved_target", "path", "changed", "written", "old_sha256", "new_sha256", "diff"],
            "additionalProperties": false
        }),
        "file_write" => json!({
            "type": "object",
            "properties": {
                "resolved_target": resolved_target_schema(),
                "path": { "type": "string" },
                "created": { "type": "boolean" },
                "written": { "type": "boolean" },
                "old_sha256": nullable_string_schema(),
                "new_sha256": { "type": "string" },
                "bytes": { "type": "integer", "minimum": 0 },
                "encoding": { "type": "string", "enum": ["utf-8", "base64"] },
                "content": nullable_string_schema(),
                "diff": { "type": "string" }
            },
            "required": ["resolved_target", "path", "created", "written", "new_sha256", "bytes", "encoding", "diff"],
            "additionalProperties": false
        }),
        "file_delete" => json!({
            "type": "object",
            "properties": {
                "resolved_target": resolved_target_schema(),
                "path": { "type": "string" },
                "deleted": { "type": "boolean" },
                "written": { "type": "boolean" },
                "old_sha256": { "type": "string" },
                "bytes": { "type": "integer", "minimum": 0 },
                "encoding": { "type": "string", "enum": ["utf-8", "base64"] },
                "content": { "type": "string" }
            },
            "required": ["resolved_target", "path", "deleted", "written", "old_sha256", "bytes", "encoding", "content"],
            "additionalProperties": false
        }),
        "file_import" => json!({
            "type": "object",
            "properties": {
                "source": {
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "enum": ["download_url", "local_path"] },
                        "file_name": { "type": "string" },
                        "mime_type": { "type": "string" },
                        "bytes": { "type": "integer", "minimum": 0 },
                        "sha256": { "type": "string" }
                    },
                    "required": ["kind", "bytes", "sha256"],
                    "additionalProperties": false
                },
                "resolved_target": resolved_target_schema(),
                "path": { "type": "string" },
                "created": { "type": "boolean" },
                "written": { "type": "boolean" },
                "old_sha256": nullable_string_schema(),
                "new_sha256": { "type": "string" },
                "bytes": { "type": "integer", "minimum": 0 },
                "encoding": { "type": "string", "enum": ["utf-8", "base64"] },
                "content": nullable_string_schema(),
                "diff": { "type": "string" }
            },
            "required": ["source", "resolved_target", "path", "created", "written", "new_sha256", "bytes", "encoding", "diff"],
            "additionalProperties": false
        }),
        "file_export" => json!({
            "type": "object",
            "properties": {
                "resolved_target": resolved_target_schema(),
                "path": { "type": "string" },
                "delivery": { "type": "string", "enum": ["link", "attachment"] },
                "file": {
                    "type": "object",
                    "properties": {
                        "file_name": { "type": "string" },
                        "mime_type": { "type": "string" },
                        "bytes": { "type": "integer", "minimum": 0 },
                        "sha256": { "type": "string" }
                    },
                    "required": ["file_name", "mime_type", "bytes", "sha256"],
                    "additionalProperties": false
                },
                "download": {
                    "type": "object",
                    "properties": {
                        "url": { "type": "string" },
                        "file_name": { "type": "string" },
                        "mime_type": { "type": "string" },
                        "bytes": { "type": "integer", "minimum": 0 },
                        "sha256": { "type": "string" },
                        "expires_at_unix_secs": { "type": "integer", "minimum": 0 },
                        "single_use": { "type": "boolean" }
                    },
                    "required": ["url", "file_name", "mime_type", "bytes", "sha256", "expires_at_unix_secs", "single_use"],
                    "additionalProperties": false
                }
            },
            "required": ["resolved_target", "path", "delivery"],
            "additionalProperties": false
        }),
        "file_patch" => json!({
            "type": "object",
            "properties": {
                "resolved_target": resolved_target_schema(),
                "path": { "type": "string" },
                "changed": { "type": "boolean" },
                "written": { "type": "boolean" },
                "old_sha256": nullable_string_schema(),
                "new_sha256": nullable_string_schema(),
                "diff": { "type": "string" },
                "files": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" },
                            "changed": { "type": "boolean" },
                            "written": { "type": "boolean" },
                            "old_sha256": { "type": "string" },
                            "new_sha256": { "type": "string" }
                        },
                        "required": ["path", "changed", "written", "old_sha256", "new_sha256"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["resolved_target", "path", "changed", "written", "old_sha256", "new_sha256", "diff", "files"],
            "additionalProperties": false
        }),
        "file_find" => json!({
            "type": "object",
            "properties": {
                "resolved_target": resolved_target_schema(),
                "path": { "type": "string" },
                "sha256": { "type": "string" },
                "matches": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "line": { "type": "integer", "minimum": 1 },
                            "text": { "type": "string" },
                            "before": string_array_schema(),
                            "after": string_array_schema()
                        },
                        "required": ["line", "text", "before", "after"],
                        "additionalProperties": false
                    }
                },
                "truncated": { "type": "boolean" }
            },
            "required": ["resolved_target", "path", "sha256", "matches", "truncated"],
            "additionalProperties": false
        }),
        "file_move" => json!({
            "type": "object",
            "properties": {
                "resolved_target": resolved_target_schema(),
                "source": { "type": "string" },
                "destination": { "type": "string" },
                "moved": { "type": "boolean" }
            },
            "required": ["resolved_target", "source", "destination", "moved"],
            "additionalProperties": false
        }),
        "file_chmod" => json!({
            "type": "object",
            "properties": {
                "resolved_target": resolved_target_schema(),
                "path": { "type": "string" },
                "mode": { "type": "string" }
            },
            "required": ["resolved_target", "path", "mode"],
            "additionalProperties": false
        }),
        "directory_create" => json!({
            "type": "object",
            "properties": {
                "resolved_target": resolved_target_schema(),
                "path": { "type": "string" },
                "created": { "type": "boolean" }
            },
            "required": ["resolved_target", "path", "created"],
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
        "terminal_resize" => terminal_size_schema(),
        "terminal_close" => json!({
            "type": "object",
            "properties": {
                "terminal_id": { "type": "string" },
                "closed": { "type": "boolean" }
            },
            "required": ["terminal_id", "closed"],
            "additionalProperties": false
        }),
        _ => unreachable!("output schema missing for tool {name}"),
    };

    if caches_app_result(name) {
        schema["properties"]["result_id"] = json!({ "type": "string" });
    }
    schema["properties"]["completed_jobs"] = json!({
        "type": "array",
        "items": completed_job_schema()
    });
    schema
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

fn completed_job_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "job_id": { "type": "string" },
            "target": { "type": "string" },
            "command": { "type": "string" },
            "cwd": { "type": "string" },
            "status": { "type": "string", "enum": ["completed", "failed", "cancelled", "timed_out"] },
            "exit_code": nullable_integer_schema(),
            "elapsed_ms": { "type": "integer", "minimum": 0 },
            "timed_out": { "type": "boolean" },
            "stdout": { "type": "string" },
            "stderr": { "type": "string" },
            "stdout_truncated": { "type": "boolean" },
            "stderr_truncated": { "type": "boolean" }
        },
        "required": ["job_id", "target", "command", "status", "elapsed_ms", "timed_out", "stdout_truncated", "stderr_truncated"],
        "additionalProperties": false
    })
}

fn file_read_batch_item_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "index": { "type": "integer", "minimum": 0 },
            "path": { "type": "string" },
            "success": { "type": "boolean" },
            "encoding": { "type": "string", "enum": ["utf-8", "base64"] },
            "content": { "type": "string" },
            "sha256": { "type": "string" },
            "bytes": { "type": "integer", "minimum": 0 },
            "truncated": { "type": "boolean" },
            "start_line": { "type": "integer", "minimum": 1 },
            "end_line": { "type": "integer", "minimum": 0 },
            "error": { "type": "string" }
        },
        "required": ["index", "path", "success"],
        "additionalProperties": false
    })
}

fn exec_batch_item_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "index": { "type": "integer", "minimum": 0 },
            "command": { "type": "string" },
            "cwd": { "type": "string" },
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
        "required": ["index", "command", "elapsed_ms", "success"],
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

fn file_entry_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "path": { "type": "string" },
            "kind": { "type": "string" },
            "size": { "type": "integer", "minimum": 0 },
            "modified_unix": nullable_integer_schema()
        },
        "required": ["name", "path", "kind", "size", "modified_unix"],
        "additionalProperties": false
    })
}

fn terminal_size_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "terminal_id": { "type": "string" },
            "rows": { "type": "integer", "minimum": 0 },
            "cols": { "type": "integer", "minimum": 0 }
        },
        "required": ["terminal_id", "rows", "cols"],
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
