//! Model-facing result projection. Tool operations still produce full results for
//! the audit store; only the MCP structuredContent is compacted.
use crate::core::util::revision_from_sha256;
use serde_json::{json, Map, Value};

#[derive(Default)]
pub(crate) struct ResultOptions {
    explicit_target: bool,
    include_diff: bool,
    include_sha256: bool,
    include_content: bool,
}

impl ResultOptions {
    pub(crate) fn from_arguments(args: &Value) -> Self {
        Self {
            explicit_target: args.get("target").and_then(Value::as_str).is_some(),
            include_diff: args.get("include_diff").and_then(Value::as_bool) == Some(true)
                || args.get("dry_run").and_then(Value::as_bool) == Some(true),
            include_sha256: args.get("include_sha256").and_then(Value::as_bool) == Some(true),
            include_content: args.get("include_content").and_then(Value::as_bool) == Some(true),
        }
    }
}

fn copy(object: &mut Map<String, Value>, source: &Value, keys: &[&str]) {
    for &key in keys {
        if let Some(value) = source.get(key).filter(|value| !value.is_null()) {
            object.insert(key.to_string(), value.clone());
        }
    }
}

fn flag(object: &mut Map<String, Value>, source: &Value, key: &str) {
    if source.get(key).and_then(Value::as_bool) == Some(true) {
        object.insert(key.into(), json!(true));
    }
}

fn resolved_target(object: &mut Map<String, Value>, source: &Value, opts: &ResultOptions) {
    if !opts.explicit_target {
        if let Some(target) = source
            .pointer("/resolved_target/target")
            .and_then(Value::as_str)
        {
            object.insert("target".into(), json!(target));
        }
    }
}

fn checksums(object: &mut Map<String, Value>, source: &Value, opts: &ResultOptions) {
    if opts.include_sha256 {
        copy(
            object,
            source,
            &[
                "sha256",
                "old_sha256",
                "new_sha256",
                "previous_sha256",
                "restored_sha256",
            ],
        );
    }
}

fn revision(object: &mut Map<String, Value>, source: &Value, sha_field: &str) {
    if let Some(hash) = source
        .get(sha_field)
        .and_then(Value::as_str)
        .filter(|hash| hash.len() == 64 && hash.bytes().all(|c| c.is_ascii_hexdigit()))
    {
        object.insert("revision".into(), json!(revision_from_sha256(hash)));
    }
}

fn mutation(source: &Value, opts: &ResultOptions, keys: &[&str]) -> Map<String, Value> {
    let mut out = Map::new();
    copy(&mut out, source, keys);
    resolved_target(&mut out, source, opts);
    checksums(&mut out, source, opts);
    // Never claim a speculative dry-run revision was actually applied.
    if source.get("written").and_then(Value::as_bool) == Some(true)
        || source.get("changed").and_then(Value::as_bool) == Some(false)
    {
        revision(&mut out, source, "new_sha256");
    }
    if opts.include_diff {
        copy(&mut out, source, &["diff"]);
    }
    out
}

fn read_item(source: &Value, batch: bool) -> Value {
    let mut item = Map::new();
    if batch {
        copy(&mut item, source, &["index", "path"]);
    }
    if source.get("success").and_then(Value::as_bool) == Some(false) {
        copy(&mut item, source, &["error"]);
        item.insert("success".into(), json!(false));
    } else {
        copy(&mut item, source, &["content"]);
        revision(&mut item, source, "sha256");
        if source.get("encoding").and_then(Value::as_str) == Some("base64") {
            copy(&mut item, source, &["encoding"]);
        }
        flag(&mut item, source, "truncated");
    }
    Value::Object(item)
}

pub(crate) fn project(name: &str, source: &Value, opts: &ResultOptions) -> Value {
    let mut out = match name {
        "file_edit" => mutation(source, opts, &["changed", "written"]),
        "write" => {
            let mut result = mutation(source, opts, &["changed", "written", "created"]);
            if let Some(files) = source.get("files").and_then(Value::as_array) {
                if files.len() > 1 {
                    let items: Vec<Value> = files
                        .iter()
                        .map(|file| {
                            Value::Object(mutation(file, opts, &["path", "changed", "written"]))
                        })
                        .collect();
                    result.insert("files".into(), json!(items));
                }
            }
            result
        }
        "file_delete" => {
            let mut result = mutation(source, opts, &["deleted", "written"]);
            if opts.include_content {
                copy(&mut result, source, &["content", "encoding"]);
            }
            result
        }
        "file_import" => mutation(source, opts, &["created", "written", "bytes"]),
        "file_transfer" => {
            let mut result = Map::new();
            copy(&mut result, source, &["transferred"]);
            result
        }
        "file_backup" => {
            let mut result = Map::new();
            if let Some(backup) = source.get("backup") {
                let mut entry = Map::new();
                copy(&mut entry, backup, &["backup_id", "expires_at_unix_secs"]);
                checksums(&mut entry, backup, opts);
                result.insert("backup".into(), Value::Object(entry));
            }
            resolved_target(&mut result, source, opts);
            result
        }
        "file_restore" => {
            if let Some(backups) = source.get("backups").and_then(Value::as_array) {
                let mut result = Map::new();
                let entries: Vec<Value> = backups
                    .iter()
                    .map(|backup| {
                        let mut entry = Map::new();
                        copy(
                            &mut entry,
                            backup,
                            &["backup_id", "path", "created_at_unix_secs", "note"],
                        );
                        if opts.include_sha256 {
                            copy(&mut entry, backup, &["sha256"]);
                        }
                        Value::Object(entry)
                    })
                    .collect();
                result.insert("backups".into(), json!(entries));
                if source.get("truncated").and_then(Value::as_bool) == Some(true) {
                    result.insert("truncated".into(), json!(true));
                    copy(&mut result, source, &["total_matches"]);
                }
                resolved_target(&mut result, source, opts);
                result
            } else {
                let mut result = mutation(source, opts, &["restored", "created", "path"]);
                result.remove("created"); // Only the restore outcome and destination matter.
                if source.get("restored").and_then(Value::as_bool) == Some(true) {
                    revision(&mut result, source, "restored_sha256");
                }
                result
            }
        }
        "file_export" => {
            let mut result = Map::new();
            if let Some(download) = source.get("download") {
                let mut item = Map::new();
                copy(
                    &mut item,
                    download,
                    &[
                        "url",
                        "file_name",
                        "mime_type",
                        "bytes",
                        "expires_at_unix_secs",
                    ],
                );
                flag(&mut item, download, "single_use");
                checksums(&mut item, download, opts);
                result.insert("download".into(), Value::Object(item));
            }
            result
        }
        "read" => {
            let mut result = Map::new();
            if source.get("match_count").is_some() {
                copy(&mut result, source, &["match_count", "results"]);
                flag(&mut result, source, "truncated");
            } else if let Some(files) = source.get("files").and_then(Value::as_array) {
                let mut items: Vec<Value> =
                    files.iter().map(|item| read_item(item, true)).collect();
                if opts.include_sha256 {
                    for (original, item) in files.iter().zip(items.iter_mut()) {
                        if let Some(value) = original.get("sha256") {
                            item["sha256"] = value.clone();
                        }
                    }
                }
                result.insert("files".into(), json!(items));
                if source.get("failed").and_then(Value::as_u64).unwrap_or(0) > 0 {
                    copy(&mut result, source, &["failed"]);
                }
                flag(&mut result, source, "truncated");
            } else {
                if let Value::Object(item) = read_item(source, false) {
                    result.extend(item);
                }
                if opts.include_sha256 {
                    copy(&mut result, source, &["sha256"]);
                }
            }
            resolved_target(&mut result, source, opts);
            result
        }
        _ => return source.clone(),
    };
    if out.is_empty() {
        return source.clone();
    }
    Value::Object(std::mem::take(&mut out))
}

fn object_schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

fn bool_field() -> Value {
    json!({"type":"boolean"})
}
fn string_field() -> Value {
    json!({"type":"string"})
}
fn revision_field() -> Value {
    json!({"type":"string","minLength":7,"maxLength":7,"pattern":"^[a-f0-9]{7}$"})
}
fn hash_fields() -> Value {
    json!({"old_sha256": {"type":"string"}, "new_sha256":{"type":"string"},
        "sha256":{"type":"string"}, "previous_sha256":{"type":"string"},
        "restored_sha256":{"type":"string"}})
}

fn fields(base: Value, hashes: bool) -> Value {
    let mut map = base.as_object().expect("schema properties").clone();
    map.insert("target".to_owned(), string_field());
    map.insert("revision".to_owned(), revision_field());
    if hashes {
        map.extend(hash_fields().as_object().expect("hash fields").clone());
    }
    Value::Object(map)
}

pub(super) fn output_schema(name: &str) -> Option<Value> {
    let boolean = bool_field();
    let text = string_field();
    let integer = json!({"type":"integer","minimum":0});
    let scheme = match name {
        "file_edit" => object_schema(
            fields(
                json!({
                    "changed":boolean, "written":boolean,
                    "diff":text
                }),
                true,
            ),
            &["changed", "written"],
        ),
        "write" => {
            let item = object_schema(
                fields(
                    json!({
                        "path":text,"changed":boolean,"written":boolean
                    }),
                    true,
                ),
                &["path", "changed", "written"],
            );
            object_schema(
                fields(
                    json!({
                        "changed":boolean,"written":boolean,"created":boolean,
                        "diff":text,
                        "files":{"type":"array","items":item}
                    }),
                    true,
                ),
                &["written"],
            )
        }
        "file_delete" => object_schema(
            fields(
                json!({
                    "deleted":boolean,"written":boolean,
                    "content":text,"encoding":text
                }),
                true,
            ),
            &["deleted", "written"],
        ),
        "file_import" => object_schema(
            fields(
                json!({
                    "created":boolean,"written":boolean,"bytes":integer,"diff":text
                }),
                true,
            ),
            &["created", "written", "bytes"],
        ),
        "file_transfer" => object_schema(json!({"transferred":boolean}), &["transferred"]),
        "file_backup" => {
            let backup = object_schema(
                json!({
                    "backup_id":text,"expires_at_unix_secs":integer,"sha256":text
                }),
                &["backup_id", "expires_at_unix_secs"],
            );
            object_schema(json!({"backup":backup,"target":text}), &["backup"])
        }
        "file_restore" => {
            let backup = object_schema(
                json!({
                    "backup_id":text,"path":text,"created_at_unix_secs":integer,
                    "note":text,"sha256":text
                }),
                &["backup_id", "path", "created_at_unix_secs"],
            );
            object_schema(
                fields(
                    json!({
                        "restored":boolean,"created":boolean,"path":text,
                        "backups":{"type":"array","items":backup},
                        "truncated":boolean,"total_matches":integer
                    }),
                    true,
                ),
                &[],
            )
        }
        "file_export" => {
            let download = object_schema(
                json!({
                    "url":text,"file_name":text,"mime_type":text,"bytes":integer,
                    "expires_at_unix_secs":integer,"single_use":boolean,"sha256":text
                }),
                &[
                    "url",
                    "file_name",
                    "mime_type",
                    "bytes",
                    "expires_at_unix_secs",
                ],
            );
            object_schema(json!({"download":download}), &["download"])
        }
        "read" => {
            let item = object_schema(
                json!({
                    "index":integer,"path":text,"success":boolean,
                    "content":text,"sha256":text,"revision":revision_field(),"encoding":text,
                    "truncated":boolean,"error":text
                }),
                &["index", "path"],
            );
            object_schema(
                json!({
                    "content":text,"sha256":text,"revision":revision_field(),"encoding":text,"truncated":boolean,
                    "target":text,"match_count":integer,
                    "results":{"type":"array","items":{"type":"object"}},
                    "files":{"type":"array","items":item},"failed":integer
                }),
                &[],
            )
        }
        _ => return None,
    };
    Some(scheme)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_write_result_does_not_echo_hash_or_diff() {
        let source = json!({
            "resolved_target":{"target":"local","source":"explicit"},
            "path":"/tmp/demo","changed":true,"written":true,
            "old_sha256":"old","new_sha256":"new",
            "diff":"-before\n+after"
        });
        let compact = project(
            "file_edit",
            &source,
            &ResultOptions {
                explicit_target: true,
                ..Default::default()
            },
        );
        assert_eq!(compact, json!({"changed":true,"written":true}));
        let expanded = project(
            "file_edit",
            &source,
            &ResultOptions {
                explicit_target: true,
                include_diff: true,
                include_sha256: true,
                ..Default::default()
            },
        );
        assert_eq!(expanded["diff"], "-before\n+after");
        assert_eq!(expanded["new_sha256"], "new");
        assert_eq!(expanded["old_sha256"], "old");
    }
    #[test]
    fn deletion_and_read_have_distinct_default_content_policies() {
        let source = json!({"deleted":true,"written":true,"path":"/tmp/demo",
            "content":"private file","encoding":"utf-8","old_sha256":"old",
            "resolved_target":{"target":"ssh:mac","source":"explicit"}});
        let opts = ResultOptions {
            explicit_target: true,
            ..Default::default()
        };
        assert_eq!(
            project("file_delete", &source, &opts),
            json!({"deleted":true,"written":true})
        );
        assert_eq!(
            project(
                "file_delete",
                &source,
                &ResultOptions {
                    explicit_target: true,
                    include_content: true,
                    ..Default::default()
                }
            )["content"],
            "private file"
        );
        let full_sha256 = crate::core::util::sha256_hex(b"code");
        let read = json!({"path":"/tmp/demo","content":"code","sha256":full_sha256,"truncated":false,
            "encoding":"utf-8","resolved_target":{"target":"local","source":"default"}});
        assert_eq!(
            project("read", &read, &ResultOptions::default()),
            json!({"content":"code","revision":&full_sha256[..7],"target":"local"})
        );
        assert_eq!(
            project(
                "read",
                &read,
                &ResultOptions {
                    include_sha256: true,
                    ..Default::default()
                }
            )["sha256"],
            full_sha256
        );
    }
    #[test]
    fn batch_files_and_backup_ids_remain_usable() {
        let opts = ResultOptions {
            explicit_target: true,
            ..Default::default()
        };
        let batch = json!({"changed":true,"written":true,"files":[
            {"path":"/a","changed":true,"written":true,"old_sha256":"x","new_sha256":"y"},
            {"path":"/b","changed":false,"written":false,"old_sha256":"z","new_sha256":"z"}]});
        let compact = project("write", &batch, &opts);
        assert_eq!(compact["files"].as_array().unwrap().len(), 2);
        assert_eq!(
            compact["files"][0],
            json!({"path":"/a","changed":true,"written":true})
        );
        let backup = json!({"backup":{"backup_id":"abc","path":"/a","target":"local",
            "sha256":"hash","expires_at_unix_secs":123},"resolved_target":{"source":"explicit","target":"local"}});
        assert_eq!(
            project("file_backup", &backup, &opts),
            json!({"backup":{"backup_id":"abc","expires_at_unix_secs":123}})
        );
        let list = json!({"backups":[{"backup_id":"abc","path":"/a",
            "target":"local","created_at_unix_secs":10,"sha256":"hash"}],
            "resolved_target":{"target":"local","source":"explicit"},
            "truncated":false,"total_matches":1});
        assert_eq!(
            project("file_restore", &list, &opts),
            json!({
            "backups":[{"backup_id":"abc","path":"/a","created_at_unix_secs":10}]})
        );
    }
}
