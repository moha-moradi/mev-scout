use alloy::primitives::{address, Address, B256, Bytes, U256};
use mev_scout_core::data::ExecutedLog;
use mev_scout_core::mev::detectors::jit_arb::JitArbDetector;
use mev_scout_core::mev::detectors::two_hop::TwoHopArbDetector;
use mev_scout_core::pipeline::scanner::ActivityScanner;
use mev_scout_core::pipeline::BacktestRunner;
use mev_scout_core::pool::decoders::{V3_MINT_TOPIC, V3_SWAP_TOPIC};
use mev_scout_core::pool::dex_type::DexType;
use mev_scout_core::pool::state::{PoolInfo, PoolManager, PoolState, UniswapV2PoolState};
use mev_scout_core::replay::BlockReplayer;
use mev_scout_core::resolver::ResolvedRange;
use mev_scout_core::rpc::RpcClient;
use mev_scout_core::types::{GasConfig, GasModel, RangeMode, Strategy};

mod common;
use common::*;

/// ── Activity Scanner Tests ────────────────────────────────────────────────────

#[tokio::test]
async fn test_activity_scanner_finds_active_blocks() {
    let rpc_url = match rpc_url() {
        Some(url) => url,
        None => {
            eprintln!("Skipping: RPC_URL not set");
            return;
        }
    };

    let rpc = match RpcClient::new(&rpc_url, 137) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Skipping: failed to create RPC client: {e}");
            return;
        }
    };

    let latest = match rpc.get_block_number().await {
        Ok(n) => n,
        Err(e) => {
            eprintln!("Skipping: failed to get block number: {e}");
            return;
        }
    };

    // Use a highly active Polygon pool: QuickSwap WMATIC/USDC
    let pool = address!("6e7a5fafcec6bb1e78bae2a1f0b612012bf14827");

    // Use the actual batch size from scanner (default 2000) to scan a realistic range
    let start = latest.saturating_sub(5000);
    let end = latest;

    let scanner = ActivityScanner::new(rpc).with_batch_size(2000);

    let active = match scanner.find_active_blocks(&[pool], start, end).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Skipping: activity scan failed: {e}");
            return;
        }
    };

    eprintln!(
        "Activity scan [{start}..{end}]: {}/{} blocks active (pool={pool})",
        active.len(),
        end.saturating_sub(start) + 1,
    );

    // QuickSwap WMATIC/USDC is a high-volume pool — should have activity
    assert!(!active.is_empty(), "Should find at least one active block for a high-volume pool");
    assert!(
        active.len() < (end - start + 1) as usize,
        "Not all blocks should be active"
    );
}

#[tokio::test]
async fn test_activity_scanner_no_pools_returns_empty() {
    let rpc_url = match rpc_url() {
        Some(url) => url,
        None => {
            eprintln!("Skipping: RPC_URL not set");
            return;
        }
    };

    let rpc = match RpcClient::new(&rpc_url, 137) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Skipping: failed to create RPC client: {e}");
            return;
        }
    };

    let scanner = ActivityScanner::new(rpc);
    let active = scanner.find_active_blocks(&[], 0, 100).await.unwrap();
    assert!(active.is_empty(), "Empty pool list should return empty set");
}

#[tokio::test]
async fn test_activity_scanner_multi_block_batch() {
    let rpc_url = match rpc_url() {
        Some(url) => url,
        None => {
            eprintln!("Skipping: RPC_URL not set");
            return;
        }
    };

    let rpc = match RpcClient::new(&rpc_url, 137) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Skipping: failed to create RPC client: {e}");
            return;
        }
    };

    let latest = match rpc.get_block_number().await {
        Ok(n) => n,
        Err(e) => {
            eprintln!("Skipping: failed to get block number: {e}");
            return;
        }
    };

    // Test with batch_size=1 (forces multiple batches even for small ranges)
    let pool = address!("6e7a5fafcec6bb1e78bae2a1f0b612012bf14827");
    let start = latest.saturating_sub(3);
    let end = latest;

    let scanner = ActivityScanner::new(rpc).with_batch_size(1);

    let active = match scanner.find_active_blocks(&[pool], start, end).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Skipping: activity scan failed: {e}");
            return;
        }
    };

    eprintln!("Multi-batch scan [{start}..{end}] (batch=1): {} active blocks", active.len());
    assert!(
        active.len() <= (end - start + 1) as usize,
        "Active set should not exceed scanned range"
    );
}

/// ── Test 1: BacktestRunner::run_block() with synthetic data ─────────────────
#[tokio::test]
async fn test_runner_run_block_synthetic() {
    let dir = temp_test_dir("run_block_synth");
    let cache = prep_synthetic_cache(&dir, 1, 2);
    let handle = tokio::runtime::Handle::current();
    let rpc = RpcClient::new("http://0.0.0.0:1", 1).unwrap();
    let replayer = BlockReplayer::new(handle, cache, rpc, 1);

    let pm = synthetic_arb_pools();

    // Verify pool manager has pools and arb pairs
    assert!(pm.pool_count() > 0, "PoolManager should have pools");
    assert!(!pm.arbitrage_pairs().is_empty(), "PoolManager should have arbitrage pairs");

    // Direct detection should find arb
    let opps_direct = TwoHopArbDetector::new(1).detect(&pm, 0, 12345678, 50_000_000_000, GasConfig::default());
    eprintln!("Direct detection: {} opps", opps_direct.len());
    assert!(!opps_direct.is_empty(), "Direct detection should find arb");

    // Create runner and run_block
    let mut runner = BacktestRunner::new(replayer, pm, GasConfig::default());

    let (opps, stats, gas_prices) = runner.run_block(1).unwrap();

    eprintln!("run_block returned {} opportunities", opps.len());
    for opp in &opps {
        eprintln!("  opp: profit={}, gas_cost_wei={}", opp.expected_profit, opp.gas_cost_wei);
    }

    assert!(!opps.is_empty(), "Should detect arb between imbalanced pools");
    assert_eq!(stats.block_number, 1);
    assert_eq!(stats.total_tx_count, 2);
    assert_eq!(stats.dex_tx_count, 0, "No txs matched pools (fast path)");
    assert_eq!(gas_prices.len(), 2, "Gas prices from 2 txs");

    for opp in &opps {
        assert!(opp.expected_profit > U256::ZERO);
        assert!(opp.gas_cost_wei > 0);
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// ── Test 2: BacktestRunner::run_range() multi-block ─────────────────────────
#[tokio::test]
async fn test_runner_run_range_multi_block() {
    let dir = temp_test_dir("run_range_multi");
    let mut runner = make_synthetic_runner(&dir, 1, GasConfig::default());

    // Add second block to cache
    prep_synthetic_cache(&dir, 2, 1);

    let resolved = ResolvedRange {
        start_block: 1,
        end_block: 2,
        block_count: 2,
        mode: RangeMode::Range(1, 2),
    };

    let (opps, stats) = runner.run_range(&resolved).unwrap();

    assert!(!opps.is_empty(), "Should detect arb across blocks");
    assert_eq!(stats.len(), 2, "Stats from 2 blocks");
    assert!(stats.iter().any(|s| s.block_number == 1));
    assert!(stats.iter().any(|s| s.block_number == 2));

    let _ = std::fs::remove_dir_all(&dir);
}

/// ── Test 3: Gas model affects gas_cost_wei ──────────────────────────────────
#[tokio::test]
async fn test_runner_gas_model_p90() {
    let dir = temp_test_dir("gas_model");
    let opps_exact = {
        let cache = prep_synthetic_cache(&dir, 1, 2);
        let handle = tokio::runtime::Handle::current();
        let rpc = RpcClient::new("http://0.0.0.0:1", 1).unwrap();
        let replayer = BlockReplayer::new(handle, cache, rpc, 1);
        let pm = synthetic_arb_pools();

        let mut runner = BacktestRunner::new(replayer, pm, GasConfig::default());
        let (opps, _, _) = runner.run_block(1).unwrap();
        opps
    };

    let dir2 = temp_test_dir("gas_model_p90");
    let opps_p90_res = {
        let cache2 = prep_synthetic_cache(&dir2, 1, 2);
        let handle2 = tokio::runtime::Handle::current();
        let rpc2 = RpcClient::new("http://0.0.0.0:1", 1).unwrap();
        let replayer2 = BlockReplayer::new(handle2, cache2, rpc2, 1);
        let pm2 = synthetic_arb_pools();

        let gas_cfg_p90 = GasConfig {
            gas_model: GasModel::P90,
            ..GasConfig::default()
        };
        let mut runner_p90 = BacktestRunner::new(replayer2, pm2, gas_cfg_p90);
        let (opps, _, _) = runner_p90.run_block(1).unwrap();
        opps
    };

    assert!(!opps_exact.is_empty());
    assert!(!opps_p90_res.is_empty());

    let exact_gas = opps_exact[0].gas_cost_wei;
    let p90_gas = opps_p90_res[0].gas_cost_wei;
    eprintln!("Gas cost: historical_exact={exact_gas}, p90={p90_gas}");

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
}

/// ── Test 4: Proximity window affects JitArb detection ───────────────────────
#[test]
fn test_runner_proximity_window() {
    let pool_p = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let pool_q = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    let sender = address!("1111111111111111111111111111111111111111");
    let usdt_addr = address!("c2132d05d31c914a87c6611c10748aeb04b58e8f");

    fn v3_mint_log(pool: Address, lower: i32, upper: i32, amount: u128) -> ExecutedLog {
        let mut data = Vec::new();
        let mut padded = [0u8; 32];
        padded[28..32].copy_from_slice(&lower.to_be_bytes());
        data.extend_from_slice(&padded);
        padded = [0u8; 32];
        padded[28..32].copy_from_slice(&upper.to_be_bytes());
        data.extend_from_slice(&padded);
        padded = [0u8; 32];
        padded[16..32].copy_from_slice(&amount.to_be_bytes());
        data.extend_from_slice(&padded);
        ExecutedLog {
            address: pool,
            topics: vec![*V3_MINT_TOPIC, B256::ZERO, B256::ZERO],
            data: data.into(),
        }
    }

    fn v3_swap_log(pool: Address) -> ExecutedLog {
        ExecutedLog {
            address: pool,
            topics: vec![V3_SWAP_TOPIC, B256::ZERO, B256::ZERO],
            data: Bytes::from_static(&[0u8; 160]),
        }
    }

    let mut pm = PoolManager::new();
    pm.add_pool(PoolState::UniswapV2(UniswapV2PoolState {
        info: PoolInfo {
            address: pool_p,
            token0: wmatic(),
            token1: usdc(),
            fee: 30,
            name: None,
            dex_type: DexType::UniswapV2,
            tick_spacing: None,
            creation_block: 0,
            pool_id: None,
            factory: None,
        },
        reserve0: 1_000_000,
        reserve1: 1_000_000,
    }));
    pm.add_pool(PoolState::UniswapV2(UniswapV2PoolState {
        info: PoolInfo {
            address: pool_q,
            token0: usdc(),
            token1: usdt_addr,
            fee: 30,
            name: None,
            dex_type: DexType::UniswapV2,
            tick_spacing: None,
            creation_block: 0,
            pool_id: None,
            factory: None,
        },
        reserve0: 1_000_000,
        reserve1: 1_000_000,
    }));
    let pm = pm.with_wrapped_native(wmatic());

    let gas_cfg = GasConfig::default();

    // txs:
    //   0: mint on P only
    //   1: swap on P (marks mint as swapped)
    //   5: swap on Q
    // Gap between P-swap (idx 1) and Q-swap (idx 5) = 4
    // window=5 → 4 ≤ 5 → detected; window=1 → 4 > 1 → NOT detected
    let logs_for_tx = |i: usize| -> Vec<ExecutedLog> {
        match i {
            0 => vec![v3_mint_log(pool_p, -100, 100, 500_000)],
            1 => vec![v3_swap_log(pool_p)],
            5 => vec![v3_swap_log(pool_q)],
            _ => vec![],
        }
    };

    // Window = 5 — gap=4 ≤ 5 → JitArb detected
    let mut detector_wide = JitArbDetector::new(42).with_proximity_window(5);
    for i in 0..=5 {
        detector_wide.process_tx(i, &logs_for_tx(i), Some(sender), &pm);
    }
    let opps_wide = detector_wide.detect(12345, &pm, 0, &gas_cfg);
    assert_eq!(opps_wide.len(), 1, "Window=5 should detect JitArb (gap=4 ≤ 5)");

    // Window = 1 — gap=4 > 1 → NOT detected
    let mut detector_narrow = JitArbDetector::new(42).with_proximity_window(1);
    for i in 0..=5 {
        detector_narrow.process_tx(i, &logs_for_tx(i), Some(sender), &pm);
    }
    let opps_narrow = detector_narrow.detect(12345, &pm, 0, &gas_cfg);
    assert!(opps_narrow.is_empty(), "Window=1 should NOT detect JitArb (gap=4 > 1)");
}

/// ── Test 5: Cross-block detection via BacktestRunner ────────────────────────
#[tokio::test]
async fn test_runner_cross_block_detection() {
    let dir = temp_test_dir("cross_block");

    // Use synthetic_arb_pools which have imbalanced V2 pools:
    //   Pool A: USDC/WMATIC, reserve0=1e12, reserve1=2e18 → price = 2e6
    //   Pool B: USDT/WMATIC, reserve0=1e12, reserve1=5e17 → price = 5e5
    // Gap = ln(2e6 / 5e5) ≈ 1.39 > MIN_ARB_GAP (0.005) → persistent arb

    // Block 1
    let cache1 = prep_synthetic_cache(&dir, 1, 2);
    let handle = tokio::runtime::Handle::current();
    let rpc = RpcClient::new("http://0.0.0.0:1", 1).unwrap();
    let replayer1 = BlockReplayer::new(handle, cache1, rpc, 1);
    let pm = synthetic_arb_pools();

    let mut runner = BacktestRunner::new(replayer1, pm, GasConfig::default()).with_cross_block(3);

    let resolved = ResolvedRange {
        start_block: 1,
        end_block: 2,
        block_count: 2,
        mode: RangeMode::Range(1, 2),
    };

    // Add block 2 cache data before running
    prep_synthetic_cache(&dir, 2, 1);

    let (opps, stats) = runner.run_range(&resolved).unwrap();

    assert_eq!(stats.len(), 2, "Should process 2 blocks");

    // Cross-block opportunities should be emitted
    let cross_opps: Vec<_> = opps
        .iter()
        .filter(|o| o.strategy == Strategy::CrossBlockArb || o.strategy == Strategy::TimeBandit)
        .collect();

    assert!(
        !cross_opps.is_empty(),
        "Should detect cross-block opportunities with imbalanced pools across 2 blocks; got {} total opps",
        opps.len(),
    );

    // Verify confidence is set on cross-block opportunities
    for opp in &cross_opps {
        assert!(opp.confidence.is_some(), "Cross-block opps should have confidence set");
        let c = opp.confidence.unwrap();
        assert!(c > 0.0 && c <= 1.0, "Confidence should be in (0.0, 1.0], got {}", c);
    }

    // CrossBlockArb with persistence >= 2 across 2 blocks with window=3 → confidence = 2/3
    for opp in cross_opps.iter().filter(|o| o.strategy == Strategy::CrossBlockArb) {
        let c = opp.confidence.unwrap();
        assert!(
            (c - 2.0 / 3.0).abs() < 1e-6,
            "CrossBlockArb confidence should be 2/3 ≈ 0.667 with persistence=2, window=3, got {}",
            c
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}
