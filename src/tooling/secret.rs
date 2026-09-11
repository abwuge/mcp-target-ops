use crate::{
    core::{
        config::TargetConfig,
        error::{Error, Result},
        policy::{self, FileAccess},
        secret::{SecretFormat, SecretRef},
        state::AppState,
        target::{TargetId, TargetSource},
    },
    transport::ssh,
};
use serde_json::Value as JsonValue;
use std::{collections::BTreeMap, fs, time::Duration};

pub fn resolve_env(
    state: &AppState,
    target: &TargetId,
    config: &TargetConfig,
    source: TargetSource,
    refs: &BTreeMap<String, SecretRef>,
    timeout: Duration,
) -> Result<BTreeMap<String, String>> {
    let mut resolved = BTreeMap::new();
    for (name, secret_ref) in refs {
        validate_env_name(name)?;
        policy::check_file(target, config, &secret_ref.path, FileAccess::Read, source)?;
        let bytes = read_bytes(state, target, config, &secret_ref.path, timeout)?;
        let mut value = resolve_value(secret_ref, &bytes)?;
        if secret_ref.trim {
            value = value.trim().to_string();
        }
        resolved.insert(name.clone(), value);
    }
    Ok(resolved)
}

pub fn resolve_ref(
    state: &AppState,
    target: &TargetId,
    config: &TargetConfig,
    source: TargetSource,
    secret_ref: &SecretRef,
    timeout: Duration,
) -> Result<String> {
    policy::check_file(target, config, &secret_ref.path, FileAccess::Read, source)?;
    let bytes = read_bytes(state, target, config, &secret_ref.path, timeout)?;
    let mut value = resolve_value(secret_ref, &bytes)?;
    if secret_ref.trim {
        value = value.trim().to_string();
    }
    Ok(value)
}

fn read_bytes(
    state: &AppState,
    target: &TargetId,
    config: &TargetConfig,
    path: &str,
    timeout: Duration,
) -> Result<Vec<u8>> {
    match (target, config) {
        (TargetId::Local, TargetConfig::Local(_)) => Ok(fs::read(path)?),
        (TargetId::Ssh(name), TargetConfig::Ssh(ssh_config)) => {
            ssh::read_file(&state.ssh_sessions, name, ssh_config, path, timeout)
        }
        _ => Err(Error::Target(format!(
            "target {target} has mismatched config"
        ))),
    }
}

fn resolve_value(secret_ref: &SecretRef, bytes: &[u8]) -> Result<String> {
    let text = String::from_utf8(bytes.to_vec())
        .map_err(|_| Error::Tool(format!("secret file {} is not UTF-8", secret_ref.path)))?;

    match secret_ref.format {
        SecretFormat::Text => {
            if secret_ref.key.is_some() {
                return Err(Error::Tool(
                    "secret key is not valid for text format".to_string(),
                ));
            }
            Ok(text)
        }
        SecretFormat::Toml => {
            let key = required_key(secret_ref)?;
            let value: toml::Value = toml::from_str(&text)?;
            let value = lookup_toml(&value, key)?;
            scalar_to_string_toml(value, key)
        }
        SecretFormat::Json => {
            let key = required_key(secret_ref)?;
            let value: JsonValue = serde_json::from_str(&text)?;
            let value = lookup_json(&value, key)?;
            scalar_to_string_json(value, key)
        }
    }
}

fn required_key(secret_ref: &SecretRef) -> Result<&str> {
    secret_ref
        .key
        .as_deref()
        .filter(|key| !key.is_empty())
        .ok_or_else(|| Error::Tool("secret key is required for structured formats".to_string()))
}

fn lookup_toml<'a>(root: &'a toml::Value, key: &str) -> Result<&'a toml::Value> {
    let mut current = root;
    for segment in key.split('.') {
        current = current.get(segment).ok_or_else(|| {
            Error::Tool(format!("secret key {key:?} was not found in TOML source"))
        })?;
    }
    Ok(current)
}

fn lookup_json<'a>(root: &'a JsonValue, key: &str) -> Result<&'a JsonValue> {
    let mut current = root;
    for segment in key.split('.') {
        current = current.get(segment).ok_or_else(|| {
            Error::Tool(format!("secret key {key:?} was not found in JSON source"))
        })?;
    }
    Ok(current)
}

fn scalar_to_string_toml(value: &toml::Value, key: &str) -> Result<String> {
    match value {
        toml::Value::String(value) => Ok(value.clone()),
        toml::Value::Integer(value) => Ok(value.to_string()),
        toml::Value::Float(value) => Ok(value.to_string()),
        toml::Value::Boolean(value) => Ok(value.to_string()),
        _ => Err(Error::Tool(format!(
            "secret key {key:?} must resolve to a scalar TOML value"
        ))),
    }
}

fn scalar_to_string_json(value: &JsonValue, key: &str) -> Result<String> {
    match value {
        JsonValue::String(value) => Ok(value.clone()),
        JsonValue::Number(value) => Ok(value.to_string()),
        JsonValue::Bool(value) => Ok(value.to_string()),
        _ => Err(Error::Tool(format!(
            "secret key {key:?} must resolve to a scalar JSON value"
        ))),
    }
}

fn validate_env_name(name: &str) -> Result<()> {
    let mut chars = name.chars();
    match chars.next() {
        Some('_') => {}
        Some(ch) if ch.is_ascii_alphabetic() => {}
        _ => {
            return Err(Error::Tool(format!(
                "secret environment name {name:?} is invalid"
            )))
        }
    }
    if chars.any(|ch| !(ch == '_' || ch.is_ascii_alphanumeric())) {
        return Err(Error::Tool(format!(
            "secret environment name {name:?} is invalid"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_nested_toml_scalar() {
        let secret_ref = SecretRef {
            path: "unused".to_string(),
            format: SecretFormat::Toml,
            key: Some("mcp.servers.memory.headers.Authorization".to_string()),
            trim: true,
        };
        let bytes = br#"
[mcp.servers.memory.headers]
Authorization = "Bearer hidden"
"#;
        assert_eq!(resolve_value(&secret_ref, bytes).unwrap(), "Bearer hidden");
    }

    #[test]
    fn validates_environment_names() {
        assert!(validate_env_name("MEMORY_AUTH").is_ok());
        assert!(validate_env_name("_TOKEN2").is_ok());
        assert!(validate_env_name("2TOKEN").is_err());
        assert!(validate_env_name("BAD-NAME").is_err());
    }
}
