use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FlashLoanProvider {
    Auto,
    Balancer,
    Aave,
    Uniswap,
}

impl fmt::Display for FlashLoanProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FlashLoanProvider::Auto => write!(f, "auto"),
            FlashLoanProvider::Balancer => write!(f, "balancer"),
            FlashLoanProvider::Aave => write!(f, "aave"),
            FlashLoanProvider::Uniswap => write!(f, "uniswap"),
        }
    }
}

impl FromStr for FlashLoanProvider {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "auto" => Ok(FlashLoanProvider::Auto),
            "balancer" => Ok(FlashLoanProvider::Balancer),
            "aave" => Ok(FlashLoanProvider::Aave),
            "uniswap" => Ok(FlashLoanProvider::Uniswap),
            _ => Err(format!(
                "unknown flash loan provider '{s}'. Supported: auto, balancer, aave, uniswap"
            )),
        }
    }
}

impl FlashLoanProvider {
    pub fn is_forced(self) -> bool {
        self != FlashLoanProvider::Auto
    }

    /// Fee rate in basis points (1/10000).
    /// Aave: 0.09% = 9 bps; Balancer: 0% = 0 bps; Uniswap: ~0.10% = 10 bps.
    /// For Auto, returns 0 (assumes we pick Balancer, which has no fee).
    pub fn fee_rate_bps(self) -> u128 {
        match self {
            FlashLoanProvider::Auto => 0,
            FlashLoanProvider::Balancer => 0,
            FlashLoanProvider::Aave => 9,     // 0.09%
            FlashLoanProvider::Uniswap => 10,  // 0.10% (varies by pool)
        }
    }

    pub fn priority_list(auto_mode: bool) -> &'static [FlashLoanProvider] {
        if auto_mode {
            &[
                FlashLoanProvider::Balancer,
                FlashLoanProvider::Aave,
                FlashLoanProvider::Uniswap,
            ]
        } else {
            &[]
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Strategy {
    TwoHopArb,
    MultiHopArb,
    Jit,
    JitArb,
    Sandwich,
    Liquidation,
    CrossBlockArb,
    TimeBandit,
}

impl fmt::Display for Strategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Strategy::TwoHopArb => write!(f, "two_hop_arb"),
            Strategy::MultiHopArb => write!(f, "multi_hop_arb"),
            Strategy::Jit => write!(f, "jit"),
            Strategy::JitArb => write!(f, "jit_arb"),
            Strategy::Sandwich => write!(f, "sandwich"),
            Strategy::Liquidation => write!(f, "liquidation"),
            Strategy::CrossBlockArb => write!(f, "cross_block_arb"),
            Strategy::TimeBandit => write!(f, "time_bandit"),
        }
    }
}

impl FromStr for Strategy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "two_hop_arb" => Ok(Strategy::TwoHopArb),
            "multi_hop_arb" => Ok(Strategy::MultiHopArb),
            "jit" => Ok(Strategy::Jit),
            "jit_arb" => Ok(Strategy::JitArb),
            "sandwich" => Ok(Strategy::Sandwich),
            "liquidation" => Ok(Strategy::Liquidation),
            "cross_block_arb" => Ok(Strategy::CrossBlockArb),
            "time_bandit" => Ok(Strategy::TimeBandit),
            _ => Err(format!(
                "unknown strategy '{s}'. Supported: two_hop_arb, multi_hop_arb, jit, jit_arb, sandwich, liquidation, cross_block_arb, time_bandit, all"
            )),
        }
    }
}

impl Strategy {
    pub fn all() -> &'static [Strategy] {
        &[
            Strategy::TwoHopArb,
            Strategy::MultiHopArb,
            Strategy::Jit,
            Strategy::JitArb,
            Strategy::Sandwich,
            Strategy::Liquidation,
            Strategy::CrossBlockArb,
            Strategy::TimeBandit,
        ]
    }

    pub fn from_comma_list(s: &str) -> Result<Vec<Strategy>, String> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("all") {
            return Ok(Strategy::all().to_vec());
        }
        s.split(',')
            .map(|part| part.trim().parse::<Strategy>())
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RangeMode {
    Days(u64),
    Blocks(u64),
    Single(u64),
    Range(u64, u64),
}

impl fmt::Display for RangeMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RangeMode::Days(n) => write!(f, "last {n} days"),
            RangeMode::Blocks(n) => write!(f, "last {n} blocks"),
            RangeMode::Single(n) => write!(f, "single block #{n}"),
            RangeMode::Range(a, b) => write!(f, "blocks {a}–{b} ({} blocks)", b - a + 1),
        }
    }
}

impl RangeMode {
    pub fn resolve_description(&self) -> String {
        match self {
            RangeMode::Days(_) => "resolves at runtime via binary search on timestamps".to_string(),
            RangeMode::Blocks(_) => "resolves at runtime from chain tip".to_string(),
            RangeMode::Single(_) => "single block mode".to_string(),
            RangeMode::Range(from, to) => format!("blocks {from}–{to} ({} blocks)", to - from + 1),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum GasModel {
    #[serde(rename = "historical_exact")]
    #[default]
    HistoricalExact,
    #[serde(rename = "p90")]
    P90,
    #[serde(rename = "fixed")]
    Fixed,
    /// Use the N-th percentile effective gas price from the historical
    /// distribution tracked by `GasPriceDistribution` (H10).
    /// Storage value N (1–99) is the percentile. Example: `Distribution(90)`
    /// uses the 90th percentile from recent blocks' effective gas prices.
    #[serde(rename = "distribution")]
    Distribution(u8),
    /// Live mode — fetches base fee and priority fee from the chain in real-time.
    /// Uses `eth_gasPrice` (or base fee from the pending block) and
    /// `eth_maxPriorityFeePerGas` to build a realistic gas price estimate.
    /// No historical distribution is used.
    #[serde(rename = "live")]
    Live,
}

impl fmt::Display for GasModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GasModel::HistoricalExact => write!(f, "historical_exact"),
            GasModel::P90 => write!(f, "p90"),
            GasModel::Fixed => write!(f, "fixed"),
            GasModel::Distribution(p) => write!(f, "distribution_{p}"),
            GasModel::Live => write!(f, "live"),
        }
    }
}

impl FromStr for GasModel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let lower = s.to_lowercase();
        match lower.as_str() {
            "historical_exact" => Ok(GasModel::HistoricalExact),
            "p90" => Ok(GasModel::P90),
            "fixed" => Ok(GasModel::Fixed),
            "live" => Ok(GasModel::Live),
            _ => {
                if let Some(rest) = lower.strip_prefix("distribution_") {
                    if let Ok(p) = rest.parse::<u8>() {
                        if p >= 1 && p <= 99 {
                            return Ok(GasModel::Distribution(p));
                        }
                    }
                }
                if let Some(rest) = lower.strip_prefix("distribution") {
                    if let Ok(p) = rest.parse::<u8>() {
                        if p >= 1 && p <= 99 {
                            return Ok(GasModel::Distribution(p));
                        }
                    }
                }
                Err(format!(
                    "unknown gas model '{s}'. Supported: historical_exact, p90, fixed, live, distribution_N (1-99)"
                ))
            }
        }
    }
}

impl GasModel {
    /// Return the target percentile for this gas model.
    /// For `P90` returns 90. For `Distribution(p)` returns p.
    /// For `HistoricalExact` and `Fixed` returns `None`.
    pub fn target_percentile(&self) -> Option<u8> {
        match self {
            GasModel::P90 => Some(90),
            GasModel::Distribution(p) => Some(*p),
            GasModel::Live => None,
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct GasConfig {
    pub gas_limit: u64,
    pub gas_model: GasModel,
    pub priority_fee_gwei: f64,
    pub flash_loan_provider: FlashLoanProvider,
    /// Premium multiplier on top of priority fee to account for PGA dynamics.
    /// 0.0 = no premium (base priority fee). 0.5 = 50% premium.
    /// When PGA is enabled, this is set from `PgaConfig` to model the
    /// winning bid premium needed to outbid competitors (H10).
    pub winning_bid_premium: f64,
    /// Pre-computed N-th percentile effective gas price from the historical
    /// gas price distribution (H10). When set, `GasModel::P90` and
    /// `GasModel::Distribution(p)` use this value instead of the crude
    /// `base_fee * 150%` multiplier. Set by `BacktestRunner` before each
    /// block based on recent blocks' effective gas prices.
    pub percentile_gas_price: Option<u128>,
}

impl GasConfig {
    /// Compute the effective priority fee in wei, optionally inflated by
    /// the PGA winning bid premium.
    fn effective_priority_fee_wei(&self) -> u128 {
        let base_pf = self.priority_fee_gwei * 1_000_000_000.0;
        let premium = self.winning_bid_premium.max(0.0);
        (base_pf * (1.0 + premium)) as u128
    }

    /// Gas cost given an explicit gas limit (pool-type-aware, per-opportunity).
    /// When `winning_bid_premium > 0`, the priority fee is inflated to
    /// model the cost of winning inclusion in a competitive auction.
    ///
    /// For `GasModel::P90` and `GasModel::Distribution(p)`, uses the
    /// pre-computed `percentile_gas_price` from the historical distribution
    /// when available, falling back to the crude `base_fee * 150%` multiplier
    /// when distribution data has not been collected yet (H10).
    pub fn compute_gas_cost_with_limit(
        &self,
        gas_limit: u64,
        base_fee_per_gas: u128,
    ) -> u128 {
        let pf_wei = self.effective_priority_fee_wei();
        let effective_price = match self.gas_model {
            GasModel::HistoricalExact => base_fee_per_gas.saturating_add(pf_wei),
            GasModel::Fixed => pf_wei,
            GasModel::P90 | GasModel::Distribution(_) => {
                // Use histogram-derived percentile when available (H10),
                // fall back to the crude 150% multiplier while collecting data.
                self.percentile_gas_price
                    .unwrap_or_else(|| {
                        base_fee_per_gas.saturating_mul(150).saturating_div(100)
                    })
                    .saturating_add(pf_wei)
            }
            GasModel::Live => base_fee_per_gas.saturating_add(pf_wei),
        };
        (gas_limit as u128).saturating_mul(effective_price)
    }

    /// Compute the flash loan fee for a given principal amount.
    /// fee = input_amount * fee_rate_bps / 10000
    pub fn flash_loan_fee(&self, input_amount: u128) -> u128 {
        let bps = self.flash_loan_provider.fee_rate_bps();
        if bps == 0 { return 0; }
        input_amount.saturating_mul(bps).saturating_div(10_000)
    }

    /// Set the winning bid premium from PGA configuration (H10).
    /// Returns self for chaining.
    ///
    /// Premium formula: `intensity × mean_competitors / (mean_competitors + 1)`.
    /// With mean_competitors=3, intensity=0.5: premium ≈ 37.5%
    /// With mean_competitors=10, intensity=1.0: premium ≈ 91%
    pub fn with_winning_bid_premium(mut self, mean_competitors: f64, intensity: f64) -> Self {
        let n = mean_competitors.max(1.0);
        let premium = intensity * (n / (n + 1.0));
        self.winning_bid_premium = premium.min(10.0); // cap at 10x
        self
    }
}

impl Default for GasConfig {
    fn default() -> Self {
        GasConfig {
            gas_limit: 200_000,
            gas_model: GasModel::default(),
            priority_fee_gwei: 0.0,
            flash_loan_provider: FlashLoanProvider::Auto,
            winning_bid_premium: 0.0,
            percentile_gas_price: None,
        }
    }
}

/// Describes where token USD prices come from.
#[derive(Debug, Clone)]
pub enum PriceSource {
    /// Fetch prices dynamically from CoinGecko API.
    CoinGecko,
    /// Pre-fetched prices from CoinGecko (token address → USD).
    FromCoinGecko(std::collections::HashMap<alloy::primitives::Address, f64>),
    /// Prices provided via CLI --token-price flag.
    FromCli(std::collections::HashMap<alloy::primitives::Address, f64>),
}

/// Controls how native token USD price is sourced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum PriceOracleMode {
    /// Use CoinGecko API only (default, backward compat).
    #[default]
    CoinGeckoOnly,
    /// Derive native token price from the highest-TVL on-chain pool.
    OnChain,
    /// Fetch both CoinGecko and on-chain; warn if divergence >5%.
    Hybrid,
}

impl fmt::Display for PriceOracleMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PriceOracleMode::CoinGeckoOnly => write!(f, "coingecko"),
            PriceOracleMode::OnChain => write!(f, "onchain"),
            PriceOracleMode::Hybrid => write!(f, "hybrid"),
        }
    }
}

impl FromStr for PriceOracleMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "coingecko" | "coingecko_only" => Ok(PriceOracleMode::CoinGeckoOnly),
            "onchain" | "on_chain" => Ok(PriceOracleMode::OnChain),
            "hybrid" => Ok(PriceOracleMode::Hybrid),
            _ => Err(format!("unknown price oracle mode '{s}'. Supported: coingecko, onchain, hybrid")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OutputFormat {
    #[serde(rename = "table")]
    Table,
    #[serde(rename = "csv")]
    Csv,
    #[serde(rename = "json")]
    Json,
}

impl fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OutputFormat::Table => write!(f, "table"),
            OutputFormat::Csv => write!(f, "csv"),
            OutputFormat::Json => write!(f, "json"),
        }
    }
}

impl FromStr for OutputFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "table" => Ok(OutputFormat::Table),
            "csv" => Ok(OutputFormat::Csv),
            "json" => Ok(OutputFormat::Json),
            _ => Err(format!(
                "unknown output format '{s}'. Supported: table, csv, json"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ExecutorType {
    FlashLoanArbitrage,
    Sandwich,
    Liquidation,
    JitLiquidity,
}

impl fmt::Display for ExecutorType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExecutorType::FlashLoanArbitrage => write!(f, "flash_loan_arbitrage"),
            ExecutorType::Sandwich => write!(f, "sandwich"),
            ExecutorType::Liquidation => write!(f, "liquidation"),
            ExecutorType::JitLiquidity => write!(f, "jit_liquidity"),
        }
    }
}

impl ExecutorType {
    pub fn from_strategy(strategy: Strategy) -> Option<Self> {
        match strategy {
            Strategy::TwoHopArb | Strategy::MultiHopArb => Some(ExecutorType::FlashLoanArbitrage),
            Strategy::Sandwich => Some(ExecutorType::Sandwich),
            Strategy::Liquidation => Some(ExecutorType::Liquidation),
            Strategy::Jit | Strategy::JitArb => Some(ExecutorType::JitLiquidity),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flash_loan_roundtrip() {
        for (p, s) in &[
            (FlashLoanProvider::Auto, "auto"),
            (FlashLoanProvider::Balancer, "balancer"),
            (FlashLoanProvider::Aave, "aave"),
            (FlashLoanProvider::Uniswap, "uniswap"),
        ] {
            assert_eq!(p.to_string(), *s);
            assert_eq!(s.parse::<FlashLoanProvider>().unwrap(), *p);
        }
    }

    #[test]
    fn test_flash_loan_is_forced() {
        assert!(!FlashLoanProvider::Auto.is_forced());
        assert!(FlashLoanProvider::Balancer.is_forced());
        assert!(FlashLoanProvider::Aave.is_forced());
        assert!(FlashLoanProvider::Uniswap.is_forced());
    }

    #[test]
    fn test_flash_loan_priority_list() {
        assert_eq!(FlashLoanProvider::priority_list(true).len(), 3);
        assert!(FlashLoanProvider::priority_list(false).is_empty());
    }

    #[test]
    fn test_strategy_roundtrip() {
        for (s, expected) in &[
            (Strategy::TwoHopArb, "two_hop_arb"),
            (Strategy::MultiHopArb, "multi_hop_arb"),
            (Strategy::Jit, "jit"),
            (Strategy::JitArb, "jit_arb"),
            (Strategy::Sandwich, "sandwich"),
            (Strategy::Liquidation, "liquidation"),
            (Strategy::CrossBlockArb, "cross_block_arb"),
            (Strategy::TimeBandit, "time_bandit"),
        ] {
            assert_eq!(s.to_string(), *expected);
            assert_eq!(expected.parse::<Strategy>().unwrap(), *s);
        }
    }

    #[test]
    fn test_strategy_unknown() {
        assert!("unknown_strat".parse::<Strategy>().unwrap_err().contains("unknown strategy"));
    }

    #[test]
    fn test_strategy_from_comma_list_single() {
        let v = Strategy::from_comma_list("two_hop_arb").unwrap();
        assert_eq!(v, vec![Strategy::TwoHopArb]);
    }

    #[test]
    fn test_strategy_from_comma_list_all() {
        let v = Strategy::from_comma_list("all").unwrap();
        assert_eq!(v, Strategy::all());
    }

    #[test]
    fn test_strategy_all_static() {
        assert_eq!(Strategy::all().len(), 8);
    }

    #[test]
    fn test_range_mode_display() {
        assert_eq!(RangeMode::Days(7).to_string(), "last 7 days");
        assert_eq!(RangeMode::Blocks(100).to_string(), "last 100 blocks");
        assert_eq!(RangeMode::Single(42).to_string(), "single block #42");
        assert_eq!(RangeMode::Range(10, 20).to_string(), "blocks 10–20 (11 blocks)");
    }

    #[test]
    fn test_range_mode_resolve_description() {
        assert!(RangeMode::Days(1).resolve_description().contains("binary search"));
        assert!(RangeMode::Blocks(1).resolve_description().contains("chain tip"));
        assert_eq!(RangeMode::Single(5).resolve_description(), "single block mode");
        assert!(RangeMode::Range(1, 10).resolve_description().contains("blocks 1–10"));
    }

    #[test]
    fn test_gas_model_roundtrip() {
        for m in &[GasModel::HistoricalExact, GasModel::P90, GasModel::Fixed] {
            let s = m.to_string();
            assert_eq!(s.parse::<GasModel>().unwrap(), *m);
        }
    }

    #[test]
    fn test_gas_model_distribution_parse() {
        let m: GasModel = "distribution_90".parse().unwrap();
        assert_eq!(m, GasModel::Distribution(90));
        assert_eq!(m.to_string(), "distribution_90");
        assert_eq!(m.target_percentile(), Some(90));

        let m: GasModel = "distribution_50".parse().unwrap();
        assert_eq!(m, GasModel::Distribution(50));
        assert_eq!(m.target_percentile(), Some(50));

        assert!("distribution_0".parse::<GasModel>().is_err());
        assert!("distribution_100".parse::<GasModel>().is_err());
    }

    #[test]
    fn test_gas_model_unknown() {
        assert!("foo".parse::<GasModel>().unwrap_err().contains("unknown gas model"));
    }

    #[test]
    fn test_output_format_roundtrip() {
        for f in &[OutputFormat::Table, OutputFormat::Csv, OutputFormat::Json] {
            let s = f.to_string();
            assert_eq!(s.parse::<OutputFormat>().unwrap(), *f);
        }
    }

    #[test]
    fn test_output_format_unknown() {
        assert!("xml".parse::<OutputFormat>().unwrap_err().contains("unknown output format"));
    }

    #[test]
    fn test_gas_config_with_limit_historical_exact() {
        let cfg = GasConfig::default();
        let cost = cfg.compute_gas_cost_with_limit(80_000, 50_000_000_000);
        assert_eq!(cost, 80_000u128 * 50_000_000_000);
    }

    #[test]
    fn test_gas_config_priority_fee_with_limit() {
        let cfg = GasConfig {
            priority_fee_gwei: 2.0,
            ..GasConfig::default()
        };
        let cost = cfg.compute_gas_cost_with_limit(80_000, 50_000_000_000u128);
        assert_eq!(cost, 80_000u128 * 52_000_000_000u128);
    }

    #[test]
    fn test_gas_config_fixed_model_with_limit() {
        let cfg = GasConfig {
            gas_model: GasModel::Fixed,
            priority_fee_gwei: 3.0,
            ..GasConfig::default()
        };
        let cost = cfg.compute_gas_cost_with_limit(80_000, 50_000_000_000u128);
        assert_eq!(cost, 80_000u128 * 3_000_000_000u128);
    }

    #[test]
    fn test_gas_config_p90_model_with_limit() {
        let cfg = GasConfig {
            gas_model: GasModel::P90,
            priority_fee_gwei: 1.0,
            ..GasConfig::default()
        };
        let cost = cfg.compute_gas_cost_with_limit(80_000, 50_000_000_000u128);
        assert_eq!(cost, 80_000u128 * 76_000_000_000u128);
    }

    #[test]
    fn test_flash_loan_fee() {
        let cfg_no_fee = GasConfig::default();
        assert_eq!(cfg_no_fee.flash_loan_fee(1_000_000), 0);
        let cfg_aave = GasConfig { flash_loan_provider: FlashLoanProvider::Aave, ..GasConfig::default() };
        assert_eq!(cfg_aave.flash_loan_fee(1_000_000), 900); // 0.09% of 1M
        let cfg_uni = GasConfig { flash_loan_provider: FlashLoanProvider::Uniswap, ..GasConfig::default() };
        assert_eq!(cfg_uni.flash_loan_fee(1_000_000), 1000); // 0.10% of 1M
    }

    #[test]
    fn test_gas_config_p90_with_percentile_price() {
        let cfg = GasConfig {
            gas_model: GasModel::P90,
            priority_fee_gwei: 0.0,
            percentile_gas_price: Some(80_000_000_000u128), // 80 gwei
            ..GasConfig::default()
        };
        // Uses percentile_gas_price (80 gwei) instead of base_fee * 150%
        let cost = cfg.compute_gas_cost_with_limit(100_000, 50_000_000_000u128);
        assert_eq!(cost, 100_000u128 * 80_000_000_000u128);
    }

    #[test]
    fn test_gas_config_distribution_model() {
        let cfg = GasConfig {
            gas_model: GasModel::Distribution(75),
            priority_fee_gwei: 1.0,
            percentile_gas_price: Some(100_000_000_000u128),
            ..GasConfig::default()
        };
        // Uses percentile + priority fee
        let cost = cfg.compute_gas_cost_with_limit(100_000, 50_000_000_000u128);
        assert_eq!(cost, 100_000u128 * 101_000_000_000u128);
    }

    #[test]
    fn test_gas_config_distribution_fallback_no_percentile() {
        let cfg = GasConfig {
            gas_model: GasModel::Distribution(90),
            priority_fee_gwei: 0.0,
            percentile_gas_price: None,
            ..GasConfig::default()
        };
        // Falls back to base_fee * 150% when no distribution data
        let cost = cfg.compute_gas_cost_with_limit(100_000, 50_000_000_000u128);
        assert_eq!(cost, 100_000u128 * 75_000_000_000u128);
    }
}
