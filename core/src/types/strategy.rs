use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, strum::Display, strum::EnumString)]
#[strum(ascii_case_insensitive)]
pub enum FlashLoanProvider {
    #[strum(serialize = "auto")]
    Auto,
    #[strum(serialize = "balancer")]
    Balancer,
    #[strum(serialize = "aave")]
    Aave,
    #[strum(serialize = "uniswap")]
    Uniswap,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, strum::Display, strum::EnumString)]
#[strum(ascii_case_insensitive)]
pub enum Strategy {
    #[strum(serialize = "two_hop_arb")]
    TwoHopArb,
    #[strum(serialize = "multi_hop_arb")]
    MultiHopArb,
    #[strum(serialize = "jit")]
    Jit,
    #[strum(serialize = "jit_arb")]
    JitArb,
    #[strum(serialize = "sandwich")]
    Sandwich,
    #[strum(serialize = "liquidation")]
    Liquidation,
    #[strum(serialize = "cross_block_arb")]
    CrossBlockArb,
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
            .map(|part| part.trim().parse::<Strategy>().map_err(|e| e.to_string()))
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, strum::Display, strum::EnumString)]
#[strum(ascii_case_insensitive)]
pub enum PriceOracleMode {
    /// Use CoinGecko API only (default, backward compat).
    #[default]
    #[strum(serialize = "coingecko", serialize = "coingecko_only")]
    CoinGeckoOnly,
    /// Derive native token price from the highest-TVL on-chain pool.
    #[strum(serialize = "onchain", serialize = "on_chain")]
    OnChain,
    /// Fetch both CoinGecko and on-chain; warn if divergence >5%.
    #[strum(serialize = "hybrid")]
    Hybrid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, strum::Display, strum::EnumString)]
#[strum(ascii_case_insensitive)]
pub enum OutputFormat {
    #[serde(rename = "table")]
    #[strum(serialize = "table")]
    Table,
    #[serde(rename = "csv")]
    #[strum(serialize = "csv")]
    Csv,
    #[serde(rename = "json")]
    #[strum(serialize = "json")]
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, strum::Display)]
pub enum ExecutorType {
    #[strum(serialize = "flash_loan_arbitrage")]
    FlashLoanArbitrage,
    #[strum(serialize = "sandwich")]
    Sandwich,
    #[strum(serialize = "liquidation")]
    Liquidation,
    #[strum(serialize = "jit_liquidity")]
    JitLiquidity,
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

