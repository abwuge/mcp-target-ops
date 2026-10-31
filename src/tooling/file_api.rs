use crate::{
    core::{
        error::{Error, Result},
        policy::{self, FileAccess},
        state::AppState,
    },
    tooling::{
        exec::{self, ExecRequest},
        file_backup::{self, FileBackupListRequest, FileRestoreRequest},
        fs::{self, FilePatchRequest, FileWriteRequest},
    },
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Deserialize)]
struct SearchInput {
    #[serde(default)]
    target: Option<String>,
    path: String,
    pattern: String,
    #[serde(default = "default_true")]
    regex: bool,
    #[serde(default = "default_true")]
    case_sensitive: bool,
    #[serde(default)]
    glob: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(default = "default_context")]
    context_lines: usize,
    #[serde(default = "default_matches")]
    max_matches: usize,
    #[serde(default)]
    max_output_bytes: Option<usize>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}
fn default_true() -> bool {
    true
}
fn default_context() -> usize {
    2
}
fn default_matches() -> usize {
    100
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace("'", "'\"'\"'"))
}

pub fn read(state: &AppState, args: Value) -> Result<Value> {
    if args.get("pattern").is_none() {
        return Ok(serde_json::to_value(fs::read(
            state,
            serde_json::from_value(args)?,
        )?)?);
    }
    let req: SearchInput = serde_json::from_value(args)?;
    if req.pattern.is_empty() {
        return Err(Error::Tool("search pattern must not be empty".into()));
    }
    if req.max_matches == 0 || req.max_matches > 200 {
        return Err(Error::Tool("max_matches must be between 1 and 200".into()));
    }
    if req.context_lines > 8 {
        return Err(Error::Tool("context_lines may not exceed 8".into()));
    }
    let (target, source) = state.resolve_target(req.target.as_deref())?;
    let config = state.get_target_config(&target)?;
    policy::check_file(&target, config, &req.path, FileAccess::Read, source)?;

    // Reuse ripgrep's optimized engine on the target rather than implementing
    // a slower, weaker Rust regex walker. Every user input is shell-quoted.
    let mut arguments = vec![
        "rg".to_string(),
        "--json".into(),
        "--no-config".into(),
        "--color".into(),
        "never".into(),
        "--max-columns".into(),
        "2048".into(),
        "--max-columns-preview".into(),
        "--max-count".into(),
        req.max_matches.to_string(),
        "--context".into(),
        req.context_lines.to_string(),
    ];
    if !req.regex {
        arguments.push("--fixed-strings".into());
    }
    if !req.case_sensitive {
        arguments.push("--ignore-case".into());
    }
    for glob in &req.glob {
        arguments.extend(["--glob".into(), quote(glob)]);
    }
    for exclude in &req.exclude {
        arguments.extend(["--glob".into(), quote(&format!("!{exclude}"))]);
    }
    arguments.push("--".into());
    arguments.push(quote(&req.pattern));
    arguments.push(quote(&req.path));

    let command = arguments.join(" ");
    let max_bytes = req
        .max_output_bytes
        .unwrap_or(256 * 1024)
        .clamp(1024, 1024 * 1024);
    let output = exec::run_to_completion(
        state,
        ExecRequest {
            target: Some(target.to_string()),
            command,
            cwd: None,
            timeout_ms: req.timeout_ms.or(Some(20_000)),
            max_output_bytes: Some(max_bytes),
            secret_env: BTreeMap::new(),
        },
    )?;
    if output.timed_out {
        return Err(Error::Tool(
            "rg search timed out; narrow the path or pattern".into(),
        ));
    }
    if output.exit_code != Some(0) && output.exit_code != Some(1) {
        let err = output.stderr.trim();
        return Err(Error::Tool(format!(
            "rg search failed: {}",
            if err.is_empty() {
                "ripgrep missing or invalid pattern"
            } else {
                err
            }
        )));
    }

    let mut events = Vec::new();
    let mut truncated = output.stdout_truncated;
    let mut hits = 0;
    for line in output.stdout.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            truncated = true;
            continue;
        };
        let Some(kind) = event["type"].as_str() else {
            continue;
        };
        if kind != "match" && kind != "context" {
            continue;
        }
        if kind == "match" {
            if hits >= req.max_matches {
                truncated = true;
                break;
            }
            hits += 1;
        }
        let value = &event["data"];
        events.push(json!({
            "kind": kind,
            "path": value["path"]["text"],
            "line": value["line_number"],
            "text": value["lines"]["text"],
            "submatches": if kind=="match" { value["submatches"].clone() } else {json!([])},
        }));
        if events.len()
            >= req
                .max_matches
                .saturating_mul(1 + 2 * req.context_lines)
                .max(200)
        {
            truncated = true;
            break;
        }
    }
    Ok(json!({
        "resolved_target": state.resolved_target_value(target, source),
        "path": req.path,
        "pattern": req.pattern,
        "match_count":hits,
        "results":events,
        "truncated":truncated,
        "timed_out":false,
    }))
}

pub fn write(state: &AppState, args: Value) -> Result<Value> {
    let patch = args.get("patch").is_some();
    let content = args.get("content").is_some();
    if patch == content {
        return Err(Error::Tool(
            "provide exactly one of content or patch".into(),
        ));
    }
    if patch {
        let request: FilePatchRequest = serde_json::from_value(args)?;
        Ok(serde_json::to_value(fs::patch(state, request)?)?)
    } else {
        let request: FileWriteRequest = serde_json::from_value(args)?;
        Ok(serde_json::to_value(fs::write(state, request)?)?)
    }
}

pub fn restore(state: &AppState, args: Value) -> Result<Value> {
    if args.get("backup_id").is_some() {
        let req: FileRestoreRequest = serde_json::from_value(args)?;
        Ok(serde_json::to_value(file_backup::restore(state, req)?)?)
    } else {
        let req: FileBackupListRequest = serde_json::from_value(args)?;
        Ok(serde_json::to_value(file_backup::list(state, req)?)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quoted_search_values_do_not_escape_shell() {
        let x = quote("has'quote");
        assert_eq!(x, "'has'\"'\"'quote'");
    }
}
