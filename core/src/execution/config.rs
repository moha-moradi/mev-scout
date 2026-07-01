use alloy::primitives::Address;

use super::broadcaster::BroadcastMode;

#[derive(Debug, Clone)]
pub struct ExecutionConfig {
    pub private_key: Option<String>,
    pub broadcast_mode: BroadcastMode,
    pub executor_factory: Option<Address>,
    pub flashbots_relay_url: Option<String>,
    pub mevshare_relay_url: Option<String>,
    pub confirmation_blocks: u64,
    pub gas_limit_multiplier: f64,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        ExecutionConfig {
            private_key: None,
            broadcast_mode: BroadcastMode::Public,
            executor_factory: None,
            flashbots_relay_url: Some("https://relay.flashbots.net".into()),
            mevshare_relay_url: Some("https://mev-share.flashbots.net".into()),
            confirmation_blocks: 1,
            gas_limit_multiplier: 1.2,
        }
    }
}
