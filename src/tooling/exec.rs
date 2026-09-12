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
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

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

pub fn run_command_collect(mut cmd: Command, timeout: Duration) -> Result<RawExecOutput> {
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

        if started.elapsed() > timeout {
            timed_out = true;
            let _ = child.kill();
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
}
