use super::schema::output_schema;
use crate::tooling::mcp_client::{self, AdvertisedMcpServer};
use serde_json::{json, Value};

pub fn list_tools(
    oauth_scopes: Option<&[String]>,
    enabled_mcp_servers: &[AdvertisedMcpServer],
) -> Value {
    let server_names: Vec<&str> = enabled_mcp_servers
        .iter()
        .map(|server| server.name.as_str())
        .collect();
    let server_inventory = mcp_client::describe_enabled_servers(enabled_mcp_servers);
    let server_parameter = || {
        let mut server = required_string("server", "Enabled downstream MCP server name.");
        if !server_names.is_empty() {
            server.value["enum"] = json!(server_names);
        }
        server
    };
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
        tool("server_info", "Return server configuration summary, active target state, and enabled/disabled downstream MCP service names; discover their tools with mcp_tools_list(server).", object_schema(vec![])),
        tool("target_list", "List configured local and SSH targets, including policy summaries and active marker.", object_schema(vec![])),
        tool("target_current", "Return the currently selected active target, if any.", object_schema(vec![])),
        tool("target_select", "Select a process-scoped active target. Later calls may omit target; use explicit targets when multiple clients share this server.", object_schema(vec![required_string("target", "Target id: local or ssh:<profile>")])),
        tool("target_connect", "Connect or warm an SSH target persistent worker.", object_schema(vec![required_string("target", "Target id: local or ssh:<profile>")])),
        tool("target_disconnect", "Disconnect an SSH target persistent worker, or no-op for local targets.", object_schema(vec![required_string("target", "Target id: local or ssh:<profile>")])),
        tool("mcp_server_list", &format!("List configured downstream MCP servers, including disabled entries. Enabled: {server_inventory}."), object_schema(vec![])),
        tool("mcp_tools_list", &format!("Discover tool names and schemas for one enabled downstream MCP server. Enabled: {server_inventory}."), object_schema(vec![
            server_parameter(),
        ])),
        tool("mcp_tool_call", &format!("Invoke a downstream MCP tool after discovering its schema with mcp_tools_list. Enabled servers: {server_inventory}."), object_schema(vec![
            server_parameter(),
            required_string("tool", "Downstream MCP tool name."),
            optional_value("arguments", "JSON object passed as downstream tool arguments. Defaults to an empty object.", json!({"type":"object","additionalProperties":true})),
        ])),
        tool("exec", "Execute one command or a batch of independent commands, sequentially or in parallel. Supports yield_time_ms for automatic background promotion and timeout_ms for a real execution deadline. For job_id results use job.", exec_schema()),
        tool("job", "Manage foreground-promoted or background jobs and batch groups by job_id. action=status/read/wait/cancel, default read; wait_ms controls blocking, output cursors for single jobs.", job_schema()),
        tool("read", "Read files or search with ripgrep. Content reads return a seven-digit SHA-256 revision for safe subsequent edits; request include_sha256=true for the full digest. Supports file ranges, batch reads, regex, globs and context.", read_schema()),
        tool("file_backup", "Create a managed rollback snapshot of one file, returning a backup_id usable with file_restore. Expires automatically and preserves original file permissions.", file_backup_schema()),
        tool("file_restore", "List managed backups when backup_id is omitted; restore one backup when backup_id is supplied. Existing destinations require expected_revision or overwrite=true.", restore_schema()),
        tool("file_edit", "Apply exact text replacements with optional content revision guard. Returns changed/written and a short revision for subsequent edits; include_diff or include_sha256 for details. dry_run returns a diff without writing.", file_edit_schema()),
        tool("write", "Atomically write content or apply a unified patch, including multi-file changes. Supply exactly one of content or patch. Default result confirms the write without echoing diff or hashes; request details explicitly if needed.", write_schema()),
        tool("file_delete", "Delete one file, optionally guarded by expected_revision. Returns deletion status only; set include_content=true to retrieve the previous contents. Directories are refused.", file_delete_schema()),
        tool("file_import", "Import a ChatGPT or connector file reference into a target path. Accepts platform-rewritten local paths or HTTPS download URLs and preserves the normal target write policy.", file_import_schema()),
        tool("file_export", "Export a target file as a short-lived HTTPS download link. The host may require approval before returning a link.", file_export_schema()),
        tool("file_transfer", "Copy one regular file between two different targets without materializing file data in the model. Both source_target and destination_target must be explicit. Uses rsync when available and falls back to scp. SSH-to-SSH transfers are direct between the two targets: Target Ops may initiate commands from either remote side, but the plugin host is never used as a file-data relay.", file_transfer_schema()),
        tool("terminal_open", "Open a persistent PTY terminal on the explicit target or active target.", object_schema(vec![
            optional_string("target", "Target id. Omit to use active target."),
            optional_string("cwd", "Initial working directory."),
            optional_string("shell", "Shell program to run."),
            optional_integer("rows", "PTY rows."),
            optional_integer("cols", "PTY columns."),
        ])),
        tool("terminal_send", "Send input and optionally resize the same live PTY session; supply rows and cols together, with or without input.", object_schema(vec![
            required_string("terminal_id", "Terminal id from terminal_open."),
            optional_string("input", "UTF-8 terminal input; omit for a resize-only operation."),
            optional_integer("rows", "Optional PTY height; must accompany cols."),
            optional_integer("cols", "Optional PTY width; must accompany rows."),
        ])),
        tool("terminal_read", "Read incremental output from an existing terminal_id.", object_schema(vec![
            required_string("terminal_id", "Terminal id from terminal_open."),
            optional_integer("since_seq", "Last seen sequence number. Omit or 0 to read buffered output."),
            optional_integer("max_bytes", "Maximum output bytes."),
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
    }

    if matches!(name, "target_list" | "mcp_server_list") {
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

    if matches!(name, "exec" | "job") {
        let (invoking, invoked) = if name == "job" {
            ("Checking job…", "Job result received")
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

    if name == "read" {
        meta.insert(
            "openai/toolInvocation/invoking".to_string(),
            Value::String("Reading file…".to_string()),
        );
        meta.insert(
            "openai/toolInvocation/invoked".to_string(),
            Value::String("File read".to_string()),
        );
    }

    if matches!(name, "write" | "file_edit" | "file_delete" | "file_import") {
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
        meta.insert("openai/fileParams".to_string(), paths);
    }

    if name == "file_export" {
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
            | "read"
            | "file_export"
            | "terminal_read"
    );
    let destructive = matches!(
        name,
        "exec"
            | "job"
            | "write"
            | "mcp_tool_call"
            | "file_edit"
            | "file_delete"
            | "file_restore"
            | "file_import"
            | "file_transfer"
    );
    let open_world = matches!(
        name,
        "exec"
            | "job"
            | "write"
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

fn optional_integer(name: &'static str, description: &'static str) -> Prop {
    Prop {
        name,
        required: false,
        value: json!({ "type": "integer", "description": description }),
    }
}

fn exec_schema() -> Value {
    let mut schema = exec_batch_schema();
    let properties = schema["properties"].as_object_mut().unwrap();
    properties.insert("command".into(), json!({"type":"string","minLength":1,"description":"One shell command/script; supply this OR commands."}));
    properties.insert(
        "cwd".into(),
        json!({"type":"string","description":"Single-command working directory."}),
    );
    properties.insert("yield_time_ms".into(), json!({"type":"integer","minimum":0,"maximum":120000,"description":"Foreground wait before promoting to a job; 0 starts in background. Different from execution timeout."}));
    properties.insert("timeout_ms".into(), json!({"type":"integer","minimum":1,"description":"Execution deadline (per command for batch)."}));
    properties.insert(
        "max_output_bytes".into(),
        json!({"type":"integer","minimum":1}),
    );
    schema["required"] = json!([]);
    schema["oneOf"] = json!([{"required":["command"]}, {"required":["commands"]}]);
    schema
}

fn job_schema() -> Value {
    json!({"type":"object","properties":{
        "job_id":{"type":"string","description":"Single job or batch group ID from exec."},
        "action":{"type":"string","enum":["status","read","wait","cancel"],"description":"Defaults to read, including status and output. wait blocks for a bounded window."},
        "wait_ms":{"type":"integer","minimum":0,"description":"Maximum wait window for action=wait."},
        "stdout_since_seq":{"type":"integer","minimum":0,"description":"Single job stdout cursor."},
        "stderr_since_seq":{"type":"integer","minimum":0,"description":"Single job stderr cursor."},
        "max_bytes":{"type":"integer","minimum":1}
    },"required":["job_id"],"additionalProperties":false})
}

fn read_schema() -> Value {
    let mut schema = file_read_schema();
    let props = schema["properties"].as_object_mut().unwrap();
    props.insert("pattern".into(),json!({"type":"string","minLength":1,"description":"Search path recursively with ripgrep, return structured matches and context; otherwise read path/files exactly."}));
    props.insert(
        "regex".into(),
        json!({"type":"boolean","description":"Default true; false performs literal matching."}),
    );
    props.insert(
        "case_sensitive".into(),
        json!({"type":"boolean","description":"Default true."}),
    );
    props.insert(
        "glob".into(),
        json!({"type":"array","items":{"type":"string"},"description":"Include globs."}),
    );
    props.insert(
        "exclude".into(),
        json!({"type":"array","items":{"type":"string"},"description":"Exclude globs."}),
    );
    props.insert(
        "context_lines".into(),
        json!({"type":"integer","minimum":0,"maximum":8}),
    );
    props.insert(
        "max_matches".into(),
        json!({"type":"integer","minimum":1,"maximum":200}),
    );
    props.insert("max_output_bytes".into(),json!({"type":"integer","minimum":1024,"description":"Search byte budget; file reads use max_bytes."}));
    props.insert("include_sha256".into(), json!({"type":"boolean","description":"Include full SHA-256 (default false). Short revision is always returned for file content reads."}));
    schema
}

fn write_schema() -> Value {
    let mut schema = file_write_schema();
    let patch = file_patch_schema();
    let properties = schema["properties"].as_object_mut().unwrap();
    properties.insert("patch".into(), patch["properties"]["patch"].clone());
    properties.insert("dry_run".into(), patch["properties"]["dry_run"].clone());
    properties.insert("include_diff".into(), json!({"type":"boolean","description":"Include a computed diff (default false; dry_run includes diff when available)."}));
    properties.insert(
        "include_sha256".into(),
        json!({"type":"boolean","description":"Include old/new SHA-256 hashes (default false)."}),
    );
    schema["required"] = json!(["path"]);
    schema["oneOf"] = json!([{"required":["content"]},{"required":["patch"]}]);
    schema
}

fn restore_schema() -> Value {
    let mut schema = file_restore_schema();
    let list = file_backup_list_schema();
    let props = schema["properties"].as_object_mut().unwrap();
    props.insert("path".into(), list["properties"]["path"].clone());
    props.insert("limit".into(), list["properties"]["limit"].clone());
    schema["required"] = json!([]);
    schema
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
                "description": "For common independent checks, use concise strings: [\"git status\", \"git log -1\"]. For an item needing cwd or a timeout, pass an object: {\"command\":\"pwd\",\"cwd\":\"/project\"}. Results preserve input order.",
                "items": {
                    "oneOf": [
                        {"type": "string", "minLength": 1, "description": "One command or shell script (simple case)."},
                        {
                            "type": "object",
                            "properties": {
                                "command": { "type": "string", "minLength": 1, "description": "One shell command or script unit." },
                                "cwd": { "type": "string", "description": "Working directory for this command." },
                                "timeout_ms": { "type": "integer", "description": "Timeout in milliseconds for this command." },
                                "max_output_bytes": { "type": "integer", "description": "Maximum bytes returned for this command's stdout and stderr." }
                            },
                            "required": ["command"],
                            "additionalProperties": false
                        }
                    ]
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
            "include_sha256": { "type":"boolean", "description":"Include the backup SHA-256 (default false)." },
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
            "backup_id": { "type": "string", "description": "Opaque managed backup id returned by file_backup or file_restore listing." },
            "include_sha256": { "type":"boolean", "description":"Include stored SHA-256 hashes (default false)." },
            "destination": { "type": "string", "description": "Optional restore path. Defaults to the backup's original path." },
            "expected_revision": { "type": "string", "minLength": 7, "maxLength": 7, "pattern": "^[a-f0-9]{7}$", "description": "Short content revision from read or previous edit; refuses restore if destination changed." },
            "overwrite": { "type": "boolean", "description": "Allow restoring over an existing destination without a revision guard. Defaults to false." },
            "timeout_ms": { "type": "integer", "minimum": 1, "description": "Timeout for remote file access." }
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
            "expected_revision": { "type": "string", "minLength": 7, "maxLength": 7, "pattern": "^[a-f0-9]{7}$", "description": "Content revision returned by read or previous edit; refuses stale changes." },
            "dry_run": { "type": "boolean", "description": "Return diff without writing." },
            "timeout_ms": { "type": "integer", "description": "Timeout for remote read/write." },
            "include_diff": { "type": "boolean", "description": "Return the computed diff. Dry-run always includes diff." },
            "include_sha256": { "type": "boolean", "description": "Return old and new full SHA-256 hashes; revision is always included (default false)." },
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
            "expected_revision": { "type": "string", "minLength": 7, "maxLength": 7, "pattern": "^[a-f0-9]{7}$", "description": "Short revision from read or previous edit; allows guarded overwrite without overwrite=true." },
            "overwrite": { "type": "boolean", "description": "Allow replacement without a revision guard. Defaults to false." },
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
            "expected_revision": { "type": "string", "minLength": 7, "maxLength": 7, "pattern": "^[a-f0-9]{7}$", "description": "Content revision returned by read or previous edit; refuses stale changes." },
            "dry_run": { "type": "boolean", "description": "Check the deletion without removing the file. Combine with include_content=true for a content preview." },
            "include_content": { "type": "boolean", "description": "Return previous file contents (default false)." },
            "include_sha256": { "type": "boolean", "description": "Return the previous file SHA-256 (default false)." },
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
            "expected_revision": { "type": "string", "minLength": 7, "maxLength": 7, "pattern": "^[a-f0-9]{7}$", "description": "Short content revision from read or previous write; refuses stale import." },
            "overwrite": { "type": "boolean", "description": "Allow replacement without a CAS hash. Defaults to false." },
            "mode": { "type": "string", "description": "Optional octal mode such as 0644 or 0755." },
            "timeout_ms": { "type": "integer", "minimum": 1, "description": "Transfer and target write timeout in milliseconds." },
            "max_bytes": { "type": "integer", "minimum": 1, "maximum": 104857600, "description": "Maximum accepted source file size. Uses the configured default when omitted." },
            "include_diff": { "type": "boolean", "description": "Return the computed diff when available (default false)." },
            "include_sha256": { "type": "boolean", "description": "Return old/new SHA-256 hashes (default false)." }
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
            "link_ttl_secs": { "type": "integer", "minimum": 1, "maximum": 86400, "description": "Link lifetime in seconds for delivery=link. Uses the configured default (600 seconds by default)." },
            "include_sha256": { "type": "boolean", "description": "Return file checksum (default false for link delivery)." },
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
            "expected_revision": { "type": "string", "minLength": 7, "maxLength": 7, "pattern": "^[a-f0-9]{7}$", "description": "Short content revision for a single-file patch; rejects stale content." },
            "dry_run": { "type": "boolean", "description": "Validate and return the patch result without writing." },
            "timeout_ms": { "type": "integer", "description": "Timeout for remote read/write." }
        },
        "required": ["path", "patch"],
        "additionalProperties": false
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downstream_inventory_is_visible_without_calling_discovery_tools() {
        let servers = vec![
            AdvertisedMcpServer {
                name: "memory".into(),
                description: Some("Personal memory search".into()),
            },
            AdvertisedMcpServer {
                name: "research".into(),
                description: None,
            },
        ];
        let tools = list_tools(None, &servers);
        let tools = tools.as_array().unwrap();
        assert_eq!(tools.len(), 24);
        for name in ["mcp_tools_list", "mcp_tool_call"] {
            let descriptor = tools.iter().find(|tool| tool["name"] == name).unwrap();
            assert_eq!(
                descriptor["inputSchema"]["properties"]["server"]["enum"],
                json!(["memory", "research"]),
            );
            assert!(descriptor["description"]
                .as_str()
                .unwrap()
                .contains("memory (Personal memory search), research"));
        }
        let empty = list_tools(None, &[]);
        let empty_tools = empty.as_array().unwrap();
        let discover = empty_tools
            .iter()
            .find(|tool| tool["name"] == "mcp_tools_list")
            .unwrap();
        assert!(discover["inputSchema"]["properties"]["server"]
            .get("enum")
            .is_none());
        assert!(discover["description"].as_str().unwrap().contains("none"));
    }

    #[test]
    fn catalog_contains_only_model_tools_without_app_metadata() {
        let tools = list_tools(None, &[]);
        let tools = tools.as_array().unwrap();
        assert_eq!(tools.len(), 24);
        for tool in tools {
            assert!(!matches!(
                tool["name"].as_str().unwrap(),
                "panel_open" | "panel_read" | "result_read" | "target_instructions"
            ));
            let meta = &tool["_meta"];
            for key in [
                "ui",
                "openai/outputTemplate",
                "openai/widgetAccessible",
                "openai/visibility",
            ] {
                assert!(meta.get(key).is_none(), "{} advertises {key}", tool["name"]);
            }
            assert!(tool["outputSchema"]["properties"]
                .get("result_id")
                .is_none());
        }
    }

    #[test]
    fn every_tool_declares_an_object_output_schema() {
        let tools = list_tools(None, &[]);
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
    fn inventory_tools_keep_invocation_metadata() {
        let tools = list_tools(None, &[]);
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
            assert!(tool["_meta"]["ui"]["resourceUri"].is_null());
            assert!(tool["_meta"]["openai/outputTemplate"].is_null());
            assert_eq!(tool["_meta"]["openai/toolInvocation/invoking"], invoking);
            assert_eq!(tool["_meta"]["openai/toolInvocation/invoked"], invoked);
        }
    }

    #[test]
    fn file_read_returns_data_without_rendering() {
        let tools = list_tools(None, &[]);
        let file_read = tools
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "read")
            .expect("read tool");

        assert!(file_read["_meta"]["ui"]["resourceUri"].is_null());
        assert!(file_read["_meta"]["openai/outputTemplate"].is_null());
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
    fn output_schemas_do_not_advertise_echoed_request_payloads() {
        let descriptors = list_tools(None, &[]);
        let descriptors = descriptors.as_array().unwrap();
        let schema = |name: &str| -> &Value {
            &descriptors
                .iter()
                .find(|tool| tool["name"] == name)
                .expect("tool is present")["outputSchema"]
        };
        assert!(schema("exec")["properties"].get("command").is_none());
        assert!(!schema("exec")["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|required| required == "command"));
        let batch = &schema("exec")["properties"]["results"]["items"];
        assert!(batch["properties"].get("command").is_none());
        assert!(batch["properties"].get("cwd").is_none());
        let completed = &schema("target_current")["properties"]["completed_jobs"]["items"];
        assert!(completed["properties"].get("command").is_none());
        assert!(completed["properties"].get("cwd").is_none());
        assert!(schema("write")["properties"].get("content").is_none());
        assert!(schema("file_import")["properties"].get("content").is_none());
        assert!(schema("write")["properties"].get("files").is_some());

        // These are actual results, not repeated request payloads.
        assert!(schema("read")["properties"].get("content").is_some());
        assert!(schema("file_delete")["properties"].get("content").is_some());
        assert!(schema("file_edit")["properties"].get("diff").is_some());
        assert!(schema("write")["properties"].get("diff").is_some());
    }

    #[test]
    fn unified_exec_supports_single_and_batch_commands() {
        let tools = list_tools(None, &[]);
        let tools = tools.as_array().unwrap();
        let exec = tools.iter().find(|t| t["name"] == "exec").unwrap();
        let items = &exec["inputSchema"]["properties"]["commands"]["items"]["oneOf"];
        assert_eq!(items[0]["type"], "string");
        assert_eq!(items[1]["type"], "object");
        assert_eq!(exec["inputSchema"]["oneOf"][0]["required"][0], "command");
        assert_eq!(exec["inputSchema"]["oneOf"][1]["required"][0], "commands");
        assert!(tools.iter().any(|t| t["name"] == "job"));
        assert!(!tools.iter().any(|t| t["name"] == "exec_batch"));
    }

    #[test]
    fn exec_tools_return_data_without_rendering() {
        let tools = list_tools(None, &[]);
        let tools = tools.as_array().unwrap();

        for (name, invoking, invoked) in [
            ("exec", "Running command…", "Command finished"),
            ("job", "Checking job…", "Job result received"),
        ] {
            let tool = tools
                .iter()
                .find(|tool| tool["name"] == name)
                .expect("exec tool");
            assert!(tool["_meta"]["ui"]["resourceUri"].is_null());
            assert!(tool["_meta"]["openai/outputTemplate"].is_null());
            assert_eq!(tool["_meta"]["openai/toolInvocation/invoking"], invoking);
            assert_eq!(tool["_meta"]["openai/toolInvocation/invoked"], invoked);
        }
    }
}
