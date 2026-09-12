use crate::core::error::{Error, Result};
use serde::Serialize;
use std::{fmt, str::FromStr};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetId {
    Local,
    Ssh(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetSource {
    Explicit,
    Active,
    Default,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolvedTarget {
    pub target: String,
    pub source: TargetSource,
}

impl TargetId {
    pub fn config_key(&self) -> &str {
        match self {
            TargetId::Local => "local",
            TargetId::Ssh(name) => name.as_str(),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            TargetId::Local => "local",
            TargetId::Ssh(_) => "ssh",
        }
    }
}

impl fmt::Display for TargetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TargetId::Local => write!(f, "local"),
            TargetId::Ssh(name) => write!(f, "ssh:{name}"),
        }
    }
}

impl FromStr for TargetId {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        if value == "local" {
            return Ok(TargetId::Local);
        }

        if let Some(name) = value.strip_prefix("ssh:") {
            if name.is_empty() {
                return Err(Error::Target("empty ssh target name".to_string()));
            }
            if name.trim() != name {
                return Err(Error::Target(
                    "ssh target names must not be padded with whitespace".to_string(),
                ));
            }
            if name == "local" {
                return Err(Error::Target(
                    "ssh:local is invalid because 'local' is reserved for the local target"
                        .to_string(),
                ));
            }
            return Ok(TargetId::Ssh(name.to_string()));
        }

        Err(Error::Target(format!(
            "invalid target '{value}'. Use 'local' or 'ssh:<profile>'"
        )))
    }
}

impl ResolvedTarget {
    pub fn new(target: TargetId, source: TargetSource) -> Self {
        Self {
            target: target.to_string(),
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_targets() {
        assert_eq!("local".parse::<TargetId>().unwrap(), TargetId::Local);
        assert_eq!(
            "ssh:dev".parse::<TargetId>().unwrap(),
            TargetId::Ssh("dev".to_string())
        );
        assert!("dev".parse::<TargetId>().is_err());
        assert!("ssh:".parse::<TargetId>().is_err());
        assert!("ssh: dev".parse::<TargetId>().is_err());
        assert!("ssh:dev ".parse::<TargetId>().is_err());
        assert!("ssh:local".parse::<TargetId>().is_err());
    }
}
