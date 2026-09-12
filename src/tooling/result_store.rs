use crate::core::error::{Error, Result};
use rand::RngCore;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

const SESSIONS_DIR: &str = "sessions";
const ACTIVITY_FILE: &str = ".activity";
// COMPAT(COMPAT-002): Flat result files from the pre-session cache layout remain
// readable and share one synthetic activity marker until that migration path is removed.
const LEGACY_ACTIVITY_FILE: &str = ".legacy-activity";

#[derive(Debug, Serialize)]
pub struct ResultReadResponse {
    pub found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Debug, Clone)]
struct ResultLocation {
    path: PathBuf,
    session_bucket: Option<String>,
}

struct SessionUsage {
    session_bucket: Option<String>,
    bytes: u64,
    result_ids: Vec<String>,
    fallback_activity: SystemTime,
}

pub struct ResultStore {
    dir: PathBuf,
    sessions_dir: PathBuf,
    max_bytes: u64,
    index: Mutex<HashMap<String, ResultLocation>>,
}

impl ResultStore {
    pub fn new(runtime_dir: &Path, max_bytes: u64) -> Result<Self> {
        let dir = runtime_dir.join("results");
        let sessions_dir = dir.join(SESSIONS_DIR);
        fs::create_dir_all(&sessions_dir).map_err(|err| {
            Error::Config(format!(
                "failed to create result cache {}: {err}",
                sessions_dir.display()
            ))
        })?;
        let store = Self {
            index: Mutex::new(scan_index(&dir, &sessions_dir)?),
            dir,
            sessions_dir,
            max_bytes,
        };
        store.cleanup()?;
        Ok(store)
    }

    pub fn touch_session(&self, caller_key: &str) -> Result<()> {
        let bucket = session_bucket(caller_key);
        let session_dir = self.sessions_dir.join(bucket);
        if session_dir.is_dir() {
            touch_activity(&session_dir.join(ACTIVITY_FILE))?;
        }
        Ok(())
    }

    pub fn store(&self, caller_key: &str, data: &Value) -> Result<String> {
        let result_id = random_result_id();
        let bucket = session_bucket(caller_key);
        let session_dir = self.sessions_dir.join(&bucket);
        fs::create_dir_all(&session_dir)?;
        touch_activity(&session_dir.join(ACTIVITY_FILE))?;

        let bytes = serde_json::to_vec(data)?;
        let path = session_dir.join(format!("{result_id}.json"));
        let temp = path.with_extension(format!("{}.tmp", std::process::id()));
        fs::write(&temp, bytes)?;
        fs::rename(&temp, &path)?;
        self.index.lock().unwrap().insert(
            result_id.clone(),
            ResultLocation {
                path,
                session_bucket: Some(bucket),
            },
        );
        self.cleanup()?;
        Ok(result_id)
    }

    pub fn read_for_caller(&self, caller_key: &str, result_id: &str) -> Result<ResultReadResponse> {
        self.touch_session(caller_key)?;
        if !valid_result_id(result_id) {
            return Ok(ResultReadResponse {
                found: false,
                data: None,
            });
        }

        let location = self.index.lock().unwrap().get(result_id).cloned();
        let Some(location) = location else {
            return Ok(ResultReadResponse {
                found: false,
                data: None,
            });
        };

        let bytes = match fs::read(&location.path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                self.index.lock().unwrap().remove(result_id);
                return Ok(ResultReadResponse {
                    found: false,
                    data: None,
                });
            }
            Err(err) => return Err(err.into()),
        };
        self.touch_location(&location)?;
        let data = serde_json::from_slice(&bytes)?;
        Ok(ResultReadResponse {
            found: true,
            data: Some(data),
        })
    }

    #[cfg(test)]
    fn read(&self, result_id: &str) -> Result<ResultReadResponse> {
        self.read_for_caller("direct", result_id)
    }

    fn touch_location(&self, location: &ResultLocation) -> Result<()> {
        if let Some(bucket) = location.session_bucket.as_deref() {
            touch_activity(&self.sessions_dir.join(bucket).join(ACTIVITY_FILE))
        } else {
            touch_activity(&self.dir.join(LEGACY_ACTIVITY_FILE))
        }
    }

    fn cleanup(&self) -> Result<()> {
        let mut index = self.index.lock().unwrap();
        let mut stale = Vec::new();
        let mut groups: HashMap<Option<String>, SessionUsage> = HashMap::new();
        let mut total = 0_u64;

        for (result_id, location) in index.iter() {
            let metadata = match fs::metadata(&location.path) {
                Ok(metadata) => metadata,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    stale.push(result_id.clone());
                    continue;
                }
                Err(err) => return Err(err.into()),
            };
            let size = metadata.len();
            let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
            total = total.saturating_add(size);
            let usage = groups
                .entry(location.session_bucket.clone())
                .or_insert_with(|| SessionUsage {
                    session_bucket: location.session_bucket.clone(),
                    bytes: 0,
                    result_ids: Vec::new(),
                    fallback_activity: UNIX_EPOCH,
                });
            usage.bytes = usage.bytes.saturating_add(size);
            usage.result_ids.push(result_id.clone());
            usage.fallback_activity = usage.fallback_activity.max(modified);
        }
        for result_id in stale {
            index.remove(&result_id);
        }

        if total <= self.max_bytes {
            return Ok(());
        }

        let mut usages = groups.into_values().collect::<Vec<_>>();
        usages.sort_by_key(|usage| self.activity_time(usage));
        for usage in usages {
            self.remove_session_usage(&usage, &mut index)?;
            total = total.saturating_sub(usage.bytes);
            if total <= self.max_bytes {
                break;
            }
        }
        Ok(())
    }

    fn activity_time(&self, usage: &SessionUsage) -> SystemTime {
        let activity_path = match usage.session_bucket.as_deref() {
            Some(bucket) => self.sessions_dir.join(bucket).join(ACTIVITY_FILE),
            None => self.dir.join(LEGACY_ACTIVITY_FILE),
        };
        fs::metadata(activity_path)
            .and_then(|metadata| metadata.modified())
            .unwrap_or(usage.fallback_activity)
    }

    fn remove_session_usage(
        &self,
        usage: &SessionUsage,
        index: &mut HashMap<String, ResultLocation>,
    ) -> Result<()> {
        if let Some(bucket) = usage.session_bucket.as_deref() {
            let path = self.sessions_dir.join(bucket);
            match fs::remove_dir_all(path) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(err.into()),
            }
        } else {
            for result_id in &usage.result_ids {
                if let Some(location) = index.get(result_id) {
                    match fs::remove_file(&location.path) {
                        Ok(()) => {}
                        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                        Err(err) => return Err(err.into()),
                    }
                }
            }
            let _ = fs::remove_file(self.dir.join(LEGACY_ACTIVITY_FILE));
        }
        for result_id in &usage.result_ids {
            index.remove(result_id);
        }
        Ok(())
    }
}

fn scan_index(dir: &Path, sessions_dir: &Path) -> Result<HashMap<String, ResultLocation>> {
    let mut index = HashMap::new();
    // COMPAT(COMPAT-002): Index legacy runtime/results/*.json files written before
    // the session-scoped layout so old conversation cards can still restore them.
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        if let Some(result_id) = result_id_from_path(&path) {
            index.insert(
                result_id,
                ResultLocation {
                    path,
                    session_bucket: None,
                },
            );
        }
    }

    for session in fs::read_dir(sessions_dir)? {
        let session = session?;
        let session_path = session.path();
        if !session_path.is_dir() {
            continue;
        }
        let Some(bucket) = session.file_name().to_str().map(str::to_string) else {
            continue;
        };
        for entry in fs::read_dir(&session_path)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|value| value.to_str()) != Some("json")
            {
                continue;
            }
            if let Some(result_id) = result_id_from_path(&path) {
                index.insert(
                    result_id,
                    ResultLocation {
                        path,
                        session_bucket: Some(bucket.clone()),
                    },
                );
            }
        }
    }
    Ok(index)
}

fn result_id_from_path(path: &Path) -> Option<String> {
    let value = path.file_stem()?.to_str()?;
    valid_result_id(value).then(|| value.to_string())
}

fn touch_activity(path: &Path) -> Result<()> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .to_string();
    fs::write(path, now)?;
    Ok(())
}

fn session_bucket(caller_key: &str) -> String {
    let digest = Sha256::digest(caller_key.as_bytes());
    let mut result = String::with_capacity(64);
    use std::fmt::Write;
    for byte in digest {
        let _ = write!(result, "{byte:02x}");
    }
    result
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
    use std::{thread, time::Duration};
    use tempfile::tempdir;

    #[test]
    fn stores_and_reads_by_result_id() {
        let temp = tempdir().unwrap();
        let store = ResultStore::new(temp.path(), 100 * 1024 * 1024).unwrap();
        let result_id = store
            .store("session:a", &json!({"stdout":"hello"}))
            .unwrap();
        assert_eq!(result_id.len(), 32);
        let result = store.read(&result_id).unwrap();
        assert!(result.found);
        assert_eq!(result.data, Some(json!({"stdout":"hello"})));
    }

    #[test]
    fn invalid_result_ids_are_rejected() {
        let temp = tempdir().unwrap();
        let store = ResultStore::new(temp.path(), 100 * 1024 * 1024).unwrap();
        assert!(!store.read("../other-result").unwrap().found);
    }

    #[test]
    fn evicts_whole_oldest_session_when_size_limit_is_exceeded() {
        let temp = tempdir().unwrap();
        let store = ResultStore::new(temp.path(), 220).unwrap();
        let a1 = store
            .store("session:a", &json!({"payload":"a".repeat(70)}))
            .unwrap();
        let a2 = store
            .store("session:a", &json!({"payload":"b".repeat(70)}))
            .unwrap();
        thread::sleep(Duration::from_millis(20));
        let b = store
            .store("session:b", &json!({"payload":"c".repeat(70)}))
            .unwrap();

        assert!(!store.read(&a1).unwrap().found);
        assert!(!store.read(&a2).unwrap().found);
        assert!(store.read(&b).unwrap().found);
    }

    #[test]
    fn touching_session_refreshes_lru_activity() {
        let temp = tempdir().unwrap();
        let store = ResultStore::new(temp.path(), 200).unwrap();
        let a = store
            .store("session:a", &json!({"payload":"a".repeat(70)}))
            .unwrap();
        thread::sleep(Duration::from_millis(20));
        let b = store
            .store("session:b", &json!({"payload":"b".repeat(70)}))
            .unwrap();
        thread::sleep(Duration::from_millis(20));
        store.touch_session("session:a").unwrap();
        thread::sleep(Duration::from_millis(20));
        let c = store
            .store("session:c", &json!({"payload":"c".repeat(70)}))
            .unwrap();

        assert!(store.read(&a).unwrap().found);
        assert!(!store.read(&b).unwrap().found);
        assert!(store.read(&c).unwrap().found);
    }

    #[test]
    fn legacy_flat_cache_remains_readable_without_ttl() {
        let temp = tempdir().unwrap();
        let result_dir = temp.path().join("results");
        fs::create_dir_all(&result_dir).unwrap();
        let result_id = "0123456789abcdef0123456789abcdef";
        fs::write(
            result_dir.join(format!("{result_id}.json")),
            serde_json::to_vec(&json!({"legacy":true})).unwrap(),
        )
        .unwrap();

        let store = ResultStore::new(temp.path(), 100 * 1024 * 1024).unwrap();
        let result = store.read_for_caller("session:new", result_id).unwrap();
        assert!(result.found);
        assert_eq!(result.data, Some(json!({"legacy":true})));
    }
}
