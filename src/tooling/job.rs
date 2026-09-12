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
const AUTO_BACKGROUND_AFTER: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy)]
enum SessionTimeoutPolicy {
    ForegroundDefault,
    ExplicitOnly,
    DefaultBefore(Duration),
}

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

#[derive(Debug, Clone, Deserialize)]
pub struct JobWaitRequest {
    pub job_id: String,
    #[serde(default)]
    pub wait_timeout_ms: Option<u64>,
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

#[derive(Debug, Clone, Serialize)]
pub struct JobWaitResponse {
    #[serde(flatten)]
    pub output: JobOutputResponse,
    pub wait_timed_out: bool,
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

#[derive(Debug)]
pub enum AdaptiveExecOutput {
    Completed(ForegroundOutput),
    Backgrounded {
        resolved_target: ResolvedTarget,
        job_id: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct CompletedJobResult {
    pub job_id: String,
    pub target: String,
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub elapsed_ms: u64,
    pub timed_out: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub stdout: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

struct JobEntry {
    session: Arc<CommandSession>,
    caller_key: String,
    command: String,
    cwd: Option<String>,
    order: u64,
    delivered: bool,
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
    sessions: Mutex<HashMap<String, JobEntry>>,
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
        let (resolved_target, session) =
            self.create_session(state, &req, SessionTimeoutPolicy::ForegroundDefault)?;
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

    pub fn run_adaptive(
        &self,
        state: &AppState,
        req: ExecStartRequest,
        request_id: Option<&Value>,
        caller_key: &str,
    ) -> Result<AdaptiveExecOutput> {
        self.run_adaptive_with_threshold(state, req, request_id, caller_key, AUTO_BACKGROUND_AFTER)
    }

    fn run_adaptive_with_threshold(
        &self,
        state: &AppState,
        req: ExecStartRequest,
        request_id: Option<&Value>,
        caller_key: &str,
        threshold: Duration,
    ) -> Result<AdaptiveExecOutput> {
        let (resolved_target, session) =
            self.create_session(state, &req, SessionTimeoutPolicy::DefaultBefore(threshold))?;
        let order = EXEC_COUNTER.fetch_add(1, Ordering::Relaxed);
        let foreground_id = format!("exec_{order}");
        self.insert_foreground(
            foreground_id.clone(),
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

        if session.wait_for_eof(Some(threshold)) {
            let snapshot = session.snapshot();
            self.foreground.lock().unwrap().remove(&foreground_id);
            return Ok(AdaptiveExecOutput::Completed(ForegroundOutput {
                resolved_target,
                exit_code: snapshot.exit_code,
                stdout: snapshot.stdout,
                stderr: snapshot.stderr,
                stdout_truncated: snapshot.stdout_truncated,
                stderr_truncated: snapshot.stderr_truncated,
                timed_out: snapshot.timed_out,
            }));
        }

        self.foreground.lock().unwrap().remove(&foreground_id);
        let order = JOB_COUNTER.fetch_add(1, Ordering::Relaxed);
        let job_id = format!("job_{order}");
        self.insert(
            job_id.clone(),
            JobEntry {
                session,
                caller_key: caller_key.to_string(),
                command: req.command,
                cwd: req.cwd,
                order,
                delivered: false,
            },
        );
        Ok(AdaptiveExecOutput::Backgrounded {
            resolved_target,
            job_id,
        })
    }

    #[cfg(test)]
    pub fn start(&self, state: &AppState, req: ExecStartRequest) -> Result<ExecStartResponse> {
        self.start_for_caller(state, req, "direct")
    }

    pub fn start_for_caller(
        &self,
        state: &AppState,
        req: ExecStartRequest,
        caller_key: &str,
    ) -> Result<ExecStartResponse> {
        let (resolved_target, session) =
            self.create_session(state, &req, SessionTimeoutPolicy::ExplicitOnly)?;
        let order = JOB_COUNTER.fetch_add(1, Ordering::Relaxed);
        let id = format!("job_{order}");
        self.insert(
            id.clone(),
            JobEntry {
                session,
                caller_key: caller_key.to_string(),
                command: req.command,
                cwd: req.cwd,
                order,
                delivered: false,
            },
        );
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
        timeout_policy: SessionTimeoutPolicy,
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
        let default_timeout = Duration::from_millis(target_policy.default_timeout_ms);
        let timeout = req
            .timeout_ms
            .map(Duration::from_millis)
            .or_else(|| match timeout_policy {
                SessionTimeoutPolicy::ForegroundDefault => Some(default_timeout),
                SessionTimeoutPolicy::ExplicitOnly => None,
                SessionTimeoutPolicy::DefaultBefore(limit) => {
                    (default_timeout <= limit).then_some(default_timeout)
                }
            });
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

    #[cfg(test)]
    pub fn poll(&self, req: JobPollRequest) -> Result<JobPollResponse> {
        self.poll_for_caller(req, "direct")
    }

    pub fn poll_for_caller(
        &self,
        req: JobPollRequest,
        caller_key: &str,
    ) -> Result<JobPollResponse> {
        let session = self.get_for_caller(&req.job_id, caller_key)?;
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

    #[cfg(test)]
    pub fn output(&self, req: JobOutputRequest) -> Result<JobOutputResponse> {
        self.output_for_caller(req, "direct")
    }

    pub fn output_for_caller(
        &self,
        req: JobOutputRequest,
        caller_key: &str,
    ) -> Result<JobOutputResponse> {
        let session = self.get_for_caller(&req.job_id, caller_key)?;
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

    #[cfg(test)]
    pub fn wait(&self, req: JobWaitRequest) -> Result<JobWaitResponse> {
        self.wait_for_caller(req, "direct")
    }

    pub fn wait_for_caller(
        &self,
        req: JobWaitRequest,
        caller_key: &str,
    ) -> Result<JobWaitResponse> {
        let session = self.get_for_caller(&req.job_id, caller_key)?;
        let wait_timeout =
            Duration::from_millis(req.wait_timeout_ms.unwrap_or(60_000).min(120_000));
        let completed = session.wait_for_eof(Some(wait_timeout));
        let output = self.output_for_caller(
            JobOutputRequest {
                job_id: req.job_id,
                stdout_since_seq: req.stdout_since_seq,
                stderr_since_seq: req.stderr_since_seq,
                max_bytes: req.max_bytes,
            },
            caller_key,
        )?;
        if output.eof {
            self.mark_delivered(&output.job_id, caller_key)?;
        }
        Ok(JobWaitResponse {
            wait_timed_out: !completed && !output.eof,
            output,
        })
    }

    pub fn cancel_for_caller(
        &self,
        req: JobCancelRequest,
        caller_key: &str,
    ) -> Result<JobCancelResponse> {
        let session = self.get_for_caller(&req.job_id, caller_key)?;
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
            // COMPAT(COMPAT-005): Older hosts may omit the original tools/call
            // request id, so fall back to a one-time target/command/cwd match.
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

    pub fn take_completed_for_caller(&self, caller_key: &str) -> Vec<CompletedJobResult> {
        let mut sessions = self.sessions.lock().unwrap();
        let mut ready = sessions
            .iter()
            .filter(|(_, entry)| {
                entry.caller_key == caller_key
                    && !entry.delivered
                    && entry.session.status_snapshot().eof
            })
            .map(|(id, entry)| (entry.order, id.clone()))
            .collect::<Vec<_>>();
        ready.sort_by_key(|(order, _)| *order);

        ready
            .into_iter()
            .filter_map(|(_, id)| {
                let entry = sessions.get_mut(&id)?;
                let status = entry.session.status_snapshot();
                let snapshot = entry.session.snapshot();
                entry.delivered = true;
                Some(CompletedJobResult {
                    job_id: id,
                    target: entry.session.target.to_string(),
                    command: entry.command.clone(),
                    cwd: entry.cwd.clone(),
                    status: entry.session.state().to_string(),
                    exit_code: status.exit_code,
                    elapsed_ms: status.elapsed_ms,
                    timed_out: status.timed_out,
                    stdout: String::from_utf8_lossy(&snapshot.stdout).to_string(),
                    stderr: String::from_utf8_lossy(&snapshot.stderr).to_string(),
                    stdout_truncated: snapshot.stdout_truncated,
                    stderr_truncated: snapshot.stderr_truncated,
                })
            })
            .collect()
    }

    fn get_for_caller(&self, id: &str, caller_key: &str) -> Result<Arc<CommandSession>> {
        let sessions = self.sessions.lock().unwrap();
        let entry = sessions
            .get(id)
            .ok_or_else(|| Error::Tool(format!("job {id} not found")))?;
        if entry.caller_key != caller_key {
            return Err(Error::Tool(format!("job {id} not found")));
        }
        Ok(Arc::clone(&entry.session))
    }

    fn mark_delivered(&self, id: &str, caller_key: &str) -> Result<()> {
        let mut sessions = self.sessions.lock().unwrap();
        let entry = sessions
            .get_mut(id)
            .ok_or_else(|| Error::Tool(format!("job {id} not found")))?;
        if entry.caller_key != caller_key {
            return Err(Error::Tool(format!("job {id} not found")));
        }
        entry.delivered = true;
        Ok(())
    }

    fn insert(&self, id: String, entry: JobEntry) {
        let mut sessions = self.sessions.lock().unwrap();
        if sessions.len() >= MAX_RETAINED_JOBS {
            sessions
                .retain(|_, entry| !entry.session.status_snapshot().finished || !entry.delivered);
        }
        if sessions.len() >= MAX_RETAINED_JOBS {
            if let Some(id) = sessions
                .iter()
                .filter(|(_, entry)| entry.session.status_snapshot().finished)
                .min_by_key(|(_, entry)| (if entry.delivered { 0_u8 } else { 1_u8 }, entry.order))
                .map(|(id, _)| id.clone())
            {
                sessions.remove(&id);
            }
        }
        sessions.insert(id, entry);
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
    use crate::core::{config::Config, state::AppState, util::shell_quote};
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
        let control = tempdir().unwrap();
        let release = control.path().join("release");
        let command = format!(
            "printf first; while [ ! -f {} ]; do sleep 0.01; done; printf second",
            shell_quote(&release.to_string_lossy())
        );
        let request = ExecStartRequest {
            target: Some("local".into()),
            command: command.clone(),
            cwd: None,
            timeout_ms: Some(5000),
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

        let mut first = None;
        for _ in 0..200 {
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
            if response.attached && response.stdout.contains("first") {
                first = Some(response);
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let first = first.expect("foreground stream attaches and exposes first chunk");
        let session_id = first.session_id.clone().expect("stream session id");
        assert!(!first.eof, "command must still be waiting for release");

        std::fs::write(&release, b"go").unwrap();
        let mut stdout_seq = first.stdout_next_seq;
        let mut stderr_seq = first.stderr_next_seq;
        let mut saw_second = false;
        for _ in 0..200 {
            let response = state
                .jobs
                .stream(ExecStreamRequest {
                    session_id: Some(session_id.clone()),
                    request_id: None,
                    target: None,
                    command: None,
                    cwd: None,
                    stdout_since_seq: Some(stdout_seq),
                    stderr_since_seq: Some(stderr_seq),
                    max_bytes: None,
                })
                .unwrap();
            assert!(response.attached);
            saw_second |= response.stdout.contains("second");
            stdout_seq = response.stdout_next_seq;
            stderr_seq = response.stderr_next_seq;
            if saw_second || response.eof {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(saw_second, "incremental stream exposes second chunk");

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

    #[test]
    fn job_wait_blocks_until_completion_and_returns_output() {
        let state = test_state();
        let started = state
            .jobs
            .start(
                &state,
                ExecStartRequest {
                    target: Some("local".into()),
                    command: "sleep 0.05; printf waited".into(),
                    cwd: None,
                    timeout_ms: Some(1000),
                    max_output_bytes: None,
                    secret_env: BTreeMap::new(),
                },
            )
            .unwrap();
        let waited = state
            .jobs
            .wait(JobWaitRequest {
                job_id: started.job_id,
                wait_timeout_ms: Some(1000),
                stdout_since_seq: None,
                stderr_since_seq: None,
                max_bytes: None,
            })
            .unwrap();
        assert!(!waited.wait_timed_out);
        assert!(waited.output.eof);
        assert_eq!(waited.output.status, "completed");
        assert_eq!(waited.output.stdout, "waited");
    }

    #[test]
    fn job_wait_can_return_when_wait_window_expires() {
        let state = test_state();
        let started = state
            .jobs
            .start(
                &state,
                ExecStartRequest {
                    target: Some("local".into()),
                    command: "sleep 0.2; printf done".into(),
                    cwd: None,
                    timeout_ms: Some(1000),
                    max_output_bytes: None,
                    secret_env: BTreeMap::new(),
                },
            )
            .unwrap();
        let waited = state
            .jobs
            .wait(JobWaitRequest {
                job_id: started.job_id.clone(),
                wait_timeout_ms: Some(20),
                stdout_since_seq: None,
                stderr_since_seq: None,
                max_bytes: None,
            })
            .unwrap();
        assert!(waited.wait_timed_out);
        assert_eq!(waited.output.status, "running");
        assert!(!waited.output.eof);
        state
            .jobs
            .wait(JobWaitRequest {
                job_id: started.job_id,
                wait_timeout_ms: Some(1000),
                stdout_since_seq: None,
                stderr_since_seq: None,
                max_bytes: None,
            })
            .unwrap();
    }

    #[test]
    fn adaptive_exec_promotes_same_session_without_rerunning() {
        let state = test_state();
        let outcome = state
            .jobs
            .run_adaptive_with_threshold(
                &state,
                ExecStartRequest {
                    target: Some("local".into()),
                    command: "printf before; sleep 0.08; printf after".into(),
                    cwd: None,
                    timeout_ms: Some(1000),
                    max_output_bytes: None,
                    secret_env: BTreeMap::new(),
                },
                None,
                "caller-a",
                Duration::from_millis(15),
            )
            .unwrap();
        let job_id = match outcome {
            AdaptiveExecOutput::Backgrounded { job_id, .. } => job_id,
            AdaptiveExecOutput::Completed(_) => panic!("command should have been backgrounded"),
        };
        let waited = state
            .jobs
            .wait_for_caller(
                JobWaitRequest {
                    job_id,
                    wait_timeout_ms: Some(1000),
                    stdout_since_seq: None,
                    stderr_since_seq: None,
                    max_bytes: None,
                },
                "caller-a",
            )
            .unwrap();
        assert_eq!(waited.output.stdout, "beforeafter");
        assert!(waited.output.eof);
    }

    #[test]
    fn adaptive_exec_drops_long_foreground_default_timeout_after_promotion() {
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

        let outcome = state
            .jobs
            .run_adaptive_with_threshold(
                &state,
                ExecStartRequest {
                    target: Some("local".into()),
                    command: "sleep 0.06; printf survived".into(),
                    cwd: None,
                    timeout_ms: None,
                    max_output_bytes: None,
                    secret_env: BTreeMap::new(),
                },
                None,
                "caller-a",
                Duration::from_millis(10),
            )
            .unwrap();
        let job_id = match outcome {
            AdaptiveExecOutput::Backgrounded { job_id, .. } => job_id,
            AdaptiveExecOutput::Completed(_) => panic!("command should have been backgrounded"),
        };
        let waited = state
            .jobs
            .wait_for_caller(
                JobWaitRequest {
                    job_id,
                    wait_timeout_ms: Some(1000),
                    stdout_since_seq: None,
                    stderr_since_seq: None,
                    max_bytes: None,
                },
                "caller-a",
            )
            .unwrap();
        assert_eq!(waited.output.status, "completed");
        assert!(!waited.output.timed_out);
        assert_eq!(waited.output.stdout, "survived");
    }

    #[test]
    fn completed_job_inbox_is_caller_scoped_and_drains_once() {
        let state = test_state();
        let started = state
            .jobs
            .start_for_caller(
                &state,
                ExecStartRequest {
                    target: Some("local".into()),
                    command: "printf queued".into(),
                    cwd: None,
                    timeout_ms: Some(1000),
                    max_output_bytes: None,
                    secret_env: BTreeMap::new(),
                },
                "caller-a",
            )
            .unwrap();
        while state
            .jobs
            .poll_for_caller(
                JobPollRequest {
                    job_id: started.job_id.clone(),
                },
                "caller-a",
            )
            .unwrap()
            .status
            == "running"
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(state.jobs.take_completed_for_caller("caller-b").is_empty());
        let completed = state.jobs.take_completed_for_caller("caller-a");
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].job_id, started.job_id);
        assert_eq!(completed[0].stdout, "queued");
        assert!(state.jobs.take_completed_for_caller("caller-a").is_empty());
    }

    #[test]
    fn job_output_peek_does_not_claim_completion() {
        let state = test_state();
        let started = state
            .jobs
            .start_for_caller(
                &state,
                ExecStartRequest {
                    target: Some("local".into()),
                    command: "printf peeked".into(),
                    cwd: None,
                    timeout_ms: Some(1000),
                    max_output_bytes: None,
                    secret_env: BTreeMap::new(),
                },
                "caller-a",
            )
            .unwrap();
        while state
            .jobs
            .poll_for_caller(
                JobPollRequest {
                    job_id: started.job_id.clone(),
                },
                "caller-a",
            )
            .unwrap()
            .status
            == "running"
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        let output = state
            .jobs
            .output_for_caller(
                JobOutputRequest {
                    job_id: started.job_id.clone(),
                    stdout_since_seq: None,
                    stderr_since_seq: None,
                    max_bytes: None,
                },
                "caller-a",
            )
            .unwrap();
        assert!(output.eof);
        assert_eq!(output.stdout, "peeked");
        let completed = state.jobs.take_completed_for_caller("caller-a");
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].job_id, started.job_id);
    }

    #[test]
    fn job_wait_claims_completion_before_inbox_delivery() {
        let state = test_state();
        let started = state
            .jobs
            .start_for_caller(
                &state,
                ExecStartRequest {
                    target: Some("local".into()),
                    command: "sleep 0.02; printf claimed".into(),
                    cwd: None,
                    timeout_ms: Some(1000),
                    max_output_bytes: None,
                    secret_env: BTreeMap::new(),
                },
                "caller-a",
            )
            .unwrap();
        let waited = state
            .jobs
            .wait_for_caller(
                JobWaitRequest {
                    job_id: started.job_id,
                    wait_timeout_ms: Some(1000),
                    stdout_since_seq: None,
                    stderr_since_seq: None,
                    max_bytes: None,
                },
                "caller-a",
            )
            .unwrap();
        assert_eq!(waited.output.stdout, "claimed");
        assert!(state.jobs.take_completed_for_caller("caller-a").is_empty());
    }
}
