use anyhow::Context;
use std::collections::{HashMap, HashSet};

use alloy::primitives::Address;
use indicatif::{ProgressBar, ProgressStyle};

use crate::cli::{DiscoverArgs, DiscoverySource};
use crate::rpc_setup::init_rpc;
use mev_scout_core::cache::{SqliteStore, TokenCache};
use mev_scout_core::config::validation;
use mev_scout_core::config::Config;
use mev_scout_core::pool::discovery::{DiscoveryConfig, DiscoveredPool};
use mev_scout_core::pool::discovery::remote as remote_src;
use mev_scout_core::pool::state::PoolInfo;
use mev_scout_core::types::ChainName;
use mev_scout_core::dex_type::DexType;
use mev_scout_core::resolver::RangeResolver;
use mev_scout_core::rpc::recommended_get_logs_batch;

/// Fetch remote pools from free aggregators (GeckoTerminal + DexScreener).
/// Empty vec on failure (caller falls back to RPC).
/// When `show_progress` is set, renders a progress bar on stderr.
pub(crate) async fn fetch_remote(
    chain_name: &ChainName,
    max_pools: Option<usize>,
    min_tvl: Option<f64>,
    show_progress: bool,
) -> Vec<DiscoveredPool> {
    let bar = if show_progress {
        let pb = ProgressBar::new_spinner();
        let style = ProgressStyle::default_bar()
            .template("  {spinner:.cyan} {msg} ({pos} pools)")
            .unwrap_or_else(|_| ProgressStyle::default_bar())
            .progress_chars("=> ");
        pb.set_style(style);
        pb.enable_steady_tick(std::time::Duration::from_millis(120));
        Some(pb)
    } else {
        None
    };

    let slug = &chain_name.to_string();

    if let Some(ref pb) = bar {
        pb.set_message("remote aggregators");
    }
    let remote = remote_src::discover_via_remote(slug, max_pools, min_tvl).await;
    if remote.is_empty() {
        tracing::warn!("Remote aggregators returned 0 pools — no off-chain pool source available");
        if let Some(ref pb) = bar {
            pb.abandon_with_message("remote aggregators returned 0 pools");
        }
    } else if let Some(ref pb) = bar {
        pb.finish_with_message(format!("{} pools via remote aggregators", remote.len()));
    }
    remote
}

/// Merge enrichment metrics (tvl/volume/symbols) from remote pools into local pools.
/// Only fills fields missing on the on-chain side (`merge_from` semantics).
fn enrich_from_remote(pools: &mut [DiscoveredPool], remote: &[DiscoveredPool]) {
    use std::collections::HashMap;
    let by_addr: HashMap<Address, &DiscoveredPool> =
        remote.iter().map(|p| (p.address, p)).collect();
    let mut enriched = 0usize;
    for p in pools.iter_mut() {
        if p.tvl_usd.is_none() {
            if let Some(r) = by_addr.get(&p.address) {
                p.merge_from(r);
                enriched += 1;
            }
        }
    }
    tracing::info!("Enrichment: {enriched} pool(s) received TVL/volume metrics from remote sources");
}

/// Dedup pools by address with field-level merge — HashMap-indexed, O(n)
/// instead of the previous O(n²) linear scan per entry.
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

/// Reconstruct a `DiscoveredPool` from a cached `PoolInfo` row.
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

/// Persist the merged pool universe so remote/hybrid unions survive across
/// runs. Zero-token entries are skipped (unusable downstream). Each write is a
/// cache-first merge: richer data already stored by a previous run (on-chain
/// metadata, symbols) is never clobbered by a sparser remote entry.
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

pub async fn cmd_discover(config: &Config, args: &DiscoverArgs) -> anyhow::Result<()> {
    let (chain_name, chain_config) = validation::resolve_chain(config)
        .context("failed to resolve chain configuration")?;
    let chain_id = chain_name.chain_id();

    let is_remote_only = matches!(args.source, DiscoverySource::Remote);
    let is_hybrid = matches!(args.source, DiscoverySource::Hybrid);
    let should_fetch_remote = is_remote_only || is_hybrid || args.enrich;

    // min_tvl: 0 => no filter (keep parity with explorer dust)
    let min_tvl_opt = if args.min_tvl > 0.0 {
        Some(args.min_tvl)
    } else {
        None
    };
    let max_pools_opt = Some(args.max_pools);

    if args.batch_size > 5000 {
        eprintln!("  Warning: batch_size={} exceeds recommended maximum of 5000 for public RPCs. \
                   Free-tier endpoints (drpc, Ankr, CloudFlare) typically cap eth_getLogs at 5K–10K blocks. \
                   Consider using --batch-size 2000 for best results.", args.batch_size);
    }

    // RPC is required for on-chain legs and for health_check (even in remote-only mode).
    // init_rpc will use public fallbacks if no URL is configured, so it never
    // hard-fails just because user didn't set --rpc in remote mode.
    let setup = init_rpc(config, chain_name.clone(), true).await?;
    let rpc = setup.rpc;

    // ── Block range — not needed for pure remote mode ──
    let (from, to) = if is_remote_only {
        match validation::resolve_block_range(
            config.days, config.blocks, config.block, config.from_block, config.to_block,
        ) {
            Ok(mode) => {
                let resolver = RangeResolver::new(rpc.clone());
                let resolved = resolver.resolve(&mode).await?;
                (resolved.start_block, resolved.end_block)
            }
            Err(e) => {
                let err_msg = e.to_string();
                if err_msg.contains("no block range specified") {
                    // Remote-only: no window needed; keep tip for health checks.
                    (0u64, rpc.get_block_number().await.unwrap_or(0))
                } else {
                    anyhow::bail!("{}", e);
                }
            }
        }
    } else {
        match validation::resolve_block_range(
            config.days, config.blocks, config.block, config.from_block, config.to_block,
        ) {
            Ok(mode) => {
                let resolver = RangeResolver::new(rpc.clone());
                let resolved = resolver.resolve(&mode).await?;
                (resolved.start_block, resolved.end_block)
            }
            Err(e) => {
                let err_msg = e.to_string();
                if err_msg.contains("no block range specified") {
                    let from = chain_config.pool_discovery_start_block
                        .ok_or_else(|| anyhow::anyhow!(
                            "no block range specified and no pool_discovery_start_block configured for chain '{chain_name}' \
                             (use --days, --blocks, --block, or --from-block/--to-block)"
                        ))?;
                    let to = rpc.get_block_number().await?;
                    tracing::info!(
                        "No block range specified. Using pool_discovery_start_block ({}) from config.",
                        from
                    );
                    (from, to)
                } else {
                    anyhow::bail!("{}", e);
                }
            }
        }
    };

    // ── Open cache once and reuse ──
    let cache_path = config.effective_db_path(&chain_name);
    let cache = SqliteStore::open(&cache_path)?;

    // ── Load token symbol cache (SQLite + pre-populated known tokens) ──
    let mut token_cache = TokenCache::warm(chain_id);
    match TokenCache::load(&cache) {
        Ok(persisted) => token_cache.merge(persisted),
        Err(e) => tracing::warn!("Failed to load token cache from SQLite: {e:#}"),
    }

    // ── Phase 5.1: Incremental mode — override from_block from cache (RPC-only) ──
    let (from, to) = if args.incremental && !is_remote_only {
        match cache.max_creation_block() {
            Ok(Some(max_block)) if max_block > 0 => {
                let new_from = max_block + 1;
                if new_from > to {
                    if !args.json {
                        println!("  Incremental mode: cache is up-to-date (max block {}). No scan needed.", max_block);
                    }
                    return Ok(());
                }
                if !args.json {
                    println!("  Incremental mode: scanning from block {} (cache max: {})", new_from, max_block);
                }
                tracing::info!("Incremental scan: cache max_block={}, scanning {} → {}", max_block, new_from, to);
                (new_from, to)
            }
            Ok(_) => {
                if !args.json {
                    println!("  Incremental mode: no cached pools found, running full scan.");
                }
                (from, to)
            }
            Err(e) => {
                tracing::warn!("Incremental mode: failed to query cache: {e:#}. Running full scan.");
                (from, to)
            }
        }
    } else {
        (from, to)
    };

    if !args.json {
        println!();
        println!("  Pool Discovery");
        println!("  Chain:       {}", chain_name);
        if is_remote_only {
            println!("  Sources:     remote aggregators: GeckoTerminal + DexScreener (on-chain scan skipped)");
        } else if is_hybrid {
            println!("  Block range: {}–{}", from, to);
            println!("  Sources:     hybrid (on-chain + remote aggregator union)");
        } else if args.enrich {
            println!("  Block range: {}–{}", from, to);
            println!("  Sources:     on-chain + remote enrichment");
        } else {
            println!("  Block range: {}–{}", from, to);
            println!("  Sources:     on-chain events (factory logs)");
        }
        if should_fetch_remote {
            let tvl_note = min_tvl_opt.map(|v| format!(" min-tvl ${v:.0}")).unwrap_or_default();
            println!("  Remote:      GeckoTerminal + DexScreener, cap {}{}", args.max_pools, tvl_note);
        }
        println!();
    }

    // Progress bar: only meaningful for on-chain scan
    let total_blocks = if is_remote_only { 1 } else { to.saturating_sub(from) + 1 };
    let _pb = if !is_remote_only {
        let pb = ProgressBar::new(total_blocks);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("  [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} blocks ({eta})")?
                .progress_chars("=> "),
        );
        Some(pb)
    } else {
        None
    };
    // Tick closure for discover_and_cache (increment once per batch)
    let tick = || {
        if let Some(ref pb) = _pb { pb.inc(1); }
    };

    let mut all_pools: Vec<DiscoveredPool> = Vec::new();
    let mut all_active_blocks = HashSet::new();

    // ── Phase 1: On-chain event scan discovery (unless --source remote) ──
    if !is_remote_only {
        // ── Factory address resolution ──
        let vault = chain_config
            .balancer_vault
            .as_ref()
            .and_then(|s| s.parse::<Address>().ok());
        let registry = chain_config
            .curve_registry
            .as_ref()
            .and_then(|s| s.parse::<Address>().ok());

        let v2_factories: Vec<Address> = if let Some(factories) = chain_config.uniswap_v2_factories.as_ref() {
            factories.iter().filter_map(|s| s.parse().ok()).collect()
        } else {
            chain_name.default_uniswap_v2_factories().iter().filter_map(|s| s.parse().ok()).collect()
        };
        let v3_factories: Vec<Address> = if let Some(factories) = chain_config.uniswap_v3_factories.as_ref() {
            factories.iter().filter_map(|s| s.parse().ok()).collect()
        } else {
            chain_name.default_uniswap_v3_factories().iter().filter_map(|s| s.parse().ok()).collect()
        };
        let solidly_factories: Vec<Address> = if let Some(factories) = chain_config.solidly_factories.as_ref() {
            factories.iter().filter_map(|s| s.parse().ok()).collect()
        } else {
            chain_name.default_solidly_factories().iter().filter_map(|s| s.parse().ok()).collect()
        };
        let camelot_factories: Vec<Address> = if let Some(factories) = chain_config.camelot_factories.as_ref() {
            factories.iter().filter_map(|s| s.parse().ok()).collect()
        } else {
            chain_name.default_camelot_factories().iter().filter_map(|s| s.parse().ok()).collect()
        };

        let v4_pool_manager: Option<Address> = chain_config.v4_pool_manager.as_ref()
            .and_then(|s| s.parse::<Address>().ok());

        let trader_joe_factory: Option<Address> = chain_config.trader_joe_factory.as_ref()
            .and_then(|s| s.parse::<Address>().ok());

        let pendle_factory: Option<Address> = chain_config.pendle_factory.as_ref()
            .and_then(|s| s.parse::<Address>().ok());

        if !args.json && (!v2_factories.is_empty() || !v3_factories.is_empty() || vault.is_some() || registry.is_some()
            || !solidly_factories.is_empty() || !camelot_factories.is_empty())
        {
            tracing::info!("Factories: {} V2, {} V3, {} Solidly, {} Camelot, Balancer: {}, Curve: {}",
                v2_factories.len(), v3_factories.len(), solidly_factories.len(), camelot_factories.len(),
                vault.is_some(), registry.is_some());
        }

        let disc_config = DiscoveryConfig {
            batch_size: recommended_get_logs_batch(&config.rpc.rpc_urls, args.batch_size),
            v2_fee_override: chain_config.uniswap_v2_default_fee,
            balancer_vault: vault,
            v2_factories: if v2_factories.is_empty() { None } else { Some(v2_factories.as_slice()) },
            v3_factories: if v3_factories.is_empty() { None } else { Some(v3_factories.as_slice()) },
            curve_registry: registry,
            solidly_factories: if solidly_factories.is_empty() { None } else { Some(solidly_factories.as_slice()) },
            camelot_factories: if camelot_factories.is_empty() { None } else { Some(camelot_factories.as_slice()) },
            solidly_fee_bps: args.solidly_fee_bps,
            v4_pool_manager,
            trader_joe_factory,
            pendle_factory,
            rpc_concurrency: args.rpc_concurrency,
            token_cache: Some(&token_cache),
            pool_cache: Some(&cache),
        };

        match mev_scout_core::pool::discovery::discover_and_cache(
            &rpc, &cache, from, to, &disc_config, Some(&tick),
        ).await {
            Ok((pools, active_blocks)) => {
                tracing::info!("On-chain: found {} pools in {} active blocks", pools.len(), active_blocks.len());
                all_pools.extend(pools);
                all_active_blocks.extend(active_blocks);
            }
            Err(e) => eprintln!("  On-chain pool discovery failed: {e:#}"),
        }
        if let Some(pb) = _pb { pb.finish_and_clear(); }
    }

    // ── Phase 2: Remote sourcing (aggregator fallback ladder) ──
    let mut remote_pools: Vec<DiscoveredPool> = Vec::new();
    if should_fetch_remote {
        remote_pools = fetch_remote(&chain_name, max_pools_opt, min_tvl_opt, !args.json).await;
        if remote_pools.is_empty() && (is_remote_only || is_hybrid) {
            if !args.json {
                eprintln!("  Warning: all remote sources returned 0 pools — falling back to RPC-only results");
            }
            tracing::warn!("Remote sources returned 0 pools — falling back to RPC-only results");
        }
    }

    // ── Phase 3: Dedup by address with field-level merge ──
    let mut pools: Vec<DiscoveredPool> = dedup_by_address(all_pools);

    // Merge remote according to --source semantics
    if is_remote_only {
        // Remote-only: replace (or extend if we had no on-chain). Deduplicate remote internally.
        pools = dedup_by_address(remote_pools);
    } else if is_hybrid {
        // Hybrid: union with dedup
        pools = remote_src::merge_pools(pools, remote_pools);
    } else if args.enrich && !remote_pools.is_empty() {
        // Onchain+enrich: attach tvl/volume/symbols where missing, do not add new addresses
        enrich_from_remote(&mut pools, &remote_pools);
    }

    // ── Phase 3.5: Resolve missing metadata on remote-sourced CL pools (opt-in) ──
    //
    // Remote aggregators don't expose fee/tickSpacing, which breaks quote math
    // for concentrated-liquidity pools. One Multicall3 eth_call resolves ~25
    // pools; results are persisted below and never re-fetched. Gated behind
    // --resolve-remote-metadata so offline/remote-only workflows stay RPC-free.
    if args.resolve_remote_metadata && !pools.is_empty() {
        let targets: Vec<Address> = pools
            .iter()
            .filter(|p| {
                p.dex_type == DexType::UniswapV3
                    && !p.address.is_zero()
                    && (p.fee == 0 || p.tick_spacing.is_none())
            })
            .map(|p| p.address)
            .collect();
        if targets.is_empty() {
            tracing::info!("Remote metadata resolution: 0 candidate pools need RPC");
        } else {
            match mev_scout_core::rpc::multicall::resolve_pool_metadata(
                &rpc,
                &targets,
                args.rpc_concurrency,
            )
            .await
            {
                Ok(resolved) => {
                    let mut filled = 0usize;
                    for p in pools.iter_mut() {
                        let Some(m) = resolved.get(&p.address) else { continue };
                        let before = (p.token0, p.token1, p.fee, p.tick_spacing);
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
                            if let Some(f) = m.fee {
                                p.fee = f;
                            }
                        }
                        if p.tick_spacing.is_none() {
                            p.tick_spacing = m.tick_spacing;
                        }
                        if (p.token0, p.token1, p.fee, p.tick_spacing) != before {
                            filled += 1;
                        }
                    }
                    tracing::info!(
                        "Remote metadata resolution: updated {} of {} candidate pool(s)",
                        filled,
                        targets.len()
                    );
                    if !args.json {
                        println!(
                            "  Metadata resolution: updated {} of {} CL pool(s) via Multicall3",
                            filled,
                            targets.len()
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!("Remote metadata resolution failed: {e:#}");
                }
            }
        }
    }

    // ── Phase 5.2: Pool health check (applies to all sources — remote TVL can be stale) ──
    if args.health_check && !pools.is_empty() {
        let before = pools.len();
        let balancer_vault = chain_config.balancer_vault.as_ref()
            .and_then(|s| s.parse::<Address>().ok());
        let (checked, removed) = mev_scout_core::pool::discovery::health_check_pools(
            &rpc, pools, args.rpc_concurrency,
            balancer_vault,
        ).await;
        pools = checked;
        if removed > 0 && !args.json {
            println!("  Health check: removed {} drained/paused pools ({} remaining)", removed, before - removed);
        }
    }

    // ── Phase 5.3: Persist the merged universe ──
    //
    // Remote/hybrid unions previously vanished between runs because only the
    // core `discover_and_cache` path wrote to SQLite. Persisting here (after
    // health check, cache-first merge per entry) makes incremental mode and
    // downstream scans see the full universe.
    let persisted = persist_universe(&cache, &pools);
    if persisted > 0 {
        if args.json {
            tracing::info!("Persisted {persisted} pool(s) to {cache_path}");
        } else {
            println!("  Cached {persisted} pool(s) to {cache_path}");
        }
    }

    // ── Phase 4: Display & cache ──
    if args.json {
        println!("{}", serde_json::to_string_pretty(&pools)?);
    } else {
        for p in &pools {
            let dex = p.dex_name.as_deref().unwrap_or(match p.dex_type {
                DexType::UniswapV2 => "V2",
                DexType::UniswapV3 => "V3",
                DexType::UniswapV4 => "V4",
                _ => "Pool",
            });
            let t0 = p.token0_symbol.as_deref().unwrap_or("???");
            let t1 = p.token1_symbol.as_deref().unwrap_or("???");
            let tvl_note = p.tvl_usd.map(|v| format!(" tvl=${:.0}", v)).unwrap_or_default();
            match p.dex_type {
                DexType::UniswapV2 => {
                    println!("  {dex}  {}  {}/{}{}", p.address, t0, t1, tvl_note);
                }
                DexType::UniswapV3 | DexType::UniswapV4 => {
                    println!("  {dex}  {}  {}/{}  fee={}  tickSpacing={}{}",
                        p.address, t0, t1, p.fee, p.tick_spacing.unwrap_or(0), tvl_note);
                }
                DexType::Solidly | DexType::Camelot => {
                    let stable = p.is_stable.map(|s| if s { " stable" } else { "" }).unwrap_or("");
                    println!("  {dex}{stable}  {}  {}/{}{}", p.address, t0, t1, tvl_note);
                }
                DexType::Balancer | DexType::Curve => {
                    if let Some(ref tokens) = p.underlying_tokens {
                        let syms: Vec<String> = tokens.iter().map(|t| format!("{}", t)).collect();
                        println!("  {dex}  {}  [{}]{}", p.address, syms.join(", "), tvl_note);
                    } else {
                        println!("  {dex}  {}  {}/{}{}", p.address, t0, t1, tvl_note);
                    }
                }
                DexType::TraderJoeLB => {
                    println!("  {dex}  {}  {}/{}  binStep={}{}",
                        p.address, t0, t1, p.bin_step.unwrap_or(0), tvl_note);
                }
                DexType::Pendle => {
                    println!("  {dex}  {}  {}/{}  maturity={}{}",
                        p.address, t0, t1, p.maturity_timestamp.unwrap_or(0), tvl_note);
                }
            }
        }
        println!();
        if is_remote_only {
            println!("  Found {} pool(s) via remote sources", pools.len());
        } else {
            println!("  Found {} pool(s) in {} active blocks", pools.len(), all_active_blocks.len());
            if (is_hybrid || args.enrich) && !pools.is_empty() {
                let enriched = pools.iter().filter(|p| p.tvl_usd.is_some()).count();
                println!("  Enriched: {} pool(s) with TVL metadata", enriched);
            }
        }
    }

    Ok(())
}
