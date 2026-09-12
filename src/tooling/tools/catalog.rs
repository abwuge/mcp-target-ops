use super::{schema::output_schema, EXEC_TERMINAL_UI_URI, FILE_CHANGE_UI_URI, INVENTORY_UI_URI};
use serde_json::{json, Value};

pub fn list_tools(oauth_scopes: Option<&[String]>) -> Value {
    let security_schemes = oauth_scopes.map(|scopes| {
        json!([{
            "type": "oauth2",
            "scopes": scopes,
        }])
    });
    let tool = |name: &str, description: &str, input_schema: Value| {
        tool(
            name,
            description,
            input_schema,
            output_schema(name),
            security_schemes.as_ref(),
        )
    };

    json!([
        tool("server_info", "Return server configuration summary and active target state.", object_schema(vec![])),
        tool("target_list", "List configured local and SSH targets, including policy summaries and active marker.", object_schema(vec![])),
        tool("target_current", "Return the currently selected active target, if any.", object_schema(vec![])),
        tool("target_select", "Select a session-scoped active target. Later calls may omit target and use this sticky target.", object_schema(vec![required_string("target", "Target id: local or ssh:<profile>")])),
        tool("target_connect", "Connect or warm an SSH target persistent worker.", object_schema(vec![required_string("target", "Target id: local or ssh:<profile>")])),
        tool("target_disconnect", "Disconnect an SSH target persistent worker, or no-op for local targets.", object_schema(vec![required_string("target", "Target id: local or ssh:<profile>")])),
        tool("mcp_server_list", "List configured downstream MCP servers. Secret values and endpoint URLs are not exposed.", object_schema(vec![])),
        tool("mcp_tools_list", "Initialize one configured downstream MCP server and return its tools/list result.", object_schema(vec![
            required_string("server", "Configured downstream MCP server name."),
        ])),
        tool("mcp_tool_call", "Initialize one configured downstream MCP server and call one of its tools. Only configured servers are reachable; credentials stay in server-owned configuration or server-side secret references.", object_schema(vec![
            required_string("server", "Configured downstream MCP server name."),
            required_string("tool", "Downstream MCP tool name."),
            optional_value("arguments", "JSON object passed as downstream tool arguments. Defaults to an empty object.", json!({"type":"object","additionalProperties":true})),
        ])),
        tool("exec", "Run a non-interactive command on the explicit target or current active target.", object_schema(vec![
            optional_string("target", "Target id: local or ssh:<profile>. Omit to use active target."),
            required_string("command", "Shell command to execute."),
            optional_string("cwd", "Working directory."),
            optional_integer("timeout_ms", "Timeout in milliseconds."),
            optional_integer("max_output_bytes", "Maximum bytes to return for stdout and stderr."),
            optional_value("secret_env", "Map environment variable names to secret references. Secret values are resolved inside Target Ops and are not included in the tool request or tool metadata; commands can still expose them if they print their environment.", secret_env_schema()),
        ])),
        tool("exec_start", "Start a non-interactive command as a background job. Jobs use dedicated processes, support incremental output and cancellation, and do not block later tool calls.", object_schema(vec![
            optional_string("target", "Target id: local or ssh:<profile>. Omit to use active target."),
            required_string("command", "Shell command to execute."),
            optional_string("cwd", "Working directory."),
            optional_integer("timeout_ms", "Optional job timeout in milliseconds. Omit for no runtime timeout."),
            optional_integer("max_output_bytes", "Maximum retained bytes for each stdout and stderr buffer."),
            optional_value("secret_env", "Map environment variable names to secret references. Secret values are resolved inside Target Ops and are not included in the tool request or tool metadata; commands can still expose them if they print their environment.", secret_env_schema()),
        ])),
        tool("job_poll", "Return the current state and exit information for a background job.", object_schema(vec![
            required_string("job_id", "Job id returned by exec_start."),
        ])),
        tool("job_output", "Read incremental stdout and stderr from a background job using independent sequence cursors.", object_schema(vec![
            required_string("job_id", "Job id returned by exec_start."),
            optional_integer("stdout_since_seq", "Last stdout sequence already consumed. Omit or use 0 for buffered output."),
            optional_integer("stderr_since_seq", "Last stderr sequence already consumed. Omit or use 0 for buffered output."),
            optional_integer("max_bytes", "Maximum bytes to return from each stream."),
        ])),
        tool("job_cancel", "Request cancellation of a running background job.", object_schema(vec![
            required_string("job_id", "Job id returned by exec_start."),
        ])),
        tool("file_read", "Read a UTF-8 or binary file from the explicit target or active target.", object_schema(vec![
            optional_string("target", "Target id. Omit to use active target."),
            required_string("path", "File path."),
            optional_integer("max_bytes", "Maximum bytes to return."),
            optional_integer("start_line", "Optional 1-based first line to return for UTF-8 files."),
            optional_integer("end_line", "Optional 1-based inclusive last line to return for UTF-8 files."),
            optional_integer("timeout_ms", "Timeout in milliseconds for remote file access."),
        ])),
        tool("file_list", "List one directory on the explicit target or active target.", object_schema(vec![
            optional_string("target", "Target id. Omit to use active target."),
            required_string("path", "Directory path."),
            optional_integer("timeout_ms", "Timeout in milliseconds for remote directory access."),
        ])),
        tool("file_edit", "Apply exact text replacements with sha256 compare-and-swap support. Existing file permissions are preserved. Writes require explicit target by default.", file_edit_schema()),
        tool("file_write", "Create or replace a UTF-8 or base64 file atomically. Existing files require overwrite=true or an expected sha256. Writes require explicit target by default.", file_write_schema()),
        tool("file_delete", "Delete one file after optionally checking its sha256. Returns the deleted content for review. Directories are refused.", file_delete_schema()),
        tool("file_import", "Import a ChatGPT or connector file reference into a target path. Accepts platform-rewritten local paths or HTTPS download URLs and preserves the normal target write policy.", file_import_schema()),
        tool("file_export", "Export one target file as an MCP embedded resource and ChatGPT-compatible file output.", file_export_schema()),
        tool("file_patch", "Apply a unified diff to one UTF-8 file, or a standard multi-file unified diff rooted at path. Single-file mode supports sha256 compare-and-swap; multi-file mode validates all files before writing and rolls back earlier writes if a later write fails.", file_patch_schema()),
        tool("file_find", "Find literal text in one UTF-8 file and return matching lines with bounded context.", file_find_schema()),
        tool("file_move", "Move or rename a file or directory within one target. Existing destinations are not replaced unless overwrite=true.", file_move_schema()),
        tool("file_chmod", "Change the Unix mode of a file or directory using an octal mode string.", file_chmod_schema()),
        tool("directory_create", "Create a directory on the selected target, recursively by default.", directory_create_schema()),
        tool("terminal_open", "Open a persistent PTY terminal on the explicit target or active target.", object_schema(vec![
            optional_string("target", "Target id. Omit to use active target."),
            optional_string("cwd", "Initial working directory."),
            optional_string("shell", "Shell program to run."),
            optional_integer("rows", "PTY rows."),
            optional_integer("cols", "PTY columns."),
        ])),
        tool("terminal_send", "Send input to an existing terminal_id. The terminal is already bound to its target.", object_schema(vec![
            required_string("terminal_id", "Terminal id from terminal_open."),
            required_string("input", "Input bytes represented as UTF-8 text, usually ending in newline."),
        ])),
        tool("terminal_read", "Read incremental output from an existing terminal_id.", object_schema(vec![
            required_string("terminal_id", "Terminal id from terminal_open."),
            optional_integer("since_seq", "Last seen sequence number. Omit or 0 to read buffered output."),
            optional_integer("max_bytes", "Maximum output bytes."),
        ])),
        tool("terminal_resize", "Resize the live PTY associated with an open terminal session.", object_schema(vec![
            required_string("terminal_id", "Terminal id from terminal_open."),
            required_integer("rows", "PTY rows."),
            required_integer("cols", "PTY columns."),
        ])),
        tool("terminal_close", "Close an existing terminal session.", object_schema(vec![
            required_string("terminal_id", "Terminal id from terminal_open."),
        ])),
    ])
}

fn tool(
    name: &str,
    description: &str,
    input_schema: Value,
    output_schema: Value,
    security_schemes: Option<&Value>,
) -> Value {
    let mut descriptor = json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema,
        "outputSchema": output_schema,
        "annotations": tool_annotations(name),
    });

    let object = descriptor
        .as_object_mut()
        .expect("tool descriptor is an object");
    let mut meta = serde_json::Map::new();

    if let Some(schemes) = security_schemes {
        object.insert("securitySchemes".to_string(), schemes.clone());
        meta.insert("securitySchemes".to_string(), schemes.clone());
    }

    if matches!(name, "target_list" | "mcp_server_list") {
        meta.insert("ui".to_string(), json!({ "resourceUri": INVENTORY_UI_URI }));
        meta.insert(
            "openai/outputTemplate".to_string(),
            Value::String(INVENTORY_UI_URI.to_string()),
        );
        let (invoking, invoked) = if name == "target_list" {
            ("Listing targets…", "Targets listed")
        } else {
            ("Listing MCP servers…", "MCP servers listed")
        };
        meta.insert(
            "openai/toolInvocation/invoking".to_string(),
            Value::String(invoking.to_string()),
        );
        meta.insert(
            "openai/toolInvocation/invoked".to_string(),
            Value::String(invoked.to_string()),
        );
    }

    if name == "exec" {
        meta.insert(
            "ui".to_string(),
            json!({ "resourceUri": EXEC_TERMINAL_UI_URI }),
        );
        meta.insert(
            "openai/outputTemplate".to_string(),
            Value::String(EXEC_TERMINAL_UI_URI.to_string()),
        );
        meta.insert(
            "openai/toolInvocation/invoking".to_string(),
            Value::String("Running command…".to_string()),
        );
        meta.insert(
            "openai/toolInvocation/invoked".to_string(),
            Value::String("Command finished".to_string()),
        );
    }

    if matches!(
        name,
        "file_edit" | "file_write" | "file_delete" | "file_import" | "file_patch" | "file_move"
    ) {
        meta.insert(
            "ui".to_string(),
            json!({ "resourceUri": FILE_CHANGE_UI_URI }),
        );
        meta.insert(
            "openai/outputTemplate".to_string(),
            Value::String(FILE_CHANGE_UI_URI.to_string()),
        );
        meta.insert(
            "openai/toolInvocation/invoking".to_string(),
            Value::String("Applying file change…".to_string()),
        );
        meta.insert(
            "openai/toolInvocation/invoked".to_string(),
            Value::String("File change applied".to_string()),
        );
    }

    if name == "file_import" {
        let paths = json!(["file"]);
        meta.insert("file_arg_rewrite_paths".to_string(), paths.clone());
        meta.insert("openai/fileParams".to_string(), paths);
    }

    if name == "file_export" {
        let paths = json!(["file"]);
        meta.insert("file_result_rewrite_paths".to_string(), paths.clone());
        meta.insert("openai/fileResultPaths".to_string(), paths.clone());
        meta.insert("openai/fileOutputs".to_string(), paths);
        meta.insert(
            "openai/toolInvocation/invoking".to_string(),
            Value::String("Exporting file…".to_string()),
        );
        meta.insert(
            "openai/toolInvocation/invoked".to_string(),
            Value::String("File exported".to_string()),
        );
    }

    if !meta.is_empty() {
        object.insert("_meta".to_string(), Value::Object(meta));
    }

    descriptor
}

fn tool_annotations(name: &str) -> Value {
    let read_only = matches!(
        name,
        "server_info"
            | "target_list"
            | "target_current"
            | "mcp_server_list"
            | "mcp_tools_list"
            | "job_poll"
            | "job_output"
            | "file_read"
            | "file_list"
            | "file_export"
            | "file_find"
            | "terminal_read"
    );
    let destructive = matches!(
        name,
        "exec"
            | "exec_start"
            | "job_cancel"
            | "mcp_tool_call"
            | "file_edit"
            | "file_write"
            | "file_delete"
            | "file_import"
            | "file_patch"
            | "file_move"
            | "file_chmod"
    );
    let open_world = matches!(
        name,
        "exec" | "exec_start" | "mcp_tools_list" | "mcp_tool_call" | "file_import"
    );

    json!({
        "readOnlyHint": read_only,
        "destructiveHint": destructive,
        "idempotentHint": read_only,
        "openWorldHint": open_world,
    })
}

#[derive(Debug, Clone)]
struct Prop {
    name: &'static str,
    value: Value,
    required: bool,
}

fn object_schema(props: Vec<Prop>) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for prop in props {
        properties.insert(prop.name.to_string(), prop.value);
        if prop.required {
            required.push(Value::String(prop.name.to_string()));
        }
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

fn required_string(name: &'static str, description: &'static str) -> Prop {
    Prop {
        name,
        required: true,
        value: json!({ "type": "string", "description": description }),
    }
}

fn optional_string(name: &'static str, description: &'static str) -> Prop {
    Prop {
        name,
        required: false,
        value: json!({ "type": "string", "description": description }),
    }
}

fn optional_value(name: &'static str, description: &'static str, schema: Value) -> Prop {
    let mut value = schema;
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "description".to_string(),
            Value::String(description.to_string()),
        );
    }
    Prop {
        name,
        required: false,
        value,
    }
}

fn required_integer(name: &'static str, description: &'static str) -> Prop {
    Prop {
        name,
        required: true,
        value: json!({ "type": "integer", "description": description }),
    }
}

fn optional_integer(name: &'static str, description: &'static str) -> Prop {
    Prop {
        name,
        required: false,
        value: json!({ "type": "integer", "description": description }),
    }
}

fn file_edit_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target": { "type": "string", "description": "Target id. For writes this is required by default policy." },
            "path": { "type": "string", "description": "UTF-8 text file path." },
            "expected_sha256": { "type": "string", "description": "Optional CAS guard from file_read." },
            "dry_run": { "type": "boolean", "description": "Return diff without writing." },
            "timeout_ms": { "type": "integer", "description": "Timeout for remote read/write." },
            "edits": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "old": { "type": "string" },
                        "new": { "type": "string" },
                        "replace_all": { "type": "boolean" }
                    },
                    "required": ["old", "new"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["path", "edits"],
        "additionalProperties": false
    })
}

fn secret_env_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": secret_ref_schema()
    })
}

fn secret_ref_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": { "type": "string", "description": "Secret source file on the selected target. The path must pass the target file-read policy." },
            "format": { "type": "string", "enum": ["text", "toml", "json"], "description": "Source format. Defaults to text." },
            "key": { "type": "string", "description": "Dot-separated scalar key for TOML or JSON sources." },
            "trim": { "type": "boolean", "description": "Trim surrounding whitespace from the resolved value. Defaults to true." }
        },
        "required": ["path"],
        "additionalProperties": false
    })
}

fn file_write_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target": { "type": "string", "description": "Target id. For writes this is required by default policy." },
            "path": { "type": "string", "description": "Destination file path." },
            "content": { "type": "string", "description": "File content encoded according to encoding." },
            "encoding": { "type": "string", "enum": ["utf8", "base64"], "description": "Content encoding. Defaults to utf8." },
            "expected_sha256": { "type": "string", "description": "Optional CAS guard for an existing file." },
            "overwrite": { "type": "boolean", "description": "Allow replacement without a CAS hash. Defaults to false." },
            "mode": { "type": "string", "description": "Optional octal mode such as 0644 or 0755. Existing mode is preserved when omitted." },
            "timeout_ms": { "type": "integer", "description": "Timeout for remote file access." }
        },
        "required": ["path", "content"],
        "additionalProperties": false
    })
}

fn file_delete_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target": { "type": "string", "description": "Target id. For writes this is required by default policy." },
            "path": { "type": "string", "description": "Existing file path to delete." },
            "expected_sha256": { "type": "string", "description": "Optional CAS guard from file_read." },
            "dry_run": { "type": "boolean", "description": "Preview the deletion and return existing content without removing the file." },
            "timeout_ms": { "type": "integer", "description": "Timeout for remote file access." }
        },
        "required": ["path"],
        "additionalProperties": false
    })
}

fn file_import_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target": { "type": "string", "description": "Target id. For writes this is required by default policy." },
            "path": { "type": "string", "description": "Destination file path on the selected target." },
            "file": {
                "type": "string",
                "format": "binary",
                "description": "ChatGPT or connector file parameter. The runtime may rewrite this argument before tool delivery; Target Ops also accepts compatible file-reference objects at runtime."
            },
            "expected_sha256": { "type": "string", "description": "Optional CAS guard for an existing destination file." },
            "overwrite": { "type": "boolean", "description": "Allow replacement without a CAS hash. Defaults to false." },
            "mode": { "type": "string", "description": "Optional octal mode such as 0644 or 0755." },
            "timeout_ms": { "type": "integer", "minimum": 1, "description": "Transfer and target write timeout in milliseconds." },
            "max_bytes": { "type": "integer", "minimum": 1, "maximum": 104857600, "description": "Maximum accepted source file size. Defaults to 26214400 bytes." }
        },
        "required": ["path", "file"],
        "additionalProperties": false
    })
}

fn file_export_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target": { "type": "string", "description": "Target id. Omit to use active target." },
            "path": { "type": "string", "description": "Existing file path to export." },
            "mime_type": { "type": "string", "description": "Optional media type override. Otherwise inferred from the filename." },
            "max_bytes": { "type": "integer", "minimum": 1, "maximum": 104857600, "description": "Maximum exported file size. Defaults to 26214400 bytes." },
            "timeout_ms": { "type": "integer", "minimum": 1, "description": "Timeout for remote file access." }
        },
        "required": ["path"],
        "additionalProperties": false
    })
}

fn file_patch_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target": { "type": "string", "description": "Target id. For writes this is required by default policy." },
            "path": { "type": "string", "description": "UTF-8 text file path in single-file mode, or base directory for a multi-file unified diff." },
            "patch": { "type": "string", "description": "Unified diff. If it contains multiple ---/+++ file sections, each relative path is resolved under path; git a/ and b/ prefixes are stripped." },
            "expected_sha256": { "type": "string", "description": "Optional CAS guard from file_read. Valid only for single-file patches." },
            "dry_run": { "type": "boolean", "description": "Validate and return the patch result without writing." },
            "timeout_ms": { "type": "integer", "description": "Timeout for remote read/write." }
        },
        "required": ["path", "patch"],
        "additionalProperties": false
    })
}

fn file_find_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target": { "type": "string", "description": "Target id. Omit to use active target." },
            "path": { "type": "string", "description": "UTF-8 text file path." },
            "pattern": { "type": "string", "description": "Literal text to find." },
            "case_sensitive": { "type": "boolean", "description": "Use case-sensitive matching. Defaults to true." },
            "context_lines": { "type": "integer", "minimum": 0, "maximum": 20, "description": "Lines of context before and after each match. Defaults to 2." },
            "max_matches": { "type": "integer", "minimum": 1, "maximum": 200, "description": "Maximum matches to return. Defaults to 20." },
            "timeout_ms": { "type": "integer", "description": "Timeout for remote file access." }
        },
        "required": ["path", "pattern"],
        "additionalProperties": false
    })
}

fn file_move_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target": { "type": "string", "description": "Target id. Writes require an explicit target by default." },
            "source": { "type": "string", "description": "Existing source path." },
            "destination": { "type": "string", "description": "Destination path on the same target." },
            "overwrite": { "type": "boolean", "description": "Replace an existing destination. Defaults to false." },
            "timeout_ms": { "type": "integer", "description": "Timeout for remote file access." }
        },
        "required": ["source", "destination"],
        "additionalProperties": false
    })
}

fn file_chmod_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target": { "type": "string", "description": "Target id. Writes require an explicit target by default." },
            "path": { "type": "string", "description": "File or directory path." },
            "mode": { "type": "string", "description": "Octal mode such as 0644 or 0755." },
            "timeout_ms": { "type": "integer", "description": "Timeout for remote file access." }
        },
        "required": ["path", "mode"],
        "additionalProperties": false
    })
}

fn directory_create_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target": { "type": "string", "description": "Target id. Writes require an explicit target by default." },
            "path": { "type": "string", "description": "Directory path." },
            "recursive": { "type": "boolean", "description": "Create missing parents. Defaults to true." },
            "mode": { "type": "string", "description": "Optional octal mode such as 0755." },
            "timeout_ms": { "type": "integer", "description": "Timeout for remote file access." }
        },
        "required": ["path"],
        "additionalProperties": false
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_declares_an_object_output_schema() {
        let tools = list_tools(None);
        let tools = tools.as_array().expect("tool list is an array");

        assert_eq!(tools.len(), 31);
        for tool in tools {
            let name = tool["name"].as_str().expect("tool has a name");
            assert_eq!(
                tool["outputSchema"]["type"], "object",
                "tool {name} must declare an object output schema"
            );
        }
    }

    #[test]
    fn list_tools_bind_inventory_app_resource() {
        let tools = list_tools(None);
        let tools = tools.as_array().unwrap();

        for (name, invoking, invoked) in [
            ("target_list", "Listing targets…", "Targets listed"),
            (
                "mcp_server_list",
                "Listing MCP servers…",
                "MCP servers listed",
            ),
        ] {
            let tool = tools
                .iter()
                .find(|tool| tool["name"] == name)
                .expect("list tool");
            assert_eq!(tool["_meta"]["ui"]["resourceUri"], INVENTORY_UI_URI);
            assert_eq!(tool["_meta"]["openai/outputTemplate"], INVENTORY_UI_URI);
            assert_eq!(tool["_meta"]["openai/toolInvocation/invoking"], invoking);
            assert_eq!(tool["_meta"]["openai/toolInvocation/invoked"], invoked);
        }
    }

    #[test]
    fn exec_binds_command_result_app_resource() {
        let tools = list_tools(None);
        let exec = tools
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "exec")
            .expect("exec tool");

        assert_eq!(exec["_meta"]["ui"]["resourceUri"], EXEC_TERMINAL_UI_URI);
        assert_eq!(exec["_meta"]["openai/outputTemplate"], EXEC_TERMINAL_UI_URI);
        assert_eq!(
            exec["_meta"]["openai/toolInvocation/invoking"],
            "Running command…"
        );
        assert_eq!(
            exec["_meta"]["openai/toolInvocation/invoked"],
            "Command finished"
        );
    }
}
