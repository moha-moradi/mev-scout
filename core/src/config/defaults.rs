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
    /// SparkLend pool (Aave-V3 ABI alias — same `LiquidationCall` topic0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spark_pool: Option<Address>,
    /// Morpho Blue singleton (shared address on ETH/Base/Arb).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub morpho_blue: Option<Address>,
    /// Silo V2 PartialLiquidation hook receiver(s).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub silo_v2_partial_liquidation: Option<Vec<Address>>,
    /// Euler V2 vault factory / known liquidatable vault set (optional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub euler_v2_evault_factory: Option<Address>,
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
            spark_pool,
            morpho_blue,
            silo_v2_partial_liquidation,
            euler_v2_evault_factory,
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

    #[test]
    fn chains_toml_registry_covers_builtin_addresses() {
        use crate::types::ChainName;

        let chains = default_chains();
        for key in [
            "polygon",
            "avalanche",
            "bsc",
            "arbitrum",
            "base",
            "ethereum",
            "optimism",
        ] {
            assert!(chains.contains_key(key), "{key} missing from chains.toml");
        }

        let metric = address!("0xe22F9fc0f04486dE25ed6CF1800a4a47aFD82e0C");
        let fluid = address!("0x91716C4EDA1Fb55e84Bf8b4c7085f84285c19085");
        for name in [
            "polygon",
            "avalanche",
            "bsc",
            "arbitrum",
            "base",
            "ethereum",
            "optimism",
        ] {
            assert_eq!(chains[name].metric_factory, Some(metric), "{name}");
        }
        assert_eq!(
            chains["bsc"].infinity_cl_pool_manager,
            Some(address!("0xa0FfB9c1CE1Fe56963B0321B32E7A0302114058b"))
        );
        assert_eq!(chains["ethereum"].fluid_factory, Some(fluid));
        assert_eq!(chains["base"].fluid_factory, Some(fluid));
        assert_eq!(chains["arbitrum"].fluid_factory, Some(fluid));
        let pancake_v3 = address!("0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865");
        let ramses_v3 = address!("0xd0019e86edB35E1fedaaB03aED5c3c60f115d28b");
        let base_curve = address!("0xd2002373543Ce3527023C75e7518C274A51ce712");
        assert!(
            chains["bsc"]
                .uniswap_v3_factories
                .as_ref()
                .is_some_and(|v| v.contains(&pancake_v3)),
            "bsc missing Pancake V3"
        );
        assert!(
            chains["arbitrum"]
                .uniswap_v3_factories
                .as_ref()
                .is_some_and(|v| v.contains(&pancake_v3) && v.contains(&ramses_v3)),
            "arbitrum missing Pancake V3 / Ramses V3"
        );
        assert!(
            chains["base"]
                .curve_factories
                .as_ref()
                .is_some_and(|v| v.contains(&base_curve)),
            "base missing Curve Stableswap NG"
        );

        let contains = |chain: ChainName, got: &[&str], want: &str| {
            assert!(
                got.iter().any(|s| s.eq_ignore_ascii_case(want)),
                "{chain:?} missing {want}"
            );
        };
        contains(
            ChainName::Arbitrum,
            ChainName::Arbitrum.default_uniswap_v3_factories(),
            "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865",
        );
        contains(
            ChainName::Arbitrum,
            ChainName::Arbitrum.default_uniswap_v3_factories(),
            "0xd0019e86edB35E1fedaaB03aED5c3c60f115d28b",
        );
        contains(
            ChainName::Bsc,
            ChainName::Bsc.default_uniswap_v3_factories(),
            "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865",
        );
        contains(
            ChainName::Bsc,
            ChainName::Bsc.default_uniswap_v2_factories(),
            "0x858E3312ed3A876947EA49d572A7C42DE08af7EE",
        );
        contains(
            ChainName::Base,
            ChainName::Base.default_uniswap_v2_factories(),
            "0x8909Dc15e40173Ff4699343b6eB8132c65e18eC6",
        );
        contains(
            ChainName::Polygon,
            ChainName::Polygon.default_uniswap_v3_factories(),
            "0x2Bef16A0081565E72100D73CBe19B1Bd2d802380",
        );
        for want in [
            "0xAE6E5c62328ade73ceefD42228528b70c8157D0d",
            "0x1128F23D0bc0A8396E9FBC3c0c68f5EA228B8256",
            "0x512eb749541B7cf294be882D636218c84a5e9E5F",
        ] {
            contains(
                ChainName::Avalanche,
                ChainName::Avalanche.default_uniswap_v3_factories(),
                want,
            );
        }
        for chain in [
            ChainName::Ethereum,
            ChainName::Base,
            ChainName::Arbitrum,
            ChainName::Optimism,
            ChainName::Polygon,
            ChainName::Bsc,
            ChainName::Avalanche,
        ] {
            contains(
                chain,
                &chain.default_metric_factories(),
                "0xe22F9fc0f04486dE25ed6CF1800a4a47aFD82e0C",
            );
        }
        for chain in [ChainName::Ethereum, ChainName::Base, ChainName::Arbitrum] {
            contains(
                chain,
                chain.default_fluid_factories().as_slice(),
                "0x91716C4EDA1Fb55e84Bf8b4c7085f84285c19085",
            );
        }
        assert!(ChainName::Bsc.default_fluid_factories().is_empty());
        let avax_lb = ChainName::Avalanche.default_trader_joe_factories();
        contains(
            ChainName::Avalanche,
            avax_lb.as_slice(),
            "0xEb480050b016f6c6d45203D2346B68bDDDa23D4D",
        );
        contains(
            ChainName::Bsc,
            ChainName::Bsc.default_curve_factories(),
            "0xd7E72f3615aa65b92A4DBdC211E296a35512988B",
        );
        contains(
            ChainName::Polygon,
            ChainName::Polygon.default_curve_factories(),
            "0x1764ee18e8B3ccA4787249Ceb249356192594585",
        );
        contains(
            ChainName::Ethereum,
            ChainName::Ethereum.default_curve_factories(),
            "0xF6c9ffA64bD0aE8a068dd7b7d954c654A3E7F8a6",
        );
        contains(
            ChainName::Base,
            ChainName::Base.default_curve_factories(),
            "0xd2002373543Ce3527023C75e7518C274A51ce712",
        );

        for chain in [
            ChainName::Polygon,
            ChainName::Avalanche,
            ChainName::Bsc,
            ChainName::Arbitrum,
            ChainName::Base,
            ChainName::Ethereum,
            ChainName::Optimism,
        ] {
            let metric = chain.default_metric_factories();
            let fluid_list = chain.default_fluid_factories();
            let joe = chain.default_trader_joe_factories();
            let solidly = chain.default_solidly_factories();
            let camelot = chain.default_camelot_factories();
            let lists: [&[&str]; 7] = [
                chain.default_uniswap_v2_factories(),
                chain.default_uniswap_v3_factories(),
                solidly.as_slice(),
                camelot.as_slice(),
                joe.as_slice(),
                chain.default_curve_factories(),
                metric.as_slice(),
            ];
            for list in lists {
                for f in list {
                    f.parse::<Address>()
                        .unwrap_or_else(|e| panic!("{chain:?} factory {f}: {e}"));
                }
            }
            for f in fluid_list.as_slice() {
                f.parse::<Address>()
                    .unwrap_or_else(|e| panic!("{chain:?} fluid {f}: {e}"));
            }
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
            partial.pool_discovery_start_block, poly_default.pool_discovery_start_block,
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
