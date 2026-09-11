use crate::{
    core::{
        config::TargetConfig,
        error::{Error, Result},
        policy,
        state::AppState,
        target::{ResolvedTarget, TargetId, TargetSource},
    },
    tooling::{
        exec::{self, ExecRequest},
        fs::{
            self, DirectoryCreateRequest, FileChmodRequest, FileEditRequest, FileFindRequest,
            FileListRequest, FileMoveRequest, FilePatchRequest, FileReadRequest, FileWriteRequest,
        },
        job::{ExecStartRequest, JobCancelRequest, JobOutputRequest, JobPollRequest},
        terminal::{
            TerminalCloseRequest, TerminalOpenRequest, TerminalReadRequest, TerminalResizeRequest,
            TerminalSendRequest,
        },
    },
    transport::ssh,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{str::FromStr, sync::Arc, time::Duration};

pub fn list_tools(oauth_scopes: Option<&[String]>) -> Value {
    let security_schemes = oauth_scopes.map(|scopes| {
        json!([{
            "type": "oauth2",
            "scopes": scopes,
        }])
    });
    let tool = |name: &str, description: &str, input_schema: Value| {
        tool(
            name,
            description,
            input_schema,
            output_schema(name),
            security_schemes.as_ref(),
        )
    };

    json!([
        tool("server_info", "Return server configuration summary and active target state.", object_schema(vec![])),
        tool("target_list", "List configured local and SSH targets, including policy summaries and active marker.", object_schema(vec![])),
        tool("target_current", "Return the currently selected active target, if any.", object_schema(vec![])),
        tool("target_select", "Select a session-scoped active target. Later calls may omit target and use this sticky target.", object_schema(vec![required_string("target", "Target id: local or ssh:<profile>")])),
        tool("target_connect", "Connect or warm an SSH target persistent worker.", object_schema(vec![required_string("target", "Target id: local or ssh:<profile>")])),
        tool("target_disconnect", "Disconnect an SSH target persistent worker, or no-op for local targets.", object_schema(vec![required_string("target", "Target id: local or ssh:<profile>")])),
        tool("exec", "Run a non-interactive command on the explicit target or current active target.", object_schema(vec![
            optional_string("target", "Target id: local or ssh:<profile>. Omit to use active target."),
            required_string("command", "Shell command to execute."),
            optional_string("cwd", "Working directory."),
            optional_integer("timeout_ms", "Timeout in milliseconds."),
            optional_integer("max_output_bytes", "Maximum bytes to return for stdout and stderr."),
            optional_value("secret_env", "Map environment variable names to secret references. Secret values are resolved inside Target Ops and are not included in the tool request or tool metadata; commands can still expose them if they print their environment.", secret_env_schema()),
        ])),
        tool("exec_start", "Start a non-interactive command as a background job. Jobs use dedicated processes, support incremental output and cancellation, and do not block later tool calls.", object_schema(vec![
            optional_string("target", "Target id: local or ssh:<profile>. Omit to use active target."),
            required_string("command", "Shell command to execute."),
            optional_string("cwd", "Working directory."),
            optional_integer("timeout_ms", "Optional job timeout in milliseconds. Omit for no runtime timeout."),
            optional_integer("max_output_bytes", "Maximum retained bytes for each stdout and stderr buffer."),
            optional_value("secret_env", "Map environment variable names to secret references. Secret values are resolved inside Target Ops and are not included in the tool request or tool metadata; commands can still expose them if they print their environment.", secret_env_schema()),
        ])),
        tool("job_poll", "Return the current state and exit information for a background job.", object_schema(vec![
            required_string("job_id", "Job id returned by exec_start."),
        ])),
        tool("job_output", "Read incremental stdout and stderr from a background job using independent sequence cursors.", object_schema(vec![
            required_string("job_id", "Job id returned by exec_start."),
            optional_integer("stdout_since_seq", "Last stdout sequence already consumed. Omit or use 0 for buffered output."),
            optional_integer("stderr_since_seq", "Last stderr sequence already consumed. Omit or use 0 for buffered output."),
            optional_integer("max_bytes", "Maximum bytes to return from each stream."),
        ])),
        tool("job_cancel", "Request cancellation of a running background job.", object_schema(vec![
            required_string("job_id", "Job id returned by exec_start."),
        ])),
        tool("file_read", "Read a UTF-8 or binary file from the explicit target or active target.", object_schema(vec![
            optional_string("target", "Target id. Omit to use active target."),
            required_string("path", "File path."),
            optional_integer("max_bytes", "Maximum bytes to return."),
            optional_integer("start_line", "Optional 1-based first line to return for UTF-8 files."),
            optional_integer("end_line", "Optional 1-based inclusive last line to return for UTF-8 files."),
            optional_integer("timeout_ms", "Timeout in milliseconds for remote file access."),
        ])),
        tool("file_list", "List one directory on the explicit target or active target.", object_schema(vec![
            optional_string("target", "Target id. Omit to use active target."),
            required_string("path", "Directory path."),
            optional_integer("timeout_ms", "Timeout in milliseconds for remote directory access."),
        ])),
        tool("file_edit", "Apply exact text replacements with sha256 compare-and-swap support. Existing file permissions are preserved. Writes require explicit target by default.", file_edit_schema()),
        tool("file_write", "Create or replace a UTF-8 or base64 file atomically. Existing files require overwrite=true or an expected sha256. Writes require explicit target by default.", file_write_schema()),
        tool("file_patch", "Apply a unified diff to one UTF-8 file with optional sha256 compare-and-swap protection. Existing file permissions are preserved.", file_patch_schema()),
        tool("file_find", "Find literal text in one UTF-8 file and return matching lines with bounded context.", file_find_schema()),
        tool("file_move", "Move or rename a file or directory within one target. Existing destinations are not replaced unless overwrite=true.", file_move_schema()),
        tool("file_chmod", "Change the Unix mode of a file or directory using an octal mode string.", file_chmod_schema()),
        tool("directory_create", "Create a directory on the selected target, recursively by default.", directory_create_schema()),
        tool("terminal_open", "Open a persistent PTY terminal on the explicit target or active target.", object_schema(vec![
            optional_string("target", "Target id. Omit to use active target."),
            optional_string("cwd", "Initial working directory."),
            optional_string("shell", "Shell program to run."),
            optional_integer("rows", "PTY rows."),
            optional_integer("cols", "PTY columns."),
        ])),
        tool("terminal_send", "Send input to an existing terminal_id. The terminal is already bound to its target.", object_schema(vec![
            required_string("terminal_id", "Terminal id from terminal_open."),
            required_string("input", "Input bytes represented as UTF-8 text, usually ending in newline."),
        ])),
        tool("terminal_read", "Read incremental output from an existing terminal_id.", object_schema(vec![
            required_string("terminal_id", "Terminal id from terminal_open."),
            optional_integer("since_seq", "Last seen sequence number. Omit or 0 to read buffered output."),
            optional_integer("max_bytes", "Maximum output bytes."),
        ])),
        tool("terminal_resize", "Record a terminal resize request. Actual PTY resize is marked TODO in this MVP.", object_schema(vec![
            required_string("terminal_id", "Terminal id from terminal_open."),
            required_integer("rows", "PTY rows."),
            required_integer("cols", "PTY columns."),
        ])),
        tool("terminal_close", "Close an existing terminal session.", object_schema(vec![
            required_string("terminal_id", "Terminal id from terminal_open."),
        ])),
    ])
}

pub fn call_tool(state: Arc<AppState>, name: &str, args: Value) -> Result<Value> {
    match name {
        "server_info" => Ok(server_info(&state)),
        "target_list" => Ok(json!({ "targets": state.list_targets() })),
        "target_current" => Ok(target_current(&state)),
        "target_select" => target_select(&state, parse(args)?),
        "target_connect" => target_connect(&state, parse(args)?),
        "target_disconnect" => target_disconnect(&state, parse(args)?),
        "exec" => Ok(serde_json::to_value(exec::run(
            &state,
            parse::<ExecRequest>(args)?,
        )?)?),
        "exec_start" => Ok(serde_json::to_value(
            state.jobs.start(&state, parse::<ExecStartRequest>(args)?)?,
        )?),
        "job_poll" => {
            Ok(serde_json::to_value(state.jobs.poll(parse::<
                JobPollRequest,
            >(
                args
            )?)?)?)
        }
        "job_output" => {
            Ok(serde_json::to_value(state.jobs.output(parse::<
                JobOutputRequest,
            >(
                args
            )?)?)?)
        }
        "job_cancel" => {
            Ok(serde_json::to_value(state.jobs.cancel(parse::<
                JobCancelRequest,
            >(
                args
            )?)?)?)
        }
        "file_read" => Ok(serde_json::to_value(fs::read(
            &state,
            parse::<FileReadRequest>(args)?,
        )?)?),
        "file_list" => Ok(serde_json::to_value(fs::list(
            &state,
            parse::<FileListRequest>(args)?,
        )?)?),
        "file_edit" => Ok(serde_json::to_value(fs::edit(
            &state,
            parse::<FileEditRequest>(args)?,
        )?)?),
        "file_write" => Ok(serde_json::to_value(fs::write(
            &state,
            parse::<FileWriteRequest>(args)?,
        )?)?),
        "file_patch" => Ok(serde_json::to_value(fs::patch(
            &state,
            parse::<FilePatchRequest>(args)?,
        )?)?),
        "file_find" => Ok(serde_json::to_value(fs::find(
            &state,
            parse::<FileFindRequest>(args)?,
        )?)?),
        "file_move" => Ok(serde_json::to_value(fs::move_path(
            &state,
            parse::<FileMoveRequest>(args)?,
        )?)?),
        "file_chmod" => Ok(serde_json::to_value(fs::chmod(
            &state,
            parse::<FileChmodRequest>(args)?,
        )?)?),
        "directory_create" => Ok(serde_json::to_value(fs::create_directory(
            &state,
            parse::<DirectoryCreateRequest>(args)?,
        )?)?),
        "terminal_open" => Ok(serde_json::to_value(
            state
                .terminals
                .open(&state, parse::<TerminalOpenRequest>(args)?)?,
        )?),
        "terminal_send" => Ok(serde_json::to_value(state.terminals.send(parse::<
            TerminalSendRequest,
        >(
            args
        )?)?)?),
        "terminal_read" => Ok(serde_json::to_value(state.terminals.read(parse::<
            TerminalReadRequest,
        >(
            args
        )?)?)?),
        "terminal_resize" => Ok(serde_json::to_value(state.terminals.resize(parse::<
            TerminalResizeRequest,
        >(
            args
        )?)?)?),
        "terminal_close" => Ok(serde_json::to_value(state.terminals.close(parse::<
            TerminalCloseRequest,
        >(
            args
        )?)?)?),
        other => Err(Error::Tool(format!("unknown tool: {other}"))),
    }
}

#[derive(Debug, Deserialize)]
struct TargetRequest {
    target: String,
}

fn parse<T: for<'de> Deserialize<'de>>(value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(Error::Json)
}

fn server_info(state: &AppState) -> Value {
    let active_target = state.current_target().map(|target| target.to_string());
    json!({
        "name": state.config.server.name.clone(),
        "version": state.config.server.version.clone(),
        "active_target": active_target,
        "ssh_session_ids": state.ssh_sessions.ids(),
        "terminal_ids": state.terminals.ids(),
        "job_ids": state.jobs.ids(),
        "runtime_dir": state.config.server.runtime_dir.display().to_string(),
        "started_at_debug": format!("{:?}", state.started_at()),
        "notes": [
            "MVP stdio MCP implementation with tools/list and tools/call.",
            "The SSH backend uses persistent per-target OpenSSH worker processes for exec and file operations.",
            "target is sticky only inside this MCP server process/session.",
            "background jobs use dedicated processes so they do not monopolize persistent SSH workers."
        ]
    })
}

fn target_current(state: &AppState) -> Value {
    match state.current_target() {
        Some(target) => json!({ "active_target": target.to_string() }),
        None => json!({ "active_target": null }),
    }
}

fn target_select(state: &AppState, req: TargetRequest) -> Result<Value> {
    let target = TargetId::from_str(&req.target)?;
    let config = state.get_target_config(&target)?;
    policy::check_target_enabled(&target, config)?;
    policy::check_select_active(&target, config)?;
    let previous = state.set_active_target(target.clone());
    Ok(json!({
        "active_target": target.to_string(),
        "previous_target": previous.map(|t| t.to_string()),
    }))
}

fn target_connect(state: &AppState, req: TargetRequest) -> Result<Value> {
    let target = TargetId::from_str(&req.target)?;
    let config = state.get_target_config(&target)?;
    policy::check_target_enabled(&target, config)?;
    match (target.clone(), config) {
        (TargetId::Local, TargetConfig::Local(_)) => Ok(json!({
            "resolved_target": ResolvedTarget::new(target, TargetSource::Explicit),
            "connected": true,
            "message": "local target is always available when enabled"
        })),
        (TargetId::Ssh(name), TargetConfig::Ssh(ssh_config)) => {
            let timeout = Duration::from_millis(policy::target_policy(config).default_timeout_ms);
            let output = ssh::connect(&state.ssh_sessions, &name, ssh_config, timeout)?;
            Ok(json!({
                "resolved_target": ResolvedTarget::new(target, TargetSource::Explicit),
                "connected": output.exit_code == Some(0),
                "exit_code": output.exit_code,
                "stdout": String::from_utf8_lossy(&output.stdout),
                "stderr": String::from_utf8_lossy(&output.stderr),
                "timed_out": output.timed_out,
            }))
        }
        _ => Err(Error::Target(format!(
            "target {target} has mismatched config"
        ))),
    }
}

fn target_disconnect(state: &AppState, req: TargetRequest) -> Result<Value> {
    let target = TargetId::from_str(&req.target)?;
    let config = state.get_target_config(&target)?;
    match (target.clone(), config) {
        (TargetId::Local, TargetConfig::Local(_)) => Ok(json!({
            "resolved_target": ResolvedTarget::new(target, TargetSource::Explicit),
            "disconnected": true,
            "message": "local target has no connection to close"
        })),
        (TargetId::Ssh(name), TargetConfig::Ssh(_)) => {
            let timeout = Duration::from_millis(policy::target_policy(config).default_timeout_ms);
            let output = ssh::disconnect(&state.ssh_sessions, &name, timeout)?;
            Ok(json!({
                "resolved_target": ResolvedTarget::new(target, TargetSource::Explicit),
                "disconnected": output.exit_code == Some(0),
                "exit_code": output.exit_code,
                "stdout": String::from_utf8_lossy(&output.stdout),
                "stderr": String::from_utf8_lossy(&output.stderr),
                "timed_out": output.timed_out,
            }))
        }
        _ => Err(Error::Target(format!(
            "target {target} has mismatched config"
        ))),
    }
}

fn tool(
    name: &str,
    description: &str,
    input_schema: Value,
    output_schema: Value,
    security_schemes: Option<&Value>,
) -> Value {
    let mut descriptor = json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema,
        "outputSchema": output_schema,
    });

    if let Some(schemes) = security_schemes {
        let object = descriptor
            .as_object_mut()
            .expect("tool descriptor is an object");
        object.insert("securitySchemes".to_string(), schemes.clone());
        object.insert(
            "_meta".to_string(),
            json!({
                "securitySchemes": schemes,
            }),
        );
    }

    descriptor
}

fn output_schema(name: &str) -> Value {
    match name {
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
        "exec" => json!({
            "type": "object",
            "properties": {
                "resolved_target": resolved_target_schema(),
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
            "required": ["job_id", "target", "stdout_from_seq", "stdout_next_seq", "stdout", "stdout_truncated", "stderr_from_seq", "stderr_next_seq", "stderr", "stderr_truncated", "eof"],
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
                "end_line": { "type": "integer", "minimum": 0 }
            },
            "required": ["resolved_target", "path", "encoding", "content", "sha256", "bytes", "truncated"],
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
                "bytes": { "type": "integer", "minimum": 0 }
            },
            "required": ["resolved_target", "path", "created", "written", "new_sha256", "bytes"],
            "additionalProperties": false
        }),
        "file_patch" => json!({
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
    }
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

#[derive(Debug, Clone)]
struct Prop {
    name: &'static str,
    value: Value,
    required: bool,
}

fn object_schema(props: Vec<Prop>) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for prop in props {
        properties.insert(prop.name.to_string(), prop.value);
        if prop.required {
            required.push(Value::String(prop.name.to_string()));
        }
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

fn required_string(name: &'static str, description: &'static str) -> Prop {
    Prop {
        name,
        required: true,
        value: json!({ "type": "string", "description": description }),
    }
}

fn optional_string(name: &'static str, description: &'static str) -> Prop {
    Prop {
        name,
        required: false,
        value: json!({ "type": "string", "description": description }),
    }
}

fn optional_value(name: &'static str, description: &'static str, schema: Value) -> Prop {
    let mut value = schema;
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "description".to_string(),
            Value::String(description.to_string()),
        );
    }
    Prop {
        name,
        required: false,
        value,
    }
}

fn required_integer(name: &'static str, description: &'static str) -> Prop {
    Prop {
        name,
        required: true,
        value: json!({ "type": "integer", "description": description }),
    }
}

fn optional_integer(name: &'static str, description: &'static str) -> Prop {
    Prop {
        name,
        required: false,
        value: json!({ "type": "integer", "description": description }),
    }
}

fn file_edit_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target": { "type": "string", "description": "Target id. For writes this is required by default policy." },
            "path": { "type": "string", "description": "UTF-8 text file path." },
            "expected_sha256": { "type": "string", "description": "Optional CAS guard from file_read." },
            "dry_run": { "type": "boolean", "description": "Return diff without writing." },
            "timeout_ms": { "type": "integer", "description": "Timeout for remote read/write." },
            "edits": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "old": { "type": "string" },
                        "new": { "type": "string" },
                        "replace_all": { "type": "boolean" }
                    },
                    "required": ["old", "new"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["path", "edits"],
        "additionalProperties": false
    })
}

fn secret_env_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": secret_ref_schema()
    })
}

fn secret_ref_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": { "type": "string", "description": "Secret source file on the selected target. The path must pass the target file-read policy." },
            "format": { "type": "string", "enum": ["text", "toml", "json"], "description": "Source format. Defaults to text." },
            "key": { "type": "string", "description": "Dot-separated scalar key for TOML or JSON sources." },
            "trim": { "type": "boolean", "description": "Trim surrounding whitespace from the resolved value. Defaults to true." }
        },
        "required": ["path"],
        "additionalProperties": false
    })
}

fn file_write_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target": { "type": "string", "description": "Target id. For writes this is required by default policy." },
            "path": { "type": "string", "description": "Destination file path." },
            "content": { "type": "string", "description": "File content encoded according to encoding." },
            "encoding": { "type": "string", "enum": ["utf8", "base64"], "description": "Content encoding. Defaults to utf8." },
            "expected_sha256": { "type": "string", "description": "Optional CAS guard for an existing file." },
            "overwrite": { "type": "boolean", "description": "Allow replacement without a CAS hash. Defaults to false." },
            "mode": { "type": "string", "description": "Optional octal mode such as 0644 or 0755. Existing mode is preserved when omitted." },
            "timeout_ms": { "type": "integer", "description": "Timeout for remote file access." }
        },
        "required": ["path", "content"],
        "additionalProperties": false
    })
}

fn file_patch_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target": { "type": "string", "description": "Target id. For writes this is required by default policy." },
            "path": { "type": "string", "description": "UTF-8 text file path." },
            "patch": { "type": "string", "description": "Unified diff that applies to this file." },
            "expected_sha256": { "type": "string", "description": "Optional CAS guard from file_read." },
            "dry_run": { "type": "boolean", "description": "Validate and return the patch result without writing." },
            "timeout_ms": { "type": "integer", "description": "Timeout for remote read/write." }
        },
        "required": ["path", "patch"],
        "additionalProperties": false
    })
}

fn file_find_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target": { "type": "string", "description": "Target id. Omit to use active target." },
            "path": { "type": "string", "description": "UTF-8 text file path." },
            "pattern": { "type": "string", "description": "Literal text to find." },
            "case_sensitive": { "type": "boolean", "description": "Use case-sensitive matching. Defaults to true." },
            "context_lines": { "type": "integer", "minimum": 0, "maximum": 20, "description": "Lines of context before and after each match. Defaults to 2." },
            "max_matches": { "type": "integer", "minimum": 1, "maximum": 200, "description": "Maximum matches to return. Defaults to 20." },
            "timeout_ms": { "type": "integer", "description": "Timeout for remote file access." }
        },
        "required": ["path", "pattern"],
        "additionalProperties": false
    })
}

fn file_move_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target": { "type": "string", "description": "Target id. Writes require an explicit target by default." },
            "source": { "type": "string", "description": "Existing source path." },
            "destination": { "type": "string", "description": "Destination path on the same target." },
            "overwrite": { "type": "boolean", "description": "Replace an existing destination. Defaults to false." },
            "timeout_ms": { "type": "integer", "description": "Timeout for remote file access." }
        },
        "required": ["source", "destination"],
        "additionalProperties": false
    })
}

fn file_chmod_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target": { "type": "string", "description": "Target id. Writes require an explicit target by default." },
            "path": { "type": "string", "description": "File or directory path." },
            "mode": { "type": "string", "description": "Octal mode such as 0644 or 0755." },
            "timeout_ms": { "type": "integer", "description": "Timeout for remote file access." }
        },
        "required": ["path", "mode"],
        "additionalProperties": false
    })
}

fn directory_create_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target": { "type": "string", "description": "Target id. Writes require an explicit target by default." },
            "path": { "type": "string", "description": "Directory path." },
            "recursive": { "type": "boolean", "description": "Create missing parents. Defaults to true." },
            "mode": { "type": "string", "description": "Optional octal mode such as 0755." },
            "timeout_ms": { "type": "integer", "description": "Timeout for remote file access." }
        },
        "required": ["path"],
        "additionalProperties": false
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_declares_an_object_output_schema() {
        let tools = list_tools(None);
        let tools = tools.as_array().expect("tool list is an array");

        assert_eq!(tools.len(), 25);
        for tool in tools {
            let name = tool["name"].as_str().expect("tool has a name");
            assert_eq!(
                tool["outputSchema"]["type"], "object",
                "tool {name} must declare an object output schema"
            );
        }
    }
}
