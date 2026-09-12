//! Configuration file parsing, types, and defaults for chains, strategies, and runtime parameters.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::defaults::{default_chains, ChainConfig};
use crate::error;

use crate::types::{ChainName, FlashLoanProvider, RangeMode, Strategy};

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
    /// Block-level concurrency within each provider shard
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_concurrency: Option<usize>,
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
    /// Minimum profit in wei to keep an opportunity (filters dust). 0 = disabled.
    #[serde(default)]
    pub min_profit_wei: u64,
    /// Maximum candidates to keep per transaction (top by profit). 0 = unlimited.
    #[serde(default)]
    pub max_candidates_per_tx: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputConfig {
    /// Output format: "table", "json", or "csv"
    #[serde(default = "default_output_format")]
    pub output: String,
    /// Directory for SQLite database file
    #[serde(default = "default_db_path")]
    pub db_path: String,
}

/// Explorer sub-config: `[explorer]` TOML section.
/// All fields optional — defaults keep existing config files valid.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplorerConfig {
    /// Explorer SQLite database path (forensic layer). Empty = derived per chain.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub db_path: String,
    /// Confirmation lag before a block is indexed (default 6 ≈ 12s on Polygon).
    #[serde(default = "default_explorer_confirmations")]
    pub confirmations: u64,
    /// Live-mode polling interval in milliseconds.
    #[serde(default = "default_explorer_poll_ms")]
    pub poll_interval_ms: u64,
    /// Blocks per sync_state checkpoint during backfill.
    #[serde(default = "default_explorer_checkpoint_every")]
    pub checkpoint_every: u64,
}

fn default_explorer_confirmations() -> u64 {
    6
}
fn default_explorer_poll_ms() -> u64 {
    2000
}
fn default_explorer_checkpoint_every() -> u64 {
    500
}

// ── Default helpers ─────────────────────────────────────────────────

fn default_rps_limit() -> f64 {
    0.0
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
fn default_db_path() -> String {
    String::new()
}
fn default_max_pairs_per_token() -> usize {
    50
}
fn default_proximity_window() -> usize {
    3
}

// ── Default impls for sub-structs ───────────────────────────────────

impl Default for RpcConfig {
    fn default() -> Self {
        RpcConfig {
            rpc_url: None,
            rpc_urls: Vec::new(),
            rpc_rps: Vec::new(),
            rps_limit: default_rps_limit(),
            block_concurrency: None,
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
            min_profit_wei: 0,
            max_candidates_per_tx: 0,
        }
    }
}

impl Default for OutputConfig {
    fn default() -> Self {
        OutputConfig {
            output: default_output_format(),
            db_path: default_db_path(),
        }
    }
}

impl Default for ExplorerConfig {
    fn default() -> Self {
        ExplorerConfig {
            db_path: String::new(),
            confirmations: default_explorer_confirmations(),
            poll_interval_ms: default_explorer_poll_ms(),
            checkpoint_every: default_explorer_checkpoint_every(),
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
    /// Explorer sub-config — a named section so its keys never collide with
    /// the flattened top-level fields above.
    #[serde(default)]
    pub explorer: ExplorerConfig,
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
            explorer: ExplorerConfig::default(),
        }
    }
}

/// One resolved RPC endpoint: URL, optional RPS limit, archive capability.
#[derive(Clone, Debug)]
pub struct ProviderConfig {
    pub url: String,
    pub rps: Option<f64>,
    pub archive: bool,
}

impl Config {
    /// Effective explorer database path for the given chain.
    pub fn effective_explorer_db_path(&self, chain: &ChainName) -> String {
        if self.explorer.db_path.is_empty() {
            format!("./cache/explorer-{}.sqlite", chain)
        } else {
            self.explorer.db_path.clone()
        }
    }

    /// Parse a TOML configuration file from disk.
    pub fn load(path: &str) -> error::Result<Self> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            error::Error::Other(format!("Failed to read config file '{}': {}", path, e))
        })?;
        let mut cfg: Config = toml::from_str(&content).map_err(|e| {
            error::Error::Other(format!("Failed to parse config file '{}': {}", path, e))
        })?;
        cfg.expand_env_secrets();
        cfg.config_path = Some(PathBuf::from(path));
        Ok(cfg)
    }

    /// Load a config file, returning `Ok(None)` only when the file does
    /// not exist. Read/parse failures are hard errors so a broken or
    /// malformed config never silently runs with defaults.
    pub fn load_optional(path: &str) -> error::Result<Option<Self>> {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(error::Error::Other(format!(
                    "Failed to read config file '{}': {}",
                    path, e
                )))
            }
        };
        let mut cfg: Config = toml::from_str(&content).map_err(|e| {
            error::Error::Other(format!("Failed to parse config file '{}': {}", path, e))
        })?;
        cfg.expand_env_secrets();
        cfg.config_path = Some(PathBuf::from(path));
        Ok(Some(cfg))
    }

    /// Expand `${ENV_VAR}` references in secret-bearing string fields
    /// (`rpc_url`, `rpc_urls`) from the process
    /// environment. Lets committed config files stay free of live API keys:
    /// write `rpc_urls = ["https://.../v2/${ALCHEMY_API_KEY}"]` and export the
    /// variable instead. Unset variables are left verbatim so a missing env
    /// never silently corrupts a URL (the provider then fails loudly).
    pub fn expand_env_secrets(&mut self) {
        fn expand(s: &str) -> String {
            let mut out = String::with_capacity(s.len());
            let mut rest = s;
            while let Some(start) = rest.find("${") {
                out.push_str(&rest[..start]);
                let after = &rest[start + 2..];
                match after.find('}') {
                    Some(end) => {
                        let name = &after[..end];
                        match std::env::var(name) {
                            Ok(val) => out.push_str(&val),
                            // Unset → keep the literal ${NAME} placeholder.
                            Err(_) => out.push_str(&rest[start..start + 2 + end + 1]),
                        }
                        rest = &after[end + 1..];
                    }
                    None => {
                        out.push_str(&rest[start..]);
                        rest = "";
                    }
                }
            }
            out.push_str(rest);
            out
        }
        if let Some(u) = &self.rpc.rpc_url {
            self.rpc.rpc_url = Some(expand(u));
        }
        for u in &mut self.rpc.rpc_urls {
            *u = expand(u);
        }
    }

    /// Load a config file, falling back to defaults only when the file
    /// does not exist. Parse/validation failures propagate so a malformed
    /// config never silently runs with wrong chain/RPC/strategy defaults.
    pub fn load_or_default(path: &str) -> error::Result<Self> {
        let mut cfg = match Self::load_optional(path)? {
            Some(mut cfg) => {
                cfg.config_path = Some(PathBuf::from(path));
                cfg
            }
            None => {
                tracing::info!("config file '{path}' not found; using built-in defaults");
                Config {
                    config_path: Some(PathBuf::from(path)),
                    ..Config::default()
                }
            }
        };
        let defaults = default_chains();
        for (name, default_cfg) in defaults {
            cfg.chains.entry(name).or_insert(default_cfg);
        }
        Ok(cfg)
    }

    #[cfg(test)]
    fn from_toml_str(s: &str) -> Self {
        let mut cfg: Config = toml::from_str(s).unwrap();
        cfg.expand_env_secrets();
        cfg
    }

    /// Resolved RPC URL list: user override(s) first, then public fallbacks for known chains.
    pub fn effective_rpc_urls(&self) -> error::Result<Vec<String>> {
        let urls = Self::merge_rpc_urls(&self.rpc.rpc_urls, &self.rpc.rpc_url);
        if urls.is_empty() {
            return Err(error::Error::Other(
                "No RPC URL provided. Use --rpc <URL>, --rpc-urls, or set rpc_url in config."
                    .into(),
            ));
        }
        Ok(urls)
    }

    /// Human-readable RPC summary for the startup plan display.
    fn effective_rpc_display(&self) -> String {
        let user_count = self.rpc.rpc_urls.len() + if self.rpc.rpc_url.is_some() { 1 } else { 0 };
        if user_count > 0 {
            format!("{} provider(s) configured", user_count)
        } else {
            "No RPC configured — using public fallbacks".to_string()
        }
    }

    /// Build full provider configs by merging user-supplied URLs with public fallbacks.
    pub fn effective_provider_configs(
        &self,
        chain_name: ChainName,
    ) -> error::Result<Vec<ProviderConfig>> {
        let urls = self.effective_rpc_urls().unwrap_or_default();
        if !urls.is_empty() {
            let public_endpoints = chain_name.public_rpc_endpoints();
            let result: Vec<ProviderConfig> = urls
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
                        return ProviderConfig {
                            url,
                            rps: Some(r),
                            archive,
                        };
                    }
                    let (default_rps, archive) = public_endpoints
                        .iter()
                        .find(|e| url.contains(e.url) || e.url.contains(&url))
                        .map(|e| (Some(e.default_rps), e.archive))
                        .unwrap_or((Some(self.rpc.rps_limit), false));
                    ProviderConfig {
                        url,
                        rps: default_rps,
                        archive,
                    }
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
            Ok(public
                .into_iter()
                .map(|e| ProviderConfig {
                    url: e.url.to_string(),
                    rps: Some(e.default_rps),
                    archive: e.archive,
                })
                .collect())
        }
    }

    /// Auto-calculate optimal `block_concurrency` from provider RPS limits.
    pub fn effective_block_concurrency(
        &self,
        provider_configs: &[ProviderConfig],
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
            .filter_map(|p| p.rps)
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
            FlashLoanProvider::Auto => {
                "auto (Balancer V2 → Aave V3 → Uniswap Flash Swap)".to_string()
            }
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
"#,
            chain_name,
            chain_cfg.chain_id,
            self.effective_rpc_display(),
            range_mode,
            range_mode.resolve_description(),
            strat_list,
            provider_desc,
            self.gas.gas_model,
            self.effective_db_path(&chain_name),
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
    pub min_profit_wei: Option<u64>,
    pub max_candidates_per_tx: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct OutputOverrides {
    pub output: Option<String>,
    pub db_path: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ExplorerOverrides {
    pub db_path: Option<String>,
    pub confirmations: Option<u64>,
    pub poll_interval_ms: Option<u64>,
    pub checkpoint_every: Option<u64>,
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
    pub explorer: ExplorerOverrides,
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
/// ```
/// use mev_scout_core::config::ConfigBuilder;
///
/// let config = ConfigBuilder::default()
///     .with_chain("polygon")
///     .build();
/// ```
#[derive(Debug, Clone, Default)]
pub struct ConfigBuilder {
    chain: Option<String>,
    output: Option<OutputConfig>,
}

impl ConfigBuilder {
    /// Set the chain name (e.g. "polygon", "ethereum").
    pub fn with_chain(mut self, chain: impl Into<String>) -> Self {
        self.chain = Some(chain.into());
        self
    }
    /// Replace the output sub-config entirely.
    pub fn with_output(mut self, output: OutputConfig) -> Self {
        self.output = Some(output);
        self
    }

    /// Build a `Config`, starting from defaults and overriding set fields.
    pub fn build(self) -> Config {
        let mut cfg = Config::default();
        if let Some(v) = self.chain {
            cfg.chain = v;
        }
        if let Some(v) = self.output {
            cfg.output = v;
        }
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

        merge_sub!(
            self,
            overrides,
            rpc,
            [
                (rpc_url, into_option),
                (rpc_urls),
                (rpc_rps),
                (rps_limit, copy),
                (block_concurrency, copy_some)
            ]
        );
        merge_sub!(
            self,
            overrides,
            gas,
            [(gas_model), (gas_limit, copy), (priority_fee_gwei, copy)]
        );
        merge_sub!(
            self,
            overrides,
            backtest,
            [
                (flash_loan_provider),
                (strategies),
                (max_pairs_per_token, copy),
                (proximity_window, copy),
                (capture_pending, copy),
                (min_profit_wei, copy),
                (max_candidates_per_tx, copy)
            ]
        );
        merge_sub!(self, overrides, output, [(output), (db_path)]);
        merge_sub!(
            self,
            overrides,
            explorer,
            [
                (db_path),
                (confirmations, copy),
                (poll_interval_ms, copy),
                (checkpoint_every, copy)
            ]
        );
    }
}

#[cfg(test)]
mod env_expansion_tests {
    use super::Config;

    #[test]
    fn expands_set_env_vars_in_rpc_urls() {
        // SAFETY: single-threaded test binary execution for this module; the
        // variable name is test-specific.
        std::env::set_var("MS_CONFIG_TEST_RPC_KEY", "sekret123");
        let cfg = Config::from_toml_str(
            r#"
rpc_urls = ["https://polygon-mainnet.g.alchemy.com/v2/${MS_CONFIG_TEST_RPC_KEY}"]
"#,
        );
        assert_eq!(
            cfg.rpc.rpc_urls[0],
            "https://polygon-mainnet.g.alchemy.com/v2/sekret123"
        );
    }

    #[test]
    fn leaves_unset_vars_verbatim() {
        std::env::remove_var("MS_CONFIG_TEST_MISSING_KEY");
        let cfg = Config::from_toml_str(
            r#"
rpc_urls = ["https://rpc.example/v2/${MS_CONFIG_TEST_MISSING_KEY}"]
"#,
        );
        assert_eq!(
            cfg.rpc.rpc_urls[0],
            "https://rpc.example/v2/${MS_CONFIG_TEST_MISSING_KEY}"
        );
    }

    #[test]
    fn expands_plain_urls_and_env_reference() {
        std::env::set_var("MS_CONFIG_TEST_CG_KEY", "CG-1");
        let cfg = Config::from_toml_str(
            r#"
rpc_urls = ["https://plain.example/v3"]
rpc_url = "https://single.example/${MS_CONFIG_TEST_CG_KEY}"
"#,
        );
        assert_eq!(cfg.rpc.rpc_urls[0], "https://plain.example/v3");
        assert_eq!(
            cfg.rpc.rpc_url.as_deref(),
            Some("https://single.example/CG-1")
        );
    }

    #[test]
    fn unterminated_placeholder_is_kept_verbatim() {
        let cfg = Config::from_toml_str(
            r#"
rpc_urls = ["https://broken.example/${NO_CLOSING"]
"#,
        );
        assert_eq!(cfg.rpc.rpc_urls[0], "https://broken.example/${NO_CLOSING");
    }
}
