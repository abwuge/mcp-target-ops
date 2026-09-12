use crate::{
    core::{
        config::TargetConfig,
        error::{Error, Result},
        policy,
        secret::SecretRef,
        state::AppState,
        target::{ResolvedTarget, TargetId},
        util::truncate_bytes,
    },
    tooling::secret,
    transport::ssh,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::Read,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

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
    let (target, source) = state.resolve_target(req.target.as_deref())?;
    let config = state.get_target_config(&target)?;
    policy::check_exec(&target, config)?;

    let policy = policy::target_policy(config);
    let timeout_ms = req.timeout_ms.unwrap_or(policy.default_timeout_ms);
    let timeout = Duration::from_millis(timeout_ms);
    let max_output = Some(
        req.max_output_bytes
            .unwrap_or(policy.max_output_bytes)
            .min(policy.max_output_bytes),
    );
    let secret_env = secret::resolve_env(state, &target, config, source, &req.secret_env, timeout)?;

    let raw = match (target.clone(), config) {
        (TargetId::Local, TargetConfig::Local(_)) => {
            run_local_shell_with_env(&req.command, req.cwd.as_deref(), &secret_env, timeout)?
        }
        (TargetId::Ssh(name), TargetConfig::Ssh(ssh_config)) => ssh::exec_with_env(
            &state.ssh_sessions,
            &name,
            ssh_config,
            &req.command,
            req.cwd.as_deref(),
            &secret_env,
            timeout,
        )?,
        _ => {
            return Err(Error::Target(format!(
                "target {target} has mismatched config"
            )))
        }
    };

    let (stdout, stdout_truncated) = truncate_bytes(raw.stdout, max_output);
    let (stderr, stderr_truncated) = truncate_bytes(raw.stderr, max_output);

    Ok(ExecResponse {
        command: req.command,
        resolved_target: state.resolved_target_value(target, source),
        exit_code: raw.exit_code,
        stdout: String::from_utf8_lossy(&stdout).to_string(),
        stderr: String::from_utf8_lossy(&stderr).to_string(),
        stdout_truncated,
        stderr_truncated,
        timed_out: raw.timed_out,
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

pub fn run_local_shell_with_env(
    command: &str,
    cwd: Option<&str>,
    env: &BTreeMap<String, String>,
    timeout: Duration,
) -> Result<RawExecOutput> {
    run_command_collect(local_shell_command(command, cwd, env), timeout)
}

pub(crate) fn local_shell_command(
    command: &str,
    cwd: Option<&str>,
    env: &BTreeMap<String, String>,
) -> Command {
    #[cfg(windows)]
    let mut cmd = {
        let mut cmd = Command::new("cmd.exe");
        cmd.arg("/C").arg(command);
        cmd
    };

    #[cfg(not(windows))]
    let mut cmd = {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string());
        let mut cmd = Command::new(shell);
        cmd.arg("-lc").arg(command);
        cmd
    };

    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    cmd.envs(env);
    cmd
}

pub(crate) fn configure_command_process_group(cmd: &mut Command) {
    #[cfg(unix)]
    {
        cmd.process_group(0);
    }
}

pub(crate) fn terminate_child_process_group(child: &mut Child) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let pgid = libc::pid_t::try_from(child.id()).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "child pid does not fit pid_t",
            )
        })?;
        // SAFETY: commands are placed in a fresh process group whose id is the child pid.
        let result = unsafe { libc::kill(-pgid, libc::SIGKILL) };
        if result == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        if child.kill().is_ok() {
            return Ok(());
        }
        Err(error)
    }

    #[cfg(not(unix))]
    {
        child.kill()
    }
}

pub fn run_command_collect(mut cmd: Command, timeout: Duration) -> Result<RawExecOutput> {
    configure_command_process_group(&mut cmd);
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::Tool("failed to open command stdout".to_string()))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| Error::Tool("failed to open command stderr".to_string()))?;

    let stdout_thread = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let stderr_thread = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });

    let started = Instant::now();
    let mut timed_out = false;
    let exit_code = loop {
        if let Some(status) = child.try_wait()? {
            break status.code();
        }

        if started.elapsed() >= timeout {
            timed_out = true;
            let _ = terminate_child_process_group(&mut child);
            let status = child.wait()?;
            break status.code();
        }

        thread::sleep(Duration::from_millis(20));
    };

    let stdout = stdout_thread
        .join()
        .map_err(|_| Error::Tool("stdout reader panicked".to_string()))?;
    let stderr = stderr_thread
        .join()
        .map_err(|_| Error::Tool("stderr reader panicked".to_string()))?;

    Ok(RawExecOutput {
        exit_code,
        stdout,
        stderr,
        timed_out,
    })
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

    #[test]
    fn injects_secret_environment_without_echoing_it() {
        let mut env = BTreeMap::new();
        env.insert("MCP_TEST_SECRET".to_string(), "hidden-value".to_string());
        let output = run_local_shell_with_env(
            "printf '%s' \"$MCP_TEST_SECRET\"",
            None,
            &env,
            Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!(output.stdout, b"hidden-value");
    }

    #[cfg(unix)]
    #[test]
    fn timeout_kills_external_child_processes_promptly() {
        let started = Instant::now();
        let output =
            run_local_shell_with_env("sleep 1", None, &BTreeMap::new(), Duration::from_millis(50))
                .unwrap();

        assert!(output.timed_out);
        assert!(
            started.elapsed() < Duration::from_millis(700),
            "timeout waited for an inherited pipe holder: {:?}",
            started.elapsed()
        );
    }
}
