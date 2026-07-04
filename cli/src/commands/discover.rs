use std::collections::HashSet;

use alloy::primitives::Address;

use crate::cli::DiscoverArgs;
use mev_scout_core::cache::SqliteStore;
use mev_scout_core::config::Config;
use mev_scout_core::dune::DuneClient;
use mev_scout_core::pool::discovery::DiscoveredPool;
use mev_scout_core::pool::dex_type::DexType;
use mev_scout_core::rpc::RpcClient;
use mev_scout_core::types::ChainName;

pub async fn cmd_discover(config: &Config, args: &DiscoverArgs) -> anyhow::Result<()> {
    let chain_name: ChainName = match args.chain_args.chain.parse() {
        Ok(c) => c,
        Err(_) => anyhow::bail!("Error: unknown chain '{}'", args.chain_args.chain),
    };
    let chain_config = config.chains.get(&args.chain_args.chain);
    let chain_id = chain_name.chain_id();

    let source = args.source.to_lowercase();
    let use_onchain = source == "onchain" || source == "all";
    let use_dune = (source == "dune" || source == "all") && config.dune_api_key.is_some();

    let provider_configs = config.effective_provider_configs(chain_name)?;
    let rpc_refs: Vec<&str> = provider_configs.iter().map(|(u, _)| u.as_str()).collect();
    let rpc = RpcClient::from_urls(&rpc_refs, chain_id)?;
    rpc.with_provider_rps(
        &provider_configs.iter().map(|(_, r)| r.unwrap_or(1.0)).collect::<Vec<_>>(),
    ).await;
    if use_onchain {
        rpc.check_connection(chain_id).await?;
    }

    let from = chain_config
        .and_then(|c| c.pool_discovery_start_block)
        .unwrap_or(0);
    let to = rpc.get_block_number().await?;

    println!();
    println!("  Pool Discovery");
    println!("  Chain:       {}", args.chain_args.chain);
    println!("  Block range: {}–{}", from, to);
    let sources: Vec<&str> = {
        let mut v = Vec::new();
        if use_dune { v.push("Dune Analytics"); }
        if use_onchain { v.push("on-chain events"); }
        v
    };
    println!("  Sources:     {}", sources.join(" + "));
    if args.no_save {
        println!("  Save to cache: no");
    }
    println!();

    let mut all_pools: Vec<DiscoveredPool> = Vec::new();
    let mut all_active_blocks = HashSet::new();

    // ── Phase 1: Dune Analytics discovery ──
    if use_dune {
        let api_key = config.dune_api_key.as_ref().expect("dune_api_key checked above");
        let dune = DuneClient::new(api_key.clone());
        tracing::info!("Starting Dune pool discovery for {}", args.chain_args.chain);

        if let Some(qid) = config.dune_v2_pools_query_id {
            let fee = chain_config.and_then(|c| c.uniswap_v2_default_fee).unwrap_or(30);
            match mev_scout_core::dune::pool_discovery::discover_v2_pools_from_dune(
                &dune, qid, &args.chain_args.chain, from, to, fee,
            ).await {
                Ok(pools) => {
                    tracing::info!("Dune V2: found {} pools", pools.len());
                    all_pools.extend(pools);
                }
                Err(e) => eprintln!("  Warning: Dune V2 discovery failed: {e:#}"),
            }
        }
        if let Some(qid) = config.dune_v3_pools_query_id {
            match mev_scout_core::dune::pool_discovery::discover_v3_pools_from_dune(
                &dune, qid, &args.chain_args.chain, from, to,
            ).await {
                Ok(pools) => {
                    tracing::info!("Dune V3: found {} pools", pools.len());
                    all_pools.extend(pools);
                }
                Err(e) => eprintln!("  Warning: Dune V3 discovery failed: {e:#}"),
            }
        }
        if let Some(qid) = config.dune_active_pools_query_id {
            match mev_scout_core::dune::pool_discovery::discover_active_pools_from_dune(
                &dune, qid, &args.chain_args.chain, from, to,
            ).await {
                Ok(pools) => {
                    tracing::info!("Dune active pools: found {} pools", pools.len());
                    all_pools.extend(pools);
                }
                Err(e) => eprintln!("  Warning: Dune active pool discovery failed: {e:#}"),
            }
        }
    }

    // ── Phase 2: On-chain event scan discovery ──
    if use_onchain {
        let batch_size = args.batch_size;
        let v2_fee = chain_config.and_then(|c| c.uniswap_v2_default_fee);
        let vault = chain_config
            .and_then(|c| c.balancer_vault.as_ref())
            .and_then(|s| s.parse::<Address>().ok());
        let registry = chain_config
            .and_then(|c| c.curve_registry.as_ref())
            .and_then(|s| s.parse::<Address>().ok());

        let v2_factories: Vec<Address> = if let Some(v2_str) = &args.v2_factories {
            v2_str.split(',').filter_map(|s| s.trim().parse().ok()).collect()
        } else if let Some(factories) = chain_config.and_then(|c| c.uniswap_v2_factories.as_ref()) {
            factories.iter().filter_map(|s| s.parse().ok()).collect()
        } else {
            chain_name.default_uniswap_v2_factories().iter().filter_map(|s| s.parse().ok()).collect()
        };
        let v3_factories: Vec<Address> = if let Some(v3_str) = &args.v3_factory {
            v3_str.split(',').filter_map(|s| s.trim().parse().ok()).collect()
        } else if let Some(factories) = chain_config.and_then(|c| c.uniswap_v3_factories.as_ref()) {
            factories.iter().filter_map(|s| s.parse().ok()).collect()
        } else {
            chain_name.default_uniswap_v3_factories().iter().filter_map(|s| s.parse().ok()).collect()
        };

        if !v2_factories.is_empty() || !v3_factories.is_empty() || vault.is_some() || registry.is_some() {
            tracing::info!("On-chain: {} V2 factories, {} V3 factories, Balancer: {}, Curve: {}",
                v2_factories.len(), v3_factories.len(), vault.is_some(), registry.is_some());
        }

        let cache = SqliteStore::open(&config.effective_db_path(&chain_name), chain_id)?;

        match mev_scout_core::pool::discovery::discover_and_cache(
            &rpc, &cache, from, to, batch_size, v2_fee, vault,
            if v2_factories.is_empty() { None } else { Some(v2_factories.as_slice()) },
            if v3_factories.is_empty() { None } else { Some(v3_factories.as_slice()) },
            None, registry,
        ).await {
            Ok((pools, active_blocks)) => {
                tracing::info!("On-chain: found {} pools in {} active blocks", pools.len(), active_blocks.len());
                all_pools.extend(pools);
                all_active_blocks.extend(active_blocks);
            }
            Err(e) => eprintln!("  On-chain pool discovery failed: {e:#}"),
        }
    }

    // ── Phase 3: Dedup by address ──
    let mut seen = HashSet::new();
    let mut pools: Vec<DiscoveredPool> = Vec::with_capacity(all_pools.len());
    for p in all_pools {
        if seen.insert(p.address) {
            pools.push(p);
        }
    }

    // ── Phase 4: Display & cache ──
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
            DexType::Balancer => {
                println!("  Balancer  {}", p.address);
            }
            DexType::Curve => {
                println!("  Curve  {}", p.address);
            }
        }
    }
    println!();
    println!("  Found {} pool(s) in {} active blocks", pools.len(), all_active_blocks.len());

    if !args.no_save && (use_onchain || !pools.is_empty()) {
        let cache_path = config.effective_db_path(&chain_name);
        if let Ok(cache) = SqliteStore::open(&cache_path, chain_id) {
            let mut saved = 0usize;
            for p in &pools {
                let info: mev_scout_core::pool::state::PoolInfo = p.clone().into();
                if cache.put_discovered_pool(&info).is_ok() {
                    saved += 1;
                }
            }
            if saved > 0 {
                println!("  Saved {saved} pool(s) to cache: {cache_path}");
            }
        }
    }

    Ok(())
}
