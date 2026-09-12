use crate::{
    core::{
        error::{Error, Result},
        state::AppState,
    },
    protocol::http,
    tooling::tools,
};
use serde_json::{json, Value};
use std::{io::Read, sync::Arc};
use tiny_http::{Method, Request};

mod openapi;

pub const OPENAPI_PATH: &str = "/openapi.json";
pub const ACTIONS_PREFIX: &str = "/actions/v1";

const LIST_TARGETS_PATH: &str = "/actions/v1/targets";
const EXECUTE_COMMAND_PATH: &str = "/actions/v1/commands/execute";
const READ_FILE_PATH: &str = "/actions/v1/files/read";
const LIST_DIRECTORY_PATH: &str = "/actions/v1/directories/list";
const PREVIEW_FILE_EDITS_PATH: &str = "/actions/v1/files/edits/preview";
const APPLY_FILE_EDITS_PATH: &str = "/actions/v1/files/edits/apply";

const MAX_REQUEST_BODY_BYTES: u64 = 64 * 1024;
const MAX_RESPONSE_CHARACTERS: usize = 90_000;
const MAX_EXEC_TIMEOUT_MS: u64 = 30_000;
const MAX_EXEC_OUTPUT_BYTES: u64 = 24 * 1024;
const MAX_FILE_BYTES: u64 = 32 * 1024;
const MAX_DIRECTORY_ENTRIES: usize = 100;
const MAX_DIFF_CHARACTERS: usize = 32 * 1024;
const MAX_COMMAND_CHARACTERS: u64 = 8 * 1024;
const MAX_EDIT_ITEMS: u64 = 50;
const MAX_EDIT_TEXT_CHARACTERS: u64 = 32 * 1024;

type ActionResult = std::result::Result<Value, ActionError>;

pub fn is_action_path(path: &str) -> bool {
    path == ACTIONS_PREFIX || path.starts_with("/actions/v1/")
}

pub fn handle_request(
    state: Arc<AppState>,
    mut request: Request,
    method: Method,
    path: &str,
) -> Result<()> {
    let result = route_request(state, &mut request, method, path).and_then(limit_response_size);

    match result {
        Ok(value) => http::respond_json(request, 200, value),
        Err(err) => http::respond_json(request, err.status, err.as_json()),
    }
}

fn route_request(
    state: Arc<AppState>,
    request: &mut Request,
    method: Method,
    path: &str,
) -> ActionResult {
    if !known_action_path(path) {
        return Err(ActionError::not_found());
    }

    if method == Method::Get && path == LIST_TARGETS_PATH {
        return call_tool(state, "target_list", json!({}));
    }

    if method != Method::Post {
        return Err(ActionError::new(
            405,
            "method_not_allowed",
            "This action does not support the requested HTTP method",
            false,
        ));
    }

    let mut args = read_json_body(request)?;
    match path {
        EXECUTE_COMMAND_PATH => {
            require_explicit_target(&args)?;
            set_bounded_integer(&mut args, "timeout_ms", MAX_EXEC_TIMEOUT_MS)?;
            set_bounded_integer(&mut args, "max_output_bytes", MAX_EXEC_OUTPUT_BYTES)?;
            call_tool(state, "exec", args)
        }
        READ_FILE_PATH => {
            require_explicit_target(&args)?;
            set_bounded_integer(&mut args, "max_bytes", MAX_FILE_BYTES)?;
            set_bounded_integer(&mut args, "timeout_ms", MAX_EXEC_TIMEOUT_MS)?;
            call_tool(state, "file_read", args)
        }
        LIST_DIRECTORY_PATH => {
            require_explicit_target(&args)?;
            set_bounded_integer(&mut args, "timeout_ms", MAX_EXEC_TIMEOUT_MS)?;
            call_tool(state, "file_list", args).map(truncate_directory_result)
        }
        PREVIEW_FILE_EDITS_PATH => {
            require_explicit_target(&args)?;
            set_bounded_integer(&mut args, "timeout_ms", MAX_EXEC_TIMEOUT_MS)?;
            set_boolean(&mut args, "dry_run", true)?;
            call_tool(state, "file_edit", args).map(truncate_edit_result)
        }
        APPLY_FILE_EDITS_PATH => {
            require_explicit_target(&args)?;
            require_nonempty_string(&args, "expected_sha256")?;
            set_bounded_integer(&mut args, "timeout_ms", MAX_EXEC_TIMEOUT_MS)?;
            set_boolean(&mut args, "dry_run", false)?;
            call_tool(state, "file_edit", args).map(truncate_edit_result)
        }
        _ => Err(ActionError::not_found()),
    }
}

fn known_action_path(path: &str) -> bool {
    matches!(
        path,
        LIST_TARGETS_PATH
            | EXECUTE_COMMAND_PATH
            | READ_FILE_PATH
            | LIST_DIRECTORY_PATH
            | PREVIEW_FILE_EDITS_PATH
            | APPLY_FILE_EDITS_PATH
    )
}

fn read_json_body(request: &mut Request) -> ActionResult {
    let mut body = Vec::new();
    request
        .as_reader()
        .take(MAX_REQUEST_BODY_BYTES + 1)
        .read_to_end(&mut body)
        .map_err(|err| {
            ActionError::new(
                400,
                "invalid_request",
                format!("Failed to read request body: {err}"),
                false,
            )
        })?;

    if body.len() as u64 > MAX_REQUEST_BODY_BYTES {
        return Err(ActionError::new(
            413,
            "request_too_large",
            format!("Action request bodies are limited to {MAX_REQUEST_BODY_BYTES} bytes"),
            false,
        ));
    }

    let value: Value = serde_json::from_slice(&body).map_err(|err| {
        ActionError::new(
            400,
            "invalid_json",
            format!("Request body must be valid JSON: {err}"),
            false,
        )
    })?;
    if !value.is_object() {
        return Err(ActionError::new(
            400,
            "invalid_request",
            "Request body must be a JSON object",
            false,
        ));
    }

    Ok(value)
}

fn require_explicit_target(args: &Value) -> std::result::Result<(), ActionError> {
    require_nonempty_string(args, "target")
}

fn require_nonempty_string(
    args: &Value,
    field: &'static str,
) -> std::result::Result<(), ActionError> {
    if args
        .get(field)
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty())
    {
        return Ok(());
    }

    Err(ActionError::new(
        400,
        "invalid_request",
        format!("{field} must be a non-empty string"),
        false,
    ))
}

fn set_bounded_integer(
    args: &mut Value,
    field: &'static str,
    maximum: u64,
) -> std::result::Result<(), ActionError> {
    let requested = match args.get(field) {
        Some(value) => value.as_u64().ok_or_else(|| {
            ActionError::new(
                400,
                "invalid_request",
                format!("{field} must be a non-negative integer"),
                false,
            )
        })?,
        None => maximum,
    };

    args.as_object_mut()
        .expect("JSON object was checked")
        .insert(field.to_string(), json!(requested.clamp(1, maximum)));
    Ok(())
}

fn set_boolean(
    args: &mut Value,
    field: &'static str,
    value: bool,
) -> std::result::Result<(), ActionError> {
    let object = args.as_object_mut().ok_or_else(|| {
        ActionError::new(
            400,
            "invalid_request",
            "Request body must be a JSON object",
            false,
        )
    })?;
    object.insert(field.to_string(), Value::Bool(value));
    Ok(())
}

fn call_tool(state: Arc<AppState>, name: &str, args: Value) -> ActionResult {
    tools::call_tool(state, name, args).map_err(ActionError::from)
}

fn truncate_directory_result(mut value: Value) -> Value {
    let Some(object) = value.as_object_mut() else {
        return value;
    };

    let (total_entries, returned_entries) =
        match object.get_mut("entries").and_then(Value::as_array_mut) {
            Some(entries) => {
                let total = entries.len();
                entries.truncate(MAX_DIRECTORY_ENTRIES);
                (total, entries.len())
            }
            None => (0, 0),
        };
    object.insert("total_entries".to_string(), json!(total_entries));
    object.insert("returned_entries".to_string(), json!(returned_entries));
    object.insert(
        "truncated".to_string(),
        json!(returned_entries < total_entries),
    );
    value
}

fn truncate_edit_result(mut value: Value) -> Value {
    let Some(object) = value.as_object_mut() else {
        return value;
    };

    let (diff, truncated) = object
        .get("diff")
        .and_then(Value::as_str)
        .map(|diff| truncate_characters(diff, MAX_DIFF_CHARACTERS))
        .unwrap_or_else(|| (String::new(), false));
    object.insert("diff".to_string(), Value::String(diff));
    object.insert("diff_truncated".to_string(), Value::Bool(truncated));
    value
}

fn truncate_characters(value: &str, maximum: usize) -> (String, bool) {
    let mut characters = value.chars();
    let truncated = characters.by_ref().take(maximum).collect::<String>();
    let was_truncated = characters.next().is_some();
    (truncated, was_truncated)
}

fn limit_response_size(value: Value) -> ActionResult {
    let characters = serde_json::to_string(&value)
        .map_err(|err| {
            ActionError::new(
                500,
                "serialization_failed",
                format!("Failed to serialize action response: {err}"),
                false,
            )
        })?
        .chars()
        .count();
    if characters > MAX_RESPONSE_CHARACTERS {
        return Err(ActionError::new(
            502,
            "response_too_large",
            "The upstream result is too large for a GPT Action response; narrow the request",
            false,
        ));
    }
    Ok(value)
}

#[derive(Debug)]
struct ActionError {
    status: u16,
    code: &'static str,
    message: String,
    retryable: bool,
}

impl ActionError {
    fn new(status: u16, code: &'static str, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            retryable,
        }
    }

    fn not_found() -> Self {
        Self::new(404, "not_found", "Action endpoint not found", false)
    }

    fn as_json(&self) -> Value {
        json!({
            "error": {
                "code": self.code,
                "message": self.message,
                "retryable": self.retryable,
            }
        })
    }
}

impl From<Error> for ActionError {
    fn from(err: Error) -> Self {
        match err {
            Error::Json(err) => Self::new(400, "invalid_request", err.to_string(), false),
            Error::Target(message) => Self::new(400, "invalid_target", message, false),
            Error::Policy(message) => Self::new(403, "policy_denied", message, false),
            Error::Tool(message)
                if message.contains("file changed before edit")
                    || message.contains("old text not found")
                    || message.contains("matched") =>
            {
                Self::new(409, "edit_conflict", message, false)
            }
            Error::Tool(message) => Self::new(400, "tool_error", message, false),
            Error::Terminal(message) => Self::new(400, "terminal_error", message, false),
            Error::Io(err) => Self::new(502, "upstream_io_error", err.to_string(), true),
            Error::Utf8(err) => Self::new(502, "upstream_encoding_error", err.to_string(), false),
            Error::Config(message) => Self::new(500, "server_misconfigured", message, false),
            Error::TomlDe(err) => Self::new(500, "server_misconfigured", err.to_string(), false),
            Error::TomlSer(err) => Self::new(500, "server_misconfigured", err.to_string(), false),
        }
    }
}

pub fn openapi_document(state: &AppState, base_url: &str) -> Value {
    openapi::document(state, base_url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_limits_are_applied_to_tool_arguments() {
        let mut args = json!({ "target": "ssh:dev", "timeout_ms": 999_999 });
        require_explicit_target(&args).unwrap();
        set_bounded_integer(&mut args, "timeout_ms", MAX_EXEC_TIMEOUT_MS).unwrap();
        set_bounded_integer(&mut args, "max_output_bytes", MAX_EXEC_OUTPUT_BYTES).unwrap();

        assert_eq!(args["timeout_ms"], MAX_EXEC_TIMEOUT_MS);
        assert_eq!(args["max_output_bytes"], MAX_EXEC_OUTPUT_BYTES);
    }

    #[test]
    fn apply_edit_requires_compare_and_swap_hash() {
        let args = json!({ "target": "ssh:dev", "path": "/tmp/a", "edits": [] });
        let err = require_nonempty_string(&args, "expected_sha256").unwrap_err();

        assert_eq!(err.status, 400);
        assert_eq!(err.code, "invalid_request");
    }
}
