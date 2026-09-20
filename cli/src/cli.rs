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

    /// Re-render a recorded run from SQLite (run_manifests + explorer opportunities)
    Report(ReportArgs),

    /// Print the fully resolved config as TOML
    Config,

    /// Discover pools from on-chain factory events and/or remote aggregators.
    /// Factory addresses are resolved from the chain config.
    /// Found pools are printed to stdout and saved to the local cache.
    Discover(DiscoverArgs),

    /// Discover and cache token metadata.
    /// Uses the bundled known-token list, SQLite cache, optional DefiLlama
    /// coins (symbol/decimals) and CoinGecko contract (name/icon URL) via
    /// `--enrich`. Populates the token cache used by pool discovery.
    Tokens(TokensArgs),

    /// Stream blocks in real-time, detecting MEV opportunities as they arrive.
    /// Processes new blocks via log-based pool state updates (arb strategies)
    /// with optional full EVM replay for complete detection.
    Live(LiveArgs),

    /// Realized-MEV explorer: forensic reconstruction of extracted MEV from
    /// raw chain data (index, stats, op detail).
    Explorer(ExplorerArgs),
}

/// Explorer subcommand group.
#[derive(Subcommand, Debug, Clone)]
pub enum ExplorerCommand {
    /// Stream-index tip blocks into the explorer store (live only).
    /// Idempotent, resumable, reorg-aware; classify-in-stream.
    Index(IndexArgs),

    /// Overview: op counts per kind, profit totals, daily breakdown, top
    /// searchers/pools. Pure SQL over the store.
    Stats(StatsArgs),

    /// Operation detail for a tx hash; --trace recomputes exact profit via
    /// debug_traceTransaction (prestateTracer diffMode).
    Show(ShowArgs),
}

#[derive(Args, Debug, Clone)]
pub struct ExplorerArgs {
    #[command(subcommand)]
    pub command: ExplorerCommand,
}

#[derive(Args, Debug, Clone)]
pub struct IndexArgs {
    /// Stop live indexing after this duration (e.g. 90s, 15m, 1h)
    #[arg(long, value_name = "DURATION")]
    pub duration: Option<String>,
}

#[derive(Args, Debug, Clone)]
pub struct StatsArgs {
    /// Time window: 1d|7d|30d|all (default all)
    #[arg(long, value_name = "WINDOW")]
    pub since: Option<String>,

    /// Filter to one kind
    #[arg(long, value_name = "KIND")]
    pub kind: Option<String>,
}

#[derive(Args, Debug, Clone)]
pub struct ShowArgs {
    /// Transaction hash
    #[arg(value_name = "TX_HASH")]
    pub tx_hash: String,

    /// On-demand debug_traceTransaction (prestateTracer diffMode) verification
    #[arg(long)]
    pub trace: bool,
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

impl TryFrom<&BlockRangeArgs> for mev_scout_core::config::validation::RangeSpec {
    type Error = mev_scout_core::error::ConfigError;

    fn try_from(a: &BlockRangeArgs) -> Result<Self, Self::Error> {
        Self::from_flags(a.days, a.blocks, a.block, a.from_block, a.to_block)
    }
}

#[derive(Args, Debug, Clone)]
pub struct RunArgs {
    #[command(flatten)]
    pub block_range: BlockRangeArgs,
}

#[derive(Args, Debug, Clone)]
pub struct ReportArgs {
    /// Specific run ID to report (default: latest)
    #[arg(long, value_name = "ID")]
    pub run_id: Option<String>,
}

#[derive(Args, Debug, Clone)]
pub struct DiscoverArgs {
    #[command(flatten)]
    pub block_range: BlockRangeArgs,

    /// Resume from the latest cached block instead of the full range.
    /// Queries the cache for the highest creation_block and scans from there.
    #[arg(long)]
    pub incremental: bool,

    /// Pool source: onchain (RPC events only), remote (GeckoTerminal
    /// aggregator only), or hybrid (union of both, deduped by address).
    /// Default onchain — zero behavior change.
    #[arg(long, default_value = "onchain", value_name = "SOURCE")]
    pub source: DiscoverySource,

    /// Attach tvl_usd / volume_usd_24h / volume_usd_30d to discovered pools
    /// from the free GeckoTerminal aggregator. Implies one remote fetch.
    #[arg(long)]
    pub enrich: bool,
}

/// Pool discovery source selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum DiscoverySource {
    Onchain,
    Remote,
    Hybrid,
}

#[derive(Args, Debug, Clone)]
pub struct TokensArgs {
    /// Only populate / report cache size; skip detailed listing
    #[arg(long)]
    pub cache_only: bool,

    /// Enrich missing fields via DefiLlama coins (symbol/decimals) and
    /// CoinGecko contract API (name + icon URL). Offline by default.
    #[arg(long)]
    pub enrich: bool,
}

#[derive(Args, Debug, Clone)]
pub struct LiveArgs {
    /// Continuously poll and process new blocks until Ctrl+C
    #[arg(long, help_heading = "Live")]
    pub r#loop: bool,

    /// Stop continuous polling after this duration (requires --loop).
    /// Accepts humantime suffixes: 90s, 15m, 1h, 1h 30m.
    #[arg(long = "duration", value_name = "DURATION", help_heading = "Live")]
    pub duration: Option<String>,

    /// Stop continuous polling after processing this many blocks
    /// (requires --loop).
    #[arg(long = "max-blocks", value_name = "NUMBER", help_heading = "Live")]
    pub max_blocks: Option<u64>,
}
