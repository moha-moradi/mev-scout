//! Configuration file parsing, types, and defaults for chains, strategies, and runtime parameters.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::types::{
    ChainName, FlashLoanProvider, RangeMode, Strategy,
};

/// Per-chain runtime parameters loaded from the configuration file.
///
/// Contains contract addresses and discovery parameters specific to each chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainConfig {
    /// EVM chain ID (e.g. 137 = Polygon, 1 = Ethereum)
    pub chain_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balancer_vault: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aave_v3_pool: Option<String>,
    /// Uniswap V3 factory addresses for on-chain pool discovery
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uniswap_v3_factories: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pools_registry_path: Option<String>,
    /// Uniswap V2 factory addresses for on-chain pool discovery
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uniswap_v2_factories: Option<Vec<String>>,
    /// Block number to start pool discovery scan from (default: genesis)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool_discovery_start_block: Option<u64>,
    /// Batch size (blocks) for each eth_getLogs request during pool discovery (default: 10)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool_discovery_batch_size: Option<u64>,
    /// Address of the chain's wrapped native token (e.g., WMATIC on Polygon, WETH on Ethereum)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrapped_native_token: Option<String>,
}

/// Top-level runtime configuration for MEV backtest runs.
///
/// Loaded from TOML files, with CLI overrides merged at startup.
/// Controls chain selection, RPC connectivity, strategy filters, gas model,
/// output format, caching, and per-chain contract addresses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Target EVM chain name (e.g. "polygon", "ethereum")
    #[serde(default = "default_chain")]
    pub chain: String,
    /// Custom RPC endpoint; falls back to publicnode if unset
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpc_url: Option<String>,
    /// Flash loan provider: "auto", "balancer", "aave", or "uniswap"
    #[serde(default = "default_flash_loan_provider")]
    pub flash_loan_provider: String,
    /// Comma-separated strategy filter (e.g. "two_hop_arb,jit,sandwich")
    #[serde(default = "default_strategies")]
    pub strategies: String,
    /// Gas cost model: "historical_exact" or "fixed"
    #[serde(default = "default_gas_model")]
    pub gas_model: String,
    /// Gas limit used for arb tx cost estimation
    #[serde(default = "default_gas_limit")]
    pub gas_limit: u64,
    /// Priority fee premium in gwei (added on top of base fee)
    #[serde(default = "default_priority_fee_gwei")]
    pub priority_fee_gwei: f64,
    /// Output format: "table", "json", or "csv"
    #[serde(default = "default_output_format")]
    pub output: String,
    /// Directory for result exports
    #[serde(default = "default_export_path")]
    pub export_path: String,
    /// Directory for on-disk block/tx cache
    #[serde(default = "default_cache_dir")]
    pub cache_dir: String,
    /// Block range (not serialized to TOML directly, handled via CLI merge)
    #[serde(skip)]
    pub days: Option<u64>,
    #[serde(skip)]
    pub blocks: Option<u64>,
    #[serde(skip)]
    pub block: Option<u64>,
    #[serde(skip)]
    pub from_block: Option<u64>,
    #[serde(skip)]
    pub to_block: Option<u64>,
    /// Per-chain configuration overrides keyed by chain name
    #[serde(default)]
    pub chains: HashMap<String, ChainConfig>,
    /// Path to the loaded config file, if any
    #[serde(skip)]
    pub config_path: Option<PathBuf>,
    /// CoinGecko API key for USD price lookups. Optional — free tier works but is rate-limited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coingecko_api_key: Option<String>,
    /// Optional per-strategy gas limit overrides.
    /// Keys are strategy names like "two_hop_arb", "sandwich", etc.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub gas_limits: std::collections::HashMap<String, u64>,
    /// Maximum number of pool pairs per token for two-hop arbitrage search.
    /// Higher values increase detection coverage but slow down pair computation.
    #[serde(default = "default_max_pairs_per_token")]
    pub max_pairs_per_token: usize,
    /// Number of concurrent RPC workers (default: 1).
    /// Keep low (1-3) for public RPCs. Increase (10-20) for private RPCs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpc_workers: Option<usize>,
}

fn default_chain() -> String {
    "polygon".to_string()
}

fn default_flash_loan_provider() -> String {
    "auto".to_string()
}

fn default_strategies() -> String {
    "all".to_string()
}

fn default_gas_model() -> String {
    "historical_exact".to_string()
}

fn default_gas_limit() -> u64 {
    200_000
}

fn default_priority_fee_gwei() -> f64 {
    0.0
}

fn default_output_format() -> String {
    "table".to_string()
}

fn default_export_path() -> String {
    "./results".to_string()
}

fn default_cache_dir() -> String {
    "./cache".to_string()
}

fn default_max_pairs_per_token() -> usize {
    50
}

impl Default for Config {
    fn default() -> Self {
        Config {
            chain: default_chain(),
            rpc_url: None,
            flash_loan_provider: default_flash_loan_provider(),
            strategies: default_strategies(),
            gas_model: default_gas_model(),
            gas_limit: default_gas_limit(),
            priority_fee_gwei: default_priority_fee_gwei(),
            output: default_output_format(),
            export_path: default_export_path(),
            cache_dir: default_cache_dir(),
            days: None,
            blocks: None,
            block: None,
            from_block: None,
            to_block: None,
            chains: default_chains(),
            config_path: None,
            coingecko_api_key: None,
            gas_limits: std::collections::HashMap::new(),
            max_pairs_per_token: default_max_pairs_per_token(),
            rpc_workers: None,
        }
    }
}

fn default_chains() -> HashMap<String, ChainConfig> {
    let mut m = HashMap::new();
    let polygon_factories = vec![
        "0x5757371414417b8C6CAad45bAeF941aBc7d3Ab32".to_string(), // QuickSwap
        "0xc35DADB65012eC5796536bD9864eD8773aBc74C4".to_string(), // SushiSwap
        "0xCf083Be4164828f00cAE704EC15a36D711491284".to_string(), // ApeSwap
        "0xE7Fb3e833eFE5F9c441105EB65Ef8b261266423B".to_string(), // DFYN
        "0x9f3044f7f9fc8bc9ed615d54845b4577b833282d".to_string(), // Meshswap
    ];
    m.insert(
        "polygon".to_string(),
        ChainConfig {
            chain_id: 137,
            balancer_vault: Some("0xBA12222222228d8Ba445958a75a0704d566BF2C8".to_string()),
            aave_v3_pool: Some("0x794a61358D6845594F94dc1DB02A252b5b4814aD".to_string()),
            uniswap_v3_factories: Some(vec![
                "0x1F98431c8aD98523631AE4a59f267346ea31F984".to_string(), // Uniswap V3
                "0x08958a3a1324f4870eb0028f1e93b2e3d8d78e09".to_string(), // QuickSwap V3
            ]),
            pools_registry_path: None,
            uniswap_v2_factories: Some(polygon_factories),
            pool_discovery_start_block: Some(0),
            pool_discovery_batch_size: None,
            wrapped_native_token: Some("0x0d500b1d8e8ef31e21c99d1db9a6444d3adf1270".to_string()),
        },
    );
    let avalanche_factories = vec![
        "0x9e5A52f57b3038F1B8EeE45F28b3C1960e1fC6b".to_string(), // SushiSwap
        "0x9Ad6C38BE94206cA50bb0d90783181662f0Cfa10".to_string(), // Trader Joe V1
    ];
    m.insert(
        "avalanche".to_string(),
        ChainConfig {
            chain_id: 43114,
            balancer_vault: Some("0xBA12222222228d8Ba445958a75a0704d566BF2C8".to_string()),
            aave_v3_pool: Some("0x69FA688f1Dc47d4B5d8029D5a35FB7a548E0B9b0".to_string()),
            uniswap_v3_factories: Some(vec![
                "0x740b1c1de25031C31FF4fC9A62f554A55cdC1baD".to_string(), // Uniswap V3
            ]),
            pools_registry_path: None,
            uniswap_v2_factories: Some(avalanche_factories),
            pool_discovery_start_block: Some(0),
            pool_discovery_batch_size: None,
            wrapped_native_token: Some("0xB31f66AA3C1e785363F0875A1B74E27b85FD66c7".to_string()),
        },
    );
    let bsc_factories = vec![
        "0xcA143Ce32Fe78f1f7019d7d551a6402fC5350c73".to_string(), // PancakeSwap V2
        "0x9e5A52f57b3038F1B8EeE45F28b3C1960e1fC6b".to_string(), // SushiSwap
    ];
    m.insert(
        "bsc".to_string(),
        ChainConfig {
            chain_id: 56,
            balancer_vault: Some("0xBA12222222228d8Ba445958a75a0704d566BF2C8".to_string()),
            aave_v3_pool: Some("0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2".to_string()),
            uniswap_v3_factories: Some(vec![
                "0xdB1d10011AD0Ff90774D0C6Bb92e5C5c8b4461F7".to_string(), // Uniswap V3
            ]),
            pools_registry_path: None,
            uniswap_v2_factories: Some(bsc_factories),
            pool_discovery_start_block: Some(0),
            pool_discovery_batch_size: None,
            wrapped_native_token: Some("0xbb4CdB9CBd36B01bD1cBaEBF2De08d9173bc095c".to_string()),
        },
    );
    let arbitrum_factories = vec![
        "0x6EcCab422D763aC031210895C81787E87B43A652".to_string(), // Camelot V2
    ];
    m.insert(
        "arbitrum".to_string(),
        ChainConfig {
            chain_id: 42161,
            balancer_vault: Some("0xBA12222222228d8Ba445958a75a0704d566BF2C8".to_string()),
            aave_v3_pool: Some("0x794a61358D6845594F94dc1DB02A252b5b4814aD".to_string()),
            uniswap_v3_factories: Some(vec![
                "0x1F98431c8aD98523631AE4a59f267346ea31F984".to_string(), // Uniswap V3
            ]),
            pools_registry_path: None,
            uniswap_v2_factories: Some(arbitrum_factories),
            pool_discovery_start_block: Some(0),
            pool_discovery_batch_size: None,
            wrapped_native_token: Some("0x82aF49447D8a07e3bd95BD0d56f35241523fBab1".to_string()),
        },
    );
    let base_factories = vec![
        "0x8909Dc15e40173Ff4699343b6eB8132c0eE88a14".to_string(), // Aerodrome
    ];
    m.insert(
        "base".to_string(),
        ChainConfig {
            chain_id: 8453,
            balancer_vault: Some("0xBA12222222228d8Ba445958a75a0704d566BF2C8".to_string()),
            aave_v3_pool: Some("0xA238Dd80C259a72e81d7e4664a9801593F98d1c5".to_string()),
            uniswap_v3_factories: Some(vec![
                "0x33128a8fC17869897dcE68Ed026d694621f6FDfD".to_string(), // Uniswap V3
            ]),
            pools_registry_path: None,
            uniswap_v2_factories: Some(base_factories),
            pool_discovery_start_block: Some(0),
            pool_discovery_batch_size: None,
            wrapped_native_token: Some("0x4200000000000000000000000000000000000006".to_string()),
        },
    );
    let ethereum_factories = vec![
        "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f".to_string(), // Uniswap V2
        "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac".to_string(), // SushiSwap
        "0xB3e281E8c6c888A5BcBf1108E4aC13dA3F5B1c9".to_string(), // ShibaSwap
    ];
    m.insert(
        "ethereum".to_string(),
        ChainConfig {
            chain_id: 1,
            balancer_vault: Some("0xBA12222222228d8Ba445958a75a0704d566BF2C8".to_string()),
            aave_v3_pool: Some("0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2".to_string()),
            uniswap_v3_factories: Some(vec![
                "0x1F98431c8aD98523631AE4a59f267346ea31F984".to_string(), // Uniswap V3
            ]),
            pools_registry_path: None,
            uniswap_v2_factories: Some(ethereum_factories),
            pool_discovery_start_block: Some(0),
            pool_discovery_batch_size: None,
            wrapped_native_token: Some("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".to_string()),
        },
    );
    let optimism_factories = vec![
        "0x9e5A52f57b3038F1B8EeE45F28b3C1960e1fC6b".to_string(), // SushiSwap
    ];
    m.insert(
        "optimism".to_string(),
        ChainConfig {
            chain_id: 10,
            balancer_vault: Some("0xBA12222222228d8Ba445958a75a0704d566BF2C8".to_string()),
            aave_v3_pool: Some("0x794a61358D6845594F94dc1DB02A252b5b4814aD".to_string()),
            uniswap_v3_factories: Some(vec![
                "0x1F98431c8aD98523631AE4a59f267346ea31F984".to_string(), // Uniswap V3
            ]),
            pools_registry_path: None,
            uniswap_v2_factories: Some(optimism_factories),
            pool_discovery_start_block: Some(0),
            pool_discovery_batch_size: None,
            wrapped_native_token: Some("0x4200000000000000000000000000000000000006".to_string()),
        },
    );
    m
}

impl Config {
    /// Parse a TOML configuration file from disk.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or parsed as valid TOML.
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("Failed to read config file '{}': {}", path, e))?;
        let mut cfg: Config = toml::from_str(&content)
            .map_err(|e| anyhow::anyhow!("Failed to parse config file '{}': {}", path, e))?;
        cfg.config_path = Some(PathBuf::from(path));
        Ok(cfg)
    }

    /// Load a config file, falling back to defaults if the file is missing or invalid.
    pub fn load_or_default(path: &str) -> Self {
        let mut cfg = Self::load(path).unwrap_or_default();
        cfg.config_path = Some(PathBuf::from(path));
        cfg
    }

    /// Return a Config pre-populated with all 7 default chain configurations.
    pub fn default_with_chains() -> Self {
        Config::default()
    }

    /// Resolved RPC URL: user-provided value, or the public fallback for the target chain.    
    pub fn effective_rpc_url(&self, chain: ChainName) -> String {
        self.rpc_url
            .clone()
            .unwrap_or_else(|| chain.public_rpc_url().to_string())
    }

    /// Resolved RPC URL list: user-provided override first, then built-in fallbacks.
    /// When the user provides an RPC URL, it is tried first; the built-in list serves as fallback.
    pub fn effective_rpc_urls(&self, chain: ChainName) -> Vec<String> {
        let built_in: Vec<String> = chain
            .public_rpc_urls()
            .iter()
            .map(|s| s.to_string())
            .collect();
        match &self.rpc_url {
            Some(custom) => {
                let mut urls = vec![custom.clone()];
                urls.extend(built_in);
                urls.dedup();
                urls
            }
            None => built_in,
        }
    }

    pub fn to_toml_string(&self) -> anyhow::Result<String> {
        let value = toml::Value::try_from(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize config: {}", e))?;
        toml::to_string(&value)
            .map_err(|e| anyhow::anyhow!("Failed to serialize config: {}", e))
    }

    pub fn plan_summary(
        &self,
        chain_name: ChainName,
        chain_cfg: &ChainConfig,
        range_mode: &RangeMode,
        strategies: &[Strategy],
        provider: FlashLoanProvider,
    ) -> String {
        let provider_desc = match provider {
            FlashLoanProvider::Auto => "auto (Balancer V2 → Aave V3 → Uniswap Flash Swap)".to_string(),
            other => format!("forced ({other})"),
        };

        let strat_list = strategies
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");

        format!(
            r#"Chain:           {} (chain ID {})
RPC:             {}
Block range:     {} → {}
Strategies:      {}
Flash loan:      {}
Gas model:       {}
Cache dir:       {}
"#,
            chain_name,
            chain_cfg.chain_id,
            self.effective_rpc_url(chain_name),
            range_mode,
            range_mode.resolve_description(),
            strat_list,
            provider_desc,
            self.gas_model,
            self.cache_dir,
        )
    }
}

#[derive(Debug, Clone)]
pub struct CliOverrides {
    pub days: Option<u64>,
    pub blocks: Option<u64>,
    pub block: Option<u64>,
    pub from_block: Option<u64>,
    pub to_block: Option<u64>,
    pub chain: Option<String>,
    pub rpc_url: Option<String>,
    pub rpc_workers: Option<usize>,
    pub flash_loan_provider: Option<String>,
    pub strategies: Option<String>,
    pub gas_model: Option<String>,
    pub gas_limit: Option<u64>,
    pub priority_fee_gwei: Option<f64>,
    pub output: Option<String>,
    pub export_path: Option<String>,
    pub cache_dir: Option<String>,
    pub coingecko_api_key: Option<String>,
}

impl Config {
    pub fn merge_cli(&mut self, overrides: &CliOverrides) {
        if let Some(v) = &overrides.days {
            self.days = Some(*v);
        }
        if let Some(v) = &overrides.blocks {
            self.blocks = Some(*v);
        }
        if let Some(v) = &overrides.block {
            self.block = Some(*v);
        }
        if let Some(v) = &overrides.from_block {
            self.from_block = Some(*v);
        }
        if let Some(v) = &overrides.to_block {
            self.to_block = Some(*v);
        }
        if let Some(v) = &overrides.chain {
            self.chain = v.clone();
        }
        if let Some(v) = &overrides.rpc_url {
            self.rpc_url = Some(v.clone());
        }
        if let Some(v) = &overrides.flash_loan_provider {
            self.flash_loan_provider = v.clone();
        }
        if let Some(v) = &overrides.strategies {
            self.strategies = v.clone();
        }
        if let Some(v) = &overrides.gas_model {
            self.gas_model = v.clone();
        }
        if let Some(v) = overrides.gas_limit {
            self.gas_limit = v;
        }
        if let Some(v) = overrides.priority_fee_gwei {
            self.priority_fee_gwei = v;
        }
        if let Some(v) = &overrides.output {
            self.output = v.clone();
        }
        if let Some(v) = &overrides.export_path {
            self.export_path = v.clone();
        }
        if let Some(v) = &overrides.cache_dir {
            self.cache_dir = v.clone();
        }
        if let Some(v) = &overrides.coingecko_api_key {
            self.coingecko_api_key = Some(v.clone());
        }
        if let Some(v) = overrides.rpc_workers {
            self.rpc_workers = Some(v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{RangeMode, Strategy, FlashLoanProvider, ChainName};

    #[test]
    fn test_default_chain_is_polygon() {
        let cfg = Config::default();
        assert_eq!(cfg.chain, "polygon");
    }

    #[test]
    fn test_default_has_seven_chains() {
        let cfg = Config::default();
        assert!(cfg.chains.contains_key("polygon"));
        assert!(cfg.chains.contains_key("ethereum"));
        assert!(cfg.chains.contains_key("bsc"));
        assert!(cfg.chains.contains_key("arbitrum"));
        assert!(cfg.chains.contains_key("avalanche"));
        assert!(cfg.chains.contains_key("base"));
        assert!(cfg.chains.contains_key("optimism"));
        assert_eq!(cfg.chains.len(), 7);
    }

    #[test]
    fn test_effective_rpc_url_uses_override() {
        let cfg = Config {
            rpc_url: Some("https://my-rpc.example.com".into()),
            ..Config::default()
        };
        assert_eq!(cfg.effective_rpc_url(ChainName::Polygon), "https://my-rpc.example.com");
    }

    #[test]
    fn test_effective_rpc_url_falls_back_to_public() {
        let cfg = Config::default();
        assert!(cfg.effective_rpc_url(ChainName::Polygon).contains("publicnode.com"));
    }

    #[test]
    fn test_load_or_default_missing_file() {
        let cfg = Config::load_or_default("/nonexistent/path/mev-scout.toml");
        assert_eq!(cfg.chain, "polygon");
    }

    #[test]
    fn test_load_valid_toml() {
        let dir = std::env::temp_dir();
        let path = dir.join("test_mev_config_valid.toml");
        std::fs::write(&path, r#"chain = "ethereum"
rpc_url = "https://eth.diy"
"#).unwrap();
        let cfg = Config::load(path.to_str().unwrap()).unwrap();
        assert_eq!(cfg.chain, "ethereum");
        assert_eq!(cfg.rpc_url.unwrap(), "https://eth.diy");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_load_invalid_toml_errors() {
        let dir = std::env::temp_dir();
        let path = dir.join("test_mev_config_invalid.toml");
        std::fs::write(&path, "not [[ valid toml [[[").unwrap();
        assert!(Config::load(path.to_str().unwrap()).is_err());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_config_to_toml_roundtrip() {
        let cfg = Config::default();
        let toml_str = cfg.to_toml_string().unwrap();
        assert!(toml_str.contains("chain"));
        assert!(toml_str.contains("polygon"));
    }

    #[test]
    fn test_merge_cli_full_override() {
        let overrides = CliOverrides {
            days: Some(14), blocks: None, block: None,
            from_block: None, to_block: None,
            chain: Some("ethereum".into()),
            rpc_url: Some("https://custom".into()),
            rpc_workers: None,
            flash_loan_provider: Some("aave".into()),
            strategies: Some("two_hop_arb".into()),
            gas_model: Some("fixed".into()),
            gas_limit: Some(300_000),
            priority_fee_gwei: Some(2.5),
            output: Some("json".into()),
            export_path: Some("./out".into()),
            cache_dir: Some("./db".into()),
            coingecko_api_key: Some("test-key".into()),
        };
        let mut cfg = Config::default();
        cfg.merge_cli(&overrides);
        assert_eq!(cfg.days, Some(14));
        assert_eq!(cfg.chain, "ethereum");
        assert_eq!(cfg.rpc_url.unwrap(), "https://custom");
        assert_eq!(cfg.flash_loan_provider, "aave");
        assert_eq!(cfg.strategies, "two_hop_arb");
        assert_eq!(cfg.gas_model, "fixed");
        assert_eq!(cfg.gas_limit, 300_000);
        assert_eq!(cfg.priority_fee_gwei, 2.5);
        assert_eq!(cfg.output, "json");
        assert_eq!(cfg.export_path, "./out");
        assert_eq!(cfg.cache_dir, "./db");
        assert_eq!(cfg.coingecko_api_key, Some("test-key".into()));
    }

    #[test]
    fn test_merge_cli_partial_override() {
        let mut cfg = Config::default();
        let overrides = CliOverrides {
            days: Some(7),
            blocks: None, block: None, from_block: None, to_block: None,
            chain: None, rpc_url: None, rpc_workers: None,
            flash_loan_provider: None, strategies: None,
            gas_model: None, gas_limit: None, priority_fee_gwei: None,
            output: None, export_path: None, cache_dir: None,
            coingecko_api_key: None,
        };
        cfg.merge_cli(&overrides);
        assert_eq!(cfg.days, Some(7));
        assert_eq!(cfg.chain, "polygon");
    }

    #[test]
    fn test_plan_summary_contains_all_sections() {
        let cfg = Config::default();
        let chain_cfg = cfg.chains.get("polygon").unwrap();
        let range = RangeMode::Single(50000000);
        let strategies = vec![Strategy::TwoHopArb];
        let summary = cfg.plan_summary(
            ChainName::Polygon, chain_cfg, &range, &strategies,
            FlashLoanProvider::Auto,
        );
        assert!(summary.contains("Chain:"));
        assert!(summary.contains("polygon"));
        assert!(summary.contains("RPC:"));
        assert!(summary.contains("single block #50000000"));
        assert!(summary.contains("two_hop_arb"));
        assert!(summary.contains("Flash loan:"));
        assert!(summary.contains("auto (Balancer"));
    }
}
