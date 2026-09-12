use crate::{
    core::{
        error::{Error, Result},
        target::TargetId,
    },
    tooling::stream::RingBuffer,
};
use std::{
    io::Read,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

pub struct CommandSession {
    pub target: TargetId,
    child: Arc<Mutex<Child>>,
    stdout: Arc<RingBuffer>,
    stderr: Arc<RingBuffer>,
    stdout_eof: Arc<AtomicBool>,
    stderr_eof: Arc<AtomicBool>,
    status: Arc<Mutex<CommandStatus>>,
    cancel_requested: Arc<AtomicBool>,
}

struct CommandStatus {
    started: Instant,
    finished: Option<Instant>,
    exit_code: Option<i32>,
    timed_out: bool,
}

#[derive(Debug)]
pub struct CommandSnapshot {
    pub exit_code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub timed_out: bool,
    pub cancelled: bool,
    pub elapsed_ms: u64,
    pub finished: bool,
    pub eof: bool,
}

#[derive(Debug)]
pub struct CommandOutputDelta {
    pub stdout_from_seq: u64,
    pub stdout_next_seq: u64,
    pub stdout: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr_from_seq: u64,
    pub stderr_next_seq: u64,
    pub stderr: Vec<u8>,
    pub stderr_truncated: bool,
    pub eof: bool,
}

impl CommandSession {
    pub fn spawn(
        mut command: Command,
        target: TargetId,
        max_output: usize,
        timeout: Option<Duration>,
    ) -> Result<Arc<Self>> {
        configure_process_group(&mut command);
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout_pipe = child
            .stdout
            .take()
            .ok_or_else(|| Error::Tool("failed to open command stdout".to_string()))?;
        let stderr_pipe = child
            .stderr
            .take()
            .ok_or_else(|| Error::Tool("failed to open command stderr".to_string()))?;

        let child = Arc::new(Mutex::new(child));
        let stdout = Arc::new(RingBuffer::new(max_output.max(1)));
        let stderr = Arc::new(RingBuffer::new(max_output.max(1)));
        let stdout_eof = Arc::new(AtomicBool::new(false));
        let stderr_eof = Arc::new(AtomicBool::new(false));
        spawn_reader(stdout_pipe, Arc::clone(&stdout), Arc::clone(&stdout_eof));
        spawn_reader(stderr_pipe, Arc::clone(&stderr), Arc::clone(&stderr_eof));

        let status = Arc::new(Mutex::new(CommandStatus {
            started: Instant::now(),
            finished: None,
            exit_code: None,
            timed_out: false,
        }));
        let cancel_requested = Arc::new(AtomicBool::new(false));
        spawn_monitor(Arc::clone(&child), Arc::clone(&status), timeout);

        Ok(Arc::new(Self {
            target,
            child,
            stdout,
            stderr,
            stdout_eof,
            stderr_eof,
            status,
            cancel_requested,
        }))
    }

    pub fn wait(&self) -> CommandSnapshot {
        loop {
            let snapshot = self.snapshot();
            if snapshot.finished && snapshot.eof {
                return snapshot;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn snapshot(&self) -> CommandSnapshot {
        let status = self.status.lock().unwrap();
        let end = status.finished.unwrap_or_else(Instant::now);
        let (stdout, _, stdout_truncated) = self.stdout.read_since(0, usize::MAX);
        let (stderr, _, stderr_truncated) = self.stderr.read_since(0, usize::MAX);
        let eof = self.stdout_eof.load(Ordering::Acquire)
            && self.stderr_eof.load(Ordering::Acquire)
            && status.finished.is_some();
        CommandSnapshot {
            exit_code: status.exit_code,
            stdout,
            stderr,
            stdout_truncated,
            stderr_truncated,
            timed_out: status.timed_out,
            cancelled: self.cancel_requested.load(Ordering::Acquire),
            elapsed_ms: end
                .saturating_duration_since(status.started)
                .as_millis()
                .min(u128::from(u64::MAX)) as u64,
            finished: status.finished.is_some(),
            eof,
        }
    }

    pub fn output_since(
        &self,
        stdout_since_seq: u64,
        stderr_since_seq: u64,
        max_bytes: usize,
    ) -> CommandOutputDelta {
        let (stdout, stdout_next_seq, stdout_truncated) =
            self.stdout.read_since(stdout_since_seq, max_bytes);
        let (stderr, stderr_next_seq, stderr_truncated) =
            self.stderr.read_since(stderr_since_seq, max_bytes);
        let eof = self.stdout_eof.load(Ordering::Acquire)
            && self.stderr_eof.load(Ordering::Acquire)
            && self.status.lock().unwrap().finished.is_some();
        CommandOutputDelta {
            stdout_from_seq: stdout_since_seq,
            stdout_next_seq,
            stdout,
            stdout_truncated,
            stderr_from_seq: stderr_since_seq,
            stderr_next_seq,
            stderr,
            stderr_truncated,
            eof,
        }
    }

    pub fn cancel(&self) -> bool {
        let running = self.status.lock().unwrap().finished.is_none();
        if !running {
            return false;
        }
        self.cancel_requested.store(true, Ordering::Release);
        terminate_process_group(&mut self.child.lock().unwrap()).is_ok()
    }

    pub fn state(&self) -> &'static str {
        let status = self.status.lock().unwrap();
        if status.finished.is_none() {
            "running"
        } else if status.timed_out {
            "timed_out"
        } else if self.cancel_requested.load(Ordering::Acquire) {
            "cancelled"
        } else if status.exit_code == Some(0) {
            "completed"
        } else {
            "failed"
        }
    }
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
    status: Arc<Mutex<CommandStatus>>,
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
                status.lock().unwrap().finished = Some(Instant::now());
                break;
            }
        }

        let should_timeout = timeout.is_some_and(|timeout| {
            let status = status.lock().unwrap();
            status.started.elapsed() >= timeout
        });
        if should_timeout {
            let mut child = child.lock().unwrap();
            let _ = terminate_process_group(&mut child);
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

fn configure_process_group(command: &mut Command) {
    #[cfg(unix)]
    {
        command.process_group(0);
    }
}

fn terminate_process_group(child: &mut Child) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let pgid = libc::pid_t::try_from(child.id()).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "child pid does not fit pid_t",
            )
        })?;
        // SAFETY: spawned commands are placed in a fresh process group whose id is the child pid.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn timeout_terminates_external_children_and_closes_pipes() {
        let mut command = Command::new("sh");
        command.arg("-lc").arg("sleep 1");
        let started = Instant::now();
        let session = CommandSession::spawn(
            command,
            TargetId::Local,
            1024,
            Some(Duration::from_millis(50)),
        )
        .unwrap();
        let snapshot = session.wait();
        assert!(snapshot.timed_out);
        assert!(snapshot.eof);
        assert!(started.elapsed() < Duration::from_millis(700));
    }
}
