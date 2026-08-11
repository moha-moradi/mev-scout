//! Configuration file parsing, types, and defaults for chains, strategies, and runtime parameters.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::defaults::{ChainConfig, default_chains};
use crate::error;

use crate::types::{
    ChainName, FlashLoanProvider, RangeMode, Strategy,
};

// ── Sub-config structs ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcConfig {
    /// Custom RPC endpoint; falls back to publicnode if unset
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpc_url: Option<String>,
    /// Additional RPC URLs for multi-provider load distribution
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rpc_urls: Vec<String>,
    /// Per-provider RPS limits
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rpc_rps: Vec<f64>,
    /// RPC rate limit in requests per second (default: 500). 0 = unlimited.
    #[serde(default = "default_rps_limit")]
    pub rps_limit: f64,
    /// Depth (in blocks) at which the archive-support probe runs (default: 10_000).
    /// Lower it for endpoints with limited historical state retention — e.g.
    /// Polygon full nodes keep only ~128 blocks of state. Note: replay/run then
    /// only work for blocks within this depth of the chain tip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_probe_depth_blocks: Option<u64>,
    /// Block-level concurrency within each provider shard
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_concurrency: Option<usize>,
    /// CoinGecko API key for USD price lookups
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coingecko_api_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GasConfig {
    /// Gas cost model: "historical_exact" or "fixed"
    #[serde(default = "default_gas_model")]
    pub gas_model: String,
    /// Gas limit used for arb tx cost estimation
    #[serde(default = "default_gas_limit")]
    pub gas_limit: u64,
    /// Priority fee premium in gwei (added on top of base fee)
    #[serde(default = "default_priority_fee_gwei")]
    pub priority_fee_gwei: f64,
    /// Optional per-strategy gas limit overrides
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub gas_limits: HashMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestConfig {
    /// Flash loan provider: "auto", "balancer", "aave", or "uniswap"
    #[serde(default = "default_flash_loan_provider")]
    pub flash_loan_provider: String,
    /// Comma-separated strategy filter (e.g. "two_hop_arb,jit,sandwich")
    #[serde(default = "default_strategies")]
    pub strategies: String,
    /// Maximum number of pool pairs per token for two-hop arbitrage search
    #[serde(default = "default_max_pairs_per_token")]
    pub max_pairs_per_token: usize,
    /// Proximity window (in tx indices) for JitArb detection (default: 3)
    #[serde(default = "default_proximity_window")]
    pub proximity_window: usize,
    /// Capture pending transactions from the mempool during backtest
    #[serde(default)]
    pub capture_pending: bool,
    /// Price oracle mode: "coingecko", "onchain", or "hybrid"
    #[serde(default)]
    pub price_oracle_mode: String,
    /// Per-token USD prices: comma-separated "ADDR=price" pairs
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_prices: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputConfig {
    /// Output format: "table", "json", or "csv"
    #[serde(default = "default_output_format")]
    pub output: String,
    /// Directory for result exports
    #[serde(default = "default_export_path")]
    pub export_path: String,
    /// Directory for SQLite database file
    #[serde(default = "default_db_path")]
    pub db_path: String,
    /// Directory for Parquet intermediate files (optional)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parquet_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuneConfig {
    /// Dune Analytics API key
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dune_api_key: Option<String>,
    /// When true, use Dune pool discovery as the primary source
    #[serde(default)]
    pub dune_primary_pool_discovery: bool,
}

// ── Default helpers ─────────────────────────────────────────────────

fn default_rps_limit() -> f64 { 0.0 }
fn default_chain() -> String { "polygon".to_string() }
fn default_flash_loan_provider() -> String { "auto".to_string() }
fn default_strategies() -> String { "all".to_string() }
fn default_gas_model() -> String { "historical_exact".to_string() }
fn default_gas_limit() -> u64 { 200_000 }
fn default_priority_fee_gwei() -> f64 { 0.0 }
fn default_output_format() -> String { "table".to_string() }
fn default_export_path() -> String { "./results".to_string() }
fn default_db_path() -> String { String::new() }
fn default_max_pairs_per_token() -> usize { 50 }
fn default_proximity_window() -> usize { 3 }

// ── Default impls for sub-structs ───────────────────────────────────

impl Default for RpcConfig {
    fn default() -> Self {
        RpcConfig {
            rpc_url: None,
            rpc_urls: Vec::new(),
            rpc_rps: Vec::new(),
            rps_limit: default_rps_limit(),
            block_concurrency: None,
            coingecko_api_key: None,
            archive_probe_depth_blocks: None,
        }
    }
}

impl Default for GasConfig {
    fn default() -> Self {
        GasConfig {
            gas_model: default_gas_model(),
            gas_limit: default_gas_limit(),
            priority_fee_gwei: default_priority_fee_gwei(),
            gas_limits: HashMap::new(),
        }
    }
}

impl Default for BacktestConfig {
    fn default() -> Self {
        BacktestConfig {
            flash_loan_provider: default_flash_loan_provider(),
            strategies: default_strategies(),
            max_pairs_per_token: default_max_pairs_per_token(),
            proximity_window: default_proximity_window(),
            capture_pending: false,
            price_oracle_mode: "coingecko".to_string(),
            token_prices: None,
        }
    }
}

impl Default for OutputConfig {
    fn default() -> Self {
        OutputConfig {
            output: default_output_format(),
            export_path: default_export_path(),
            db_path: default_db_path(),
            parquet_dir: None,
        }
    }
}

impl Default for DuneConfig {
    fn default() -> Self {
        DuneConfig {
            dune_api_key: None,
            dune_primary_pool_discovery: false,
        }
    }
}

// ── Top-level Config ────────────────────────────────────────────────

/// Top-level runtime configuration for MEV backtest runs.
///
/// Loaded from TOML files, with CLI overrides merged at startup.
/// Uses `#[serde(flatten)]` on sub-configs so existing flat TOML files
/// continue to work without changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Target EVM chain name (e.g. "polygon", "ethereum")
    #[serde(default = "default_chain")]
    pub chain: String,
    /// Per-chain configuration overrides keyed by chain name
    #[serde(default)]
    pub chains: HashMap<String, ChainConfig>,
    /// Path to the loaded config file, if any
    #[serde(skip)]
    pub config_path: Option<PathBuf>,

    // ── Block range (not serialized to TOML, CLI-only) ──────────────
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

    // ── Sub-configs (flattened for TOML compat) ─────────────────────
    #[serde(flatten)]
    pub rpc: RpcConfig,
    #[serde(flatten)]
    pub gas: GasConfig,
    #[serde(flatten)]
    pub backtest: BacktestConfig,
    #[serde(flatten)]
    pub output: OutputConfig,
    #[serde(flatten)]
    pub dune: DuneConfig,
}

impl Config {
    /// Return the effective database path for the given chain.
    pub fn effective_db_path(&self, chain: &ChainName) -> String {
        if self.output.db_path.is_empty() {
            format!("./cache/{}-mev-scout.sqlite", chain)
        } else {
            self.output.db_path.clone()
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            chain: default_chain(),
            chains: default_chains(),
            config_path: None,
            days: None,
            blocks: None,
            block: None,
            from_block: None,
            to_block: None,
            rpc: RpcConfig::default(),
            gas: GasConfig::default(),
            backtest: BacktestConfig::default(),
            output: OutputConfig::default(),
            dune: DuneConfig::default(),
        }
    }
}

impl Config {
    /// Parse a TOML configuration file from disk.
    pub fn load(path: &str) -> error::Result<Self> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| error::Error::Other(format!("Failed to read config file '{}': {}", path, e)))?;
        let mut cfg: Config = toml::from_str(&content)
            .map_err(|e| error::Error::Other(format!("Failed to parse config file '{}': {}", path, e)))?;
        cfg.config_path = Some(PathBuf::from(path));
        Ok(cfg)
    }

    /// Load a config file, falling back to defaults if the file is missing or invalid.
    pub fn load_or_default(path: &str) -> Self {
        let mut cfg = Self::load(path).unwrap_or_default();
        cfg.config_path = Some(PathBuf::from(path));
        let defaults = default_chains();
        for (name, default_cfg) in defaults {
            cfg.chains.entry(name).or_insert(default_cfg);
        }
        cfg
    }

    /// Resolved RPC URL list: user override(s) first, then public fallbacks for known chains.
    pub fn effective_rpc_urls(&self) -> error::Result<Vec<String>> {
        let urls = Self::merge_rpc_urls(&self.rpc.rpc_urls, &self.rpc.rpc_url);
        if urls.is_empty() {
            return Err(error::Error::Other(
                "No RPC URL provided. Use --rpc <URL>, --rpc-urls, or set rpc_url in config.".into()
            ));
        }
        Ok(urls)
    }

    /// Build full provider configs by merging user-supplied URLs with public fallbacks.
    pub fn effective_provider_configs(&self, chain_name: ChainName) -> error::Result<Vec<(String, Option<f64>, bool)>> {
        let urls = self.effective_rpc_urls().unwrap_or_default();
        if !urls.is_empty() {
            let public_endpoints = chain_name.public_rpc_endpoints();
            let result: Vec<(String, Option<f64>, bool)> = urls
                .into_iter()
                .enumerate()
                .map(|(i, url)| {
                    let rps = self.rpc.rpc_rps.get(i).copied();
                    if let Some(r) = rps {
                        let archive = public_endpoints
                            .iter()
                            .find(|e| url.contains(e.url) || e.url.contains(&url))
                            .map(|e| e.archive)
                            .unwrap_or(false);
                        return (url, Some(r), archive);
                    }
                    let (default_rps, archive) = public_endpoints
                        .iter()
                        .find(|e| url.contains(e.url) || e.url.contains(&url))
                        .map(|e| (Some(e.default_rps), e.archive))
                        .unwrap_or((Some(self.rpc.rps_limit), false));
                    (url, default_rps, archive)
                })
                .collect();
            Ok(result)
        } else {
            let public = chain_name.public_rpc_endpoints();
            if public.is_empty() {
                return Err(error::Error::Other(
                    "No RPC URL provided and no public endpoints available for this chain. Use --rpc <URL>, --rpc-urls, or set rpc_url in config.".into()
                ));
            }
            Ok(public.into_iter().map(|e| (e.url.to_string(), Some(e.default_rps), e.archive)).collect())
        }
    }

    /// Auto-calculate optimal `block_concurrency` from provider RPS limits.
    pub fn effective_block_concurrency(
        &self,
        provider_configs: &[(String, Option<f64>, bool)],
    ) -> usize {
        if let Some(bc) = self.rpc.block_concurrency {
            tracing::info!("block_concurrency: using explicit value {bc}");
            return bc;
        }

        const MIN_PER_SHARD: usize = 5;
        const MAX_PER_SHARD: usize = 15;
        const DEFAULT_BC: usize = 10;

        let min_rps = provider_configs
            .iter()
            .filter_map(|(_, r, _)| *r)
            .filter(|r| *r > 0.0)
            .fold(f64::INFINITY, f64::min);

        let bc = if min_rps.is_finite() && min_rps > 0.0 {
            let raw = (min_rps * 2.0).ceil() as usize;
            raw.clamp(MIN_PER_SHARD, MAX_PER_SHARD)
        } else {
            DEFAULT_BC
        };

        tracing::info!(
            "block_concurrency: auto-calculated {bc} (min_rps={min_rps:.1}, providers={})",
            provider_configs.len(),
        );
        bc
    }

    /// Return only the user-specified RPC URLs (no public fallbacks).
    pub fn user_rpc_urls(&self) -> error::Result<Vec<String>> {
        let urls = Self::merge_rpc_urls(&self.rpc.rpc_urls, &self.rpc.rpc_url);
        if urls.is_empty() {
            return Err(error::Error::Other(
                "No RPC URL provided. Use --rpc <URL>, --rpc-urls, or set rpc_url in config.".into()
            ));
        }
        Ok(urls)
    }

    /// Merge `rpc_urls` (Vec) and `rpc_url` (legacy single) into a deduplicated list.
    fn merge_rpc_urls(base: &[String], extra: &Option<String>) -> Vec<String> {
        let mut urls = base.to_vec();
        if let Some(single) = extra {
            if !urls.iter().any(|u| u == single) {
                urls.push(single.clone());
            }
        }
        urls
    }

    pub fn to_toml_string(&self) -> error::Result<String> {
        let value = toml::Value::try_from(self)
            .map_err(|e| error::Error::Other(format!("Failed to serialize config: {}", e)))?;
        toml::to_string(&value)
            .map_err(|e| error::Error::Other(format!("Failed to serialize config: {}", e)))
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
            r#"Chain:               {} (chain ID {})
RPC:                 {}
Block range:         {} → {}
Strategies:          {}
Flash loan:          {}
Gas model:           {}
DB path:             {}
Parquet dir:         {}
"#,
            chain_name,
            chain_cfg.chain_id,
            self.rpc.rpc_url.clone().unwrap_or_else(|| "RPC not set".to_string()),
            range_mode,
            range_mode.resolve_description(),
            strat_list,
            provider_desc,
            self.gas.gas_model,
            self.effective_db_path(&chain_name),
            self.output.parquet_dir.as_deref().unwrap_or("(none)"),
        )
    }
}

/// Merge an optional CLI override into a config field.
macro_rules! merge_opt {
    ($cfg:expr, $cli:expr, $field:ident) => {
        if let Some(ref v) = $cli.$field {
            $cfg.$field = v.clone();
        }
    };
    ($cfg:expr, $cli:expr, $field:ident, into_option) => {
        if let Some(ref v) = $cli.$field {
            $cfg.$field = Some(v.clone());
        }
    };
    ($cfg:expr, $cli:expr, $field:ident, copy) => {
        if let Some(v) = $cli.$field {
            $cfg.$field = v;
        }
    };
    ($cfg:expr, $cli:expr, $field:ident, copy_some) => {
        if let Some(v) = $cli.$field {
            $cfg.$field = Some(v);
        }
    };
}

// ── CliOverrides (mirrors Config structure) ─────────────────────────

#[derive(Debug, Clone, Default)]
pub struct RpcOverrides {
    pub rpc_url: Option<String>,
    pub rpc_urls: Option<Vec<String>>,
    pub rpc_rps: Option<Vec<f64>>,
    pub rps_limit: Option<f64>,
    pub block_concurrency: Option<usize>,
    pub coingecko_api_key: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct GasOverrides {
    pub gas_model: Option<String>,
    pub gas_limit: Option<u64>,
    pub priority_fee_gwei: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct BacktestOverrides {
    pub flash_loan_provider: Option<String>,
    pub strategies: Option<String>,
    pub max_pairs_per_token: Option<usize>,
    pub proximity_window: Option<usize>,
    pub capture_pending: Option<bool>,
    pub price_oracle_mode: Option<String>,
    pub token_prices: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct OutputOverrides {
    pub output: Option<String>,
    pub export_path: Option<String>,
    pub db_path: Option<String>,
    pub parquet_dir: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct DuneOverrides {
    pub dune_api_key: Option<String>,
    pub dune_primary_pool_discovery: Option<bool>,
}

#[derive(Debug, Clone, Default)]
pub struct CliOverrides {
    pub days: Option<u64>,
    pub blocks: Option<u64>,
    pub block: Option<u64>,
    pub from_block: Option<u64>,
    pub to_block: Option<u64>,
    pub chain: Option<String>,
    pub rpc: RpcOverrides,
    pub gas: GasOverrides,
    pub backtest: BacktestOverrides,
    pub output: OutputOverrides,
    pub dune: DuneOverrides,
}

macro_rules! merge_sub {
    ($cfg:expr, $cli:expr, $sub:ident, [$(($field:ident $(, $variant:ident)?)),*]) => {
        $(
            merge_opt!($cfg.$sub, $cli.$sub, $field $(, $variant)*);
        )*
    };
}

// ── ConfigBuilder ───────────────────────────────────────────────────

/// Builder for programmatic `Config` construction without TOML files.
///
/// Starts from `Config::default()` and overrides only the fields explicitly
/// set via chaining methods. Replaces ad-hoc struct construction in tests
/// and CLI command adapters.
///
/// # Example
///
/// ```ignore
/// use crate::config::{ConfigBuilder, RpcConfig};
///
/// let config = ConfigBuilder::default()
///     .with_chain("polygon")
///     .with_rpc(RpcConfig {
///         rpc_url: Some("https://my-rpc.example.com".into()),
///         ..RpcConfig::default()
///     })
///     .build();
/// ```
#[derive(Debug, Clone, Default)]
pub struct ConfigBuilder {
    chain: Option<String>,
    days: Option<u64>,
    blocks: Option<u64>,
    block: Option<u64>,
    from_block: Option<u64>,
    to_block: Option<u64>,
    rpc: Option<RpcConfig>,
    gas: Option<GasConfig>,
    backtest: Option<BacktestConfig>,
    output: Option<OutputConfig>,
    dune: Option<DuneConfig>,
}

impl ConfigBuilder {
    /// Set the chain name (e.g. "polygon", "ethereum").
    pub fn with_chain(mut self, chain: impl Into<String>) -> Self { self.chain = Some(chain.into()); self }
    /// Set --days CLI equivalent.
    pub fn with_days(mut self, days: u64) -> Self { self.days = Some(days); self }
    /// Set --blocks CLI equivalent.
    pub fn with_blocks(mut self, blocks: u64) -> Self { self.blocks = Some(blocks); self }
    /// Set --block CLI equivalent.
    pub fn with_block(mut self, block: u64) -> Self { self.block = Some(block); self }
    /// Set --from-block CLI equivalent.
    pub fn with_from_block(mut self, from: u64) -> Self { self.from_block = Some(from); self }
    /// Set --to-block CLI equivalent.
    pub fn with_to_block(mut self, to: u64) -> Self { self.to_block = Some(to); self }
    /// Replace the RPC sub-config entirely.
    pub fn with_rpc(mut self, rpc: RpcConfig) -> Self { self.rpc = Some(rpc); self }
    /// Replace the gas sub-config entirely.
    pub fn with_gas(mut self, gas: GasConfig) -> Self { self.gas = Some(gas); self }
    /// Replace the backtest sub-config entirely.
    pub fn with_backtest(mut self, backtest: BacktestConfig) -> Self { self.backtest = Some(backtest); self }
    /// Replace the output sub-config entirely.
    pub fn with_output(mut self, output: OutputConfig) -> Self { self.output = Some(output); self }
    /// Replace the Dune sub-config entirely.
    pub fn with_dune(mut self, dune: DuneConfig) -> Self { self.dune = Some(dune); self }

    /// Build a `Config`, starting from defaults and overriding set fields.
    pub fn build(self) -> Config {
        let mut cfg = Config::default();
        if let Some(v) = self.chain { cfg.chain = v; }
        if let Some(v) = self.days { cfg.days = Some(v); }
        if let Some(v) = self.blocks { cfg.blocks = Some(v); }
        if let Some(v) = self.block { cfg.block = Some(v); }
        if let Some(v) = self.from_block { cfg.from_block = Some(v); }
        if let Some(v) = self.to_block { cfg.to_block = Some(v); }
        if let Some(v) = self.rpc { cfg.rpc = v; }
        if let Some(v) = self.gas { cfg.gas = v; }
        if let Some(v) = self.backtest { cfg.backtest = v; }
        if let Some(v) = self.output { cfg.output = v; }
        if let Some(v) = self.dune { cfg.dune = v; }
        cfg
    }
}

impl Config {
    pub fn merge_cli(&mut self, overrides: &CliOverrides) {
        merge_opt!(self, overrides, days, copy_some);
        merge_opt!(self, overrides, blocks, copy_some);
        merge_opt!(self, overrides, block, copy_some);
        merge_opt!(self, overrides, from_block, copy_some);
        merge_opt!(self, overrides, to_block, copy_some);
        merge_opt!(self, overrides, chain);

        merge_sub!(self, overrides, rpc, [
            (rpc_url, into_option),
            (rpc_urls),
            (rpc_rps),
            (rps_limit, copy),
            (block_concurrency, copy_some),
            (coingecko_api_key, into_option)
        ]);
        merge_sub!(self, overrides, gas, [
            (gas_model),
            (gas_limit, copy),
            (priority_fee_gwei, copy)
        ]);
        merge_sub!(self, overrides, backtest, [
            (flash_loan_provider),
            (strategies),
            (max_pairs_per_token, copy),
            (proximity_window, copy),
            (capture_pending, copy),
            (price_oracle_mode),
            (token_prices, into_option)
        ]);
        merge_sub!(self, overrides, output, [
            (output),
            (export_path),
            (db_path),
            (parquet_dir, into_option)
        ]);
        merge_sub!(self, overrides, dune, [
            (dune_api_key, into_option),
            (dune_primary_pool_discovery, copy)
        ]);
    }

    /// Parse the `--token-price` value into a `HashMap<Address, f64>`.
    pub fn parse_token_prices(&self) -> HashMap<alloy::primitives::Address, f64> {
        let mut map = HashMap::new();
        let Some(s) = &self.backtest.token_prices else { return map };
        for pair in s.split(',') {
            let pair = pair.trim();
            if pair.is_empty() { continue; }
            if let Some((addr_str, price_str)) = pair.split_once('=') {
                match (addr_str.trim().parse::<alloy::primitives::Address>(), price_str.trim().parse::<f64>()) {
                    (Ok(addr), Ok(price)) => { map.insert(addr, price); }
                    (Ok(_), Err(_)) => tracing::warn!("unparseable token price '{}' in '{}'", price_str, pair),
                    (Err(_), _) => tracing::warn!("unparseable token address '{}' in '{}'", addr_str, pair),
                }
            } else {
                tracing::warn!("malformed token-price entry '{}' (expected address=price)", pair);
            }
        }
        map
    }
}
