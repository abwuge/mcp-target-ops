use super::{
    schema::output_schema, EXEC_TERMINAL_UI_URI, FILE_CHANGE_UI_URI, FILE_READ_UI_URI,
    INVENTORY_UI_URI,
};
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
        tool("mcp_server_list", "List downstream MCP servers.", object_schema(vec![])),
        tool("mcp_tools_list", "Initialize one configured downstream MCP server and return its tools/list result.", object_schema(vec![
            required_string("server", "Configured downstream MCP server name."),
        ])),
        tool("mcp_tool_call", "Initialize a configured downstream MCP server and call one of its tools.", object_schema(vec![
            required_string("server", "Configured downstream MCP server name."),
            required_string("tool", "Downstream MCP tool name."),
            optional_value("arguments", "JSON object passed as downstream tool arguments. Defaults to an empty object.", json!({"type":"object","additionalProperties":true})),
        ])),
        tool("exec", "Run one non-interactive shell command or script. Long-running executions may be promoted automatically to a background job and return job_id instead of blocking. When that happens, use job_wait to wait once for completion or continue with other work; do not repeatedly poll. Prefer exec_start when a command is known in advance to be long-running or live output matters. Use exec_batch for several short, logically independent commands. Prefer file_read/file_find over cat, sed, or grep when reading known files. Do not create ad-hoc .bak/.old safety copies with shell commands; use file_backup for managed rollback snapshots.", object_schema(vec![
            optional_string("target", "Target id: local or ssh:<profile>. Omit to use active target."),
            required_string("command", "Shell command or script to execute as one shell unit."),
            optional_string("cwd", "Working directory."),
            optional_integer("timeout_ms", "Timeout in milliseconds."),
            optional_integer("max_output_bytes", "Maximum bytes to return for stdout and stderr."),
            optional_value("secret_env", "Map environment variable names to server-side secret references.", secret_env_schema()),
        ])),
        // COMPAT(COMPAT-005): target/command/cwd remain in exec_stream only for
        // hosts that cannot expose the original tools/call request id to the App.
        tool("exec_stream", "Read incremental output from the foreground exec session associated with this App view.", object_schema(vec![
            optional_string("session_id", "Foreground exec session id returned by a previous exec_stream call."),
            optional_value("request_id", "Original MCP tools/call JSON-RPC id used to attach this App to its exact exec session.", json!({"type":["string","number"]})),
            optional_string("target", "Original exec target argument; compatibility fallback when request_id is unavailable."),
            optional_string("command", "Original exec command; compatibility fallback when request_id is unavailable."),
            optional_string("cwd", "Original exec working directory; compatibility fallback when request_id is unavailable."),
            optional_integer("stdout_since_seq", "Last stdout sequence already consumed."),
            optional_integer("stderr_since_seq", "Last stderr sequence already consumed."),
            optional_integer("max_bytes", "Maximum bytes to return from each stream."),
        ])),
        tool("result_read", "Reload a cached tool result for a sleeping App view.", object_schema(vec![
            required_string("result_id", "Opaque result id returned with the original App-backed tool result."),
        ])),
        tool("exec_batch", "Run multiple logically independent non-interactive commands in one tool call and return a separate result for each. Prefer this over combining unrelated inspections with shell separators; use exec for commands that must share shell state such as cd, variables, pipelines, or control flow. Prefer batch file_read for reading several known files.", exec_batch_schema()),
        tool("exec_start", "Start a non-interactive command as a background job and return immediately. Prefer this for longer commands or when live output matters. Use job_wait when the model only needs to continue after completion; use job_output for incremental output. The command App streams progress through job_output independently. Jobs support cancellation and optional runtime timeouts.", object_schema(vec![
            optional_string("target", "Target id: local or ssh:<profile>. Omit to use active target."),
            required_string("command", "Shell command to execute."),
            optional_string("cwd", "Working directory."),
            optional_integer("timeout_ms", "Optional job timeout in milliseconds. Omit for no runtime timeout."),
            optional_integer("max_output_bytes", "Maximum retained bytes for each stdout and stderr buffer."),
            optional_value("secret_env", "Map environment variable names to server-side secret references.", secret_env_schema()),
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
        tool("job_wait", "Wait for a background job to finish without repeated client polling, then return its status and incremental output. The configured default and maximum wait windows apply.", object_schema(vec![
            required_string("job_id", "Job id returned by exec_start."),
            optional_integer("wait_timeout_ms", "Maximum time to wait in this call. The configured default and cap apply."),
            optional_integer("stdout_since_seq", "Last stdout sequence already consumed. Omit or use 0 for buffered output."),
            optional_integer("stderr_since_seq", "Last stderr sequence already consumed. Omit or use 0 for buffered output."),
            optional_integer("max_bytes", "Maximum bytes to return from each stream."),
        ])),
        tool("job_cancel", "Request cancellation of a running background job.", object_schema(vec![
            required_string("job_id", "Job id returned by exec_start."),
        ])),
        // COMPAT(COMPAT-006): Keep the original top-level path/range request shape
        // while newer callers migrate to the files[] batch form.
        tool("file_read", "Read one known file or batch several independent file/range reads in one call. Prefer this over exec with cat/sed when file paths are known. Single-file path mode remains compatible; batch mode uses files[] and keeps per-file failures independent.", file_read_schema()),
        tool("file_backup", "Create a managed rollback snapshot of one file. Use this instead of cp/mv-based .bak, .old, timestamped, or in-place backup files. Snapshots are centrally tracked under the Target Ops runtime directory, expire automatically, preserve the original mode when available, and can be restored with file_restore.", file_backup_schema()),
        tool("file_backup_list", "List the newest unexpired managed rollback snapshots for one target, optionally restricted to an exact source path. Results are bounded (100 by default, 200 maximum); expired snapshots are removed automatically.", file_backup_list_schema()),
        tool("file_restore", "Restore a managed file_backup snapshot to its original path or an explicit destination. Existing destinations require expected_current_sha256 or overwrite=true. The target must match the backup and normal write policy still applies.", file_restore_schema()),
        tool("file_backup_delete", "Delete one managed rollback snapshot before its TTL expires. This removes only the managed backup; it never deletes the source file.", file_backup_delete_schema()),
        tool("file_list", "List one directory on the explicit target or active target.", object_schema(vec![
            optional_string("target", "Target id. Omit to use active target."),
            required_string("path", "Directory path."),
            optional_integer("timeout_ms", "Timeout in milliseconds for remote directory access."),
        ])),
        tool("file_edit", "Apply exact text replacements with sha256 compare-and-swap support. Existing file permissions are preserved. Writes require explicit target by default.", file_edit_schema()),
        tool("file_write", "Create or replace a UTF-8 or base64 file atomically. Existing files require overwrite=true or an expected sha256. Writes require explicit target by default.", file_write_schema()),
        tool("file_delete", "Delete one file after optionally checking its sha256. Returns the deleted content for review. Directories are refused.", file_delete_schema()),
        tool("file_import", "Import a ChatGPT or connector file reference into a target path. Accepts platform-rewritten local paths or HTTPS download URLs and preserves the normal target write policy.", file_import_schema()),
        tool("file_export", "Export one target file. IMPORTANT: file_export can trigger mandatory front-end review/approval. If the user is away, approval may time out and terminate the model's reasoning. Avoid this tool during active reasoning unless it is strictly necessary; prefer using it only after the substantive work is complete, especially with expensive Pro-tier models. The default link delivery returns a short-lived opaque HTTPS download URL; delivery=attachment preserves the embedded-resource compatibility path.", file_export_schema()),
        tool("file_transfer", "Copy one regular file between two different targets without materializing file data in the model. Both source_target and destination_target must be explicit. Uses rsync when available and falls back to scp. SSH-to-SSH transfers are direct between the two targets: Target Ops may initiate commands from either remote side, but the plugin host is never used as a file-data relay.", file_transfer_schema()),
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

    // COMPAT(COMPAT-004): Several blocks below intentionally emit OpenAI-prefixed
    // aliases alongside newer MCP Apps/file-rewrite metadata. See COMPATIBILITY.md.
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

    if matches!(name, "exec_stream" | "result_read") {
        meta.insert("ui".to_string(), json!({ "visibility": ["app"] }));
        meta.insert("openai/widgetAccessible".to_string(), Value::Bool(true));
        meta.insert(
            "openai/visibility".to_string(),
            Value::String("private".to_string()),
        );
    }

    if matches!(name, "exec" | "exec_batch" | "exec_start") {
        meta.insert(
            "ui".to_string(),
            json!({ "resourceUri": EXEC_TERMINAL_UI_URI }),
        );
        meta.insert("openai/widgetAccessible".to_string(), Value::Bool(true));
        meta.insert(
            "openai/outputTemplate".to_string(),
            Value::String(EXEC_TERMINAL_UI_URI.to_string()),
        );
        let (invoking, invoked) = if name == "exec_batch" {
            ("Running command batch…", "Command batch finished")
        } else if name == "exec_start" {
            ("Starting command…", "Command started")
        } else {
            ("Running command…", "Command finished")
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

    if name == "job_output" {
        meta.insert("openai/widgetAccessible".to_string(), Value::Bool(true));
    }

    if name == "file_read" {
        meta.insert("ui".to_string(), json!({ "resourceUri": FILE_READ_UI_URI }));
        meta.insert(
            "openai/outputTemplate".to_string(),
            Value::String(FILE_READ_UI_URI.to_string()),
        );
        meta.insert(
            "openai/toolInvocation/invoking".to_string(),
            Value::String("Reading file…".to_string()),
        );
        meta.insert(
            "openai/toolInvocation/invoked".to_string(),
            Value::String("File read".to_string()),
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

    // COMPAT(COMPAT-009): File-result rewrite aliases remain attached to
    // file_export so explicit delivery=attachment keeps host materialization.
    // Link delivery omits the `file` result field, so these paths are inert.
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
            | "job_wait"
            | "exec_stream"
            | "result_read"
            | "file_read"
            | "file_list"
            | "file_backup_list"
            | "file_export"
            | "file_find"
            | "terminal_read"
    );
    let destructive = matches!(
        name,
        "exec"
            | "exec_batch"
            | "exec_start"
            | "job_cancel"
            | "mcp_tool_call"
            | "file_edit"
            | "file_write"
            | "file_delete"
            | "file_restore"
            | "file_backup_delete"
            | "file_import"
            | "file_transfer"
            | "file_patch"
            | "file_move"
            | "file_chmod"
    );
    let open_world = matches!(
        name,
        "exec"
            | "exec_batch"
            | "exec_start"
            | "mcp_tools_list"
            | "mcp_tool_call"
            | "file_import"
            | "file_transfer"
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

fn exec_batch_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target": { "type": "string", "description": "Target id: local or ssh:<profile>. Omit to use active target." },
            "commands": {
                "type": "array",
                "minItems": 1,
                "maxItems": 32,
                "description": "Independent commands. Results preserve this order even in parallel mode.",
                "items": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "One shell command or script unit." },
                        "cwd": { "type": "string", "description": "Working directory for this command." },
                        "timeout_ms": { "type": "integer", "description": "Timeout in milliseconds for this command." },
                        "max_output_bytes": { "type": "integer", "description": "Maximum bytes returned for this command's stdout and stderr." }
                    },
                    "required": ["command"],
                    "additionalProperties": false
                }
            },
            "mode": { "type": "string", "enum": ["sequential", "parallel"], "description": "Execution mode. Defaults to sequential." },
            "stop_on_error": { "type": "boolean", "description": "In sequential mode, stop after the first non-zero, timed-out, or tool-level failure. Defaults to false." },
            "secret_env": { "description": "Shared map of environment variable names to server-side secret references.", "type": "object", "additionalProperties": secret_ref_schema() }
        },
        "required": ["commands"],
        "additionalProperties": false
    })
}

fn file_read_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target": { "type": "string", "description": "Target id. Omit to use active target." },
            "path": { "type": "string", "description": "Single file path. Use either path or files, not both." },
            "files": {
                "type": "array",
                "minItems": 1,
                "maxItems": 32,
                "description": "Independent file reads. Repeat a path with different line ranges when several ranges from one file are needed.",
                "items": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "File path." },
                        "max_bytes": { "type": "integer", "minimum": 0, "description": "Maximum raw bytes returned for this item." },
                        "start_line": { "type": "integer", "minimum": 1, "description": "Optional 1-based first line for UTF-8 text." },
                        "end_line": { "type": "integer", "minimum": 1, "description": "Optional 1-based inclusive last line for UTF-8 text." }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }
            },
            "max_bytes": { "type": "integer", "minimum": 0, "description": "Single mode: maximum returned raw bytes. Batch mode: total raw-byte budget shared across all successful reads." },
            "start_line": { "type": "integer", "minimum": 1, "description": "Single path mode only: optional 1-based first line for UTF-8 text." },
            "end_line": { "type": "integer", "minimum": 1, "description": "Single path mode only: optional 1-based inclusive last line for UTF-8 text." },
            "timeout_ms": { "type": "integer", "description": "Timeout in milliseconds for remote file access." }
        },
        "required": [],
        "anyOf": [
            { "required": ["path"] },
            { "required": ["files"] }
        ],
        "additionalProperties": false
    })
}

fn file_backup_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target": { "type": "string", "description": "Target id. Omit to use active/default target; backup creation requires file-read permission." },
            "path": { "type": "string", "description": "Existing file to snapshot." },
            "ttl_secs": { "type": "integer", "minimum": 1, "maximum": 2592000, "description": "Optional retention lifetime. Defaults to runtime.file_backup_default_ttl_secs and cannot exceed the configured maximum." },
            "note": { "type": "string", "maxLength": 512, "description": "Optional short reason or checkpoint label, for example 'before service restart'." },
            "timeout_ms": { "type": "integer", "minimum": 1, "description": "Timeout for remote file access." }
        },
        "required": ["path"],
        "additionalProperties": false
    })
}

fn file_backup_list_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target": { "type": "string", "description": "Target id. Omit to use active/default target." },
            "path": { "type": "string", "description": "Optional exact original path filter." },
            "limit": { "type": "integer", "minimum": 1, "maximum": 200, "description": "Maximum newest matching backups to return. Defaults to 100." }
        },
        "required": [],
        "additionalProperties": false
    })
}

fn file_restore_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target": { "type": "string", "description": "Target id. Must match the backup. Writes require an explicit target by default policy." },
            "backup_id": { "type": "string", "description": "Opaque managed backup id returned by file_backup or file_backup_list." },
            "destination": { "type": "string", "description": "Optional restore path. Defaults to the backup's original path." },
            "expected_current_sha256": { "type": "string", "description": "Optional CAS guard for an existing destination. Prefer this over overwrite=true when the current hash is known." },
            "overwrite": { "type": "boolean", "description": "Allow restoring over an existing destination without a CAS hash. Defaults to false." },
            "timeout_ms": { "type": "integer", "minimum": 1, "description": "Timeout for remote file access." }
        },
        "required": ["backup_id"],
        "additionalProperties": false
    })
}

fn file_backup_delete_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target": { "type": "string", "description": "Target id. Must match the backup. Writes require an explicit target by default policy." },
            "backup_id": { "type": "string", "description": "Managed backup id to delete." }
        },
        "required": ["backup_id"],
        "additionalProperties": false
    })
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
            "max_bytes": { "type": "integer", "minimum": 1, "maximum": 104857600, "description": "Maximum accepted source file size. Uses the configured default when omitted." }
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
            "delivery": { "type": "string", "enum": ["link", "attachment"], "description": "Delivery mode. Defaults to the configured mode (link by default). Link avoids ChatGPT attachment materialization; attachment returns the legacy embedded file output." },
            "link_ttl_secs": { "type": "integer", "minimum": 1, "maximum": 86400, "description": "Link lifetime in seconds for delivery=link. Uses the configured default (600 seconds by default)." },
            "single_use": { "type": "boolean", "description": "Consume a link after its first successful GET. Defaults to the configured value (false by default to tolerate link prefetching)." },
            "max_bytes": { "type": "integer", "minimum": 1, "maximum": 104857600, "description": "Maximum exported file size. Uses the configured default when omitted." },
            "timeout_ms": { "type": "integer", "minimum": 1, "description": "Timeout for remote file access." }
        },
        "required": ["path"],
        "additionalProperties": false
    })
}

fn file_transfer_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "source_target": { "type": "string", "description": "Explicit source target id: local or ssh:<profile>." },
            "source_path": { "type": "string", "description": "Existing regular file on source_target." },
            "destination_target": { "type": "string", "description": "Explicit destination target id: local or ssh:<profile>. Must differ from source_target." },
            "destination_path": { "type": "string", "description": "Destination file path. The transfer is staged beside this path and renamed into place after success." },
            "overwrite": { "type": "boolean", "description": "Allow replacing an existing destination. Defaults to false." },
            "timeout_ms": { "type": "integer", "minimum": 1, "description": "Timeout for each transfer attempt and remote file operation." }
        },
        "required": ["source_target", "source_path", "destination_target", "destination_path"],
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
    fn file_read_binds_read_app_resource() {
        let tools = list_tools(None);
        let file_read = tools
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "file_read")
            .expect("file_read tool");

        assert_eq!(file_read["_meta"]["ui"]["resourceUri"], FILE_READ_UI_URI);
        assert_eq!(
            file_read["_meta"]["openai/outputTemplate"],
            FILE_READ_UI_URI
        );
        assert_eq!(
            file_read["_meta"]["openai/toolInvocation/invoking"],
            "Reading file…"
        );
        assert_eq!(
            file_read["_meta"]["openai/toolInvocation/invoked"],
            "File read"
        );
    }

    #[test]
    fn exec_stream_is_app_only_and_exec_widget_can_call_tools() {
        let tools = list_tools(None);
        let tools = tools.as_array().unwrap();
        for name in ["exec_stream", "result_read"] {
            let tool = tools
                .iter()
                .find(|tool| tool["name"] == name)
                .expect("app-only tool");
            assert_eq!(tool["_meta"]["ui"]["visibility"], json!(["app"]));
            assert_eq!(tool["_meta"]["openai/visibility"], "private");
            assert_eq!(tool["_meta"]["openai/widgetAccessible"], true);
        }

        for name in ["exec", "exec_start", "job_output"] {
            let tool = tools
                .iter()
                .find(|tool| tool["name"] == name)
                .expect("widget-accessible command tool");
            assert_eq!(tool["_meta"]["openai/widgetAccessible"], true);
        }
    }

    #[test]
    fn exec_tools_bind_command_result_app_resource() {
        let tools = list_tools(None);
        let tools = tools.as_array().unwrap();

        for (name, invoking, invoked) in [
            ("exec", "Running command…", "Command finished"),
            (
                "exec_batch",
                "Running command batch…",
                "Command batch finished",
            ),
            ("exec_start", "Starting command…", "Command started"),
        ] {
            let tool = tools
                .iter()
                .find(|tool| tool["name"] == name)
                .expect("exec tool");
            assert_eq!(tool["_meta"]["ui"]["resourceUri"], EXEC_TERMINAL_UI_URI);
            assert_eq!(tool["_meta"]["openai/outputTemplate"], EXEC_TERMINAL_UI_URI);
            assert_eq!(tool["_meta"]["openai/toolInvocation/invoking"], invoking);
            assert_eq!(tool["_meta"]["openai/toolInvocation/invoked"], invoked);
        }
    }
}
