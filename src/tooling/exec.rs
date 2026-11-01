use crate::{
    core::{error::Result, secret::SecretRef, state::AppState, target::ResolvedTarget},
    tooling::job::ExecStartRequest,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
    pub resolved_target: ResolvedTarget,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<&'static str>,
    #[serde(skip_serializing_if = "is_false")]
    pub auto_backgrounded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_action: Option<&'static str>,
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

/// Accept concise command strings for the common case while retaining the
/// existing detailed per-command object syntax for cwd/timeouts/output limits.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ExecBatchCommand {
    Shell(String),
    Detailed(ExecBatchCommandDetails),
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExecBatchCommandDetails {
    pub command: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub max_output_bytes: Option<usize>,
}

impl ExecBatchCommand {
    pub(crate) fn into_details(self) -> ExecBatchCommandDetails {
        match self {
            Self::Shell(command) => ExecBatchCommandDetails {
                command,
                cwd: None,
                timeout_ms: None,
                max_output_bytes: None,
            },
            Self::Detailed(details) => details,
        }
    }
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

#[derive(Debug, Clone)]
pub struct RawExecOutput {
    pub exit_code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
}

pub(crate) fn run_to_completion(state: &AppState, req: ExecRequest) -> Result<ExecResponse> {
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
    )?;
    Ok(ExecResponse {
        resolved_target: output.resolved_target,
        job_id: None,
        status: None,
        auto_backgrounded: false,
        next_action: None,
        exit_code: output.exit_code,
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        stdout_truncated: output.stdout_truncated,
        stderr_truncated: output.stderr_truncated,
        timed_out: output.timed_out,
    })
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
            resolved_target: ResolvedTarget::new(TargetId::Local, TargetSource::Explicit),
            job_id: None,
            status: None,
            auto_backgrounded: false,
            next_action: None,
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
                "resolved_target": { "target": "local", "source": "explicit" },
                "exit_code": 0
            })
        );
    }
}
