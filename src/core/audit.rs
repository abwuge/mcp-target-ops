use serde_json::{json, Map, Value};

// Persist commands verbatim (within a documented size bound). Large file payloads
// and tool results are capped; this is an audit log, not a second file store.
const COMMAND_LIMIT: usize = 128 * 1024;
const FIELD_LIMIT: usize = 48 * 1024;
const JSON_LIMIT: usize = 128 * 1024;

#[derive(Debug, Clone)]
pub struct CallDetails {
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub request_json: String,
    pub response_json: String,
    pub request_truncated: bool,
    pub response_truncated: bool,
}

impl CallDetails {
    pub fn from_arguments(tool: &str, args: Option<&Value>) -> Self {
        let mut trimmed = false;
        let command = args
            .and_then(|args| command_text(tool, args))
            .map(|command| trim_text(&command, COMMAND_LIMIT, &mut trimmed));
        let cwd = args
            .and_then(|args| args.get("cwd"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let request_json = args
            .map(|args| encode_snapshot(args, &mut trimmed))
            .unwrap_or_else(|| "{}".to_string());
        Self {
            command,
            cwd,
            request_json,
            response_json: "{}".into(),
            request_truncated: trimmed,
            response_truncated: false,
        }
    }

    pub fn with_response(mut self, result: Option<&Value>, error: Option<&str>) -> Self {
        if let Some(error) = error {
            let mut truncated = false;
            self.response_json = encode_snapshot(&json!({"error": error}), &mut truncated);
            self.response_truncated = truncated;
        } else if let Some(result) = result {
            // MCP structuredContent already contains the actual tool result.
            // Avoid duplicating it alongside resource content/transport wrappers.
            let content = result.get("structuredContent").unwrap_or(result);
            self.response_json = encode_snapshot(content, &mut self.response_truncated);
            if result.get("isError").and_then(Value::as_bool) == Some(true) {
                // Keep tool errors visible even when the tool returns only content[].
                if result.get("structuredContent").is_none() {
                    self.response_json = encode_snapshot(result, &mut self.response_truncated);
                }
            }
        }
        self
    }
}

fn command_text(tool: &str, args: &Value) -> Option<String> {
    match tool {
        "exec" if args.get("commands").is_some() => {
            let cmds = args.get("commands")?.as_array()?;
            let lines: Vec<String> = cmds
                .iter()
                .enumerate()
                .filter_map(|(i, item)| {
                    let command = item
                        .as_str()
                        .or_else(|| item.get("command").and_then(Value::as_str));
                    command.map(|command| format!("# {}\n{command}", i + 1))
                })
                .collect();
            (!lines.is_empty()).then(|| lines.join("\n\n"))
        }
        "exec" | "exec_start" => args
            .get("command")
            .and_then(Value::as_str)
            .map(str::to_string),
        "exec_batch" => {
            let cmds = args.get("commands")?.as_array()?;
            let lines: Vec<String> = cmds
                .iter()
                .enumerate()
                .filter_map(|(i, item)| {
                    item.get("command")
                        .and_then(Value::as_str)
                        .map(|command| format!("# {}\n{command}", i + 1))
                })
                .collect();
            (!lines.is_empty()).then(|| lines.join("\n\n"))
        }
        _ => {
            // Non-shell tools still get a readable one-line operation in the list.
            let mut parts = vec![tool.to_string()];
            for key in [
                "path",
                "source_path",
                "destination_path",
                "destination",
                "job_id",
                "terminal_id",
                "server",
                "tool",
                "target",
            ] {
                if let Some(value) = args.get(key).and_then(Value::as_str) {
                    parts.push(format!("{key}={value}"));
                }
            }
            if let Some(files) = args.get("files").and_then(Value::as_array) {
                for file in files.iter().take(3) {
                    if let Some(path) = file.get("path").and_then(Value::as_str) {
                        parts.push(format!("path={path}"));
                    }
                }
                if files.len() > 3 {
                    parts.push(format!("+{} more files", files.len() - 3));
                }
            }
            Some(parts.join(" "))
        }
    }
}

fn secret_field(name: &str) -> bool {
    // Only redact known credential *fields*. Never redact 'command', 'script',
    // 'content', stdout/stderr, or paths: they are the purpose of the audit.
    let key = name.to_ascii_lowercase().replace('-', "_");
    matches!(
        key.as_str(),
        "password"
            | "passwd"
            | "passphrase"
            | "authorization"
            | "cookie"
            | "access_token"
            | "refresh_token"
            | "bearer_token"
            | "api_key"
            | "private_key"
            | "client_secret"
            | "id_token"
            | "secret_key"
            | "token"
            | "ssh_private_key"
            | "secret"
    ) || key.ends_with("_password")
        || key.ends_with("_token")
        || key.ends_with("_api_key")
}

fn trim_text(value: &str, limit: usize, truncated: &mut bool) -> String {
    if value.len() <= limit {
        return value.to_string();
    }
    *truncated = true;
    let mut end = limit.min(value.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[TRUNCATED: {} additional bytes]",
        &value[..end],
        value.len() - end
    )
}

fn snapshot(value: &Value, key: Option<&str>, depth: usize, truncated: &mut bool) -> Value {
    if key.is_some_and(secret_field) {
        return json!("[REDACTED: credential field]");
    }
    if depth >= 12 {
        *truncated = true;
        return json!("[TRUNCATED: nesting too deep]");
    }
    match value {
        Value::String(s) => {
            if key.is_some_and(|key| key.ends_with("base64") || key.ends_with("_blob"))
                && s.len() > FIELD_LIMIT
            {
                *truncated = true;
                json!(format!(
                    "[TRUNCATED: binary/blob field of {} bytes]",
                    s.len()
                ))
            } else {
                json!(trim_text(s, FIELD_LIMIT, truncated))
            }
        }
        Value::Array(values) => {
            let mut array: Vec<Value> = values
                .iter()
                .take(256)
                .map(|item| snapshot(item, None, depth + 1, truncated))
                .collect();
            if values.len() > 256 {
                *truncated = true;
                array.push(json!(format!(
                    "[TRUNCATED: {} more items]",
                    values.len() - 256
                )));
            }
            Value::Array(array)
        }
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, item) in map.iter().take(256) {
                out.insert(key.clone(), snapshot(item, Some(key), depth + 1, truncated));
            }
            if map.len() > 256 {
                *truncated = true;
                out.insert("__truncated".to_string(), json!("additional object fields"));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

fn encode_snapshot(value: &Value, truncated: &mut bool) -> String {
    let processed = snapshot(value, None, 0, truncated);
    let encoded = serde_json::to_string(&processed).unwrap_or_else(|_| "{}".to_string());
    if encoded.len() <= JSON_LIMIT {
        return encoded;
    }
    *truncated = true;
    // Keep the beginning and display a clear marker when the aggregate exceeds
    // the log budget. The command is stored separately and remains accessible.
    json!({
        "_notice": "Audit JSON exceeded the 128 KiB limit; preview below",
        "_first_bytes": trim_text(&encoded, JSON_LIMIT - 512, truncated)
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn commands_and_batch_commands_are_readable() {
        let a = json!({"command":"printf 'hello world'", "cwd":"/tmp", "target":"ssh:mac"});
        let detail = CallDetails::from_arguments("exec", Some(&a));
        assert_eq!(detail.command.as_deref(), Some("printf 'hello world'"));
        assert_eq!(detail.cwd.as_deref(), Some("/tmp"));
        assert!(detail.request_json.contains("printf 'hello world'"));
        let unified = CallDetails::from_arguments(
            "exec",
            Some(&json!({"commands":["printf a",{"command":"printf b","cwd":"/tmp"}]})),
        );
        assert!(unified.command.unwrap().contains("printf b"));
        let batch = CallDetails::from_arguments(
            "exec_batch",
            Some(&json!({
                "commands":[{"command":"pwd"},{"command":"ls -la"}]
            })),
        );
        assert_eq!(batch.command.as_deref(), Some("# 1\npwd\n\n# 2\nls -la"));
    }

    #[test]
    fn preserves_command_and_output_but_not_explicit_credentials() {
        let input = json!({"command":"echo 'keep this literal text'",
            "password":"not-for-audit", "api_key":"also-secret",
            "edits":[{"old":"before","new":"after"}]});
        let detail = CallDetails::from_arguments("exec", Some(&input)).with_response(
            Some(&json!({
                "isError": false,
                "structuredContent":{"stdout":"actual output", "stderr":"warning", "exit_code":0}
            })),
            None,
        );
        assert!(detail.request_json.contains("keep this literal text"));
        assert!(detail.request_json.contains("after"));
        assert!(!detail.request_json.contains("not-for-audit"));
        assert!(!detail.request_json.contains("also-secret"));
        assert!(detail.response_json.contains("actual output"));
        assert!(detail.response_json.contains("warning"));
    }

    #[test]
    fn large_command_explicitly_marks_truncation() {
        let detail = CallDetails::from_arguments(
            "exec",
            Some(&json!({"command":"x".repeat(COMMAND_LIMIT + 50)})),
        );
        assert!(detail.request_truncated);
        assert!(detail.command.as_ref().unwrap().contains("TRUNCATED"));
    }
}
