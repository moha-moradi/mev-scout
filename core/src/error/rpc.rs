use thiserror::Error;

#[derive(Debug, Error)]
pub enum RpcError {
    #[error("rpc call failed ({0})")]
    CallFailed(String),
    #[error("invalid response ({0})")]
    InvalidResponse(String),
}
