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

    /// Discover pools from on-chain factory events and/or remote aggregators.
    /// Factory addresses are resolved from the chain config.
    /// Found pools are printed to stdout and saved to the local cache.
    Discover(DiscoverArgs),

    /// Validate pool-discovery accuracy against off-chain references
    /// (GeckoTerminal). Reports recall per DEX,
    /// false positives, field mismatches, and TVL/volume deltas.
    ValidatePools(ValidatePoolsArgs),

    /// Discover and cache token metadata.
    /// Uses the bundled known-token list plus lazily resolved on-chain
    /// metadata, and populates the token cache used by pool discovery
    /// to avoid RPC symbol() calls.
    Tokens(TokensArgs),

    /// Scan on-chain events (trades, transfers, flashloans, liquidations, labels).
    /// Replaces the old Dune query system with direct RPC-based log scanning.
    Scan(ScanArgs),

    /// Stream blocks in real-time, detecting MEV opportunities as they arrive.
    /// Processes new blocks via log-based pool state updates (arb strategies)
    /// with optional full EVM replay for complete detection.
    Live(LiveArgs),

    /// Realized-MEV explorer: forensic reconstruction of extracted MEV from
    /// raw chain data (index, live feed, stats, leaderboards, op detail,
    /// cross-validation against the opportunity scanner).
    Explorer(ExplorerArgs),
}

/// Explorer subcommand group.
#[derive(Subcommand, Debug, Clone)]
pub enum ExplorerCommand {
    /// Probe every configured provider: latest block, bulk receipts, traces.
    /// Prints the capability matrix; gates Phase 0.
    Doctor,

    /// Backfill and/or stream-index blocks into the explorer store.
    /// Idempotent, resumable, reorg-aware; classify-in-stream.
    Index(IndexArgs),

    /// mev.zone-style live feed of realized ops (tail of the indexed store).
    LiveFeed(LiveFeedArgs),

    /// Overview: op counts per kind, profit totals, daily breakdown, top
    /// searchers/pools. Pure SQL over the store.
    Stats(StatsArgs),

    /// Leaderboards (senders / tokens / pools).
    Top(TopArgs),

    /// Operation detail for a tx hash; --trace recomputes exact profit via
    /// debug_traceTransaction (prestateTracer diffMode).
    Show(ShowArgs),

    /// Per-miss drill-down: realized op + the scanner's rejected candidates
    /// + inferred M1–M8 miss cause.
    Explain(ExplainArgs),

    /// Cross-validation: realized ground truth vs run/live opportunities.
    /// Reports tiered recall, USD-weighted recall, miss taxonomy, sweep.
    Validate(ValidateArgs),

    /// Bulk export of realized ops (json|csv).
    Export(ExportArgs),
}

#[derive(Args, Debug, Clone)]
pub struct ExplorerArgs {
    #[command(subcommand)]
    pub command: ExplorerCommand,
}

#[derive(Args, Debug, Clone)]
pub struct IndexArgs {
    /// Range start block (with --to)
    #[arg(long, value_name = "N")]
    pub from: Option<u64>,

    /// Range end block (with --from)
    #[arg(long, value_name = "N")]
    pub to: Option<u64>,

    /// Backfill the last N days of blocks (default 30 when no range given)
    #[arg(long, value_name = "N")]
    pub days: Option<u64>,

    /// Follow head − confirmations and index new blocks continuously
    #[arg(long)]
    pub live: bool,

    /// Stop live indexing after this duration (e.g. 90s, 15m, 1h)
    #[arg(long, value_name = "DURATION")]
    pub duration: Option<String>,
}

#[derive(Args, Debug, Clone)]
pub struct LiveFeedArgs {
    /// Comma-separated kind filter (arb_atomic,sandwich,liquidation,jit,jit_arb,unknown)
    #[arg(long, value_name = "KINDS")]
    pub kinds: Option<String>,

    /// Hide ops below this realized profit (default 0)
    #[arg(long = "min-profit-usd", default_value = "0", value_name = "USD")]
    pub min_profit_usd: f64,

    /// Poll interval in milliseconds (default from [explorer] config)
    #[arg(long = "poll-interval-ms", value_name = "MS")]
    pub poll_interval_ms: Option<u64>,

    /// Stop after this duration (e.g. 90s, 15m, 1h)
    #[arg(long, value_name = "DURATION")]
    pub duration: Option<String>,
}

#[derive(Args, Debug, Clone)]
pub struct StatsArgs {
    /// Time window: 1d|7d|30d|all (default all)
    #[arg(long, value_name = "WINDOW")]
    pub since: Option<String>,

    /// Period bucket hint: day|week|month|year
    #[arg(long, value_name = "PERIOD")]
    pub window: Option<String>,

    /// Filter to one kind
    #[arg(long, value_name = "KIND")]
    pub kind: Option<String>,
}

#[derive(Args, Debug, Clone)]
pub struct TopArgs {
    /// Leaderboard dimension: sender|token|pool
    #[arg(long, default_value = "sender")]
    pub by: String,

    /// Ranking metric: profit|ops
    #[arg(long, default_value = "profit")]
    pub metric: String,

    /// Time window: 1d|7d|30d|all
    #[arg(long, value_name = "WINDOW")]
    pub since: Option<String>,

    /// Rows to show (default 20)
    #[arg(long, default_value = "20")]
    pub limit: usize,
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
pub struct ExplainArgs {
    /// Transaction hash of the missed realized op
    #[arg(value_name = "TX_HASH")]
    pub tx_hash: String,
}

#[derive(Args, Debug, Clone)]
pub struct ValidateArgs {
    /// Time window: 1d|7d|30d|all
    #[arg(long, value_name = "WINDOW")]
    pub since: Option<String>,

    /// Match window in blocks (default 0 for run backtests; 1 recommended for live)
    #[arg(long = "match-window", default_value = "0", value_name = "N")]
    pub match_window: u64,

    /// Restrict scanner side to these run ids (repeatable)
    #[arg(long = "run", value_name = "RUN_ID")]
    pub run: Vec<String>,

    /// Report recall(count+USD) as a function of the min-profit threshold
    #[arg(long = "threshold-sweep")]
    pub threshold_sweep: bool,

    /// Write realized-op pools missing from the scanner pool set to results/missing_pools.txt
    #[arg(long = "emit-missing-pools")]
    pub emit_missing_pools: bool,

    /// Machine-readable JSON output
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug, Clone)]
pub struct ExportArgs {
    /// Output format: json|csv (default json)
    #[arg(long, default_value = "json")]
    pub format: String,

    /// Time window: 1d|7d|30d|all
    #[arg(long, value_name = "WINDOW")]
    pub since: Option<String>,

    /// Comma-separated kind filter
    #[arg(long, value_name = "KINDS")]
    pub kinds: Option<String>,

    /// Output file path (default results/explorer_export_<epoch>.<ext>)
    #[arg(long, value_name = "FILE")]
    pub out: Option<String>,
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
pub struct RunArgs {
    #[command(flatten)]
    pub block_range: BlockRangeArgs,

    /// Enable JSON-RPC batching (send block+receipts in one HTTP POST).
    /// Disabled by default — separate parallel requests often achieve better throughput.
    #[arg(long = "batch-rpc", help_heading = "RPC")]
    pub batch_rpc: bool,

    /// Record rejected candidates into the explorer store for
    /// false-negative attribution in `explorer validate`. Off by default.
    #[arg(long = "record-rejections")]
    pub record_rejections: bool,
}

#[derive(Args, Debug, Clone)]
pub struct FetchArgs {
    #[command(flatten)]
    pub block_range: BlockRangeArgs,

    /// Enable JSON-RPC batching (send block+receipts in one HTTP POST).
    /// Disabled by default — separate parallel requests often achieve better throughput.
    #[arg(long = "batch-rpc")]
    pub batch_rpc: bool,

    /// Skip 4-byte signature resolution (much faster, no 4byte.directory API calls)
    #[arg(long = "no-sig-resolve")]
    pub no_sig_resolve: bool,
}

#[derive(Args, Debug, Clone)]
pub struct ReplayArgs {
    /// Block number to replay (required)
    #[arg(long, required = true, value_name = "NUMBER")]
    pub block: u64,

    /// Replay up to this tx index (default: all)
    #[arg(long, value_name = "INDEX")]
    pub tx_index: Option<usize>,

    /// Show DEX interaction analysis per transaction
    #[arg(long)]
    pub analyze: bool,
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

    /// Batch size for each getLogs request (default: 500, safe for public RPCs)
    #[arg(long, default_value = "500", value_name = "NUMBER")]
    pub batch_size: u64,

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

    /// Pool source: onchain (RPC events only), remote (GeckoTerminal
    /// aggregator only), or hybrid (union of both, deduped by address).
    /// Default onchain — zero behavior change.
    #[arg(long, default_value = "onchain", value_name = "SOURCE")]
    pub source: DiscoverySource,

    /// Attach tvl_usd / volume_usd_24h / volume_usd_30d to discovered pools
    /// from the free GeckoTerminal aggregator. Implies one remote fetch.
    #[arg(long)]
    pub enrich: bool,

    /// Minimum USD TVL for remote-sourced pools (default 0 = full parity with
    /// on-chain discovery; opt-in to mimic explorer dust suppression).
    #[arg(long = "min-tvl", default_value = "0", value_name = "USD")]
    pub min_tvl: f64,

    /// Per-source pagination cap for remote discovery (default 1000).
    #[arg(long = "max-pools", default_value = "1000", value_name = "N")]
    pub max_pools: usize,

    /// Resolve missing fee/tickSpacing/token metadata for remote-sourced
    /// concentrated-liquidity pools via a Multicall3 batch (one eth_call per
    /// ~25 pools). Off by default so offline/remote-only workflows stay RPC-free.
    /// Results are persisted to the SQLite cache and never re-fetched.
    #[arg(long = "resolve-remote-metadata")]
    pub resolve_remote_metadata: bool,
}

/// Pool discovery source selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum DiscoverySource {
    Onchain,
    Remote,
    Hybrid,
}

#[derive(Args, Debug, Clone)]
pub struct ValidatePoolsArgs {
    /// Look-back window in days for the on-chain discovery leg (default 7).
    #[arg(long, default_value = "7", value_name = "N")]
    pub days: u64,

    /// Reference sources to compare against: all, gecko.
    #[arg(long, default_value = "all", value_name = "SOURCE")]
    pub source: ValidationSource,

    /// Output as machine-readable JSON instead of a table.
    #[arg(long)]
    pub json: bool,

    /// Also write a markdown report to this path.
    #[arg(long = "markdown-out", value_name = "PATH")]
    pub markdown_out: Option<String>,
}

/// Reference source selection for validate-pools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ValidationSource {
    All,
    Gecko,
}

#[derive(Args, Debug, Clone)]
pub struct TokensArgs {
    /// Filter by symbol pattern (case-insensitive substring match)
    #[arg(long, value_name = "PATTERN")]
    pub symbol: Option<String>,

    /// Filter by exact decimals value
    #[arg(long, value_name = "N")]
    pub decimals: Option<u8>,

    /// Maximum tokens to display (default: 100)
    #[arg(long, default_value = "100", value_name = "N")]
    pub limit: usize,

    /// Only populate SQLite cache, don't display results
    #[arg(long)]
    pub cache_only: bool,
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
    pub block_range: BlockRangeArgs,

    /// What to scan for
    #[arg(long, value_enum, default_value = "trades")]
    pub kind: ScanKind,

    /// Filter by contract address (reusable)
    #[arg(long = "address", value_name = "ADDRESS")]
    pub addresses: Option<Vec<String>>,

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

#[derive(Args, Debug, Clone)]
pub struct LiveArgs {
    /// Continuously poll and process new blocks until Ctrl+C
    #[arg(long, help_heading = "Live")]
    pub r#loop: bool,

    /// Stop continuous polling after this duration (requires --loop).
    /// Accepts humantime suffixes: 90s, 15m, 1h, 1h 30m.
    #[arg(long = "duration", value_name = "DURATION", help_heading = "Live")]
    pub duration: Option<String>,

    /// Polling interval in milliseconds (default: 2000)
    #[arg(
        long = "poll-interval",
        default_value = "2000",
        value_name = "MS",
        help_heading = "Live"
    )]
    pub poll_interval_ms: u64,

    /// Record rejected candidates into the explorer store for
    /// false-negative attribution in `explorer validate`. Off by default.
    #[arg(long = "record-rejections")]
    pub record_rejections: bool,
}
