use crate::core::{
    error::{Error, Result},
    util::sha256_hex,
};
use serde::{Deserialize, Serialize};
use similar::TextDiff;

#[derive(Debug, Clone, Deserialize)]
pub struct TextEdit {
    pub old: String,
    pub new: String,

    #[serde(default)]
    pub replace_all: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct EditOutcome {
    pub changed: bool,
    pub old_sha256: String,
    pub new_sha256: String,
    pub diff: String,
    pub text: String,
}

pub fn apply_text_edits(
    original: &str,
    expected_sha256: Option<&str>,
    edits: &[TextEdit],
) -> Result<EditOutcome> {
    let old_sha256 = sha256_hex(original.as_bytes());
    if let Some(expected) = expected_sha256 {
        if expected != old_sha256 {
            return Err(Error::Tool(format!(
                "file changed before edit: expected sha256 {expected}, got {old_sha256}"
            )));
        }
    }

    let mut current = original.to_string();
    for (idx, edit) in edits.iter().enumerate() {
        if edit.old.is_empty() {
            return Err(Error::Tool(format!("edit #{idx} has empty old text")));
        }

        let count = current.matches(&edit.old).count();
        if count == 0 {
            return Err(Error::Tool(format!("edit #{idx} old text not found")));
        }
        if count > 1 && !edit.replace_all {
            return Err(Error::Tool(format!(
                "edit #{idx} matched {count} occurrences; set replace_all=true to replace all"
            )));
        }

        if edit.replace_all {
            current = current.replace(&edit.old, &edit.new);
        } else {
            current = current.replacen(&edit.old, &edit.new, 1);
        }
    }

    let new_sha256 = sha256_hex(current.as_bytes());
    let changed = original != current;
    let diff = unified_diff(original, &current);

    Ok(EditOutcome {
        changed,
        old_sha256,
        new_sha256,
        diff,
        text: current,
    })
}

pub fn unified_diff(original: &str, current: &str) -> String {
    if original == current {
        return String::new();
    }

    TextDiff::from_lines(original, current)
        .unified_diff()
        .header("before", "after")
        .to_string()
}
