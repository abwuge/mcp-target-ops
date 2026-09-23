use crate::{
    core::{
        config::{Config, TargetConfig},
        error::{Error, Result},
        oauth::OAuthState,
        target::{ResolvedTarget, TargetId, TargetSource},
    },
    tooling::{
        download::DownloadRegistry, file_backup::FileBackupStore, job::JobRegistry,
        result_store::ResultStore, terminal::TerminalRegistry,
    },
    transport::ssh::SshSessionRegistry,
};
use serde::Serialize;
use std::{
    collections::HashSet,
    fs,
    str::FromStr,
    sync::{Arc, Mutex},
    time::SystemTime,
};

pub struct AppState {
    pub config: Config,
    active_target: Mutex<Option<TargetId>>,
    pub ssh_sessions: SshSessionRegistry,
    pub terminals: TerminalRegistry,
    pub jobs: JobRegistry,
    pub results: ResultStore,
    pub downloads: DownloadRegistry,
    pub backups: Arc<FileBackupStore>,
    pub oauth: Mutex<OAuthState>,
    target_instruction_sessions: Mutex<HashSet<String>>,
    started_at: SystemTime,
}

#[derive(Debug, Clone, Serialize)]
pub struct TargetSummary {
    pub id: String,
    pub kind: String,
    pub config_key: String,
    pub enabled: bool,
    pub active: bool,
    pub policy: TargetPolicySummary,
}

#[derive(Debug, Clone, Serialize)]
pub struct TargetPolicySummary {
    pub allow_exec: bool,
    pub allow_terminal: bool,
    pub allow_file_read: bool,
    pub allow_file_write: bool,
    pub allow_select_active: bool,
    pub require_explicit_target_for_write: bool,
    pub allowed_roots: Vec<String>,
}

impl AppState {
    pub fn new(config: Config) -> Result<Self> {
        config.ensure_runtime_dir()?;

        let results = ResultStore::new(
            &config.server.runtime_dir,
            config.runtime.result_cache_max_bytes,
        )?;
        let terminals =
            TerminalRegistry::new(config.server.terminal_ring_buffer_bytes, &config.runtime);
        let jobs = JobRegistry::new(config.runtime.clone());
        let downloads = DownloadRegistry::new(&config.server.runtime_dir)?;
        let backups = Arc::new(FileBackupStore::new(
            &config.server.runtime_dir,
            &config.runtime,
        )?);
        backups.start_cleanup_worker()?;

        Ok(Self {
            ssh_sessions: SshSessionRegistry::new()?,
            terminals,
            jobs,
            results,
            downloads,
            backups,
            oauth: Mutex::new(OAuthState::load(config.server.oauth_state_file.clone())?),
            config,
            active_target: Mutex::new(None),
            target_instruction_sessions: Mutex::new(HashSet::new()),
            started_at: SystemTime::now(),
        })
    }

    pub fn started_at(&self) -> SystemTime {
        self.started_at
    }

    pub fn target_instructions_required(&self, caller_key: &str) -> Result<bool> {
        if self
            .target_instruction_sessions
            .lock()
            .unwrap()
            .contains(caller_key)
        {
            return Ok(false);
        }

        let Some(path) = self.config.target_instructions_file.as_ref() else {
            return Ok(false);
        };
        match fs::metadata(path) {
            Ok(metadata) => Ok(metadata.len() > 0),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(Error::Config(format!(
                "failed to inspect target instructions file {}: {err}",
                path.display()
            ))),
        }
    }

    pub fn mark_target_instructions_loaded(&self, caller_key: &str) {
        self.target_instruction_sessions
            .lock()
            .unwrap()
            .insert(caller_key.to_string());
    }

    pub fn clear_target_instructions_for_caller(&self, caller_key: &str) {
        self.target_instruction_sessions
            .lock()
            .unwrap()
            .remove(caller_key);
    }

    pub fn resolve_target(&self, requested: Option<&str>) -> Result<(TargetId, TargetSource)> {
        if let Some(target) = requested {
            if !target.trim().is_empty() {
                return Ok((TargetId::from_str(target)?, TargetSource::Explicit));
            }
        }

        if let Some(active) = self.active_target.lock().unwrap().clone() {
            return Ok((active, TargetSource::Active));
        }

        if let Some(default_target) = &self.config.server.default_target {
            return Ok((TargetId::from_str(default_target)?, TargetSource::Default));
        }

        Err(Error::Target(
            "no target specified and no active target selected".to_string(),
        ))
    }

    pub fn current_target(&self) -> Option<TargetId> {
        self.active_target.lock().unwrap().clone()
    }

    pub fn set_active_target(&self, target: TargetId) -> Option<TargetId> {
        let mut guard = self.active_target.lock().unwrap();
        guard.replace(target)
    }

    pub fn get_target_config(&self, target: &TargetId) -> Result<&TargetConfig> {
        self.config.targets.get(target.config_key()).ok_or_else(|| {
            Error::Target(format!(
                "target {target} is not configured; expected key '{}'",
                target.config_key()
            ))
        })
    }

    pub fn resolved_target_value(&self, target: TargetId, source: TargetSource) -> ResolvedTarget {
        ResolvedTarget::new(target, source)
    }

    pub fn list_targets(&self) -> Vec<TargetSummary> {
        let active = self.current_target();
        let mut summaries = Vec::new();

        for (key, config) in &self.config.targets {
            let target_id = match config {
                TargetConfig::Local(_) => TargetId::Local,
                TargetConfig::Ssh(_) => TargetId::Ssh(key.clone()),
            };
            let policy = crate::core::policy::target_policy(config);
            summaries.push(TargetSummary {
                id: target_id.to_string(),
                kind: target_id.kind().to_string(),
                config_key: key.clone(),
                enabled: crate::core::policy::target_enabled(config),
                active: active.as_ref() == Some(&target_id),
                policy: TargetPolicySummary {
                    allow_exec: policy.allow_exec,
                    allow_terminal: policy.allow_terminal,
                    allow_file_read: policy.allow_file_read,
                    allow_file_write: policy.allow_file_write,
                    allow_select_active: policy.allow_select_active,
                    require_explicit_target_for_write: policy.require_explicit_target_for_write,
                    allowed_roots: policy.allowed_roots.clone(),
                },
            });
        }

        summaries
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::{tempdir, TempDir};

    fn state_with_default_target() -> (TempDir, AppState) {
        let temp = tempdir().expect("temporary directory");
        let mut config = Config::default();
        config.server.default_target = Some("local".to_string());
        config.server.oauth_state_file = None;
        config.server.runtime_dir = temp.path().join("runtime");
        let state = AppState::new(config).expect("state initializes");
        (temp, state)
    }

    #[test]
    fn default_target_is_a_fallback_not_an_active_selection() {
        let (_temp, state) = state_with_default_target();
        assert_eq!(state.current_target(), None);

        let (target, source) = state.resolve_target(None).expect("default target resolves");
        assert_eq!(target, TargetId::Local);
        assert_eq!(source, TargetSource::Default);
    }

    #[test]
    fn explicit_and_active_targets_take_precedence_over_the_default() {
        let (_temp, state) = state_with_default_target();

        let (_, source) = state
            .resolve_target(Some("local"))
            .expect("explicit target resolves");
        assert_eq!(source, TargetSource::Explicit);

        state.set_active_target(TargetId::Local);
        let (_, source) = state.resolve_target(None).expect("active target resolves");
        assert_eq!(source, TargetSource::Active);
    }
}
