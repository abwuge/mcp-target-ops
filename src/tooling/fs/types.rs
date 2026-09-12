use super::super::edit::TextEdit;
use crate::core::target::ResolvedTarget;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct FileReadRequest {
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub files: Vec<FileReadSpec>,
    #[serde(default)]
    pub max_bytes: Option<usize>,
    #[serde(default)]
    pub start_line: Option<usize>,
    #[serde(default)]
    pub end_line: Option<usize>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileReadSpec {
    pub path: String,
    #[serde(default)]
    pub max_bytes: Option<usize>,
    #[serde(default)]
    pub start_line: Option<usize>,
    #[serde(default)]
    pub end_line: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum FileReadResponse {
    Single(FileReadSingleResponse),
    Batch(FileReadBatchResponse),
}

#[derive(Debug, Clone, Serialize)]
pub struct FileReadSingleResponse {
    pub resolved_target: ResolvedTarget,
    #[serde(flatten)]
    pub file: FileReadItemResponse,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileReadItemResponse {
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

#[derive(Debug, Clone, Serialize)]
pub struct FileReadBatchResponse {
    pub resolved_target: ResolvedTarget,
    pub requested_count: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub truncated: bool,
    pub files: Vec<FileReadBatchItemResponse>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileReadBatchItemResponse {
    pub index: usize,
    pub path: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<usize>,
    #[serde(skip_serializing_if = "is_false")]
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
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
    pub encoding: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    pub diff: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileDeleteRequest {
    #[serde(default)]
    pub target: Option<String>,
    pub path: String,
    #[serde(default)]
    pub expected_sha256: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileDeleteResponse {
    pub resolved_target: ResolvedTarget,
    pub path: String,
    pub deleted: bool,
    pub written: bool,
    pub old_sha256: String,
    pub bytes: usize,
    pub encoding: String,
    pub content: String,
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

#[derive(Debug, Clone, Serialize)]
pub struct FilePatchResponse {
    pub resolved_target: ResolvedTarget,
    pub path: String,
    pub changed: bool,
    pub written: bool,
    pub old_sha256: Option<String>,
    pub new_sha256: Option<String>,
    pub diff: String,
    pub files: Vec<FilePatchFileResponse>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FilePatchFileResponse {
    pub path: String,
    pub changed: bool,
    pub written: bool,
    pub old_sha256: String,
    pub new_sha256: String,
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

fn default_true() -> bool {
    true
}

fn default_context_lines() -> usize {
    2
}

fn default_max_matches() -> usize {
    20
}
