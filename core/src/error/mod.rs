pub mod config;

pub use config::ConfigError;

pub type Result<T> = std::result::Result<T, Error>;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}")]
    Config(#[from] ConfigError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("sqlite: {0}")]
    Sqlite(String),
    #[error("rpc: {0}")]
    Rpc(String),
    #[error("decode: {0}")]
    Decode(String),
    #[error("job cancelled")]
    Cancelled,
    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn sqlite(e: impl std::fmt::Display) -> Self {
        Self::Sqlite(e.to_string())
    }

    pub fn rpc(e: impl std::fmt::Display) -> Self {
        Self::Rpc(e.to_string())
    }

    pub fn decode(e: impl std::fmt::Display) -> Self {
        Self::Decode(e.to_string())
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sqlite(e.to_string())
    }
}

impl From<String> for Error {
    fn from(s: String) -> Self {
        Error::Other(s)
    }
}

impl From<&str> for Error {
    fn from(s: &str) -> Self {
        Error::Other(s.to_string())
    }
}

impl From<anyhow::Error> for Error {
    fn from(e: anyhow::Error) -> Self {
        // Prefer typed variants when anyhow carried a typed source.
        if let Some(sql) = e.downcast_ref::<rusqlite::Error>() {
            return Self::Sqlite(sql.to_string());
        }
        Error::Other(e.to_string())
    }
}
