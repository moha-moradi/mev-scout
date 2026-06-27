use thiserror::Error;

#[derive(Debug, Error)]
pub enum ReplayError {
    #[error("Block {0} not found in cache")]
    BlockNotFound(u64),
    #[error("State trie error: {0}")]
    StateTrie(String),
    #[error("Execution error: {0}")]
    Execution(String),
}
