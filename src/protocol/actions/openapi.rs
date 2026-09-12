use super::{
    APPLY_FILE_EDITS_PATH, EXECUTE_COMMAND_PATH, LIST_DIRECTORY_PATH, LIST_TARGETS_PATH,
    MAX_COMMAND_CHARACTERS, MAX_DIRECTORY_ENTRIES, MAX_EDIT_ITEMS, MAX_EDIT_TEXT_CHARACTERS,
    MAX_EXEC_OUTPUT_BYTES, MAX_EXEC_TIMEOUT_MS, MAX_FILE_BYTES, PREVIEW_FILE_EDITS_PATH,
    READ_FILE_PATH,
};
use crate::{core::state::AppState, tooling::tools};
use serde_json::{json, Map, Value};

pub(super) fn document(state: &AppState, base_url: &str) -> Value {
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
        config.server.oauth_state_file = None;
        config.server.runtime_dir = tempfile::tempdir().unwrap().keep();
        AppState::new(config).unwrap()
    }

    #[test]
    fn document_exposes_bounded_action_surface() {
        let state = test_state();
        let document = document(&state, "https://ssh.example.com");

        assert_eq!(document["openapi"], "3.1.0");
        assert_eq!(document["servers"][0]["url"], "https://ssh.example.com");
        assert_eq!(document["paths"].as_object().unwrap().len(), 6);
        assert!(document["paths"].get("/mcp").is_none());
        assert!(document["paths"].get("/actions/v1/terminals").is_none());
    }

    #[test]
    fn operation_ids_are_unique_and_target_is_explicit() {
        let state = test_state();
        let document = document(&state, "https://ssh.example.com");
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
    fn mutating_actions_are_consequential() {
        let state = test_state();
        let document = document(&state, "https://ssh.example.com");

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
    fn document_does_not_expose_configured_secret() {
        let state = test_state();
        let serialized =
            serde_json::to_string(&document(&state, "https://ssh.example.com")).unwrap();

        assert!(!serialized.contains("top-secret-token"));
        assert!(serialized.contains("BearerAuth"));
    }
}
