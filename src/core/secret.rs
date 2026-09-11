use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretRef {
    pub path: String,
    #[serde(default)]
    pub format: SecretFormat,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default = "default_trim")]
    pub trim: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretFormat {
    #[default]
    Text,
    Toml,
    Json,
}

fn default_trim() -> bool {
    true
}
