use std::collections::HashSet;

use alloy::primitives::Address;
use indicatif::{ProgressBar, ProgressStyle};

use crate::cli::DiscoverArgs;
use mev_scout_core::cache::SqliteStore;
use mev_scout_core::config::validation;
use mev_scout_core::config::Config;
use mev_scout_core::dune::DuneClient;
use mev_scout_core::pool::discovery::{DiscoveryConfig, DiscoveredPool};
use mev_scout_core::pool::dex_type::DexType;
use mev_scout_core::resolver::RangeResolver;
use mev_scout_core::rpc::RpcClient;

pub async fn cmd_discover(config: &Config, args: &DiscoverArgs) -> anyhow::Result<()> {
    let (chain_name, chain_config) = validation::resolve_chain(config)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let chain_id = chain_name.chain_id();

    let source = args.source.to_lowercase();
    let use_onchain = source == "onchain" || source == "all";
    let use_dune = (source == "dune" || source == "all") && config.dune_api_key.is_some();

    if args.batch_size > 5000 {
        eprintln!("  Warning: batch_size={} exceeds recommended maximum of 5000 for public RPCs. \
                   Free-tier endpoints (drpc, Ankr, CloudFlare) typically cap eth_getLogs at 5K–10K blocks. \
                   Consider using --batch-size 2000 for best results.", args.batch_size);
    }

    let provider_configs = config.effective_provider_configs(chain_name)?;
    validation::validate_rpc_urls(
        &provider_configs.iter().map(|(u, _)| u.clone()).collect::<Vec<_>>(),
    ).map_err(|e| anyhow::anyhow!("{}", e))?;
    let rpc_refs: Vec<&str> = provider_configs.iter().map(|(u, _)| u.as_str()).collect();
    let rpc = RpcClient::from_urls(&rpc_refs, chain_id)?;
    rpc.with_provider_rps(
        &provider_configs.iter().map(|(_, r)| r.unwrap_or(config.rps_limit)).collect::<Vec<_>>(),
    ).await;
    if use_onchain {
        rpc.check_connection(chain_id).await?;
    }

    // Determine block range: CLI flags override pool_discovery_start_block
    let (from, to) = match validation::resolve_block_range(
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
                        "No block range specified and no pool_discovery_start_block configured for chain '{}'. \
                         Use --days, --blocks, --block, or --from-block/--to-block.",
                        chain_name
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
    };

    // ── Open cache once and reuse ──
    let cache_path = config.effective_db_path(&chain_name);
    let cache = SqliteStore::open(&cache_path, chain_id)?;

    // ── Phase 5.1: Incremental mode — override from_block from cache ──
    let (from, to) = if args.incremental {
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
        println!("  Block range: {}–{}", from, to);
        let sources: Vec<&str> = {
            let mut v = Vec::new();
            if use_dune { v.push("Dune Analytics"); }
            if use_onchain { v.push("on-chain events"); }
            v
        };
        println!("  Sources:     {}", sources.join(" + "));
        println!();
    }

    let total_blocks = to - from + 1;
    let pb = ProgressBar::new(total_blocks);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("  [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} blocks ({eta})")?
            .progress_chars("=> "),
    );
    let tick = || pb.inc(1);

    let mut all_pools: Vec<DiscoveredPool> = Vec::new();
    let mut all_active_blocks = HashSet::new();

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
        batch_size: args.batch_size,
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
    };

    // ── Phase 2: Dune Analytics discovery (runs first to support --min-pools) ──
    if use_dune {
        let api_key = config.dune_api_key.as_ref().expect("dune_api_key checked above");
        let dune = DuneClient::new(api_key.clone());
        tracing::info!("Starting Dune pool discovery for {}", chain_name);

        let dune_pb = ProgressBar::new_spinner();
        dune_pb.set_style(
            ProgressStyle::default_spinner()
                .template("  {spinner:.cyan} Dune: {msg}")?
                .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
        );
        dune_pb.set_message("querying V2 pools...");
        let dune_fee = chain_config.uniswap_v2_default_fee.unwrap_or(30);
        match mev_scout_core::dune::pool_discovery::discover_v2_pools_from_dune(
            &dune, &chain_name.to_string(), from, to, dune_fee,
        ).await {
            Ok(pools) => {
                tracing::info!("Dune V2: found {} pools", pools.len());
                all_pools.extend(pools);
            }
            Err(e) => eprintln!("  Warning: Dune V2 discovery failed: {e:#}"),
        }

        dune_pb.set_message("querying V3 pools...");
        dune_pb.tick();
        match mev_scout_core::dune::pool_discovery::discover_v3_pools_from_dune(
            &dune, &chain_name.to_string(), from, to,
        ).await {
            Ok(pools) => {
                tracing::info!("Dune V3: found {} pools", pools.len());
                all_pools.extend(pools);
            }
            Err(e) => eprintln!("  Warning: Dune V3 discovery failed: {e:#}"),
        }

        dune_pb.set_message("querying active pools...");
        dune_pb.tick();
        match mev_scout_core::dune::pool_discovery::discover_active_pools_from_dune(
            &dune, &chain_name.to_string(), from, to,
        ).await {
            Ok(pools) => {
                tracing::info!("Dune active pools: found {} pools", pools.len());
                all_pools.extend(pools);
            }
            Err(e) => eprintln!("  Warning: Dune active pool discovery failed: {e:#}"),
        }
        dune_pb.finish_and_clear();

        // ── Phase 5.3: --min-pools early exit ──
        if args.min_pools > 0 && all_pools.len() >= args.min_pools {
            if !args.json {
                println!("  Skipping on-chain scan: {} Dune pools >= --min-pools threshold ({})",
                    all_pools.len(), args.min_pools);
            }
            tracing::info!("Skipping on-chain scan: {} Dune pools >= --min-pools {}", all_pools.len(), args.min_pools);
        } else {
            // ── Phase 1: On-chain event scan discovery ──
            if use_onchain {
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
            }
        }
    } else {
        // ── Phase 1: On-chain event scan discovery (no Dune) ──
        if use_onchain {
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
        }
    }
    pb.finish_and_clear();

    // ── Phase 3: Dedup by address (on-chain pools take priority) ──
    let mut seen = HashSet::new();
    let mut pools: Vec<DiscoveredPool> = Vec::with_capacity(all_pools.len());
    for p in all_pools {
        if seen.insert(p.address) {
            pools.push(p);
        }
    }

    // ── Phase 5.2: Pool health check ──
    if args.health_check && !pools.is_empty() {
        let before = pools.len();
        let (checked, removed) = mev_scout_core::pool::discovery::health_check_pools(
            &rpc, pools, args.rpc_concurrency,
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
        if source == "dune" {
            println!("  Dune Only — pool metadata may be partial (token0, token1 only).");
            println!("  Use --source all or --source onchain for full metadata.");
            println!();
        }

        for p in &pools {
            match p.dex_type {
                DexType::UniswapV2 => {
                    println!("  V2  {}  token0={}  token1={}", p.address, p.token0, p.token1);
                }
                DexType::UniswapV3 => {
                    println!("  V3  {}  token0={}  token1={}  fee={}  tickSpacing={}",
                        p.address, p.token0, p.token1, p.fee, p.tick_spacing.unwrap_or(0));
                }
                DexType::Solidly => {
                    println!("  Solidly  {}  token0={}  token1={}", p.address, p.token0, p.token1);
                }
                DexType::Camelot => {
                    println!("  Camelot  {}  token0={}  token1={}", p.address, p.token0, p.token1);
                }
                DexType::Balancer => {
                    println!("  Balancer  {}  token0={}  token1={}", p.address, p.token0, p.token1);
                }
                DexType::Curve => {
                    println!("  Curve  {}  token0={}  token1={}", p.address, p.token0, p.token1);
                }
                DexType::Dodo => {
                    println!("  Dodo  {}  token0={}  token1={}", p.address, p.token0, p.token1);
                }
                DexType::Clipper => {
                    println!("  Clipper  {}  token0={}  token1={}", p.address, p.token0, p.token1);
                }
                DexType::UniswapV4 => {
                    println!("  V4  {}  token0={}  token1={}  fee={}  tickSpacing={}",
                        p.address, p.token0, p.token1, p.fee, p.tick_spacing.unwrap_or(0));
                }
                DexType::TraderJoeLB => {
                    println!("  TraderJoeLB  {}  token0={}  token1={}  binStep={}",
                        p.address, p.token0, p.token1, p.bin_step.unwrap_or(0));
                }
                DexType::Pendle => {
                    println!("  Pendle  {}  token0={}  token1={}  maturity={}",
                        p.address, p.token0, p.token1, p.maturity_timestamp.unwrap_or(0));
                }
            }
        }
        println!();
        println!("  Found {} pool(s) in {} active blocks", pools.len(), all_active_blocks.len());
    }

    Ok(())
}
