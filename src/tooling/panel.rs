//! Bounded, persistent conversation indexes. Payloads remain in the result cache.
use crate::core::{
    error::{Error, Result},
    state::AppState,
    util::sha256_hex,
};
use crate::tooling::job::{JobOutputRequest, JobPollRequest};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

const MAX_PANELS: usize = 128;
const MAX_ENTRIES: usize = 200;

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    seq: u64,
    result_id: String,
    tool: String,
    label: String,
    status: String,
    caller: String,
    job_id: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Panel {
    generation: String,
    owner: String,
    version: u64,
    entries: Vec<Entry>,
}

pub struct PanelStore {
    dir: PathBuf,
    panels: Mutex<HashMap<String, Panel>>,
}

impl PanelStore {
    pub fn new(runtime_dir: &Path) -> Result<Self> {
        let dir = runtime_dir.join("panels");
        fs::create_dir_all(&dir)?;
        let mut panels = HashMap::new();
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if path.extension().and_then(|s| s.to_str()) == Some("json") && valid_id(id) {
                let panel = serde_json::from_slice::<Panel>(&fs::read(&path)?)?;
                panels.insert(id.to_string(), panel);
            }
        }
        Ok(Self {
            dir,
            panels: Mutex::new(panels),
        })
    }

    pub fn open(&self, owner: &str, scope: &str) -> Result<Value> {
        let id = format!(
            "panel_{}",
            sha256_hex(format!("{owner}\0{scope}").as_bytes())
        );
        let mut panels = self.panels.lock().unwrap();
        if !panels.contains_key(&id) {
            if panels.len() >= MAX_PANELS {
                let oldest = panels
                    .keys()
                    .min_by_key(|key| {
                        fs::metadata(self.dir.join(format!("{key}.json")))
                            .and_then(|m| m.modified())
                            .ok()
                    })
                    .cloned();
                if let Some(oldest) = oldest {
                    fs::remove_file(self.dir.join(format!("{oldest}.json")))?;
                    panels.remove(&oldest);
                }
            }
            let panel = Panel {
                generation: format!(
                    "{:016x}{:016x}",
                    rand::random::<u64>(),
                    rand::random::<u64>()
                ),
                owner: sha256_hex(owner.as_bytes()),
                version: 0,
                entries: Vec::new(),
            };
            self.save(&id, &panel)?;
            panels.insert(id.clone(), panel);
        }
        Ok(json!({"panel_id": id, "version": panels[&id].version}))
    }

    pub fn record(
        &self,
        owner: &str,
        scope: &str,
        caller: &str,
        tool: &str,
        result_id: &str,
        data: &Value,
    ) -> Result<()> {
        let opened = self.open(owner, scope)?;
        let id = opened["panel_id"].as_str().unwrap();
        let mut panels = self.panels.lock().unwrap();
        let panel = panels
            .get_mut(id)
            .ok_or_else(|| Error::Tool("Panel expired".into()))?;
        panel.version += 1;
        panel.entries.push(Entry {
            seq: panel.version,
            result_id: result_id.to_string(),
            tool: tool.to_string(),
            label: data
                .get("command")
                .and_then(Value::as_str)
                .or_else(|| data.get("path").and_then(Value::as_str))
                .unwrap_or(tool)
                .chars()
                .take(160)
                .collect(),
            status: result_status(data),
            caller: caller.to_string(),
            job_id: data
                .get("job_id")
                .and_then(Value::as_str)
                .map(str::to_string),
        });
        if panel.entries.len() > MAX_ENTRIES {
            panel.entries.remove(0);
        }
        self.save(id, panel)
    }

    fn save(&self, id: &str, panel: &Panel) -> Result<()> {
        let path = self.dir.join(format!("{id}.json"));
        let temp = path.with_extension("tmp");
        fs::write(&temp, serde_json::to_vec(panel)?)?;
        fs::rename(temp, path)?;
        Ok(())
    }

    pub fn read(&self, state: &AppState, owner: &str, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Request {
            panel_id: String,
            #[serde(default)]
            after: u64,
            result_id: Option<String>,
            generation: Option<String>,
        }
        let req: Request = serde_json::from_value(args)?;
        let panel = self
            .panels
            .lock()
            .unwrap()
            .get(&req.panel_id)
            .filter(|p| p.owner == sha256_hex(owner.as_bytes()))
            .cloned()
            .ok_or_else(|| {
                Error::Tool("Panel unavailable. Open the operation panel again.".into())
            })?;
        let reset = req
            .generation
            .as_deref()
            .is_some_and(|g| g != panel.generation)
            || req.after > panel.version;
        let after = if reset { 0 } else { req.after };
        let mut entries = Vec::new();
        // Running entries are replayed so their status can change without a new operation.
        for entry in &panel.entries {
            if entry.seq <= after && entry.job_id.is_none() {
                continue;
            }
            let status = entry
                .job_id
                .as_ref()
                .map(|id| {
                    state
                        .jobs
                        .poll_for_caller(JobPollRequest { job_id: id.clone() }, &entry.caller)
                        .map(|s| s.status)
                        .unwrap_or_else(|_| "unavailable".into())
                })
                .unwrap_or_else(|| entry.status.clone());
            entries.push(json!({"seq":entry.seq,"result_id":entry.result_id,"tool":entry.tool,"label":entry.label,"status":status}));
        }
        let detail = if let Some(id) = req.result_id {
            if let Some(entry) = panel.entries.iter().find(|e| e.result_id == id) {
                let cached = state.results.read_for_caller(&entry.caller, &id)?;
                let mut data = cached.data;
                if let Some(job_id) = &entry.job_id {
                    match state.jobs.output_for_caller(
                        JobOutputRequest {
                            job_id: job_id.clone(),
                            stdout_since_seq: None,
                            stderr_since_seq: None,
                            max_bytes: Some(state.config.runtime.stream_max_bytes),
                        },
                        &entry.caller,
                    ) {
                        Ok(output) => {
                            let mut live = serde_json::to_value(output)?;
                            live["command"] = data
                                .as_ref()
                                .and_then(|v| v.get("command"))
                                .cloned()
                                .unwrap_or_else(|| json!(entry.label));
                            data = Some(live);
                        }
                        Err(_) => {
                            data = Some(
                                json!({"error":"Job output unavailable; the job may have expired or the server restarted."}),
                            );
                        }
                    }
                }
                json!({"result_id":id,"tool":entry.tool,"data":data,"found":data.is_some()})
            } else {
                json!({"result_id":id,"data":null,"found":false})
            }
        } else {
            Value::Null
        };
        Ok(
            json!({"panel_id":req.panel_id,"version":panel.version,"generation":panel.generation,"reset":reset,
            "oldest_seq":panel.entries.first().map(|e| e.seq).unwrap_or(0),
            "entries":entries,"detail":detail}),
        )
    }
}

fn valid_id(id: &str) -> bool {
    id.strip_prefix("panel_")
        .is_some_and(|s| s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit()))
}

fn result_status(data: &Value) -> String {
    if data.get("error").is_some()
        || data["isError"] == true
        || data["failed"].as_u64().unwrap_or(0) > 0
        || data["exit_code"].as_i64().is_some_and(|code| code != 0)
    {
        return "failed".into();
    }
    if data["timed_out"] == true {
        return "timed_out".into();
    }
    data["status"].as_str().unwrap_or("completed").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::Config;
    #[test]
    fn recreated_panel_resets_a_stale_generation() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.server.runtime_dir = temp.path().to_path_buf();
        config.server.oauth_state_file = None;
        let state = AppState::new(config).unwrap();
        let id = state.panels.open("owner", "chat").unwrap()["panel_id"]
            .as_str()
            .unwrap()
            .to_string();
        let original = state
            .panels
            .read(&state, "owner", json!({"panel_id":id}))
            .unwrap();
        state.panels.panels.lock().unwrap().remove(&id);
        state.panels.open("owner", "chat").unwrap();
        let refreshed = state
            .panels
            .read(
                &state,
                "owner",
                json!({"panel_id":id,"generation":original["generation"]}),
            )
            .unwrap();
        assert_eq!(refreshed["reset"], true);
        assert_ne!(refreshed["generation"], original["generation"]);
    }
}
