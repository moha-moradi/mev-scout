use std::collections::HashMap;

use alloy::primitives::{Address, U256};

use crate::cli::LiveArgs;
use mev_scout_core::cache::SqliteStore;
use mev_scout_core::config::Config;
use mev_scout_core::execution::{
    BroadcastMode, ExecutionConfig, ExecutionSigner, TxBroadcaster,
};
use mev_scout_core::mev::execution::{LiveConfig, LiveRunner};
use mev_scout_core::pipeline::BacktestRunner;
use mev_scout_core::pool::state::PoolManager;
use mev_scout_core::replay::BlockReplayer;
use mev_scout_core::rpc::RpcClient;
use mev_scout_core::types::{ChainName, GasConfig, GasModel, PriceOracleMode, Strategy};

pub async fn cmd_live(config: &Config, args: &LiveArgs) -> anyhow::Result<()> {
    let chain_name: ChainName = match args.chain_args.chain.parse() {
        Ok(c) => c,
        Err(e) => anyhow::bail!("Error: {e}"),
    };

    let provider_configs = config.effective_provider_configs(chain_name)?;
    let chain_id = chain_name.chain_id();
    let rpc_refs: Vec<&str> = provider_configs.iter().map(|(u, _)| u.as_str()).collect();
    let rpc = RpcClient::from_urls(&rpc_refs, chain_id)?;
    rpc.with_provider_rps(&provider_configs.iter().map(|(_, r)| r.unwrap_or(1.0)).collect::<Vec<_>>()).await;
    rpc.check_connection(chain_id).await?;

    let cache = SqliteStore::open(&config.effective_db_path(&chain_name), chain_id)?;

    let strategies = Strategy::from_comma_list(&args.strategies)
        .map_err(|e| anyhow::anyhow!("Error parsing strategies: {e}"))?;

    let gas_model: GasModel = args.gas_model.parse().unwrap_or(GasModel::Live);

    let gas_config = GasConfig {
        gas_limit: args.gas_limit,
        gas_model,
        priority_fee_gwei: args.priority_fee,
        ..GasConfig::default()
    };

    let mut pool_manager = PoolManager::new();
    if let Some(vault_str) = config.chains.get(&chain_name.to_string())
        .and_then(|c| c.balancer_vault.as_ref())
    {
        if let Ok(vault_addr) = vault_str.parse::<Address>() {
            pool_manager = pool_manager.with_balancer_vault(vault_addr);
        }
    }
    if let Some(native_str) = config.chains.get(&chain_name.to_string())
        .and_then(|c| c.wrapped_native_token.as_ref())
    {
        if let Ok(native_addr) = native_str.parse::<Address>() {
            pool_manager = pool_manager.with_wrapped_native(native_addr);
        }
    }

    let latest_block = rpc.get_block_number().await.unwrap_or(0);
    let init_block = latest_block.saturating_sub(1);

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
    if strategies.contains(&Strategy::CrossBlockArb) {
        runner = runner.with_cross_block(3);
    }

    if args.competition {
        runner = runner.with_competition();
    }

    let pool_manager = std::mem::take(&mut runner.pool_manager);

    let initial_balance_wei = U256::from((config.initial_balance * 1_000_000_000_000_000_000.0) as u128);
    let min_profit_wei = U256::from((config.min_profit_threshold * 1_000_000_000_000_000_000.0) as u128);

    let oracle_mode: PriceOracleMode = match config.price_oracle_mode.parse() {
        Ok(m) => m,
        Err(_) => {
            tracing::warn!(
                "Invalid price_oracle_mode '{}', falling back to coingecko",
                config.price_oracle_mode,
            );
            PriceOracleMode::CoinGeckoOnly
        }
    };
    let token_prices: HashMap<Address, f64> = config.parse_token_prices();

    // ── Build execution config & signer ─────────────────────────────
    let wallet_key = config.wallet_key.clone().or_else(|| std::env::var("MEV_SCOUT_PK").ok());

    let execution_config = wallet_key.as_ref().map(|_| ExecutionConfig {
        private_key: wallet_key.clone(),
        broadcast_mode: config.broadcast_mode.parse().unwrap_or(BroadcastMode::Public),
        executor_factory: config.executor_factory.as_ref().and_then(|s| s.parse().ok()),
        flashbots_relay_url: Some("https://relay.flashbots.net".into()),
        mevshare_relay_url: Some("https://mev-share.flashbots.net".into()),
        confirmation_blocks: 1,
        gas_limit_multiplier: config.gas_multiplier,
    });

    let execution_signer = wallet_key.as_ref().and_then(|key| {
        ExecutionSigner::from_private_key(key, chain_id).ok()
    });

    let broadcaster = execution_config.as_ref().map(|cfg| {
        let mode = cfg.broadcast_mode.for_chain(chain_id);
        TxBroadcaster::new(
            mode,
            cfg.flashbots_relay_url.clone(),
            cfg.mevshare_relay_url.clone(),
        )
    });

    let chain_defaults = config.chains.get(&chain_name.to_string()).cloned().unwrap_or_default();

    let live_config = LiveConfig {
        initial_balance_wei,
        min_profit_threshold_wei: min_profit_wei,
        poll_interval_ms: config.poll_interval_ms,
        max_executions: config.max_executions,
        strategies: strategies.clone(),
        gas_config,
        resync_interval: args.resync_interval,
        export_path: config.export_path.clone(),
        replay_file: args.replay_file.clone(),
        chain_display_name: chain_name.to_string(),
        price_oracle_mode: oracle_mode,
        token_prices,
        chain_defaults,
        rpc_url: config.rpc_url.clone().unwrap_or_default(),
    };

    let block_replayer = BlockReplayer::new(
        tokio::runtime::Handle::current(),
        cache.clone(),
        rpc.clone(),
        chain_id,
    );

    let mut live_runner = LiveRunner::new(
        live_config,
        rpc,
        cache,
        pool_manager,
        runner,
        block_replayer,
        chain_id,
        execution_signer,
        execution_config,
        broadcaster,
    ).await;

    if let Some(ref comp_db_path) = args.competition_db {
        match SqliteStore::open(comp_db_path, chain_id) {
            Ok(comp_store) => {
                live_runner.with_competition_db(&comp_store);
                tracing::info!(
                    "Loaded {} known competitor profiles from competition DB",
                    live_runner.competition_state.known_count(),
                );
            }
            Err(e) => tracing::warn!("Failed to open competition DB: {}", e),
        }
    }

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
