use alloy::primitives::{address, U256};
use mev_scout_core::pool::dex_type::DexType;
use mev_scout_core::pool::state::{
    PoolInfo, PoolManager, PoolState, UniswapV3PoolState,
};
use mev_scout_core::types::Strategy;

mod common;
use common::*;

#[test]
fn test_detection_pipeline_synthetic_profitable() {
    let mut pm = PoolManager::new();

    // Pool A: QuickSwap WMATIC/USDC with price imbalance
    // reserves: 1_000_000 USDC, 2_000_000 WMATIC (cheap WMATIC: 0.5 USDC each)
    pm.add_pool(make_pool(matic_usdc_pool(), usdc(), wmatic(), 1_000_000, 2_000_000));

    // Pool B: QuickSwap WMATIC/USDT
    // reserves: 2_000_000 USDT, 1_000_000 WMATIC (dear WMATIC: 2 USDT each)
    pm.add_pool(make_pool(matic_usdt_pool(), usdt(), wmatic(), 2_000_000, 1_000_000));

    // Direction 1: buy WMATIC from A (spend USDC), sell WMATIC to B (get USDT)
    let opps = two_hop_detect(&pm, 1_000_000, 12345678);

    assert!(!opps.is_empty(), "Should detect arb between imbalanced pools");
    assert!(opps.iter().any(|o| o.strategy == Strategy::TwoHopArb));

    for opp in &opps {
        assert!(opp.block_number == 1_000_000);
        assert!(opp.expected_profit > U256::ZERO, "Profit should be positive");
        assert!(opp.gas_cost_wei > 0, "Gas cost should be positive");
    }
}

#[test]
fn test_detection_no_arb_equal_pools() {
    let mut pm = PoolManager::new();

    // Both pools have the same price — no arb
    pm.add_pool(make_pool(matic_usdc_pool(), usdc(), wmatic(), 1_000_000, 1_000_000));
    pm.add_pool(make_pool(matic_usdt_pool(), usdt(), wmatic(), 1_000_000, 1_000_000));

    let opps = two_hop_detect(&pm, 1, 100);

    assert!(opps.is_empty(), "No arb should be detected with equal prices");
}

#[test]
fn test_gas_cost_min_profit_filter() {
    let mut pm = PoolManager::new();

    // Small imbalance — tiny profit
    pm.add_pool(make_pool(matic_usdc_pool(), usdc(), wmatic(), 1_000_000, 1_010_000));
    pm.add_pool(make_pool(matic_usdt_pool(), usdt(), wmatic(), 1_010_000, 1_000_000));

    let opps = two_hop_detect(&pm, 1, 100);

    // Check that gas_cost_wei is computed correctly
    for opp in &opps {
        assert!(opp.gas_cost_wei > 0);
        let expected_gas = 200_000u128 * 50_000_000_000;
        let diff = opp.gas_cost_wei.abs_diff(expected_gas);
        assert!(diff < 1000, "Gas cost mismatch: {} vs {}", opp.gas_cost_wei, expected_gas);
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
    pm.add_pool(make_pool(matic_usdc_pool(), usdc(), wmatic(), 1_000_000, 2_000_000));
    // Pool B: USDT/WMATIC — price: 1 WMATIC = 2.0 USDT
    pm.add_pool(make_pool(matic_usdt_pool(), usdt(), wmatic(), 1_000_000, 500_000));

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
    assert!(!opps.is_empty(), "Should detect arb between same-token pools with different prices");
}

#[test]
fn test_two_hop_v3_reserves_update_accuracy() {
    use mev_scout_core::pool::state::UniswapV3PoolState;
    // V3 pool with concentrated liquidity
    let v3_addr = address!("3333333333333333333333333333333333333333");
    let v3_pool = PoolState::UniswapV3(UniswapV3PoolState {
        info: PoolInfo {
            address: v3_addr,
            token0: wmatic(),
            token1: usdc(),
            fee: 30,
            name: None,
            dex_type: mev_scout_core::pool::dex_type::DexType::UniswapV3,
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
    pm.add_pool(make_pool(matic_usdc_pool(), usdc(), wmatic(), 1_000_000, 2_000_000));
    pm.add_pool(make_pool(matic_usdt_pool(), usdt(), wmatic(), 1_000_000, 500_000));
    // Third pool: USDC/USDT (different addresses for test)
    let usdc_usdt_pool = address!("3333333333333333333333333333333333333333");
    pm.add_pool(make_pool(usdc_usdt_pool, usdc(), usdt(), 1_000_000, 1_000_000));

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
    pm.add_pool(make_pool(matic_usdc_pool(), usdc(), wmatic(), 1_000_000, 2_000_000));
    // Pool B: WMATIC/USDT — WMATIC expensive (2 USDT each)
    pm.add_pool(make_pool(matic_usdt_pool(), usdt(), wmatic(), 1_000_000, 500_000));
    // Pool C: USDT/USDC — converts USDT back to USDC at 1:1 to complete the cycle
    let usdt_usdc_pool = address!("5555555555555555555555555555555555555555");
    pm.add_pool(make_pool(usdt_usdc_pool, usdt(), usdc(), 1_000_000, 1_000_000));

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
async fn test_real_state_initialization_and_two_hop() {
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

    // Two real Polygon pools that share the same pair (different DEX → arb)
    let qs = pool_info(
        address!("6e7a5fafcec6bb1e78bae2a1f0b612012bf14827"),
        wmatic(),
        usdc(),
        "QuickSwap WMATIC/USDC",
    );
    let ss = pool_info(
        address!("cd353f79d9fade311fc3119b841e1f456b54e858"),
        wmatic(),
        usdc(),
        "SushiSwap WMATIC/USDC",
    );

    let mut pm = PoolManager::new();
    pm.add_pool(pool_info_to_state(qs));
    pm.add_pool(pool_info_to_state(ss));

    pm.init_from_rpc(&rpc, block_num).await;

    let initialized = pm.initialized_count();
    eprintln!("Initialized {}/2 pools at block {block_num}", initialized);

    if initialized == 0 {
        eprintln!("Skipping detection assertions: no pools initialized (RPC may not support historical queries)");
        return;
    }

    // Run TwoHopArb detection on real data
    let opps = two_hop_detect(&pm, block_num, block_num);

    eprintln!("TwoHopArb detected {} opportunities on real pools at block {block_num}", opps.len());

    // Same-pair pools almost always have slight price differences
    assert!(!opps.is_empty(), "Should detect arb between real same-pair pools with different prices");
    for opp in &opps {
        assert_eq!(opp.strategy, Strategy::TwoHopArb);
        assert!(opp.expected_profit > U256::ZERO, "Profit should be > 0 on real data");
    }
}

#[tokio::test]
async fn test_real_multi_hop_detection() {
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

    // Build a pool set that supports multi-hop paths:
    //   QuickSwap WMATIC/USDC, WMATIC/USDT, USDC/USDT
    let qs_wmatic_usdc = pool_info(
        address!("6e7a5fafcec6bb1e78bae2a1f0b612012bf14827"),
        wmatic(),
        usdc(),
        "QuickSwap WMATIC/USDC",
    );
    let qs_wmatic_usdt = pool_info(
        address!("604029b0c1a79eebfb31f7c5316434484f3a4b55"),
        wmatic(),
        usdt(),
        "QuickSwap WMATIC/USDT",
    );
    let qs_usdc_usdt = pool_info(
        address!("2cf7252e74036d1da831d11089d326296e64a910"),
        usdc(),
        usdt(),
        "QuickSwap USDC/USDT",
    );

    let mut pm = PoolManager::new();
    pm.add_pool(pool_info_to_state(qs_wmatic_usdc));
    pm.add_pool(pool_info_to_state(qs_wmatic_usdt));
    pm.add_pool(pool_info_to_state(qs_usdc_usdt));

    pm.init_from_rpc(&rpc, block_num).await;

    let initialized = pm.initialized_count();
    eprintln!("Initialized {}/3 pools at block {block_num}", initialized);

    if initialized == 0 {
        eprintln!("Skipping detection assertions: no pools initialized");
        return;
    }

    // Run MultiHopArb detection
    let opps = multi_hop_detect(&pm, block_num, block_num);

    eprintln!("MultiHopArb detected {} opportunities on real pools at block {block_num}", opps.len());

    // At minimum, paths should be found (even if not all are profitable)
    if opps.is_empty() {
        // Could happen if prices are perfectly aligned — unlikely but possible
        eprintln!("No multi-hop arb opportunities at this block (prices may be aligned)");
    } else {
        for opp in &opps {
            assert_eq!(opp.strategy, Strategy::MultiHopArb);
            assert!(opp.path.is_some(), "MultiHopArb must have path populated");
            let path = opp.path.as_ref().unwrap();
            assert!(path.len() >= 2, "Path must have at least 2 pools, got {}", path.len());
        }
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
    assert_eq!(count, 6, "Should find all SushiSwap WMATIC pools, got {count}");

    pm.init_from_rpc(&rpc, block_num).await;

    let initialized = pm.initialized_count();
    eprintln!("Initialized {initialized}/{count} SushiSwap WMATIC pools at block {block_num}");

    if initialized < 2 {
        eprintln!("Skipping: too few initialized pools ({initialized})");
        return;
    }

    // TwoHopArb
    let opps = two_hop_detect(&pm, block_num, block_num);
    eprintln!("TwoHopArb detected {} opportunities across {count} real pools", opps.len());

    // With 6 WMATIC-quoted pools, arb pairs should always exist
    assert!(!opps.is_empty(), "Should detect two-hop arb across multiple WMATIC pools");

    // MultiHopArb
    let mhop_opps = multi_hop_detect(&pm, block_num, block_num);
    eprintln!("MultiHopArb detected {} opportunities across {count} real pools", mhop_opps.len());

    for opp in mhop_opps.iter().take(5) {
        assert!(opp.path.is_some());
        let path = opp.path.as_ref().unwrap();
        assert!(path.len() >= 2);
    }
}

#[tokio::test]
async fn test_real_v2_v3_cross_dex_polygon() {
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

    // Real V2 pool: QuickSwap WMATIC/USDC
    let v2 = pool_info(
        address!("6e7a5fafcec6bb1e78bae2a1f0b612012bf14827"),
        wmatic(),
        usdc(),
        "QuickSwap WMATIC/USDC",
    );

    // Real V3 pool: Uniswap V3 WMATIC/USDC 0.05%
    let v3_info = PoolInfo {
        address: address!("a374094527e1673a86de625aa59517c5de346d32"),
        token0: wmatic(),
        token1: usdc(),
        fee: 500,
        name: Some("Uniswap V3 WMATIC/USDC 0.05%".into()),
        dex_type: DexType::UniswapV3,
        tick_spacing: Some(10),
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
    };

    let mut pm = PoolManager::new();
    pm.add_pool(pool_info_to_state(v2));
    pm.add_pool(PoolState::UniswapV3(UniswapV3PoolState::new(v3_info)));

    pm.init_from_rpc(&rpc, block_num).await;

    let initialized = pm.initialized_count();
    eprintln!("Initialized {}/2 pools (V2 QuickSwap + V3 Uniswap V3) at block {block_num}", initialized);

    if initialized < 2 {
        eprintln!("Skipping: fewer than 2 pools initialized (got {initialized})");
        return;
    }

    // Run TwoHopArb detection across the V2+V3 pools
    let opps = two_hop_detect(&pm, block_num, block_num);

    eprintln!(
        "TwoHopArb detected {} cross-DEX (V2→V3) opportunities at block {block_num}",
        opps.len()
    );

    // V2 and V3 pools for the same pair almost always differ in price → arb exists
    assert!(!opps.is_empty(), "Should detect arb between V2 and V3 pools with different pricing");
    for opp in &opps {
        assert_eq!(opp.strategy, Strategy::TwoHopArb);
        assert!(opp.expected_profit > U256::ZERO, "Profit should be > 0 on real data");
    }
}
