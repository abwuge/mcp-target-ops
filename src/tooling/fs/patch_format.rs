use crate::core::error::{Error, Result};

#[derive(Debug, Clone)]
pub(super) struct UnifiedFilePatch {
    old_path: String,
    new_path: String,
    pub(super) patch: String,
}

pub(super) fn split(patch: &str) -> Result<Vec<UnifiedFilePatch>> {
    let lines: Vec<&str> = patch.split_inclusive('\n').collect();
    let mut sections = Vec::new();
    let mut index = 0;

    while index < lines.len() {
        let is_header = lines[index].starts_with("--- ")
            && index + 1 < lines.len()
            && lines[index + 1].starts_with("+++ ");
        if !is_header {
            index += 1;
            continue;
        }

        let old_path = parse_header_path(lines[index], "--- ")?;
        let new_path = parse_header_path(lines[index + 1], "+++ ")?;
        let start = index;
        index += 2;
        while index < lines.len() {
            if lines[index].starts_with("diff --git ")
                || (lines[index].starts_with("--- ")
                    && index + 1 < lines.len()
                    && lines[index + 1].starts_with("+++ "))
            {
                break;
            }
            index += 1;
        }
        sections.push(UnifiedFilePatch {
            old_path,
            new_path,
            patch: lines[start..index].concat(),
        });
    }

    Ok(sections)
}

pub(super) fn resolve_path(base_dir: &str, section: &UnifiedFilePatch) -> Result<String> {
    if section.old_path == "/dev/null" || section.new_path == "/dev/null" {
        return Err(Error::Tool(
            "multi-file file_patch does not yet support creating or deleting files".to_string(),
        ));
    }

    let old_path = strip_git_prefix(&section.old_path);
    let candidate = strip_git_prefix(&section.new_path);
    if old_path != candidate {
        return Err(Error::Tool(
            "multi-file file_patch does not yet support file renames".to_string(),
        ));
    }
    if candidate.is_empty() {
        return Err(Error::Tool(
            "multi-file patch contains an empty target path".to_string(),
        ));
    }
    if candidate.starts_with('/')
        || candidate
            .split('/')
            .any(|segment| segment == ".." || segment.is_empty())
    {
        return Err(Error::Tool(format!(
            "multi-file patch path {candidate:?} must be a relative path contained under the base directory"
        )));
    }

    Ok(format!(
        "{}/{}",
        base_dir.trim_end_matches('/'),
        candidate.trim_start_matches("./")
    ))
}

fn parse_header_path(line: &str, prefix: &str) -> Result<String> {
    let value = line
        .strip_prefix(prefix)
        .ok_or_else(|| Error::Tool(format!("invalid unified patch header: {line:?}")))?
        .trim_end_matches(['\r', '\n'])
        .split('\t')
        .next()
        .unwrap_or("")
        .trim();
    if value.is_empty() {
        return Err(Error::Tool(
            "unified patch header has an empty path".to_string(),
        ));
    }
    Ok(value.to_string())
}

fn strip_git_prefix(path: &str) -> &str {
    path.strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_multi_file_diff_and_strips_git_metadata() {
        let patch = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old a\n+new a\ndiff --git a/src/b.rs b/src/b.rs\n--- a/src/b.rs\n+++ b/src/b.rs\n@@ -1 +1 @@\n-old b\n+new b\n";
        let sections = split(patch).expect("patch splits");
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].new_path, "b/src/a.rs");
        assert!(!sections[0].patch.contains("diff --git"));
        assert_eq!(
            resolve_path("/repo", &sections[1]).expect("path resolves"),
            "/repo/src/b.rs"
        );
    }

    #[test]
    fn rejects_create_delete_sections() {
        let section = UnifiedFilePatch {
            old_path: "/dev/null".to_string(),
            new_path: "b/new.txt".to_string(),
            patch: String::new(),
        };
        assert!(resolve_path("/repo", &section).is_err());
    }

    #[test]
    fn rejects_escape_and_rename_paths() {
        let escape = UnifiedFilePatch {
            old_path: "a/../outside.txt".to_string(),
            new_path: "b/../outside.txt".to_string(),
            patch: String::new(),
        };
        assert!(resolve_path("/repo", &escape).is_err());

        let rename = UnifiedFilePatch {
            old_path: "a/old.txt".to_string(),
            new_path: "b/new.txt".to_string(),
            patch: String::new(),
        };
        assert!(resolve_path("/repo", &rename).is_err());
    }
}
