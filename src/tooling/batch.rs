use crate::{
    core::{
        error::{Error, Result},
        policy,
        state::AppState,
        target::ResolvedTarget,
    },
    tooling::{
        command_session::CommandSession,
        exec::{ExecBatchCommandDetails, ExecBatchItemResponse, ExecBatchMode, ExecBatchRequest},
        job::{ExecStartRequest, JobRegistry},
    },
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Condvar, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

static GROUP_COUNTER: AtomicU64 = AtomicU64::new(1);

struct BatchProgress {
    results: Vec<Option<ExecBatchItemResponse>>,
    active: HashMap<usize, Arc<CommandSession>>,
    done: bool,
    finished_elapsed_ms: Option<u64>,
}

struct BatchExecution<'a> {
    jobs: &'a JobRegistry,
    state: &'a AppState,
    target: &'a str,
    secrets: &'a std::collections::BTreeMap<String, crate::core::secret::SecretRef>,
    default_timeout: Option<u64>,
    default_output: Option<usize>,
}

pub struct BatchGroup {
    pub caller: String,
    pub target: ResolvedTarget,
    pub mode: ExecBatchMode,
    started: Instant,
    cancelled: AtomicBool,
    state: Mutex<BatchProgress>,
    changed: Condvar,
}

impl BatchGroup {
    fn new(caller: String, target: ResolvedTarget, mode: ExecBatchMode, count: usize) -> Self {
        Self {
            caller,
            target,
            mode,
            started: Instant::now(),
            cancelled: AtomicBool::new(false),
            state: Mutex::new(BatchProgress {
                results: vec![None; count],
                active: HashMap::new(),
                done: false,
                finished_elapsed_ms: None,
            }),
            changed: Condvar::new(),
        }
    }

    fn finish_one(&self, index: usize, result: ExecBatchItemResponse) {
        let mut state = self.state.lock().unwrap();
        state.results[index] = Some(result);
        state.active.remove(&index);
        self.changed.notify_all();
    }

    fn execute(
        &self,
        context: &BatchExecution<'_>,
        index: usize,
        command: ExecBatchCommandDetails,
    ) -> bool {
        if self.cancelled.load(Ordering::Acquire) {
            return false;
        }
        let started = Instant::now();
        let request = ExecStartRequest {
            target: Some(context.target.to_string()),
            command: command.command,
            cwd: command.cwd,
            timeout_ms: command.timeout_ms.or(context.default_timeout),
            max_output_bytes: command.max_output_bytes.or(context.default_output),
            secret_env: context.secrets.clone(),
        };
        let outcome = match context.jobs.create_batch_session(context.state, &request) {
            Ok(session) => {
                {
                    let mut guard = self.state.lock().unwrap();
                    guard.active.insert(index, Arc::clone(&session));
                }
                if self.cancelled.load(Ordering::Acquire) {
                    session.cancel();
                }
                let output = session.wait();
                let status = session.status_snapshot();
                let success = output.exit_code == Some(0) && !output.timed_out && !status.cancelled;
                ExecBatchItemResponse {
                    index,
                    exit_code: output.exit_code,
                    stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                    stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                    stdout_truncated: output.stdout_truncated,
                    stderr_truncated: output.stderr_truncated,
                    timed_out: output.timed_out,
                    error: None,
                    elapsed_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                    success,
                }
            }
            Err(error) => ExecBatchItemResponse {
                index,
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                stdout_truncated: false,
                stderr_truncated: false,
                timed_out: false,
                error: Some(error.to_string()),
                elapsed_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                success: false,
            },
        };
        let success = outcome.success;
        self.finish_one(index, outcome);
        success
    }

    fn run(
        self: &Arc<Self>,
        state: Arc<AppState>,
        request: ExecBatchRequest,
        default_timeout: Option<u64>,
        default_output: Option<usize>,
    ) {
        let target = self.target.target.to_string();
        let secrets = request.secret_env;
        let context = BatchExecution {
            jobs: &state.jobs,
            state: &state,
            target: &target,
            secrets: &secrets,
            default_timeout,
            default_output,
        };
        if matches!(self.mode, ExecBatchMode::Parallel) {
            thread::scope(|scope| {
                for (index, command) in request.commands.into_iter().enumerate() {
                    let group = Arc::clone(self);
                    let context = &context;
                    scope.spawn(move || {
                        group.execute(context, index, command.into_details());
                    });
                }
            });
        } else {
            for (index, command) in request.commands.into_iter().enumerate() {
                if self.cancelled.load(Ordering::Acquire) {
                    break;
                }
                let success = self.execute(&context, index, command.into_details());
                if !success && request.stop_on_error {
                    break;
                }
            }
        }
        {
            let mut progress = self.state.lock().unwrap();
            progress.done = true;
            progress.finished_elapsed_ms =
                Some(self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64);
        }
        self.changed.notify_all();
    }

    pub fn wait(&self, duration: Duration) -> bool {
        let guard = self.state.lock().unwrap();
        let (guard, _) = self
            .changed
            .wait_timeout_while(guard, duration, |p| !p.done)
            .unwrap();
        guard.done
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        let sessions: Vec<_> = self
            .state
            .lock()
            .unwrap()
            .active
            .values()
            .cloned()
            .collect();
        for session in sessions {
            session.cancel();
        }
        self.changed.notify_all();
    }

    pub fn report(&self, id: &str, include_output: bool, max_bytes: usize) -> Value {
        let guard = self.state.lock().unwrap();
        // A batch may have 32 children, each with a large capture. Bound the
        // aggregate returned bytes, not only individual subprocess buffers.
        let mut remaining_bytes = max_bytes;
        let results: Vec<Value> = guard
            .results
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                if let Some(item) = item {
                    let mut result = serde_json::to_value(item).unwrap_or_else(|_| json!({}));
                    if let Some(object) = result.as_object_mut() {
                        if include_output {
                            for (stream, truncated_flag) in [
                                ("stdout", "stdout_truncated"),
                                ("stderr", "stderr_truncated"),
                            ] {
                                let Some(original) = object.get(stream).and_then(Value::as_str)
                                else {
                                    continue;
                                };
                                let mut length = original.len().min(remaining_bytes);
                                while !original.is_char_boundary(length) {
                                    length -= 1;
                                }
                                let cut = length < original.len();
                                let selected = original[..length].to_owned();
                                remaining_bytes -= length;
                                object.insert(stream.to_string(), json!(selected));
                                if cut {
                                    object.insert(truncated_flag.to_string(), json!(true));
                                }
                            }
                        } else {
                            object.remove("stdout");
                            object.remove("stderr");
                        }
                    }
                    return Some(result);
                }
                guard.active.get(&index).map(|session| {
                    let status = session.status_snapshot();
                    let delta = session.output_since(0, 0, remaining_bytes.max(1));
                    let mut value = json!({
                        "index": index, "status": "running",
                        "elapsed_ms": status.elapsed_ms,
                        "stdout_next_seq": delta.stdout_next_seq,
                        "stderr_next_seq": delta.stderr_next_seq
                    });
                    if include_output {
                        // Read as bytes, so no UTF-8 boundary can make the
                        // aggregate output larger than the requested budget.
                        let take_stdout = delta.stdout.len().min(remaining_bytes);
                        let stdout =
                            String::from_utf8_lossy(&delta.stdout[..take_stdout]).to_string();
                        remaining_bytes -= take_stdout;
                        let take_stderr = delta.stderr.len().min(remaining_bytes);
                        let stderr =
                            String::from_utf8_lossy(&delta.stderr[..take_stderr]).to_string();
                        remaining_bytes -= take_stderr;
                        value["stdout"] = json!(stdout);
                        value["stderr"] = json!(stderr);
                        value["stdout_truncated"] =
                            json!(delta.stdout_truncated || take_stdout < delta.stdout.len());
                        value["stderr_truncated"] =
                            json!(delta.stderr_truncated || take_stderr < delta.stderr.len());
                    }
                    value
                })
            })
            .collect();
        let completed_count = guard.results.iter().filter(|x| x.is_some()).count();
        let succeeded = guard.results.iter().flatten().filter(|x| x.success).count();
        let failed = completed_count - succeeded;
        let timed_out = guard
            .results
            .iter()
            .flatten()
            .filter(|x| x.timed_out)
            .count();
        let cancelled = self.cancelled.load(Ordering::Acquire);
        let status = if !guard.done {
            "running"
        } else if cancelled {
            "cancelled"
        } else if timed_out > 0 {
            "timed_out"
        } else if failed > 0 {
            "failed"
        } else {
            "completed"
        };
        json!({
            "job_id": id,
            "resolved_target":self.target,
            "kind":"batch",
            "target": self.target.target.to_string(),
            "mode":self.mode,
            "status":status,
            "done":guard.done,
            "cancel_requested":cancelled,
            "requested_count": guard.results.len(),
            "executed_count":completed_count,
            "completed_count":completed_count,
            "succeeded":succeeded,
            "failed":failed,
            "timed_out":timed_out,
            "stopped_early":guard.done && completed_count < guard.results.len(),
            "elapsed_ms":guard.finished_elapsed_ms.unwrap_or_else(||self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64),
            "results":results,
        })
    }
}

pub struct BatchRegistry {
    entries: Mutex<HashMap<String, Arc<BatchGroup>>>,
}

impl BatchRegistry {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    pub fn launch(
        &self,
        state: Arc<AppState>,
        req: ExecBatchRequest,
        caller: &str,
        timeout: Option<u64>,
        max_output: Option<usize>,
    ) -> Result<(String, Arc<BatchGroup>)> {
        if req.commands.is_empty() || req.commands.len() > 32 {
            return Err(Error::Tool("exec accepts 1 to 32 batch commands".into()));
        }
        if req.stop_on_error && matches!(req.mode, ExecBatchMode::Parallel) {
            return Err(Error::Tool("stop_on_error requires sequential mode".into()));
        }
        let (target, source) = state.resolve_target(req.target.as_deref())?;
        policy::check_exec(&target, state.get_target_config(&target)?)?;
        let resolved = state.resolved_target_value(target, source);
        let id = format!("batch_{}", GROUP_COUNTER.fetch_add(1, Ordering::Relaxed));
        let group = Arc::new(BatchGroup::new(
            caller.to_string(),
            resolved,
            req.mode,
            req.commands.len(),
        ));
        {
            let mut entries = self.entries.lock().unwrap();
            let limit = state.config.runtime.max_retained_jobs;
            if entries.len() >= limit {
                // Keep completed groups readable until the configured budget
                // requires eviction. Evict the oldest completed group first.
                let oldest = entries
                    .iter()
                    .filter(|(_, group)| group.state.lock().unwrap().done)
                    .min_by_key(|(_, group)| group.started)
                    .map(|(id, _)| id.clone());
                if let Some(oldest) = oldest {
                    entries.remove(&oldest);
                }
            }
            if entries.len() >= limit {
                return Err(Error::Tool(
                    "batch job limit reached; finish existing jobs".into(),
                ));
            }
            entries.insert(id.clone(), Arc::clone(&group));
        }
        let runner = Arc::clone(&group);
        thread::spawn(move || runner.run(state, req, timeout, max_output));
        Ok((id, group))
    }

    pub fn get(&self, id: &str, caller: &str) -> Result<Option<Arc<BatchGroup>>> {
        let entries = self.entries.lock().unwrap();
        let Some(group) = entries.get(id) else {
            return Ok(None);
        };
        if group.caller != caller {
            return Err(Error::Tool(format!("job {id} not found")));
        }
        Ok(Some(Arc::clone(group)))
    }

    pub fn ids_for_caller(&self, caller: &str) -> Vec<String> {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, group)| group.caller == caller)
            .map(|(id, _)| id.clone())
            .collect()
    }
}
