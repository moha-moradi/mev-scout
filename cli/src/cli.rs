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

    /// Discover pools from on-chain events and/or Dune Analytics.
    /// Factory addresses are resolved from the chain config.
    /// Found pools are printed to stdout and saved to the local cache.
    Discover(DiscoverArgs),

    /// Live mode — connect to the live chain and run as a virtual MEV bot
    Live(LiveArgs),

    /// Audit a previous run against Dune Analytics data.
    /// Compares MEV Scout's detected opportunities with Dune's curated
    /// datasets (dex.sandwiches, dex.trades, etc.). Requires configured
    /// Dune query IDs in the config file.
    Audit(AuditArgs),

    /// Query Dune Analytics for Uniswap V2/V3 trade counts in a block.
    /// Executes raw SQL against Dune's dex.trades dataset and prints
    /// per-project transaction and swap counts for the given block.
    DuneCheck(DuneCheckArgs),
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

    /// Number of concurrent RPC workers (default: 1).
    /// Keep low (1-3) for public RPCs. Increase (10-20) for private RPCs.
    #[arg(long = "rpc-workers", default_value = "1", value_name = "N")]
    pub rpc_workers: usize,

    /// RPC requests per second rate limit (default: 1). 0 = unlimited.
    #[arg(long = "rps-limit", default_value = "1.0", value_name = "RPS")]
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

    /// Concurrent blocks to fetch within a single contiguous range.
    /// Higher values pipeline RPC requests for better throughput.
    /// The RPS limiter still applies across all concurrent workers.
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
    #[arg(long, default_value_t = 0.0, value_name = "GWEI", help_heading = "Gas Model")]
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

    /// Disable JSON-RPC batching (fetch block+receipts in separate calls instead of one)
    #[arg(long = "no-batch-rpc", help_heading = "RPC")]
    pub no_batch_rpc: bool,

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

    /// Cross-block MEV detection window size (default: 0 = disabled).
    /// When > 1, tracks pool price snapshots across consecutive blocks and
    /// emits persistent arb (CrossBlockArb) and time-bandit (TimeBandit)
    /// opportunities. Requires at least 2 blocks in the range.
    #[arg(long = "cross-block-window", default_value = "0", value_name = "N", help_heading = "Strategies")]
    pub cross_block_window: usize,

}

#[derive(Args, Debug, Clone)]
pub struct FetchArgs {
    #[command(flatten)]
    pub block_range: BlockRangeArgs,

    #[command(flatten)]
    pub chain_args: ChainArgs,

    /// Concurrent blocks to fetch within a single contiguous range.
    /// Higher values pipeline RPC requests for better throughput.
    /// The RPS limiter still applies across all concurrent workers.
    #[arg(long = "block-concurrency", value_name = "N", help_heading = "Performance")]
    pub block_concurrency: Option<usize>,

    /// SQLite database path (defaults to config's db_path or ./cache)
    #[arg(long = "db-path", value_name = "PATH")]
    pub db_path: Option<String>,

    /// Disable JSON-RPC batching (fetch block+receipts in separate calls instead of one)
    #[arg(long = "no-batch-rpc")]
    pub no_batch_rpc: bool,

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
pub struct LiveArgs {
    #[command(flatten)]
    pub chain_args: ChainArgs,

    /// Starting virtual balance in native token (e.g., 10.0 ETH)
    #[arg(long = "initial-balance", default_value = "10.0", value_name = "AMOUNT", help_heading = "Wallet")]
    pub initial_balance: f64,

    /// Minimum profit in native token to execute a virtual trade
    #[arg(long = "min-profit", default_value = "0.001", value_name = "AMOUNT", help_heading = "Wallet")]
    pub min_profit: f64,

    /// How often (ms) to poll the mempool
    #[arg(long = "poll-interval", default_value_t = 1000, value_name = "MS", help_heading = "Mempool")]
    pub poll_interval: u64,

    /// Maximum number of virtual executions before auto-stop (default: unlimited)
    #[arg(long = "max-executions", value_name = "N", help_heading = "Wallet")]
    pub max_executions: Option<u64>,

    /// Detection strategies (comma-separated)
    #[arg(long, default_value = "two_hop_arb,multi_hop_arb", value_name = "LIST", help_heading = "Strategies")]
    pub strategies: String,

    /// Gas limit per virtual trade
    #[arg(long = "gas-limit", default_value_t = 200_000, value_name = "GAS", help_heading = "Gas Model")]
    pub gas_limit: u64,

    /// Priority fee in gwei
    #[arg(long = "priority-fee", default_value_t = 1.0, value_name = "GWEI", help_heading = "Gas Model")]
    pub priority_fee: f64,

    /// Gas model: live (fetch from chain) or fixed (use --priority-fee only)
    #[arg(long = "gas-model", default_value = "live", value_name = "MODEL", help_heading = "Gas Model")]
    pub gas_model: String,

    /// Number of poll cycles between full pool state resyncs
    #[arg(long = "resync-interval", default_value_t = 60, value_name = "N", help_heading = "Mempool")]
    pub resync_interval: u64,

    /// Price oracle mode: coingecko, onchain, or hybrid (only used for dashboard)
    #[arg(long = "price-oracle", default_value = "coingecko", value_name = "MODE", help_heading = "Pricing")]
    pub price_oracle_mode: String,

    /// Per-token USD prices (only used for dashboard)
    #[arg(long = "token-price", value_name = "PAIRS", help_heading = "Pricing")]
    pub token_prices: Option<String>,

    /// Output directory for execution logs
    #[arg(long = "export-path", default_value = "./results", value_name = "PATH", help_heading = "Output")]
    pub export_path: String,

    /// Path to a recorded pending-tx JSON file for offline replay (disables live RPC polling)
    #[arg(long = "replay-file", value_name = "PATH", help_heading = "Mempool")]
    pub replay_file: Option<String>,

    /// SQLite database path (defaults to config's db_path or ./cache)
    #[arg(long = "db-path", value_name = "PATH", help_heading = "Output")]
    pub db_path: Option<String>,

}

#[derive(Args, Debug, Clone)]
pub struct DiscoverArgs {
    #[command(flatten)]
    pub chain_args: ChainArgs,

    #[command(flatten)]
    pub block_range: BlockRangeArgs,

    /// Batch size for each getLogs request
    #[arg(long, default_value = "10", value_name = "NUMBER")]
    pub batch_size: u64,

    /// SQLite database path (overrides config's default: ./cache/{chain}-mev-scout.sqlite)
    #[arg(long = "db-path", value_name = "PATH")]
    pub db_path: Option<String>,

    /// Pool discovery source: onchain (event logs), dune (Dune Analytics), or all (merge both).
    /// Requires configured dune_api_key and query IDs in config for "dune" or "all" sources.
    #[arg(long, default_value = "onchain", value_name = "SOURCE")]
    pub source: String,
}

#[derive(Args, Debug, Clone)]
pub struct AuditArgs {
    #[command(flatten)]
    pub chain_args: ChainArgs,

    /// Start block for audit range
    #[arg(long, value_name = "NUMBER")]
    pub from_block: u64,

    /// End block for audit range (inclusive)
    #[arg(long, value_name = "NUMBER")]
    pub to_block: u64,

    /// Run ID from a previous run to compare against Dune.
    /// If provided, loads saved opportunities instead of running detection again.
    #[arg(long, value_name = "RUN_ID")]
    pub run_id: Option<String>,

    /// Path to results file (alternative to --run-id).
    #[arg(long, value_name = "PATH")]
    pub results_file: Option<String>,
}

#[derive(Args, Debug, Clone)]
pub struct DuneCheckArgs {
    /// Block number to check for Uniswap V2/V3 trades
    #[arg(short = 'b', long = "block", required = true, value_name = "NUMBER")]
    pub block: u64,

    /// Chain name (default: polygon)
    #[arg(short = 'n', long = "chain", default_value = "polygon", value_name = "NAME")]
    pub chain: String,

    /// Dune API key (overrides config file)
    #[arg(long = "dune-api-key", value_name = "KEY")]
    pub dune_api_key: Option<String>,
}
