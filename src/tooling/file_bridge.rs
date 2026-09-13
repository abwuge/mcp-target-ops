use crate::{
    core::{
        config::{FileExportDelivery, FILE_EXPORT_LINK_MAX_TTL_SECS, FILE_TRANSFER_HARD_MAX_BYTES},
        error::{Error, Result},
        policy::{self, FileAccess},
        state::AppState,
        target::TargetId,
        util::sha256_hex,
    },
    tooling::fs::{self, FileContentEncoding, FileWriteRequest, FileWriteResponse},
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use reqwest::{blocking::Client, redirect::Policy, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{io::Read, path::Path, time::Duration};

#[derive(Debug, Clone, Deserialize)]
pub struct FileImportRequest {
    #[serde(default)]
    pub target: Option<String>,
    pub path: String,
    pub file: Value,
    #[serde(default)]
    pub expected_sha256: Option<String>,
    #[serde(default)]
    pub overwrite: bool,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub max_bytes: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileImportResponse {
    pub source: FileImportSource,
    #[serde(flatten)]
    pub write: FileWriteResponse,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileImportSource {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    pub bytes: usize,
    pub sha256: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileExportRequest {
    #[serde(default)]
    pub target: Option<String>,
    pub path: String,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub delivery: Option<FileExportDelivery>,
    #[serde(default)]
    pub link_ttl_secs: Option<u64>,
    #[serde(default)]
    pub single_use: Option<bool>,
    #[serde(default)]
    pub max_bytes: Option<usize>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileExportResponse {
    pub resolved_target: crate::core::target::ResolvedTarget,
    pub path: String,
    pub delivery: FileExportDelivery,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<ExportedFile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download: Option<DownloadLink>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExportedFile {
    pub file_name: String,
    pub mime_type: String,
    pub bytes: usize,
    pub sha256: String,
    pub data_base64: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DownloadLink {
    pub url: String,
    pub file_name: String,
    pub mime_type: String,
    pub bytes: usize,
    pub sha256: String,
    pub expires_at_unix_secs: u64,
    pub single_use: bool,
}

#[derive(Debug)]
enum ImportLocation {
    Url(String),
    LocalPath(String),
}

#[derive(Debug)]
struct ImportDescriptor {
    location: ImportLocation,
    file_name: Option<String>,
    mime_type: Option<String>,
}

pub fn import(state: &AppState, req: FileImportRequest) -> Result<FileImportResponse> {
    let limit = transfer_limit(
        req.max_bytes,
        state.config.runtime.file_transfer_default_max_bytes,
    )?;
    let descriptor = import_descriptor(&req.file)?;
    let timeout_ms = req
        .timeout_ms
        .unwrap_or(state.config.runtime.file_download_timeout_ms);
    let bytes = match &descriptor.location {
        ImportLocation::Url(url) => download_bytes(url, limit, timeout_ms)?,
        ImportLocation::LocalPath(path) => read_local_connector_file(state, path, limit)?,
    };
    let source_sha256 = sha256_hex(&bytes);

    let write = fs::write(
        state,
        FileWriteRequest {
            target: req.target,
            path: req.path,
            content: BASE64.encode(&bytes),
            encoding: FileContentEncoding::Base64,
            expected_sha256: req.expected_sha256,
            overwrite: req.overwrite,
            mode: req.mode,
            timeout_ms: req.timeout_ms,
        },
    )?;

    Ok(FileImportResponse {
        source: FileImportSource {
            kind: match descriptor.location {
                ImportLocation::Url(_) => "download_url".to_string(),
                ImportLocation::LocalPath(_) => "local_path".to_string(),
            },
            file_name: descriptor.file_name,
            mime_type: descriptor.mime_type,
            bytes: bytes.len(),
            sha256: source_sha256,
        },
        write,
    })
}

pub fn export(state: &AppState, req: FileExportRequest) -> Result<FileExportResponse> {
    let limit = transfer_limit(
        req.max_bytes,
        state.config.runtime.file_transfer_default_max_bytes,
    )?;
    let delivery = req
        .delivery
        .unwrap_or(state.config.runtime.file_export_delivery);
    let (target, source) = state.resolve_target(req.target.as_deref())?;
    let config = state.get_target_config(&target)?;
    policy::check_file(&target, config, &req.path, FileAccess::Read, source)?;
    let timeout_ms = req
        .timeout_ms
        .unwrap_or_else(|| policy::target_policy(config).default_timeout_ms);
    let bytes = fs::read_bytes(
        state,
        &target,
        config,
        &req.path,
        Duration::from_millis(timeout_ms),
    )?;
    if bytes.len() > limit {
        return Err(Error::Tool(format!(
            "file exceeds export limit: {} bytes > {limit} bytes",
            bytes.len()
        )));
    }

    let file_name = Path::new(&req.path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("export.bin")
        .to_string();
    let mime_type = req
        .mime_type
        .unwrap_or_else(|| infer_mime_type(&file_name).to_string());
    validate_mime_type(&mime_type)?;
    let sha256 = sha256_hex(&bytes);

    let (file, download) = match delivery {
        // COMPAT(COMPAT-009): Keep the original embedded attachment payload as an
        // explicit opt-in while link delivery is the default.
        FileExportDelivery::Attachment => (
            Some(ExportedFile {
                file_name: file_name.clone(),
                mime_type: mime_type.clone(),
                bytes: bytes.len(),
                sha256: sha256.clone(),
                data_base64: BASE64.encode(&bytes),
            }),
            None,
        ),
        FileExportDelivery::Link => {
            let ttl_secs = req
                .link_ttl_secs
                .unwrap_or(state.config.runtime.file_export_link_ttl_secs);
            if ttl_secs == 0 || ttl_secs > FILE_EXPORT_LINK_MAX_TTL_SECS {
                return Err(Error::Tool(format!(
                    "link_ttl_secs must be between 1 and {FILE_EXPORT_LINK_MAX_TTL_SECS}"
                )));
            }
            let single_use = req
                .single_use
                .unwrap_or(state.config.runtime.file_export_link_single_use);
            let base_url = download_base_url(state)?;
            let issued = state.downloads.issue(
                &bytes,
                file_name.clone(),
                mime_type.clone(),
                sha256.clone(),
                Duration::from_secs(ttl_secs),
                single_use,
            )?;
            (
                None,
                Some(DownloadLink {
                    url: format!("{base_url}/downloads/{}", issued.token),
                    file_name,
                    mime_type,
                    bytes: bytes.len(),
                    sha256,
                    expires_at_unix_secs: issued.expires_at_unix_secs,
                    single_use,
                }),
            )
        }
    };

    Ok(FileExportResponse {
        resolved_target: state.resolved_target_value(target, source),
        path: req.path,
        delivery,
        file,
        download,
    })
}

fn download_base_url(state: &AppState) -> Result<String> {
    let base_url = state.config.server.public_base_url.as_deref().ok_or_else(|| {
        Error::Tool(
            "link delivery requires server.public_base_url; configure the public HTTPS origin or request delivery=attachment"
                .to_string(),
        )
    })?;
    let parsed = Url::parse(base_url)
        .map_err(|err| Error::Tool(format!("invalid server.public_base_url: {err}")))?;
    let local_http = parsed.scheme() == "http"
        && matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "::1"));
    if parsed.scheme() != "https" && !local_http {
        return Err(Error::Tool(
            "link delivery requires an HTTPS public_base_url (plain HTTP is allowed only for localhost testing)"
                .to_string(),
        ));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(Error::Tool(
            "server.public_base_url must not contain credentials".to_string(),
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(Error::Tool(
            "server.public_base_url must not contain a query or fragment".to_string(),
        ));
    }
    Ok(base_url.trim_end_matches('/').to_string())
}

fn transfer_limit(requested: Option<usize>, default_limit: usize) -> Result<usize> {
    let limit = requested.unwrap_or(default_limit);
    if limit == 0 || limit > FILE_TRANSFER_HARD_MAX_BYTES {
        return Err(Error::Tool(format!(
            "max_bytes must be between 1 and {FILE_TRANSFER_HARD_MAX_BYTES}"
        )));
    }
    Ok(limit)
}

fn import_descriptor(file: &Value) -> Result<ImportDescriptor> {
    if let Some(value) = file
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(ImportDescriptor {
            location: if value.starts_with("https://") {
                ImportLocation::Url(value.to_string())
            } else {
                ImportLocation::LocalPath(value.to_string())
            },
            file_name: None,
            mime_type: None,
        });
    }

    let object = file.as_object().ok_or_else(|| {
        Error::Tool("file must be a connector file reference object or string".to_string())
    })?;
    let string_field = |names: &[&str]| {
        names.iter().find_map(|name| {
            object
                .get(*name)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
    };

    let location = if let Some(url) = string_field(&["download_url", "url"]) {
        ImportLocation::Url(url)
    } else if let Some(path) = string_field(&["local_path", "file_path", "mount_path", "path"]) {
        ImportLocation::LocalPath(path)
    } else {
        return Err(Error::Tool(
            "file reference has neither download_url/url nor local_path/file_path/mount_path/path"
                .to_string(),
        ));
    };

    Ok(ImportDescriptor {
        location,
        file_name: string_field(&["file_name", "filename", "name"]),
        mime_type: string_field(&["mime_type", "mimeType"]),
    })
}

fn download_bytes(url: &str, limit: usize, timeout_ms: u64) -> Result<Vec<u8>> {
    let parsed = Url::parse(url).map_err(|err| Error::Tool(format!("invalid file URL: {err}")))?;
    if parsed.scheme() != "https" {
        return Err(Error::Tool("file download URL must use https".to_string()));
    }

    let client = Client::builder()
        .redirect(Policy::none())
        .timeout(Duration::from_millis(timeout_ms))
        .build()
        .map_err(|err| Error::Tool(format!("build file download client: {err}")))?;
    let mut response = client
        .get(parsed)
        .send()
        .map_err(|err| Error::Tool(format!("download connector file: {err}")))?;
    if !response.status().is_success() {
        return Err(Error::Tool(format!(
            "download connector file returned HTTP {}",
            response.status()
        )));
    }
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(Error::Tool(format!(
            "connector file exceeds import limit of {limit} bytes"
        )));
    }

    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| Error::Tool(format!("read connector file download: {err}")))?;
    if bytes.len() > limit {
        return Err(Error::Tool(format!(
            "connector file exceeds import limit of {limit} bytes"
        )));
    }
    Ok(bytes)
}

fn read_local_connector_file(state: &AppState, path: &str, limit: usize) -> Result<Vec<u8>> {
    let target = TargetId::Local;
    let config = state.get_target_config(&target)?;
    policy::check_file(
        &target,
        config,
        path,
        FileAccess::Read,
        crate::core::target::TargetSource::Explicit,
    )?;
    let bytes = fs::read_bytes(
        state,
        &target,
        config,
        path,
        Duration::from_millis(policy::target_policy(config).default_timeout_ms),
    )?;
    if bytes.len() > limit {
        return Err(Error::Tool(format!(
            "connector file exceeds import limit of {limit} bytes"
        )));
    }
    Ok(bytes)
}

fn validate_mime_type(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 256
        || value.trim() != value
        || !value.bytes().all(|byte| matches!(byte, 0x21..=0x7e))
        || !value.contains('/')
    {
        return Err(Error::Tool(
            "mime_type must be a non-empty visible ASCII media type without whitespace or control characters"
                .to_string(),
        ));
    }
    Ok(())
}

fn infer_mime_type(file_name: &str) -> &'static str {
    match Path::new(file_name)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "txt" | "log" => "text/plain",
        "md" => "text/markdown",
        "json" => "application/json",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "tar" => "application/x-tar",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accepts_openai_style_file_reference() {
        let descriptor = import_descriptor(&json!({
            "download_url": "https://example.com/input.dat",
            "file_name": "input.dat",
            "mime_type": "application/octet-stream"
        }))
        .unwrap();
        assert!(matches!(descriptor.location, ImportLocation::Url(_)));
        assert_eq!(descriptor.file_name.as_deref(), Some("input.dat"));
    }

    #[test]
    fn transfer_limit_is_bounded() {
        let default_limit = 25 * 1024 * 1024;
        assert_eq!(transfer_limit(None, default_limit).unwrap(), default_limit);
        assert!(transfer_limit(Some(0), default_limit).is_err());
        assert!(transfer_limit(Some(FILE_TRANSFER_HARD_MAX_BYTES + 1), default_limit).is_err());
    }

    #[test]
    fn infers_common_mime_types() {
        assert_eq!(infer_mime_type("report.pdf"), "application/pdf");
        assert_eq!(infer_mime_type("data.bin"), "application/octet-stream");
    }

    #[test]
    fn rejects_unsafe_mime_type_overrides() {
        assert!(validate_mime_type("application/zip").is_ok());
        assert!(validate_mime_type("text/plain;charset=utf-8").is_ok());
        assert!(validate_mime_type("text/plain\r\nX-Evil: 1").is_err());
        assert!(validate_mime_type(" text/plain").is_err());
        assert!(validate_mime_type("not-a-media-type").is_err());
    }
}
