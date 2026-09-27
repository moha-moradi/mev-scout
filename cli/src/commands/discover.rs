//! Pool discovery command (orchestration lives in `mev_scout_core::jobs`).

use crate::cli::{DiscoverArgs, DiscoverySource};
use crate::job_progress::JobProgress;
use mev_scout_core::config::Config;
use mev_scout_core::dex_type::DexType;
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

    let is_remote_only = matches!(args.source, DiscoverySource::Remote);
    let is_hybrid = matches!(args.source, DiscoverySource::Hybrid);
    let source = match args.source {
        DiscoverySource::Onchain => "onchain",
        DiscoverySource::Remote => "remote",
        DiscoverySource::Hybrid => "hybrid",
    };

    let json = matches!(config.output.output, OutputFormat::Json);
    let opts = mev_scout_core::jobs::DiscoverOpts {
        source: source.to_string(),
        enrich: args.enrich,
        min_tvl: if d.min_tvl > 0.0 {
            Some(d.min_tvl)
        } else {
            None
        },
        max_pools: d.max_pools,
        batch_size: d.batch_size,
        rpc_concurrency: d.rpc_concurrency,
        incremental: args.incremental,
        health_check: d.health_check,
        json,
        solidly_fee_bps: d.solidly_fee_bps.map(u64::from),
        resolve_remote_metadata: d.resolve_remote_metadata,
    };

    let outcome = mev_scout_core::jobs::job_discover(config, &opts, progress).await?;

    if outcome.pools_found == 0 && outcome.active_blocks == 0 && args.incremental && !is_remote_only
    {
        return Ok(());
    }

    let mode = OutputMode {
        json,
        remote_only: is_remote_only,
        hybrid: is_hybrid,
        enrich: args.enrich,
    };
    print_discovered_pools(&outcome.pools, mode, outcome.active_blocks)?;

    Ok(())
}

/// Mode flags that steer every human-readable line of the `discover` flow.
#[derive(Clone, Copy)]
struct OutputMode {
    json: bool,
    remote_only: bool,
    hybrid: bool,
    enrich: bool,
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
    if mode.remote_only {
        println!("  Found {} pool(s) via remote sources", pools.len());
    } else {
        println!(
            "  Found {} pool(s) in {} active blocks",
            pools.len(),
            active_blocks
        );
        if (mode.hybrid || mode.enrich) && !pools.is_empty() {
            let enriched = pools.iter().filter(|p| p.tvl_usd.is_some()).count();
            println!("  Enriched: {} pool(s) with TVL metadata", enriched);
        }
    }
    Ok(())
}
