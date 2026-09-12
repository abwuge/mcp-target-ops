use crate::core::error::{Error, Result};
use rand::RngCore;
use serde::Serialize;
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const RESULT_TTL: Duration = Duration::from_secs(60 * 60);
const RESULT_MAX_BYTES: u64 = 100 * 1024 * 1024;

#[derive(Debug, Serialize)]
pub struct ResultReadResponse {
    pub found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

pub struct ResultStore {
    dir: PathBuf,
}

impl ResultStore {
    pub fn new(runtime_dir: &Path) -> Result<Self> {
        let dir = runtime_dir.join("results");
        fs::create_dir_all(&dir).map_err(|err| {
            Error::Config(format!(
                "failed to create result cache {}: {err}",
                dir.display()
            ))
        })?;
        Ok(Self { dir })
    }

    pub fn store(&self, data: &Value) -> Result<String> {
        self.cleanup()?;
        let result_id = random_result_id();
        let bytes = serde_json::to_vec(data)?;
        let path = self.path_for(&result_id);
        let temp = path.with_extension(format!("{}.tmp", std::process::id()));
        fs::write(&temp, bytes)?;
        fs::rename(&temp, &path)?;
        self.cleanup()?;
        Ok(result_id)
    }

    pub fn read(&self, result_id: &str) -> Result<ResultReadResponse> {
        if !valid_result_id(result_id) {
            return Ok(ResultReadResponse {
                found: false,
                data: None,
            });
        }
        self.cleanup()?;
        let path = self.path_for(result_id);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ResultReadResponse {
                    found: false,
                    data: None,
                });
            }
            Err(err) => return Err(err.into()),
        };
        let data = serde_json::from_slice(&bytes)?;
        Ok(ResultReadResponse {
            found: true,
            data: Some(data),
        })
    }

    fn path_for(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.json"))
    }

    fn cleanup(&self) -> Result<()> {
        let now = SystemTime::now();
        let mut entries = Vec::new();
        let mut total = 0_u64;
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let metadata = entry.metadata()?;
            let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
            if now.duration_since(modified).unwrap_or_default() > RESULT_TTL {
                let _ = fs::remove_file(path);
                continue;
            }
            total = total.saturating_add(metadata.len());
            entries.push((modified, metadata.len(), path));
        }
        if total <= RESULT_MAX_BYTES {
            return Ok(());
        }
        entries.sort_by_key(|(modified, _, _)| *modified);
        for (_, size, path) in entries {
            let _ = fs::remove_file(path);
            total = total.saturating_sub(size);
            if total <= RESULT_MAX_BYTES {
                break;
            }
        }
        Ok(())
    }
}

fn random_result_id() -> String {
    let mut bytes = [0_u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    let mut result = String::with_capacity(32);
    use std::fmt::Write;
    for byte in bytes {
        let _ = write!(result, "{byte:02x}");
    }
    result
}

fn valid_result_id(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn stores_and_reads_by_result_id() {
        let temp = tempdir().unwrap();
        let store = ResultStore::new(temp.path()).unwrap();
        let result_id = store.store(&json!({"stdout":"hello"})).unwrap();
        assert_eq!(result_id.len(), 32);
        let result = store.read(&result_id).unwrap();
        assert!(result.found);
        assert_eq!(result.data, Some(json!({"stdout":"hello"})));
    }

    #[test]
    fn invalid_result_ids_are_rejected() {
        let temp = tempdir().unwrap();
        let store = ResultStore::new(temp.path()).unwrap();
        assert!(!store.read("../other-result").unwrap().found);
    }
}
