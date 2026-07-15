use std::collections::HashMap;

use alloy::primitives::Address;
use serde::{Deserialize, Serialize};

use crate::types::ExecutorType;

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
    let mut m = HashMap::new();
    let polygon_factories = vec![
        "0x5757371414417b8C6CAad45bAeF941aBc7d3Ab32".to_string(),
        "0xc35DADB65012eC5796536bD9864eD8773aBc74C4".to_string(),
        "0xCf083Be4164828f00cAE704EC15a36D711491284".to_string(),
        "0xE7Fb3e833eFE5F9c441105EB65Ef8b261266423B".to_string(),
        "0x9f3044f7f9fc8bc9ed615d54845b4577b833282d".to_string(),
    ];
    m.insert(
        "polygon".to_string(),
        ChainConfig {
            chain_id: 137,
            balancer_vault: Some("0xBA12222222228d8Ba445958a75a0704d566BF2C8".to_string()),
            aave_v3_pool: Some("0x794a61358D6845594F94dc1DB02A252b5b4814aD".to_string()),
            uniswap_v3_factories: Some(vec![
                "0x1F98431c8aD98523631AE4a59f267346ea31F984".to_string(),
                "0x08958a3a1324f4870eb0028f1e93b2e3d8d78e09".to_string(),
            ]),
            uniswap_v2_factories: Some(polygon_factories),
            pool_discovery_start_block: Some(49_100_000),
            pool_discovery_batch_size: None,
            wrapped_native_token: Some("0x0d500b1d8e8ef31e21c99d1db9a6444d3adf1270".to_string()),
            uniswap_v2_default_fee: None,
            solidly_factories: None,
            camelot_factories: None,
            curve_registry: None,
            v4_pool_manager: None,
            trader_joe_factory: None,
            pendle_factory: None,
        },
    );
    let avalanche_factories = vec![
        "0x9e5A52f57b3038F1B8EeE45F28b3C1960e1fC6b".to_string(),
        "0x9Ad6C38BE94206cA50bb0d90783181662f0Cfa10".to_string(),
    ];
    m.insert(
        "avalanche".to_string(),
        ChainConfig {
            chain_id: 43114,
            balancer_vault: Some("0xBA12222222228d8Ba445958a75a0704d566BF2C8".to_string()),
            aave_v3_pool: Some("0x69FA688f1Dc47d4B5d8029D5a35FB7a548E0B9b0".to_string()),
            uniswap_v3_factories: Some(vec![
                "0x740b1c1de25031C31FF4fC9A62f554A55cdC1baD".to_string(),
            ]),
            uniswap_v2_factories: Some(avalanche_factories),
            pool_discovery_start_block: Some(4_200_000),
            pool_discovery_batch_size: None,
            wrapped_native_token: Some("0xB31f66AA3C1e785363F0875A1B74E27b85FD66c7".to_string()),
            uniswap_v2_default_fee: None,
            solidly_factories: None,
            camelot_factories: None,
            curve_registry: None,
            v4_pool_manager: None,
            trader_joe_factory: None,
            pendle_factory: None,
        },
    );
    let bsc_factories = vec![
        "0xcA143Ce32Fe78f1f7019d7d551a6402fC5350c73".to_string(),
        "0x9e5A52f57b3038F1B8EeE45F28b3C1960e1fC6b".to_string(),
    ];
    m.insert(
        "bsc".to_string(),
        ChainConfig {
            chain_id: 56,
            balancer_vault: Some("0xBA12222222228d8Ba445958a75a0704d566BF2C8".to_string()),
            aave_v3_pool: Some("0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2".to_string()),
            uniswap_v3_factories: Some(vec![
                "0xdB1d10011AD0Ff90774D0C6Bb92e5C5c8b4461F7".to_string(),
            ]),
            uniswap_v2_factories: Some(bsc_factories),
            pool_discovery_start_block: Some(5_063_800),
            pool_discovery_batch_size: None,
            wrapped_native_token: Some("0xbb4CdB9CBd36B01bD1cBaEBF2De08d9173bc095c".to_string()),
            uniswap_v2_default_fee: Some(25),
            solidly_factories: None,
            camelot_factories: None,
            curve_registry: None,
            v4_pool_manager: None,
            trader_joe_factory: None,
            pendle_factory: None,
        },
    );
    m.insert(
        "arbitrum".to_string(),
        ChainConfig {
            chain_id: 42161,
            balancer_vault: Some("0xBA12222222228d8Ba445958a75a0704d566BF2C8".to_string()),
            aave_v3_pool: Some("0x794a61358D6845594F94dc1DB02A252b5b4814aD".to_string()),
            uniswap_v3_factories: Some(vec![
                "0x1F98431c8aD98523631AE4a59f267346ea31F984".to_string(),
            ]),
            uniswap_v2_factories: None,
            pool_discovery_start_block: Some(172_000),
            pool_discovery_batch_size: None,
            wrapped_native_token: Some("0x82aF49447D8a07e3bd95BD0d56f35241523fBab1".to_string()),
            uniswap_v2_default_fee: None,
            solidly_factories: None,
            camelot_factories: Some(vec![
                "0x6EcCab422D763aC031210895C81787E87B43A652".to_string(),
            ]),
            curve_registry: None,
            v4_pool_manager: None,
            trader_joe_factory: None,
            pendle_factory: None,
        },
    );
    m.insert(
        "base".to_string(),
        ChainConfig {
            chain_id: 8453,
            balancer_vault: Some("0xBA12222222228d8Ba445958a75a0704d566BF2C8".to_string()),
            aave_v3_pool: Some("0xA238Dd80C259a72e81d7e4664a9801593F98d1c5".to_string()),
            uniswap_v3_factories: Some(vec![
                "0x33128a8fC17869897dcE68Ed026d694621f6FDfD".to_string(),
            ]),
            uniswap_v2_factories: None,
            pool_discovery_start_block: Some(96_000),
            pool_discovery_batch_size: None,
            wrapped_native_token: Some("0x4200000000000000000000000000000000000006".to_string()),
            uniswap_v2_default_fee: None,
            solidly_factories: Some(vec![
                "0x8909Dc15e40173Ff4699343b6eB8132c0eE88a14".to_string(),
            ]),
            camelot_factories: None,
            curve_registry: None,
            v4_pool_manager: None,
            trader_joe_factory: None,
            pendle_factory: None,
        },
    );
    let ethereum_factories = vec![
        "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f".to_string(),
        "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac".to_string(),
        "0xB3e281E8c6c888A5BcBf1108E4aC13dA3F5B1c9".to_string(),
    ];
    m.insert(
        "ethereum".to_string(),
        ChainConfig {
            chain_id: 1,
            balancer_vault: Some("0xBA12222222228d8Ba445958a75a0704d566BF2C8".to_string()),
            aave_v3_pool: Some("0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2".to_string()),
            uniswap_v3_factories: Some(vec![
                "0x1F98431c8aD98523631AE4a59f267346ea31F984".to_string(),
            ]),
            uniswap_v2_factories: Some(ethereum_factories),
            pool_discovery_start_block: Some(10_008_335),
            pool_discovery_batch_size: None,
            wrapped_native_token: Some("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".to_string()),
            uniswap_v2_default_fee: None,
            solidly_factories: None,
            camelot_factories: None,
            curve_registry: Some("0x90E00ACe148ca3b23Ac1bC8C240C2a7Dd9c2d7f5".to_string()),
            v4_pool_manager: None,
            trader_joe_factory: None,
            pendle_factory: None,
        },
    );
    m.insert(
        "optimism".to_string(),
        ChainConfig {
            chain_id: 10,
            balancer_vault: Some("0xBA12222222228d8Ba445958a75a0704d566BF2C8".to_string()),
            aave_v3_pool: Some("0x794a61358D6845594F94dc1DB02A252b5b4814aD".to_string()),
            uniswap_v3_factories: Some(vec![
                "0x1F98431c8aD98523631AE4a59f267346ea31F984".to_string(),
            ]),
            uniswap_v2_factories: Some(vec![
                "0x9e5A52f57b3038F1B8EeE45F28b3C1960e1fC6b".to_string(), // SushiSwap
            ]),
            pool_discovery_start_block: Some(10_827_000),
            pool_discovery_batch_size: None,
            wrapped_native_token: Some("0x4200000000000000000000000000000000000006".to_string()),
            uniswap_v2_default_fee: None,
            solidly_factories: Some(vec![
                "0x420DD381b31aEf6683db6B902084cB0FFECe40Da".to_string(), // Velodrome V2
            ]),
            camelot_factories: None,
            curve_registry: None,
            v4_pool_manager: None,
            trader_joe_factory: None,
            pendle_factory: None,
        },
    );
    m
}

/// Returns default executor addresses per chain.
/// These are placeholder addresses populated after initial Foundry deployment.
pub fn default_executor_addresses() -> HashMap<String, HashMap<ExecutorType, Address>> {
    let mut map = HashMap::new();
    for (name, _) in default_chains() {
        map.insert(name, HashMap::new());
    }
    map
}



