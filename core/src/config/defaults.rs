use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Per-chain runtime parameters loaded from the configuration file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChainConfig {
    pub chain_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balancer_vault: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aave_v3_pool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uniswap_v3_factories: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uniswap_v2_factories: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solidly_factories: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camelot_factories: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool_discovery_start_block: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool_discovery_batch_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrapped_native_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uniswap_v2_default_fee: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curve_registry: Option<String>,
    /// Uniswap V4 singleton PoolManager contract address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub v4_pool_manager: Option<String>,
    /// Trader Joe V2 LB factory contract address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trader_joe_factory: Option<String>,
    /// Pendle Finance factory contract address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pendle_factory: Option<String>,
}

pub fn default_chains() -> HashMap<String, ChainConfig> {
    toml::from_str(include_str!("../../data/chains.toml"))
        .expect("invalid chains.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `chains.toml` must stay parseable — discovery silently degrades
    /// to defaults when chain entries fail to deserialize.
    #[test]
    fn test_default_chains_parse() {
        let chains = default_chains();
        assert!(chains.contains_key("polygon"));
        assert!(chains.contains_key("ethereum"));
    }
}





