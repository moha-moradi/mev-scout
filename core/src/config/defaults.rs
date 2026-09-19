use std::collections::HashMap;

use alloy::primitives::Address;
use serde::{Deserialize, Serialize};

use super::settings::RpcConfig;

/// Per-chain runtime parameters loaded from the configuration file.
///
/// Address-bearing fields are typed as [`Address`] so a typo'd hex string
/// fails at config load (serde) instead of silently dropping a DEX venue.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChainConfig {
    pub chain_id: u64,
    /// Per-chain RPC override (`[chains.<name>.rpc]`). When present, its
    /// `rpc_url` / `rpc_urls` / `rpc_rps` win over the top-level (global
    /// default) values for this chain; `rps_limit` / `block_concurrency`
    /// stay global. Absent = use the top-level RPC config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpc: Option<RpcConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balancer_vault: Option<Address>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aave_v3_pool: Option<Address>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uniswap_v3_factories: Option<Vec<Address>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uniswap_v2_factories: Option<Vec<Address>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solidly_factories: Option<Vec<Address>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camelot_factories: Option<Vec<Address>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool_discovery_start_block: Option<u64>,
    /// When no CLI block range is given, scan the last N blocks to tip
    /// (preferred over `pool_discovery_start_block` for default discovery).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool_discovery_lookback_blocks: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool_discovery_batch_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrapped_native_token: Option<Address>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uniswap_v2_default_fee: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curve_registry: Option<Address>,
    /// Curve stableswap factory contract addresses (CurveStableswapFactoryNG
    /// deployments emitting `PoolDeployed(address)`; older factories emit
    /// `PoolAdded(address,uint256)` — both are scanned).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curve_factories: Option<Vec<Address>>,
    /// Uniswap V4 singleton PoolManager contract address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub v4_pool_manager: Option<Address>,
    /// Pancake Infinity singleton CLPoolManager contract address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub infinity_cl_pool_manager: Option<Address>,
    /// Trader Joe / LFJ V2 LB factory contract addresses (V2.1 + V2.2 can coexist).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "trader_joe_factory"
    )]
    pub trader_joe_factories: Option<Vec<Address>>,
    /// Pendle Finance factory contract address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pendle_factory: Option<Address>,
    /// Metric V2 AMM factory address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metric_factory: Option<Address>,
    /// Fluid DEX factory address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fluid_factory: Option<Address>,
}

impl ChainConfig {
    /// Fill unset `Option` fields from `defaults`, keeping user-provided values.
    ///
    /// A partial `[chains.<name>]` section (e.g. only `chain_id` + `rpc`) would
    /// otherwise replace the whole built-in entry and drop
    /// `pool_discovery_start_block`, factories, vaults, etc.
    pub fn merge_defaults(&mut self, defaults: &ChainConfig) {
        // `chain_id` is required on every section; keep the user's value.
        macro_rules! fill {
            ($($field:ident),+ $(,)?) => {
                $(
                    if self.$field.is_none() {
                        self.$field = defaults.$field.clone();
                    }
                )+
            };
        }
        fill!(
            rpc,
            balancer_vault,
            aave_v3_pool,
            uniswap_v3_factories,
            uniswap_v2_factories,
            solidly_factories,
            camelot_factories,
            pool_discovery_start_block,
            pool_discovery_lookback_blocks,
            pool_discovery_batch_size,
            wrapped_native_token,
            uniswap_v2_default_fee,
            curve_registry,
            curve_factories,
            v4_pool_manager,
            infinity_cl_pool_manager,
            trader_joe_factories,
            pendle_factory,
            metric_factory,
            fluid_factory,
        );
    }
}

pub fn default_chains() -> HashMap<String, ChainConfig> {
    toml::from_str(include_str!("../../data/chains.toml")).expect("invalid chains.toml")
}

/// Merge built-in chain defaults into `chains`: insert missing chains, and for
/// chains already present fill only unset `Option` fields (user overrides win).
pub fn merge_default_chains(chains: &mut HashMap<String, ChainConfig>) {
    for (name, default_cfg) in default_chains() {
        match chains.entry(name) {
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(default_cfg);
            }
            std::collections::hash_map::Entry::Occupied(mut e) => {
                e.get_mut().merge_defaults(&default_cfg);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

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
            bsc.infinity_cl_pool_manager,
            Some(address!("0xa0FfB9c1CE1Fe56963B0321B32E7A0302114058b"))
        );
    }

    #[test]
    fn ethereum_wires_fluid_and_metric_factories() {
        let chains = default_chains();
        let ethereum = &chains["ethereum"];
        assert_eq!(
            ethereum.fluid_factory,
            Some(address!("0x91716C4EDA1Fb55e84Bf8b4c7085f84285c19085"))
        );
        assert_eq!(
            ethereum.metric_factory,
            Some(address!("0xe22F9fc0f04486dE25ed6CF1800a4a47aFD82e0C"))
        );
    }

    #[test]
    fn metric_factory_wired_on_all_supported_chains() {
        let chains = default_chains();
        let expected = address!("0xe22F9fc0f04486dE25ed6CF1800a4a47aFD82e0C");
        for name in [
            "polygon",
            "avalanche",
            "bsc",
            "arbitrum",
            "base",
            "ethereum",
            "optimism",
        ] {
            let cfg = &chains[name];
            assert_eq!(
                cfg.metric_factory,
                Some(expected),
                "{name} must wire the Metric V2 factory"
            );
        }
    }

    #[test]
    fn merge_defaults_fills_unset_fields_keeps_overrides() {
        let defaults = default_chains();
        let poly_default = defaults["polygon"].clone();

        let mut partial = ChainConfig {
            chain_id: 137,
            rpc: Some(super::super::settings::RpcConfig {
                rpc_urls: vec!["https://example.invalid".into()],
                ..Default::default()
            }),
            // Explicitly override one address; leave the rest unset.
            balancer_vault: Some(address!("0x1111111111111111111111111111111111111111")),
            ..Default::default()
        };

        partial.merge_defaults(&poly_default);

        assert_eq!(
            partial.pool_discovery_start_block,
            poly_default.pool_discovery_start_block,
            "unset pool_discovery_start_block must come from defaults"
        );
        assert_eq!(
            partial.uniswap_v3_factories,
            poly_default.uniswap_v3_factories
        );
        assert_eq!(
            partial.balancer_vault,
            Some(address!("0x1111111111111111111111111111111111111111")),
            "user-provided vault must win"
        );
        assert_eq!(
            partial.rpc.as_ref().unwrap().rpc_urls,
            vec!["https://example.invalid".to_string()],
            "user rpc must win"
        );
    }

    #[test]
    fn merge_default_chains_partial_polygon_keeps_start_block() {
        let mut chains = HashMap::new();
        chains.insert(
            "polygon".to_string(),
            ChainConfig {
                chain_id: 137,
                ..Default::default()
            },
        );
        merge_default_chains(&mut chains);
        assert_eq!(
            chains["polygon"].pool_discovery_start_block,
            Some(49_100_000)
        );
        assert!(chains.contains_key("ethereum"));
    }
}
