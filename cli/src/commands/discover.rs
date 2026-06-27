use alloy::primitives::Address;

use crate::cli::DiscoverArgs;
use mev_scout_core::cache::SqliteStore;
use mev_scout_core::config::Config;
use mev_scout_core::rpc::RpcClient;
use mev_scout_core::types::ChainName;

pub async fn cmd_discover(config: &Config, args: &DiscoverArgs) -> anyhow::Result<()> {
    let chain_name: ChainName = match args.chain_args.chain.parse() {
        Ok(c) => c,
        Err(_) => anyhow::bail!("Error: unknown chain '{}'", args.chain_args.chain),
    };
    let chain_config = config.chains.get(&args.chain_args.chain);
    let chain_id = chain_name.chain_id();
    let provider_configs = config.effective_provider_configs(chain_name)?;
    let rpc_refs: Vec<&str> = provider_configs.iter().map(|(u, _)| u.as_str()).collect();
    let rpc = RpcClient::from_urls(&rpc_refs, chain_id)?;
    rpc.with_provider_rps(&provider_configs.iter().map(|(_, r)| r.unwrap_or(1.0)).collect::<Vec<_>>()).await;
    rpc.check_connection(chain_id).await?;

    let from = args.from_block;
    let to = args.to_block;
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

    println!();
    println!("  Pool Discovery (unified)");
    println!("  Chain:       {}", args.chain_args.chain);
    println!("  Block range: {from}–{to}");
    println!("  DEX activity scan: yes");
    if !v2_factories.is_empty() || !v3_factories.is_empty() || vault.is_some() || registry.is_some() {
        println!("  Factory event scan: yes ({} V2, {} V3, Balancer: {}, Curve: {})",
            v2_factories.len(), v3_factories.len(), vault.is_some(), registry.is_some());
    }
    if args.no_save {
        println!("  Save to cache: no");
    }
    println!();

    let cache = SqliteStore::open(&config.db_path, chain_id)?;

    match mev_scout_core::pool::discovery::discover_and_cache(
        &rpc,
        &cache,
        from,
        to,
        batch_size,
        v2_fee,
        vault,
        if v2_factories.is_empty() { None } else { Some(v2_factories.as_slice()) },
        if v3_factories.is_empty() { None } else { Some(v3_factories.as_slice()) },
        None,
        registry,
    )
    .await
    {
        Ok((pools, active_blocks)) => {
            for p in &pools {
                match p.dex_type {
                    mev_scout_core::pool::dex_type::DexType::UniswapV2 => {
                        println!("  V2  {}  token0={}  token1={}", p.address, p.token0, p.token1);
                    }
                    mev_scout_core::pool::dex_type::DexType::UniswapV3 => {
                        println!("  V3  {}  token0={}  token1={}  fee={}  tickSpacing={}",
                            p.address, p.token0, p.token1, p.fee, p.tick_spacing.unwrap_or(0));
                    }
                    mev_scout_core::pool::dex_type::DexType::Balancer => {
                        println!("  Balancer  {}", p.address);
                    }
                    mev_scout_core::pool::dex_type::DexType::Curve => {
                        println!("  Curve  {}", p.address);
                    }
                }
            }
            println!();
            println!("  Found {} pool(s) in {} active blocks", pools.len(), active_blocks.len());
            if args.no_save {
                println!("  (not saved to cache)");
            } else {
                println!("  Saved {} pool(s) to cache: {}", pools.len(), config.db_path);
            }
        }
        Err(e) => {
            eprintln!("  Pool discovery failed: {e:#}");
        }
    }

    Ok(())
}
