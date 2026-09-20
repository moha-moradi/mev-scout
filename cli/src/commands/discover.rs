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

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, Address};
    use mev_scout_core::cache::SqliteStore;
    use mev_scout_core::pool::state::PoolInfo;

    #[allow(clippy::field_reassign_with_default)]
    fn cached_to_discovered(existing: PoolInfo) -> DiscoveredPool {
        DiscoveredPool::new(
            existing.address,
            existing.token0,
            existing.token1,
            existing.fee,
            existing.dex_type,
            existing.creation_block,
        )
        .with_tick_spacing(existing.tick_spacing.map(|ts| ts as i32))
        .with_pool_id(existing.pool_id)
        .with_factory(existing.factory)
        .with_is_stable(existing.is_stable)
        .with_balancer_pool_type(existing.balancer_pool_type)
        .with_hook_address(existing.hook_address)
        .with_bin_step(existing.bin_step)
        .with_maturity_timestamp(existing.maturity_timestamp)
        .with_underlying_tokens(existing.underlying_tokens)
        .with_dex_name(existing.dex_name.as_deref().map(String::from))
        .with_token0_symbol(existing.token0_symbol.as_deref().map(String::from))
        .with_token1_symbol(existing.token1_symbol.as_deref().map(String::from))
        .with_tvl_usd(existing.tvl_usd)
        .with_volume_usd_24h(existing.volume_usd_24h)
        .with_volume_usd_30d(existing.volume_usd_30d)
    }

    fn persist_universe(cache: &SqliteStore, pools: &[DiscoveredPool]) -> usize {
        let mut persisted = 0usize;
        for p in pools {
            if p.token0.is_zero() || p.token1.is_zero() {
                continue;
            }
            let mut merged = p.clone();
            if let Ok(Some(existing)) = cache.get_discovered_pool(&p.address) {
                merged.merge_from(&cached_to_discovered(existing));
            }
            let info: PoolInfo = merged.into();
            match cache.put_discovered_pool(&info) {
                Ok(()) => persisted += 1,
                Err(e) => tracing::warn!("Failed to cache pool {}: {e:#}", p.address),
            }
        }
        persisted
    }

    fn temp_store(tag: &str) -> (SqliteStore, std::path::PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("mev_scout_persist_{}_{}", tag, std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("cache.db");
        (SqliteStore::open(&path).unwrap(), path)
    }

    #[test]
    fn persist_universe_roundtrips_remote_pools_with_dex_type() {
        let (store, path) = temp_store("roundtrip");

        let t0 = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let t1 = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let pools = vec![
            DiscoveredPool::new(
                address!("1111111111111111111111111111111111111111"),
                t0,
                t1,
                0,
                DexType::UniswapV3,
                0,
            )
            .with_tvl_usd(Some(50_000.0)),
            DiscoveredPool::new(
                address!("2222222222222222222222222222222222222222"),
                t0,
                t1,
                0,
                DexType::TraderJoeLB,
                0,
            ),
            DiscoveredPool::new(
                address!("3333333333333333333333333333333333333333"),
                t0,
                t1,
                0,
                DexType::Pendle,
                0,
            ),
            DiscoveredPool::new(
                address!("4444444444444444444444444444444444444444"),
                Address::ZERO,
                t1,
                3000,
                DexType::UniswapV3,
                0,
            ),
        ];

        let persisted = persist_universe(&store, &pools);
        assert_eq!(persisted, 3, "zero-token entry must not be persisted");
        drop(store);

        let store = SqliteStore::open(&path).unwrap();
        for p in pools.iter().take(3) {
            let got = store
                .get_discovered_pool(&p.address)
                .unwrap()
                .unwrap_or_else(|| panic!("pool {} missing after round-trip", p.address));
            assert_eq!(got.address, p.address);
            assert_eq!(got.dex_type, p.dex_type);
            assert_eq!(got.token0, p.token0);
            assert_eq!(got.token1, p.token1);
            assert_eq!(got.tvl_usd, p.tvl_usd);
        }
        assert!(store
            .get_discovered_pool(&address!("4444444444444444444444444444444444444444"))
            .unwrap()
            .is_none());
    }

    #[test]
    fn persist_universe_second_sparser_run_never_clobbers_cached_row() {
        let (store, _path) = temp_store("merge");

        let addr = address!("5555555555555555555555555555555555555555");
        let t0 = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let t1 = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");

        let rich = vec![DiscoveredPool::new(
            addr,
            t0,
            t1,
            500,
            DexType::UniswapV3,
            100,
        )];
        assert_eq!(persist_universe(&store, &rich), 1);

        let sparse = vec![DiscoveredPool::new(addr, t0, t1, 0, DexType::UniswapV2, 0)];
        assert_eq!(persist_universe(&store, &sparse), 1);

        let got = store.get_discovered_pool(&addr).unwrap().unwrap();
        assert_eq!(got.dex_type, DexType::UniswapV3);
        assert_eq!(got.fee, 500);
        assert_eq!(got.creation_block, 100);
    }
}
