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
use serde_json::Value;
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
static EXEC_COUNTER: AtomicU64 = AtomicU64::new(1);
const MAX_RETAINED_JOBS: usize = 128;
const MAX_RETAINED_FOREGROUND: usize = 64;

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
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub elapsed_ms: u64,
    pub timed_out: bool,
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

#[derive(Debug, Clone, Deserialize)]
pub struct ExecStreamRequest {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub request_id: Option<Value>,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub stdout_since_seq: Option<u64>,
    #[serde(default)]
    pub stderr_since_seq: Option<u64>,
    #[serde(default)]
    pub max_bytes: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecStreamResponse {
    pub attached: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    pub stdout_from_seq: u64,
    pub stdout_next_seq: u64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub stdout: String,
    pub stdout_truncated: bool,
    pub stderr_from_seq: u64,
    pub stderr_next_seq: u64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub stderr: String,
    pub stderr_truncated: bool,
    pub eof: bool,
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

struct ForegroundSession {
    session: Arc<CommandSession>,
    request_key: Option<String>,
    requested_target: Option<String>,
    command: String,
    cwd: Option<String>,
    order: u64,
    claimed: bool,
}

pub struct JobRegistry {
    sessions: Mutex<HashMap<String, Arc<CommandSession>>>,
    foreground: Mutex<HashMap<String, ForegroundSession>>,
}

impl JobRegistry {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            foreground: Mutex::new(HashMap::new()),
        }
    }

    pub fn run_foreground(
        &self,
        state: &AppState,
        req: ExecStartRequest,
        request_id: Option<&Value>,
    ) -> Result<ForegroundOutput> {
        let (resolved_target, session) = self.create_session(state, &req, true)?;
        let order = EXEC_COUNTER.fetch_add(1, Ordering::Relaxed);
        let id = format!("exec_{order}");
        self.insert_foreground(
            id,
            ForegroundSession {
                session: Arc::clone(&session),
                request_key: request_id.map(request_key).transpose()?,
                requested_target: req.target.clone(),
                command: req.command.clone(),
                cwd: req.cwd.clone(),
                order,
                claimed: false,
            },
        );
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
        let (resolved_target, session) = self.create_session(state, &req, false)?;
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
        use_default_timeout: bool,
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
        let timeout = req
            .timeout_ms
            .or(use_default_timeout.then_some(target_policy.default_timeout_ms))
            .map(Duration::from_millis);
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
        let status = session.status_snapshot();
        Ok(JobPollResponse {
            job_id: req.job_id,
            target: session.target.to_string(),
            status: session.state().to_string(),
            exit_code: status.exit_code,
            elapsed_ms: status.elapsed_ms,
            timed_out: status.timed_out,
            cancel_requested: status.cancelled,
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
        let status = session.status_snapshot();
        Ok(JobOutputResponse {
            job_id: req.job_id,
            target: session.target.to_string(),
            status: session.state().to_string(),
            exit_code: status.exit_code,
            elapsed_ms: status.elapsed_ms,
            timed_out: status.timed_out,
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
            cancel_requested: requested || session.status_snapshot().cancelled,
            status: session.state().to_string(),
        })
    }

    pub fn stream(&self, req: ExecStreamRequest) -> Result<ExecStreamResponse> {
        let stdout_from_seq = req.stdout_since_seq.unwrap_or(0);
        let stderr_from_seq = req.stderr_since_seq.unwrap_or(0);
        let max_bytes = req.max_bytes.unwrap_or(64 * 1024).clamp(1, 512 * 1024);
        let mut foreground = self.foreground.lock().unwrap();
        let session_id = if let Some(session_id) = req.session_id.as_deref() {
            foreground
                .contains_key(session_id)
                .then(|| session_id.to_string())
        } else if let Some(request_id) = req.request_id.as_ref() {
            let key = request_key(request_id)?;
            foreground
                .iter()
                .filter(|(_, entry)| entry.request_key.as_deref() == Some(key.as_str()))
                .max_by_key(|(_, entry)| entry.order)
                .map(|(id, _)| id.clone())
        } else {
            let command = req.command.as_deref().ok_or_else(|| {
                Error::Tool("exec_stream requires session_id, request_id, or command".to_string())
            })?;
            let id = foreground
                .iter()
                .filter(|(_, entry)| {
                    !entry.claimed
                        && entry.command == command
                        && entry.cwd.as_deref() == req.cwd.as_deref()
                        && entry.requested_target.as_deref() == req.target.as_deref()
                })
                .max_by_key(|(_, entry)| entry.order)
                .map(|(id, _)| id.clone());
            if let Some(id) = id.as_deref() {
                if let Some(entry) = foreground.get_mut(id) {
                    entry.claimed = true;
                }
            }
            id
        };

        let Some(session_id) = session_id else {
            return Ok(ExecStreamResponse {
                attached: false,
                session_id: None,
                target: None,
                status: None,
                exit_code: None,
                elapsed_ms: None,
                stdout_from_seq,
                stdout_next_seq: stdout_from_seq,
                stdout: String::new(),
                stdout_truncated: false,
                stderr_from_seq,
                stderr_next_seq: stderr_from_seq,
                stderr: String::new(),
                stderr_truncated: false,
                eof: false,
            });
        };

        let entry = foreground
            .get(&session_id)
            .expect("matched foreground session remains registered");
        let delta = entry
            .session
            .output_since(stdout_from_seq, stderr_from_seq, max_bytes);
        let status = entry.session.status_snapshot();
        Ok(ExecStreamResponse {
            attached: true,
            session_id: Some(session_id),
            target: Some(entry.session.target.to_string()),
            status: Some(entry.session.state().to_string()),
            exit_code: status.exit_code,
            elapsed_ms: Some(status.elapsed_ms),
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
            sessions.retain(|_, session| !session.status_snapshot().finished);
        }
        if sessions.len() >= MAX_RETAINED_JOBS {
            if let Some(id) = sessions
                .iter()
                .find_map(|(id, session)| session.status_snapshot().finished.then(|| id.clone()))
            {
                sessions.remove(&id);
            }
        }
        sessions.insert(id, session);
    }

    fn insert_foreground(&self, id: String, entry: ForegroundSession) {
        let mut foreground = self.foreground.lock().unwrap();
        if foreground.len() >= MAX_RETAINED_FOREGROUND {
            foreground.retain(|_, entry| !entry.session.status_snapshot().finished);
        }
        if foreground.len() >= MAX_RETAINED_FOREGROUND {
            if let Some(oldest) = foreground
                .iter()
                .filter(|(_, entry)| entry.session.status_snapshot().finished)
                .min_by_key(|(_, entry)| entry.order)
                .map(|(id, _)| id.clone())
            {
                foreground.remove(&oldest);
            }
        }
        foreground.insert(id, entry);
    }
}

fn request_key(request_id: &Value) -> Result<String> {
    match request_id {
        Value::String(value) => Ok(format!("string:{value}")),
        Value::Number(value) => Ok(format!("number:{value}")),
        _ => Err(Error::Tool(
            "MCP tool request id must be a string or number".to_string(),
        )),
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
    fn foreground_stream_can_attach_and_read_incremental_output() {
        let state = Arc::new(test_state());
        let command = "printf first; sleep 0.12; printf second".to_string();
        let request = ExecStartRequest {
            target: Some("local".into()),
            command: command.clone(),
            cwd: None,
            timeout_ms: Some(1000),
            max_output_bytes: None,
            secret_env: BTreeMap::new(),
        };
        let worker_state = Arc::clone(&state);
        let worker = std::thread::spawn(move || {
            worker_state
                .jobs
                .run_foreground(
                    &worker_state,
                    request,
                    Some(&Value::String("stream-test".into())),
                )
                .unwrap()
        });

        let mut attached = None;
        for _ in 0..30 {
            let response = state
                .jobs
                .stream(ExecStreamRequest {
                    session_id: None,
                    request_id: Some(Value::String("stream-test".into())),
                    target: Some("local".into()),
                    command: Some(command.clone()),
                    cwd: None,
                    stdout_since_seq: None,
                    stderr_since_seq: None,
                    max_bytes: None,
                })
                .unwrap();
            if response.attached {
                attached = Some(response);
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let first = attached.expect("foreground stream attaches");
        let session_id = first.session_id.clone().expect("stream session id");
        assert!(first.stdout.contains("first") || !first.eof);

        std::thread::sleep(Duration::from_millis(160));
        let second = state
            .jobs
            .stream(ExecStreamRequest {
                session_id: Some(session_id),
                request_id: None,
                target: None,
                command: None,
                cwd: None,
                stdout_since_seq: Some(first.stdout_next_seq),
                stderr_since_seq: Some(first.stderr_next_seq),
                max_bytes: None,
            })
            .unwrap();
        assert!(second.attached);
        assert!(second.stdout.contains("second") || second.eof);

        let foreground = worker.join().unwrap();
        assert_eq!(foreground.stdout, b"firstsecond");
    }

    #[test]
    fn foreground_exec_uses_policy_default_timeout() {
        let temp = tempdir().unwrap();
        let path = temp.keep();
        let mut config = Config::default();
        if let Some(TargetConfig::Local(local)) = config.targets.get_mut("local") {
            local.enabled = true;
            local.policy.allow_exec = true;
            local.policy.default_timeout_ms = 30;
        }
        config.server.default_target = Some("local".to_string());
        config.server.oauth_state_file = None;
        config.server.runtime_dir = path.join("runtime");
        let state = AppState::new(config).unwrap();
        let started = std::time::Instant::now();
        let output = state
            .jobs
            .run_foreground(
                &state,
                ExecStartRequest {
                    target: Some("local".into()),
                    command: "sleep 1".into(),
                    cwd: None,
                    timeout_ms: None,
                    max_output_bytes: None,
                    secret_env: BTreeMap::new(),
                },
                None,
            )
            .unwrap();
        assert!(output.timed_out);
        assert!(started.elapsed() < Duration::from_millis(700));
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
        let foreground = state
            .jobs
            .run_foreground(&state, request.clone(), None)
            .unwrap();
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
