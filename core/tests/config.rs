#![allow(clippy::unwrap_used, clippy::expect_used)]
use alloy::primitives::address;
use mev_scout_core::config::{
    BacktestOverrides, CliOverrides, Config, ConfigBuilder, GasOverrides, OutputConfig,
};
use mev_scout_core::dex_type::DexType;
use mev_scout_core::pool::discovery::DiscoveredPool;
use mev_scout_core::pool::state::PoolInfo;
use mev_scout_core::types::{
    ChainName, GasModel, MevOpportunity, OutputFormat, ResultsFile, Strategy,
};

/// ── ResultsFile JSON roundtrip ──────────────────────────────────────────────
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

/// ── Config TOML output is valid ─────────────────────────────────────────────
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
    assert_eq!(parsed.backtest.strategies, config.backtest.strategies);
    assert_eq!(parsed.gas.gas_limit, config.gas.gas_limit);
    assert_eq!(config.explorer.mev_tolerance_pct, 20.0);
    assert_eq!(config.explorer.mev_error_usd_tol, 0.50);
}

/// ── CLI override merging ────────────────────────────────────────────────────
#[test]
fn test_cli_override_merging() {
    let mut config = Config::default();

    let overrides = CliOverrides {
        chain: Some("avalanche".into()),
        backtest: BacktestOverrides {
            strategies: Some("two_hop_arb,jit".into()),
            ..BacktestOverrides::default()
        },
        gas: GasOverrides {
            gas_model: Some("p90".into()),
            ..GasOverrides::default()
        },
        ..CliOverrides::default()
    };
    config.merge_cli(&overrides).unwrap();

    assert_eq!(config.chain, ChainName::Avalanche);
    assert_eq!(
        config.backtest.strategies,
        vec![Strategy::TwoHopArb, Strategy::Jit]
    );
    assert_eq!(config.gas.gas_model, GasModel::Distribution(90));

    // Unset fields keep defaults
    assert_eq!(config.gas.gas_limit, 200_000);
    assert_eq!(config.gas.priority_fee_gwei, 0.0);
    assert_eq!(config.output.output, OutputFormat::Table);
}

/// ── DiscoveredPool → PoolInfo conversion ────────────────────────────────────
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
        is_stable: None,
        balancer_pool_type: None,
        hook_address: None,
        bin_step: None,
        maturity_timestamp: None,
        underlying_tokens: None,
        dex_name: None,
        token0_symbol: None,
        token1_symbol: None,
        tvl_usd: None,
        volume_usd_24h: None,
        volume_usd_30d: None,
    };

    let info: PoolInfo = dp.into();
    assert_eq!(
        info.address,
        address!("cafe000000000000000000000000000000000001")
    );
    assert_eq!(
        info.token0,
        address!("aaaa0000000000000000000000000000000000aa")
    );
    assert_eq!(
        info.token1,
        address!("bbbb0000000000000000000000000000000000bb")
    );
    assert_eq!(info.fee, 500);
    assert_eq!(info.dex_type, DexType::UniswapV3);
    assert_eq!(info.tick_spacing, Some(10));
    assert_eq!(info.creation_block, 42);
    assert!(info.pool_id.is_none());
    assert_eq!(
        info.factory,
        Some(address!("cafe0000000000000000000000000000000000aa"))
    );
}

/// ── ConfigBuilder produces correct config ───────────────────────────────────
#[test]
fn test_config_builder() {
    let config = ConfigBuilder::default()
        .with_chain(ChainName::Ethereum)
        .with_output(OutputConfig {
            output: OutputFormat::Json,
            ..OutputConfig::default()
        })
        .build();

    assert_eq!(config.chain, ChainName::Ethereum);
    assert_eq!(config.output.output, OutputFormat::Json);

    // Unset fields keep defaults — the no-override path is covered here, so a
    // separate `ConfigBuilder::default().build() == Config::default()` test
    // would only restate these assertions.
    assert_eq!(config.gas.gas_limit, 200_000);
    assert_eq!(config.backtest.strategies, Strategy::all().to_vec());
    assert!(config.rpc.rpc_url.is_none());
    assert!(config.days.is_none());
    assert!(config.from_block.is_none());
}

/// ── Retired strategy names ───────────────────────────────────────────────────
/// Liquidation capture has no `Strategy` variant any more, but configs written
/// before the prune must keep loading. `from_comma_list` drops the retired name
/// instead of failing TOML parsing.
#[test]
fn retired_strategy_name_is_dropped_not_fatal() {
    let parsed = Strategy::from_comma_list("two_hop_arb,liquidation").unwrap();
    assert_eq!(parsed, vec![Strategy::TwoHopArb]);
}

#[test]
fn retired_strategy_name_alongside_live_ones_is_dropped() {
    assert_eq!(
        Strategy::from_comma_list("liquidation, jit").unwrap(),
        vec![Strategy::Jit]
    );
}

/// An empty list is load-bearing: `job_live` / `job_run` gate `init_pools` on
/// `!strategies.is_empty()`, so a config naming only retired strategies must
/// fail loudly rather than resolve to an empty selection that skips pool sync
/// and silently reports zero opportunities.
#[test]
fn only_retired_strategy_names_is_an_error() {
    let err = Strategy::from_comma_list("liquidation").unwrap_err();
    assert!(err.contains("retired"), "error was: {err}");
}

/// `"all"` must keep expanding to the runnable set and stay unaffected by the
/// tombstone list.
#[test]
fn all_still_expands_to_runnable_strategies() {
    assert_eq!(Strategy::from_comma_list("all").unwrap(), Strategy::all());
    assert_eq!(Strategy::from_comma_list("ALL").unwrap(), Strategy::all());
}

/// A genuinely unknown name is still a hard error — the tombstone list must not
/// degrade into a blanket "ignore what it cannot parse".
#[test]
fn unknown_strategy_name_is_still_an_error() {
    assert!(Strategy::from_comma_list("sandwich").is_err());
}

// ── Explorer tip-sink addresses (builder-tip PnL guard) ─────────────────────

/// Default config carries no tip sinks so existing TOML files keep parsing and
/// no new rejection behavior is introduced by default.
#[test]
fn explorer_tip_sink_addresses_default_empty() {
    let config = Config::default();
    assert!(config.explorer.tip_sink_addresses.is_empty());
}

/// `tip_sink_addresses` in the `[explorer]` section parses as a `Vec<Address>`
/// and lands in the explorer sub-config.
#[test]
fn explorer_tip_sink_addresses_parse_from_toml() {
    let sink = address!("1111000000000000000000000000000000000011");
    let config: Config = toml::from_str(&format!(
        "chain = \"polygon\"\n[explorer]\ntip_sink_addresses = [\"{sink}\"]\n"
    ))
    .expect("valid tip sink addresses must parse");
    assert_eq!(config.explorer.tip_sink_addresses, vec![sink]);
}

/// A list with multiple configured tip sinks parses in order.
#[test]
fn explorer_tip_sink_multiple_addresses_parse() {
    let sink1 = address!("2222000000000000000000000000000000000022");
    let sink2 = address!("3333000000000000000000000000000000000033");
    let config: Config = toml::from_str(&format!(
        "chain = \"polygon\"\n[explorer]\ntip_sink_addresses = [\"{sink1}\", \"{sink2}\"]\n"
    ))
    .unwrap();
    assert_eq!(config.explorer.tip_sink_addresses, vec![sink1, sink2]);
}

/// A malformed tip-sink address is a hard TOML parse error, matching the
/// strictness of the per-chain address fields (`savax`, factories, …).
#[test]
fn explorer_tip_sink_invalid_address_is_rejected() {
    let err = toml::from_str::<Config>(
        "chain = \"polygon\"\n[explorer]\ntip_sink_addresses = [\"notanaddress\"]\n",
    );
    let msg = match err {
        Err(e) => e.to_string(),
        Ok(_) => panic!("invalid tip sink address must fail to parse"),
    };
    assert!(
        msg.contains("address"),
        "parse error should mention the field, got: {msg}"
    );
}

/// Roundtrip through the config serializer: tip sinks survive `to_toml_string`
/// (empty by default) and a set list.
#[test]
fn explorer_tip_sink_roundtrip_preserves_set_list() {
    let sink = address!("4444000000000000000000000000000000000044");
    let mut config = Config::default();
    config.explorer.tip_sink_addresses = vec![sink];
    let toml_str = config.to_toml_string().unwrap();
    let parsed: Config = toml::from_str(&toml_str).expect("serialized config must parse");
    assert_eq!(parsed.explorer.tip_sink_addresses, vec![sink]);
}
