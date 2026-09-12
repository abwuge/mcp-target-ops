use crate::{
    core::{
        config::TargetConfig,
        error::{Error, Result},
        policy,
        secret::SecretRef,
        state::AppState,
        target::{ResolvedTarget, TargetId},
    },
    tooling::{
        exec::{
            configure_command_process_group, local_shell_command, terminate_child_process_group,
        },
        secret,
        stream::RingBuffer,
    },
    transport::ssh,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    io::Read,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
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

pub struct JobRegistry {
    sessions: Mutex<HashMap<String, Arc<JobSession>>>,
}

struct JobSession {
    target: TargetId,
    child: Arc<Mutex<Child>>,
    stdout: Arc<RingBuffer>,
    stderr: Arc<RingBuffer>,
    stdout_eof: Arc<AtomicBool>,
    stderr_eof: Arc<AtomicBool>,
    status: Arc<Mutex<JobStatus>>,
    cancel_requested: Arc<AtomicBool>,
}

struct JobStatus {
    started: Instant,
    finished: Option<Instant>,
    exit_code: Option<i32>,
    timed_out: bool,
}

impl JobRegistry {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }

    pub fn start(&self, state: &AppState, req: ExecStartRequest) -> Result<ExecStartResponse> {
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

        let command = match (target.clone(), config) {
            (TargetId::Local, TargetConfig::Local(_)) => {
                local_shell_command(&req.command, req.cwd.as_deref(), &secret_env)
            }
            (TargetId::Ssh(_), TargetConfig::Ssh(ssh_config)) => {
                let (program, args) = ssh::exec_program_and_args(
                    ssh_config,
                    &req.command,
                    req.cwd.as_deref(),
                    &secret_env,
                );
                let mut command = Command::new(program);
                command.args(args);
                command
            }
            _ => {
                return Err(Error::Target(format!(
                    "target {target} has mismatched config"
                )))
            }
        };

        let id = format!("job_{}", JOB_COUNTER.fetch_add(1, Ordering::Relaxed));
        let session = Arc::new(spawn_job(
            command,
            target.clone(),
            max_output,
            req.timeout_ms,
        )?);
        self.insert(id.clone(), Arc::clone(&session));

        Ok(ExecStartResponse {
            resolved_target: state.resolved_target_value(target, source),
            job_id: id,
            status: "running",
        })
    }

    pub fn poll(&self, req: JobPollRequest) -> Result<JobPollResponse> {
        let session = self.get(&req.job_id)?;
        Ok(poll_response(&req.job_id, &session))
    }

    pub fn output(&self, req: JobOutputRequest) -> Result<JobOutputResponse> {
        let session = self.get(&req.job_id)?;
        let stdout_from_seq = req.stdout_since_seq.unwrap_or(0);
        let stderr_from_seq = req.stderr_since_seq.unwrap_or(0);
        let max_bytes = req.max_bytes.unwrap_or(64 * 1024).clamp(1, 512 * 1024);
        let (stdout, stdout_next_seq, stdout_truncated) =
            session.stdout.read_since(stdout_from_seq, max_bytes);
        let (stderr, stderr_next_seq, stderr_truncated) =
            session.stderr.read_since(stderr_from_seq, max_bytes);
        let eof = session.stdout_eof.load(Ordering::Acquire)
            && session.stderr_eof.load(Ordering::Acquire)
            && session.status.lock().unwrap().finished.is_some();

        Ok(JobOutputResponse {
            job_id: req.job_id,
            target: session.target.to_string(),
            stdout_from_seq,
            stdout_next_seq,
            stdout: String::from_utf8_lossy(&stdout).to_string(),
            stdout_truncated,
            stderr_from_seq,
            stderr_next_seq,
            stderr: String::from_utf8_lossy(&stderr).to_string(),
            stderr_truncated,
            eof,
        })
    }

    pub fn cancel(&self, req: JobCancelRequest) -> Result<JobCancelResponse> {
        let session = self.get(&req.job_id)?;
        let running = session.status.lock().unwrap().finished.is_none();
        let requested = if running {
            session.cancel_requested.store(true, Ordering::Release);
            terminate_child_process_group(&mut session.child.lock().unwrap()).is_ok()
        } else {
            false
        };
        let response = poll_response(&req.job_id, &session);
        Ok(JobCancelResponse {
            job_id: req.job_id,
            cancel_requested: requested || response.cancel_requested,
            status: response.status,
        })
    }

    pub fn ids(&self) -> Vec<String> {
        self.sessions.lock().unwrap().keys().cloned().collect()
    }

    fn get(&self, id: &str) -> Result<Arc<JobSession>> {
        self.sessions
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| Error::Tool(format!("job {id} not found")))
    }

    fn insert(&self, id: String, session: Arc<JobSession>) {
        let mut sessions = self.sessions.lock().unwrap();
        if sessions.len() >= MAX_RETAINED_JOBS {
            sessions.retain(|_, session| session.status.lock().unwrap().finished.is_none());
        }
        if sessions.len() >= MAX_RETAINED_JOBS {
            if let Some(oldest_finished) = sessions
                .iter()
                .filter_map(|(id, session)| {
                    session
                        .status
                        .lock()
                        .unwrap()
                        .finished
                        .map(|finished| (id.clone(), finished))
                })
                .min_by_key(|(_, finished)| *finished)
                .map(|(id, _)| id)
            {
                sessions.remove(&oldest_finished);
            }
        }
        sessions.insert(id, session);
    }
}

fn spawn_job(
    mut command: Command,
    target: TargetId,
    max_output: usize,
    timeout_ms: Option<u64>,
) -> Result<JobSession> {
    configure_command_process_group(&mut command);
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| Error::Tool("failed to open job stdout".to_string()))?;
    let stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| Error::Tool("failed to open job stderr".to_string()))?;

    let child = Arc::new(Mutex::new(child));
    let stdout = Arc::new(RingBuffer::new(max_output));
    let stderr = Arc::new(RingBuffer::new(max_output));
    let stdout_eof = Arc::new(AtomicBool::new(false));
    let stderr_eof = Arc::new(AtomicBool::new(false));
    spawn_reader(stdout_pipe, Arc::clone(&stdout), Arc::clone(&stdout_eof));
    spawn_reader(stderr_pipe, Arc::clone(&stderr), Arc::clone(&stderr_eof));

    let status = Arc::new(Mutex::new(JobStatus {
        started: Instant::now(),
        finished: None,
        exit_code: None,
        timed_out: false,
    }));
    let cancel_requested = Arc::new(AtomicBool::new(false));
    spawn_monitor(
        Arc::clone(&child),
        Arc::clone(&status),
        timeout_ms.map(Duration::from_millis),
    );

    Ok(JobSession {
        target,
        child,
        stdout,
        stderr,
        stdout_eof,
        stderr_eof,
        status,
        cancel_requested,
    })
}

fn spawn_reader(
    mut reader: impl Read + Send + 'static,
    buffer: Arc<RingBuffer>,
    eof: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        let mut scratch = [0_u8; 8192];
        loop {
            match reader.read(&mut scratch) {
                Ok(0) => break,
                Ok(n) => buffer.push(&scratch[..n]),
                Err(_) => break,
            }
        }
        eof.store(true, Ordering::Release);
    });
}

fn spawn_monitor(
    child: Arc<Mutex<Child>>,
    status: Arc<Mutex<JobStatus>>,
    timeout: Option<Duration>,
) {
    thread::spawn(move || loop {
        let maybe_status = {
            let mut child = child.lock().unwrap();
            child.try_wait()
        };
        match maybe_status {
            Ok(Some(exit)) => {
                let mut status = status.lock().unwrap();
                status.exit_code = exit.code();
                status.finished = Some(Instant::now());
                break;
            }
            Ok(None) => {}
            Err(_) => {
                let mut status = status.lock().unwrap();
                status.finished = Some(Instant::now());
                break;
            }
        }

        let should_timeout = timeout.is_some_and(|timeout| {
            let status = status.lock().unwrap();
            status.started.elapsed() >= timeout
        });
        if should_timeout {
            let mut child = child.lock().unwrap();
            let _ = terminate_child_process_group(&mut child);
            let exit = child.wait().ok();
            let mut status = status.lock().unwrap();
            status.exit_code = exit.and_then(|status| status.code());
            status.timed_out = true;
            status.finished = Some(Instant::now());
            break;
        }

        thread::sleep(Duration::from_millis(20));
    });
}

fn poll_response(job_id: &str, session: &JobSession) -> JobPollResponse {
    let status = session.status.lock().unwrap();
    let cancel_requested = session.cancel_requested.load(Ordering::Acquire);
    let state = if status.finished.is_none() {
        "running"
    } else if status.timed_out {
        "timed_out"
    } else if cancel_requested {
        "cancelled"
    } else if status.exit_code == Some(0) {
        "completed"
    } else {
        "failed"
    };
    let end = status.finished.unwrap_or_else(Instant::now);
    JobPollResponse {
        job_id: job_id.to_string(),
        target: session.target.to_string(),
        status: state.to_string(),
        exit_code: status.exit_code,
        elapsed_ms: end
            .saturating_duration_since(status.started)
            .as_millis()
            .min(u128::from(u64::MAX)) as u64,
        timed_out: status.timed_out,
        cancel_requested,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_incremental_output_and_completion() {
        let command =
            local_shell_command("printf one; sleep 0.05; printf two", None, &BTreeMap::new());
        let session = spawn_job(command, TargetId::Local, 1024, Some(10_000)).unwrap();
        let session = Arc::new(session);
        let deadline = Instant::now() + Duration::from_secs(10);
        while session.status.lock().unwrap().finished.is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(session.status.lock().unwrap().finished.is_some());
        let (stdout, _, _) = session.stdout.read_since(0, 1024);
        assert_eq!(stdout, b"onetwo");
        assert_eq!(poll_response("job_test", &session).status, "completed");
    }

    #[test]
    fn times_out_long_running_job() {
        let command = local_shell_command("sleep 1", None, &BTreeMap::new());
        let session = spawn_job(command, TargetId::Local, 1024, Some(20)).unwrap();
        let session = Arc::new(session);
        for _ in 0..100 {
            if session.status.lock().unwrap().finished.is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(poll_response("job_test", &session).status, "timed_out");

        let eof_deadline = Instant::now() + Duration::from_millis(700);
        while (!session.stdout_eof.load(Ordering::Acquire)
            || !session.stderr_eof.load(Ordering::Acquire))
            && Instant::now() < eof_deadline
        {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(session.stdout_eof.load(Ordering::Acquire));
        assert!(session.stderr_eof.load(Ordering::Acquire));
    }
}
