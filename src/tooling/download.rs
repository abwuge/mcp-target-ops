use crate::core::error::{Error, Result};
use rand::{rngs::OsRng, RngCore};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const TOKEN_BYTES: usize = 32;

#[derive(Debug, Clone)]
struct DownloadEntry {
    path: PathBuf,
    file_name: String,
    mime_type: String,
    sha256: String,
    expires_at: SystemTime,
    single_use: bool,
}

#[derive(Debug, Clone)]
pub struct IssuedDownload {
    pub token: String,
    pub expires_at_unix_secs: u64,
}

pub struct DownloadFile {
    pub file: File,
    pub file_name: String,
    pub mime_type: String,
    pub sha256: String,
}

pub struct DownloadRegistry {
    dir: PathBuf,
    entries: Mutex<HashMap<String, DownloadEntry>>,
}

impl DownloadRegistry {
    pub fn new(runtime_dir: &Path) -> Result<Self> {
        let dir = runtime_dir.join("downloads");
        fs::create_dir_all(&dir).map_err(|err| {
            Error::Config(format!(
                "failed to create download staging dir {}: {err}",
                dir.display()
            ))
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).map_err(|err| {
                Error::Config(format!(
                    "failed to secure download staging dir {}: {err}",
                    dir.display()
                ))
            })?;
        }
        purge_dir(&dir)?;
        Ok(Self {
            dir,
            entries: Mutex::new(HashMap::new()),
        })
    }

    pub fn issue(
        &self,
        bytes: &[u8],
        file_name: String,
        mime_type: String,
        sha256: String,
        ttl: Duration,
        single_use: bool,
    ) -> Result<IssuedDownload> {
        if ttl.is_zero() {
            return Err(Error::Tool(
                "download link ttl must be greater than zero".to_string(),
            ));
        }

        let now = SystemTime::now();
        let expires_at = now
            .checked_add(ttl)
            .ok_or_else(|| Error::Tool("download link ttl is too large".to_string()))?;
        let mut entries = self.entries.lock().unwrap();
        cleanup_expired(&mut entries, now);

        for _ in 0..8 {
            let token = random_token();
            if entries.contains_key(&token) {
                continue;
            }
            let path = self.dir.join(format!("{token}.bin"));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = match options.open(&path) {
                Ok(file) => file,
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(Error::Io(err)),
            };
            if let Err(err) = file.write_all(bytes).and_then(|_| file.sync_all()) {
                let _ = fs::remove_file(&path);
                return Err(Error::Io(err));
            }

            entries.insert(
                token.clone(),
                DownloadEntry {
                    path,
                    file_name,
                    mime_type,
                    sha256,
                    expires_at,
                    single_use,
                },
            );
            return Ok(IssuedDownload {
                token,
                expires_at_unix_secs: unix_secs(expires_at),
            });
        }

        Err(Error::Tool(
            "failed to allocate a unique download token".to_string(),
        ))
    }

    pub fn open(&self, token: &str) -> Result<Option<DownloadFile>> {
        if !valid_token(token) {
            return Ok(None);
        }

        let now = SystemTime::now();
        let mut entries = self.entries.lock().unwrap();
        cleanup_expired(&mut entries, now);
        let entry = match entries.get(token).cloned() {
            Some(entry) => entry,
            None => return Ok(None),
        };

        let file = match File::open(&entry.path) {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                entries.remove(token);
                return Ok(None);
            }
            Err(err) => return Err(Error::Io(err)),
        };

        if entry.single_use {
            entries.remove(token);
            // The open file descriptor remains readable on Unix. On platforms that
            // refuse unlinking an open file, the registry entry is still removed and
            // startup cleanup removes the orphaned staging file later.
            let _ = fs::remove_file(&entry.path);
        }

        Ok(Some(DownloadFile {
            file,
            file_name: entry.file_name,
            mime_type: entry.mime_type,
            sha256: entry.sha256,
        }))
    }
}

fn cleanup_expired(entries: &mut HashMap<String, DownloadEntry>, now: SystemTime) {
    let expired: Vec<String> = entries
        .iter()
        .filter_map(|(token, entry)| (entry.expires_at <= now).then_some(token.clone()))
        .collect();
    for token in expired {
        if let Some(entry) = entries.remove(&token) {
            let _ = fs::remove_file(entry.path);
        }
    }
}

fn purge_dir(dir: &Path) -> Result<()> {
    for item in fs::read_dir(dir).map_err(Error::Io)? {
        let path = item.map_err(Error::Io)?.path();
        let metadata = fs::symlink_metadata(&path).map_err(Error::Io)?;
        if metadata.is_dir() {
            fs::remove_dir_all(&path).map_err(Error::Io)?;
        } else {
            fs::remove_file(&path).map_err(Error::Io)?;
        }
    }
    Ok(())
}

fn random_token() -> String {
    let mut bytes = [0_u8; TOKEN_BYTES];
    OsRng.fill_bytes(&mut bytes);
    let mut token = String::with_capacity(TOKEN_BYTES * 2);
    for byte in bytes {
        token.push_str(&format!("{byte:02x}"));
    }
    token
}

fn valid_token(token: &str) -> bool {
    token.len() == TOKEN_BYTES * 2
        && token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn unix_secs(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn issues_random_reusable_downloads() {
        let temp = tempdir().unwrap();
        let registry = DownloadRegistry::new(temp.path()).unwrap();
        let issued = registry
            .issue(
                b"hello",
                "hello.txt".to_string(),
                "text/plain".to_string(),
                "abc".to_string(),
                Duration::from_secs(60),
                false,
            )
            .unwrap();
        assert_eq!(issued.token.len(), 64);
        assert!(valid_token(&issued.token));
        assert!(registry.open(&issued.token).unwrap().is_some());
        assert!(registry.open(&issued.token).unwrap().is_some());
    }

    #[test]
    fn single_use_download_is_claimed_once() {
        let temp = tempdir().unwrap();
        let registry = DownloadRegistry::new(temp.path()).unwrap();
        let issued = registry
            .issue(
                b"hello",
                "hello.txt".to_string(),
                "text/plain".to_string(),
                "abc".to_string(),
                Duration::from_secs(60),
                true,
            )
            .unwrap();
        assert!(registry.open(&issued.token).unwrap().is_some());
        assert!(registry.open(&issued.token).unwrap().is_none());
    }

    #[test]
    fn rejects_malformed_tokens_without_touching_disk() {
        let temp = tempdir().unwrap();
        let registry = DownloadRegistry::new(temp.path()).unwrap();
        assert!(registry.open("../secret").unwrap().is_none());
        assert!(registry.open(&"A".repeat(64)).unwrap().is_none());
    }
}
