use alloy::primitives::{address, U256};
use mev_scout_core::config::{CliOverrides, Config};
use mev_scout_core::pool::dex_type::DexType;
use mev_scout_core::pool::discovery::DiscoveredPool;
use mev_scout_core::pool::state::{PoolInfo, PoolManager};
use mev_scout_core::types::{MevOpportunity, ResultsFile, Strategy};

mod common;
use common::*;

/// ── Test 6: ResultsFile JSON roundtrip ──────────────────────────────────────
#[test]
fn test_results_file_roundtrip() {
    let opp = MevOpportunity::new(
        100,
        0,
        Strategy::TwoHopArb,
        address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        12345678,
    );
    let file = ResultsFile {
        run_id: "test_run".into(),
        chain: "polygon".into(),
        start_block: 100,
        end_block: 200,
        range_mode: "range".into(),
        strategies: vec!["two_hop_arb".into()],
        flash_loan_provider: "aave".into(),
        resolved_at: 12345678,
        created_at: 12345679,
        opportunities: vec![opp],
    };

    let json = serde_json::to_string_pretty(&file).unwrap();
    let deser: ResultsFile = serde_json::from_str(&json).unwrap();

    assert_eq!(deser.run_id, "test_run");
    assert_eq!(deser.chain, "polygon");
    assert_eq!(deser.start_block, 100);
    assert_eq!(deser.end_block, 200);
    assert_eq!(deser.range_mode, "range");
    assert_eq!(deser.strategies, vec!["two_hop_arb"]);
    assert_eq!(deser.flash_loan_provider, "aave");
    assert_eq!(deser.resolved_at, 12345678);
    assert_eq!(deser.created_at, 12345679);
    assert_eq!(deser.opportunities.len(), 1);
    assert_eq!(deser.opportunities[0].strategy, Strategy::TwoHopArb);
    assert_eq!(deser.opportunities[0].block_number, 100);
}

/// ── Test 7: Config TOML output is valid ─────────────────────────────────────
#[test]
fn test_config_toml_output() {
    let config = Config::default();
    let toml_str = config.to_toml_string().unwrap();
    assert!(!toml_str.is_empty());
    assert!(toml_str.contains("chain"));
    assert!(toml_str.contains("strategies"));

    // Parse back — must be valid TOML
    let parsed: Config = toml::from_str(&toml_str).unwrap();
    assert_eq!(parsed.chain, config.chain);
    assert_eq!(parsed.strategies, config.strategies);
    assert_eq!(parsed.gas_limit, config.gas_limit);
}

/// ── Test 8: CLI override merging ────────────────────────────────────────────
#[test]
fn test_cli_override_merging() {
    let mut config = Config::default();

    let overrides = CliOverrides {
        chain: Some("avalanche".into()),
        strategies: Some("two_hop_arb,sandwich".into()),
        gas_model: Some("p90".into()),
        proximity_window: Some(5),
        days: None,
        blocks: None,
        block: None,
        from_block: None,
        to_block: None,
        rpc_url: None,
        rpc_urls: None,
        rpc_rps: None,
        rps_limit: None,
        flash_loan_provider: None,
        gas_limit: None,
        priority_fee_gwei: None,
        output: None,
        export_path: None,
        db_path: None,
        parquet_dir: None,
        coingecko_api_key: None,
        price_oracle_mode: None,
        token_prices: None,
        capture_pending: None,
        cross_block_window: None,
        block_concurrency: None,
        initial_balance: None,
        min_profit_threshold: None,
        poll_interval_ms: None,
        max_executions: None,
        dune_api_key: None,
        dune_primary_pool_discovery: None,
    };
    config.merge_cli(&overrides);

    assert_eq!(config.chain, "avalanche");
    assert_eq!(config.strategies, "two_hop_arb,sandwich");
    assert_eq!(config.gas_model, "p90");
    assert_eq!(config.proximity_window, 5);

    // Unset fields keep defaults
    assert_eq!(config.gas_limit, 200_000);
    assert_eq!(config.priority_fee_gwei, 0.0);
    assert_eq!(config.output, "table");
}

/// ── Test 9: Discover V3 pools synthetic (topic verification) ───────────────
#[test]
fn test_discover_v3_pipeline() {
    let dp = DiscoveredPool {
        address: address!("cafe000000000000000000000000000000000001"),
        token0: address!("aaaa0000000000000000000000000000000000aa"),
        token1: address!("bbbb0000000000000000000000000000000000bb"),
        fee: 500,
        tick_spacing: Some(10),
        dex_type: DexType::UniswapV3,
        creation_block: 42,
        pool_id: None,
        factory: Some(address!("cafe0000000000000000000000000000000000aa")),
    };

    let info: PoolInfo = dp.into();
    assert_eq!(info.address, address!("cafe000000000000000000000000000000000001"));
    assert_eq!(info.token0, address!("aaaa0000000000000000000000000000000000aa"));
    assert_eq!(info.token1, address!("bbbb0000000000000000000000000000000000bb"));
    assert_eq!(info.fee, 500);
    assert_eq!(info.dex_type, DexType::UniswapV3);
    assert_eq!(info.tick_spacing, Some(10));
    assert_eq!(info.creation_block, 42);
    assert!(info.pool_id.is_none());
    assert_eq!(info.factory, Some(address!("cafe0000000000000000000000000000000000aa")));
}


