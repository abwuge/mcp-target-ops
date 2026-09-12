mod validation;

use crate::core::{
    error::{Error, Result},
    secret::SecretRef,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,

    #[serde(default)]
    pub targets: BTreeMap<String, TargetConfig>,

    #[serde(default)]
    pub mcp_servers: BTreeMap<String, McpServerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_name")]
    pub name: String,

    #[serde(default = "default_version")]
    pub version: String,

    #[serde(default = "default_http_bearer_token")]
    pub http_bearer_token: Option<String>,

    #[serde(default = "default_oauth_enabled")]
    pub oauth_enabled: bool,

    #[serde(default = "default_public_base_url")]
    pub public_base_url: Option<String>,

    #[serde(default = "default_oauth_authorization_password")]
    pub oauth_authorization_password: Option<String>,

    #[serde(default = "default_oauth_scopes")]
    pub oauth_scopes: Vec<String>,

    #[serde(default = "default_oauth_allow_dynamic_client_registration")]
    pub oauth_allow_dynamic_client_registration: bool,

    #[serde(default = "default_oauth_authorization_code_ttl_secs")]
    pub oauth_authorization_code_ttl_secs: u64,

    #[serde(default = "default_oauth_access_token_ttl_secs")]
    pub oauth_access_token_ttl_secs: u64,

    #[serde(default = "default_oauth_refresh_token_ttl_secs")]
    pub oauth_refresh_token_ttl_secs: u64,

    #[serde(default = "default_oauth_state_file")]
    pub oauth_state_file: Option<PathBuf>,

    #[serde(default)]
    pub default_target: Option<String>,

    #[serde(default = "default_ring_buffer_bytes")]
    pub terminal_ring_buffer_bytes: usize,

    #[serde(default = "default_runtime_dir")]
    pub runtime_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TargetConfig {
    Local(LocalTargetConfig),
    Ssh(SshTargetConfig),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalTargetConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default)]
    pub shell: Option<String>,

    #[serde(default)]
    pub policy: PolicyConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshTargetConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,

    pub host: String,

    #[serde(default = "default_ssh_port")]
    pub port: u16,

    #[serde(default)]
    pub user: Option<String>,

    #[serde(default)]
    pub identity_file: Option<PathBuf>,

    #[serde(default)]
    pub extra_args: Vec<String>,

    #[serde(default)]
    pub shell: Option<String>,

    #[serde(default)]
    pub policy: PolicyConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,

    #[serde(default)]
    pub config_file: Option<PathBuf>,

    #[serde(default)]
    pub url: Option<String>,

    #[serde(default)]
    pub url_secret: Option<SecretRef>,

    #[serde(default)]
    pub headers: BTreeMap<String, String>,

    #[serde(default)]
    pub secret_headers: BTreeMap<String, SecretRef>,

    #[serde(default)]
    pub secret_target: Option<String>,

    #[serde(default = "default_mcp_timeout_ms")]
    pub timeout_ms: u64,

    #[serde(default = "default_mcp_max_response_bytes")]
    pub max_response_bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyConfig {
    #[serde(default)]
    pub allow_exec: bool,

    #[serde(default)]
    pub allow_terminal: bool,

    #[serde(default)]
    pub allow_file_read: bool,

    #[serde(default)]
    pub allow_file_write: bool,

    #[serde(default)]
    pub allow_select_active: bool,

    #[serde(default = "default_true")]
    pub require_explicit_target_for_write: bool,

    #[serde(default)]
    pub allowed_roots: Vec<String>,

    #[serde(default = "default_exec_timeout_ms")]
    pub default_timeout_ms: u64,

    #[serde(default = "default_max_output_bytes")]
    pub max_output_bytes: usize,
}

impl Default for Config {
    fn default() -> Self {
        let mut targets = BTreeMap::new();
        targets.insert(
            "local".to_string(),
            TargetConfig::Local(LocalTargetConfig {
                enabled: false,
                shell: None,
                policy: PolicyConfig::default(),
            }),
        );

        Self {
            server: ServerConfig::default(),
            targets,
            mcp_servers: BTreeMap::new(),
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            name: default_name(),
            version: default_version(),
            http_bearer_token: default_http_bearer_token(),
            oauth_enabled: default_oauth_enabled(),
            public_base_url: default_public_base_url(),
            oauth_authorization_password: default_oauth_authorization_password(),
            oauth_scopes: default_oauth_scopes(),
            oauth_allow_dynamic_client_registration:
                default_oauth_allow_dynamic_client_registration(),
            oauth_authorization_code_ttl_secs: default_oauth_authorization_code_ttl_secs(),
            oauth_access_token_ttl_secs: default_oauth_access_token_ttl_secs(),
            oauth_refresh_token_ttl_secs: default_oauth_refresh_token_ttl_secs(),
            oauth_state_file: default_oauth_state_file(),
            default_target: None,
            terminal_ring_buffer_bytes: default_ring_buffer_bytes(),
            runtime_dir: default_runtime_dir(),
        }
    }
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            allow_exec: false,
            allow_terminal: false,
            allow_file_read: false,
            allow_file_write: false,
            allow_select_active: false,
            require_explicit_target_for_write: true,
            allowed_roots: Vec::new(),
            default_timeout_ms: default_exec_timeout_ms(),
            max_output_bytes: default_max_output_bytes(),
        }
    }
}

impl Config {
    pub fn load(path: Option<PathBuf>) -> Result<Self> {
        if let Some(path) = path {
            return Self::load_from_path(&path);
        }

        let default_path = default_config_path();
        if default_path.exists() {
            return Self::load_from_path(&default_path);
        }

        let config = Config::default();
        config.validate()?;
        Ok(config)
    }

    pub fn load_from_path(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path).map_err(|err| {
            Error::Config(format!("failed to read config {}: {err}", path.display()))
        })?;
        let mut config: Config = toml::from_str(&text)?;
        config.ensure_local_target();
        config.validate()?;
        Ok(config)
    }

    pub fn ensure_runtime_dir(&self) -> Result<()> {
        fs::create_dir_all(&self.server.runtime_dir).map_err(|err| {
            Error::Config(format!(
                "failed to create runtime dir {}: {err}",
                self.server.runtime_dir.display()
            ))
        })
    }

    fn ensure_local_target(&mut self) {
        self.targets.entry("local".to_string()).or_insert_with(|| {
            TargetConfig::Local(LocalTargetConfig {
                enabled: false,
                shell: None,
                policy: PolicyConfig::default(),
            })
        });
    }

    fn validate(&self) -> Result<()> {
        validation::validate(self)
    }
}

fn default_name() -> String {
    "mcp-target-ops".to_string()
}

fn default_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

fn default_http_bearer_token() -> Option<String> {
    std::env::var("MCP_TARGET_OPS_HTTP_TOKEN")
        .ok()
        .filter(|token| !token.trim().is_empty())
}

fn default_oauth_enabled() -> bool {
    matches!(
        std::env::var("MCP_TARGET_OPS_OAUTH").as_deref(),
        Ok("1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON")
    )
}

fn default_public_base_url() -> Option<String> {
    std::env::var("MCP_TARGET_OPS_PUBLIC_BASE_URL")
        .ok()
        .map(|url| url.trim_end_matches('/').to_string())
        .filter(|url| !url.is_empty())
}

fn default_oauth_authorization_password() -> Option<String> {
    std::env::var("MCP_TARGET_OPS_OAUTH_PASSWORD")
        .ok()
        .filter(|password| !password.trim().is_empty())
}

fn default_oauth_scopes() -> Vec<String> {
    std::env::var("MCP_TARGET_OPS_OAUTH_SCOPES")
        .ok()
        .map(|scopes| {
            scopes
                .split_whitespace()
                .filter(|scope| !scope.is_empty())
                .map(ToString::to_string)
                .collect()
        })
        .filter(|scopes: &Vec<String>| !scopes.is_empty())
        .unwrap_or_else(|| vec!["mcp:tools".to_string()])
}

fn default_oauth_allow_dynamic_client_registration() -> bool {
    true
}

fn default_oauth_authorization_code_ttl_secs() -> u64 {
    600
}

fn default_oauth_access_token_ttl_secs() -> u64 {
    3600
}

fn default_oauth_refresh_token_ttl_secs() -> u64 {
    30 * 24 * 60 * 60
}

fn default_oauth_state_file() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("MCP_TARGET_OPS_OAUTH_STATE_FILE") {
        return Some(PathBuf::from(path));
    }

    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    Some(
        home.join(".config")
            .join("mcp-target-ops")
            .join("oauth-state.json"),
    )
}

fn default_ring_buffer_bytes() -> usize {
    512 * 1024
}

fn default_runtime_dir() -> PathBuf {
    std::env::temp_dir().join("mcp-target-ops")
}

fn default_ssh_port() -> u16 {
    22
}

fn default_true() -> bool {
    true
}

fn default_exec_timeout_ms() -> u64 {
    30_000
}

fn default_mcp_timeout_ms() -> u64 {
    20_000
}

fn default_mcp_max_response_bytes() -> usize {
    1024 * 1024
}

fn default_max_output_bytes() -> usize {
    200_000
}

pub fn default_config_path() -> PathBuf {
    if let Ok(path) = std::env::var("MCP_TARGET_OPS_CONFIG") {
        return PathBuf::from(path);
    }

    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    home.join(".config")
        .join("mcp-target-ops")
        .join("config.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_config(source: &str) -> Config {
        let mut config: Config = toml::from_str(source).expect("valid TOML");
        config.ensure_local_target();
        config
    }

    #[test]
    fn accepts_local_and_named_ssh_targets() {
        let config = parse_config(
            r#"
            [server]
            default_target = "ssh:dev"

            [targets.dev]
            kind = "ssh"
            host = "dev.example.com"
            "#,
        );

        config.validate().expect("target layout is valid");
    }

    #[test]
    fn rejects_local_target_under_a_named_profile() {
        let config = parse_config(
            r#"
            [targets.other]
            kind = "local"
            "#,
        );

        let err = config
            .validate()
            .expect_err("named local target is invalid");
        assert!(err.to_string().contains("targets.other"));
    }

    #[test]
    fn rejects_ssh_target_using_the_reserved_local_key() {
        let config = parse_config(
            r#"
            [targets.local]
            kind = "ssh"
            host = "example.com"
            "#,
        );

        let err = config
            .validate()
            .expect_err("reserved local key is invalid");
        assert!(err.to_string().contains("targets.local"));
    }

    #[test]
    fn rejects_unconfigured_default_target() {
        let config = parse_config(
            r#"
            [server]
            default_target = "ssh:missing"
            "#,
        );

        let err = config
            .validate()
            .expect_err("default target must be configured");
        assert!(err.to_string().contains("unconfigured target ssh:missing"));
    }

    #[test]
    fn rejects_unconfigured_secret_target() {
        let config = parse_config(
            r#"
            [mcp_servers.memory]
            url = "https://memory.example.com/mcp"
            secret_target = "ssh:missing"
            "#,
        );

        let err = config
            .validate()
            .expect_err("secret target must be configured");
        assert!(err.to_string().contains(
            "mcp_servers.memory.secret_target refers to unconfigured target ssh:missing"
        ));
    }

    #[test]
    fn rejects_zero_limits() {
        let server = parse_config(
            r#"
            [server]
            terminal_ring_buffer_bytes = 0
            "#,
        );
        assert!(server.validate().is_err());

        let target = parse_config(
            r#"
            [targets.dev]
            kind = "ssh"
            host = "dev.example.com"

            [targets.dev.policy]
            default_timeout_ms = 0
            "#,
        );
        assert!(target.validate().is_err());
    }
}
