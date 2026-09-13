mod files;
mod session;

pub use self::files::{
    chmod_path, create_directory, file_exists, file_mode, list_dir, move_path, read_file,
    remove_file, write_file,
};
pub use self::session::SshSessionRegistry;

use crate::{
    core::{config::SshTargetConfig, error::Result, util::shell_quote},
    tooling::exec::RawExecOutput,
};
use std::{collections::BTreeMap, time::Duration};

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

pub fn exec_program_and_args(
    ssh: &SshTargetConfig,
    command: &str,
    cwd: Option<&str>,
    env: &BTreeMap<String, String>,
) -> (String, Vec<String>) {
    let mut args = base_args(ssh);
    args.push(destination(ssh));
    let remote_command = with_cwd_and_env(command, cwd, env);
    args.push(format!("sh -lc {}", shell_quote(&remote_command)));
    ("ssh".to_string(), args)
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

pub(super) fn run_script(
    sessions: &SshSessionRegistry,
    target_name: &str,
    ssh: &SshTargetConfig,
    script: &str,
    script_args: &[&str],
    timeout: Duration,
) -> Result<RawExecOutput> {
    sessions.run_script(target_name, ssh, script, script_args, timeout)
}

fn with_cwd_and_env(command: &str, cwd: Option<&str>, env: &BTreeMap<String, String>) -> String {
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
