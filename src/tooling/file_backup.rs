use crate::{
    core::{
        config::RuntimeConfig,
        error::{Error, Result},
        policy::{self, FileAccess},
        state::AppState,
        target::{ResolvedTarget, TargetId},
        util::sha256_hex,
    },
    tooling::fs::{self, file_mode},
};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use std::{
    fs as stdfs,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const BACKUP_FORMAT_VERSION: u32 = 1;
const BACKUP_ID_BYTES: usize = 16;
const MAX_BACKUP_NOTE_BYTES: usize = 512;
const DEFAULT_LIST_LIMIT: usize = 100;
const MAX_LIST_LIMIT: usize = 200;

#[derive(Debug, Clone, Deserialize)]
pub struct FileBackupRequest {
    #[serde(default)]
    pub target: Option<String>,
    pub path: String,
    #[serde(default)]
    pub ttl_secs: Option<u64>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileBackupResponse {
    pub resolved_target: ResolvedTarget,
    pub backup: FileBackupEntry,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileBackupListRequest {
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileBackupListResponse {
    pub resolved_target: ResolvedTarget,
    pub backups: Vec<FileBackupEntry>,
    pub total_matches: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileRestoreRequest {
    #[serde(default)]
    pub target: Option<String>,
    pub backup_id: String,
    #[serde(default)]
    pub destination: Option<String>,
    #[serde(default)]
    pub expected_current_sha256: Option<String>,
    #[serde(default)]
    pub overwrite: bool,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileRestoreResponse {
    pub resolved_target: ResolvedTarget,
    pub backup_id: String,
    pub source_path: String,
    pub path: String,
    pub created: bool,
    pub restored: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_sha256: Option<String>,
    pub restored_sha256: String,
    pub bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    pub backup_expires_at_unix_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileBackupDeleteRequest {
    #[serde(default)]
    pub target: Option<String>,
    pub backup_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileBackupDeleteResponse {
    pub resolved_target: ResolvedTarget,
    pub backup_id: String,
    pub path: String,
    pub deleted: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileBackupEntry {
    pub backup_id: String,
    pub target: String,
    pub path: String,
    pub sha256: String,
    pub bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    pub created_at_unix_secs: u64,
    pub expires_at_unix_secs: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BackupMetadata {
    version: u32,
    backup_id: String,
    target: String,
    path: String,
    sha256: String,
    bytes: usize,
    mode: Option<u32>,
    created_at_unix_secs: u64,
    expires_at_unix_secs: u64,
    note: Option<String>,
}

impl BackupMetadata {
    fn to_entry(&self) -> FileBackupEntry {
        FileBackupEntry {
            backup_id: self.backup_id.clone(),
            target: self.target.clone(),
            path: self.path.clone(),
            sha256: self.sha256.clone(),
            bytes: self.bytes,
            mode: self.mode.map(format_mode),
            created_at_unix_secs: self.created_at_unix_secs,
            expires_at_unix_secs: self.expires_at_unix_secs,
            note: self.note.clone(),
        }
    }
}

pub struct FileBackupStore {
    root: PathBuf,
    default_ttl_secs: u64,
    max_ttl_secs: u64,
    cleanup_interval_secs: u64,
    max_file_bytes: usize,
    store_max_bytes: u64,
    max_entries: usize,
    last_cleanup_unix_secs: AtomicU64,
    gate: Mutex<()>,
}

impl FileBackupStore {
    pub fn new(runtime_dir: &Path, runtime: &RuntimeConfig) -> Result<Self> {
        let root = runtime_dir.join("file-backups");
        stdfs::create_dir_all(&root)?;
        secure_directory(&root)?;
        let store = Self {
            root,
            default_ttl_secs: runtime.file_backup_default_ttl_secs,
            max_ttl_secs: runtime.file_backup_max_ttl_secs,
            cleanup_interval_secs: runtime.file_backup_cleanup_interval_secs,
            max_file_bytes: runtime.file_backup_max_file_bytes,
            store_max_bytes: runtime.file_backup_store_max_bytes,
            max_entries: runtime.file_backup_max_entries,
            last_cleanup_unix_secs: AtomicU64::new(0),
            gate: Mutex::new(()),
        };
        store.cleanup_now()?;
        Ok(store)
    }

    pub fn start_cleanup_worker(self: &Arc<Self>) -> Result<()> {
        let weak = Arc::downgrade(self);
        let interval = Duration::from_secs(self.cleanup_interval_secs);
        thread::Builder::new()
            .name("target-ops-file-backup-cleanup".to_string())
            .spawn(move || loop {
                thread::sleep(interval);
                let Some(store) = weak.upgrade() else {
                    break;
                };
                let _ = store.cleanup_now();
            })
            .map(|_| ())
            .map_err(|error| {
                Error::Tool(format!(
                    "failed to start managed backup cleanup worker: {error}"
                ))
            })
    }

    pub fn maybe_cleanup(&self) -> Result<()> {
        let now = unix_now()?;
        let last = self.last_cleanup_unix_secs.load(Ordering::Relaxed);
        if now.saturating_sub(last) < self.cleanup_interval_secs {
            return Ok(());
        }
        if self
            .last_cleanup_unix_secs
            .compare_exchange(last, now, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
        {
            return Ok(());
        }
        if let Err(error) = self.cleanup_at(now) {
            self.last_cleanup_unix_secs.store(last, Ordering::Release);
            return Err(error);
        }
        Ok(())
    }

    pub fn cleanup_now(&self) -> Result<()> {
        let now = unix_now()?;
        self.cleanup_at(now)?;
        self.last_cleanup_unix_secs.store(now, Ordering::Release);
        Ok(())
    }

    fn cleanup_at(&self, now: u64) -> Result<()> {
        let _guard = self.lock()?;
        let mut entries = self.read_all_unlocked()?;
        for metadata in entries
            .iter()
            .filter(|metadata| metadata.expires_at_unix_secs <= now)
        {
            self.remove_unlocked(&metadata.backup_id)?;
        }
        entries.retain(|metadata| metadata.expires_at_unix_secs > now);
        self.enforce_budget_unlocked(&mut entries, 0, 0)
    }

    fn validate_ttl(&self, ttl_secs: Option<u64>) -> Result<u64> {
        let ttl = ttl_secs.unwrap_or(self.default_ttl_secs);
        if ttl == 0 || ttl > self.max_ttl_secs {
            return Err(Error::Tool(format!(
                "ttl_secs must be between 1 and {}",
                self.max_ttl_secs
            )));
        }
        Ok(ttl)
    }

    fn save(&self, mut metadata: BackupMetadata, bytes: &[u8]) -> Result<BackupMetadata> {
        if bytes.len() > self.max_file_bytes {
            return Err(Error::Tool(format!(
                "file exceeds managed backup limit: {} bytes > {} bytes",
                bytes.len(),
                self.max_file_bytes
            )));
        }
        let _guard = self.lock()?;
        let mut entries = self.read_all_unlocked()?;
        self.enforce_budget_unlocked(&mut entries, bytes.len() as u64, 1)?;

        for _ in 0..16 {
            let backup_id = random_backup_id();
            let dir = self.root.join(&backup_id);
            if dir.exists() {
                continue;
            }
            metadata.backup_id = backup_id.clone();
            stdfs::create_dir(&dir)?;
            secure_directory(&dir)?;
            let result = (|| -> Result<()> {
                let payload = dir.join("payload.bin");
                stdfs::write(&payload, bytes)?;
                secure_file(&payload)?;
                let metadata_path = dir.join("metadata.json");
                let encoded = serde_json::to_vec_pretty(&metadata)?;
                stdfs::write(&metadata_path, encoded)?;
                secure_file(&metadata_path)?;
                Ok(())
            })();
            if let Err(error) = result {
                let _ = stdfs::remove_dir_all(&dir);
                return Err(error);
            }
            return Ok(metadata);
        }

        Err(Error::Tool(
            "failed to allocate a unique managed backup id".to_string(),
        ))
    }

    fn metadata(&self, backup_id: &str) -> Result<BackupMetadata> {
        validate_backup_id(backup_id)?;
        self.cleanup_now()?;
        let _guard = self.lock()?;
        self.read_metadata_unlocked(backup_id)
    }

    fn load(&self, backup_id: &str) -> Result<(BackupMetadata, Vec<u8>)> {
        validate_backup_id(backup_id)?;
        self.cleanup_now()?;
        let _guard = self.lock()?;
        let metadata = self.read_metadata_unlocked(backup_id)?;
        let bytes = stdfs::read(self.root.join(backup_id).join("payload.bin"))?;
        if bytes.len() != metadata.bytes || sha256_hex(&bytes) != metadata.sha256 {
            return Err(Error::Tool(format!(
                "managed backup {backup_id} failed integrity verification"
            )));
        }
        Ok((metadata, bytes))
    }

    fn list(&self) -> Result<Vec<BackupMetadata>> {
        self.cleanup_now()?;
        let _guard = self.lock()?;
        let mut entries = self.read_all_unlocked()?;
        entries.sort_by(|a, b| {
            b.created_at_unix_secs
                .cmp(&a.created_at_unix_secs)
                .then_with(|| b.backup_id.cmp(&a.backup_id))
        });
        Ok(entries)
    }

    fn delete(&self, backup_id: &str) -> Result<BackupMetadata> {
        validate_backup_id(backup_id)?;
        self.cleanup_now()?;
        let _guard = self.lock()?;
        let metadata = self.read_metadata_unlocked(backup_id)?;
        self.remove_unlocked(backup_id)?;
        Ok(metadata)
    }

    fn read_all_unlocked(&self) -> Result<Vec<BackupMetadata>> {
        let mut entries = Vec::new();
        for entry in stdfs::read_dir(&self.root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let backup_id = entry.file_name().to_string_lossy().to_string();
            if validate_backup_id(&backup_id).is_err() {
                continue;
            }
            match self.read_metadata_unlocked(&backup_id) {
                Ok(metadata) => entries.push(metadata),
                Err(_) => continue,
            }
        }
        Ok(entries)
    }

    fn read_metadata_unlocked(&self, backup_id: &str) -> Result<BackupMetadata> {
        let path = self.root.join(backup_id).join("metadata.json");
        let metadata: BackupMetadata = serde_json::from_slice(&stdfs::read(path)?)?;
        if metadata.version != BACKUP_FORMAT_VERSION || metadata.backup_id != backup_id {
            return Err(Error::Tool(format!(
                "managed backup {backup_id} has incompatible metadata"
            )));
        }
        Ok(metadata)
    }

    fn remove_unlocked(&self, backup_id: &str) -> Result<()> {
        let dir = self.root.join(backup_id);
        if dir.exists() {
            stdfs::remove_dir_all(dir)?;
        }
        Ok(())
    }

    fn enforce_budget_unlocked(
        &self,
        entries: &mut Vec<BackupMetadata>,
        incoming_bytes: u64,
        incoming_entries: usize,
    ) -> Result<()> {
        if incoming_bytes > self.store_max_bytes {
            return Err(Error::Tool(format!(
                "backup exceeds total managed backup budget: {incoming_bytes} bytes > {} bytes",
                self.store_max_bytes
            )));
        }
        entries.sort_by(|a, b| {
            a.created_at_unix_secs
                .cmp(&b.created_at_unix_secs)
                .then_with(|| a.backup_id.cmp(&b.backup_id))
        });
        let mut used = entries.iter().map(|entry| entry.bytes as u64).sum::<u64>();
        if incoming_entries > self.max_entries {
            return Err(Error::Tool(format!(
                "backup entry count exceeds managed backup limit: {incoming_entries} > {}",
                self.max_entries
            )));
        }
        let mut remove_count = 0usize;
        while (used.saturating_add(incoming_bytes) > self.store_max_bytes
            || entries
                .len()
                .saturating_sub(remove_count)
                .saturating_add(incoming_entries)
                > self.max_entries)
            && remove_count < entries.len()
        {
            let metadata = &entries[remove_count];
            self.remove_unlocked(&metadata.backup_id)?;
            used = used.saturating_sub(metadata.bytes as u64);
            remove_count += 1;
        }
        if remove_count > 0 {
            entries.drain(0..remove_count);
        }
        if used.saturating_add(incoming_bytes) > self.store_max_bytes
            || entries.len().saturating_add(incoming_entries) > self.max_entries
        {
            return Err(Error::Tool(
                "managed backup store does not have enough capacity".to_string(),
            ));
        }
        Ok(())
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, ()>> {
        self.gate
            .lock()
            .map_err(|_| Error::Tool("managed backup store lock is poisoned".to_string()))
    }
}

pub fn create(state: &AppState, req: FileBackupRequest) -> Result<FileBackupResponse> {
    state.backups.cleanup_now()?;
    let (target, source) = state.resolve_target(req.target.as_deref())?;
    let config = state.get_target_config(&target)?;
    policy::check_file(&target, config, &req.path, FileAccess::Read, source)?;
    let timeout = Duration::from_millis(
        req.timeout_ms
            .unwrap_or_else(|| policy::target_policy(config).default_timeout_ms),
    );
    let bytes = fs::read_bytes(state, &target, config, &req.path, timeout)?;
    if bytes.len() > state.config.runtime.file_backup_max_file_bytes {
        return Err(Error::Tool(format!(
            "file exceeds managed backup limit: {} bytes > {} bytes",
            bytes.len(),
            state.config.runtime.file_backup_max_file_bytes
        )));
    }
    let note = normalize_note(req.note)?;
    let ttl_secs = state.backups.validate_ttl(req.ttl_secs)?;
    let created_at_unix_secs = unix_now()?;
    let expires_at_unix_secs = created_at_unix_secs.saturating_add(ttl_secs);
    let mode = file_mode(state, &target, config, &req.path, timeout)?;
    let metadata = BackupMetadata {
        version: BACKUP_FORMAT_VERSION,
        backup_id: String::new(),
        target: target.to_string(),
        path: req.path,
        sha256: sha256_hex(&bytes),
        bytes: bytes.len(),
        mode,
        created_at_unix_secs,
        expires_at_unix_secs,
        note,
    };
    let metadata = state.backups.save(metadata, &bytes)?;
    Ok(FileBackupResponse {
        resolved_target: state.resolved_target_value(target, source),
        backup: metadata.to_entry(),
    })
}

pub fn list(state: &AppState, req: FileBackupListRequest) -> Result<FileBackupListResponse> {
    let limit = req.limit.unwrap_or(DEFAULT_LIST_LIMIT);
    if limit == 0 || limit > MAX_LIST_LIMIT {
        return Err(Error::Tool(format!(
            "limit must be between 1 and {MAX_LIST_LIMIT}"
        )));
    }
    let (target, source) = state.resolve_target(req.target.as_deref())?;
    let config = state.get_target_config(&target)?;
    policy::check_target_enabled(&target, config)?;
    if !policy::target_policy(config).allow_file_read {
        return Err(Error::Policy(format!("file read is disabled for {target}")));
    }
    if let Some(path) = &req.path {
        policy::check_file(&target, config, path, FileAccess::Read, source)?;
    }
    let target_name = target.to_string();
    let mut backups = state
        .backups
        .list()?
        .into_iter()
        .filter(|metadata| metadata.target == target_name)
        .filter(|metadata| match req.path.as_deref() {
            Some(path) => metadata.path == path,
            None => true,
        })
        .filter(|metadata| {
            policy::check_file(&target, config, &metadata.path, FileAccess::Read, source).is_ok()
        })
        .map(|metadata| metadata.to_entry())
        .collect::<Vec<_>>();
    let total_matches = backups.len();
    let truncated = total_matches > limit;
    backups.truncate(limit);
    Ok(FileBackupListResponse {
        resolved_target: state.resolved_target_value(target, source),
        backups,
        total_matches,
        truncated,
    })
}

pub fn restore(state: &AppState, req: FileRestoreRequest) -> Result<FileRestoreResponse> {
    let metadata = state.backups.metadata(&req.backup_id)?;
    let backup_target = TargetId::from_str(&metadata.target)?;
    let (target, source) = state.resolve_target(req.target.as_deref())?;
    if target != backup_target {
        return Err(Error::Tool(format!(
            "backup {} belongs to {}, not {}",
            metadata.backup_id, metadata.target, target
        )));
    }
    let config = state.get_target_config(&target)?;
    let path = req.destination.unwrap_or_else(|| metadata.path.clone());
    policy::check_file(&target, config, &path, FileAccess::Write, source)?;
    let (loaded_metadata, bytes) = state.backups.load(&req.backup_id)?;
    if loaded_metadata.sha256 != metadata.sha256 {
        return Err(Error::Tool(
            "managed backup changed while preparing restore; retry the operation".to_string(),
        ));
    }
    let timeout = Duration::from_millis(
        req.timeout_ms
            .unwrap_or_else(|| policy::target_policy(config).default_timeout_ms),
    );
    let exists = fs::file_exists(state, &target, config, &path, timeout)?;
    let previous_sha256 = if exists {
        Some(sha256_hex(&fs::read_bytes(
            state, &target, config, &path, timeout,
        )?))
    } else {
        None
    };
    if let Some(expected) = &req.expected_current_sha256 {
        if previous_sha256.as_deref() != Some(expected.as_str()) {
            return Err(Error::Tool(format!(
                "current sha256 mismatch for {path}: expected {expected}, got {}",
                previous_sha256.as_deref().unwrap_or("<missing>")
            )));
        }
    } else if exists && !req.overwrite {
        return Err(Error::Tool(
            "destination already exists; provide expected_current_sha256 or set overwrite=true"
                .to_string(),
        ));
    }

    fs::write_bytes(
        state,
        &target,
        config,
        &path,
        &bytes,
        metadata.mode,
        timeout,
    )?;

    Ok(FileRestoreResponse {
        resolved_target: state.resolved_target_value(target, source),
        backup_id: metadata.backup_id,
        source_path: metadata.path,
        path,
        created: !exists,
        restored: true,
        previous_sha256,
        restored_sha256: metadata.sha256,
        bytes: metadata.bytes,
        mode: metadata.mode.map(format_mode),
        backup_expires_at_unix_secs: metadata.expires_at_unix_secs,
    })
}

pub fn delete(state: &AppState, req: FileBackupDeleteRequest) -> Result<FileBackupDeleteResponse> {
    let metadata = state.backups.metadata(&req.backup_id)?;
    let backup_target = TargetId::from_str(&metadata.target)?;
    let (target, source) = state.resolve_target(req.target.as_deref())?;
    if target != backup_target {
        return Err(Error::Tool(format!(
            "backup {} belongs to {}, not {}",
            metadata.backup_id, metadata.target, target
        )));
    }
    let config = state.get_target_config(&target)?;
    policy::check_file(&target, config, &metadata.path, FileAccess::Write, source)?;
    let deleted = state.backups.delete(&metadata.backup_id)?;
    Ok(FileBackupDeleteResponse {
        resolved_target: state.resolved_target_value(target, source),
        backup_id: deleted.backup_id,
        path: deleted.path,
        deleted: true,
    })
}

fn normalize_note(note: Option<String>) -> Result<Option<String>> {
    let Some(note) = note else {
        return Ok(None);
    };
    let note = note.trim().to_string();
    if note.is_empty() {
        return Ok(None);
    }
    if note.len() > MAX_BACKUP_NOTE_BYTES {
        return Err(Error::Tool(format!(
            "backup note exceeds {MAX_BACKUP_NOTE_BYTES} bytes"
        )));
    }
    Ok(Some(note))
}

fn validate_backup_id(value: &str) -> Result<()> {
    if value.len() != BACKUP_ID_BYTES * 2 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Error::Tool("invalid managed backup id".to_string()));
    }
    Ok(())
}

fn random_backup_id() -> String {
    let mut bytes = [0u8; BACKUP_ID_BYTES];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unix_now() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| Error::Tool("system clock is before the Unix epoch".to_string()))
}

fn format_mode(mode: u32) -> String {
    format!("{:04o}", mode & 0o7777)
}

fn secure_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        stdfs::set_permissions(path, stdfs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn secure_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        stdfs::set_permissions(path, stdfs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::{Config, TargetConfig};
    use tempfile::tempdir;

    fn test_state() -> (tempfile::TempDir, AppState, PathBuf) {
        test_state_with_max_entries(1024)
    }

    fn test_state_with_max_entries(max_entries: usize) -> (tempfile::TempDir, AppState, PathBuf) {
        let temp = tempdir().unwrap();
        let root = temp.path().join("files");
        stdfs::create_dir_all(&root).unwrap();
        let mut config = Config::default();
        let local = match config.targets.get_mut("local").unwrap() {
            TargetConfig::Local(local) => local,
            _ => unreachable!(),
        };
        local.enabled = true;
        local.policy.allow_file_read = true;
        local.policy.allow_file_write = true;
        local.policy.allowed_roots = vec![root.to_string_lossy().to_string()];
        config.server.default_target = Some("local".to_string());
        config.server.oauth_state_file = None;
        config.server.runtime_dir = temp.path().join("runtime");
        config.runtime.file_backup_cleanup_interval_secs = 1;
        config.runtime.file_backup_max_entries = max_entries;
        let state = AppState::new(config).unwrap();
        (temp, state, root)
    }

    #[test]
    fn managed_backup_restores_content_and_mode() {
        let (_temp, state, root) = test_state();
        let path = root.join("config.txt");
        stdfs::write(&path, b"before\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            stdfs::set_permissions(&path, stdfs::Permissions::from_mode(0o640)).unwrap();
        }
        let backup = create(
            &state,
            FileBackupRequest {
                target: Some("local".to_string()),
                path: path.to_string_lossy().to_string(),
                ttl_secs: Some(120),
                note: Some("before restart".to_string()),
                timeout_ms: None,
            },
        )
        .unwrap();
        stdfs::write(&path, b"after\n").unwrap();
        let current_sha = sha256_hex(&stdfs::read(&path).unwrap());
        let restored = restore(
            &state,
            FileRestoreRequest {
                target: Some("local".to_string()),
                backup_id: backup.backup.backup_id.clone(),
                destination: None,
                expected_current_sha256: Some(current_sha),
                overwrite: false,
                timeout_ms: None,
            },
        )
        .unwrap();
        assert_eq!(stdfs::read(&path).unwrap(), b"before\n");
        assert_eq!(restored.restored_sha256, backup.backup.sha256);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                stdfs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o640
            );
        }
    }

    #[test]
    fn restore_requires_guard_when_destination_exists() {
        let (_temp, state, root) = test_state();
        let path = root.join("guard.txt");
        stdfs::write(&path, b"before").unwrap();
        let backup = create(
            &state,
            FileBackupRequest {
                target: Some("local".to_string()),
                path: path.to_string_lossy().to_string(),
                ttl_secs: None,
                note: None,
                timeout_ms: None,
            },
        )
        .unwrap();
        stdfs::write(&path, b"after").unwrap();
        let error = restore(
            &state,
            FileRestoreRequest {
                target: Some("local".to_string()),
                backup_id: backup.backup.backup_id,
                destination: None,
                expected_current_sha256: None,
                overwrite: false,
                timeout_ms: None,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("destination already exists"));
    }

    #[test]
    fn max_entry_budget_evicts_oldest_even_for_empty_files() {
        let (_temp, state, root) = test_state_with_max_entries(1);
        let first_path = root.join("first-empty.txt");
        let second_path = root.join("second-empty.txt");
        stdfs::write(&first_path, b"").unwrap();
        stdfs::write(&second_path, b"").unwrap();

        let first = create(
            &state,
            FileBackupRequest {
                target: Some("local".to_string()),
                path: first_path.to_string_lossy().to_string(),
                ttl_secs: None,
                note: None,
                timeout_ms: None,
            },
        )
        .unwrap();
        let second = create(
            &state,
            FileBackupRequest {
                target: Some("local".to_string()),
                path: second_path.to_string_lossy().to_string(),
                ttl_secs: None,
                note: None,
                timeout_ms: None,
            },
        )
        .unwrap();

        assert!(state.backups.metadata(&first.backup.backup_id).is_err());
        assert!(state.backups.metadata(&second.backup.backup_id).is_ok());
    }

    #[test]
    fn list_limit_reports_total_matches_and_truncation() {
        let (_temp, state, root) = test_state();
        let path = root.join("listed.txt");
        stdfs::write(&path, b"one").unwrap();
        for note in ["first", "second"] {
            create(
                &state,
                FileBackupRequest {
                    target: Some("local".to_string()),
                    path: path.to_string_lossy().to_string(),
                    ttl_secs: None,
                    note: Some(note.to_string()),
                    timeout_ms: None,
                },
            )
            .unwrap();
        }

        let listed = list(
            &state,
            FileBackupListRequest {
                target: Some("local".to_string()),
                path: Some(path.to_string_lossy().to_string()),
                limit: Some(1),
            },
        )
        .unwrap();
        assert_eq!(listed.backups.len(), 1);
        assert_eq!(listed.total_matches, 2);
        assert!(listed.truncated);
    }

    #[test]
    fn background_cleanup_removes_expired_backups_without_tool_traffic() {
        let (_temp, state, root) = test_state();
        let path = root.join("background-expire.txt");
        stdfs::write(&path, b"content").unwrap();
        let backup = create(
            &state,
            FileBackupRequest {
                target: Some("local".to_string()),
                path: path.to_string_lossy().to_string(),
                ttl_secs: Some(1),
                note: None,
                timeout_ms: None,
            },
        )
        .unwrap();
        let backup_dir = state.backups.root.join(&backup.backup.backup_id);
        assert!(backup_dir.exists());

        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while backup_dir.exists() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }

        assert!(!backup_dir.exists());
        assert_eq!(stdfs::read(&path).unwrap(), b"content");
    }

    #[test]
    fn cleanup_removes_expired_backups_without_touching_source() {
        let (_temp, state, root) = test_state();
        let path = root.join("expire.txt");
        stdfs::write(&path, b"content").unwrap();
        let backup = create(
            &state,
            FileBackupRequest {
                target: Some("local".to_string()),
                path: path.to_string_lossy().to_string(),
                ttl_secs: Some(10),
                note: None,
                timeout_ms: None,
            },
        )
        .unwrap();
        state
            .backups
            .cleanup_at(backup.backup.expires_at_unix_secs)
            .unwrap();
        assert!(stdfs::read(&path).is_ok());
        assert!(state.backups.load(&backup.backup.backup_id).is_err());
    }
}
