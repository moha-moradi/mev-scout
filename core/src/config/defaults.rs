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
    /// Curve stableswap factory contract addresses (CurveStableswapFactoryNG
    /// deployments emitting `PoolDeployed(address)`; older factories emit
    /// `PoolAdded(address,uint256)` — both are scanned).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curve_factories: Option<Vec<String>>,
    /// Uniswap V4 singleton PoolManager contract address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub v4_pool_manager: Option<String>,
    /// Pancake Infinity singleton CLPoolManager contract address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub infinity_cl_pool_manager: Option<String>,
    /// Trader Joe / LFJ V2 LB factory contract addresses (V2.1 + V2.2 can coexist).
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "trader_joe_factory")]
    pub trader_joe_factories: Option<Vec<String>>,
    /// Pendle Finance factory contract address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pendle_factory: Option<String>,
    /// Metric V2 AMM factory address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metric_factory: Option<String>,
    /// Fluid DEX factory address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fluid_factory: Option<String>,
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

    #[test]
    fn bsc_wires_pancake_infinity_manager() {
        let chains = default_chains();
        let bsc = &chains["bsc"];
        assert_eq!(
            bsc.infinity_cl_pool_manager.as_deref(),
            Some("0xa0FfB9c1CE1Fe56963B0321B32E7A0302114058b")
        );
    }

    #[test]
    fn ethereum_wires_fluid_and_metric_factories() {
        let chains = default_chains();
        let ethereum = &chains["ethereum"];
        assert_eq!(
            ethereum.fluid_factory.as_deref(),
            Some("0x91716C4EDA1Fb55e84Bf8b4c7085f84285c19085")
        );
        assert_eq!(
            ethereum.metric_factory.as_deref(),
            Some("0xe22F9fc0f04486dE25ed6CF1800a4a47aFD82e0C")
        );
    }

    #[test]
    fn metric_factory_wired_on_all_supported_chains() {
        let chains = default_chains();
        for name in [
            "polygon", "avalanche", "bsc", "arbitrum", "base", "ethereum", "optimism",
        ] {
            let cfg = &chains[name];
            assert_eq!(
                cfg.metric_factory.as_deref(),
                Some("0xe22F9fc0f04486dE25ed6CF1800a4a47aFD82e0C"),
                "{name} must wire the Metric V2 factory"
            );
        }
    }
}