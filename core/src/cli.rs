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

    /// Discover pools from factory events via the RPC endpoint.
    /// Found pools are printed to stdout and optionally saved to the sled cache.
    Discover(DiscoverArgs),

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
}

#[derive(Args, Debug, Clone)]
pub struct RunArgs {
    #[command(flatten)]
    pub block_range: BlockRangeArgs,

    #[command(flatten)]
    pub chain_args: ChainArgs,

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

    /// Block/state cache directory
    #[arg(long, default_value = "./cache", value_name = "PATH", help_heading = "Output")]
    pub cache_dir: String,
}

#[derive(Args, Debug, Clone)]
pub struct FetchArgs {
    #[command(flatten)]
    pub block_range: BlockRangeArgs,

    #[command(flatten)]
    pub chain_args: ChainArgs,

    /// Block/state cache directory
    #[arg(long, default_value = "./cache", value_name = "PATH")]
    pub cache_dir: String,
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

    /// Block/state cache directory
    #[arg(long, default_value = "./cache", value_name = "PATH")]
    pub cache_dir: String,
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

    /// Uniswap V2 factory addresses (comma-separated)
    #[arg(long, value_name = "ADDRS")]
    pub v2_factories: Option<String>,

    /// Uniswap V3 factory address
    #[arg(long, value_name = "ADDR")]
    pub v3_factory: Option<String>,

    /// Start block for discovery scan
    #[arg(long, value_name = "NUMBER")]
    pub from_block: u64,

    /// End block for discovery scan (inclusive)
    #[arg(long, value_name = "NUMBER")]
    pub to_block: u64,

    /// Batch size for each getLogs request
    #[arg(long, default_value = "10", value_name = "NUMBER")]
    pub batch_size: u64,

    /// Save discovered pools to the sled cache
    #[arg(long)]
    pub save: bool,

    /// Block/state cache directory (used when --save is set)
    #[arg(long, default_value = "./cache", value_name = "PATH")]
    pub cache_dir: String,
}
