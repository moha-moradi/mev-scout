use thiserror::Error;

#[derive(Debug, Error)]
pub enum RpcError {
    #[error("rpc call failed ({0})")]
    CallFailed(String),
    #[error("all providers failed")]
    AllProvidersFailed,
    #[error("rate limited")]
    RateLimited,
    #[error("invalid response ({0})")]
    InvalidResponse(String),
}
