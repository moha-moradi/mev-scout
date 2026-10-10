//! Pool discovery command (orchestration lives in `mev_scout_core::jobs`).
use crate::cli::DiscoverArgs;
use crate::job_progress::JobProgress;
use mev_scout_core::config::Config;
use mev_scout_core::dex_type::DexType;
use mev_scout_core::jobs::DiscoverOpts;
use mev_scout_core::pool::discovery::DiscoveredPool;
use mev_scout_core::types::OutputFormat;

pub async fn cmd_discover(
    config: &Config,
    args: &DiscoverArgs,
    progress: &dyn JobProgress,
) -> anyhow::Result<()> {
    let d = &config.discover;
    if d.batch_size > 5000 {
        eprintln!(
            "  Warning: batch_size={} exceeds recommended maximum of 5000 for public RPCs. \
                   Free-tier endpoints (drpc, Ankr, CloudFlare) typically cap eth_getLogs at 5K–10K blocks. \
                   Consider setting [discover].batch_size = 2000 for best results.",
            d.batch_size
        );
    }

    // With no explicit range and a populated cache, resume from the newest
    // cached block instead of rescanning the lookback window on every run.
    let incremental =
        args.incremental || (!args.block_range.is_specified() && cache_is_populated(config));
    if incremental && !args.incremental {
        progress.log("No block range given and pools are cached — resuming incrementally.");
    }

    let json = matches!(config.output.output, OutputFormat::Json);
    let mut opts = DiscoverOpts::from_config(config);
    opts.incremental = incremental;
    opts.json = json;

    let outcome = mev_scout_core::jobs::job_discover(config, &opts, progress).await?;

    if outcome.pools_found == 0 && outcome.active_blocks == 0 && incremental {
        return Ok(());
    }

    let mode = OutputMode { json };
    print_discovered_pools(&outcome.pools, mode, outcome.active_blocks)?;

    Ok(())
}

/// Whether the pool cache already holds rows for the configured chain. An
/// unreadable cache reads as "empty", which downgrades to a plain full scan.
fn cache_is_populated(config: &Config) -> bool {
    let Ok((chain, _)) = mev_scout_core::config::validation::resolve_chain(config) else {
        return false;
    };
    let path = config.effective_db_path(&chain);
    match mev_scout_core::cache::SqliteStore::open(&path) {
        Ok(store) => store.pool_count().unwrap_or(0) > 0,
        Err(e) => {
            tracing::debug!("could not open pool cache {}: {e:#}", path);
            false
        }
    }
}

/// Mode flags that steer every human-readable line of the `discover` flow.
#[derive(Clone, Copy)]
struct OutputMode {
    json: bool,
}

/// Render the discovered pool list (JSON passthrough or per-DEX table lines).
fn print_discovered_pools(
    pools: &[DiscoveredPool],
    mode: OutputMode,
    active_blocks: usize,
) -> anyhow::Result<()> {
    if mode.json {
        println!("{}", serde_json::to_string_pretty(pools)?);
        return Ok(());
    }
    for p in pools {
        let dex = p.dex_name.as_deref().unwrap_or(match p.dex_type {
            DexType::UniswapV2 => "V2",
            DexType::UniswapV3 => "V3",
            DexType::UniswapV4 => "V4",
            _ => "Pool",
        });
        let t0 = p.token0_symbol.as_deref().unwrap_or("???");
        let t1 = p.token1_symbol.as_deref().unwrap_or("???");
        let tvl_note = p
            .tvl_usd
            .map(|v| format!(" tvl=${:.0}", v))
            .unwrap_or_default();
        match p.dex_type {
            DexType::UniswapV2 => {
                println!("  {dex}  {}  {}/{}{}", p.address, t0, t1, tvl_note);
            }
            DexType::UniswapV3 | DexType::UniswapV4 | DexType::PancakeInfinity => {
                println!(
                    "  {dex}  {}  {}/{}  fee={}  tickSpacing={}{}",
                    p.address,
                    t0,
                    t1,
                    p.fee,
                    p.tick_spacing.unwrap_or(0),
                    tvl_note
                );
            }
            DexType::Solidly | DexType::Camelot => {
                let stable = p
                    .is_stable
                    .map(|s| if s { " stable" } else { "" })
                    .unwrap_or("");
                println!("  {dex}{stable}  {}  {}/{}{}", p.address, t0, t1, tvl_note);
            }
            DexType::Balancer | DexType::Curve => {
                if let Some(ref tokens) = p.underlying_tokens {
                    let syms: Vec<String> = tokens.iter().map(|t| t.to_string()).collect();
                    println!("  {dex}  {}  [{}]{}", p.address, syms.join(", "), tvl_note);
                } else {
                    println!("  {dex}  {}  {}/{}{}", p.address, t0, t1, tvl_note);
                }
            }
            DexType::TraderJoeLB => {
                println!(
                    "  {dex}  {}  {}/{}  binStep={}{}",
                    p.address,
                    t0,
                    t1,
                    p.bin_step.unwrap_or(0),
                    tvl_note
                );
            }
            DexType::Pendle => {
                println!(
                    "  {dex}  {}  {}/{}  maturity={}{}",
                    p.address,
                    t0,
                    t1,
                    p.maturity_timestamp.unwrap_or(0),
                    tvl_note
                );
            }
            DexType::Metric | DexType::Fluid => {
                println!("  {dex}  {}  {}/{}{}", p.address, t0, t1, tvl_note);
            }
        }
    }
    println!();
    println!(
        "  Found {} pool(s) in {} active blocks",
        pools.len(),
        active_blocks
    );
    Ok(())
}
