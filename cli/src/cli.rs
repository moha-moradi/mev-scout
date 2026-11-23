//! CLI argument parsing via clap, defining the command-line interface for mev-scout.
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// MEV Scout — MEV opportunity scanner & backtester for EVM-compatible chains.
///
/// Run bare to scan the most recent blocks for MEV opportunities: that is
/// exactly equivalent to `mev-scout live`.
#[derive(Parser, Debug)]
#[command(name = "mev-scout", version, about)]
pub struct Cli {
    /// Subcommand; defaults to `live` (single-pass) when omitted.
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Path to TOML config file
    #[arg(global = true, short = 'f', long = "config", value_name = "FILE")]
    pub config: Option<String>,

    /// Enable debug-level logging
    #[arg(global = true, short, long)]
    pub verbose: bool,

    /// Lower tracing to `error` only (stdout tables/ledger/summaries still print)
    #[arg(global = true, long)]
    pub quiet: bool,
}

/// Width of the zero-config single-pass window, in blocks, used when neither
/// `--blocks` nor `--max-blocks` narrows it down. Chosen so a bare `mev-scout`
/// scans enough recent history to actually contain arbitrage activity while
/// still finishing quickly.
pub const DEFAULT_SINGLE_PASS_BLOCKS: u64 = 64;

impl Cli {
    /// The subcommand to run, materializing the implicit `live` default when
    /// none was given. Built programmatically so the zero-arg path cannot drift
    /// from the typed `live` definition.
    pub fn command_or_default(&self) -> Command {
        self.command
            .clone()
            .unwrap_or_else(|| Command::Live(LiveArgs::default()))
    }
}

#[derive(Subcommand, Debug, Clone)]
pub enum Command {
    /// Re-render a recorded run from SQLite (run_manifests + explorer opportunities)
    Report(ReportArgs),

    /// Print the fully resolved config as TOML
    Config,

    /// Discover pools from on-chain factory events.
    /// Factory addresses are resolved from the chain config; with no range flag
    /// it scans the chain's default lookback window and resumes incrementally
    /// when the pool cache is already populated.
    /// Found pools are printed to stdout and saved to the local cache.
    Discover(DiscoverArgs),

    /// Stream blocks in real-time, detecting MEV opportunities as they arrive.
    /// Processes new blocks via log-based pool state updates (arb strategies)
    /// with optional full EVM replay for complete detection.
    ///
    /// Without `--loop` this is a single pass over the most recent blocks that
    /// prints the opportunity table plus a ledger summary, then exits.
    Live(LiveArgs),

    /// Realized-MEV explorer: forensic reconstruction of extracted MEV from
    /// raw chain data. Bare `explorer` emits the all-time HTML dashboard;
    /// use `index` to populate history for a range, then emit its dashboard.
    Explorer(ExplorerArgs),
}

/// Explorer subcommand group. Omitted → all-time HTML dashboard.
#[derive(Subcommand, Debug, Clone)]
pub enum ExplorerCommand {
    /// Index a historical block range into the explorer store, then emit the
    /// HTML dashboard for that range.
    ///
    /// Backfills a trailing `--window` (default 7d) or an exact
    /// `--from-block`/`--to-block` range. Idempotent, resumable.
    Index(IndexArgs),

    /// Audit one transaction: stored ops/evidence plus a call trace when a
    /// configured RPC serves `debug_traceTransaction`/`trace_transaction`.
    Audit(AuditArgs),
}

#[derive(Args, Debug, Clone)]
pub struct AuditArgs {
    /// Transaction hash to audit (0x + 64 hex).
    #[arg(long, value_name = "HASH")]
    pub tx: String,
}

#[derive(Args, Debug, Clone)]
pub struct ExplorerArgs {
    /// Subcommand; omitted → all-time HTML dashboard.
    #[command(subcommand)]
    pub command: Option<ExplorerCommand>,

    /// Output HTML path (default: `results/explorer-<chain>-<range>.html`).
    #[arg(long, value_name = "PATH", global = true)]
    pub out: Option<PathBuf>,
}

#[derive(Args, Debug, Clone)]
pub struct IndexArgs {
    /// Trailing duration (`24h`, `7d`, `2w`) up to the confirmed tip.
    /// Incompatible with `--from-block`/`--to-block`.
    #[arg(
        long,
        value_name = "WINDOW",
        conflicts_with_all = ["from_block", "to_block"],
        help_heading = "Range"
    )]
    pub window: Option<String>,

    /// Exact range start (requires --to-block). Inclusive.
    #[arg(
        long = "from-block",
        value_name = "NUMBER",
        requires = "to_block",
        conflicts_with = "window",
        help_heading = "Range"
    )]
    pub from_block: Option<u64>,

    /// Exact range end (requires --from-block). Inclusive.
    #[arg(
        long = "to-block",
        value_name = "NUMBER",
        requires = "from_block",
        conflicts_with = "window",
        help_heading = "Range"
    )]
    pub to_block: Option<u64>,

    /// Discard the range's stored facts and reclassify it, so detector/tag
    /// changes apply to already-indexed blocks.
    #[arg(long, help_heading = "Index")]
    pub reclassify: bool,

    /// Run on-chain pool discovery first. Fills the scanner cache used for
    /// skim eligibility and venue tags (skipped when the registry is already
    /// populated and this flag is absent).
    #[arg(long = "discover-pools", help_heading = "Index")]
    pub discover_pools: bool,
}

#[derive(Args, Debug, Clone, Default)]
#[command(next_help_heading = "Block Range (optional — defaults to the chain lookback window)")]
pub struct BlockRangeArgs {
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

impl BlockRangeArgs {
    /// True when the user pinned an explicit range, which suppresses the
    /// implicit incremental/lookback resolution.
    pub fn is_specified(&self) -> bool {
        self.blocks.is_some()
            || self.block.is_some()
            || self.from_block.is_some()
            || self.to_block.is_some()
    }
}

impl TryFrom<&BlockRangeArgs> for mev_scout_core::config::validation::RangeSpec {
    type Error = mev_scout_core::error::ConfigError;

    fn try_from(a: &BlockRangeArgs) -> Result<Self, Self::Error> {
        Self::from_flags(None, a.blocks, a.block, a.from_block, a.to_block)
    }
}

#[derive(Args, Debug, Clone)]
pub struct ReportArgs {
    /// Specific run ID to report (default: latest)
    #[arg(long, value_name = "ID")]
    pub run_id: Option<String>,
}

#[derive(Args, Debug, Clone, Default)]
pub struct DiscoverArgs {
    #[command(flatten)]
    pub block_range: BlockRangeArgs,

    /// Resume from the latest cached block instead of the full range.
    /// Queries the cache for the highest creation_block and scans from there.
    /// Implied automatically when no range flag is given and the pool cache is
    /// already populated.
    #[arg(long)]
    pub incremental: bool,
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

    /// Width of the single-pass window in blocks, ending at the tip
    /// (default 64). Ignored with --loop.
    #[arg(
        long = "blocks",
        value_name = "NUMBER",
        default_value_t = DEFAULT_SINGLE_PASS_BLOCKS,
        value_parser = clap::value_parser!(u64).range(1..),
        help_heading = "Live"
    )]
    pub blocks: u64,

    /// Starting gas wallet in wei (native, 18 decimals). Overrides
    /// [paper].starting_gas_wei. Exact and needs no network.
    #[arg(long = "initial-balance", value_name = "WEI", help_heading = "Ledger")]
    pub initial_balance: Option<u128>,

    /// Native wei kept idle so one large fill cannot starve later blocks.
    /// Overrides [paper].reserve_wei.
    #[arg(long, value_name = "WEI", help_heading = "Ledger")]
    pub reserve: Option<u128>,

    /// Paper fill verification: `assumed` (default, claim detector net) or
    /// `state` (re-quote against pool snapshots with liquidity consumption).
    /// Overrides [paper].sim.
    #[arg(long = "sim", value_name = "MODE", help_heading = "Ledger")]
    pub sim: Option<String>,

    /// Virtual execution backend: `none` (default), `revm`, or `anvil`.
    /// Overrides [paper].exec. Never broadcasts to mainnet.
    #[arg(long = "exec", value_name = "MODE", help_heading = "Ledger")]
    pub exec: Option<String>,

    /// Flash-loan funding layer for this session: auto, balancer, aave,
    /// uniswap, lfj, v2_flash_swap. Overrides [backtest].flash_loan_provider.
    #[arg(
        long = "funding-provider",
        value_name = "PROVIDER",
        help_heading = "Live"
    )]
    pub funding_provider: Option<String>,

    /// Reject quotes older than this many milliseconds before ledger admission.
    /// Overrides [live].max_quote_age_ms.
    #[arg(long = "max-quote-age-ms", value_name = "MS", help_heading = "Live")]
    pub max_quote_age_ms: Option<u64>,

    /// Reject any opportunity carrying a risk flag (funding reentrancy, drift, …).
    /// Overrides [live].require_risk_clear.
    #[arg(long = "require-risk-clear", help_heading = "Live")]
    pub require_risk_clear: bool,

    /// Re-quote backruns around large pending swaps with an explicit quote
    /// deadline and victim tx index. Overrides [live].pending_backrun.
    #[arg(long = "pending-backrun", help_heading = "Live")]
    pub pending_backrun: bool,

    /// Enable the candidate-L amount sweep (requires `--sim state`).
    /// Overrides [paper].optimizer_sweep.
    #[arg(long = "optimizer-sweep", help_heading = "Ledger")]
    pub optimizer_sweep: bool,
}

impl Default for LiveArgs {
    /// Mirrors the clap defaults so the implicit bare-`mev-scout` path and the
    /// explicit `live` subcommand behave identically.
    fn default() -> Self {
        LiveArgs {
            r#loop: false,
            duration: None,
            max_blocks: None,
            blocks: DEFAULT_SINGLE_PASS_BLOCKS,
            initial_balance: None,
            reserve: None,
            sim: None,
            exec: None,
            funding_provider: None,
            max_quote_age_ms: None,
            require_risk_clear: false,
            pending_backrun: false,
            optimizer_sweep: false,
        }
    }
}
