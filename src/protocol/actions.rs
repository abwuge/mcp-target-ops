use crate::{
    core::{
        error::{Error, Result},
        state::AppState,
    },
    protocol::http,
    tooling::tools,
};
use serde_json::{json, Map, Value};
use std::{io::Read, sync::Arc};
use tiny_http::{Method, Request};

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
    let catalog = tools::list_tools(None);

    let mut exec_input = tool_schema(&catalog, "exec", "inputSchema");
    require_schema_field(&mut exec_input, "target");
    set_schema_maximum(&mut exec_input, "timeout_ms", MAX_EXEC_TIMEOUT_MS);
    set_schema_maximum(&mut exec_input, "max_output_bytes", MAX_EXEC_OUTPUT_BYTES);
    set_schema_max_length(&mut exec_input, "command", MAX_COMMAND_CHARACTERS);

    let mut read_input = tool_schema(&catalog, "file_read", "inputSchema");
    require_schema_field(&mut read_input, "target");
    set_schema_maximum(&mut read_input, "max_bytes", MAX_FILE_BYTES);
    set_schema_maximum(&mut read_input, "timeout_ms", MAX_EXEC_TIMEOUT_MS);

    let mut list_input = tool_schema(&catalog, "file_list", "inputSchema");
    require_schema_field(&mut list_input, "target");
    set_schema_maximum(&mut list_input, "timeout_ms", MAX_EXEC_TIMEOUT_MS);

    let mut preview_edit_input = tool_schema(&catalog, "file_edit", "inputSchema");
    require_schema_field(&mut preview_edit_input, "target");
    remove_schema_property(&mut preview_edit_input, "dry_run");
    set_schema_maximum(&mut preview_edit_input, "timeout_ms", MAX_EXEC_TIMEOUT_MS);
    constrain_edit_schema(&mut preview_edit_input);

    let mut apply_edit_input = preview_edit_input.clone();
    require_schema_field(&mut apply_edit_input, "expected_sha256");

    let mut directory_output = tool_schema(&catalog, "file_list", "outputSchema");
    add_schema_property(
        &mut directory_output,
        "total_entries",
        json!({ "type": "integer", "minimum": 0 }),
        true,
    );
    add_schema_property(
        &mut directory_output,
        "returned_entries",
        json!({ "type": "integer", "minimum": 0, "maximum": MAX_DIRECTORY_ENTRIES }),
        true,
    );
    add_schema_property(
        &mut directory_output,
        "truncated",
        json!({ "type": "boolean" }),
        true,
    );

    let mut edit_output = tool_schema(&catalog, "file_edit", "outputSchema");
    add_schema_property(
        &mut edit_output,
        "diff_truncated",
        json!({ "type": "boolean" }),
        true,
    );

    let mut paths = Map::new();
    paths.insert(
        LIST_TARGETS_PATH.to_string(),
        json!({
            "get": operation(
                "List configured targets",
                "Returns target IDs and their enabled policy capabilities. Call this before choosing a target for another action.",
                "listTargets",
                None,
                tool_schema(&catalog, "target_list", "outputSchema"),
                false,
            )
        }),
    );
    paths.insert(
        EXECUTE_COMMAND_PATH.to_string(),
        json!({
            "post": operation(
                "Execute a command",
                "Runs one non-interactive shell command on an explicit target. It may modify the target and always requires user confirmation.",
                "executeCommand",
                Some(exec_input),
                tool_schema(&catalog, "exec", "outputSchema"),
                true,
            )
        }),
    );
    paths.insert(
        READ_FILE_PATH.to_string(),
        json!({
            "post": operation(
                "Read a file",
                "Reads a bounded UTF-8 or binary file result from an explicit target and returns its SHA-256 hash.",
                "readFile",
                Some(read_input),
                tool_schema(&catalog, "file_read", "outputSchema"),
                false,
            )
        }),
    );
    paths.insert(
        LIST_DIRECTORY_PATH.to_string(),
        json!({
            "post": operation(
                "List a directory",
                "Lists up to 100 entries in one directory on an explicit target.",
                "listDirectory",
                Some(list_input),
                directory_output,
                false,
            )
        }),
    );
    paths.insert(
        PREVIEW_FILE_EDITS_PATH.to_string(),
        json!({
            "post": operation(
                "Preview exact file edits",
                "Previews exact text replacements without writing. Use the returned hashes and diff before applying the edit.",
                "previewFileEdits",
                Some(preview_edit_input),
                edit_output.clone(),
                false,
            )
        }),
    );
    paths.insert(
        APPLY_FILE_EDITS_PATH.to_string(),
        json!({
            "post": operation(
                "Apply exact file edits",
                "Applies exact text replacements to an explicit target using the required expected SHA-256 compare-and-swap guard.",
                "applyFileEdits",
                Some(apply_edit_input),
                edit_output,
                true,
            )
        }),
    );

    let authentication_configured =
        state.config.server.http_bearer_token.is_some() || state.config.server.oauth_enabled;
    let security = if authentication_configured {
        json!([{ "BearerAuth": [] }])
    } else {
        json!([])
    };

    json!({
        "openapi": "3.1.0",
        "info": {
            "title": format!("{} GPT Actions", state.config.server.name),
            "description": "A bounded REST facade for controlling configured local or SSH targets. Every target operation requires an explicit target ID.",
            "version": state.config.server.version,
        },
        "servers": [{
            "url": base_url.trim_end_matches('/'),
            "description": "mcp-target-ops public HTTPS endpoint"
        }],
        "security": security,
        "tags": [{
            "name": "Host actions",
            "description": "Bounded operations on configured local and SSH targets"
        }],
        "paths": Value::Object(paths),
        "components": {
            "securitySchemes": {
                "BearerAuth": {
                    "type": "http",
                    "scheme": "bearer",
                    "bearerFormat": "API key",
                    "description": "Configure the same bearer token in the GPT editor and MCP_TARGET_OPS_HTTP_TOKEN."
                }
            },
            "schemas": {
                "ErrorResponse": error_response_schema()
            }
        },
        "externalDocs": {
            "description": "OpenAI GPT Actions documentation",
            "url": "https://developers.openai.com/api/docs/actions/getting-started"
        }
    })
}

fn operation(
    summary: &str,
    description: &str,
    operation_id: &str,
    input_schema: Option<Value>,
    output_schema: Value,
    consequential: bool,
) -> Value {
    let mut operation = json!({
        "tags": ["Host actions"],
        "summary": summary,
        "description": description,
        "operationId": operation_id,
        "x-openai-isConsequential": consequential,
        "responses": action_responses(output_schema),
    });

    if let Some(input_schema) = input_schema {
        operation
            .as_object_mut()
            .expect("operation is an object")
            .insert(
                "requestBody".to_string(),
                json!({
                    "required": true,
                    "content": {
                        "application/json": {
                            "schema": input_schema
                        }
                    }
                }),
            );
    }

    operation
}

fn action_responses(output_schema: Value) -> Value {
    json!({
        "200": {
            "description": "Successful action result",
            "content": {
                "application/json": {
                    "schema": output_schema
                }
            }
        },
        "400": error_response("Invalid action input"),
        "401": error_response("Missing or invalid bearer token"),
        "403": error_response("Target policy denied the action"),
        "404": error_response("Action endpoint or resource not found"),
        "409": error_response("File edit compare-and-swap conflict"),
        "413": error_response("Action request is too large"),
        "500": error_response("Internal server error"),
        "502": error_response("Target transport or response error"),
    })
}

fn error_response(description: &str) -> Value {
    json!({
        "description": description,
        "content": {
            "application/json": {
                "schema": {
                    "$ref": "#/components/schemas/ErrorResponse"
                }
            }
        }
    })
}

fn error_response_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "error": {
                "type": "object",
                "properties": {
                    "code": { "type": "string" },
                    "message": { "type": "string" },
                    "retryable": { "type": "boolean" }
                },
                "required": ["code", "message", "retryable"],
                "additionalProperties": false
            }
        },
        "required": ["error"],
        "additionalProperties": false
    })
}

fn tool_schema(catalog: &Value, name: &str, field: &str) -> Value {
    catalog
        .as_array()
        .expect("tool catalog is an array")
        .iter()
        .find(|tool| tool.get("name").and_then(Value::as_str) == Some(name))
        .and_then(|tool| tool.get(field))
        .cloned()
        .unwrap_or_else(|| panic!("missing {field} for tool {name}"))
}

fn require_schema_field(schema: &mut Value, name: &str) {
    let required = schema
        .get_mut("required")
        .and_then(Value::as_array_mut)
        .expect("object schema has required array");
    if !required.iter().any(|value| value.as_str() == Some(name)) {
        required.push(Value::String(name.to_string()));
    }
}

fn remove_schema_property(schema: &mut Value, name: &str) {
    schema
        .get_mut("properties")
        .and_then(Value::as_object_mut)
        .expect("object schema has properties")
        .remove(name);
    if let Some(required) = schema.get_mut("required").and_then(Value::as_array_mut) {
        required.retain(|value| value.as_str() != Some(name));
    }
}

fn set_schema_maximum(schema: &mut Value, name: &str, maximum: u64) {
    schema["properties"][name]["minimum"] = json!(1);
    schema["properties"][name]["maximum"] = json!(maximum);
}

fn set_schema_max_length(schema: &mut Value, name: &str, maximum: u64) {
    schema["properties"][name]["minLength"] = json!(1);
    schema["properties"][name]["maxLength"] = json!(maximum);
}

fn constrain_edit_schema(schema: &mut Value) {
    schema["properties"]["edits"]["minItems"] = json!(1);
    schema["properties"]["edits"]["maxItems"] = json!(MAX_EDIT_ITEMS);
    schema["properties"]["edits"]["items"]["properties"]["old"]["minLength"] = json!(1);
    schema["properties"]["edits"]["items"]["properties"]["old"]["maxLength"] =
        json!(MAX_EDIT_TEXT_CHARACTERS);
    schema["properties"]["edits"]["items"]["properties"]["new"]["maxLength"] =
        json!(MAX_EDIT_TEXT_CHARACTERS);
}

fn add_schema_property(schema: &mut Value, name: &str, property: Value, required: bool) {
    schema
        .get_mut("properties")
        .and_then(Value::as_object_mut)
        .expect("object schema has properties")
        .insert(name.to_string(), property);
    if required {
        require_schema_field(schema, name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::Config;
    use std::collections::BTreeSet;

    fn test_state() -> AppState {
        let mut config = Config::default();
        config.server.public_base_url = Some("https://ssh.example.com".to_string());
        config.server.http_bearer_token = Some("top-secret-token".to_string());
        AppState::new(config).unwrap()
    }

    #[test]
    fn openapi_document_exposes_bounded_action_surface() {
        let state = test_state();
        let document = openapi_document(&state, "https://ssh.example.com");

        assert_eq!(document["openapi"], "3.1.0");
        assert_eq!(document["servers"][0]["url"], "https://ssh.example.com");
        assert_eq!(document["paths"].as_object().unwrap().len(), 6);
        assert!(document["paths"].get("/mcp").is_none());
        assert!(document["paths"].get("/actions/v1/terminals").is_none());
    }

    #[test]
    fn openapi_operation_ids_are_unique_and_target_is_explicit() {
        let state = test_state();
        let document = openapi_document(&state, "https://ssh.example.com");
        let mut operation_ids = BTreeSet::new();

        for path_item in document["paths"].as_object().unwrap().values() {
            for operation in path_item.as_object().unwrap().values() {
                let operation_id = operation["operationId"].as_str().unwrap();
                assert!(operation_ids.insert(operation_id));
                if let Some(required) = operation
                    .pointer("/requestBody/content/application~1json/schema/required")
                    .and_then(Value::as_array)
                {
                    assert!(required.iter().any(|field| field == "target"));
                }
            }
        }
        assert_eq!(operation_ids.len(), 6);
    }

    #[test]
    fn openapi_marks_mutating_actions_as_consequential() {
        let state = test_state();
        let document = openapi_document(&state, "https://ssh.example.com");

        assert_eq!(
            document["paths"][EXECUTE_COMMAND_PATH]["post"]["x-openai-isConsequential"],
            true
        );
        assert_eq!(
            document["paths"][APPLY_FILE_EDITS_PATH]["post"]["x-openai-isConsequential"],
            true
        );
        assert_eq!(
            document["paths"][READ_FILE_PATH]["post"]["x-openai-isConsequential"],
            false
        );
    }

    #[test]
    fn openapi_document_does_not_expose_configured_secret() {
        let state = test_state();
        let serialized =
            serde_json::to_string(&openapi_document(&state, "https://ssh.example.com")).unwrap();

        assert!(!serialized.contains("top-secret-token"));
        assert!(serialized.contains("BearerAuth"));
    }

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
