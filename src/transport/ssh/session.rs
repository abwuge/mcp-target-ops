use super::{base_args, client_args, destination};
use crate::{
    core::{
        config::SshTargetConfig,
        error::{Error, Result},
        util::shell_quote,
    },
    tooling::exec::RawExecOutput,
};
use std::{
    collections::{hash_map::DefaultHasher, HashMap},
    fs,
    hash::{Hash, Hasher},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        mpsc::{self, Receiver},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tempfile::{Builder, TempDir};

pub struct SshSessionRegistry {
    sessions: Mutex<HashMap<String, Arc<SshSession>>>,
    masters: Mutex<HashMap<String, SshControlMaster>>,
    control_dir: TempDir,
}

impl SshSessionRegistry {
    pub fn new() -> Result<Self> {
        let control_dir = Builder::new().prefix("mcp-target-ops-ssh-").tempdir()?;
        Ok(Self {
            sessions: Mutex::new(HashMap::new()),
            masters: Mutex::new(HashMap::new()),
            control_dir,
        })
    }

    pub fn ids(&self) -> Vec<String> {
        let mut ids: Vec<_> = self.sessions.lock().unwrap().keys().cloned().collect();
        ids.sort();
        ids
    }

    pub fn master_ids(&self) -> Vec<String> {
        let mut ids: Vec<_> = self.masters.lock().unwrap().keys().cloned().collect();
        ids.sort();
        ids
    }

    pub(super) fn ensure_control_master(
        &self,
        target_name: &str,
        ssh: &SshTargetConfig,
        timeout: Duration,
    ) -> Result<Option<PathBuf>> {
        if !ssh.control_master {
            return Ok(None);
        }

        let mut masters = self.masters.lock().unwrap();
        if let Some(master) = masters.get(target_name) {
            if master.control_path.exists() {
                return Ok(Some(master.control_path.clone()));
            }
            masters.remove(target_name);
        }

        let control_path = self.control_path_for(target_name);
        let _ = fs::remove_file(&control_path);
        start_control_master(target_name, ssh, &control_path, timeout)?;
        masters.insert(
            target_name.to_string(),
            SshControlMaster {
                control_path: control_path.clone(),
                ssh: ssh.clone(),
            },
        );
        Ok(Some(control_path))
    }

    fn control_path_for(&self, target_name: &str) -> PathBuf {
        let mut hasher = DefaultHasher::new();
        target_name.hash(&mut hasher);
        self.control_dir
            .path()
            .join(format!("{:016x}.sock", hasher.finish()))
    }

    fn get_or_start(
        &self,
        target_name: &str,
        ssh: &SshTargetConfig,
        timeout: Duration,
    ) -> Result<Arc<SshSession>> {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(session) = sessions.get(target_name) {
            if !session.is_closed() {
                return Ok(Arc::clone(session));
            }
        }

        let control_path = self.ensure_control_master(target_name, ssh, timeout)?;
        let session = Arc::new(SshSession::start(ssh, control_path.as_deref())?);
        sessions.insert(target_name.to_string(), Arc::clone(&session));
        Ok(session)
    }

    pub(super) fn run_script(
        &self,
        target_name: &str,
        ssh: &SshTargetConfig,
        script: &str,
        script_args: &[&str],
        timeout: Duration,
    ) -> Result<RawExecOutput> {
        let session = self.get_or_start(target_name, ssh, timeout)?;
        let output = session.run_script(script, script_args, timeout);
        if matches!(&output, Ok(raw) if raw.timed_out) || output.is_err() || session.is_closed() {
            let mut sessions = self.sessions.lock().unwrap();
            if sessions
                .get(target_name)
                .is_some_and(|current| Arc::ptr_eq(current, &session))
            {
                sessions.remove(target_name);
            }
        }
        output
    }

    pub(super) fn disconnect(&self, target_name: &str, timeout: Duration) -> Result<RawExecOutput> {
        let session = self.sessions.lock().unwrap().remove(target_name);
        let mut output = match session {
            Some(session) => session.shutdown(timeout)?,
            None => RawExecOutput {
                exit_code: Some(0),
                stdout: b"persistent ssh worker was not running\n".to_vec(),
                stderr: Vec::new(),
                timed_out: false,
            },
        };

        if let Some(master) = self.masters.lock().unwrap().remove(target_name) {
            let master_output = stop_control_master(&master, timeout)?;
            output.stdout.extend_from_slice(&master_output.stdout);
            output.stderr.extend_from_slice(&master_output.stderr);
            output.timed_out |= master_output.timed_out;
            if master_output.exit_code != Some(0) {
                output.exit_code = master_output.exit_code;
            }
        }

        Ok(output)
    }
}

impl Drop for SshSessionRegistry {
    fn drop(&mut self) {
        if let Ok(sessions) = self.sessions.get_mut() {
            sessions.clear();
        }
        if let Ok(masters) = self.masters.get_mut() {
            for (_, master) in masters.drain() {
                let _ = stop_control_master(&master, Duration::from_millis(500));
            }
        }
    }
}

struct SshControlMaster {
    control_path: PathBuf,
    ssh: SshTargetConfig,
}

struct SshSession {
    state: Mutex<SshSessionState>,
}

struct SshSessionState {
    child: Child,
    stdin: ChildStdin,
    stdout_rx: Receiver<Vec<u8>>,
    stderr_rx: Receiver<Vec<u8>>,
    marker_prefix: String,
    next_command_id: u64,
    closed: bool,
}

impl SshSession {
    fn start(ssh: &SshTargetConfig, control_path: Option<&Path>) -> Result<Self> {
        let mut args = client_args(ssh, control_path);
        args.push("-T".to_string());
        args.push(destination(ssh));
        args.push("sh".to_string());

        let mut child = Command::new("ssh")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::Tool("failed to open ssh worker stdin".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Tool("failed to open ssh worker stdout".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| Error::Tool("failed to open ssh worker stderr".to_string()))?;

        let (stdout_tx, stdout_rx) = mpsc::channel();
        let (stderr_tx, stderr_rx) = mpsc::channel();
        spawn_reader(stdout, stdout_tx);
        spawn_reader(stderr, stderr_tx);

        Ok(Self {
            state: Mutex::new(SshSessionState {
                child,
                stdin,
                stdout_rx,
                stderr_rx,
                marker_prefix: marker_prefix(),
                next_command_id: 0,
                closed: false,
            }),
        })
    }

    fn is_closed(&self) -> bool {
        self.state.lock().unwrap().closed
    }

    fn run_script(
        &self,
        script: &str,
        script_args: &[&str],
        timeout: Duration,
    ) -> Result<RawExecOutput> {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return Err(Error::Tool("persistent ssh worker is closed".to_string()));
        }

        let command_id = state.next_command_id;
        state.next_command_id += 1;

        let marker_id = format!("{}_{}", state.marker_prefix, command_id);
        let stdout_prefix = format!("__MCP_TARGET_OPS_{marker_id}_STDOUT_END_");
        let stdout_suffix = b"__\n";
        let stderr_marker = format!("__MCP_TARGET_OPS_{marker_id}_STDERR_END__\n");
        let wrapper = build_wrapper(script, script_args, &stdout_prefix, &stderr_marker);

        if let Err(err) = state.stdin.write_all(wrapper.as_bytes()) {
            state.closed = true;
            return Err(Error::Io(err));
        }
        if let Err(err) = state.stdin.flush() {
            state.closed = true;
            return Err(Error::Io(err));
        }

        let deadline = Instant::now() + timeout;
        let stdout_prefix = stdout_prefix.into_bytes();
        let stderr_marker = stderr_marker.into_bytes();
        let mut stdout_buffer = Vec::new();
        let mut stderr_buffer = Vec::new();
        let mut stdout: Option<Vec<u8>> = None;
        let mut stderr: Option<Vec<u8>> = None;
        let mut exit_code = None;
        let mut timed_out = false;

        loop {
            drain_channel(&state.stdout_rx, &mut stdout_buffer);
            drain_channel(&state.stderr_rx, &mut stderr_buffer);

            if stdout.is_none() {
                if let Some((code, bytes)) =
                    take_stdout_until_marker(&mut stdout_buffer, &stdout_prefix, stdout_suffix)?
                {
                    exit_code = Some(code);
                    stdout = Some(bytes);
                }
            }
            if stderr.is_none() {
                stderr = take_until_marker(&mut stderr_buffer, &stderr_marker);
            }

            if stdout.is_some() && stderr.is_some() {
                break;
            }

            if let Some(status) = state.child.try_wait()? {
                state.closed = true;
                drain_channel(&state.stdout_rx, &mut stdout_buffer);
                drain_channel(&state.stderr_rx, &mut stderr_buffer);
                return Ok(RawExecOutput {
                    exit_code: status.code(),
                    stdout: stdout.unwrap_or(stdout_buffer),
                    stderr: stderr.unwrap_or(stderr_buffer),
                    timed_out: false,
                });
            }

            if Instant::now() >= deadline {
                timed_out = true;
                state.closed = true;
                let _ = state.child.kill();
                let status = state.child.wait()?;
                exit_code = status.code();
                drain_channel(&state.stdout_rx, &mut stdout_buffer);
                drain_channel(&state.stderr_rx, &mut stderr_buffer);
                break;
            }

            thread::sleep(Duration::from_millis(10));
        }

        Ok(RawExecOutput {
            exit_code,
            stdout: stdout.unwrap_or(stdout_buffer),
            stderr: stderr.unwrap_or(stderr_buffer),
            timed_out,
        })
    }

    fn shutdown(&self, timeout: Duration) -> Result<RawExecOutput> {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return Ok(RawExecOutput {
                exit_code: Some(0),
                stdout: b"persistent ssh worker was already closed\n".to_vec(),
                stderr: Vec::new(),
                timed_out: false,
            });
        }

        let _ = state.stdin.write_all(b"exit\n");
        let _ = state.stdin.flush();
        let started = Instant::now();
        let mut timed_out = false;
        let exit_code = loop {
            if let Some(status) = state.child.try_wait()? {
                break status.code();
            }
            if started.elapsed() >= timeout {
                timed_out = true;
                let _ = state.child.kill();
                break state.child.wait()?.code();
            }
            thread::sleep(Duration::from_millis(10));
        };
        state.closed = true;

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        drain_channel(&state.stdout_rx, &mut stdout);
        drain_channel(&state.stderr_rx, &mut stderr);
        if stdout.is_empty() {
            stdout.extend_from_slice(b"persistent ssh worker disconnected\n");
        }

        Ok(RawExecOutput {
            exit_code,
            stdout,
            stderr,
            timed_out,
        })
    }
}

impl Drop for SshSession {
    fn drop(&mut self) {
        let Ok(state) = self.state.get_mut() else {
            return;
        };
        if state.closed {
            return;
        }

        let _ = state.stdin.write_all(b"exit\n");
        let _ = state.stdin.flush();
        let started = Instant::now();
        loop {
            match state.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if started.elapsed() < Duration::from_millis(200) => {
                    thread::sleep(Duration::from_millis(10));
                }
                _ => {
                    let _ = state.child.kill();
                    let _ = state.child.wait();
                    break;
                }
            }
        }
        state.closed = true;
    }
}

fn start_control_master(
    target_name: &str,
    ssh: &SshTargetConfig,
    control_path: &Path,
    timeout: Duration,
) -> Result<()> {
    let mut args = vec![
        "-M".to_string(),
        "-N".to_string(),
        "-f".to_string(),
        "-T".to_string(),
        "-S".to_string(),
        control_path.display().to_string(),
        "-o".to_string(),
        format!("ControlPersist={}", ssh.control_persist_secs),
    ];
    args.extend(base_args(ssh));
    args.push(destination(ssh));

    let output = run_ssh_management_command(args, timeout)?;
    if output.timed_out {
        return Err(Error::Tool(format!(
            "timed out while starting shared SSH control master for ssh:{target_name}"
        )));
    }
    if output.exit_code != Some(0) {
        return Err(Error::Tool(format!(
            "failed to start shared SSH control master for ssh:{target_name}: {}",
            diagnostic_text(&output)
        )));
    }

    let check = control_operation(ssh, control_path, "check", timeout)?;
    if check.exit_code != Some(0) {
        let _ = fs::remove_file(control_path);
        return Err(Error::Tool(format!(
            "shared SSH control master for ssh:{target_name} did not become ready: {}",
            diagnostic_text(&check)
        )));
    }
    Ok(())
}

fn stop_control_master(master: &SshControlMaster, timeout: Duration) -> Result<RawExecOutput> {
    if !master.control_path.exists() {
        return Ok(RawExecOutput {
            exit_code: Some(0),
            stdout: b"shared SSH control master was not running\n".to_vec(),
            stderr: Vec::new(),
            timed_out: false,
        });
    }

    let output = control_operation(&master.ssh, &master.control_path, "exit", timeout)?;
    let _ = fs::remove_file(&master.control_path);
    Ok(output)
}

fn control_operation(
    ssh: &SshTargetConfig,
    control_path: &Path,
    operation: &str,
    timeout: Duration,
) -> Result<RawExecOutput> {
    let mut args = vec![
        "-S".to_string(),
        control_path.display().to_string(),
        "-O".to_string(),
        operation.to_string(),
    ];
    args.extend(base_args(ssh));
    args.push(destination(ssh));
    run_ssh_management_command(args, timeout)
}

fn run_ssh_management_command(args: Vec<String>, timeout: Duration) -> Result<RawExecOutput> {
    let mut stdout_file = tempfile::tempfile()?;
    let mut stderr_file = tempfile::tempfile()?;
    let mut child = Command::new("ssh")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_file.try_clone()?))
        .stderr(Stdio::from(stderr_file.try_clone()?))
        .spawn()?;

    let started = Instant::now();
    let mut timed_out = false;
    let exit_code = loop {
        if let Some(status) = child.try_wait()? {
            break status.code();
        }
        if started.elapsed() >= timeout {
            timed_out = true;
            let _ = child.kill();
            break child.wait()?.code();
        }
        thread::sleep(Duration::from_millis(10));
    };

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    stdout_file.seek(SeekFrom::Start(0))?;
    stderr_file.seek(SeekFrom::Start(0))?;
    stdout_file.read_to_end(&mut stdout)?;
    stderr_file.read_to_end(&mut stderr)?;

    Ok(RawExecOutput {
        exit_code,
        stdout,
        stderr,
        timed_out,
    })
}

fn diagnostic_text(output: &RawExecOutput) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let text = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    if text.is_empty() {
        format!("ssh exited with {:?}", output.exit_code)
    } else {
        text.to_string()
    }
}

fn spawn_reader<R>(mut reader: R, tx: mpsc::Sender<Vec<u8>>)
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut scratch = [0_u8; 8192];
        loop {
            match reader.read(&mut scratch) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(scratch[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
}

fn drain_channel(rx: &Receiver<Vec<u8>>, buffer: &mut Vec<u8>) {
    while let Ok(chunk) = rx.try_recv() {
        buffer.extend_from_slice(&chunk);
    }
}

fn build_wrapper(
    script: &str,
    script_args: &[&str],
    stdout_prefix: &str,
    stderr_marker: &str,
) -> String {
    let mut invocation = format!("sh -c {} sh", shell_quote(script));
    for arg in script_args {
        invocation.push(' ');
        invocation.push_str(&shell_quote(arg));
    }

    format!(
        "\n{invocation}\n__mcp_status=$?\nprintf '%s%s%s\\n' {} \"$__mcp_status\" __\nprintf '%s\\n' {} >&2\n",
        shell_quote(stdout_prefix),
        shell_quote(stderr_marker.trim_end())
    )
}

fn take_stdout_until_marker(
    buffer: &mut Vec<u8>,
    prefix: &[u8],
    suffix: &[u8],
) -> Result<Option<(i32, Vec<u8>)>> {
    let Some(start) = find_subsequence(buffer, prefix) else {
        return Ok(None);
    };
    let status_start = start + prefix.len();
    let Some(relative_end) = find_subsequence(&buffer[status_start..], suffix) else {
        return Ok(None);
    };
    let status_end = status_start + relative_end;
    let status_text = String::from_utf8_lossy(&buffer[status_start..status_end]);
    let exit_code = status_text.trim().parse::<i32>().map_err(|err| {
        Error::Tool(format!(
            "persistent ssh worker returned invalid exit marker {status_text:?}: {err}"
        ))
    })?;
    let output = buffer[..start].to_vec();
    buffer.drain(..status_end + suffix.len());
    Ok(Some((exit_code, output)))
}

fn take_until_marker(buffer: &mut Vec<u8>, marker: &[u8]) -> Option<Vec<u8>> {
    let start = find_subsequence(buffer, marker)?;
    let output = buffer[..start].to_vec();
    buffer.drain(..start + marker.len());
    Some(output)
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn marker_prefix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("{}_{}", std::process::id(), nanos)
}
