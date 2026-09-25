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
    /// raw chain data (index, stats, show, report, backfill).
    Explorer(ExplorerArgs),

    /// Virtual-fund bot P&L over detected opportunities (theoretical, no competition).
    Paper(PaperArgs),
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

    /// Revenue report: cost, profit, and volume per time window (1d/7d/30d
    /// default), broken out per MEV kind, with daily trend and top-op detail.
    /// Requires the store to hold history — populate it with `backfill`.
    Report(ExplorerReportArgs),

    /// Index a historical block range into the store so the revenue-report
    /// windows (1d/7d/30d) have realized data. Idempotent + resumable.
    Backfill(ExplorerBackfillArgs),

    /// Cross-validation report: realized explorer ops vs scanner
    /// opportunities (T1/T2/T3 matching). Read-only; does no RPC.
    Validate(ExplorerValidateArgs),
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

    /// Override `[explorer] trace_tolerance_pct` for the gate verdict
    #[arg(long = "tolerance-pct", value_name = "PCT")]
    pub tolerance_pct: Option<f64>,
}

#[derive(Args, Debug, Clone)]
pub struct ExplorerReportArgs {
    /// Time windows: comma-separated 1d|7d|30d|all (default 1d,7d,30d)
    #[arg(
        long,
        value_name = "WINDOWS",
        value_delimiter = ',',
        default_value = "1d,7d,30d"
    )]
    pub windows: Vec<String>,

    /// Filter the whole report to one kind
    #[arg(long, value_name = "KIND")]
    pub kind: Option<String>,

    /// Detail depth: top-N ops by net profit per window
    #[arg(long, value_name = "N", default_value = "10")]
    pub top: usize,
}

#[derive(Args, Debug, Clone)]
pub struct ExplorerBackfillArgs {
    /// Backfill the trailing N days up to the current confirmed tip
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..=365))]
    pub days: Option<u64>,

    /// Exact range start (requires --to-block). Inclusive.
    #[arg(long = "from-block", value_name = "NUMBER", requires = "to_block")]
    pub from_block: Option<u64>,

    /// Exact range end (requires --from-block). Inclusive.
    #[arg(long = "to-block", value_name = "NUMBER", requires = "from_block")]
    pub to_block: Option<u64>,
}

#[derive(Args, Debug, Clone)]
pub struct ExplorerValidateArgs {
    /// Window: 1d|7d|30d|all (default all; 'all' means every indexed op)
    #[arg(long, value_name = "WINDOW")]
    pub since: Option<String>,

    /// Blocks of tolerance when matching ops to opportunities (default 0)
    #[arg(long = "match-window", value_name = "N", default_value = "0")]
    pub match_window: u64,

    /// Restrict the scanner side to a recorded run id (repeatable)
    #[arg(long = "run-id", value_name = "RUN_ID")]
    pub run_ids: Vec<String>,

    /// Sweep the profit threshold and report count/USD recall per point
    #[arg(long = "threshold-sweep")]
    pub threshold_sweep: bool,

    /// Write pools absent from the scanner coverage to results/missing_pools.txt
    #[arg(long = "emit-missing-pools")]
    pub emit_missing_pools: bool,

    /// Write precision-review candidates to a CSV at this path
    #[arg(long = "review-csv", value_name = "FILE")]
    pub review_csv: Option<String>,

    /// Embed the Phase 0.5 labeled causal-set score in the report
    #[arg(long = "golden-causal")]
    pub golden_causal: bool,

    /// Emit the report as pretty JSON instead of a terminal table
    #[arg(long)]
    pub json: bool,
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

/// Paper subcommand group (virtual-fund P&L).
#[derive(Args, Debug, Clone)]
pub struct PaperArgs {
    #[command(subcommand)]
    pub command: PaperCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub enum PaperCommand {
    /// Backtest range then apply paper ledger
    Run(PaperRunArgs),
    /// Tip detection (optional --loop) then paper ledger
    Live(PaperLiveArgs),
    /// Offline ledger replay over a stored run's opportunities
    Sim(PaperSimArgs),
    /// Summarize paper sessions
    Stats(PaperStatsArgs),
}

#[derive(Args, Debug, Clone)]
pub struct PaperRunArgs {
    #[command(flatten)]
    pub block_range: BlockRangeArgs,
}

#[derive(Args, Debug, Clone)]
pub struct PaperLiveArgs {
    /// Continuously poll and process new blocks until Ctrl+C / deadline
    #[arg(long, help_heading = "Live")]
    pub r#loop: bool,

    /// Stop continuous polling after this duration (requires --loop).
    #[arg(long = "duration", value_name = "DURATION", help_heading = "Live")]
    pub duration: Option<String>,

    /// Stop after processing this many tip passes (requires --loop).
    #[arg(long = "max-blocks", value_name = "NUMBER", help_heading = "Live")]
    pub max_blocks: Option<u64>,
}

#[derive(Args, Debug, Clone)]
pub struct PaperSimArgs {
    /// Existing detection run id (`run_…` / `live_…`)
    #[arg(long = "run", value_name = "RUN_ID")]
    pub run_id: String,

    /// Multiply `[paper].starting_gas_wei` (e.g. 2 = twice the wallet)
    #[arg(long = "wallet-multiplier", value_name = "N", default_value = "1")]
    pub wallet_multiplier: f64,
}

#[derive(Args, Debug, Clone)]
pub struct PaperStatsArgs {
    /// Specific paper session id
    #[arg(long, value_name = "ID")]
    pub session: Option<String>,

    /// Time window: 1d|7d|30d|all (default all)
    #[arg(long, value_name = "WINDOW")]
    pub since: Option<String>,
}
