pub mod cache;
pub mod config;
pub mod replay;
pub mod rpc;

pub use cache::*;
pub use config::*;
pub use replay::*;
pub use rpc::*;

pub type Result<T> = std::result::Result<T, Error>;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}")]
    Config(#[from] ConfigError),
    #[error("{0}")]
    Rpc(#[from] RpcError),
    #[error("{0}")]
    Replay(#[from] ReplayError),
    #[error("{0}")]
    Cache(#[from] CacheError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
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
        Error::Other(e.to_string())
    }
}
