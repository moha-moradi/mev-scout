use std::collections::HashMap;

use serde::{Deserialize, Serialize};

pub use crate::types::{SubgraphConfig, SubgraphSchema};

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
    /// Off-chain subgraph sources for this chain (preferred over on-chain scan).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subgraphs: Vec<SubgraphConfig>,
}

pub fn default_chains() -> HashMap<String, ChainConfig> {
    toml::from_str(include_str!("../../data/chains.toml"))
        .expect("invalid chains.toml")
}





