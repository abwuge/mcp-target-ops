use crate::{
    core::{
        error::{Error, Result},
        secret::SecretRef,
        state::AppState,
    },
    tooling::{
        exec::{ExecBatchCommand, ExecBatchMode, ExecBatchRequest},
        job::{
            AdaptiveExecOutput, ExecStartRequest, JobCancelRequest, JobOutputRequest,
            JobPollRequest, JobWaitRequest,
        },
    },
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

#[derive(Debug, Deserialize)]
pub struct ExecInput {
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    commands: Option<Vec<ExecBatchCommand>>,
    #[serde(default)]
    mode: ExecBatchMode,
    #[serde(default)]
    stop_on_error: bool,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    max_output_bytes: Option<usize>,
    #[serde(default)]
    yield_time_ms: Option<u64>,
    #[serde(default)]
    secret_env: BTreeMap<String, SecretRef>,
}

pub fn execute(state: Arc<AppState>, args: Value, caller: &str) -> Result<Value> {
    let req: ExecInput = serde_json::from_value(args)?;
    if req.command.is_some() == req.commands.is_some() {
        return Err(Error::Tool(
            "provide exactly one of command or commands".into(),
        ));
    }
    let yield_ms = req
        .yield_time_ms
        .unwrap_or(state.config.runtime.exec_auto_background_after_ms)
        .min(120_000);

    if let Some(command) = req.command {
        if command.trim().is_empty() {
            return Err(Error::Tool("command must not be empty".into()));
        }
        let request = ExecStartRequest {
            target: req.target,
            command,
            cwd: req.cwd,
            timeout_ms: req.timeout_ms,
            max_output_bytes: req.max_output_bytes,
            secret_env: req.secret_env,
        };
        let outcome = state.jobs.run_adaptive_with_threshold(
            &state,
            request,
            caller,
            Duration::from_millis(yield_ms),
        )?;
        return match outcome {
            AdaptiveExecOutput::Completed(o) => {
                let mut value = json!({
                    "resolved_target":o.resolved_target, "exit_code":o.exit_code,
                    "timed_out":o.timed_out, "stdout_truncated":o.stdout_truncated,
                    "stderr_truncated":o.stderr_truncated,
                });
                let out = String::from_utf8_lossy(&o.stdout).to_string();
                let err = String::from_utf8_lossy(&o.stderr).to_string();
                if !out.is_empty() {
                    value["stdout"] = json!(out);
                }
                if !err.is_empty() {
                    value["stderr"] = json!(err);
                }
                Ok(value)
            }
            AdaptiveExecOutput::Backgrounded {
                resolved_target,
                job_id,
            } => Ok(json!({"resolved_target":resolved_target, "job_id":job_id,
                    "status":"running", "auto_backgrounded":true})),
        };
    }
    let commands = req.commands.unwrap_or_default();
    if commands.iter().any(|cmd| match cmd {
        ExecBatchCommand::Shell(s) => s.trim().is_empty(),
        ExecBatchCommand::Detailed(s) => s.command.trim().is_empty(),
    }) {
        return Err(Error::Tool("batch commands may not be empty".into()));
    }
    let request = ExecBatchRequest {
        target: req.target,
        commands,
        mode: req.mode,
        stop_on_error: req.stop_on_error,
        secret_env: req.secret_env,
    };
    let (id, group) = state.batches.launch(
        Arc::clone(&state),
        request,
        caller,
        req.timeout_ms,
        req.max_output_bytes,
    )?;
    if yield_ms > 0 {
        group.wait(Duration::from_millis(yield_ms));
    }
    let mut response = group.report(
        &id,
        true,
        req.max_output_bytes
            .unwrap_or(64 * 1024)
            .clamp(1, 128 * 1024),
    );
    if response["status"] == "running" {
        response["auto_backgrounded"] = json!(true);
    }
    Ok(response)
}

#[derive(Debug, Deserialize)]
struct JobInput {
    job_id: String,
    #[serde(default = "default_action")]
    action: String,
    #[serde(default)]
    wait_ms: Option<u64>,
    #[serde(default)]
    stdout_since_seq: Option<u64>,
    #[serde(default)]
    stderr_since_seq: Option<u64>,
    #[serde(default)]
    max_bytes: Option<usize>,
}

fn default_action() -> String {
    "read".to_string()
}

pub fn job(state: Arc<AppState>, args: Value, caller: &str) -> Result<Value> {
    let req: JobInput = serde_json::from_value(args)?;
    if let Some(group) = state.batches.get(&req.job_id, caller)? {
        match req.action.as_str() {
            "status" => {}
            "read" => {}
            "wait" => {
                group.wait(Duration::from_millis(
                    req.wait_ms
                        .unwrap_or(state.config.runtime.job_wait_default_timeout_ms)
                        .min(state.config.runtime.job_wait_max_timeout_ms),
                ));
            }
            "cancel" => group.cancel(),
            _ => {
                return Err(Error::Tool(
                    "action must be status, read, wait, or cancel".into(),
                ))
            }
        }
        return Ok(group.report(
            &req.job_id,
            req.action != "status",
            req.max_bytes
                .unwrap_or(state.config.runtime.stream_default_max_bytes)
                .clamp(1, state.config.runtime.stream_max_bytes),
        ));
    }

    match req.action.as_str() {
        "status" => Ok(serde_json::to_value(
            state
                .jobs
                .poll_for_caller(JobPollRequest { job_id: req.job_id }, caller)?,
        )?),
        "read" => Ok(serde_json::to_value(state.jobs.output_for_caller(
            JobOutputRequest {
                job_id: req.job_id,
                stdout_since_seq: req.stdout_since_seq,
                stderr_since_seq: req.stderr_since_seq,
                max_bytes: req.max_bytes,
            },
            caller,
        )?)?),
        "wait" => Ok(serde_json::to_value(state.jobs.wait_for_caller(
            JobWaitRequest {
                job_id: req.job_id,
                wait_timeout_ms: req.wait_ms,
                stdout_since_seq: req.stdout_since_seq,
                stderr_since_seq: req.stderr_since_seq,
                max_bytes: req.max_bytes,
            },
            caller,
        )?)?),
        "cancel" => Ok(serde_json::to_value(
            state
                .jobs
                .cancel_for_caller(JobCancelRequest { job_id: req.job_id }, caller)?,
        )?),
        _ => Err(Error::Tool(
            "action must be status, read, wait, or cancel".into(),
        )),
    }
}
