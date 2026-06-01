use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ChainName {
    Polygon,
    Avalanche,
    Bsc,
    Arbitrum,
    Base,
    Ethereum,
    Optimism,
}

impl ChainName {
    pub fn chain_id(self) -> u64 {
        match self {
            ChainName::Polygon => 137,
            ChainName::Avalanche => 43114,
            ChainName::Bsc => 56,
            ChainName::Arbitrum => 42161,
            ChainName::Base => 8453,
            ChainName::Ethereum => 1,
            ChainName::Optimism => 10,
        }
    }

    /// Public (free-tier) RPC endpoint — no API key required.
    pub fn public_rpc_url(&self) -> &'static str {
        match self {
            ChainName::Polygon => "https://polygon-bor.publicnode.com",
            ChainName::Avalanche => "https://avalanche-c-chain.publicnode.com",
            ChainName::Bsc => "https://bsc.publicnode.com",
            ChainName::Arbitrum => "https://arbitrum-one.publicnode.com",
            ChainName::Base => "https://base.publicnode.com",
            ChainName::Ethereum => "https://ethereum-rpc.publicnode.com",
            ChainName::Optimism => "https://optimism-rpc.publicnode.com",
        }
    }

    pub fn all() -> &'static [ChainName] {
        &[
            ChainName::Polygon,
            ChainName::Avalanche,
            ChainName::Bsc,
            ChainName::Arbitrum,
            ChainName::Base,
            ChainName::Ethereum,
            ChainName::Optimism,
        ]
    }
}

impl fmt::Display for ChainName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChainName::Polygon => write!(f, "polygon"),
            ChainName::Avalanche => write!(f, "avalanche"),
            ChainName::Bsc => write!(f, "bsc"),
            ChainName::Arbitrum => write!(f, "arbitrum"),
            ChainName::Base => write!(f, "base"),
            ChainName::Ethereum => write!(f, "ethereum"),
            ChainName::Optimism => write!(f, "optimism"),
        }
    }
}

impl FromStr for ChainName {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "polygon" => Ok(ChainName::Polygon),
            "avalanche" => Ok(ChainName::Avalanche),
            "bsc" => Ok(ChainName::Bsc),
            "arbitrum" => Ok(ChainName::Arbitrum),
            "base" => Ok(ChainName::Base),
            "ethereum" => Ok(ChainName::Ethereum),
            "optimism" => Ok(ChainName::Optimism),
            _ => Err(format!(
                "unknown chain '{s}'. Supported: {}",
                ChainName::all()
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }
}

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
}

impl fmt::Display for Strategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Strategy::TwoHopArb => write!(f, "two_hop_arb"),
            Strategy::MultiHopArb => write!(f, "multi_hop_arb"),
            Strategy::Jit => write!(f, "jit"),
            Strategy::JitArb => write!(f, "jit_arb"),
            Strategy::Sandwich => write!(f, "sandwich"),
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
            _ => Err(format!(
                "unknown strategy '{s}'. Supported: two_hop_arb, multi_hop_arb, jit, jit_arb, sandwich, all"
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GasModel {
    #[serde(rename = "historical_exact")]
    HistoricalExact,
    #[serde(rename = "p90")]
    P90,
    #[serde(rename = "fixed")]
    Fixed,
}

impl fmt::Display for GasModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GasModel::HistoricalExact => write!(f, "historical_exact"),
            GasModel::P90 => write!(f, "p90"),
            GasModel::Fixed => write!(f, "fixed"),
        }
    }
}

impl FromStr for GasModel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "historical_exact" => Ok(GasModel::HistoricalExact),
            "p90" => Ok(GasModel::P90),
            "fixed" => Ok(GasModel::Fixed),
            _ => Err(format!(
                "unknown gas model '{s}'. Supported: historical_exact, p90, fixed"
            )),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Run,
    Fetch,
    Report,
    Config,
    Replay,
}
