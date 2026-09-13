use super::{Config, PolicyConfig, TargetConfig};
use crate::core::{
    error::{Error, Result},
    target::TargetId,
};
use std::str::FromStr;

pub(super) fn validate(config: &Config) -> Result<()> {
    validate_runtime(config)?;
    validate_server(config)?;
    validate_targets(config)?;
    validate_mcp_servers(config)
}

fn validate_runtime(config: &Config) -> Result<()> {
    let runtime = &config.runtime;
    validate_nonzero(
        "runtime.result_cache_max_bytes",
        runtime.result_cache_max_bytes,
    )?;
    validate_nonzero("runtime.max_retained_jobs", runtime.max_retained_jobs)?;
    validate_nonzero(
        "runtime.max_retained_foreground_execs",
        runtime.max_retained_foreground_execs,
    )?;
    validate_nonzero(
        "runtime.exec_auto_background_after_ms",
        runtime.exec_auto_background_after_ms,
    )?;
    validate_nonzero(
        "runtime.stream_default_max_bytes",
        runtime.stream_default_max_bytes,
    )?;
    validate_nonzero("runtime.stream_max_bytes", runtime.stream_max_bytes)?;
    if runtime.stream_default_max_bytes > runtime.stream_max_bytes {
        return Err(Error::Config(
            "runtime.stream_default_max_bytes must not exceed runtime.stream_max_bytes".to_string(),
        ));
    }
    validate_nonzero(
        "runtime.job_wait_default_timeout_ms",
        runtime.job_wait_default_timeout_ms,
    )?;
    validate_nonzero(
        "runtime.job_wait_max_timeout_ms",
        runtime.job_wait_max_timeout_ms,
    )?;
    if runtime.job_wait_default_timeout_ms > runtime.job_wait_max_timeout_ms {
        return Err(Error::Config(
            "runtime.job_wait_default_timeout_ms must not exceed runtime.job_wait_max_timeout_ms"
                .to_string(),
        ));
    }
    validate_nonzero(
        "runtime.file_transfer_default_max_bytes",
        runtime.file_transfer_default_max_bytes,
    )?;
    if runtime.file_transfer_default_max_bytes > super::FILE_TRANSFER_HARD_MAX_BYTES {
        return Err(Error::Config(format!(
            "runtime.file_transfer_default_max_bytes must not exceed {}",
            super::FILE_TRANSFER_HARD_MAX_BYTES
        )));
    }
    validate_nonzero(
        "runtime.file_export_link_ttl_secs",
        runtime.file_export_link_ttl_secs,
    )?;
    if runtime.file_export_link_ttl_secs > super::FILE_EXPORT_LINK_MAX_TTL_SECS {
        return Err(Error::Config(format!(
            "runtime.file_export_link_ttl_secs must not exceed {}",
            super::FILE_EXPORT_LINK_MAX_TTL_SECS
        )));
    }
    validate_nonzero(
        "runtime.file_download_timeout_ms",
        runtime.file_download_timeout_ms,
    )?;
    validate_nonzero(
        "runtime.file_backup_default_ttl_secs",
        runtime.file_backup_default_ttl_secs,
    )?;
    validate_nonzero(
        "runtime.file_backup_max_ttl_secs",
        runtime.file_backup_max_ttl_secs,
    )?;
    if runtime.file_backup_default_ttl_secs > runtime.file_backup_max_ttl_secs {
        return Err(Error::Config(
            "runtime.file_backup_default_ttl_secs must not exceed runtime.file_backup_max_ttl_secs"
                .to_string(),
        ));
    }
    if runtime.file_backup_max_ttl_secs > super::FILE_BACKUP_HARD_MAX_TTL_SECS {
        return Err(Error::Config(format!(
            "runtime.file_backup_max_ttl_secs must not exceed {}",
            super::FILE_BACKUP_HARD_MAX_TTL_SECS
        )));
    }
    validate_nonzero(
        "runtime.file_backup_cleanup_interval_secs",
        runtime.file_backup_cleanup_interval_secs,
    )?;
    validate_nonzero(
        "runtime.file_backup_max_file_bytes",
        runtime.file_backup_max_file_bytes,
    )?;
    if runtime.file_backup_max_file_bytes > super::FILE_TRANSFER_HARD_MAX_BYTES {
        return Err(Error::Config(format!(
            "runtime.file_backup_max_file_bytes must not exceed {}",
            super::FILE_TRANSFER_HARD_MAX_BYTES
        )));
    }
    validate_nonzero(
        "runtime.file_backup_store_max_bytes",
        runtime.file_backup_store_max_bytes,
    )?;
    if runtime.file_backup_store_max_bytes < runtime.file_backup_max_file_bytes as u64 {
        return Err(Error::Config(
            "runtime.file_backup_store_max_bytes must be at least runtime.file_backup_max_file_bytes"
                .to_string(),
        ));
    }
    validate_nonzero(
        "runtime.file_backup_max_entries",
        runtime.file_backup_max_entries,
    )?;
    validate_nonzero(
        "runtime.terminal_default_rows",
        runtime.terminal_default_rows,
    )?;
    validate_nonzero(
        "runtime.terminal_default_cols",
        runtime.terminal_default_cols,
    )?;
    validate_nonzero(
        "runtime.app_success_collapse_ms",
        runtime.app_success_collapse_ms,
    )?;
    validate_nonzero(
        "runtime.app_failure_collapse_ms",
        runtime.app_failure_collapse_ms,
    )?;
    validate_nonzero("runtime.app_sleep_after_ms", runtime.app_sleep_after_ms)?;
    validate_nonzero(
        "runtime.app_job_poll_interval_ms",
        runtime.app_job_poll_interval_ms,
    )
}

fn validate_server(config: &Config) -> Result<()> {
    let server = &config.server;

    if server.terminal_ring_buffer_bytes == 0 {
        return Err(Error::Config(
            "server.terminal_ring_buffer_bytes must be greater than 0".to_string(),
        ));
    }
    if server.runtime_dir.as_os_str().is_empty() {
        return Err(Error::Config(
            "server.runtime_dir must not be empty".to_string(),
        ));
    }

    validate_optional_secret(
        "server.http_bearer_token",
        server.http_bearer_token.as_deref(),
    )?;
    validate_optional_secret(
        "server.oauth_authorization_password",
        server.oauth_authorization_password.as_deref(),
    )?;

    if let Some(base_url) = &server.public_base_url {
        if base_url.trim().is_empty() || base_url.trim() != base_url {
            return Err(Error::Config(
                "server.public_base_url must not be empty or padded with whitespace".to_string(),
            ));
        }
        if base_url.ends_with('/') {
            return Err(Error::Config(
                "server.public_base_url must not end with /".to_string(),
            ));
        }
    }

    if server.oauth_enabled && server.oauth_scopes.is_empty() {
        return Err(Error::Config(
            "server.oauth_scopes must contain at least one scope when OAuth is enabled".to_string(),
        ));
    }
    if let Some(scope) = server
        .oauth_scopes
        .iter()
        .find(|scope| scope.trim().is_empty() || scope.trim() != *scope)
    {
        return Err(Error::Config(format!(
            "server.oauth_scopes contains an empty or padded scope: {scope:?}"
        )));
    }

    validate_nonzero(
        "server.oauth_authorization_code_ttl_secs",
        server.oauth_authorization_code_ttl_secs,
    )?;
    validate_nonzero(
        "server.oauth_access_token_ttl_secs",
        server.oauth_access_token_ttl_secs,
    )?;
    validate_nonzero(
        "server.oauth_refresh_token_ttl_secs",
        server.oauth_refresh_token_ttl_secs,
    )
}

fn validate_optional_secret(label: &str, value: Option<&str>) -> Result<()> {
    if value.is_some_and(|value| value.trim().is_empty() || value.trim() != value) {
        return Err(Error::Config(format!(
            "{label} must not be empty or padded with whitespace"
        )));
    }
    Ok(())
}

fn validate_targets(config: &Config) -> Result<()> {
    for (name, target) in &config.targets {
        if name.trim().is_empty() || name.trim() != name {
            return Err(Error::Config(
                "target names must not be empty or padded with whitespace".to_string(),
            ));
        }

        match (name.as_str(), target) {
            ("local", TargetConfig::Local(local)) => {
                validate_target_shell("targets.local.shell", local.shell.as_deref())?;
                validate_policy("targets.local.policy", &local.policy)?;
            }
            ("local", TargetConfig::Ssh(_)) => {
                return Err(Error::Config(
                    "targets.local is reserved for kind = \"local\"".to_string(),
                ));
            }
            (_, TargetConfig::Local(_)) => {
                return Err(Error::Config(format!(
                    "targets.{name} uses kind = \"local\"; the local target must be configured as targets.local"
                )));
            }
            (_, TargetConfig::Ssh(ssh)) => {
                if ssh.host.trim().is_empty() || ssh.host.trim() != ssh.host {
                    return Err(Error::Config(format!(
                        "targets.{name}.host must not be empty or padded with whitespace"
                    )));
                }
                validate_nonzero(&format!("targets.{name}.port"), ssh.port)?;
                if ssh
                    .identity_file
                    .as_ref()
                    .is_some_and(|path| path.as_os_str().is_empty())
                {
                    return Err(Error::Config(format!(
                        "targets.{name}.identity_file must not be empty"
                    )));
                }
                validate_target_shell(&format!("targets.{name}.shell"), ssh.shell.as_deref())?;
                validate_policy(&format!("targets.{name}.policy"), &ssh.policy)?;
            }
        }
    }

    if let Some(default_target) = &config.server.default_target {
        if default_target.trim() != default_target {
            return Err(Error::Config(
                "server.default_target must not be padded with whitespace".to_string(),
            ));
        }
        let target = TargetId::from_str(default_target)
            .map_err(|err| Error::Config(format!("server.default_target is invalid: {err}")))?;
        ensure_configured_target(config, &target, "server.default_target")?;
    }

    Ok(())
}

fn validate_mcp_servers(config: &Config) -> Result<()> {
    for (name, mcp) in &config.mcp_servers {
        if name.trim().is_empty() || name.trim() != name {
            return Err(Error::Config(
                "mcp server names must not be empty or padded with whitespace".to_string(),
            ));
        }

        let source_count = usize::from(mcp.config_file.is_some())
            + usize::from(mcp.url.is_some())
            + usize::from(mcp.url_secret.is_some());
        if source_count != 1 {
            return Err(Error::Config(format!(
                "mcp_servers.{name} must configure exactly one of config_file, url, or url_secret"
            )));
        }
        if mcp
            .config_file
            .as_ref()
            .is_some_and(|path| path.as_os_str().is_empty())
        {
            return Err(Error::Config(format!(
                "mcp_servers.{name}.config_file must not be empty"
            )));
        }
        if mcp
            .url
            .as_ref()
            .is_some_and(|url| url.trim().is_empty() || url.trim() != url)
        {
            return Err(Error::Config(format!(
                "mcp_servers.{name}.url must not be empty or padded with whitespace"
            )));
        }

        if let Some(target) = &mcp.secret_target {
            if target.trim().is_empty() || target.trim() != target {
                return Err(Error::Config(format!(
                    "mcp_servers.{name}.secret_target must not be empty or padded with whitespace"
                )));
            }
            let target_id = TargetId::from_str(target).map_err(|err| {
                Error::Config(format!(
                    "mcp_servers.{name}.secret_target is invalid: {err}"
                ))
            })?;
            ensure_configured_target(
                config,
                &target_id,
                &format!("mcp_servers.{name}.secret_target"),
            )?;
        }

        validate_nonzero(&format!("mcp_servers.{name}.timeout_ms"), mcp.timeout_ms)?;
        validate_nonzero(
            &format!("mcp_servers.{name}.max_response_bytes"),
            mcp.max_response_bytes,
        )?;
    }

    Ok(())
}

fn ensure_configured_target(config: &Config, target: &TargetId, label: &str) -> Result<()> {
    if config.targets.contains_key(target.config_key()) {
        return Ok(());
    }
    Err(Error::Config(format!(
        "{label} refers to unconfigured target {target}"
    )))
}

fn validate_target_shell(label: &str, shell: Option<&str>) -> Result<()> {
    if shell.is_some_and(|value| value.trim().is_empty() || value.trim() != value) {
        return Err(Error::Config(format!(
            "{label} must not be empty or padded with whitespace"
        )));
    }
    Ok(())
}

fn validate_policy(label: &str, policy: &PolicyConfig) -> Result<()> {
    validate_nonzero(
        &format!("{label}.default_timeout_ms"),
        policy.default_timeout_ms,
    )?;
    validate_nonzero(
        &format!("{label}.max_output_bytes"),
        policy.max_output_bytes,
    )
}

fn validate_nonzero<T>(label: &str, value: T) -> Result<()>
where
    T: Default + PartialEq,
{
    if value == T::default() {
        return Err(Error::Config(format!("{label} must be greater than 0")));
    }
    Ok(())
}
