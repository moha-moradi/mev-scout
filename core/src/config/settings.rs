//! Configuration file parsing, types, and defaults for chains, strategies, and runtime parameters.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::defaults::{ChainConfig, default_chains};
use crate::error;

use crate::types::{
    ChainName, FlashLoanProvider, RangeMode, Strategy,
};

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
    /// Additional RPC URLs for multi-provider load distribution (comma-separated in CLI).
    /// When set alongside `rpc_url`, all URLs are used for load distribution.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rpc_urls: Vec<String>,
    /// Per-provider RPS limits, one per entry in the combined `effective_rpc_urls` list.
    /// Empty = use default RPS from `ProviderEndpoint` metadata.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rpc_rps: Vec<f64>,
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
    /// Directory for SQLite database file
    #[serde(default = "default_db_path")]
    pub db_path: String,

    /// Directory for Parquet intermediate files (optional, unset = no Parquet)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parquet_dir: Option<String>,
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
    /// Block-level concurrency within each provider shard (default: 100 via CLI).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_concurrency: Option<usize>,
    /// RPC rate limit in requests per second (default: 500). 0 = unlimited.
    #[serde(default = "default_rps_limit")]
    pub rps_limit: f64,
    /// Price oracle mode: "coingecko", "onchain", or "hybrid" (default: "coingecko").
    #[serde(default)]
    pub price_oracle_mode: String,
    /// Per-token USD prices: comma-separated "ADDR=price" pairs (e.g. "0x...=0.999,0x...=1800").
    /// Overrides CoinGecko prices for the specified tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_prices: Option<String>,
    /// Proximity window (in tx indices) for JitArb detection (default: 3).
    #[serde(default = "default_proximity_window")]
    pub proximity_window: usize,
    /// Capture pending transactions from the mempool during backtest (default: false).
    #[serde(default)]
    pub capture_pending: bool,
    /// Cross-block MEV detection window size (default: 0 = disabled).
    /// When > 1, tracks pool price snapshots across consecutive blocks.
    #[serde(default)]
    pub cross_block_window: usize,

    // ── Live mode fields ──────────────────────────────────────────────
    /// Starting virtual balance (native token, e.g. 10.0 ETH).
    #[serde(default = "default_initial_balance")]
    pub initial_balance: f64,
    /// Minimum profit threshold (native token) to execute a virtual trade.
    #[serde(default = "default_min_profit_threshold")]
    pub min_profit_threshold: f64,
    /// Mempool poll interval in milliseconds.
    #[serde(default = "default_poll_interval_ms")]
    pub poll_interval_ms: u64,
    /// Optional cap on virtual executions (None = unlimited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_executions: Option<u64>,

    // ── Dune Analytics integration ───────────────────────────────────────
    /// Dune Analytics API key. If set, enables Dune-based pool discovery and
    /// cross-validation features. Optional — all features gracefully degrade
    /// when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dune_api_key: Option<String>,
    /// When true, use Dune pool discovery as the primary source (on-chain fallback always runs).
    #[serde(default)]
    pub dune_primary_pool_discovery: bool,

}

fn default_initial_balance() -> f64 { 10.0 }
fn default_min_profit_threshold() -> f64 { 0.001 }
fn default_poll_interval_ms() -> u64 { 1000 }

fn default_rps_limit() -> f64 { 0.0 }

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

fn default_db_path() -> String {
    String::new() // empty = resolve to per-chain default
}

impl Config {
    /// Return the effective database path for the given chain.
    /// If the user provided a custom path via config or CLI, use that.
    /// Otherwise, use a per-chain path: `./cache/{chain}-mev-scout.sqlite`.
    pub fn effective_db_path(&self, chain: &ChainName) -> String {
        if self.db_path.is_empty() {
            format!("./cache/{}-mev-scout.sqlite", chain)
        } else {
            self.db_path.clone()
        }
    }
}

fn default_max_pairs_per_token() -> usize {
    50
}

fn default_proximity_window() -> usize { 3 }

impl Default for Config {
    fn default() -> Self {
        Config {
            chain: default_chain(),
            rpc_url: None,
            rpc_urls: Vec::new(),
            rpc_rps: Vec::new(),
            flash_loan_provider: default_flash_loan_provider(),
            strategies: default_strategies(),
            gas_model: default_gas_model(),
            gas_limit: default_gas_limit(),
            priority_fee_gwei: default_priority_fee_gwei(),
            output: default_output_format(),
            export_path: default_export_path(),
            db_path: default_db_path(),
            parquet_dir: None,
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
            block_concurrency: None,
            rps_limit: default_rps_limit(),
            price_oracle_mode: "coingecko".to_string(),
            token_prices: None,
            proximity_window: default_proximity_window(),
            capture_pending: false,
            cross_block_window: 0,
            initial_balance: default_initial_balance(),
            min_profit_threshold: default_min_profit_threshold(),
            poll_interval_ms: default_poll_interval_ms(),
            max_executions: None,
            dune_api_key: None,
            dune_primary_pool_discovery: false,
        }
    }
}



impl Config {
    /// Parse a TOML configuration file from disk.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or parsed as valid TOML.
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
        // Merge default chain configs so any chain referenced in cfg.chain
        // has a fallback, even if the TOML file has no [chains.*] section.
        let defaults = default_chains();
        for (name, default_cfg) in defaults {
            cfg.chains.entry(name).or_insert(default_cfg);
        }
        cfg
    }

    /// Return a Config pre-populated with all 7 default chain configurations.
    pub fn default_with_chains() -> Self {
        Config::default()
    }

    /// Resolved RPC URL list: user override(s) first, then public fallbacks for known chains.
    ///
    /// Returns URLs from `rpc_urls` first, then `rpc_url` (legacy single), then public endpoints.
    /// Errors only if no RPC source is available (no user URL and unknown chain).
    pub fn effective_rpc_urls(&self) -> error::Result<Vec<String>> {
        let mut urls = self.rpc_urls.clone();
        if let Some(single) = &self.rpc_url {
            if !urls.iter().any(|u| u == single) {
                urls.push(single.clone());
            }
        }
        if urls.is_empty() {
            return Err(error::Error::Other(
                "No RPC URL provided. Use --rpc <URL>, --rpc-urls, or set rpc_url in config.".into()
            ));
        }
        Ok(urls)
    }

    /// Build full provider configs by merging user-supplied URLs with public fallbacks.
    ///
    /// Returns `Vec<(String, Option<f64>, bool)>` — URL, optional per-provider RPS limit, and
    /// whether the endpoint is known to support archive queries. When `rpc_rps` has matching
    /// entries, those are used; otherwise defaults from `ChainName::public_rpc_endpoints()` are
    /// used for public endpoints. Falls back to public endpoints for known chains if no user RPC
    /// is provided.
    pub fn effective_provider_configs(&self, chain_name: crate::types::ChainName) -> error::Result<Vec<(String, Option<f64>, bool)>> {
        let urls = self.effective_rpc_urls().unwrap_or_default();
        if !urls.is_empty() {
            let public_endpoints = chain_name.public_rpc_endpoints();
            let result: Vec<(String, Option<f64>, bool)> = urls
                .into_iter()
                .enumerate()
                .map(|(i, url)| {
                    let rps = self.rpc_rps.get(i).copied();
                    if let Some(r) = rps {
                        // Check if any known public endpoint matches
                        let archive = public_endpoints
                            .iter()
                            .find(|e| url.contains(e.url) || e.url.contains(&url))
                            .map(|e| e.archive)
                            .unwrap_or(true); // Unknown endpoints assumed archive until proven otherwise
                        return (url, Some(r), archive);
                    }
                    let (default_rps, archive) = public_endpoints
                        .iter()
                        .find(|e| url.contains(e.url) || e.url.contains(&url))
                        .map(|e| (Some(e.default_rps), e.archive))
                        .unwrap_or((Some(self.rps_limit), true));
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
    ///
    /// When `block_concurrency` is explicitly set in config/CLI, that value is used.
    /// Otherwise, calculates from the minimum per-provider RPS:
    ///   `min(min_rps × 2, MAX_PER_SHARD).max(MIN_PER_SHARD)`
    ///
    /// The ×2 factor keeps the fetch pipeline full (tasks queue on the rate limiter
    /// while waiting for their turn, so concurrency must exceed RPS to avoid idle slots).
    /// The cap prevents overwhelming free/public RPC providers with too many
    /// simultaneous connections.
    ///
    /// Defaults to 20 when no RPS limits are configured (safe for public RPCs).
    pub fn effective_block_concurrency(
        &self,
        provider_configs: &[(String, Option<f64>, bool)],
    ) -> usize {
        if let Some(bc) = self.block_concurrency {
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

    /// Return only the user-specified RPC URLs (no public fallbacks), for backward compat.
    /// Errors if no user URL is provided.
    pub fn user_rpc_urls(&self) -> error::Result<Vec<String>> {
        let mut urls = self.rpc_urls.clone();
        if let Some(single) = &self.rpc_url {
            if !urls.iter().any(|u| u == single) {
                urls.push(single.clone());
            }
        }
        if urls.is_empty() {
            return Err(error::Error::Other(
                "No RPC URL provided. Use --rpc <URL>, --rpc-urls, or set rpc_url in config.".into()
            ));
        }
        Ok(urls)
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
Cross-block window:  {}
DB path:             {}
Parquet dir:         {}
"#,
            chain_name,
            chain_cfg.chain_id,
            self.rpc_url.clone().unwrap_or_else(|| "RPC not set".to_string()),
            range_mode,
            range_mode.resolve_description(),
            strat_list,
            provider_desc,
            self.gas_model,
            if self.cross_block_window > 0 { format!("{} blocks", self.cross_block_window) } else { "disabled".to_string() },
            self.effective_db_path(&chain_name),
            self.parquet_dir.as_deref().unwrap_or("(none)"),
        )
    }
}

/// Merge an optional CLI override into a config field.
macro_rules! merge_opt {
    // Non-Copy types: override Option<T> → config T (clone out)
    ($cfg:expr, $cli:expr, $field:ident) => {
        if let Some(ref v) = $cli.$field {
            $cfg.$field = v.clone();
        }
    };
    // Non-Copy types: override Option<T> → config Option<T>
    ($cfg:expr, $cli:expr, $field:ident, into_option) => {
        if let Some(ref v) = $cli.$field {
            $cfg.$field = Some(v.clone());
        }
    };
    // Copy types: override Option<Copy> → config Copy
    ($cfg:expr, $cli:expr, $field:ident, copy) => {
        if let Some(v) = $cli.$field {
            $cfg.$field = v;
        }
    };
    // Copy types: override Option<Copy> → config Option<Copy>
    ($cfg:expr, $cli:expr, $field:ident, copy_some) => {
        if let Some(v) = $cli.$field {
            $cfg.$field = Some(v);
        }
    };
}

#[derive(Debug, Clone, Default)]
pub struct CliOverrides {
    pub days: Option<u64>,
    pub blocks: Option<u64>,
    pub block: Option<u64>,
    pub from_block: Option<u64>,
    pub to_block: Option<u64>,
    pub chain: Option<String>,
    pub rpc_url: Option<String>,
    pub rpc_urls: Option<Vec<String>>,
    pub rpc_rps: Option<Vec<f64>>,
    pub block_concurrency: Option<usize>,
    pub rps_limit: Option<f64>,
    pub flash_loan_provider: Option<String>,
    pub strategies: Option<String>,
    pub gas_model: Option<String>,
    pub gas_limit: Option<u64>,
    pub priority_fee_gwei: Option<f64>,
    pub output: Option<String>,
    pub export_path: Option<String>,
    pub db_path: Option<String>,
    pub parquet_dir: Option<String>,
    pub coingecko_api_key: Option<String>,
    pub price_oracle_mode: Option<String>,
    pub token_prices: Option<String>,
    pub proximity_window: Option<usize>,
    pub capture_pending: Option<bool>,
    pub cross_block_window: Option<usize>,

    // ── Live mode overrides ───────────────────────────────────────────
    pub initial_balance: Option<f64>,
    pub min_profit_threshold: Option<f64>,
    pub poll_interval_ms: Option<u64>,
    pub max_executions: Option<u64>,

    // ── Dune overrides ─────────────────────────────────────────────────
    pub dune_api_key: Option<String>,
    pub dune_primary_pool_discovery: Option<bool>,
}

impl Config {
    pub fn merge_cli(&mut self, overrides: &CliOverrides) {
        merge_opt!(self, overrides, days, copy_some);
        merge_opt!(self, overrides, blocks, copy_some);
        merge_opt!(self, overrides, block, copy_some);
        merge_opt!(self, overrides, from_block, copy_some);
        merge_opt!(self, overrides, to_block, copy_some);
        merge_opt!(self, overrides, chain);
        merge_opt!(self, overrides, rpc_url, into_option);
        merge_opt!(self, overrides, rpc_urls);
        merge_opt!(self, overrides, rpc_rps);
        merge_opt!(self, overrides, flash_loan_provider);        merge_opt!(self, overrides, strategies);
        merge_opt!(self, overrides, gas_model);
        merge_opt!(self, overrides, gas_limit, copy);
        merge_opt!(self, overrides, priority_fee_gwei, copy);
        merge_opt!(self, overrides, output);
        merge_opt!(self, overrides, export_path);
        merge_opt!(self, overrides, db_path);
        merge_opt!(self, overrides, parquet_dir, into_option);
        merge_opt!(self, overrides, coingecko_api_key, into_option);
        merge_opt!(self, overrides, block_concurrency, copy_some);
        merge_opt!(self, overrides, rps_limit, copy);
        merge_opt!(self, overrides, price_oracle_mode);
        merge_opt!(self, overrides, token_prices, into_option);
        merge_opt!(self, overrides, proximity_window, copy);
        merge_opt!(self, overrides, capture_pending, copy);
        merge_opt!(self, overrides, cross_block_window, copy);
        merge_opt!(self, overrides, initial_balance, copy);
        merge_opt!(self, overrides, min_profit_threshold, copy);
        merge_opt!(self, overrides, poll_interval_ms, copy);
        merge_opt!(self, overrides, max_executions, copy_some);
        merge_opt!(self, overrides, dune_api_key, into_option);
        merge_opt!(self, overrides, dune_primary_pool_discovery, copy);
    }

    /// Parse the `--token-price` value (e.g. "0xABC=0.999,0xDEF=1800") into a
    /// `HashMap<Address, f64>`. Returns an empty map when config value is `None`.
    pub fn parse_token_prices(&self) -> std::collections::HashMap<alloy::primitives::Address, f64> {
        let mut map = std::collections::HashMap::new();
        let Some(s) = &self.token_prices else { return map };
        for pair in s.split(',') {
            let pair = pair.trim();
            if pair.is_empty() { continue; }
            if let Some((addr_str, price_str)) = pair.split_once('=') {
                if let (Ok(addr), Ok(price)) = (
                    addr_str.trim().parse::<alloy::primitives::Address>(),
                    price_str.trim().parse::<f64>(),
                ) {
                    map.insert(addr, price);
                }
            }
        }
        map
    }
}


