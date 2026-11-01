use sha2::{Digest, Sha256};
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Human-readable 96-bit SHA-256 prefix for short content version tokens.
/// The full SHA-256 is still computed for internal integrity checks.
pub fn revision_from_sha256(full_hash: &str) -> String {
    debug_assert!(full_hash.len() == 64 && full_hash.bytes().all(|b| b.is_ascii_hexdigit()));
    full_hash[..24].to_string()
}

pub fn revision_matches(full_hash: &str, expected: &str) -> bool {
    expected.len() == 24
        && expected
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && revision_from_sha256(full_hash) == expected
}

pub fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }

    let safe = value.bytes().all(|b| {
        b.is_ascii_alphanumeric()
            || matches!(b, b'_' | b'-' | b'.' | b'/' | b':' | b'@' | b'=' | b',')
    });

    if safe {
        return value.to_string();
    }

    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn truncate_bytes(mut bytes: Vec<u8>, max_bytes: Option<usize>) -> (Vec<u8>, bool) {
    if let Some(max) = max_bytes {
        if bytes.len() > max {
            bytes.truncate(max);
            return (bytes, true);
        }
    }
    (bytes, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_revision_is_short_stable_and_content_bound() {
        let original = sha256_hex(b"before\n");
        let same = sha256_hex(b"before\n");
        let different = sha256_hex(b"after\n");
        let revision = revision_from_sha256(&original);
        assert_eq!(revision.len(), 24);
        assert!(original.starts_with(&revision));
        assert!(revision_matches(&original, &revision));
        assert_eq!(revision, revision_from_sha256(&same));
        assert!(!revision_matches(&different, &revision));
        assert!(!revision_matches(&original, "too-short"));
        assert!(!revision_matches(&original, &"z".repeat(24)));
    }

    #[test]
    fn quotes_single_quotes() {
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    }
}
