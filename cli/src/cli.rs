//! CLI argument parsing via clap, defining the command-line interface for mev-scout.

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

    /// Suppress all output except the final summary
    #[arg(global = true, long)]
    pub quiet: bool,
}

/// Width of the zero-config single-pass window, in blocks, used when neither
/// `--blocks` nor `--max-blocks` narrows it down. Chosen so a bare `mev-scout`
/// scans enough recent history to actually contain arbitrage activity while
/// still finishing quickly.
pub const DEFAULT_SINGLE_PASS_BLOCKS: u64 = 64;

/// Trailing window `explorer index` (without `--loop`) uses when no range flag
/// is given, so the 1d/7d/30d revenue reports have data without the user
/// picking a range.
pub const DEFAULT_BACKFILL_DAYS: u64 = 7;

/// Default windows for bare `explorer` (revenue report).
pub const DEFAULT_EXPLORER_WINDOWS: &[&str] = &["1d", "7d", "30d"];

/// Default top-N ops for bare `explorer`.
pub const DEFAULT_EXPLORER_TOP: usize = 10;

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
    /// raw chain data. Bare `explorer` prints the revenue report; use
    /// `index` to populate history and `show` for per-tx detail.
    Explorer(ExplorerArgs),
}

/// Explorer subcommand group. Omitted → revenue report (same as the former
/// `explorer report`).
#[derive(Subcommand, Debug, Clone)]
pub enum ExplorerCommand {
    /// Index blocks into the explorer store.
    ///
    /// Without `--loop` this backfills a historical range (default: trailing
    /// 7 days) and exits. With `--loop` it streams tip blocks until Ctrl+C
    /// (or `--duration`). Idempotent, resumable, reorg-aware.
    Index(IndexArgs),

    /// Operation detail for a tx hash; --trace recomputes exact profit via
    /// debug_traceTransaction (prestateTracer diffMode).
    Show(ShowArgs),

    /// Cross-validation report: realized explorer ops vs scanner
    /// opportunities (T1/T2/T3 matching). Read-only; does no RPC.
    ///
    /// Research tooling, hidden from the default surface. Enable with
    /// `cargo build -p mev-scout-cli --features validate`.
    #[cfg(feature = "validate")]
    Validate(ExplorerValidateArgs),
}

#[derive(Args, Debug, Clone)]
pub struct ExplorerArgs {
    /// Subcommand; omitted → revenue report.
    #[command(subcommand)]
    pub command: Option<ExplorerCommand>,

    /// Time windows: comma-separated 1d|7d|30d|all (default 1d,7d,30d).
    /// Only valid on bare `explorer` (the revenue report).
    #[arg(long, value_name = "WINDOWS", value_delimiter = ',')]
    pub windows: Vec<String>,

    /// Filter the whole report to one kind.
    /// Only valid on bare `explorer`.
    #[arg(long, value_name = "KIND")]
    pub kind: Option<String>,

    /// Detail depth: top-N competitors and ops by net profit per window
    /// (default 10). Only valid on bare `explorer`.
    #[arg(long, value_name = "N")]
    pub top: Option<usize>,
}

impl ExplorerArgs {
    /// True when any revenue-report flag was set on the command line.
    pub fn report_flags_set(&self) -> bool {
        !self.windows.is_empty() || self.kind.is_some() || self.top.is_some()
    }

    /// Resolved windows for the revenue report.
    pub fn resolved_windows(&self) -> Vec<String> {
        if self.windows.is_empty() {
            DEFAULT_EXPLORER_WINDOWS
                .iter()
                .map(|s| (*s).to_string())
                .collect()
        } else {
            self.windows.clone()
        }
    }

    /// Resolved top-N for the revenue report.
    pub fn resolved_top(&self) -> usize {
        self.top.unwrap_or(DEFAULT_EXPLORER_TOP)
    }
}

#[derive(Args, Debug, Clone)]
pub struct IndexArgs {
    /// Continuously index new tip blocks until Ctrl+C
    #[arg(long, help_heading = "Live")]
    pub r#loop: bool,

    /// Stop continuous indexing after this duration (requires --loop).
    /// Accepts humantime suffixes: 90s, 15m, 1h.
    #[arg(long, value_name = "DURATION", help_heading = "Live")]
    pub duration: Option<String>,

    /// Backfill the trailing N days up to the current confirmed tip
    /// (default 7 when no range flag is given). Incompatible with --loop.
    #[arg(
        long,
        value_name = "N",
        value_parser = clap::value_parser!(u64).range(1..=365),
        help_heading = "Range"
    )]
    pub days: Option<u64>,

    /// Exact range start (requires --to-block). Inclusive. Incompatible with --loop.
    #[arg(
        long = "from-block",
        value_name = "NUMBER",
        requires = "to_block",
        help_heading = "Range"
    )]
    pub from_block: Option<u64>,

    /// Exact range end (requires --from-block). Inclusive. Incompatible with --loop.
    #[arg(
        long = "to-block",
        value_name = "NUMBER",
        requires = "from_block",
        help_heading = "Range"
    )]
    pub to_block: Option<u64>,
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

#[cfg(feature = "validate")]
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

#[derive(Args, Debug, Clone)]
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
        }
    }
}
