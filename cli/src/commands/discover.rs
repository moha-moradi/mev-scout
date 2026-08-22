use anyhow::Context;
use std::collections::HashSet;

use alloy::primitives::Address;
use indicatif::{ProgressBar, ProgressStyle};

use crate::cli::{DiscoverArgs, DiscoverySource};
use crate::rpc_setup::init_rpc;
use mev_scout_core::cache::{SqliteStore, TokenCache};
use mev_scout_core::config::validation;
use mev_scout_core::config::Config;
use mev_scout_core::pool::discovery::{DiscoveryConfig, DiscoveredPool};
use mev_scout_core::pool::discovery::remote as remote_src;
use mev_scout_core::types::{ChainName, SubgraphConfig};
use mev_scout_core::config::ChainConfig;
use mev_scout_core::dex_type::DexType;
use mev_scout_core::resolver::RangeResolver;
use mev_scout_core::rpc::recommended_get_logs_batch;

/// Resolve subgraph configs: `chains.toml` overrides → hardcoded defaults.
pub(crate) fn resolve_subgraphs(chain_name: &ChainName, chain_config: &ChainConfig) -> Vec<SubgraphConfig> {
    if chain_config.subgraphs.is_empty() {
        chain_name.default_subgraphs()
    } else {
        chain_config.subgraphs.clone()
    }
}

/// Fetch remote pools with graceful degradation:
/// subgraphs → aggregators (GeckoTerminal, DefiLlama) → empty vec (caller falls back to RPC).
pub(crate) async fn fetch_remote(
    chain_name: &ChainName,
    subgraphs: &[SubgraphConfig],
    max_pools: Option<usize>,
    min_tvl: Option<f64>,
) -> Vec<DiscoveredPool> {
    let mut remote = if subgraphs.is_empty() {
        Vec::new()
    } else {
        remote_src::discover_via_remote(subgraphs, max_pools, min_tvl).await
    };
    if !remote.is_empty() {
        return remote;
    }
    tracing::warn!("All subgraph sources failed/empty — falling back to free aggregators");
    let slug = &chain_name.to_string();
    remote = remote_src::discover_via_geckoterminal(slug, max_pools, min_tvl).await;
    if !remote.is_empty() {
        return remote;
    }
    remote = remote_src::discover_via_defillama(slug, max_pools, min_tvl).await;
    if remote.is_empty() {
        tracing::warn!("All remote sources failed — no off-chain pools available");
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

pub async fn cmd_discover(config: &Config, args: &DiscoverArgs) -> anyhow::Result<()> {
    let (chain_name, chain_config) = validation::resolve_chain(config)
        .context("failed to resolve chain configuration")?;
    let chain_id = chain_name.chain_id();

    let subgraphs = resolve_subgraphs(&chain_name, &chain_config);
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
            println!("  Sources:     remote subgraphs (on-chain scan skipped)");
        } else if is_hybrid {
            println!("  Block range: {}–{}", from, to);
            println!("  Sources:     hybrid (on-chain + remote union)");
        } else if args.enrich {
            println!("  Block range: {}–{}", from, to);
            println!("  Sources:     on-chain + remote enrichment");
        } else {
            println!("  Block range: {}–{}", from, to);
            println!("  Sources:     on-chain events (factory logs)");
        }
        if should_fetch_remote && !subgraphs.is_empty() {
            let tvl_note = min_tvl_opt.map(|v| format!(" min-tvl ${v:.0}")).unwrap_or_default();
            println!("  Remote:      {} subgraph(s), cap {}{}", subgraphs.len(), args.max_pools, tvl_note);
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

    // ── Phase 2: Remote sourcing (subgraphs + aggregator fallback ladder) ──
    let mut remote_pools: Vec<DiscoveredPool> = Vec::new();
    if should_fetch_remote {
        remote_pools = fetch_remote(&chain_name, &subgraphs, max_pools_opt, min_tvl_opt).await;
        if remote_pools.is_empty() && (is_remote_only || is_hybrid) {
            if !args.json {
                eprintln!("  Warning: all remote sources returned 0 pools — falling back to RPC-only results");
            }
            tracing::warn!("All remote sources (subgraph, GeckoTerminal, DefiLlama) returned 0 pools");
        }
    }

    // ── Phase 3: Dedup by address with field-level merge ──
    let mut pools: Vec<DiscoveredPool> = Vec::with_capacity(all_pools.len() + remote_pools.len());
    for p in all_pools {
        match pools.iter_mut().find(|e| e.address == p.address) {
            Some(existing) => existing.merge_from(&p),
            None => pools.push(p),
        }
    }

    // Merge remote according to --source semantics
    if is_remote_only {
        // Remote-only: replace (or extend if we had no on-chain). Deduplicate remote internally.
        let mut remote_dedup: Vec<DiscoveredPool> = Vec::with_capacity(remote_pools.len());
        for p in remote_pools {
            match remote_dedup.iter_mut().find(|e| e.address == p.address) {
                Some(existing) => existing.merge_from(&p),
                None => remote_dedup.push(p),
            }
        }
        pools = remote_dedup;
    } else if is_hybrid {
        // Hybrid: union with dedup
        pools = remote_src::merge_pools(pools, remote_pools);
    } else if args.enrich && !remote_pools.is_empty() {
        // Onchain+enrich: attach tvl/volume/symbols where missing, do not add new addresses
        enrich_from_remote(&mut pools, &remote_pools);
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
