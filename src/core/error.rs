use std::io;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("target error: {0}")]
    Target(String),

    #[error("policy denied: {0}")]
    Policy(String),

    #[error("tool error: {0}")]
    Tool(String),

    #[error("terminal error: {0}")]
    Terminal(String),

    #[error("io error: {0}")]
    Io(#[from] io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("toml decode error: {0}")]
    TomlDe(#[from] toml::de::Error),

    #[error("toml encode error: {0}")]
    TomlSer(#[from] toml::ser::Error),

    #[error("utf-8 error: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
}

impl Error {
    pub fn json_rpc_code(&self) -> i64 {
        match self {
            Error::Json(_) => -32700,
            Error::Config(_)
            | Error::Target(_)
            | Error::Policy(_)
            | Error::Tool(_)
            | Error::Terminal(_) => -32000,
            Error::Io(_)
            | Error::Database(_)
            | Error::TomlDe(_)
            | Error::TomlSer(_)
            | Error::Utf8(_) => -32001,
        }
    }
}
