use crate::{
    core::{
        config::TargetConfig,
        error::{Error, Result},
        policy,
        secret::SecretRef,
        state::AppState,
        target::{ResolvedTarget, TargetId},
    },
    tooling::{command_session::CommandSession, secret},
    transport::ssh,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    process::Command,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

static JOB_COUNTER: AtomicU64 = AtomicU64::new(1);
const MAX_RETAINED_JOBS: usize = 128;

#[derive(Debug, Clone, Deserialize)]
pub struct ExecStartRequest {
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
pub struct ExecStartResponse {
    pub resolved_target: ResolvedTarget,
    pub job_id: String,
    pub status: &'static str,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JobPollRequest {
    pub job_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct JobPollResponse {
    pub job_id: String,
    pub target: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub elapsed_ms: u64,
    pub timed_out: bool,
    pub cancel_requested: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JobOutputRequest {
    pub job_id: String,
    #[serde(default)]
    pub stdout_since_seq: Option<u64>,
    #[serde(default)]
    pub stderr_since_seq: Option<u64>,
    #[serde(default)]
    pub max_bytes: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct JobOutputResponse {
    pub job_id: String,
    pub target: String,
    pub stdout_from_seq: u64,
    pub stdout_next_seq: u64,
    pub stdout: String,
    pub stdout_truncated: bool,
    pub stderr_from_seq: u64,
    pub stderr_next_seq: u64,
    pub stderr: String,
    pub stderr_truncated: bool,
    pub eof: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JobCancelRequest {
    pub job_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct JobCancelResponse {
    pub job_id: String,
    pub cancel_requested: bool,
    pub status: String,
}

#[derive(Debug)]
pub struct ForegroundOutput {
    pub resolved_target: ResolvedTarget,
    pub exit_code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub timed_out: bool,
}

pub struct JobRegistry {
    sessions: Mutex<HashMap<String, Arc<CommandSession>>>,
}

impl JobRegistry {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }

    pub fn run_foreground(
        &self,
        state: &AppState,
        req: ExecStartRequest,
    ) -> Result<ForegroundOutput> {
        let (resolved_target, session) = self.create_session(state, &req)?;
        let snapshot = session.wait();
        Ok(ForegroundOutput {
            resolved_target,
            exit_code: snapshot.exit_code,
            stdout: snapshot.stdout,
            stderr: snapshot.stderr,
            stdout_truncated: snapshot.stdout_truncated,
            stderr_truncated: snapshot.stderr_truncated,
            timed_out: snapshot.timed_out,
        })
    }

    pub fn start(&self, state: &AppState, req: ExecStartRequest) -> Result<ExecStartResponse> {
        let (resolved_target, session) = self.create_session(state, &req)?;
        let id = format!("job_{}", JOB_COUNTER.fetch_add(1, Ordering::Relaxed));
        self.insert(id.clone(), session);
        Ok(ExecStartResponse {
            resolved_target,
            job_id: id,
            status: "running",
        })
    }

    fn create_session(
        &self,
        state: &AppState,
        req: &ExecStartRequest,
    ) -> Result<(ResolvedTarget, Arc<CommandSession>)> {
        let (target, source) = state.resolve_target(req.target.as_deref())?;
        let config = state.get_target_config(&target)?;
        policy::check_exec(&target, config)?;
        let target_policy = policy::target_policy(config);
        let secret_timeout = Duration::from_millis(target_policy.default_timeout_ms);
        let secret_env = secret::resolve_env(
            state,
            &target,
            config,
            source,
            &req.secret_env,
            secret_timeout,
        )?;
        let max_output = req
            .max_output_bytes
            .unwrap_or(target_policy.max_output_bytes)
            .min(target_policy.max_output_bytes)
            .max(1);
        let timeout = req.timeout_ms.map(Duration::from_millis);
        let command = command_for_target(
            &target,
            config,
            &req.command,
            req.cwd.as_deref(),
            &secret_env,
        )?;
        let session = CommandSession::spawn(command, target.clone(), max_output, timeout)?;
        Ok((state.resolved_target_value(target, source), session))
    }

    pub fn poll(&self, req: JobPollRequest) -> Result<JobPollResponse> {
        let session = self.get(&req.job_id)?;
        let snapshot = session.snapshot();
        Ok(JobPollResponse {
            job_id: req.job_id,
            target: session.target.to_string(),
            status: session.state().to_string(),
            exit_code: snapshot.exit_code,
            elapsed_ms: snapshot.elapsed_ms,
            timed_out: snapshot.timed_out,
            cancel_requested: snapshot.cancelled,
        })
    }

    pub fn output(&self, req: JobOutputRequest) -> Result<JobOutputResponse> {
        let session = self.get(&req.job_id)?;
        let stdout_from_seq = req.stdout_since_seq.unwrap_or(0);
        let stderr_from_seq = req.stderr_since_seq.unwrap_or(0);
        let delta = session.output_since(
            stdout_from_seq,
            stderr_from_seq,
            req.max_bytes.unwrap_or(64 * 1024).clamp(1, 512 * 1024),
        );
        Ok(JobOutputResponse {
            job_id: req.job_id,
            target: session.target.to_string(),
            stdout_from_seq: delta.stdout_from_seq,
            stdout_next_seq: delta.stdout_next_seq,
            stdout: String::from_utf8_lossy(&delta.stdout).to_string(),
            stdout_truncated: delta.stdout_truncated,
            stderr_from_seq: delta.stderr_from_seq,
            stderr_next_seq: delta.stderr_next_seq,
            stderr: String::from_utf8_lossy(&delta.stderr).to_string(),
            stderr_truncated: delta.stderr_truncated,
            eof: delta.eof,
        })
    }

    pub fn cancel(&self, req: JobCancelRequest) -> Result<JobCancelResponse> {
        let session = self.get(&req.job_id)?;
        let requested = session.cancel();
        Ok(JobCancelResponse {
            job_id: req.job_id,
            cancel_requested: requested || session.snapshot().cancelled,
            status: session.state().to_string(),
        })
    }

    pub fn ids(&self) -> Vec<String> {
        self.sessions.lock().unwrap().keys().cloned().collect()
    }

    fn get(&self, id: &str) -> Result<Arc<CommandSession>> {
        self.sessions
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| Error::Tool(format!("job {id} not found")))
    }

    fn insert(&self, id: String, session: Arc<CommandSession>) {
        let mut sessions = self.sessions.lock().unwrap();
        if sessions.len() >= MAX_RETAINED_JOBS {
            sessions.retain(|_, session| !session.snapshot().finished);
        }
        if sessions.len() >= MAX_RETAINED_JOBS {
            if let Some(id) = sessions
                .iter()
                .find_map(|(id, session)| session.snapshot().finished.then(|| id.clone()))
            {
                sessions.remove(&id);
            }
        }
        sessions.insert(id, session);
    }
}

fn command_for_target(
    target: &TargetId,
    config: &TargetConfig,
    command: &str,
    cwd: Option<&str>,
    env: &BTreeMap<String, String>,
) -> Result<Command> {
    match (target, config) {
        (TargetId::Local, TargetConfig::Local(_)) => Ok(local_shell_command(command, cwd, env)),
        (TargetId::Ssh(_), TargetConfig::Ssh(ssh_config)) => {
            let (program, args) = ssh::exec_program_and_args(ssh_config, command, cwd, env);
            let mut process = Command::new(program);
            process.args(args);
            Ok(process)
        }
        _ => Err(Error::Target(format!(
            "target {target} has mismatched config"
        ))),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{core::config::Config, core::state::AppState};
    use tempfile::tempdir;

    fn test_state() -> AppState {
        let temp = tempdir().unwrap();
        let path = temp.keep();
        let mut config = Config::default();
        if let Some(TargetConfig::Local(local)) = config.targets.get_mut("local") {
            local.enabled = true;
            local.policy.allow_exec = true;
        }
        config.server.default_target = Some("local".to_string());
        config.server.oauth_state_file = None;
        config.server.runtime_dir = path.join("runtime");
        AppState::new(config).unwrap()
    }

    #[test]
    fn foreground_and_background_share_command_session_behavior() {
        let state = test_state();
        let request = ExecStartRequest {
            target: Some("local".into()),
            command: "printf one; sleep 0.03; printf two".into(),
            cwd: None,
            timeout_ms: Some(1000),
            max_output_bytes: None,
            secret_env: BTreeMap::new(),
        };
        let foreground = state.jobs.run_foreground(&state, request.clone()).unwrap();
        assert_eq!(foreground.stdout, b"onetwo");
        let started = state.jobs.start(&state, request).unwrap();
        while state
            .jobs
            .poll(JobPollRequest {
                job_id: started.job_id.clone(),
            })
            .unwrap()
            .status
            == "running"
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = state
            .jobs
            .output(JobOutputRequest {
                job_id: started.job_id,
                stdout_since_seq: None,
                stderr_since_seq: None,
                max_bytes: None,
            })
            .unwrap();
        assert_eq!(output.stdout, "onetwo");
        assert!(output.eof);
    }
}
