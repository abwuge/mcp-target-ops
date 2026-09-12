mod backend;
mod patch_format;
mod types;

pub(crate) use self::backend::read_bytes;
pub use self::types::*;

use self::backend::{file_exists, list_entries, write_bytes};

use super::edit::{apply_text_edits, unified_diff, EditOutcome};
use crate::{
    core::{
        config::TargetConfig,
        error::{Error, Result},
        policy::{self, FileAccess},
        state::AppState,
        target::TargetId,
        util::{sha256_hex, truncate_bytes},
    },
    transport::ssh,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use std::{collections::HashSet, fs, time::Duration};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[derive(Debug)]
struct PreparedFilePatch {
    path: String,
    original: String,
    text: String,
    old_sha256: String,
    new_sha256: String,
    changed: bool,
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

    let timeout = Duration::from_millis(
        req.timeout_ms
            .unwrap_or_else(|| policy::target_policy(config).default_timeout_ms),
    );
    let entries = list_entries(state, &target, config, &req.path, timeout)?;

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
    let new_text = std::str::from_utf8(&content).ok();
    let diff = old_bytes
        .as_deref()
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .zip(new_text)
        .map(|(old, new)| unified_diff(old, new))
        .unwrap_or_default();
    let (encoding, ui_content) = match new_text {
        Some(text) => ("utf-8".to_string(), (!exists).then(|| text.to_string())),
        None => (
            "base64".to_string(),
            (!exists).then(|| BASE64.encode(&content)),
        ),
    };

    write_bytes(state, &target, config, &req.path, &content, mode, timeout)?;

    Ok(FileWriteResponse {
        resolved_target: state.resolved_target_value(target, source),
        path: req.path,
        created: !exists,
        written: true,
        old_sha256,
        new_sha256,
        bytes: content.len(),
        encoding,
        content: ui_content,
        diff,
    })
}

pub fn delete(state: &AppState, req: FileDeleteRequest) -> Result<FileDeleteResponse> {
    let (target, source) = state.resolve_target(req.target.as_deref())?;
    let config = state.get_target_config(&target)?;
    policy::check_file(&target, config, &req.path, FileAccess::Write, source)?;

    let timeout = Duration::from_millis(
        req.timeout_ms
            .unwrap_or_else(|| policy::target_policy(config).default_timeout_ms),
    );
    let bytes = read_bytes(state, &target, config, &req.path, timeout)?;
    let old_sha256 = sha256_hex(&bytes);
    if let Some(expected) = req.expected_sha256.as_deref() {
        if expected != old_sha256 {
            return Err(Error::Tool(format!(
                "file changed before delete: expected sha256 {expected}, got {old_sha256}"
            )));
        }
    }

    let (encoding, content) = match String::from_utf8(bytes.clone()) {
        Ok(text) => ("utf-8".to_string(), text),
        Err(_) => ("base64".to_string(), BASE64.encode(&bytes)),
    };

    if !req.dry_run {
        match (target.clone(), config) {
            (TargetId::Local, TargetConfig::Local(_)) => fs::remove_file(&req.path)?,
            (TargetId::Ssh(name), TargetConfig::Ssh(ssh_config)) => {
                ssh::remove_file(&state.ssh_sessions, &name, ssh_config, &req.path, timeout)?
            }
            _ => {
                return Err(Error::Target(format!(
                    "target {target} has mismatched config"
                )))
            }
        }
    }

    Ok(FileDeleteResponse {
        resolved_target: state.resolved_target_value(target, source),
        path: req.path,
        deleted: !req.dry_run,
        written: !req.dry_run,
        old_sha256,
        bytes: bytes.len(),
        encoding,
        content,
    })
}

pub fn patch(state: &AppState, req: FilePatchRequest) -> Result<FilePatchResponse> {
    let (target, source) = state.resolve_target(req.target.as_deref())?;
    let config = state.get_target_config(&target)?;
    let timeout = Duration::from_millis(
        req.timeout_ms
            .unwrap_or_else(|| policy::target_policy(config).default_timeout_ms),
    );

    let sections = patch_format::split(&req.patch)?;
    if sections.len() <= 1 {
        policy::check_file(&target, config, &req.path, FileAccess::Write, source)?;
        let patch_text = sections
            .first()
            .map(|section| section.patch.as_str())
            .unwrap_or(req.patch.as_str());
        let prepared = prepare_file_patch(
            state,
            &target,
            config,
            &req.path,
            patch_text,
            req.expected_sha256.as_deref(),
            timeout,
        )?;
        if prepared.changed && !req.dry_run {
            write_bytes(
                state,
                &target,
                config,
                &prepared.path,
                prepared.text.as_bytes(),
                None,
                timeout,
            )?;
        }
        let changed = prepared.changed;
        let old_sha256 = prepared.old_sha256.clone();
        let new_sha256 = prepared.new_sha256.clone();
        let files = vec![patch_file_response(&prepared, !req.dry_run)];
        return Ok(FilePatchResponse {
            resolved_target: state.resolved_target_value(target, source),
            path: req.path,
            changed,
            written: changed && !req.dry_run,
            old_sha256: Some(old_sha256),
            new_sha256: Some(new_sha256),
            diff: if changed { req.patch } else { String::new() },
            files,
        });
    }

    if req.expected_sha256.is_some() {
        return Err(Error::Tool(
            "expected_sha256 is only valid for single-file patches".to_string(),
        ));
    }

    let mut prepared_files = Vec::with_capacity(sections.len());
    let mut seen = HashSet::new();
    for section in &sections {
        let patch_path = patch_format::resolve_path(&req.path, section)?;
        if !seen.insert(patch_path.clone()) {
            return Err(Error::Tool(format!(
                "multi-file patch contains duplicate target path {patch_path:?}"
            )));
        }
        policy::check_file(&target, config, &patch_path, FileAccess::Write, source)?;
        prepared_files.push(prepare_file_patch(
            state,
            &target,
            config,
            &patch_path,
            &section.patch,
            None,
            timeout,
        )?);
    }

    let changed = prepared_files.iter().any(|file| file.changed);
    if !req.dry_run {
        let mut written = Vec::new();
        for (index, prepared) in prepared_files.iter().enumerate() {
            if !prepared.changed {
                continue;
            }
            if let Err(write_err) = write_bytes(
                state,
                &target,
                config,
                &prepared.path,
                prepared.text.as_bytes(),
                None,
                timeout,
            ) {
                let mut rollback_errors = Vec::new();
                for previous_index in written.into_iter().rev() {
                    let previous: &PreparedFilePatch = &prepared_files[previous_index];
                    if let Err(rollback_err) = write_bytes(
                        state,
                        &target,
                        config,
                        &previous.path,
                        previous.original.as_bytes(),
                        None,
                        timeout,
                    ) {
                        rollback_errors.push(format!("{}: {rollback_err}", previous.path));
                    }
                }
                let rollback_note = if rollback_errors.is_empty() {
                    "previous writes were rolled back".to_string()
                } else {
                    format!("rollback also failed for {}", rollback_errors.join(", "))
                };
                return Err(Error::Tool(format!(
                    "multi-file patch write failed for {}: {write_err}; {rollback_note}",
                    prepared.path
                )));
            }
            written.push(index);
        }
    }
    let files = prepared_files
        .iter()
        .map(|prepared| patch_file_response(prepared, !req.dry_run))
        .collect();

    Ok(FilePatchResponse {
        resolved_target: state.resolved_target_value(target, source),
        path: req.path,
        changed,
        written: changed && !req.dry_run,
        old_sha256: None,
        new_sha256: None,
        diff: if changed { req.patch } else { String::new() },
        files,
    })
}

fn prepare_file_patch(
    state: &AppState,
    target: &TargetId,
    config: &TargetConfig,
    path: &str,
    patch_text: &str,
    expected_sha256: Option<&str>,
    timeout: Duration,
) -> Result<PreparedFilePatch> {
    let original_bytes = read_bytes(state, target, config, path, timeout)?;
    let original = String::from_utf8(original_bytes).map_err(|_| {
        Error::Tool(format!(
            "file_patch currently supports UTF-8 text files only: {path}"
        ))
    })?;
    let old_sha256 = sha256_hex(original.as_bytes());
    if let Some(expected) = expected_sha256 {
        if expected != old_sha256 {
            return Err(Error::Tool(format!(
                "file changed before patch: expected sha256 {expected}, got {old_sha256}"
            )));
        }
    }

    let patch = diffy::Patch::from_str(patch_text)
        .map_err(|err| Error::Tool(format!("invalid unified patch for {path}: {err}")))?;
    let text = diffy::apply(&original, &patch)
        .map_err(|err| Error::Tool(format!("patch does not apply to {path}: {err}")))?;
    let changed = text != original;
    let new_sha256 = sha256_hex(text.as_bytes());

    Ok(PreparedFilePatch {
        path: path.to_string(),
        original,
        text,
        old_sha256,
        new_sha256,
        changed,
    })
}

fn patch_file_response(
    prepared: &PreparedFilePatch,
    writes_enabled: bool,
) -> FilePatchFileResponse {
    FilePatchFileResponse {
        path: prepared.path.clone(),
        changed: prepared.changed,
        written: prepared.changed && writes_enabled,
        old_sha256: prepared.old_sha256.clone(),
        new_sha256: prepared.new_sha256.clone(),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
