use crate::{
    core::{
        error::{Error, Result},
        policy,
        secret::SecretRef,
        state::AppState,
        target::ResolvedTarget,
    },
    tooling::job::ExecStartRequest,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, thread, time::Instant};

const MAX_BATCH_COMMANDS: usize = 32;

#[derive(Debug, Clone, Deserialize)]
pub struct ExecRequest {
    #[serde(default)]
    pub target: Option<String>,
    pub command: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub max_output_bytes: Option<usize>,
    #[serde(default)]
    pub secret_env: BTreeMap<String, SecretRef>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecResponse {
    pub command: String,
    pub resolved_target: ResolvedTarget,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub stdout: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub stderr: String,
    #[serde(skip_serializing_if = "is_false")]
    pub stdout_truncated: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub stderr_truncated: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub timed_out: bool,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecBatchMode {
    #[default]
    Sequential,
    Parallel,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExecBatchCommand {
    pub command: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub max_output_bytes: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExecBatchRequest {
    #[serde(default)]
    pub target: Option<String>,
    pub commands: Vec<ExecBatchCommand>,
    #[serde(default)]
    pub mode: ExecBatchMode,
    #[serde(default)]
    pub stop_on_error: bool,
    #[serde(default)]
    pub secret_env: BTreeMap<String, SecretRef>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecBatchItemResponse {
    pub index: usize,
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub stdout: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub stderr: String,
    #[serde(skip_serializing_if = "is_false")]
    pub stdout_truncated: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub stderr_truncated: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub timed_out: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub elapsed_ms: u64,
    pub success: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecBatchResponse {
    pub resolved_target: ResolvedTarget,
    pub mode: ExecBatchMode,
    pub requested_count: usize,
    pub executed_count: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub timed_out: usize,
    pub stopped_early: bool,
    pub results: Vec<ExecBatchItemResponse>,
}

#[derive(Debug, Clone)]
pub struct RawExecOutput {
    pub exit_code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
}

pub fn run(state: &AppState, req: ExecRequest) -> Result<ExecResponse> {
    run_with_request_id(state, req, None)
}

pub fn run_with_request_id(
    state: &AppState,
    req: ExecRequest,
    request_id: Option<&Value>,
) -> Result<ExecResponse> {
    let command = req.command.clone();
    let output = state.jobs.run_foreground(
        state,
        ExecStartRequest {
            target: req.target,
            command: req.command,
            cwd: req.cwd,
            timeout_ms: req.timeout_ms,
            max_output_bytes: req.max_output_bytes,
            secret_env: req.secret_env,
        },
        request_id,
    )?;

    Ok(ExecResponse {
        command,
        resolved_target: output.resolved_target,
        exit_code: output.exit_code,
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        stdout_truncated: output.stdout_truncated,
        stderr_truncated: output.stderr_truncated,
        timed_out: output.timed_out,
    })
}

pub fn run_batch(state: &AppState, req: ExecBatchRequest) -> Result<ExecBatchResponse> {
    if req.commands.is_empty() {
        return Err(Error::Tool(
            "exec_batch requires at least one command".to_string(),
        ));
    }
    if req.commands.len() > MAX_BATCH_COMMANDS {
        return Err(Error::Tool(format!(
            "exec_batch accepts at most {MAX_BATCH_COMMANDS} commands"
        )));
    }
    if matches!(req.mode, ExecBatchMode::Parallel) && req.stop_on_error {
        return Err(Error::Tool(
            "stop_on_error is only supported for sequential exec_batch mode".to_string(),
        ));
    }

    let (target, source) = state.resolve_target(req.target.as_deref())?;
    let config = state.get_target_config(&target)?;
    policy::check_exec(&target, config)?;
    let resolved_target = state.resolved_target_value(target.clone(), source);
    let target_name = target.to_string();
    let requested_count = req.commands.len();

    let results = match req.mode {
        ExecBatchMode::Sequential => {
            let mut results = Vec::with_capacity(requested_count);
            for (index, command) in req.commands.into_iter().enumerate() {
                let item = run_batch_item(state, &target_name, &req.secret_env, index, command);
                let should_stop = req.stop_on_error && !item.success;
                results.push(item);
                if should_stop {
                    break;
                }
            }
            results
        }
        ExecBatchMode::Parallel => {
            thread::scope(|scope| -> Result<Vec<ExecBatchItemResponse>> {
                let mut handles = Vec::with_capacity(requested_count);
                for (index, command) in req.commands.into_iter().enumerate() {
                    let target_name = target_name.clone();
                    let secret_env = req.secret_env.clone();
                    handles.push(scope.spawn(move || {
                        run_batch_item(state, &target_name, &secret_env, index, command)
                    }));
                }

                let mut results = Vec::with_capacity(requested_count);
                for handle in handles {
                    results.push(handle.join().map_err(|_| {
                        Error::Tool("exec_batch worker thread panicked".to_string())
                    })?);
                }
                Ok(results)
            })?
        }
    };

    let executed_count = results.len();
    let succeeded = results.iter().filter(|result| result.success).count();
    let timed_out = results.iter().filter(|result| result.timed_out).count();

    Ok(ExecBatchResponse {
        resolved_target,
        mode: req.mode,
        requested_count,
        executed_count,
        succeeded,
        failed: executed_count.saturating_sub(succeeded),
        timed_out,
        stopped_early: executed_count < requested_count,
        results,
    })
}

fn run_batch_item(
    state: &AppState,
    target: &str,
    secret_env: &BTreeMap<String, SecretRef>,
    index: usize,
    command: ExecBatchCommand,
) -> ExecBatchItemResponse {
    let started = Instant::now();
    let request = ExecRequest {
        target: Some(target.to_string()),
        command: command.command.clone(),
        cwd: command.cwd.clone(),
        timeout_ms: command.timeout_ms,
        max_output_bytes: command.max_output_bytes,
        secret_env: secret_env.clone(),
    };

    match run(state, request) {
        Ok(response) => {
            let success = response.exit_code == Some(0) && !response.timed_out;
            ExecBatchItemResponse {
                index,
                command: response.command,
                cwd: command.cwd,
                exit_code: response.exit_code,
                stdout: response.stdout,
                stderr: response.stderr,
                stdout_truncated: response.stdout_truncated,
                stderr_truncated: response.stderr_truncated,
                timed_out: response.timed_out,
                error: None,
                elapsed_ms: elapsed_ms(started),
                success,
            }
        }
        Err(error) => ExecBatchItemResponse {
            index,
            command: command.command,
            cwd: command.cwd,
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            timed_out: false,
            error: Some(error.to_string()),
            elapsed_ms: elapsed_ms(started),
            success: false,
        },
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::target::{TargetId, TargetSource};
    use serde_json::json;

    #[test]
    fn omits_empty_and_inactive_exec_fields() {
        let response = ExecResponse {
            command: "true".to_string(),
            resolved_target: ResolvedTarget::new(TargetId::Local, TargetSource::Explicit),
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            timed_out: false,
        };

        assert_eq!(
            serde_json::to_value(response).unwrap(),
            json!({
                "command": "true",
                "resolved_target": { "target": "local", "source": "explicit" },
                "exit_code": 0
            })
        );
    }
}
