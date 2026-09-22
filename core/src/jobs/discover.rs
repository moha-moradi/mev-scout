use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use alloy::primitives::Address;
use anyhow::Context;

use crate::cache::{SqliteStore, TokenCache};
use crate::config::validation;
use crate::config::{ChainConfig, Config};
use crate::pool::discovery::remote as remote_src;
use crate::pool::discovery::{
    DiscoveredPool, DiscoveryConfig, DiscoveryRuntimeOpts, ResolvedFactories,
};
use crate::pool::state::PoolInfo;
use crate::progress::{JobProgress, ProgressEvent};
use crate::resolver::RangeResolver;
use crate::rpc::{recommended_get_logs_batch, RpcClient};
use crate::types::ChainName;

use super::rpc::init_rpc;

#[derive(Debug, Clone)]
pub struct DiscoverOpts {
    pub source: String,
    pub enrich: bool,
    pub min_tvl: Option<f64>,
    pub max_pools: usize,
    pub batch_size: u64,
    pub rpc_concurrency: usize,
    pub incremental: bool,
    pub health_check: bool,
    pub json: bool,
    pub solidly_fee_bps: Option<u64>,
    pub resolve_remote_metadata: bool,
}

impl Default for DiscoverOpts {
    fn default() -> Self {
        DiscoverOpts {
            source: "onchain".to_string(),
            enrich: false,
            min_tvl: None,
            max_pools: 1000,
            batch_size: 500,
            rpc_concurrency: 8,
            incremental: false,
            health_check: true,
            json: false,
            solidly_fee_bps: None,
            resolve_remote_metadata: false,
        }
    }
}

pub struct DiscoverOutcome {
    pub pools_found: usize,
    pub active_blocks: usize,
    pub remote_count: usize,
    pub pools: Vec<DiscoveredPool>,
}

async fn fetch_remote(
    chain_name: ChainName,
    max_pools: Option<usize>,
    min_tvl: Option<f64>,
) -> Vec<DiscoveredPool> {
    let slug = chain_name.to_string();
    remote_src::discover_via_remote(&slug, max_pools, min_tvl).await
}

/// Enrich missing TVL/volume/symbols on on-chain pools from remote entries.
fn enrich_from_remote(pools: &mut [DiscoveredPool], remote: &[DiscoveredPool]) {
    let by_addr: HashMap<Address, &DiscoveredPool> =
        remote.iter().map(|p| (p.address, p)).collect();
    for p in pools.iter_mut() {
        if p.tvl_usd.is_none() {
            if let Some(r) = by_addr.get(&p.address) {
                p.merge_from(r);
            }
        }
    }
}

fn dedup_by_address(pools: Vec<DiscoveredPool>) -> Vec<DiscoveredPool> {
    let mut index: HashMap<Address, usize> = HashMap::with_capacity(pools.len());
    let mut out: Vec<DiscoveredPool> = Vec::with_capacity(pools.len());
    for p in pools {
        match index.get(&p.address) {
            Some(&i) => out[i].merge_from(&p),
            None => {
                index.insert(p.address, out.len());
                out.push(p);
            }
        }
    }
    out
}

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

async fn scan_onchain(
    rpc: &RpcClient,
    cache: &SqliteStore,
    from: u64,
    to: u64,
    disc_config: &DiscoveryConfig<'_>,
    progress: &dyn JobProgress,
) -> anyhow::Result<(Vec<DiscoveredPool>, std::collections::HashSet<u64>)> {
    let total = to.saturating_sub(from) + 1;
    let done = Arc::new(AtomicU64::new(0));
    let tick = move || {
        if progress.cancelled() {
            return false;
        }
        let d = done.fetch_add(1, Ordering::Relaxed) + 1;
        progress.emit(ProgressEvent {
            stage: "fetch".to_string(),
            done: Some(d),
            total: Some(total),
            run_id: None,
            ops: None,
            elapsed_ms: None,
        });
        true
    };
    let result =
        crate::pool::discovery::discover_and_cache(rpc, cache, from, to, disc_config, Some(&tick))
            .await;
    match result {
        Ok((pools, active_blocks)) => {
            progress.log(&format!(
                "On-chain: found {} pools in {} active blocks",
                pools.len(),
                active_blocks.len()
            ));
            Ok((pools, active_blocks))
        }
        Err(e) => {
            if progress.cancelled() {
                return Err(e);
            }
            progress.log(&format!("  On-chain pool discovery failed: {e:#}"));
            Ok((Vec::new(), std::collections::HashSet::new()))
        }
    }
}

pub async fn job_discover(
    config: &Config,
    opts: &DiscoverOpts,
    progress: &dyn JobProgress,
) -> anyhow::Result<DiscoverOutcome> {
    let (chain_name, chain_config) =
        validation::resolve_chain(config).context("failed to resolve chain configuration")?;
    validation::validate_chain_config_addresses(&chain_config)
        .context("chain configuration has invalid addresses")?;
    let chain_id = chain_name.chain_id();

    let source = opts.source.clone();
    let is_remote_only = source == "remote";
    let is_hybrid = source == "hybrid";
    let enrich = opts.enrich;
    let should_fetch_remote = is_remote_only || is_hybrid || enrich;

    let min_tvl_opt = opts.min_tvl.filter(|v| *v > 0.0);
    let max_pools_opt = opts.max_pools;
    let batch_size = opts.batch_size;
    let rpc_concurrency = opts.rpc_concurrency;
    let incremental = opts.incremental;
    let health_check = opts.health_check;
    let json = opts.json;

    let setup = init_rpc(config, chain_name, true).await?;
    let rpc = setup.rpc;

    // Block range — not needed for pure remote mode.
    let (from, to) =
        resolve_scan_window(&rpc, config, &chain_config, chain_name, is_remote_only).await?;

    let cache_path = config.effective_db_path(&chain_name);
    let cache = SqliteStore::open(&cache_path)?;

    let mut token_cache = TokenCache::warm(chain_id);
    match TokenCache::load(&cache) {
        Ok(persisted) => token_cache.merge(persisted),
        Err(e) => tracing::warn!("Failed to load token cache from SQLite: {e:#}"),
    }

    // Phase 2.5 start-block guard.
    if !is_remote_only {
        if let Ok(by_factory) = cache.earliest_creation_block_by_factory() {
            for (factory, first_block) in by_factory {
                if let Some(cfg_start) = chain_config.pool_discovery_start_block {
                    if cfg_start > first_block {
                        tracing::warn!(
                            "pool_discovery_start_block ({cfg_start}) is later than the first \
                             observed pool-creation block ({first_block}) for factory {factory}"
                        );
                    }
                }
            }
        }
    }

    // Phase 5.1: incremental mode.
    let (from, to) = if incremental && !is_remote_only {
        match cache.max_creation_block() {
            Ok(Some(max_block)) if max_block > 0 => {
                let new_from = max_block + 1;
                if new_from > to {
                    progress.log(&format!(
                        "Incremental mode: cache is up-to-date (max block {max_block}). No scan needed."
                    ));
                    return Ok(DiscoverOutcome {
                        pools_found: 0,
                        active_blocks: 0,
                        remote_count: 0,
                        pools: Vec::new(),
                    });
                }
                progress.log(&format!(
                    "Incremental mode: scanning from block {new_from} (cache max: {max_block})"
                ));
                (new_from, to)
            }
            Ok(_) => {
                progress.log("Incremental mode: no cached pools found, running full scan.");
                (from, to)
            }
            Err(e) => {
                tracing::warn!(
                    "Incremental mode: failed to query cache: {e:#}. Running full scan."
                );
                (from, to)
            }
        }
    } else {
        (from, to)
    };

    progress.log(&format!(
        "Pool discovery — chain {chain_name}, sources: {source}, blocks {from}-{to} (json={json})"
    ));

    // Phase 1: factory/event scan.
    let (all_pools, all_active_blocks) = if is_remote_only {
        (Vec::new(), std::collections::HashSet::new())
    } else {
        let factories = ResolvedFactories::from_chain_config(&chain_config, chain_name);
        let disc_config = factories.discovery_config(DiscoveryRuntimeOpts {
            batch_size: recommended_get_logs_batch(
                &config.effective_rpc(chain_name).rpc_urls,
                batch_size,
            ),
            solidly_fee_bps: opts.solidly_fee_bps.map(|v| v as u32),
            rpc_concurrency,
            token_cache: Some(&token_cache),
            pool_cache: Some(&cache),
        });
        scan_onchain(&rpc, &cache, from, to, &disc_config, progress).await?
    };

    // Phases 2+3: remote sourcing + dedup/merge.
    let mut remote_pools: Vec<DiscoveredPool> = Vec::new();
    if should_fetch_remote {
        remote_pools = fetch_remote(chain_name, Some(max_pools_opt), min_tvl_opt).await;
        progress.log(&format!(
            "Remote aggregators: {} pool(s)",
            remote_pools.len()
        ));
    }

    let mut pools: Vec<DiscoveredPool> = dedup_by_address(all_pools);
    let remote_count = remote_pools.len();
    if is_remote_only {
        pools = dedup_by_address(remote_pools);
    } else if is_hybrid {
        pools = remote_src::merge_pools(pools, remote_pools);
    } else if enrich && !remote_pools.is_empty() {
        enrich_from_remote(&mut pools, &remote_pools);
    }

    // Phase 3.5: resolve missing CL metadata (opt-in).
    if opts.resolve_remote_metadata && !pools.is_empty() {
        let targets: Vec<Address> = pools
            .iter()
            .filter(|p| p.address != Address::ZERO && (p.fee == 0 || p.tick_spacing.is_none()))
            .map(|p| p.address)
            .collect();
        if !targets.is_empty() {
            match crate::rpc::multicall::resolve_pool_metadata(&rpc, &targets, rpc_concurrency)
                .await
            {
                Ok(resolved) => {
                    let mut filled = 0usize;
                    for p in pools.iter_mut() {
                        let Some(m) = resolved.get(&p.address) else {
                            continue;
                        };
                        if p.token0.is_zero() {
                            if let Some(t) = m.token0 {
                                p.token0 = t;
                            }
                        }
                        if p.token1.is_zero() {
                            if let Some(t) = m.token1 {
                                p.token1 = t;
                            }
                        }
                        if p.fee == 0 {
                            if let Some(fee) = m.fee {
                                p.fee = fee;
                            }
                        }
                        if p.tick_spacing.is_none() {
                            p.tick_spacing = m.tick_spacing;
                        }
                        filled += 1;
                    }
                    progress.log(&format!(
                        "Metadata resolution: updated {filled} of {} CL pool(s) via Multicall3",
                        targets.len()
                    ));
                }
                Err(e) => tracing::warn!("Remote metadata resolution failed: {e:#}"),
            }
        }
    }

    // Phase 5.2: health check.
    if health_check && !pools.is_empty() {
        let before = pools.len();
        let (checked, removed) = crate::pool::discovery::health_check_pools(
            &rpc,
            pools,
            rpc_concurrency,
            chain_config.balancer_vault,
        )
        .await;
        if removed > 0 {
            progress.log(&format!(
                "Health check: removed {removed} drained/paused pools ({} remaining)",
                before - removed
            ));
        }
        pools = checked;
    }

    // Phase 5.3: persist merged universe.
    let persisted = persist_universe(&cache, &pools);
    if persisted > 0 {
        progress.log(&format!("Cached {persisted} pool(s) to {cache_path}"));
    }

    progress.log(&format!(
        "Found {} pool(s) in {} active blocks (remote={})",
        pools.len(),
        all_active_blocks.len(),
        remote_count,
    ));

    Ok(DiscoverOutcome {
        pools_found: pools.len(),
        active_blocks: all_active_blocks.len(),
        remote_count,
        pools,
    })
}

async fn resolve_scan_window(
    rpc: &RpcClient,
    config: &Config,
    chain_config: &ChainConfig,
    chain_name: ChainName,
    is_remote_only: bool,
) -> anyhow::Result<(u64, u64)> {
    match config.range_spec() {
        Ok(mode) => {
            let resolver = RangeResolver::new(rpc.clone());
            let resolved = resolver.resolve(&mode.resolve()).await?;
            Ok((resolved.start_block, resolved.end_block))
        }
        Err(e) => {
            let err_msg = e.to_string();
            if !err_msg.contains("no block range specified") {
                anyhow::bail!("{err_msg}");
            }
            if is_remote_only {
                Ok((0u64, rpc.get_block_number().await.unwrap_or(0)))
            } else {
                let to = rpc.get_block_number().await?;
                // Prefer a lookback window (last N blocks) over an absolute
                // historical start — keeps default hybrid/on-chain discovery fast.
                if let Some(lookback) = chain_config.pool_discovery_lookback_blocks {
                    if lookback == 0 {
                        anyhow::bail!(
                            "pool_discovery_lookback_blocks must be >= 1 for chain '{chain_name}'"
                        );
                    }
                    let from = to.saturating_sub(lookback.saturating_sub(1));
                    tracing::info!(
                        "No block range specified. Using last {lookback} blocks ({from}-{to}) for chain '{chain_name}'."
                    );
                    Ok((from, to))
                } else if let Some(from) = chain_config.pool_discovery_start_block {
                    tracing::info!(
                        "No block range specified. Using pool_discovery_start_block ({from}) from config."
                    );
                    Ok((from, to))
                } else {
                    anyhow::bail!(
                        "no block range specified and neither pool_discovery_lookback_blocks nor \
                         pool_discovery_start_block configured for chain '{chain_name}'"
                    )
                }
            }
        }
    }
}
