use crate::{
    core::{
        config::TargetConfig,
        error::{Error, Result},
        state::AppState,
        target::TargetId,
    },
    transport::ssh,
};
use std::{fs, io::Write, path::Path, time::Duration};

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
