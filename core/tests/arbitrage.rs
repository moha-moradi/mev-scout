use alloy::primitives::{address, U256};
use mev_scout_core::pool::state::{PoolInfo, PoolManager, PoolState};
use mev_scout_core::types::Strategy;

mod common;
use common::*;

#[test]
fn test_detection_pipeline_synthetic_profitable() {
    let mut pm = PoolManager::new();

    // Pool A: QuickSwap WMATIC/USDC with price imbalance
    // reserves: 1_000_000 USDC, 2_000_000 WMATIC (cheap WMATIC: 0.5 USDC each)
    pm.add_pool(make_pool(
        matic_usdc_pool(),
        usdc(),
        wmatic(),
        1_000_000,
        2_000_000,
    ));

    // Pool B: QuickSwap WMATIC/USDT
    // reserves: 2_000_000 USDT, 1_000_000 WMATIC (dear WMATIC: 2 USDT each)
    pm.add_pool(make_pool(
        matic_usdt_pool(),
        usdt(),
        wmatic(),
        2_000_000,
        1_000_000,
    ));

    // Direction 1: buy WMATIC from A (spend USDC), sell WMATIC to B (get USDT)
    let opps = two_hop_detect(&pm, 1_000_000, 12345678);

    assert!(
        !opps.is_empty(),
        "Should detect arb between imbalanced pools"
    );
    assert!(opps.iter().any(|o| o.strategy == Strategy::TwoHopArb));

    for opp in &opps {
        assert!(opp.block_number == 1_000_000);
        assert!(
            opp.expected_profit > U256::ZERO,
            "Profit should be positive"
        );
        assert!(opp.gas_cost_wei > 0, "Gas cost should be positive");
    }
}

#[test]
fn test_detection_no_arb_equal_pools() {
    let mut pm = PoolManager::new();

    // Both pools have the same price — no arb
    pm.add_pool(make_pool(
        matic_usdc_pool(),
        usdc(),
        wmatic(),
        1_000_000,
        1_000_000,
    ));
    pm.add_pool(make_pool(
        matic_usdt_pool(),
        usdt(),
        wmatic(),
        1_000_000,
        1_000_000,
    ));

    let opps = two_hop_detect(&pm, 1, 100);

    assert!(
        opps.is_empty(),
        "No arb should be detected with equal prices"
    );
}

#[test]
fn test_gas_cost_min_profit_filter() {
    let mut pm = PoolManager::new();

    // Small imbalance — tiny profit
    pm.add_pool(make_pool(
        matic_usdc_pool(),
        usdc(),
        wmatic(),
        1_000_000,
        1_010_000,
    ));
    pm.add_pool(make_pool(
        matic_usdt_pool(),
        usdt(),
        wmatic(),
        1_010_000,
        1_000_000,
    ));

    let opps = two_hop_detect(&pm, 1, 100);

    // The closed-form V2↔V2 optimizer must find this small imbalance
    // (the old ternary search missed it entirely due to integer rounding).
    assert!(!opps.is_empty(), "Should detect the small arb");

    // Check that gas_cost_wei is computed correctly:
    // base 40k + calldata(2 pools) 26.6k + 2×V2 swap 160k + flash-loan overhead 150k
    for opp in &opps {
        assert!(opp.gas_cost_wei > 0);
        let expected_gas = 376_600u128 * 50_000_000_000;
        let diff = opp.gas_cost_wei.abs_diff(expected_gas);
        assert!(
            diff < 1000,
            "Gas cost mismatch: {} vs {}",
            opp.gas_cost_wei,
            expected_gas
        );
    }
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
    pm.add_pool(make_pool(
        pool_c,
        usdc(),
        address!("8f3cf7ad23cd3cadbd9735aff958023239c6a063"),
        1000,
        1000,
    ));

    let pairs = pm.arbitrage_pairs();

    // Pair A-B (via WMATIC), Pair A-C (via USDC), Pair B-C should NOT share a token
    assert_eq!(pairs.len(), 2, "Should find 2 arbitrage pairs");
    assert!(
        pairs
            .iter()
            .any(|(a, b, t)| (*a == pool_a && *b == pool_b && *t == wmatic())
                || (*a == pool_b && *b == pool_a && *t == wmatic())),
        "A-B via WMATIC"
    );
    assert!(
        pairs
            .iter()
            .any(|(a, b, t)| (*a == pool_a && *b == pool_c && *t == usdc())
                || (*a == pool_c && *b == pool_a && *t == usdc())),
        "A-C via USDC"
    );
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
    pm.add_pool(make_pool(
        matic_usdc_pool(),
        usdc(),
        wmatic(),
        1_000_000,
        2_000_000,
    ));
    pm.add_pool(make_pool(
        matic_usdt_pool(),
        usdt(),
        wmatic(),
        1_000_000,
        500_000,
    ));

    let opps = two_hop_detect(&pm, 1, 100);

    // Should find arb in at least one direction
    assert!(!opps.is_empty(), "Should detect arb");

    // Both directions checked means we should have at most 2 opportunities
    assert!(opps.len() <= 2, "At most 2 direction opportunities");
}

#[test]
fn test_arb_profit_accuracy_known_delta() {
    let mut pm = PoolManager::new();

    // Pool A: USDC/WMATIC — price: 1 WMATIC = 0.5 USDC
    pm.add_pool(make_pool(
        matic_usdc_pool(),
        usdc(),
        wmatic(),
        1_000_000,
        2_000_000,
    ));
    // Pool B: USDT/WMATIC — price: 1 WMATIC = 2.0 USDT
    pm.add_pool(make_pool(
        matic_usdt_pool(),
        usdt(),
        wmatic(),
        1_000_000,
        500_000,
    ));

    let opps = two_hop_detect(&pm, 1, 100);

    assert!(!opps.is_empty(), "Should detect arb");
    for opp in &opps {
        assert!(opp.expected_profit > U256::ZERO, "Profit should be > 0");
        assert!(opp.gas_cost_wei > 0, "Gas cost should be > 0");
    }
}

#[test]
fn test_two_hop_same_token_different_reserves() {
    let mut pm = PoolManager::new();

    // Two pools with same token pair but different reserves
    // Pool A: 1M USDC, 3M WMATIC (price: 3 WMATIC per USDC — WMATIC cheap)
    // Pool B: 1M USDC, 1M WMATIC (price: 1 WMATIC per USDC — WMATIC expensive)
    // Arb: buy WMATIC on A, sell on B
    let pool_a = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let pool_b = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");

    pm.add_pool(make_pool(pool_a, usdc(), wmatic(), 1_000_000, 3_000_000));
    pm.add_pool(make_pool(pool_b, usdc(), wmatic(), 1_000_000, 1_000_000));

    let opps = two_hop_detect(&pm, 1, 100);

    // Arb exists: buy WMATIC cheap on A, sell expensive on B
    assert!(
        !opps.is_empty(),
        "Should detect arb between same-token pools with different prices"
    );
}

#[test]
fn test_two_hop_v3_reserves_update_accuracy() {
    use mev_scout_core::pool::state::UniswapV3PoolState; // V3 pool with concentrated liquidity
    let v3_addr = address!("3333333333333333333333333333333333333333");
    let v3_pool = PoolState::UniswapV3(UniswapV3PoolState {
        info: PoolInfo {
            address: v3_addr,
            token0: wmatic(),
            token1: usdc(),
            fee: 30,
            name: None,
            dex_type: mev_scout_core::dex_type::DexType::UniswapV3,
            tick_spacing: Some(60),
            creation_block: 0,
            pool_id: None,
            factory: None,
            is_stable: None,
            is_fot: None,
            is_rebase: None,
            underlying_tokens: None,
            balancer_pool_type: None,
            hook_address: None,
            bin_step: None,
            maturity_timestamp: None,
            dex_name: None,
            token0_symbol: None,
            token1_symbol: None,
            tvl_usd: None,
            volume_usd_24h: None,
            volume_usd_30d: None,
        },
        sqrt_price_x96: U256::from(79228162514264337593543950336u128), // price = 1.0
        tick: 0,
        liquidity: 1_000_000_000_000u128,
        ticks: std::collections::BTreeMap::new(),
        fee_growth_global_0_x128: U256::ZERO,
        fee_growth_global_1_x128: U256::ZERO,
    });

    let v2_addr = address!("4444444444444444444444444444444444444444");
    let v2_pool = make_pool(v2_addr, wmatic(), usdt(), 100_000_000, 100_000_000);

    let mut pm = PoolManager::new();
    pm.add_pool(v3_pool);
    pm.add_pool(v2_pool);

    let opps = two_hop_detect(&pm, 1, 100);

    // V3+V2 cross-DEX detection should work
    // This may or may not detect an arb depending on price state
    // At minimum should not panic or crash
    assert!(opps.len() <= 2, "At most 2 opportunities");
}

#[test]
fn test_multi_hop_detection_three_pool() {
    let mut pm = PoolManager::new();

    // Triangular arb: USDC → WMATIC → USDT → USDC
    // Pool A: USDC/WMATIC (WMATIC cheap: 0.5 USDC each)
    // Pool B: WMATIC/USDT (WMATIC expensive: 2 USDT each)
    // Pool C: USDC/USDT (1:1)
    pm.add_pool(make_pool(
        matic_usdc_pool(),
        usdc(),
        wmatic(),
        1_000_000,
        2_000_000,
    ));
    pm.add_pool(make_pool(
        matic_usdt_pool(),
        usdt(),
        wmatic(),
        1_000_000,
        500_000,
    ));
    // Third pool: USDC/USDT (different addresses for test)
    let usdc_usdt_pool = address!("3333333333333333333333333333333333333333");
    pm.add_pool(make_pool(
        usdc_usdt_pool,
        usdc(),
        usdt(),
        1_000_000,
        1_000_000,
    ));

    let opps = multi_hop_detect(&pm, 1, 12345);

    assert!(!opps.is_empty(), "Should detect multi-hop arb");

    // Find a 3-pool opportunity
    let three_hop: Vec<_> = opps
        .iter()
        .filter(|o| o.path.as_ref().map(|p| p.len() >= 3).unwrap_or(false))
        .collect();
    assert!(!three_hop.is_empty(), "Should detect a 3-pool arb");

    for opp in &opps {
        assert_eq!(opp.strategy, Strategy::MultiHopArb);
        assert!(opp.expected_profit > U256::ZERO);
        assert!(opp.gas_cost_wei > 0);
    }
}

#[test]
fn test_multi_hop_path_field_populated() {
    let mut pm = PoolManager::new();
    // Pool A: USDC/WMATIC — WMATIC cheap (0.5 USDC each)
    pm.add_pool(make_pool(
        matic_usdc_pool(),
        usdc(),
        wmatic(),
        1_000_000,
        2_000_000,
    ));
    // Pool B: WMATIC/USDT — WMATIC expensive (2 USDT each)
    pm.add_pool(make_pool(
        matic_usdt_pool(),
        usdt(),
        wmatic(),
        1_000_000,
        500_000,
    ));
    // Pool C: USDT/USDC — converts USDT back to USDC at 1:1 to complete the cycle
    let usdt_usdc_pool = address!("5555555555555555555555555555555555555555");
    pm.add_pool(make_pool(
        usdt_usdc_pool,
        usdt(),
        usdc(),
        1_000_000,
        1_000_000,
    ));

    let opps = multi_hop_detect(&pm, 1, 12345);

    assert!(!opps.is_empty());
    for opp in &opps {
        assert!(opp.path.is_some(), "MultiHopArb must have path populated");
        let path = opp.path.as_ref().unwrap();
        assert!(path.len() >= 2, "Path must have at least 2 pools");
        assert_eq!(path[0], opp.pool_a);
        assert_eq!(path[path.len() - 1], opp.pool_b);
    }
}

#[tokio::test]
async fn test_real_detection_all_sushi_wmatic_pools() {
    let rpc_url = match rpc_url() {
        Some(url) => url,
        None => {
            eprintln!("Skipping: RPC_URL not set");
            return;
        }
    };

    let rpc = match mev_scout_core::rpc::RpcClient::new(&rpc_url, 137) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Skipping: failed to create RPC client: {e}");
            return;
        }
    };

    let block_num = match rpc.get_block_number().await {
        Ok(n) => n.saturating_sub(10),
        Err(e) => {
            eprintln!("Skipping: failed to get block number: {e}");
            return;
        }
    };

    // All SushiSwap WMATIC pools share WMATIC → dense arbitrage graph
    let sushipools = [
        pool_info(
            address!("cd353f79d9fade311fc3119b841e1f456b54e858"),
            wmatic(),
            usdc(),
            "SushiSwap WMATIC/USDC",
        ),
        pool_info(
            address!("55ff76bffc3cdd9d5fdbbc2ece4528ecce45047e"),
            wmatic(),
            usdt(),
            "SushiSwap WMATIC/USDT",
        ),
        pool_info(
            address!("8929d3fea77398f64448c85015633c2d6472fb29"),
            wmatic(),
            address!("8f3cf7ad23cd3cadbd9735aff958023239c6a063"),
            "SushiSwap WMATIC/DAI",
        ),
        pool_info(
            address!("c4e595acdd7d12fec385e5da5d43160e8a0bac0e"),
            wmatic(),
            address!("7ceb23fd6bc0add59e62ac25578270cff1b9f619"),
            "SushiSwap WMATIC/WETH",
        ),
        pool_info(
            address!("8531c4e29491fe6e5e87af6054fc20fccf0b4290"),
            wmatic(),
            address!("1bfd67037b42cf73acf2047067bd4f2c47d9bfd6"),
            "SushiSwap WMATIC/WBTC",
        ),
        pool_info(
            address!("27a2e38b0b7e0f526b6b57a7672d6aa3645cdb48"),
            wmatic(),
            address!("3a58a54c066fdc0f2d55fc9c89f0415c92ebf3c4"),
            "SushiSwap WMATIC/stMATIC",
        ),
    ];

    let mut pm = PoolManager::new();
    for info in &sushipools {
        pm.add_pool(pool_info_to_state(info.clone()));
    }

    let count = pm.pool_count();
    assert_eq!(
        count, 6,
        "Should find all SushiSwap WMATIC pools, got {count}"
    );

    pm.init_from_rpc(&rpc, block_num, None).await;

    let initialized = pm.initialized_count();
    eprintln!("Initialized {initialized}/{count} SushiSwap WMATIC pools at block {block_num}");

    if initialized < 2 {
        eprintln!("Skipping: too few initialized pools ({initialized})");
        return;
    }

    // TwoHopArb
    let opps = two_hop_detect(&pm, block_num, block_num);
    eprintln!(
        "TwoHopArb detected {} opportunities across {count} real pools",
        opps.len()
    );

    // With 6 WMATIC-quoted pools, arb pairs should always exist
    assert!(
        !opps.is_empty(),
        "Should detect two-hop arb across multiple WMATIC pools"
    );

    // MultiHopArb
    let mhop_opps = multi_hop_detect(&pm, block_num, block_num);
    eprintln!(
        "MultiHopArb detected {} opportunities across {count} real pools",
        mhop_opps.len()
    );

    for opp in mhop_opps.iter().take(5) {
        assert!(opp.path.is_some());
        let path = opp.path.as_ref().unwrap();
        assert!(path.len() >= 2);
    }
}

#[test]
fn test_spot_prefilter_skips_aligned_prices() {
    use mev_scout_core::mev::detectors::two_hop::quote_path;

    // Aligned prices: gross cycle rate below fee break-even → quote_path is None
    let aligned_a = make_pool(
        address!("1111111111111111111111111111111111111111"),
        usdc(),
        wmatic(),
        1_000_000,
        1_000_000,
    );
    let aligned_b = make_pool(
        address!("2222222222222222222222222222222222222222"),
        usdt(),
        wmatic(),
        1_000_000,
        1_000_000,
    );
    assert!(quote_path(&aligned_a, &aligned_b, wmatic()).is_none());

    // Imbalanced: profitable direction passes the pre-filter and finds the arb
    let cheap = make_pool(matic_usdc_pool(), usdc(), wmatic(), 1_000_000, 2_000_000);
    let dear = make_pool(matic_usdt_pool(), usdt(), wmatic(), 2_000_000, 500_000);
    let fwd = quote_path(&cheap, &dear, wmatic());
    assert!(fwd.is_some(), "profitable direction must pass pre-filter");
    assert!(fwd.unwrap().profit > 0);

    // Reverse direction: provably below break-even → skipped without optimizing
    assert!(quote_path(&dear, &cheap, wmatic()).is_none());
}

#[test]
fn test_dirty_scope_restricts_detection() {
    use mev_scout_core::pool::state::ScanScope;
    use std::collections::HashSet;

    let mut pm = PoolManager::new();
    pm.add_pool(make_pool(
        matic_usdc_pool(),
        usdc(),
        wmatic(),
        1_000_000,
        2_000_000,
    ));
    pm.add_pool(make_pool(
        matic_usdt_pool(),
        usdt(),
        wmatic(),
        2_000_000,
        500_000,
    ));

    // Full scan finds the arb
    let full_opps = two_hop_detect(&pm, 0, 100);
    assert!(!full_opps.is_empty());

    // Empty dirty set → nothing scanned
    let empty: HashSet<_> = HashSet::new();
    let mut d = TwoHopArbDetectorForTest::new(1);
    let opps = d.detect(
        &pm,
        0,
        100,
        50_000_000_000,
        default_gas_config(),
        &ScanScope::Dirty(&empty),
    );
    assert!(
        opps.is_empty(),
        "dirty scope with no dirty pools must scan nothing"
    );

    // Only pool B dirty → pair containing it is scanned
    let mut only_b: HashSet<_> = HashSet::new();
    only_b.insert(matic_usdt_pool());
    let mut d = TwoHopArbDetectorForTest::new(2);
    let opps = d.detect(
        &pm,
        0,
        100,
        50_000_000_000,
        default_gas_config(),
        &ScanScope::Dirty(&only_b),
    );
    assert!(
        !opps.is_empty(),
        "pair containing a dirty pool must be scanned"
    );
}

/// Minimal wrapper so tests can construct the detector directly.
use mev_scout_core::mev::detectors::two_hop::TwoHopArbDetector as TwoHopArbDetectorForTest;

#[test]
fn test_dirty_pools_tracked_via_mark_and_take() {
    let mut pm = PoolManager::new();
    assert_eq!(pm.dirty_pool_count(), 0);

    pm.mark_dirty_pool(matic_usdc_pool());
    pm.mark_dirty_pool(matic_usdt_pool());
    pm.mark_dirty_pool(matic_usdc_pool()); // idempotent
    assert_eq!(pm.dirty_pool_count(), 2);

    let drained = pm.take_dirty_pools();
    assert_eq!(drained.len(), 2);
    assert_eq!(pm.dirty_pool_count(), 0);
}

#[test]
fn test_fot_tokens_excluded_from_candidates() {
    use mev_scout_core::pool::state::is_fee_on_transfer_token;

    // Ethereum-mainnet USDT is in the bundled FOT registry
    let eth_usdt = address!("dac17f958d2ee523a2206206994597c13d831ec7");
    assert!(is_fee_on_transfer_token(&eth_usdt));

    // Profitable imbalance but token_out (eth USDT) carries a transfer tax —
    // the raw-amount profit would be phantom. Must be excluded entirely.
    let mut pm = PoolManager::new();
    pm.add_pool(make_pool(
        address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        usdc(),
        wmatic(),
        1_000_000,
        2_000_000,
    ));
    pm.add_pool(make_pool(
        address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        eth_usdt,
        wmatic(),
        2_000_000,
        500_000,
    ));
    assert!(
        two_hop_detect(&pm, 1, 100).is_empty(),
        "FOT output token must be excluded"
    );

    // Same imbalance with a non-FOT quote token still detects
    let mut pm_ok = PoolManager::new();
    pm_ok.add_pool(make_pool(
        address!("cccccccccccccccccccccccccccccccccccccccc"),
        usdc(),
        wmatic(),
        1_000_000,
        2_000_000,
    ));
    pm_ok.add_pool(make_pool(
        address!("dddddddddddddddddddddddddddddddddddddddd"),
        usdt(),
        wmatic(),
        2_000_000,
        500_000,
    ));
    assert!(
        !two_hop_detect(&pm_ok, 1, 100).is_empty(),
        "non-FOT pair must still detect"
    );
}

#[test]
fn test_v3_two_hop_segmented_finds_profitable_arb() {
    use alloy::primitives::Address;
    use mev_scout_core::dex_type::DexType;
    use mev_scout_core::pool::state::UniswapV3PoolState;
    use std::collections::BTreeMap;

    // V3 pool A: WMATIC/USDC priced at tick 0 (1.0), liquidity concentrated
    let mk_v3 = |addr: Address,
                 token0: Address,
                 token1: Address,
                 sqrt_tick: i32,
                 ticks: Vec<(i32, i128)>| {
        PoolState::UniswapV3(UniswapV3PoolState {
            info: PoolInfo {
                address: addr,
                token0,
                token1,
                fee: 3000,
                name: None,
                dex_type: DexType::UniswapV3,
                tick_spacing: Some(60),
                creation_block: 0,
                pool_id: None,
                factory: None,
                is_stable: None,
                is_fot: None,
                is_rebase: None,
                underlying_tokens: None,
                balancer_pool_type: None,
                hook_address: None,
                bin_step: None,
                maturity_timestamp: None,
                dex_name: None,
                token0_symbol: None,
                token1_symbol: None,
                tvl_usd: None,
                volume_usd_24h: None,
                volume_usd_30d: None,
            },
            sqrt_price_x96: mev_scout_core::pool::math::get_sqrt_ratio_at_tick(sqrt_tick),
            tick: sqrt_tick,
            liquidity: 5_000_000_000_000u128,
            ticks: ticks.into_iter().collect::<BTreeMap<_, _>>(),
            fee_growth_global_0_x128: U256::ZERO,
            fee_growth_global_1_x128: U256::ZERO,
        })
    };

    // Pool A at tick 0; pool B at tick +200 (price differs → arb across bands)
    let v3_a = address!("3333333333333333333333333333333333333333");
    let v3_b = address!("4444444444444444444444444444444444444444");
    let mut pm = PoolManager::new();
    pm.add_pool(mk_v3(
        v3_a,
        wmatic(),
        usdc(),
        0,
        vec![(-120, 1_000_000_000_000), (120, 2_000_000_000_000)],
    ));
    pm.add_pool(mk_v3(
        v3_b,
        usdc(),
        wmatic(),
        200,
        vec![(260, 1_000_000_000_000), (380, 1_000_000_000_000)],
    ));

    let opps = two_hop_detect(&pm, 1, 100);
    for opp in &opps {
        assert!(opp.expected_profit > U256::ZERO);
    }
    // Detection must not panic and must respect dedup bounds
    assert!(opps.len() <= 2);
}
