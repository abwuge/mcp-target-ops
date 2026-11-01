use super::{run_script, SshSessionRegistry};
use crate::core::{
    config::SshTargetConfig,
    error::{Error, Result},
    util::shell_quote,
};
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

pub fn is_regular_file(
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
        r#"if [ -f "$1" ]; then exit 0; else exit 1; fi"#,
        &[path],
        timeout,
    )?;
    match output.exit_code {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(Error::Tool(format!(
            "remote file type check failed: {}",
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
