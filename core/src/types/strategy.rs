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
            _ => Err(format!(
                "unknown strategy '{s}'. Supported: two_hop_arb, multi_hop_arb, jit, jit_arb, sandwich, liquidation, cross_block_arb, all"
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
            "p90" => Ok(GasModel::Distribution(90)),
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
                    "unknown gas model '{s}'. Supported: historical_exact, fixed, live, distribution_N (1-99)"
                ))
            }
        }
    }
}

impl GasModel {
    /// Return the target percentile for this gas model.
    /// For `Distribution(p)` returns p. For `HistoricalExact` and `Fixed` returns `None`.
    pub fn target_percentile(&self) -> Option<u8> {
        match self {
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
    pub winning_bid_premium: f64,
    /// Pre-computed N-th percentile effective gas price from the historical
    /// gas price distribution (H10). When set, `GasModel::Distribution(p)`
    /// uses this value instead of the crude
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
    /// For `GasModel::Distribution(p)`, uses the
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
            GasModel::Distribution(_) => {
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

