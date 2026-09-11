use crate::{
    core::{
        config::SshTargetConfig,
        error::{Error, Result},
        util::shell_quote,
    },
    tooling::exec::RawExecOutput,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::{Read, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        mpsc::{self, Receiver},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub struct SshSessionRegistry {
    sessions: Mutex<HashMap<String, Arc<SshSession>>>,
}

impl SshSessionRegistry {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }

    pub fn ids(&self) -> Vec<String> {
        let mut ids: Vec<_> = self.sessions.lock().unwrap().keys().cloned().collect();
        ids.sort();
        ids
    }

    fn get_or_start(&self, target_name: &str, ssh: &SshTargetConfig) -> Result<Arc<SshSession>> {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(session) = sessions.get(target_name) {
            if !session.is_closed() {
                return Ok(Arc::clone(session));
            }
        }

        let session = Arc::new(SshSession::start(ssh)?);
        sessions.insert(target_name.to_string(), Arc::clone(&session));
        Ok(session)
    }

    fn run_script(
        &self,
        target_name: &str,
        ssh: &SshTargetConfig,
        script: &str,
        script_args: &[&str],
        timeout: Duration,
    ) -> Result<RawExecOutput> {
        let session = self.get_or_start(target_name, ssh)?;
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

    fn disconnect(&self, target_name: &str, timeout: Duration) -> Result<RawExecOutput> {
        let session = self.sessions.lock().unwrap().remove(target_name);
        match session {
            Some(session) => session.shutdown(timeout),
            None => Ok(RawExecOutput {
                exit_code: Some(0),
                stdout: b"persistent ssh worker was not running\n".to_vec(),
                stderr: Vec::new(),
                timed_out: false,
            }),
        }
    }
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
    fn start(ssh: &SshTargetConfig) -> Result<Self> {
        let mut args = base_args(ssh);
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

pub fn connect(
    sessions: &SshSessionRegistry,
    target_name: &str,
    ssh: &SshTargetConfig,
    timeout: Duration,
) -> Result<RawExecOutput> {
    sessions.run_script(target_name, ssh, "true", &[], timeout)
}

pub fn disconnect(
    sessions: &SshSessionRegistry,
    target_name: &str,
    timeout: Duration,
) -> Result<RawExecOutput> {
    sessions.disconnect(target_name, timeout)
}

pub fn exec_with_env(
    sessions: &SshSessionRegistry,
    target_name: &str,
    ssh: &SshTargetConfig,
    command: &str,
    cwd: Option<&str>,
    env: &std::collections::BTreeMap<String, String>,
    timeout: Duration,
) -> Result<RawExecOutput> {
    let remote_command = with_cwd_and_env(command, cwd, env);
    sessions.run_script(target_name, ssh, &remote_command, &[], timeout)
}

pub fn exec_program_and_args(
    ssh: &SshTargetConfig,
    command: &str,
    cwd: Option<&str>,
    env: &std::collections::BTreeMap<String, String>,
) -> (String, Vec<String>) {
    let mut args = base_args(ssh);
    args.push(destination(ssh));
    let remote_command = with_cwd_and_env(command, cwd, env);
    args.push(format!("sh -lc {}", shell_quote(&remote_command)));
    ("ssh".to_string(), args)
}

pub fn read_file(
    sessions: &SshSessionRegistry,
    target_name: &str,
    ssh: &SshTargetConfig,
    path: &str,
    timeout: Duration,
) -> Result<Vec<u8>> {
    let output = remote_sh(
        sessions,
        target_name,
        ssh,
        r#"cat < "$1""#,
        &[path],
        timeout,
    )?;
    if output.exit_code == Some(0) {
        Ok(output.stdout)
    } else {
        Err(Error::Tool(format!(
            "remote read failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

pub fn write_file(
    sessions: &SshSessionRegistry,
    target_name: &str,
    ssh: &SshTargetConfig,
    path: &str,
    bytes: &[u8],
    mode: Option<u32>,
    timeout: Duration,
) -> Result<()> {
    let mode_arg = mode.map(|value| format!("{value:o}")).unwrap_or_default();
    let mut script = r#"
p=$1
requested_mode=$2
parent=${p%/*}
if [ "$parent" = "$p" ]; then
    parent=.
fi
if [ -z "$parent" ]; then
    parent=/
fi
base=${p##*/}
if [ -z "$base" ]; then
    printf "%s\n" "refusing to write directory path: $p" >&2
    exit 1
fi
if [ -n "$requested_mode" ]; then
    final_mode=$requested_mode
elif [ -e "$p" ] || [ -L "$p" ]; then
    final_mode=$(stat -c "%a" "$p" 2>/dev/null || stat -f "%Lp" "$p" 2>/dev/null || printf "")
else
    final_mode=644
fi
mkdir -p "$parent" || exit 1
tmp=$(mktemp "$parent/.$base.XXXXXX") || exit 1
cleanup() {
    rm -f "$tmp"
}
trap cleanup EXIT HUP INT TERM
: > "$tmp" || exit 1
"#
    .to_string();
    append_printf_chunks(&mut script, bytes);
    script.push_str(
        r#"
if [ -n "$final_mode" ]; then
    chmod "$final_mode" "$tmp" || exit 1
fi
mv -f "$tmp" "$p" || exit 1
trap - EXIT HUP INT TERM
"#,
    );
    let output = remote_sh(
        sessions,
        target_name,
        ssh,
        &script,
        &[path, mode_arg.as_str()],
        timeout,
    )?;
    if output.exit_code == Some(0) {
        Ok(())
    } else {
        Err(Error::Tool(format!(
            "remote write failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

pub fn file_exists(
    sessions: &SshSessionRegistry,
    target_name: &str,
    ssh: &SshTargetConfig,
    path: &str,
    timeout: Duration,
) -> Result<bool> {
    let output = remote_sh(
        sessions,
        target_name,
        ssh,
        r#"if [ -e "$1" ] || [ -L "$1" ]; then exit 0; else exit 1; fi"#,
        &[path],
        timeout,
    )?;
    match output.exit_code {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(Error::Tool(format!(
            "remote existence check failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ))),
    }
}

pub fn move_path(
    sessions: &SshSessionRegistry,
    target_name: &str,
    ssh: &SshTargetConfig,
    source: &str,
    destination: &str,
    overwrite: bool,
    timeout: Duration,
) -> Result<()> {
    let overwrite_arg = if overwrite { "1" } else { "0" };
    let script = r#"
src=$1
dst=$2
overwrite=$3
if [ "$overwrite" != 1 ] && { [ -e "$dst" ] || [ -L "$dst" ]; }; then
    printf "%s\n" "destination already exists: $dst" >&2
    exit 2
fi
if [ "$overwrite" = 1 ]; then
    if [ -d "$dst" ] && [ ! -L "$dst" ]; then
        printf "%s\n" "refusing to overwrite an existing directory: $dst" >&2
        exit 2
    fi
    rm -f "$dst" || exit 1
fi
mv "$src" "$dst" || exit 1
"#;
    let output = remote_sh(
        sessions,
        target_name,
        ssh,
        script,
        &[source, destination, overwrite_arg],
        timeout,
    )?;
    if output.exit_code == Some(0) {
        Ok(())
    } else {
        Err(Error::Tool(format!(
            "remote move failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

pub fn chmod_path(
    sessions: &SshSessionRegistry,
    target_name: &str,
    ssh: &SshTargetConfig,
    path: &str,
    mode: u32,
    timeout: Duration,
) -> Result<()> {
    let mode = format!("{mode:o}");
    let output = remote_sh(
        sessions,
        target_name,
        ssh,
        r#"chmod "$2" "$1""#,
        &[path, mode.as_str()],
        timeout,
    )?;
    if output.exit_code == Some(0) {
        Ok(())
    } else {
        Err(Error::Tool(format!(
            "remote chmod failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

pub fn create_directory(
    sessions: &SshSessionRegistry,
    target_name: &str,
    ssh: &SshTargetConfig,
    path: &str,
    recursive: bool,
    mode: Option<u32>,
    timeout: Duration,
) -> Result<()> {
    let recursive_arg = if recursive { "1" } else { "0" };
    let mode_arg = mode.map(|value| format!("{value:o}")).unwrap_or_default();
    let script = r#"
p=$1
recursive=$2
mode=$3
if [ "$recursive" = 1 ]; then
    mkdir -p "$p" || exit 1
else
    mkdir "$p" || exit 1
fi
if [ -n "$mode" ]; then
    chmod "$mode" "$p" || exit 1
fi
"#;
    let output = remote_sh(
        sessions,
        target_name,
        ssh,
        script,
        &[path, recursive_arg, mode_arg.as_str()],
        timeout,
    )?;
    if output.exit_code == Some(0) {
        Ok(())
    } else {
        Err(Error::Tool(format!(
            "remote directory creation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

pub fn list_dir(
    sessions: &SshSessionRegistry,
    target_name: &str,
    ssh: &SshTargetConfig,
    path: &str,
    timeout: Duration,
) -> Result<Value> {
    let script = r#"
dir=$1
if [ ! -d "$dir" ]; then
    printf "%s\n" "not a directory: $dir" >&2
    exit 1
fi
for child in "$dir"/* "$dir"/.[!.]* "$dir"/..?*; do
    [ -e "$child" ] || [ -L "$child" ] || continue
    if [ -L "$child" ]; then
        kind=symlink
    elif [ -d "$child" ]; then
        kind=dir
    elif [ -f "$child" ]; then
        kind=file
    else
        kind=other
    fi
    size=$(stat -c "%s" "$child" 2>/dev/null || stat -f "%z" "$child" 2>/dev/null || printf 0)
    modified=$(stat -c "%Y" "$child" 2>/dev/null || stat -f "%m" "$child" 2>/dev/null || printf "")
    name=${child##*/}
    printf "%s\000%s\000%s\000%s\000%s\000" "$name" "$child" "$kind" "$size" "$modified"
done
"#;
    let output = remote_sh(sessions, target_name, ssh, script, &[path], timeout)?;
    if output.exit_code != Some(0) {
        return Err(Error::Tool(format!(
            "remote list failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    parse_list_dir_output(&output.stdout)
}

pub fn terminal_program_and_args(
    ssh: &SshTargetConfig,
    cwd: Option<&str>,
    shell: Option<&str>,
) -> (String, Vec<String>) {
    let mut args = base_args(ssh);
    args.push("-tt".to_string());
    args.push(destination(ssh));

    if cwd.is_some() || shell.is_some() {
        let shell = shell.or(ssh.shell.as_deref()).unwrap_or("${SHELL:-sh}");
        let mut remote = String::new();
        if let Some(cwd) = cwd {
            remote.push_str("cd ");
            remote.push_str(&shell_quote(cwd));
            remote.push_str(" && ");
        }
        remote.push_str("exec ");
        remote.push_str(shell);
        args.push(remote);
    }

    ("ssh".to_string(), args)
}

pub fn base_args(ssh: &SshTargetConfig) -> Vec<String> {
    let mut args = Vec::new();
    args.push("-p".to_string());
    args.push(ssh.port.to_string());

    if let Some(identity_file) = &ssh.identity_file {
        args.push("-i".to_string());
        args.push(identity_file.display().to_string());
    }

    args.extend(ssh.extra_args.clone());
    args
}

fn remote_sh(
    sessions: &SshSessionRegistry,
    target_name: &str,
    ssh: &SshTargetConfig,
    script: &str,
    script_args: &[&str],
    timeout: Duration,
) -> Result<RawExecOutput> {
    sessions.run_script(target_name, ssh, script, script_args, timeout)
}

fn append_printf_chunks(script: &mut String, bytes: &[u8]) {
    for chunk in bytes.chunks(4096) {
        let mut escaped = String::with_capacity(chunk.len() * 4);
        for byte in chunk {
            escaped.push('\\');
            escaped.push(char::from(b'0' + (byte >> 6)));
            escaped.push(char::from(b'0' + ((byte >> 3) & 0o7)));
            escaped.push(char::from(b'0' + (byte & 0o7)));
        }
        script.push_str("printf '%b' ");
        script.push_str(&shell_quote(&escaped));
        script.push_str(" >> \"$tmp\" || exit 1\n");
    }
}

fn parse_list_dir_output(stdout: &[u8]) -> Result<Value> {
    let mut fields: Vec<&[u8]> = stdout.split(|byte| *byte == 0).collect();
    if fields.last().is_some_and(|field| field.is_empty()) {
        fields.pop();
    }
    let (chunks, remainder) = fields.as_chunks::<5>();
    if !remainder.is_empty() {
        return Err(Error::Tool(format!(
            "remote list returned malformed metadata: expected groups of 5 fields, got {}",
            fields.len()
        )));
    }

    let mut entries = Vec::new();
    for field in chunks {
        let name = decode_field(field[0]);
        let path = decode_field(field[1]);
        let kind = decode_field(field[2]);
        let size_text = decode_field(field[3]);
        let modified_text = decode_field(field[4]);

        let size = size_text.trim().parse::<u64>().map_err(|err| {
            Error::Tool(format!(
                "remote list returned invalid size for {path}: {size_text:?}: {err}"
            ))
        })?;
        let modified_unix = if modified_text.trim().is_empty() {
            None
        } else {
            Some(modified_text.trim().parse::<u64>().map_err(|err| {
                Error::Tool(format!(
                    "remote list returned invalid mtime for {path}: {modified_text:?}: {err}"
                ))
            })?)
        };

        entries.push(json!({
            "name": name,
            "path": path,
            "kind": kind,
            "size": size,
            "modified_unix": modified_unix,
        }));
    }

    entries.sort_by(|left, right| {
        let left = left.get("name").and_then(Value::as_str).unwrap_or_default();
        let right = right
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        left.cmp(right)
    });

    Ok(json!({ "entries": entries }))
}

fn decode_field(field: &[u8]) -> String {
    String::from_utf8_lossy(field).to_string()
}

fn with_cwd_and_env(
    command: &str,
    cwd: Option<&str>,
    env: &std::collections::BTreeMap<String, String>,
) -> String {
    let mut remote = String::new();
    for (name, value) in env {
        remote.push_str(name);
        remote.push('=');
        remote.push_str(&shell_quote(value));
        remote.push_str("; export ");
        remote.push_str(name);
        remote.push_str("; ");
    }
    if let Some(cwd) = cwd {
        remote.push_str("cd ");
        remote.push_str(&shell_quote(cwd));
        remote.push_str(" && ");
    }
    remote.push_str(command);
    remote
}

fn destination(ssh: &SshTargetConfig) -> String {
    match &ssh.user {
        Some(user) if !user.is_empty() => format!("{user}@{}", ssh.host),
        _ => ssh.host.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_remote_list_nul_records() {
        let fields: [&[u8]; 10] = [
            b"b", b"/tmp/b", b"file", b"12", b"172", b"a", b"/tmp/a", b"dir", b"0", b"",
        ];
        let mut output = Vec::new();
        for field in fields {
            output.extend_from_slice(field);
            output.push(0);
        }

        let value = parse_list_dir_output(&output).unwrap();
        let entries = value["entries"].as_array().unwrap();

        assert_eq!(entries[0]["name"].as_str(), Some("a"));
        assert_eq!(entries[0]["modified_unix"], Value::Null);
        assert_eq!(entries[1]["name"].as_str(), Some("b"));
        assert_eq!(entries[1]["size"].as_u64(), Some(12));
        assert_eq!(entries[1]["modified_unix"].as_u64(), Some(172));
    }

    #[test]
    fn rejects_malformed_remote_list_records() {
        let err = parse_list_dir_output(b"name\0path\0").unwrap_err();
        assert!(err.to_string().contains("malformed metadata"));
    }
}
