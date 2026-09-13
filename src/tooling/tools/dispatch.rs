use crate::{
    core::{
        config::TargetConfig,
        error::{Error, Result},
        policy,
        state::AppState,
        target::{ResolvedTarget, TargetId, TargetSource},
    },
    tooling::{
        exec::{self, ExecBatchRequest, ExecRequest},
        file_backup::{
            self, FileBackupDeleteRequest, FileBackupListRequest, FileBackupRequest,
            FileRestoreRequest,
        },
        file_bridge::{self, FileExportRequest, FileImportRequest},
        fs::{
            self, DirectoryCreateRequest, FileChmodRequest, FileDeleteRequest, FileEditRequest,
            FileFindRequest, FileListRequest, FileMoveRequest, FilePatchRequest, FileReadRequest,
            FileWriteRequest,
        },
        job::{
            ExecStartRequest, ExecStreamRequest, JobCancelRequest, JobOutputRequest,
            JobPollRequest, JobWaitRequest,
        },
        mcp_client,
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

pub fn call_tool(state: Arc<AppState>, name: &str, args: Value) -> Result<Value> {
    call_tool_with_context(state, name, args, None, "direct")
}

pub fn call_tool_with_context(
    state: Arc<AppState>,
    name: &str,
    args: Value,
    request_id: Option<&Value>,
    caller_key: &str,
) -> Result<Value> {
    let _ = state.backups.maybe_cleanup();
    match name {
        "server_info" => Ok(server_info(&state)),
        "target_list" => Ok(json!({ "targets": state.list_targets() })),
        "target_current" => Ok(target_current(&state)),
        "target_select" => target_select(&state, parse(args)?),
        "target_connect" => target_connect(&state, parse(args)?),
        "target_disconnect" => target_disconnect(&state, parse(args)?),
        "mcp_server_list" => Ok(json!({ "servers": mcp_client::list_servers(&state) })),
        "mcp_tools_list" => {
            let req = parse::<McpServerRequest>(args)?;
            let result = mcp_client::tools_list(&state, &req.server)?;
            Ok(json!({ "server": req.server, "result": result }))
        }
        "mcp_tool_call" => {
            let req = parse::<McpToolCallRequest>(args)?;
            let result = mcp_client::tool_call(
                &state,
                &req.server,
                &req.tool,
                req.arguments.unwrap_or_else(|| json!({})),
            )?;
            Ok(json!({ "server": req.server, "tool": req.tool, "result": result }))
        }
        "exec" => Ok(serde_json::to_value(exec::run_with_context(
            &state,
            parse::<ExecRequest>(args)?,
            request_id,
            caller_key,
        )?)?),
        "exec_batch" => Ok(serde_json::to_value(exec::run_batch(
            &state,
            parse::<ExecBatchRequest>(args)?,
        )?)?),
        "exec_stream" => {
            Ok(serde_json::to_value(state.jobs.stream(parse::<
                ExecStreamRequest,
            >(
                args
            )?)?)?)
        }
        "result_read" => {
            let result_id = args
                .get("result_id")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::Tool("result_read requires result_id".to_string()))?;
            Ok(serde_json::to_value(
                state.results.read_for_caller(caller_key, result_id)?,
            )?)
        }
        "exec_start" => Ok(serde_json::to_value(state.jobs.start_for_caller(
            &state,
            parse::<ExecStartRequest>(args)?,
            caller_key,
        )?)?),
        "job_poll" => Ok(serde_json::to_value(
            state
                .jobs
                .poll_for_caller(parse::<JobPollRequest>(args)?, caller_key)?,
        )?),
        "job_output" => Ok(serde_json::to_value(
            state
                .jobs
                .output_for_caller(parse::<JobOutputRequest>(args)?, caller_key)?,
        )?),
        "job_wait" => Ok(serde_json::to_value(
            state
                .jobs
                .wait_for_caller(parse::<JobWaitRequest>(args)?, caller_key)?,
        )?),
        "job_cancel" => Ok(serde_json::to_value(
            state
                .jobs
                .cancel_for_caller(parse::<JobCancelRequest>(args)?, caller_key)?,
        )?),
        "file_read" => Ok(serde_json::to_value(fs::read(
            &state,
            parse::<FileReadRequest>(args)?,
        )?)?),
        "file_backup" => Ok(serde_json::to_value(file_backup::create(
            &state,
            parse::<FileBackupRequest>(args)?,
        )?)?),
        "file_backup_list" => Ok(serde_json::to_value(file_backup::list(
            &state,
            parse::<FileBackupListRequest>(args)?,
        )?)?),
        "file_restore" => Ok(serde_json::to_value(file_backup::restore(
            &state,
            parse::<FileRestoreRequest>(args)?,
        )?)?),
        "file_backup_delete" => Ok(serde_json::to_value(file_backup::delete(
            &state,
            parse::<FileBackupDeleteRequest>(args)?,
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
        "file_delete" => Ok(serde_json::to_value(fs::delete(
            &state,
            parse::<FileDeleteRequest>(args)?,
        )?)?),
        "file_import" => Ok(serde_json::to_value(file_bridge::import(
            &state,
            parse::<FileImportRequest>(args)?,
        )?)?),
        "file_export" => Ok(serde_json::to_value(file_bridge::export(
            &state,
            parse::<FileExportRequest>(args)?,
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

#[derive(Debug, Deserialize)]
struct McpServerRequest {
    server: String,
}

#[derive(Debug, Deserialize)]
struct McpToolCallRequest {
    server: String,
    tool: String,
    #[serde(default)]
    arguments: Option<Value>,
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
        "ssh_master_ids": state.ssh_sessions.master_ids(),
        "terminal_ids": state.terminals.ids(),
        "job_ids": state.jobs.ids(),
        "runtime_dir": state.config.server.runtime_dir.display().to_string(),
        "started_at_debug": format!("{:?}", state.started_at()),
        "notes": [
            "SSH exec, jobs, terminals, and persistent file workers share a per-target OpenSSH ControlMaster when enabled.",
            "exec and exec_start share the same command-session implementation.",
            "The active target is process-scoped; terminal and job sessions remain bound to their original targets."
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
