use super::edit::{apply_text_edits, EditOutcome, TextEdit};
use crate::{
    core::{
        config::TargetConfig,
        error::{Error, Result},
        policy::{self, FileAccess},
        state::AppState,
        target::{ResolvedTarget, TargetId},
        util::{sha256_hex, truncate_bytes},
    },
    transport::ssh,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    fs,
    io::Write,
    path::Path,
    time::{Duration, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[derive(Debug, Clone, Deserialize)]
pub struct FileReadRequest {
    #[serde(default)]
    pub target: Option<String>,
    pub path: String,
    #[serde(default)]
    pub max_bytes: Option<usize>,
    #[serde(default)]
    pub start_line: Option<usize>,
    #[serde(default)]
    pub end_line: Option<usize>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileReadResponse {
    pub resolved_target: ResolvedTarget,
    pub path: String,
    pub encoding: String,
    pub content: String,
    pub sha256: String,
    pub bytes: usize,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileListRequest {
    #[serde(default)]
    pub target: Option<String>,
    pub path: String,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileListResponse {
    pub resolved_target: ResolvedTarget,
    pub path: String,
    pub entries: Vec<FileEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub kind: String,
    pub size: u64,
    pub modified_unix: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileEditRequest {
    #[serde(default)]
    pub target: Option<String>,
    pub path: String,
    #[serde(default)]
    pub expected_sha256: Option<String>,
    pub edits: Vec<TextEdit>,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileEditResponse {
    pub resolved_target: ResolvedTarget,
    pub path: String,
    pub changed: bool,
    pub written: bool,
    pub old_sha256: String,
    pub new_sha256: String,
    pub diff: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileWriteRequest {
    #[serde(default)]
    pub target: Option<String>,
    pub path: String,
    pub content: String,
    #[serde(default)]
    pub encoding: FileContentEncoding,
    #[serde(default)]
    pub expected_sha256: Option<String>,
    #[serde(default)]
    pub overwrite: bool,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileContentEncoding {
    #[default]
    Utf8,
    Base64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileWriteResponse {
    pub resolved_target: ResolvedTarget,
    pub path: String,
    pub created: bool,
    pub written: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_sha256: Option<String>,
    pub new_sha256: String,
    pub bytes: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FilePatchRequest {
    #[serde(default)]
    pub target: Option<String>,
    pub path: String,
    pub patch: String,
    #[serde(default)]
    pub expected_sha256: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileFindRequest {
    #[serde(default)]
    pub target: Option<String>,
    pub path: String,
    pub pattern: String,
    #[serde(default = "default_true")]
    pub case_sensitive: bool,
    #[serde(default = "default_context_lines")]
    pub context_lines: usize,
    #[serde(default = "default_max_matches")]
    pub max_matches: usize,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileFindResponse {
    pub resolved_target: ResolvedTarget,
    pub path: String,
    pub sha256: String,
    pub matches: Vec<FileFindMatch>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileFindMatch {
    pub line: usize,
    pub text: String,
    pub before: Vec<String>,
    pub after: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileMoveRequest {
    #[serde(default)]
    pub target: Option<String>,
    pub source: String,
    pub destination: String,
    #[serde(default)]
    pub overwrite: bool,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileMoveResponse {
    pub resolved_target: ResolvedTarget,
    pub source: String,
    pub destination: String,
    pub moved: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileChmodRequest {
    #[serde(default)]
    pub target: Option<String>,
    pub path: String,
    pub mode: String,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileChmodResponse {
    pub resolved_target: ResolvedTarget,
    pub path: String,
    pub mode: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DirectoryCreateRequest {
    #[serde(default)]
    pub target: Option<String>,
    pub path: String,
    #[serde(default = "default_true")]
    pub recursive: bool,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DirectoryCreateResponse {
    pub resolved_target: ResolvedTarget,
    pub path: String,
    pub created: bool,
}

pub fn read(state: &AppState, req: FileReadRequest) -> Result<FileReadResponse> {
    let (target, source) = state.resolve_target(req.target.as_deref())?;
    let config = state.get_target_config(&target)?;
    policy::check_file(&target, config, &req.path, FileAccess::Read, source)?;

    let target_policy = policy::target_policy(config);
    let timeout_ms = req.timeout_ms.unwrap_or(target_policy.default_timeout_ms);
    let bytes = read_bytes(
        state,
        &target,
        config,
        &req.path,
        Duration::from_millis(timeout_ms),
    )?;
    let sha256 = sha256_hex(&bytes);
    let original_len = bytes.len();
    let max_bytes = req
        .max_bytes
        .unwrap_or(target_policy.max_output_bytes)
        .min(target_policy.max_output_bytes);

    let (selected, start_line, end_line) = select_line_range(bytes, req.start_line, req.end_line)?;
    let (selected, truncated) = truncate_bytes(selected, Some(max_bytes));
    let (encoding, content) = match String::from_utf8(selected.clone()) {
        Ok(text) => ("utf-8".to_string(), text),
        Err(_) => ("base64".to_string(), BASE64.encode(&selected)),
    };

    Ok(FileReadResponse {
        resolved_target: state.resolved_target_value(target, source),
        path: req.path,
        encoding,
        content,
        sha256,
        bytes: original_len,
        truncated,
        start_line,
        end_line,
    })
}

pub fn list(state: &AppState, req: FileListRequest) -> Result<FileListResponse> {
    let (target, source) = state.resolve_target(req.target.as_deref())?;
    let config = state.get_target_config(&target)?;
    policy::check_file(&target, config, &req.path, FileAccess::Read, source)?;

    let entries = match (target.clone(), config) {
        (TargetId::Local, TargetConfig::Local(_)) => list_local(&req.path)?,
        (TargetId::Ssh(name), TargetConfig::Ssh(ssh_config)) => {
            let timeout = Duration::from_millis(
                req.timeout_ms
                    .unwrap_or_else(|| policy::target_policy(config).default_timeout_ms),
            );
            list_remote(state, &name, ssh_config, &req.path, timeout)?
        }
        _ => {
            return Err(Error::Target(format!(
                "target {target} has mismatched config"
            )))
        }
    };

    Ok(FileListResponse {
        resolved_target: state.resolved_target_value(target, source),
        path: req.path,
        entries,
    })
}

pub fn edit(state: &AppState, req: FileEditRequest) -> Result<FileEditResponse> {
    let (target, source) = state.resolve_target(req.target.as_deref())?;
    let config = state.get_target_config(&target)?;
    policy::check_file(&target, config, &req.path, FileAccess::Write, source)?;

    let timeout = Duration::from_millis(
        req.timeout_ms
            .unwrap_or_else(|| policy::target_policy(config).default_timeout_ms),
    );
    let original_bytes = read_bytes(state, &target, config, &req.path, timeout)?;
    let original = String::from_utf8(original_bytes).map_err(|_| {
        Error::Tool("file_edit currently supports UTF-8 text files only".to_string())
    })?;

    let EditOutcome {
        changed,
        old_sha256,
        new_sha256,
        diff,
        text,
    } = apply_text_edits(&original, req.expected_sha256.as_deref(), &req.edits)?;

    if changed && !req.dry_run {
        write_bytes(
            state,
            &target,
            config,
            &req.path,
            text.as_bytes(),
            None,
            timeout,
        )?;
    }

    Ok(FileEditResponse {
        resolved_target: state.resolved_target_value(target, source),
        path: req.path,
        changed,
        written: changed && !req.dry_run,
        old_sha256,
        new_sha256,
        diff,
    })
}

pub fn write(state: &AppState, req: FileWriteRequest) -> Result<FileWriteResponse> {
    let (target, source) = state.resolve_target(req.target.as_deref())?;
    let config = state.get_target_config(&target)?;
    policy::check_file(&target, config, &req.path, FileAccess::Write, source)?;

    let timeout = Duration::from_millis(
        req.timeout_ms
            .unwrap_or_else(|| policy::target_policy(config).default_timeout_ms),
    );
    let mode = req.mode.as_deref().map(parse_mode).transpose()?;
    let content = match req.encoding {
        FileContentEncoding::Utf8 => req.content.into_bytes(),
        FileContentEncoding::Base64 => BASE64
            .decode(req.content.as_bytes())
            .map_err(|err| Error::Tool(format!("invalid base64 file content: {err}")))?,
    };

    let exists = file_exists(state, &target, config, &req.path, timeout)?;
    let old_bytes = if exists {
        Some(read_bytes(state, &target, config, &req.path, timeout)?)
    } else {
        None
    };
    let old_sha256 = old_bytes.as_deref().map(sha256_hex);

    if let Some(expected) = req.expected_sha256.as_deref() {
        match old_sha256.as_deref() {
            Some(actual) if actual == expected => {}
            Some(actual) => {
                return Err(Error::Tool(format!(
                    "file changed before write: expected sha256 {expected}, got {actual}"
                )))
            }
            None => {
                return Err(Error::Tool(format!(
                    "file does not exist but expected sha256 {expected} was supplied"
                )))
            }
        }
    } else if exists && !req.overwrite {
        return Err(Error::Tool(
            "file already exists; supply expected_sha256 or set overwrite=true".to_string(),
        ));
    }

    let new_sha256 = sha256_hex(&content);
    write_bytes(state, &target, config, &req.path, &content, mode, timeout)?;

    Ok(FileWriteResponse {
        resolved_target: state.resolved_target_value(target, source),
        path: req.path,
        created: !exists,
        written: true,
        old_sha256,
        new_sha256,
        bytes: content.len(),
    })
}

pub fn patch(state: &AppState, req: FilePatchRequest) -> Result<FileEditResponse> {
    let (target, source) = state.resolve_target(req.target.as_deref())?;
    let config = state.get_target_config(&target)?;
    policy::check_file(&target, config, &req.path, FileAccess::Write, source)?;
    let timeout = Duration::from_millis(
        req.timeout_ms
            .unwrap_or_else(|| policy::target_policy(config).default_timeout_ms),
    );
    let original_bytes = read_bytes(state, &target, config, &req.path, timeout)?;
    let original = String::from_utf8(original_bytes).map_err(|_| {
        Error::Tool("file_patch currently supports UTF-8 text files only".to_string())
    })?;
    let old_sha256 = sha256_hex(original.as_bytes());
    if let Some(expected) = req.expected_sha256.as_deref() {
        if expected != old_sha256 {
            return Err(Error::Tool(format!(
                "file changed before patch: expected sha256 {expected}, got {old_sha256}"
            )));
        }
    }

    let patch = diffy::Patch::from_str(&req.patch)
        .map_err(|err| Error::Tool(format!("invalid unified patch: {err}")))?;
    let text = diffy::apply(&original, &patch)
        .map_err(|err| Error::Tool(format!("patch does not apply: {err}")))?;
    let changed = text != original;
    let new_sha256 = sha256_hex(text.as_bytes());
    let diff = if changed { req.patch } else { String::new() };
    if changed && !req.dry_run {
        write_bytes(
            state,
            &target,
            config,
            &req.path,
            text.as_bytes(),
            None,
            timeout,
        )?;
    }

    Ok(FileEditResponse {
        resolved_target: state.resolved_target_value(target, source),
        path: req.path,
        changed,
        written: changed && !req.dry_run,
        old_sha256,
        new_sha256,
        diff,
    })
}

pub fn find(state: &AppState, req: FileFindRequest) -> Result<FileFindResponse> {
    if req.pattern.is_empty() {
        return Err(Error::Tool(
            "file_find pattern must not be empty".to_string(),
        ));
    }
    let (target, source) = state.resolve_target(req.target.as_deref())?;
    let config = state.get_target_config(&target)?;
    policy::check_file(&target, config, &req.path, FileAccess::Read, source)?;
    let timeout = Duration::from_millis(
        req.timeout_ms
            .unwrap_or_else(|| policy::target_policy(config).default_timeout_ms),
    );
    let bytes = read_bytes(state, &target, config, &req.path, timeout)?;
    let sha256 = sha256_hex(&bytes);
    let text = String::from_utf8(bytes)
        .map_err(|_| Error::Tool("file_find supports UTF-8 text files only".to_string()))?;
    let lines: Vec<&str> = text.lines().collect();
    let needle = if req.case_sensitive {
        req.pattern.clone()
    } else {
        req.pattern.to_lowercase()
    };
    let max_matches = req.max_matches.clamp(1, 200);
    let context_lines = req.context_lines.min(20);
    let mut matches = Vec::new();
    let mut truncated = false;

    for (index, line) in lines.iter().enumerate() {
        let haystack = if req.case_sensitive {
            (*line).to_string()
        } else {
            line.to_lowercase()
        };
        if !haystack.contains(&needle) {
            continue;
        }
        if matches.len() >= max_matches {
            truncated = true;
            break;
        }
        let before_start = index.saturating_sub(context_lines);
        let after_end = (index + 1 + context_lines).min(lines.len());
        matches.push(FileFindMatch {
            line: index + 1,
            text: (*line).to_string(),
            before: lines[before_start..index]
                .iter()
                .map(|line| (*line).to_string())
                .collect(),
            after: lines[index + 1..after_end]
                .iter()
                .map(|line| (*line).to_string())
                .collect(),
        });
    }

    Ok(FileFindResponse {
        resolved_target: state.resolved_target_value(target, source),
        path: req.path,
        sha256,
        matches,
        truncated,
    })
}

pub fn move_path(state: &AppState, req: FileMoveRequest) -> Result<FileMoveResponse> {
    let (target, source_kind) = state.resolve_target(req.target.as_deref())?;
    let config = state.get_target_config(&target)?;
    policy::check_file(&target, config, &req.source, FileAccess::Write, source_kind)?;
    policy::check_file(
        &target,
        config,
        &req.destination,
        FileAccess::Write,
        source_kind,
    )?;
    let timeout = Duration::from_millis(
        req.timeout_ms
            .unwrap_or_else(|| policy::target_policy(config).default_timeout_ms),
    );
    let destination_exists = file_exists(state, &target, config, &req.destination, timeout)?;
    if destination_exists && !req.overwrite {
        return Err(Error::Tool(
            "destination already exists; set overwrite=true to replace it".to_string(),
        ));
    }

    match (target.clone(), config) {
        (TargetId::Local, TargetConfig::Local(_)) => {
            if req.overwrite && destination_exists {
                let metadata = fs::symlink_metadata(&req.destination)?;
                if metadata.is_dir() && !metadata.file_type().is_symlink() {
                    return Err(Error::Tool(
                        "refusing to overwrite an existing directory".to_string(),
                    ));
                }
                fs::remove_file(&req.destination)?;
            }
            fs::rename(&req.source, &req.destination)?;
        }
        (TargetId::Ssh(name), TargetConfig::Ssh(ssh_config)) => ssh::move_path(
            &state.ssh_sessions,
            &name,
            ssh_config,
            &req.source,
            &req.destination,
            req.overwrite,
            timeout,
        )?,
        _ => {
            return Err(Error::Target(format!(
                "target {target} has mismatched config"
            )))
        }
    }

    Ok(FileMoveResponse {
        resolved_target: state.resolved_target_value(target, source_kind),
        source: req.source,
        destination: req.destination,
        moved: true,
    })
}

pub fn chmod(state: &AppState, req: FileChmodRequest) -> Result<FileChmodResponse> {
    let (target, source) = state.resolve_target(req.target.as_deref())?;
    let config = state.get_target_config(&target)?;
    policy::check_file(&target, config, &req.path, FileAccess::Write, source)?;
    let mode = parse_mode(&req.mode)?;
    let timeout = Duration::from_millis(
        req.timeout_ms
            .unwrap_or_else(|| policy::target_policy(config).default_timeout_ms),
    );

    match (target.clone(), config) {
        (TargetId::Local, TargetConfig::Local(_)) => {
            #[cfg(unix)]
            fs::set_permissions(&req.path, fs::Permissions::from_mode(mode))?;
            #[cfg(not(unix))]
            return Err(Error::Tool(
                "file_chmod is supported only on Unix local targets".to_string(),
            ));
        }
        (TargetId::Ssh(name), TargetConfig::Ssh(ssh_config)) => ssh::chmod_path(
            &state.ssh_sessions,
            &name,
            ssh_config,
            &req.path,
            mode,
            timeout,
        )?,
        _ => {
            return Err(Error::Target(format!(
                "target {target} has mismatched config"
            )))
        }
    }

    Ok(FileChmodResponse {
        resolved_target: state.resolved_target_value(target, source),
        path: req.path,
        mode: format!("{mode:04o}"),
    })
}

pub fn create_directory(
    state: &AppState,
    req: DirectoryCreateRequest,
) -> Result<DirectoryCreateResponse> {
    let (target, source) = state.resolve_target(req.target.as_deref())?;
    let config = state.get_target_config(&target)?;
    policy::check_file(&target, config, &req.path, FileAccess::Write, source)?;
    let mode = req.mode.as_deref().map(parse_mode).transpose()?;
    let timeout = Duration::from_millis(
        req.timeout_ms
            .unwrap_or_else(|| policy::target_policy(config).default_timeout_ms),
    );
    let exists = file_exists(state, &target, config, &req.path, timeout)?;

    match (target.clone(), config) {
        (TargetId::Local, TargetConfig::Local(_)) => {
            if req.recursive {
                fs::create_dir_all(&req.path)?;
            } else if !exists {
                fs::create_dir(&req.path)?;
            }
            #[cfg(unix)]
            if let Some(mode) = mode {
                fs::set_permissions(&req.path, fs::Permissions::from_mode(mode))?;
            }
        }
        (TargetId::Ssh(name), TargetConfig::Ssh(ssh_config)) => ssh::create_directory(
            &state.ssh_sessions,
            &name,
            ssh_config,
            &req.path,
            req.recursive,
            mode,
            timeout,
        )?,
        _ => {
            return Err(Error::Target(format!(
                "target {target} has mismatched config"
            )))
        }
    }

    Ok(DirectoryCreateResponse {
        resolved_target: state.resolved_target_value(target, source),
        path: req.path,
        created: !exists,
    })
}

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

fn write_bytes(
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

fn file_exists(
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

fn select_line_range(
    bytes: Vec<u8>,
    start_line: Option<usize>,
    end_line: Option<usize>,
) -> Result<(Vec<u8>, Option<usize>, Option<usize>)> {
    if start_line.is_none() && end_line.is_none() {
        return Ok((bytes, None, None));
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| Error::Tool("line-range reads require a UTF-8 text file".to_string()))?;
    let start = start_line.unwrap_or(1);
    if start == 0 {
        return Err(Error::Tool(
            "start_line is 1-based and must be at least 1".to_string(),
        ));
    }
    let end = end_line.unwrap_or(usize::MAX);
    if end < start {
        return Err(Error::Tool(
            "end_line must be greater than or equal to start_line".to_string(),
        ));
    }

    let mut selected = String::new();
    let mut actual_end = None;
    for (index, line) in text.split_inclusive('\n').enumerate() {
        let line_no = index + 1;
        if line_no < start {
            continue;
        }
        if line_no > end {
            break;
        }
        selected.push_str(line);
        actual_end = Some(line_no);
    }
    Ok((
        selected.into_bytes(),
        Some(start),
        actual_end.or(Some(start.saturating_sub(1))),
    ))
}

fn parse_mode(value: &str) -> Result<u32> {
    let value = value.trim();
    if value.is_empty() || value.len() > 4 || !value.chars().all(|ch| matches!(ch, '0'..='7')) {
        return Err(Error::Tool(format!(
            "invalid file mode {value:?}; use an octal string such as 0644 or 0755"
        )));
    }
    u32::from_str_radix(value, 8)
        .map_err(|err| Error::Tool(format!("invalid file mode {value:?}: {err}")))
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

fn default_true() -> bool {
    true
}

fn default_context_lines() -> usize {
    2
}

fn default_max_matches() -> usize {
    20
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn line_range_is_one_based_and_preserves_newlines() {
        let (bytes, start, end) =
            select_line_range(b"one\ntwo\nthree\n".to_vec(), Some(2), Some(3)).unwrap();
        assert_eq!(bytes, b"two\nthree\n");
        assert_eq!(start, Some(2));
        assert_eq!(end, Some(3));
    }

    #[test]
    fn parses_octal_modes() {
        assert_eq!(parse_mode("0755").unwrap(), 0o755);
        assert_eq!(parse_mode("644").unwrap(), 0o644);
        assert!(parse_mode("0899").is_err());
    }

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
