use thiserror::Error;

#[derive(Debug, Error)]
pub enum CacheError {
    #[error("Cache miss: {0}")]
    Miss(String),
    #[error("Serialization error: {0}")]
    Serialization(String),
    #[error("Corrupt data: {0}")]
    CorruptData(String),
}

#[derive(Debug, Error)]
pub enum SqliteError {
    #[error("Query failed: {0}")]
    Query(String),
    #[error("Migration failed: {0}")]
    Migration(String),
    #[error("Connection error: {0}")]
    Connection(String),
}
