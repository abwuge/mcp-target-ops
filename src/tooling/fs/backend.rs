use super::FileEntry;
use crate::{
    core::{
        config::TargetConfig,
        error::{Error, Result},
        state::AppState,
        target::TargetId,
    },
    transport::ssh,
};
use serde_json::json;
use std::{
    fs,
    io::Write,
    path::Path,
    time::{Duration, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

pub(crate) fn read_bytes(
    state: &AppState,
    target: &TargetId,
    config: &TargetConfig,
    path: &str,
    timeout: Duration,
) -> Result<Vec<u8>> {
    match (target, config) {
        (TargetId::Local, TargetConfig::Local(_)) => Ok(fs::read(path)?),
        (TargetId::Ssh(name), TargetConfig::Ssh(ssh_config)) => {
            ssh::read_file(&state.ssh_sessions, name, ssh_config, path, timeout)
        }
        _ => Err(Error::Target(format!(
            "target {target} has mismatched config"
        ))),
    }
}

pub(crate) fn write_bytes(
    state: &AppState,
    target: &TargetId,
    config: &TargetConfig,
    path: &str,
    bytes: &[u8],
    mode: Option<u32>,
    timeout: Duration,
) -> Result<()> {
    match (target, config) {
        (TargetId::Local, TargetConfig::Local(_)) => write_local_atomic(path, bytes, mode),
        (TargetId::Ssh(name), TargetConfig::Ssh(ssh_config)) => ssh::write_file(
            &state.ssh_sessions,
            name,
            ssh_config,
            path,
            bytes,
            mode,
            timeout,
        ),
        _ => Err(Error::Target(format!(
            "target {target} has mismatched config"
        ))),
    }
}

pub(crate) fn file_mode(
    state: &AppState,
    target: &TargetId,
    config: &TargetConfig,
    path: &str,
    timeout: Duration,
) -> Result<Option<u32>> {
    match (target, config) {
        (TargetId::Local, TargetConfig::Local(_)) => {
            #[cfg(unix)]
            {
                Ok(Some(fs::metadata(path)?.permissions().mode() & 0o7777))
            }
            #[cfg(not(unix))]
            {
                let _ = path;
                Ok(None)
            }
        }
        (TargetId::Ssh(name), TargetConfig::Ssh(ssh_config)) => {
            ssh::file_mode(&state.ssh_sessions, name, ssh_config, path, timeout)
        }
        _ => Err(Error::Target(format!(
            "target {target} has mismatched config"
        ))),
    }
}

pub(crate) fn file_exists(
    state: &AppState,
    target: &TargetId,
    config: &TargetConfig,
    path: &str,
    timeout: Duration,
) -> Result<bool> {
    match (target, config) {
        (TargetId::Local, TargetConfig::Local(_)) => Ok(Path::new(path).exists()),
        (TargetId::Ssh(name), TargetConfig::Ssh(ssh_config)) => {
            ssh::file_exists(&state.ssh_sessions, name, ssh_config, path, timeout)
        }
        _ => Err(Error::Target(format!(
            "target {target} has mismatched config"
        ))),
    }
}

fn write_local_atomic(path: &str, bytes: &[u8], mode: Option<u32>) -> Result<()> {
    let path = Path::new(path);
    let parent = path
        .parent()
        .ok_or_else(|| Error::Tool(format!("path {} has no parent", path.display())))?;
    fs::create_dir_all(parent)?;
    let existing_permissions = fs::metadata(path).ok().map(|meta| meta.permissions());
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(bytes)?;
    tmp.flush()?;

    #[cfg(unix)]
    {
        let selected_mode =
            mode.or_else(|| existing_permissions.as_ref().map(PermissionsExt::mode));
        let selected_mode = selected_mode.unwrap_or(0o644);
        tmp.as_file()
            .set_permissions(fs::Permissions::from_mode(selected_mode))?;
    }
    #[cfg(not(unix))]
    if let Some(permissions) = existing_permissions {
        tmp.as_file().set_permissions(permissions)?;
    }

    tmp.persist(path).map_err(|err| Error::Io(err.error))?;
    Ok(())
}

pub(super) fn list_entries(
    state: &AppState,
    target: &TargetId,
    config: &TargetConfig,
    path: &str,
    timeout: Duration,
) -> Result<Vec<FileEntry>> {
    match (target, config) {
        (TargetId::Local, TargetConfig::Local(_)) => list_local(path),
        (TargetId::Ssh(name), TargetConfig::Ssh(ssh_config)) => {
            list_remote(state, name, ssh_config, path, timeout)
        }
        _ => Err(Error::Target(format!(
            "target {target} has mismatched config"
        ))),
    }
}

fn list_local(path: &str) -> Result<Vec<FileEntry>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let meta = fs::symlink_metadata(entry.path())?;
        let kind = if meta.is_dir() {
            "dir"
        } else if meta.is_file() {
            "file"
        } else if meta.file_type().is_symlink() {
            "symlink"
        } else {
            "other"
        };
        let modified_unix = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs());
        entries.push(FileEntry {
            name: entry.file_name().to_string_lossy().to_string(),
            path: entry.path().display().to_string(),
            kind: kind.to_string(),
            size: meta.len(),
            modified_unix,
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

fn list_remote(
    state: &AppState,
    target_name: &str,
    ssh_config: &crate::core::config::SshTargetConfig,
    path: &str,
    timeout: Duration,
) -> Result<Vec<FileEntry>> {
    let value = ssh::list_dir(&state.ssh_sessions, target_name, ssh_config, path, timeout)?;
    let entries = value.get("entries").cloned().unwrap_or_else(|| json!([]));
    serde_json::from_value::<Vec<FileEntry>>(entries).map_err(Error::Json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn atomic_write_preserves_existing_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("script.sh");
        fs::write(&path, b"old\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        write_local_atomic(path.to_str().unwrap(), b"new\n", None).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
    }
}
