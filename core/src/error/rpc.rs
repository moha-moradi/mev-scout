use thiserror::Error;

#[derive(Debug, Error)]
pub enum RpcError {
    #[error("RPC call failed: {0}")]
    CallFailed(String),
    #[error("All providers failed")]
    AllProvidersFailed,
    #[error("Rate limited")]
    RateLimited,
    #[error("Invalid response: {0}")]
    InvalidResponse(String),
}
