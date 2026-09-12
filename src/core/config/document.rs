use super::{
    default_exec_timeout_ms, default_max_output_bytes, default_mcp_max_response_bytes,
    default_mcp_timeout_ms, default_runtime_dir, default_ssh_port, Config,
};
use crate::core::error::{Error, Result};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};
use toml_edit::{value, Array, DocumentMut, Item, Table};

const BUILTIN_TEMPLATE: &str = r#"# Target Ops configuration.
#
# On startup, Target Ops fills in newly introduced managed defaults and rewrites
# this file so upgrades remain self-documenting. Set rewrite_on_start = false to
# keep the file byte-for-byte unchanged after it is read.

[config]
rewrite_on_start = true

# Process-wide operational defaults. Safety/protocol hard limits remain compiled
# into the binary; these values control normal runtime behavior below those caps.
[runtime]
result_cache_max_bytes = 104857600
max_retained_jobs = 128
max_retained_foreground_execs = 64
exec_auto_background_after_ms = 5000
stream_default_max_bytes = 65536
stream_max_bytes = 524288
job_wait_default_timeout_ms = 60000
job_wait_max_timeout_ms = 120000
file_transfer_default_max_bytes = 26214400
file_download_timeout_ms = 30000
terminal_default_rows = 30
terminal_default_cols = 120
app_success_collapse_ms = 3000
app_failure_collapse_ms = 6000
app_sleep_after_ms = 30000
app_job_poll_interval_ms = 180

[server]
name = "mcp-target-ops"
oauth_allow_dynamic_client_registration = true
oauth_authorization_code_ttl_secs = 600
oauth_access_token_ttl_secs = 3600
oauth_refresh_token_ttl_secs = 2592000
terminal_ring_buffer_bytes = 524288
# runtime_dir is populated with this machine's platform-specific default.
# Environment-backed or secret-bearing options are intentionally not materialized
# here unless you configure them yourself, so environment secrets are never
# copied into this file by the startup rewrite.

[targets.local]
kind = "local"
enabled = false

[targets.local.policy]
allow_exec = false
allow_terminal = false
allow_file_read = false
allow_file_write = false
allow_select_active = false
require_explicit_target_for_write = true
allowed_roots = []
default_timeout_ms = 30000
max_output_bytes = 200000
"#;

pub(super) fn load(path: &Path) -> Result<Config> {
    let existed = path.exists();
    let mut document = if existed {
        let text = fs::read_to_string(path).map_err(|err| {
            Error::Config(format!("failed to read config {}: {err}", path.display()))
        })?;
        parse_document(&text, path)?
    } else {
        builtin_document()?
    };

    if existed {
        let defaults = builtin_document()?;
        merge_table(document.as_table_mut(), defaults.as_table());
    }
    merge_dynamic_defaults(&mut document);

    let rendered = document.to_string();
    let mut config: Config = toml::from_str(&rendered)?;
    config.ensure_local_target();
    config.validate()?;

    if !existed || config.config.rewrite_on_start {
        write_document(path, &rendered)?;
    }

    Ok(config)
}

fn parse_document(text: &str, path: &Path) -> Result<DocumentMut> {
    text.parse::<DocumentMut>().map_err(|err| {
        Error::Config(format!(
            "failed to parse config {} as TOML: {err}",
            path.display()
        ))
    })
}

fn builtin_document() -> Result<DocumentMut> {
    let mut document = BUILTIN_TEMPLATE
        .parse::<DocumentMut>()
        .map_err(|err| Error::Config(format!("invalid built-in config template: {err}")))?;
    document["server"]["runtime_dir"] = value(default_runtime_dir().to_string_lossy().to_string());
    Ok(document)
}

fn merge_table(current: &mut Table, defaults: &Table) {
    for (key, default_item) in defaults.iter() {
        match current.get_mut(key) {
            None => {
                current.insert(key, default_item.clone());
            }
            Some(current_item) => {
                if let (Some(current_table), Some(default_table)) =
                    (current_item.as_table_mut(), default_item.as_table())
                {
                    merge_table(current_table, default_table);
                }
            }
        }
    }
}

fn merge_dynamic_defaults(document: &mut DocumentMut) {
    if let Some(targets) = document.get_mut("targets").and_then(Item::as_table_mut) {
        for (_, target_item) in targets.iter_mut() {
            let Some(target) = target_item.as_table_mut() else {
                continue;
            };
            let kind = target
                .get("kind")
                .and_then(Item::as_value)
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            match kind {
                "local" => insert_if_missing(target, "enabled", value(false)),
                "ssh" => {
                    insert_if_missing(target, "enabled", value(true));
                    insert_if_missing(target, "port", value(i64::from(default_ssh_port())));
                    insert_if_missing(target, "extra_args", value(Array::new()));
                }
                _ => {}
            }

            if !target.contains_key("policy") {
                target.insert("policy", Item::Table(Table::new()));
            }
            if let Some(policy) = target.get_mut("policy").and_then(Item::as_table_mut) {
                merge_policy_defaults(policy);
            }
        }
    }

    if let Some(servers) = document.get_mut("mcp_servers").and_then(Item::as_table_mut) {
        for (_, server_item) in servers.iter_mut() {
            let Some(server) = server_item.as_table_mut() else {
                continue;
            };
            insert_if_missing(server, "enabled", value(true));
            insert_if_missing(server, "timeout_ms", value(default_mcp_timeout_ms() as i64));
            insert_if_missing(
                server,
                "max_response_bytes",
                value(default_mcp_max_response_bytes() as i64),
            );
        }
    }
}

fn merge_policy_defaults(policy: &mut Table) {
    insert_if_missing(policy, "allow_exec", value(false));
    insert_if_missing(policy, "allow_terminal", value(false));
    insert_if_missing(policy, "allow_file_read", value(false));
    insert_if_missing(policy, "allow_file_write", value(false));
    insert_if_missing(policy, "allow_select_active", value(false));
    insert_if_missing(policy, "require_explicit_target_for_write", value(true));
    insert_if_missing(policy, "allowed_roots", value(Array::new()));
    insert_if_missing(
        policy,
        "default_timeout_ms",
        value(default_exec_timeout_ms() as i64),
    );
    insert_if_missing(
        policy,
        "max_output_bytes",
        value(default_max_output_bytes() as i64),
    );
}

fn insert_if_missing(table: &mut Table, key: &str, item: Item) {
    if !table.contains_key(key) {
        table.insert(key, item);
    }
}

fn write_document(path: &Path, rendered: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            Error::Config(format!(
                "failed to create config directory {}: {err}",
                parent.display()
            ))
        })?;
    }

    if path.exists() {
        fs::write(path, rendered).map_err(|err| {
            Error::Config(format!(
                "failed to rewrite config {}: {err}",
                path.display()
            ))
        })?;
        return Ok(());
    }

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|err| {
        Error::Config(format!("failed to create config {}: {err}", path.display()))
    })?;
    file.write_all(rendered.as_bytes()).map_err(|err| {
        Error::Config(format!("failed to write config {}: {err}", path.display()))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn missing_config_is_generated_from_builtin_template() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("nested/config.toml");
        let config = load(&path).unwrap();
        let written = fs::read_to_string(&path).unwrap();

        assert!(config.config.rewrite_on_start);
        assert_eq!(config.runtime, super::super::RuntimeConfig::default());
        assert!(written.contains("[runtime]"));
        assert!(written.contains("exec_auto_background_after_ms = 5000"));
        assert!(written.contains("default_timeout_ms = 30000"));
        assert!(!written.contains("http_bearer_token"));
        assert!(!written.contains("oauth_authorization_password"));
    }

    #[test]
    fn rewrite_preserves_custom_values_and_adds_new_defaults() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("config.toml");
        fs::write(
            &path,
            r#"# keep this comment
[server]
name = "custom"

[runtime]
result_cache_max_bytes = 12345

[targets.dev]
kind = "ssh"
host = "example.com"
control_master = true

[mcp_servers.memory]
url = "https://example.com/mcp"
"#,
        )
        .unwrap();

        let config = load(&path).unwrap();
        let written = fs::read_to_string(&path).unwrap();

        assert_eq!(config.server.name, "custom");
        assert_eq!(config.runtime.result_cache_max_bytes, 12345);
        assert!(written.contains("# keep this comment"));
        assert!(written.contains("control_master = true"));
        assert!(written.contains("[runtime]"));
        assert!(written.contains("[targets.dev.policy]"));
        assert!(written.contains("default_timeout_ms = 30000"));
        assert!(written.contains("timeout_ms = 20000"));
    }

    #[test]
    fn rewrite_can_be_disabled_without_disabling_runtime_defaults() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("config.toml");
        let original = "[config]\nrewrite_on_start = false\n";
        fs::write(&path, original).unwrap();

        let config = load(&path).unwrap();
        let written = fs::read_to_string(&path).unwrap();

        assert!(!config.config.rewrite_on_start);
        assert_eq!(config.runtime.job_wait_default_timeout_ms, 60_000);
        assert_eq!(written, original);
    }
}
