use thiserror::Error;

#[derive(Debug, Error)]
pub enum ReplayError {
    #[error("block not found in cache ({0})")]
    BlockNotFound(u64),
    #[error("state trie error ({0})")]
    StateTrie(String),
    #[error("execution error ({0})")]
    Execution(String),
}
