use alloy::primitives::{address, Address, U256};
use mev_backtest_core::mev::pricing;
use mev_backtest_core::mev::two_hop::TwoHopArbDetector;
use mev_backtest_core::pool::state::UniswapV2PoolState;
use mev_backtest_core::pool::state::{PoolInfo, PoolManager, PoolState};
use mev_backtest_core::types::Strategy;

fn wmatic() -> Address {
    address!("0d500b1d8e8ef31e21c99d1db9a6444d3adf1270")
}
fn usdc() -> Address {
    address!("2791bca1f2de4661ed88a30c99a7a9449aa84174")
}
fn usdt() -> Address {
    address!("c2132d05d31c914a87c6611c10748aeb04b58e8f")
}
fn matic_usdc_pool() -> Address {
    address!("6e7a5fafcec6bb1e78bae2a1f0b612012bf14827")
}
fn matic_usdt_pool() -> Address {
    address!("604029b0c1a79eebfb31f7c5316434484f3a4b55")
}

fn make_pool(addr: Address, token0: Address, token1: Address, r0: u128, r1: u128) -> PoolState {
    PoolState::UniswapV2(UniswapV2PoolState {
        info: PoolInfo {
            address: addr,
            pool_type: "uniswap_v2".into(),
            token0,
            token1,
            fee: 30,
            name: None,
            dex_type: mev_backtest_core::pool::dex_type::DexType::UniswapV2,
            tick_spacing: None,
        },
        reserve0: r0,
        reserve1: r1,
    })
}

#[test]
fn test_pool_registry_loads() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let pool_path = manifest.parent().unwrap().join("pools/polygon.json");
    let path_str = pool_path.to_str().unwrap();
    let pools = mev_backtest_core::pool::registry::PoolRegistry::load_optional(Some(path_str));
    assert!(!pools.is_empty(), "Pool registry should load pools from {}", path_str);
    assert!(pools.len() >= 45, "Should have at least 45 pools, got {}. Path: {}", pools.len(), path_str);
}

#[test]
fn test_detection_pipeline_synthetic_profitable() {
    let mut pm = PoolManager::new();

    // Pool A: QuickSwap WMATIC/USDC with price imbalance
    // reserves: 1_000_000 USDC, 2_000_000 WMATIC (cheap WMATIC: 0.5 USDC each)
    pm.add_pool(make_pool(
        matic_usdc_pool(), usdc(), wmatic(),
        1_000_000, 2_000_000,
    ));

    // Pool B: QuickSwap WMATIC/USDT
    // reserves: 2_000_000 USDT, 1_000_000 WMATIC (dear WMATIC: 2 USDT each)
    pm.add_pool(make_pool(
        matic_usdt_pool(), usdt(), wmatic(),
        2_000_000, 1_000_000,
    ));

    let detector = TwoHopArbDetector::new(0.0);

    // Direction 1: buy WMATIC from A (spend USDC), sell WMATIC to B (get USDT)
    let opps = detector.detect(&pm, 1_000_000, 0, 12345678, 50_000_000_000, 1.0);

    assert!(!opps.is_empty(), "Should detect arb between imbalanced pools");
    assert!(opps.iter().any(|o| o.strategy == Strategy::TwoHopArb));

    for opp in &opps {
        assert!(opp.block_number == 1_000_000);
        assert!(opp.expected_profit > U256::ZERO, "Profit should be positive");
        assert!(opp.expected_profit_usd > 0.0, "USD profit should be positive");
        assert!(opp.gas_cost_usd > 0.0, "Gas cost should be positive");
        assert!(opp.net_profit_usd > 0.0, "Net profit should be positive after gas with 0 min_profit");
    }
}

#[test]
fn test_detection_no_arb_equal_pools() {
    let mut pm = PoolManager::new();

    // Both pools have the same price — no arb
    pm.add_pool(make_pool(
        matic_usdc_pool(), usdc(), wmatic(),
        1_000_000, 1_000_000,
    ));
    pm.add_pool(make_pool(
        matic_usdt_pool(), usdt(), wmatic(),
        1_000_000, 1_000_000,
    ));

    let detector = TwoHopArbDetector::new(0.0);
    let opps = detector.detect(&pm, 1, 0, 100, 50_000_000_000, 1.0);

    assert!(opps.is_empty(), "No arb should be detected with equal prices");
}

#[test]
fn test_gas_cost_min_profit_filter() {
    let mut pm = PoolManager::new();

    // Small imbalance — tiny profit
    pm.add_pool(make_pool(
        matic_usdc_pool(), usdc(), wmatic(),
        1_000_000, 1_010_000, // slight imbalance
    ));
    pm.add_pool(make_pool(
        matic_usdt_pool(), usdt(), wmatic(),
        1_010_000, 1_000_000,
    ));

    // Very high min_profit_usd — no arbs should pass
    let detector = TwoHopArbDetector::new(1000.0);
    let opps = detector.detect(&pm, 1, 0, 100, 50_000_000_000, 1.0);
    assert!(opps.is_empty(), "High min_profit should filter all arbs");

    // Low min_profit — should see arbs if profitable
    let detector2 = TwoHopArbDetector::new(0.0);
    let opps2 = detector2.detect(&pm, 1, 0, 100, 50_000_000_000, 1.0);

    // Check that gas_cost_usd is computed correctly
    // 200_000 gas * (50 gwei + 1 gwei) = 200_000 * 51e9 = 1.02e16 wei = 0.0102 MATIC
    // MATIC price = $0.50, so gas_cost_usd = 0.0051
    for opp in &opps2 {
        assert!(opp.gas_cost_usd > 0.0);
        let expected_gas = 200_000.0 * (50.0 + 1.0) * 1e9 / 1e18 * 0.50;
        let diff = (opp.gas_cost_usd - expected_gas).abs();
        assert!(diff < 0.001, "Gas cost mismatch: {} vs {}", opp.gas_cost_usd, expected_gas);
    }
}

#[test]
fn test_pricing_module() {
    assert_eq!(pricing::matic_usd_price(), 0.50);

    // USDC: 6 decimals, $1.00
    let usd = pricing::raw_amount_to_usd(usdc(), 1_000_000).unwrap(); // 1 USDC
    assert!((usd - 1.0).abs() < 0.01);

    // WMATIC: 18 decimals, $0.50
    let usd = pricing::raw_amount_to_usd(wmatic(), 10_000_000_000_000_000_000u128).unwrap(); // 10 WMATIC
    assert!((usd - 5.0).abs() < 0.01);

    // Unknown token returns None
    let unknown = address!("deaddeaddeaddeaddeaddeaddeaddeaddeaddead");
    assert!(pricing::raw_amount_to_usd(unknown, 1000).is_none());

    assert_eq!(pricing::token_decimals(usdc()), Some(6));
    assert_eq!(pricing::token_decimals(wmatic()), Some(18));
}

#[test]
fn test_pool_manager_arbitrage_pairs() {
    let mut pm = PoolManager::new();

    let pool_a = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let pool_b = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    let pool_c = address!("cccccccccccccccccccccccccccccccccccccccc");

    // Pool A: USDC/WMATIC
    pm.add_pool(make_pool(pool_a, usdc(), wmatic(), 1000, 1000));
    // Pool B: USDT/WMATIC — shares WMATIC with pool A
    pm.add_pool(make_pool(pool_b, usdt(), wmatic(), 1000, 1000));
    // Pool C: USDC/DAI — shares USDC with pool A
    pm.add_pool(make_pool(pool_c, usdc(), address!("8f3cf7ad23cd3cadbd9735aff958023239c6a063"), 1000, 1000));

    let pairs = pm.arbitrage_pairs();

    // Pair A-B (via WMATIC), Pair A-C (via USDC), Pair B-C should NOT share a token
    assert_eq!(pairs.len(), 2, "Should find 2 arbitrage pairs");
    assert!(pairs.iter().any(|(a, b, t)| (*a == pool_a && *b == pool_b && *t == wmatic())
        || (*a == pool_b && *b == pool_a && *t == wmatic())), "A-B via WMATIC");
    assert!(pairs.iter().any(|(a, b, t)| (*a == pool_a && *b == pool_c && *t == usdc())
        || (*a == pool_c && *b == pool_a && *t == usdc())), "A-C via USDC");
}

#[test]
fn test_pool_addresses_filter() {
    let mut pm = PoolManager::new();

    let addr_a = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let addr_b = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");

    pm.add_pool(make_pool(addr_a, usdc(), wmatic(), 100, 100));
    pm.add_pool(make_pool(addr_b, usdt(), wmatic(), 100, 100));

    let addrs = pm.pool_addresses();
    assert_eq!(addrs.len(), 2);
    assert!(addrs.contains(&addr_a));
    assert!(addrs.contains(&addr_b));
}

#[test]
fn test_detect_both_directions() {
    let mut pm = PoolManager::new();

    // Pool A and B both trade WMATIC/stable
    // Pool A: 1 USDC = 2 WMATIC (WMATIC cheap)
    // Pool B: 1 USDT = 0.5 WMATIC (WMATIC expensive)
    pm.add_pool(make_pool(matic_usdc_pool(), usdc(), wmatic(), 1_000_000, 2_000_000));
    pm.add_pool(make_pool(matic_usdt_pool(), usdt(), wmatic(), 1_000_000, 500_000));

    let detector = TwoHopArbDetector::new(0.0);
    let opps = detector.detect(&pm, 1, 0, 100, 50_000_000_000, 1.0);

    // Should find arb in at least one direction
    assert!(!opps.is_empty(), "Should detect arb");

    // Both directions checked means we should have at most 2 opportunities
    assert!(opps.len() <= 2, "At most 2 direction opportunities");
}
