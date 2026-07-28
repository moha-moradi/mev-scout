use thiserror::Error;

#[derive(Debug, Error)]
pub enum CacheError {
    #[error("cache miss ({0})")]
    Miss(String),
    #[error("serialization error ({0})")]
    Serialization(String),
    #[error("corrupt data ({0})")]
    CorruptData(String),
}

#[derive(Debug, Error)]
pub enum SqliteError {
    #[error("query failed ({0})")]
    Query(String),
    #[error("migration failed ({0})")]
    Migration(String),
    #[error("connection error ({0})")]
    Connection(String),
}
