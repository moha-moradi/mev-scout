//! CLI argument parsing via clap, defining the command-line interface for mev-scout.

use clap::{Args, Parser, Subcommand};

/// MEV Scout — MEV opportunity scanner & backtester for EVM-compatible chains.
#[derive(Parser, Debug)]
#[command(name = "mev-scout", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Path to TOML config file
    #[arg(global = true, short = 'f', long = "config", value_name = "FILE")]
    pub config: Option<String>,

    /// Enable debug-level logging
    #[arg(global = true, short, long)]
    pub verbose: bool,

    /// Suppress all output except the final summary
    #[arg(global = true, long)]
    pub quiet: bool,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Execute the full backtest
    Run(RunArgs),

    /// Pre-cache block data without running strategies
    Fetch(FetchArgs),

    /// Re-render terminal tables from saved JSON
    Report(ReportArgs),

    /// Print the fully resolved config as TOML
    Config,

    /// Replay a specific block for debugging
    Replay(ReplayArgs),

    /// Discover pools from on-chain factory events.
    /// Factory addresses are resolved from the chain config.
    /// Found pools are printed to stdout and saved to the local cache.
    Discover(DiscoverArgs),

    /// Discover and cache token metadata.
    /// Uses the bundled known-token list plus lazily resolved on-chain
    /// metadata, and populates the token cache used by pool discovery
    /// to avoid RPC symbol() calls.
    Tokens(TokensArgs),

    /// Scan on-chain events (trades, transfers, flashloans, liquidations, labels).
    /// Replaces the old Dune query system with direct RPC-based log scanning.
    Scan(ScanArgs),
}

#[derive(Args, Debug, Clone)]
#[command(next_help_heading = "Block Range (exactly one required)")]
pub struct BlockRangeArgs {
    /// Last N days of blocks (1–365)
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..=365))]
    pub days: Option<u64>,

    /// Last N blocks from chain tip (≥1)
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    pub blocks: Option<u64>,

    /// Single specific block number (>0)
    #[arg(long, value_name = "NUMBER", value_parser = clap::value_parser!(u64).range(1..))]
    pub block: Option<u64>,

    /// Range start (requires --to-block)
    #[arg(long, value_name = "NUMBER")]
    pub from_block: Option<u64>,

    /// Range end (requires --from-block)
    #[arg(long, value_name = "NUMBER")]
    pub to_block: Option<u64>,
}

#[derive(Args, Debug, Clone)]
#[command(next_help_heading = "Chain & Connection")]
pub struct ChainArgs {
    /// Chain name: polygon, avalanche, bsc, arbitrum, base, ethereum, optimism
    #[arg(short = 'n', long, default_value = "polygon", value_name = "NAME")]
    pub chain: String,

    /// Archive node RPC endpoint
    #[arg(short = 'r', long = "rpc", value_name = "URL")]
    pub rpc_url: Option<String>,

    /// RPC requests per second rate limit (default: 0 = unlimited).
    /// 0 disables client-side rate limiting; servers enforce their own limits.
    #[arg(long = "rps-limit", default_value = "0.0", value_name = "RPS")]
    pub rps_limit: f64,

    /// Additional RPC URLs for multi-provider load distribution (comma-separated).
    /// Each URL is used alongside --rpc for concurrent block fetching.
    #[arg(long = "rpc-urls", value_name = "URLS", value_delimiter = ',')]
    pub rpc_urls: Option<Vec<String>>,

    /// Per-provider RPS limits, one per entry in the combined URL list (comma-separated).
    /// Maps 1:1 in order: --rpc (first), then --rpc-urls entries, then public fallbacks.
    #[arg(long = "rpc-rps", value_name = "RPS", value_delimiter = ',')]
    pub rpc_rps: Option<Vec<f64>>,

}

#[derive(Args, Debug, Clone)]
pub struct RunArgs {
    #[command(flatten)]
    pub block_range: BlockRangeArgs,

    #[command(flatten)]
    pub chain_args: ChainArgs,

    /// Concurrent blocks per provider shard. Auto-calculated from per-provider RPS
    /// when not set (recommended: omit for optimal defaults).
    #[arg(long = "block-concurrency", value_name = "N", help_heading = "Performance")]
    pub block_concurrency: Option<usize>,

    /// Flash loan provider strategy: auto, balancer, aave, uniswap
    #[arg(long, default_value = "auto", value_name = "PROVIDER", help_heading = "Flash Loan")]
    pub flash_loan_provider: String,

    /// Strategies to run: comma-separated or "all"
    #[arg(long, default_value = "all", value_name = "LIST", help_heading = "Strategies")]
    pub strategies: String,

    /// Gas price model: historical_exact, p90, fixed
    #[arg(long, default_value = "historical_exact", value_name = "MODEL", help_heading = "Gas Model")]
    pub gas_model: String,

    /// Gas limit for arb transaction cost estimation
    #[arg(long, default_value_t = 200_000, value_name = "GAS", help_heading = "Gas Model", value_parser = clap::value_parser!(u64).range(1..))]
    pub gas_limit: u64,

    /// Priority fee premium in gwei (added on top of base fee)
    #[arg(long, default_value_t = 1.0, value_name = "GWEI", help_heading = "Gas Model")]
    pub priority_fee: f64,

    /// Output format: table, csv, json
    #[arg(long, default_value = "table", value_name = "FORMAT", help_heading = "Output")]
    pub output: String,

    /// Directory for CSV/JSON exports
    #[arg(long, default_value = "./results", value_name = "PATH", help_heading = "Output")]
    pub export_path: String,

    /// SQLite database path (defaults to config's db_path or ./cache)
    #[arg(long = "db-path", value_name = "PATH", help_heading = "Output")]
    pub db_path: Option<String>,

    /// Enable JSON-RPC batching (send block+receipts in one HTTP POST).
    /// Disabled by default — separate parallel requests often achieve better throughput.
    #[arg(long = "batch-rpc", help_heading = "RPC")]
    pub batch_rpc: bool,

    /// Parquet directory (optional, unset = no Parquet output)
    #[arg(long = "parquet-dir", value_name = "PATH", help_heading = "Output")]
    pub parquet_dir: Option<String>,

    /// Print detailed fact-check report after the run
    #[arg(long, help_heading = "Output")]
    pub fact_check: bool,

    /// Use EVM-based fact-check (re-fetches pool state from chain via eth_call).
    /// Requires --fact-check. Catches detection bugs that structural check misses.
    #[arg(long, help_heading = "Output")]
    pub evm_fact_check: bool,

    /// Price oracle mode: coingecko, onchain, or hybrid (default: coingecko)
    #[arg(long = "price-oracle", default_value = "coingecko", value_name = "MODE", help_heading = "Pricing")]
    pub price_oracle_mode: String,

    /// Per-token USD prices as comma-separated ADDR=price pairs (e.g. "0x...=0.999,0x...=1800")
    #[arg(long = "token-price", value_name = "PAIRS", help_heading = "Pricing")]
    pub token_prices: Option<String>,

    /// Proximity window (in tx indices) for JitArb detection (default: 3).
    #[arg(long = "proximity-window", default_value = "3", value_name = "N", help_heading = "Strategies")]
    pub proximity_window: usize,

    /// Capture pending transactions from the mempool (default: false).
    /// Fetches the current pending block via eth_getBlockByNumber("pending")
    /// after processing each block range and logs the pending tx count.
    #[arg(long = "capture-pending", help_heading = "Mempool")]
    pub capture_pending: bool,

}

#[derive(Args, Debug, Clone)]
pub struct FetchArgs {
    #[command(flatten)]
    pub block_range: BlockRangeArgs,

    #[command(flatten)]
    pub chain_args: ChainArgs,

    /// Concurrent blocks per provider shard. Auto-calculated from per-provider RPS
    /// when not set (recommended: omit for optimal defaults).
    #[arg(long = "block-concurrency", value_name = "N", help_heading = "Performance")]
    pub block_concurrency: Option<usize>,

    /// SQLite database path (defaults to config's db_path or ./cache)
    #[arg(long = "db-path", value_name = "PATH")]
    pub db_path: Option<String>,

    /// Enable JSON-RPC batching (send block+receipts in one HTTP POST).
    /// Disabled by default — separate parallel requests often achieve better throughput.
    #[arg(long = "batch-rpc")]
    pub batch_rpc: bool,

    /// Skip 4-byte signature resolution (much faster, no 4byte.directory API calls)
    #[arg(long = "no-sig-resolve")]
    pub no_sig_resolve: bool,

    /// Parquet directory (optional, unset = no Parquet output)
    #[arg(long = "parquet-dir", value_name = "PATH")]
    pub parquet_dir: Option<String>,
}

#[derive(Args, Debug, Clone)]
pub struct ReplayArgs {
    /// Block number to replay (required)
    #[arg(long, required = true, value_name = "NUMBER")]
    pub block: u64,

    /// Replay up to this tx index (default: all)
    #[arg(long, value_name = "INDEX")]
    pub tx_index: Option<usize>,

    #[command(flatten)]
    pub chain_args: ChainArgs,

    /// SQLite database path (defaults to config's db_path or ./cache)
    #[arg(long = "db-path", value_name = "PATH")]
    pub db_path: Option<String>,

    /// Parquet directory (optional, unset = no Parquet output)
    #[arg(long = "parquet-dir", value_name = "PATH")]
    pub parquet_dir: Option<String>,

    /// Show DEX interaction analysis per transaction
    #[arg(long)]
    pub analyze: bool,
}

#[derive(Args, Debug, Clone)]
pub struct ReportArgs {
    /// Specific run ID to report (default: latest)
    #[arg(long, value_name = "ID")]
    pub run_id: Option<String>,

    /// Output format: table, csv, json
    #[arg(long, default_value = "table", value_name = "FORMAT")]
    pub output: String,

    /// Directory where result files are stored
    #[arg(long, default_value = "./results", value_name = "PATH")]
    pub export_path: String,
}

#[derive(Args, Debug, Clone)]
pub struct DiscoverArgs {
    #[command(flatten)]
    pub chain_args: ChainArgs,

    #[command(flatten)]
    pub block_range: BlockRangeArgs,

    /// Batch size for each getLogs request (default: 500, safe for public RPCs)
    #[arg(long, default_value = "500", value_name = "NUMBER")]
    pub batch_size: u64,

    /// SQLite database path (overrides config's default: ./cache/{chain}-mev-scout.sqlite)
    #[arg(long = "db-path", value_name = "PATH")]
    pub db_path: Option<String>,

    /// Output discovered pools as JSON instead of human-readable tables
    #[arg(long)]
    pub json: bool,

    /// Max concurrent RPC calls during pool metadata fetch (default: 8, safe for public RPCs)
    #[arg(long = "rpc-concurrency", default_value = "8", value_name = "NUMBER")]
    pub rpc_concurrency: usize,

    /// Resume from the latest cached block instead of the full range.
    /// Queries the cache for the highest creation_block and scans from there.
    #[arg(long)]
    pub incremental: bool,

    /// Run a post-discovery health check that queries on-chain state to filter
    /// out drained (zero-reserve) pools. Enabled by default.
    #[arg(long, default_value = "true", value_name = "BOOL")]
    pub health_check: bool,

    /// Solidly-style pool fee in basis points (default: 30).
    /// Overrides v2_fee_override for Solidly/Velodrome/Aerodrome pools.
    #[arg(long = "solidly-fee-bps", value_name = "BPS")]
    pub solidly_fee_bps: Option<u32>,
}

#[derive(Args, Debug, Clone)]
pub struct TokensArgs {
    #[command(flatten)]
    pub chain_args: ChainArgs,

    /// Token filter: all (fast), active, new, tvl
    #[arg(long, default_value = "all", value_name = "FILTER")]
    pub filter: String,

    /// Look-back period in days (for active/new/tvl filters)
    #[arg(long, default_value = "7", value_name = "N")]
    pub days: u64,

    /// Top N tokens (tvl filter only)
    #[arg(long, default_value = "50", value_name = "N")]
    pub top: usize,

    /// Minimum USD trade volume threshold (filters out low-volume tokens)
    #[arg(long, value_name = "USD")]
    pub min_volume: Option<f64>,

    /// Minimum USD TVL threshold (tvl filter only)
    #[arg(long, default_value = "1000", value_name = "USD")]
    pub min_tvl: f64,

    /// Sort results by: trades (default), volume, tvl, symbol, name
    #[arg(long, default_value = "trades", value_name = "FIELD")]
    pub sort: String,

    /// Filter by symbol pattern (case-insensitive substring match)
    #[arg(long, value_name = "PATTERN")]
    pub symbol: Option<String>,

    /// Filter by exact decimals value
    #[arg(long, value_name = "N")]
    pub decimals: Option<u8>,

    /// Maximum tokens to display (default: 100)
    #[arg(long, default_value = "100", value_name = "N")]
    pub limit: usize,

    /// Output format: table, json, csv
    #[arg(long, default_value = "table", value_name = "FORMAT")]
    pub output: String,

    /// Only populate SQLite cache, don't display results
    #[arg(long)]
    pub cache_only: bool,

    /// Skip cache persistence (display only)
    #[arg(long)]
    pub no_cache: bool,
}

/// Event scan kind — determines which event topics to scan for.
#[derive(Debug, Clone, PartialEq, Eq, clap::ValueEnum)]
pub enum ScanKind {
    /// DEX swap events (V2/V3/Algebra/Solidly/Curve)
    Trades,
    /// ERC-20 Transfer events (whale detection)
    Transfers,
    /// Flash loan events (Aave V2/V3, Balancer V2, Uniswap V3)
    Flashloans,
    /// Liquidation events (Aave V3, Compound V3)
    Liquidations,
    /// Address label lookup
    Labels,
}

impl std::fmt::Display for ScanKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScanKind::Trades => write!(f, "trades"),
            ScanKind::Transfers => write!(f, "transfers"),
            ScanKind::Flashloans => write!(f, "flashloans"),
            ScanKind::Liquidations => write!(f, "liquidations"),
            ScanKind::Labels => write!(f, "labels"),
        }
    }
}

#[derive(Args, Debug, Clone)]
pub struct ScanArgs {
    #[command(flatten)]
    pub chain_args: ChainArgs,

    #[command(flatten)]
    pub block_range: BlockRangeArgs,

    /// What to scan for
    #[arg(long, value_enum, default_value = "trades")]
    pub kind: ScanKind,

    /// Filter by contract address (reusable)
    #[arg(long = "address", value_name = "ADDRESS")]
    pub addresses: Option<Vec<String>>,

    /// Output format: table, json, csv
    #[arg(long, default_value = "table", value_name = "FORMAT")]
    pub output: String,

    /// Maximum results to display (0 = unlimited)
    #[arg(long, default_value = "500", value_name = "N")]
    pub limit: usize,

    /// Batch size for eth_getLogs requests (default: 500)
    #[arg(long, default_value = "500", value_name = "N")]
    pub batch_size: u64,

    /// Minimum transfer value for whale detection (transfers only)
    #[arg(long, value_name = "WEI")]
    pub min_value: Option<String>,
}
