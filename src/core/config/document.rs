use super::{
    default_control_persist_secs, default_exec_timeout_ms, default_max_output_bytes,
    default_mcp_max_response_bytes, default_mcp_timeout_ms, default_runtime_dir, default_ssh_port,
    Config,
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
# this file so upgrades remain self-documenting. Existing values and comments are
# preserved. Environment-backed secrets are never copied here automatically.

[config]
# Rewrite this TOML after loading it so defaults introduced by a newer binary
# become visible and editable. Set to false to keep the file byte-for-byte
# unchanged; missing options still use built-in defaults in memory.
rewrite_on_start = true

# Process-wide operational defaults. Values ending in _ms are milliseconds;
# values ending in _bytes are raw bytes. Safety/protocol hard limits remain
# compiled into the binary and cannot be raised here.
[runtime]
# Total on-disk budget for cached MCP App results. There is no TTL; when this
# budget is exceeded, the least-recently-active whole session is evicted.
result_cache_max_bytes = 104857600
# Maximum number of background job entries retained in memory.
max_retained_jobs = 128
# Maximum number of foreground exec sessions retained for MCP App attachment and
# incremental-output compatibility.
max_retained_foreground_execs = 64
# Foreground wait before an ordinary exec is promoted to the same running command
# as a background job. Explicit per-call timeout_ms remains authoritative.
exec_auto_background_after_ms = 5000
# Default bytes returned by one incremental job/terminal output read when the
# caller omits max_bytes.
stream_default_max_bytes = 65536
# Maximum bytes allowed for one incremental job/terminal output read. The default
# above must not exceed this value.
stream_max_bytes = 524288
# Default server-side wait window for job_wait when wait_timeout_ms is omitted.
job_wait_default_timeout_ms = 60000
# Maximum job_wait window; larger per-call requests are clamped to this value.
job_wait_max_timeout_ms = 120000
# Default file_import/file_export transfer limit. The compiled absolute transfer
# ceiling is 100 MiB and cannot be raised through configuration.
file_transfer_default_max_bytes = 26214400
# Default file_export delivery. "link" stages an opaque short-lived HTTPS download
# and avoids ChatGPT attachment materialization; "attachment" preserves the
# embedded-resource compatibility path.
file_export_delivery = "link"
# Lifetime of link-mode exports in seconds. The compiled maximum is 86400 (24h).
file_export_link_ttl_secs = 600
# If true, the first successful GET consumes a link. False is safer for browsers
# and clients that may prefetch or inspect links before the user clicks them.
file_export_link_single_use = false
# Default timeout for downloading HTTPS connector/file references in file_import.
file_download_timeout_ms = 30000
# Managed file_backup retention. Backups live under server.runtime_dir instead of
# beside the source file, so models do not leave scattered .bak/.old copies.
file_backup_default_ttl_secs = 86400
# Per-call ttl_secs cannot exceed this value; the compiled ceiling is 2592000 (30d).
file_backup_max_ttl_secs = 2592000
# Background interval for removing expired backups and enforcing store budgets.
# Normal tool traffic also performs throttled cleanup as a fallback.
file_backup_cleanup_interval_secs = 60
# Maximum size of one managed backup and total on-disk backup budget.
file_backup_max_file_bytes = 104857600
file_backup_store_max_bytes = 1073741824
# Maximum number of retained managed snapshots. This also bounds empty/tiny-file
# backups whose metadata would otherwise evade a byte-only payload budget.
file_backup_max_entries = 1024
# Default terminal_open PTY size when rows/cols are omitted.
terminal_default_rows = 30
terminal_default_cols = 120
# Auto-collapse delay for successful MCP App result cards.
app_success_collapse_ms = 3000
# Auto-collapse delay for failed, timed-out, or warning-like App result cards.
app_failure_collapse_ms = 6000
# Delay before a collapsed App releases heavy DOM and relies on cached restore.
app_sleep_after_ms = 30000
# Refresh interval used by the command App when reading exec_start job output.
app_job_poll_interval_ms = 180

[server]
# Server name advertised during MCP initialize.
name = "mcp-target-ops"
# Allow OAuth Dynamic Client Registration at /oauth/register.
oauth_allow_dynamic_client_registration = true
# Lifetime of one-time OAuth authorization codes, in seconds.
oauth_authorization_code_ttl_secs = 600
# Lifetime of OAuth access tokens, in seconds.
oauth_access_token_ttl_secs = 3600
# Lifetime of OAuth refresh tokens, in seconds.
oauth_refresh_token_ttl_secs = 2592000
# Per-terminal retained output capacity. Older terminal output rolls out of this
# ring buffer when a session exceeds the configured size.
terminal_ring_buffer_bytes = 524288
# runtime_dir is populated with this machine's platform-specific default and is
# used for runtime state such as App result cache files.
# Environment-backed or secret-bearing options are intentionally not materialized
# here unless you configure them yourself, so environment secrets are never
# copied into this file by the startup rewrite.

[targets.local]
# The reserved local target must use kind = "local".
kind = "local"
# Local access is disabled in the generated configuration for deny-by-default
# single-binary startup. Enable it only for a trusted host.
enabled = false

[targets.local.policy]
# Permit non-interactive commands such as exec, exec_batch, and exec_start.
allow_exec = false
# Permit persistent interactive PTY sessions.
allow_terminal = false
# Permit structured file/directory reads inside allowed_roots.
allow_file_read = false
# Permit file mutations inside allowed_roots.
allow_file_write = false
# Permit target_select to make local the session-scoped active target.
allow_select_active = false
# Require write tools to name their target explicitly instead of inheriting
# active/default target state.
require_explicit_target_for_write = true
# Absolute roots visible to file tools. Empty means all file access is refused,
# even if allow_file_read or allow_file_write is enabled.
allowed_roots = []
# Default command/remote/file-operation timeout when a request omits timeout_ms.
default_timeout_ms = 30000
# Per-target command output cap; per-call max_output_bytes cannot exceed it.
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
    let config_dir = path.parent().unwrap_or_else(|| Path::new("."));
    config.target_instructions_file = Some(config_dir.join("AGENTS.md"));
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
                    insert_if_missing(target, "control_master", value(true));
                    insert_if_missing(
                        target,
                        "control_persist_secs",
                        value(default_control_persist_secs() as i64),
                    );
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
        assert!(written.contains("file_export_delivery = \"link\""));
        assert!(written.contains("file_export_link_ttl_secs = 600"));
        assert!(written.contains("file_backup_default_ttl_secs = 86400"));
        assert!(written.contains("file_backup_store_max_bytes = 1073741824"));
        assert!(written.contains("file_backup_max_entries = 1024"));
        assert!(written.contains("default_timeout_ms = 30000"));
        assert!(!written.contains("startup_prompt"));
        assert_eq!(
            config.target_instructions_file,
            Some(path.parent().unwrap().join("AGENTS.md"))
        );
        assert!(!path.parent().unwrap().join("AGENTS.md").exists());
        assert!(!written.contains("http_bearer_token"));
        assert!(!written.contains("oauth_authorization_password"));
    }

    #[test]
    fn target_instructions_file_is_fixed_next_to_config_and_preserved() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("nested/config.toml");
        let agents = temp.path().join("nested/AGENTS.md");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "[server]\nname = \"custom\"\n").unwrap();
        fs::write(
            &agents,
            "# Long target instructions\nKeep this file untouched.\n",
        )
        .unwrap();

        let config = load(&path).unwrap();
        assert_eq!(config.target_instructions_file, Some(agents.clone()));
        assert_eq!(
            fs::read_to_string(&agents).unwrap(),
            "# Long target instructions\nKeep this file untouched.\n"
        );
        let written = fs::read_to_string(&path).unwrap();
        assert!(!written.contains("startup_prompt"));
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
        assert!(written.contains("control_persist_secs = 1800"));
        assert!(written.contains("[runtime]"));
        assert!(written.contains("[targets.dev.policy]"));
        assert!(written.contains("file_backup_default_ttl_secs = 86400"));
        assert!(written.contains("file_backup_max_ttl_secs = 2592000"));
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
