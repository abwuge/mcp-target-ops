use crate::{
    core::{
        config::TargetConfig,
        error::{Error, Result},
        policy,
        state::AppState,
        target::{ResolvedTarget, TargetId},
    },
    tooling::stream::RingBuffer,
    transport::ssh,
};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::{Read, Write},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
};

static TERMINAL_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Deserialize)]
pub struct TerminalOpenRequest {
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub shell: Option<String>,
    #[serde(default = "default_rows")]
    pub rows: u16,
    #[serde(default = "default_cols")]
    pub cols: u16,
}

#[derive(Debug, Clone, Serialize)]
pub struct TerminalOpenResponse {
    pub resolved_target: ResolvedTarget,
    pub terminal_id: String,
    pub rows: u16,
    pub cols: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TerminalSendRequest {
    pub terminal_id: String,
    pub input: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TerminalSendResponse {
    pub terminal_id: String,
    pub bytes_written: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TerminalReadRequest {
    pub terminal_id: String,
    #[serde(default)]
    pub since_seq: Option<u64>,
    #[serde(default)]
    pub max_bytes: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TerminalReadResponse {
    pub terminal_id: String,
    pub target: String,
    pub from_seq: u64,
    pub next_seq: u64,
    pub output: String,
    pub truncated: bool,
    pub eof: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TerminalResizeRequest {
    pub terminal_id: String,
    pub rows: u16,
    pub cols: u16,
}

#[derive(Debug, Clone, Serialize)]
pub struct TerminalResizeResponse {
    pub terminal_id: String,
    pub rows: u16,
    pub cols: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TerminalCloseRequest {
    pub terminal_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TerminalCloseResponse {
    pub terminal_id: String,
    pub closed: bool,
}

pub struct TerminalRegistry {
    sessions: Mutex<HashMap<String, Arc<TerminalSession>>>,
    default_buffer_bytes: usize,
}

pub struct TerminalSession {
    target: TargetId,
    writer: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    child: Mutex<Box<dyn Child + Send>>,
    buffer: Arc<RingBuffer>,
    eof: Arc<Mutex<bool>>,
}

impl TerminalRegistry {
    pub fn new(default_buffer_bytes: usize) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            default_buffer_bytes,
        }
    }

    pub fn open(&self, state: &AppState, req: TerminalOpenRequest) -> Result<TerminalOpenResponse> {
        let (target, source) = state.resolve_target(req.target.as_deref())?;
        let config = state.get_target_config(&target)?;
        policy::check_terminal(&target, config)?;

        let (program, args, cwd) = match (target.clone(), config) {
            (TargetId::Local, TargetConfig::Local(local)) => {
                let shell = req
                    .shell
                    .clone()
                    .or_else(|| local.shell.clone())
                    .or_else(|| std::env::var("SHELL").ok())
                    .unwrap_or_else(|| "sh".to_string());
                (shell, Vec::new(), req.cwd.clone())
            }
            (TargetId::Ssh(_), TargetConfig::Ssh(ssh_config)) => {
                let (program, args) = ssh::terminal_program_and_args(
                    ssh_config,
                    req.cwd.as_deref(),
                    req.shell.as_deref(),
                );
                (program, args, None)
            }
            _ => {
                return Err(Error::Target(format!(
                    "target {target} has mismatched config"
                )))
            }
        };

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: req.rows,
                cols: req.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|err| Error::Terminal(err.to_string()))?;

        let mut cmd = CommandBuilder::new(program);
        for arg in args {
            cmd.arg(arg);
        }
        if let Some(cwd) = cwd {
            cmd.cwd(cwd);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|err| Error::Terminal(err.to_string()))?;
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|err| Error::Terminal(err.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|err| Error::Terminal(err.to_string()))?;

        let id = format!("term_{}", TERMINAL_COUNTER.fetch_add(1, Ordering::Relaxed));
        let buffer = Arc::new(RingBuffer::new(self.default_buffer_bytes));
        let eof = Arc::new(Mutex::new(false));
        let reader_buffer = Arc::clone(&buffer);
        let reader_eof = Arc::clone(&eof);

        thread::spawn(move || {
            let mut scratch = [0_u8; 8192];
            loop {
                match reader.read(&mut scratch) {
                    Ok(0) => {
                        *reader_eof.lock().unwrap() = true;
                        break;
                    }
                    Ok(n) => reader_buffer.push(&scratch[..n]),
                    Err(_) => {
                        *reader_eof.lock().unwrap() = true;
                        break;
                    }
                }
            }
        });

        let session = Arc::new(TerminalSession {
            target: target.clone(),
            writer: Mutex::new(writer),
            master: Mutex::new(pair.master),
            child: Mutex::new(child),
            buffer,
            eof,
        });

        self.sessions.lock().unwrap().insert(id.clone(), session);

        Ok(TerminalOpenResponse {
            resolved_target: state.resolved_target_value(target, source),
            terminal_id: id,
            rows: req.rows,
            cols: req.cols,
        })
    }

    pub fn send(&self, req: TerminalSendRequest) -> Result<TerminalSendResponse> {
        let session = self.get(&req.terminal_id)?;
        let mut writer = session.writer.lock().unwrap();
        writer.write_all(req.input.as_bytes())?;
        writer.flush()?;
        Ok(TerminalSendResponse {
            terminal_id: req.terminal_id,
            bytes_written: req.input.len(),
        })
    }

    pub fn read(&self, req: TerminalReadRequest) -> Result<TerminalReadResponse> {
        let session = self.get(&req.terminal_id)?;
        let from_seq = req.since_seq.unwrap_or(0);
        let (output, next_seq, truncated) = session
            .buffer
            .read_since(from_seq, req.max_bytes.unwrap_or(64 * 1024));
        let eof = *session.eof.lock().unwrap();
        Ok(TerminalReadResponse {
            terminal_id: req.terminal_id,
            target: session.target.to_string(),
            from_seq,
            next_seq,
            output: String::from_utf8_lossy(&output).to_string(),
            truncated,
            eof,
        })
    }

    pub fn resize(&self, req: TerminalResizeRequest) -> Result<TerminalResizeResponse> {
        let session = self.get(&req.terminal_id)?;
        session
            .master
            .lock()
            .unwrap()
            .resize(PtySize {
                rows: req.rows,
                cols: req.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|err| Error::Terminal(err.to_string()))?;
        Ok(TerminalResizeResponse {
            terminal_id: req.terminal_id,
            rows: req.rows,
            cols: req.cols,
        })
    }

    pub fn close(&self, req: TerminalCloseRequest) -> Result<TerminalCloseResponse> {
        let session = self.sessions.lock().unwrap().remove(&req.terminal_id);
        if let Some(session) = session {
            let _ = session.child.lock().unwrap().kill();
            Ok(TerminalCloseResponse {
                terminal_id: req.terminal_id,
                closed: true,
            })
        } else {
            Ok(TerminalCloseResponse {
                terminal_id: req.terminal_id,
                closed: false,
            })
        }
    }

    pub fn ids(&self) -> Vec<String> {
        self.sessions.lock().unwrap().keys().cloned().collect()
    }

    fn get(&self, id: &str) -> Result<Arc<TerminalSession>> {
        self.sessions
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| Error::Terminal(format!("terminal {id} not found")))
    }
}

fn default_rows() -> u16 {
    30
}

fn default_cols() -> u16 {
    120
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::{Config, LocalTargetConfig, PolicyConfig, TargetConfig};

    #[cfg(unix)]
    #[test]
    fn resizes_live_local_pty() {
        let mut config = Config::default();
        config.targets.insert(
            "local".to_string(),
            TargetConfig::Local(LocalTargetConfig {
                enabled: true,
                shell: Some("sh".to_string()),
                policy: PolicyConfig {
                    allow_terminal: true,
                    allowed_roots: vec!["/tmp".to_string()],
                    ..PolicyConfig::default()
                },
            }),
        );
        let state = AppState::new(config).unwrap();
        let opened = state
            .terminals
            .open(
                &state,
                TerminalOpenRequest {
                    target: Some("local".to_string()),
                    cwd: Some("/tmp".to_string()),
                    shell: Some("sh".to_string()),
                    rows: 20,
                    cols: 80,
                },
            )
            .unwrap();
        let resized = state
            .terminals
            .resize(TerminalResizeRequest {
                terminal_id: opened.terminal_id.clone(),
                rows: 40,
                cols: 120,
            })
            .unwrap();
        assert_eq!(resized.rows, 40);
        assert_eq!(resized.cols, 120);
        state
            .terminals
            .close(TerminalCloseRequest {
                terminal_id: opened.terminal_id,
            })
            .unwrap();
    }
}
