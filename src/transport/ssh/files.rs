use super::{run_script, SshSessionRegistry};
use crate::core::{
    config::SshTargetConfig,
    error::{Error, Result},
    util::shell_quote,
};
use serde_json::{json, Value};
use std::time::Duration;

pub fn read_file(
    sessions: &SshSessionRegistry,
    target_name: &str,
    ssh: &SshTargetConfig,
    path: &str,
    timeout: Duration,
) -> Result<Vec<u8>> {
    let output = run_script(
        sessions,
        target_name,
        ssh,
        r#"cat < "$1""#,
        &[path],
        timeout,
    )?;
    if output.exit_code == Some(0) {
        Ok(output.stdout)
    } else {
        Err(Error::Tool(format!(
            "remote read failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

pub fn write_file(
    sessions: &SshSessionRegistry,
    target_name: &str,
    ssh: &SshTargetConfig,
    path: &str,
    bytes: &[u8],
    mode: Option<u32>,
    timeout: Duration,
) -> Result<()> {
    let mode_arg = mode.map(|value| format!("{value:o}")).unwrap_or_default();
    let mut script = r#"
p=$1
requested_mode=$2
parent=${p%/*}
if [ "$parent" = "$p" ]; then
    parent=.
fi
if [ -z "$parent" ]; then
    parent=/
fi
base=${p##*/}
if [ -z "$base" ]; then
    printf "%s\n" "refusing to write directory path: $p" >&2
    exit 1
fi
if [ -n "$requested_mode" ]; then
    final_mode=$requested_mode
elif [ -e "$p" ] || [ -L "$p" ]; then
    final_mode=$(stat -c "%a" "$p" 2>/dev/null || stat -f "%Lp" "$p" 2>/dev/null || printf "")
else
    final_mode=644
fi
mkdir -p "$parent" || exit 1
tmp=$(mktemp "$parent/.$base.XXXXXX") || exit 1
cleanup() {
    rm -f "$tmp"
}
trap cleanup EXIT HUP INT TERM
: > "$tmp" || exit 1
"#
    .to_string();
    append_printf_chunks(&mut script, bytes);
    script.push_str(
        r#"
if [ -n "$final_mode" ]; then
    chmod "$final_mode" "$tmp" || exit 1
fi
mv -f "$tmp" "$p" || exit 1
trap - EXIT HUP INT TERM
"#,
    );
    let output = run_script(
        sessions,
        target_name,
        ssh,
        &script,
        &[path, mode_arg.as_str()],
        timeout,
    )?;
    if output.exit_code == Some(0) {
        Ok(())
    } else {
        Err(Error::Tool(format!(
            "remote write failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

pub fn file_mode(
    sessions: &SshSessionRegistry,
    target_name: &str,
    ssh: &SshTargetConfig,
    path: &str,
    timeout: Duration,
) -> Result<Option<u32>> {
    let output = run_script(
        sessions,
        target_name,
        ssh,
        r#"stat -c "%a" "$1" 2>/dev/null || stat -f "%Lp" "$1" 2>/dev/null"#,
        &[path],
        timeout,
    )?;
    if output.exit_code != Some(0) {
        return Err(Error::Tool(format!(
            "remote stat failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    u32::from_str_radix(text, 8)
        .map(Some)
        .map_err(|err| Error::Tool(format!("invalid remote file mode {text:?}: {err}")))
}

pub fn file_exists(
    sessions: &SshSessionRegistry,
    target_name: &str,
    ssh: &SshTargetConfig,
    path: &str,
    timeout: Duration,
) -> Result<bool> {
    let output = run_script(
        sessions,
        target_name,
        ssh,
        r#"if [ -e "$1" ] || [ -L "$1" ]; then exit 0; else exit 1; fi"#,
        &[path],
        timeout,
    )?;
    match output.exit_code {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(Error::Tool(format!(
            "remote existence check failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ))),
    }
}

pub fn move_path(
    sessions: &SshSessionRegistry,
    target_name: &str,
    ssh: &SshTargetConfig,
    source: &str,
    destination: &str,
    overwrite: bool,
    timeout: Duration,
) -> Result<()> {
    let overwrite_arg = if overwrite { "1" } else { "0" };
    let script = r#"
src=$1
dst=$2
overwrite=$3
if [ "$overwrite" != 1 ] && { [ -e "$dst" ] || [ -L "$dst" ]; }; then
    printf "%s\n" "destination already exists: $dst" >&2
    exit 2
fi
if [ "$overwrite" = 1 ]; then
    if [ -d "$dst" ] && [ ! -L "$dst" ]; then
        printf "%s\n" "refusing to overwrite an existing directory: $dst" >&2
        exit 2
    fi
    rm -f "$dst" || exit 1
fi
mv "$src" "$dst" || exit 1
"#;
    let output = run_script(
        sessions,
        target_name,
        ssh,
        script,
        &[source, destination, overwrite_arg],
        timeout,
    )?;
    if output.exit_code == Some(0) {
        Ok(())
    } else {
        Err(Error::Tool(format!(
            "remote move failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

pub fn remove_file(
    sessions: &SshSessionRegistry,
    target_name: &str,
    ssh: &SshTargetConfig,
    path: &str,
    timeout: Duration,
) -> Result<()> {
    let output = run_script(
        sessions,
        target_name,
        ssh,
        r#"if [ -d "$1" ] && [ ! -L "$1" ]; then
    printf "%s\n" "refusing to delete a directory: $1" >&2
    exit 2
fi
rm -- "$1""#,
        &[path],
        timeout,
    )?;
    if output.exit_code == Some(0) {
        Ok(())
    } else {
        Err(Error::Tool(format!(
            "remote delete failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

pub fn chmod_path(
    sessions: &SshSessionRegistry,
    target_name: &str,
    ssh: &SshTargetConfig,
    path: &str,
    mode: u32,
    timeout: Duration,
) -> Result<()> {
    let mode = format!("{mode:o}");
    let output = run_script(
        sessions,
        target_name,
        ssh,
        r#"chmod "$2" "$1""#,
        &[path, mode.as_str()],
        timeout,
    )?;
    if output.exit_code == Some(0) {
        Ok(())
    } else {
        Err(Error::Tool(format!(
            "remote chmod failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

pub fn create_directory(
    sessions: &SshSessionRegistry,
    target_name: &str,
    ssh: &SshTargetConfig,
    path: &str,
    recursive: bool,
    mode: Option<u32>,
    timeout: Duration,
) -> Result<()> {
    let recursive_arg = if recursive { "1" } else { "0" };
    let mode_arg = mode.map(|value| format!("{value:o}")).unwrap_or_default();
    let script = r#"
p=$1
recursive=$2
mode=$3
if [ "$recursive" = 1 ]; then
    mkdir -p "$p" || exit 1
else
    mkdir "$p" || exit 1
fi
if [ -n "$mode" ]; then
    chmod "$mode" "$p" || exit 1
fi
"#;
    let output = run_script(
        sessions,
        target_name,
        ssh,
        script,
        &[path, recursive_arg, mode_arg.as_str()],
        timeout,
    )?;
    if output.exit_code == Some(0) {
        Ok(())
    } else {
        Err(Error::Tool(format!(
            "remote directory creation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

pub fn list_dir(
    sessions: &SshSessionRegistry,
    target_name: &str,
    ssh: &SshTargetConfig,
    path: &str,
    timeout: Duration,
) -> Result<Value> {
    let script = r#"
dir=$1
if [ ! -d "$dir" ]; then
    printf "%s\n" "not a directory: $dir" >&2
    exit 1
fi
for child in "$dir"/* "$dir"/.[!.]* "$dir"/..?*; do
    [ -e "$child" ] || [ -L "$child" ] || continue
    if [ -L "$child" ]; then
        kind=symlink
    elif [ -d "$child" ]; then
        kind=dir
    elif [ -f "$child" ]; then
        kind=file
    else
        kind=other
    fi
    size=$(stat -c "%s" "$child" 2>/dev/null || stat -f "%z" "$child" 2>/dev/null || printf 0)
    modified=$(stat -c "%Y" "$child" 2>/dev/null || stat -f "%m" "$child" 2>/dev/null || printf "")
    name=${child##*/}
    printf "%s\000%s\000%s\000%s\000%s\000" "$name" "$child" "$kind" "$size" "$modified"
done
"#;
    let output = run_script(sessions, target_name, ssh, script, &[path], timeout)?;
    if output.exit_code != Some(0) {
        return Err(Error::Tool(format!(
            "remote list failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    parse_list_dir_output(&output.stdout)
}

fn append_printf_chunks(script: &mut String, bytes: &[u8]) {
    for chunk in bytes.chunks(4096) {
        let mut escaped = String::with_capacity(chunk.len() * 4);
        for byte in chunk {
            escaped.push('\\');
            escaped.push(char::from(b'0' + (byte >> 6)));
            escaped.push(char::from(b'0' + ((byte >> 3) & 0o7)));
            escaped.push(char::from(b'0' + (byte & 0o7)));
        }
        script.push_str("printf '%b' ");
        script.push_str(&shell_quote(&escaped));
        script.push_str(" >> \"$tmp\" || exit 1\n");
    }
}

fn parse_list_dir_output(stdout: &[u8]) -> Result<Value> {
    let mut fields: Vec<&[u8]> = stdout.split(|byte| *byte == 0).collect();
    if fields.last().is_some_and(|field| field.is_empty()) {
        fields.pop();
    }
    let (chunks, remainder) = fields.as_chunks::<5>();
    if !remainder.is_empty() {
        return Err(Error::Tool(format!(
            "remote list returned malformed metadata: expected groups of 5 fields, got {}",
            fields.len()
        )));
    }

    let mut entries = Vec::new();
    for field in chunks {
        let name = decode_field(field[0]);
        let path = decode_field(field[1]);
        let kind = decode_field(field[2]);
        let size_text = decode_field(field[3]);
        let modified_text = decode_field(field[4]);

        let size = size_text.trim().parse::<u64>().map_err(|err| {
            Error::Tool(format!(
                "remote list returned invalid size for {path}: {size_text:?}: {err}"
            ))
        })?;
        let modified_unix = if modified_text.trim().is_empty() {
            None
        } else {
            Some(modified_text.trim().parse::<u64>().map_err(|err| {
                Error::Tool(format!(
                    "remote list returned invalid mtime for {path}: {modified_text:?}: {err}"
                ))
            })?)
        };

        entries.push(json!({
            "name": name,
            "path": path,
            "kind": kind,
            "size": size,
            "modified_unix": modified_unix,
        }));
    }

    entries.sort_by(|left, right| {
        let left = left.get("name").and_then(Value::as_str).unwrap_or_default();
        let right = right
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        left.cmp(right)
    });

    Ok(json!({ "entries": entries }))
}

fn decode_field(field: &[u8]) -> String {
    String::from_utf8_lossy(field).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_remote_list_nul_records() {
        let fields: [&[u8]; 10] = [
            b"b", b"/tmp/b", b"file", b"12", b"172", b"a", b"/tmp/a", b"dir", b"0", b"",
        ];
        let mut output = Vec::new();
        for field in fields {
            output.extend_from_slice(field);
            output.push(0);
        }

        let value = parse_list_dir_output(&output).unwrap();
        let entries = value["entries"].as_array().unwrap();

        assert_eq!(entries[0]["name"].as_str(), Some("a"));
        assert_eq!(entries[0]["modified_unix"], Value::Null);
        assert_eq!(entries[1]["name"].as_str(), Some("b"));
        assert_eq!(entries[1]["size"].as_u64(), Some(12));
        assert_eq!(entries[1]["modified_unix"].as_u64(), Some(172));
    }

    #[test]
    fn rejects_malformed_remote_list_records() {
        let err = parse_list_dir_output(b"name\0path\0").unwrap_err();
        assert!(err.to_string().contains("malformed metadata"));
    }
}
