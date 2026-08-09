use anyhow::Context;
use crate::cli::LiveArgs;
use crate::rpc_setup::init_rpc;
use mev_scout_core::cache::{SqliteStore, TokenCache};
use mev_scout_core::config::Config;
use mev_scout_core::mev::execution::{LiveConfig, LiveRunner};
use mev_scout_core::pipeline::BacktestRunner;
use mev_scout_core::pool::discovery::{discover_and_cache, DiscoveryConfig};
use mev_scout_core::pool::state::PoolManager;
use mev_scout_core::replay::BlockReplayer;
use mev_scout_core::rpc::RpcClient;
use mev_scout_core::types::{ChainName, GasConfig, GasModel, PriceOracleMode, Strategy};

pub async fn cmd_live(config: &Config, args: &LiveArgs) -> anyhow::Result<()> {
    let chain_name: ChainName = match args.chain_args.chain.parse() {
        Ok(c) => c,
        Err(e) => anyhow::bail!("{e}"),
    };
    let chain_id = chain_name.chain_id();

    let setup = init_rpc(config, chain_name.clone(), true).await?;
    let rpc = setup.rpc;

    let cache = SqliteStore::open(&config.effective_db_path(&chain_name))?;

    let strategies = Strategy::from_comma_list(&args.strategies)
        .map_err(anyhow::Error::msg)
        .context("Error parsing strategies")?;

    let gas_model: GasModel = args.gas_model.parse().unwrap_or(GasModel::Live);

    let gas_config = GasConfig {
        gas_limit: args.gas_limit,
        gas_model,
        priority_fee_gwei: args.priority_fee,
        ..GasConfig::default()
    };

    let mut pool_manager = PoolManager::new();
    pool_manager.set_concurrency_limit(setup.provider_configs.len().max(1) as u32);
    if let Some(vault_str) = config.chains.get(&chain_name.to_string())
        .and_then(|c| c.balancer_vault.as_ref())
    {
        if let Ok(vault_addr) = vault_str.parse::<alloy::primitives::Address>() {
            pool_manager = pool_manager.with_balancer_vault(vault_addr);
        }
    }
    if let Some(native_str) = config.chains.get(&chain_name.to_string())
        .and_then(|c| c.wrapped_native_token.as_ref())
    {
        if let Ok(native_addr) = native_str.parse::<alloy::primitives::Address>() {
            pool_manager = pool_manager.with_wrapped_native(native_addr);
        }
    }

    let latest_block = rpc.get_block_number().await.unwrap_or(0);
    let init_block = latest_block;

    // Live mode has no archive requirement: pool state is initialized and
    // validated against the `latest` tag so any full node can serve it.
    pool_manager = pool_manager.with_use_latest(true);

    // Fresh cache (no pools discovered yet): run on-chain discovery so live
    // mode has pools to scan even without a prior `discover` run.
    if cache.count_discovered_pools().unwrap_or(0) == 0 {
        match run_auto_discovery(config, chain_name.clone(), chain_id, &rpc, &cache, latest_block).await {
            Ok(count) if count > 0 => {
                tracing::info!("Auto-discovery: cached {} pools", count);
            }
            Ok(_) => tracing::warn!("Auto-discovery: no pools found in range"),
            Err(e) => tracing::warn!("Auto-discovery failed (continuing with empty pool set): {e:#}"),
        }
    }

    if !strategies.is_empty() {
        BacktestRunner::init_pools(
            &mut pool_manager,
            &rpc,
            init_block,
            Some(&cache),
        ).await;
    }

    let replayer = BlockReplayer::new(
        tokio::runtime::Handle::current(),
        cache.clone(),
        rpc.clone(),
        chain_id,
    );

    let mut runner = BacktestRunner::new(replayer, pool_manager, gas_config);

    let pool_manager = std::mem::take(&mut runner.pool_manager);

    let initial_balance_wei = alloy::primitives::U256::from((config.live.initial_balance * 1_000_000_000_000_000_000.0) as u128);
    let min_profit_wei = alloy::primitives::U256::from((config.live.min_profit_threshold * 1_000_000_000_000_000_000.0) as u128);

    let oracle_mode: PriceOracleMode = match config.backtest.price_oracle_mode.parse() {
        Ok(m) => m,
        Err(_) => {
            tracing::warn!(
                "Invalid price_oracle_mode '{}', falling back to coingecko",
                config.backtest.price_oracle_mode,
            );
            PriceOracleMode::CoinGeckoOnly
        }
    };
    let token_prices: std::collections::HashMap<alloy::primitives::Address, f64> = config.parse_token_prices();

    let chain_defaults = config.chains.get(&chain_name.to_string()).cloned().unwrap_or_default();

    let live_config = LiveConfig {
        initial_balance_wei,
        min_profit_threshold_wei: min_profit_wei,
        poll_interval_ms: config.live.poll_interval_ms,
        max_executions: config.live.max_executions,
        strategies: strategies.clone(),
        gas_config,
        resync_interval: args.resync_interval,
        export_path: config.output.export_path.clone(),
        replay_file: args.replay_file.clone(),
        chain_display_name: chain_name.to_string(),
        price_oracle_mode: oracle_mode,
        token_prices,
        chain_defaults,
        rpc_url: config.rpc.rpc_url.clone().unwrap_or_default(),
    };

    let mut live_runner = LiveRunner::new(
        live_config,
        rpc,
        cache,
        pool_manager,
        runner,
        chain_id,
    ).await;

    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);

    let cancel_on_signal = cancel_tx.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        tracing::info!("Ctrl+C received, shutting down live mode...");
        let _ = cancel_on_signal.send(true);
    });

    live_runner.run(cancel_rx).await?;

    Ok(())
}

/// On-chain pool discovery for a chain whose discovery cache is empty.
///
/// Scans factory events from `pool_discovery_start_block` (chain config) or a
/// bounded recent window up to `to_block`, then caches the pools so the normal
/// `init_pools` path can load them. Best-effort: failures are non-fatal.
async fn run_auto_discovery(
    config: &Config,
    chain_name: ChainName,
    chain_id: u64,
    rpc: &RpcClient,
    cache: &SqliteStore,
    to_block: u64,
) -> anyhow::Result<usize> {
    let chain_config = config.chains.get(&chain_name.to_string()).cloned().unwrap_or_default();

    let parse_list = |v: &Option<Vec<String>>, fallback: Vec<&'static str>| -> Vec<alloy::primitives::Address> {
        match v {
            Some(list) => list.iter().filter_map(|s| s.parse().ok()).collect(),
            None => fallback.into_iter().filter_map(|s| s.parse().ok()).collect(),
        }
    };

    let v2_factories = parse_list(&chain_config.uniswap_v2_factories, chain_name.default_uniswap_v2_factories().to_vec());
    let v3_factories = parse_list(&chain_config.uniswap_v3_factories, chain_name.default_uniswap_v3_factories().to_vec());
    let solidly_factories = parse_list(&chain_config.solidly_factories, chain_name.default_solidly_factories());
    let camelot_factories = parse_list(&chain_config.camelot_factories, chain_name.default_camelot_factories());

    let vault = chain_config.balancer_vault.as_ref().and_then(|s| s.parse().ok());
    let registry = chain_config.curve_registry.as_ref().and_then(|s| s.parse().ok());
    let v4_pool_manager = chain_config.v4_pool_manager.as_ref().and_then(|s| s.parse().ok());
    let trader_joe_factory = chain_config.trader_joe_factory.as_ref().and_then(|s| s.parse().ok());
    let pendle_factory = chain_config.pendle_factory.as_ref().and_then(|s| s.parse().ok());

    let from = chain_config
        .pool_discovery_start_block
        .unwrap_or(to_block.saturating_sub(2_000_000));

    let token_cache = TokenCache::warm(chain_id);
    let disc_config = DiscoveryConfig {
        batch_size: chain_config.pool_discovery_batch_size.unwrap_or(2000),
        v2_fee_override: chain_config.uniswap_v2_default_fee,
        balancer_vault: vault,
        v2_factories: if v2_factories.is_empty() { None } else { Some(v2_factories.as_slice()) },
        v3_factories: if v3_factories.is_empty() { None } else { Some(v3_factories.as_slice()) },
        curve_registry: registry,
        solidly_factories: if solidly_factories.is_empty() { None } else { Some(solidly_factories.as_slice()) },
        camelot_factories: if camelot_factories.is_empty() { None } else { Some(camelot_factories.as_slice()) },
        solidly_fee_bps: Some(30),
        v4_pool_manager,
        trader_joe_factory,
        pendle_factory,
        rpc_concurrency: 24,
        token_cache: Some(&token_cache),
        pool_cache: Some(cache),
    };

    tracing::info!(
        "Discovery cache empty for {} — running on-chain pool discovery {} → {}",
        chain_name,
        from,
        to_block,
    );
    let (pools, _active_blocks) = discover_and_cache(rpc, cache, from, to_block, &disc_config, None).await?;
    Ok(pools.len())
}
