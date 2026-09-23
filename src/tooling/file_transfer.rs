use crate::{
    core::{
        config::{SshTargetConfig, TargetConfig},
        error::{Error, Result},
        policy::{self, FileAccess},
        state::AppState,
        target::{TargetId, TargetSource},
    },
    tooling::fs::{self, FileMoveRequest},
    transport::ssh,
};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use std::{
    fs as std_fs,
    io::{Read, Seek, SeekFrom},
    path::Path,
    process::{Command, Stdio},
    str::FromStr,
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Deserialize)]
pub struct FileTransferRequest {
    pub source_target: String,
    pub source_path: String,
    pub destination_target: String,
    pub destination_path: String,
    #[serde(default)]
    pub overwrite: bool,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileTransferResponse {
    pub source_target: String,
    pub source_path: String,
    pub destination_target: String,
    pub destination_path: String,
    pub method: String,
    pub direction: String,
    pub direct: bool,
    pub transferred: bool,
}

struct ProcessOutput {
    exit_code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    timed_out: bool,
}

struct AttemptFailure {
    method: &'static str,
    direction: &'static str,
    diagnostic: String,
}

pub fn transfer(state: &AppState, req: FileTransferRequest) -> Result<FileTransferResponse> {
    let source_target = TargetId::from_str(&req.source_target)?;
    let destination_target = TargetId::from_str(&req.destination_target)?;
    if source_target == destination_target {
        return Err(Error::Tool(
            "file_transfer is only for different targets; use file_move within one target"
                .to_string(),
        ));
    }

    let source_config = state.get_target_config(&source_target)?;
    let destination_config = state.get_target_config(&destination_target)?;

    policy::check_file(
        &source_target,
        source_config,
        &req.source_path,
        FileAccess::Read,
        TargetSource::Explicit,
    )?;
    policy::check_file(
        &destination_target,
        destination_config,
        &req.destination_path,
        FileAccess::Write,
        TargetSource::Explicit,
    )?;

    let timeout_ms = req.timeout_ms.unwrap_or_else(|| {
        policy::target_policy(source_config)
            .default_timeout_ms
            .max(policy::target_policy(destination_config).default_timeout_ms)
    });
    let timeout = Duration::from_millis(timeout_ms);

    ensure_regular_source(
        state,
        &source_target,
        source_config,
        &req.source_path,
        timeout,
    )?;

    let destination_exists = fs::file_exists(
        state,
        &destination_target,
        destination_config,
        &req.destination_path,
        timeout,
    )?;
    if destination_exists && !req.overwrite {
        return Err(Error::Tool(
            "destination already exists; set overwrite=true to replace it".to_string(),
        ));
    }

    let destination_staging = sibling_staging_path(&req.destination_path)?;
    policy::check_file(
        &destination_target,
        destination_config,
        &destination_staging,
        FileAccess::Write,
        TargetSource::Explicit,
    )?;

    let result = match (
        &source_target,
        source_config,
        &destination_target,
        destination_config,
    ) {
        (
            TargetId::Local,
            TargetConfig::Local(_),
            TargetId::Ssh(dst_name),
            TargetConfig::Ssh(dst),
        ) => transfer_local_ssh(
            state,
            true,
            dst_name,
            dst,
            &req.source_path,
            &destination_staging,
            timeout,
        ),
        (
            TargetId::Ssh(src_name),
            TargetConfig::Ssh(src),
            TargetId::Local,
            TargetConfig::Local(_),
        ) => transfer_local_ssh(
            state,
            false,
            src_name,
            src,
            &req.source_path,
            &destination_staging,
            timeout,
        ),
        (
            TargetId::Ssh(src_name),
            TargetConfig::Ssh(src),
            TargetId::Ssh(dst_name),
            TargetConfig::Ssh(dst),
        ) => transfer_ssh_to_ssh_direct(
            state,
            src_name,
            src,
            &req.source_path,
            dst_name,
            dst,
            &destination_staging,
            timeout,
        ),
        _ => Err(Error::Target(
            "source or destination target has mismatched configuration".to_string(),
        )),
    };

    let (method, direction, direct) = match result {
        Ok(value) => value,
        Err(err) => {
            cleanup_staging(
                state,
                &destination_target,
                destination_config,
                &destination_staging,
                timeout,
            );
            return Err(err);
        }
    };

    if let Err(err) = fs::move_path(
        state,
        FileMoveRequest {
            target: Some(req.destination_target.clone()),
            source: destination_staging.clone(),
            destination: req.destination_path.clone(),
            overwrite: req.overwrite,
            timeout_ms: Some(timeout_ms),
        },
    ) {
        cleanup_staging(
            state,
            &destination_target,
            destination_config,
            &destination_staging,
            timeout,
        );
        return Err(err);
    }

    Ok(FileTransferResponse {
        source_target: req.source_target,
        source_path: req.source_path,
        destination_target: req.destination_target,
        destination_path: req.destination_path,
        method,
        direction,
        direct,
        transferred: true,
    })
}

fn transfer_local_ssh(
    state: &AppState,
    local_is_source: bool,
    remote_name: &str,
    remote: &SshTargetConfig,
    source_path: &str,
    destination_staging: &str,
    timeout: Duration,
) -> Result<(String, String, bool)> {
    let remote_shell = ssh::rsync_shell_command(&state.ssh_sessions, remote_name, remote, timeout)?;
    let remote_host = remote_host(remote);
    let (source, destination, direction) = if local_is_source {
        (
            source_path.to_string(),
            format!("{remote_host}:{destination_staging}"),
            "push",
        )
    } else {
        (
            format!("{remote_host}:{source_path}"),
            destination_staging.to_string(),
            "pull",
        )
    };

    let rsync_args = vec![
        "-a".to_string(),
        "--protect-args".to_string(),
        "--quiet".to_string(),
        "-e".to_string(),
        remote_shell,
        "--".to_string(),
        source.clone(),
        destination.clone(),
    ];

    match run_process("rsync", &rsync_args, timeout) {
        Ok(output) if !output.timed_out && output.exit_code == Some(0) => {
            return Ok(("rsync".to_string(), direction.to_string(), true));
        }
        Ok(output) if output.timed_out => {
            return Err(Error::Tool("rsync file transfer timed out".to_string()));
        }
        Ok(output) if !rsync_is_unsupported(&output) => {
            return Err(Error::Tool(format!(
                "rsync file transfer failed: {}",
                diagnostic_text(&output)
            )));
        }
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
            return Err(Error::Io(err));
        }
        _ => {}
    }

    let mut scp_args = ssh::scp_client_args(&state.ssh_sessions, remote_name, remote, timeout)?;
    scp_args.push("-B".to_string());
    scp_args.push("-q".to_string());
    scp_args.push("-p".to_string());
    scp_args.push(source);
    scp_args.push(destination);

    let output = run_process("scp", &scp_args, timeout).map_err(Error::Io)?;
    if output.timed_out {
        return Err(Error::Tool(
            "scp fallback file transfer timed out".to_string(),
        ));
    }
    if output.exit_code != Some(0) {
        return Err(Error::Tool(format!(
            "scp fallback file transfer failed: {}",
            diagnostic_text(&output)
        )));
    }

    Ok(("scp".to_string(), direction.to_string(), true))
}

#[allow(clippy::too_many_arguments)]
fn transfer_ssh_to_ssh_direct(
    state: &AppState,
    source_name: &str,
    source: &SshTargetConfig,
    source_path: &str,
    destination_name: &str,
    destination: &SshTargetConfig,
    destination_staging: &str,
    timeout: Duration,
) -> Result<(String, String, bool)> {
    let mut failures = Vec::new();

    for (method, direction) in [
        ("rsync", "push"),
        ("rsync", "pull"),
        ("scp", "push"),
        ("scp", "pull"),
    ] {
        cleanup_remote_staging(
            state,
            destination_name,
            destination,
            destination_staging,
            timeout,
        );

        let attempt = match (method, direction) {
            ("rsync", "push") => remote_rsync(
                state,
                source_name,
                source,
                source_path,
                destination,
                destination_staging,
                true,
                timeout,
            ),
            ("rsync", "pull") => remote_rsync(
                state,
                destination_name,
                destination,
                destination_staging,
                source,
                source_path,
                false,
                timeout,
            ),
            ("scp", "push") => remote_scp(
                state,
                source_name,
                source,
                source_path,
                destination,
                destination_staging,
                true,
                timeout,
            ),
            ("scp", "pull") => remote_scp(
                state,
                destination_name,
                destination,
                destination_staging,
                source,
                source_path,
                false,
                timeout,
            ),
            _ => unreachable!(),
        };

        match attempt {
            Ok(()) => {
                return Ok((method.to_string(), direction.to_string(), true));
            }
            Err(diagnostic) => failures.push(AttemptFailure {
                method,
                direction,
                diagnostic,
            }),
        }
    }

    let summary = failures
        .into_iter()
        .map(|failure| {
            format!(
                "{} {}: {}",
                failure.method, failure.direction, failure.diagnostic
            )
        })
        .collect::<Vec<_>>()
        .join("; ");

    Err(Error::Tool(format!(
        "direct SSH-to-SSH transfer failed without relaying file data through the Target Ops host: {summary}"
    )))
}

#[allow(clippy::too_many_arguments)]
fn remote_rsync(
    state: &AppState,
    initiator_name: &str,
    initiator: &SshTargetConfig,
    local_path: &str,
    peer: &SshTargetConfig,
    peer_path: &str,
    push: bool,
    timeout: Duration,
) -> std::result::Result<(), String> {
    let peer_host = remote_host(peer);
    let remote_shell = remote_shell_command(peer, timeout);
    let push_flag = if push { "1" } else { "0" };
    let script = r#"
local_path=$1
peer_path=$2
peer_host=$3
remote_shell=$4
push=$5
if ! command -v rsync >/dev/null 2>&1; then
    printf '%s\n' 'rsync is not installed on transfer initiator' >&2
    exit 127
fi
if [ "$push" = 1 ]; then
    exec rsync -a --protect-args --quiet -e "$remote_shell" -- "$local_path" "$peer_host:$peer_path"
else
    exec rsync -a --protect-args --quiet -e "$remote_shell" -- "$peer_host:$peer_path" "$local_path"
fi
"#;

    let output = ssh::run_script(
        &state.ssh_sessions,
        initiator_name,
        initiator,
        script,
        &[
            local_path,
            peer_path,
            peer_host.as_str(),
            remote_shell.as_str(),
            push_flag,
        ],
        timeout,
    )
    .map_err(|err| err.to_string())?;

    if output.timed_out {
        return Err("timed out".to_string());
    }
    if output.exit_code == Some(0) {
        return Ok(());
    }
    Err(raw_diagnostic(&output))
}

#[allow(clippy::too_many_arguments)]
fn remote_scp(
    state: &AppState,
    initiator_name: &str,
    initiator: &SshTargetConfig,
    local_path: &str,
    peer: &SshTargetConfig,
    peer_path: &str,
    push: bool,
    timeout: Duration,
) -> std::result::Result<(), String> {
    let peer_host = remote_host(peer);
    let peer_port = peer.port.to_string();
    let push_flag = if push { "1" } else { "0" };
    let connect_timeout = timeout.as_secs().clamp(1, 600).to_string();
    let script = r#"
local_path=$1
peer_path=$2
peer_host=$3
peer_port=$4
push=$5
connect_timeout=$6
if ! command -v scp >/dev/null 2>&1; then
    printf '%s\n' 'scp is not installed on transfer initiator' >&2
    exit 127
fi
if [ "$push" = 1 ]; then
    exec scp -B -q -p -o "ConnectTimeout=$connect_timeout" -P "$peer_port" -- "$local_path" "$peer_host:$peer_path"
else
    exec scp -B -q -p -o "ConnectTimeout=$connect_timeout" -P "$peer_port" -- "$peer_host:$peer_path" "$local_path"
fi
"#;

    let output = ssh::run_script(
        &state.ssh_sessions,
        initiator_name,
        initiator,
        script,
        &[
            local_path,
            peer_path,
            peer_host.as_str(),
            peer_port.as_str(),
            push_flag,
            connect_timeout.as_str(),
        ],
        timeout,
    )
    .map_err(|err| err.to_string())?;

    if output.timed_out {
        return Err("timed out".to_string());
    }
    if output.exit_code == Some(0) {
        return Ok(());
    }
    Err(raw_diagnostic(&output))
}

fn remote_shell_command(peer: &SshTargetConfig, timeout: Duration) -> String {
    let connect_timeout = timeout.as_secs().clamp(1, 600);
    let mut parts = vec![
        "ssh".to_string(),
        "-o".to_string(),
        "BatchMode=yes".to_string(),
        "-o".to_string(),
        format!("ConnectTimeout={connect_timeout}"),
        "-p".to_string(),
        peer.port.to_string(),
    ];
    parts
        .drain(..)
        .map(|part| crate::core::util::shell_quote(&part))
        .collect::<Vec<_>>()
        .join(" ")
}

fn ensure_regular_source(
    state: &AppState,
    target: &TargetId,
    config: &TargetConfig,
    path: &str,
    timeout: Duration,
) -> Result<()> {
    let regular = match (target, config) {
        (TargetId::Local, TargetConfig::Local(_)) => std_fs::metadata(path)
            .map(|metadata| metadata.is_file())
            .map_err(Error::Io)?,
        (TargetId::Ssh(name), TargetConfig::Ssh(ssh_config)) => {
            ssh::is_regular_file(&state.ssh_sessions, name, ssh_config, path, timeout)?
        }
        _ => {
            return Err(Error::Target(format!(
                "target {target} has mismatched config"
            )));
        }
    };

    if regular {
        Ok(())
    } else {
        Err(Error::Tool(format!(
            "source path is not a regular file: {path}"
        )))
    }
}

fn remote_host(ssh: &SshTargetConfig) -> String {
    let host = if ssh.host.contains(':') && !(ssh.host.starts_with('[') && ssh.host.ends_with(']'))
    {
        format!("[{}]", ssh.host)
    } else {
        ssh.host.clone()
    };
    match ssh.user.as_deref() {
        Some(user) if !user.is_empty() => format!("{user}@{host}"),
        _ => host,
    }
}

fn sibling_staging_path(destination: &str) -> Result<String> {
    let path = Path::new(destination);
    let parent = path
        .parent()
        .ok_or_else(|| Error::Tool(format!("destination path has no parent: {destination}")))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| Error::Tool(format!("destination path has no file name: {destination}")))?;

    let mut random = [0_u8; 12];
    OsRng.fill_bytes(&mut random);
    let suffix = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let staging = parent.join(format!(".{file_name}.target-ops-transfer-{suffix}"));
    staging
        .to_str()
        .map(ToString::to_string)
        .ok_or_else(|| Error::Tool("destination staging path is not valid UTF-8".to_string()))
}

fn cleanup_staging(
    state: &AppState,
    target: &TargetId,
    config: &TargetConfig,
    path: &str,
    timeout: Duration,
) {
    match (target, config) {
        (TargetId::Local, TargetConfig::Local(_)) => {
            let _ = std_fs::remove_file(path);
        }
        (TargetId::Ssh(name), TargetConfig::Ssh(ssh_config)) => {
            let _ = ssh::remove_file(&state.ssh_sessions, name, ssh_config, path, timeout);
        }
        _ => {}
    }
}

fn cleanup_remote_staging(
    state: &AppState,
    target_name: &str,
    target: &SshTargetConfig,
    path: &str,
    timeout: Duration,
) {
    let _ = ssh::remove_file(&state.ssh_sessions, target_name, target, path, timeout);
}

fn run_process(
    program: &str,
    args: &[String],
    timeout: Duration,
) -> std::io::Result<ProcessOutput> {
    let mut stdout_file = tempfile::tempfile()?;
    let mut stderr_file = tempfile::tempfile()?;
    let mut child = Command::new(program)
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

    stdout_file.seek(SeekFrom::Start(0))?;
    stderr_file.seek(SeekFrom::Start(0))?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    stdout_file.read_to_end(&mut stdout)?;
    stderr_file.read_to_end(&mut stderr)?;

    Ok(ProcessOutput {
        exit_code,
        stdout,
        stderr,
        timed_out,
    })
}

fn rsync_is_unsupported(output: &ProcessOutput) -> bool {
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    )
    .to_ascii_lowercase();

    output.exit_code == Some(127)
        || text.contains("rsync: command not found")
        || text.contains("rsync: not found")
        || text.contains("failed to exec rsync")
        || text.contains("unknown option")
        || text.contains("unrecognized option")
}

fn diagnostic_text(output: &ProcessOutput) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let selected = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    if selected.is_empty() {
        format!("process exited with {:?}", output.exit_code)
    } else {
        selected.chars().take(4096).collect()
    }
}

fn raw_diagnostic(output: &crate::tooling::exec::RawExecOutput) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let selected = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    if selected.is_empty() {
        format!("remote process exited with {:?}", output.exit_code)
    } else {
        selected.chars().take(4096).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staging_path_stays_next_to_destination() {
        let path = sibling_staging_path("/tmp/out.bin").unwrap();
        assert!(path.starts_with("/tmp/.out.bin.target-ops-transfer-"));
    }

    #[test]
    fn remote_host_brackets_ipv6() {
        let ssh = SshTargetConfig {
            enabled: true,
            host: "2001:db8::1".to_string(),
            port: 22,
            user: Some("alice".to_string()),
            identity_file: None,
            control_master: true,
            control_persist_secs: 60,
            extra_args: Vec::new(),
            shell: None,
            policy: Default::default(),
        };
        assert_eq!(remote_host(&ssh), "alice@[2001:db8::1]");
    }

    #[test]
    fn detects_missing_remote_rsync() {
        let output = ProcessOutput {
            exit_code: Some(12),
            stdout: Vec::new(),
            stderr: b"sh: rsync: command not found\n".to_vec(),
            timed_out: false,
        };
        assert!(rsync_is_unsupported(&output));
    }
}
