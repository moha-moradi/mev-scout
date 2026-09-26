//! Replayer and activity-scanner integration tests.
//!
//! The activity-scanner tests hit a live RPC endpoint and are gated like the
//! CLI E2E suite: they need `MEV_SCOUT_E2E=1` and `RPC_URL`, otherwise they
//! skip gracefully (no fallback to the repo `mev-scout.toml`).
use alloy::primitives::{address, Address, Bytes, B256, U256};
use mev_scout_core::data::ExecutedLog;
use mev_scout_core::dex_type::DexType;
use mev_scout_core::mev::detectors::jit_arb::JitArbDetector;
use mev_scout_core::mev::detectors::two_hop::TwoHopArbDetector;
use mev_scout_core::pipeline::scanner::ActivityScanner;
use mev_scout_core::pipeline::BacktestRunner;
use mev_scout_core::pool::decoders::{V3_MINT_TOPIC, V3_SWAP_TOPIC};
use mev_scout_core::pool::state::{
    PoolInfo, PoolManager, PoolState, ScanScope, UniswapV2PoolState,
};
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
    assert!(
        !active.is_empty(),
        "Should find at least one active block for a high-volume pool"
    );
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

    eprintln!(
        "Multi-batch scan [{start}..{end}] (batch=1): {} active blocks",
        active.len()
    );
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
    assert!(
        !pm.arbitrage_pairs().is_empty(),
        "PoolManager should have arbitrage pairs"
    );

    // Direct detection should find arb
    let opps_direct = TwoHopArbDetector::new(1).detect(
        &pm,
        0,
        12345678,
        50_000_000_000,
        GasConfig::default(),
        &ScanScope::Full,
    );
    eprintln!("Direct detection: {} opps", opps_direct.len());
    assert!(!opps_direct.is_empty(), "Direct detection should find arb");

    // Create runner and run_block
    let mut runner = BacktestRunner::new(replayer, pm, GasConfig::default());

    let (opps, stats, gas_prices) = runner.run_block(1).unwrap();

    eprintln!("run_block returned {} opportunities", opps.len());
    for opp in &opps {
        eprintln!(
            "  opp: profit={}, gas_cost_wei={}",
            opp.expected_profit, opp.gas_cost_wei
        );
    }

    assert!(
        !opps.is_empty(),
        "Should detect arb between imbalanced pools"
    );
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

/// ── Test 1b: Deterministic golden `run_block` output on the fixed synthetic
/// cache (C.2 offline fixture) ─────────────────────────────────────────────
///
/// `GasModel::HistoricalExact` pins gas so a single `run_block` is
/// fully deterministic given a warm cache. Asserts the derived facts that the
/// pipeline cross-check (`explorer validate`) relies on: kind present,
/// every retained op is net-positive (`expected_profit > gas_cost_wei`), and
/// canonical ids are well-formed + unique. Exact profit amounts are never
/// asserted (classifier drift tolerance), matching the corpus convention.
#[tokio::test]
async fn test_runner_run_block_synthetic_golden() {
    let dir = temp_test_dir("run_block_synth_golden");
    let gas_cfg = GasConfig {
        gas_model: GasModel::HistoricalExact,
        ..GasConfig::default()
    };

    let run = |dir: &str| -> Vec<mev_scout_core::types::MevOpportunity> {
        let mut runner = make_synthetic_runner(dir, 1, gas_cfg);
        let (opps, stats, _) = runner.run_block(1).unwrap();
        assert_eq!(stats.total_tx_count, 2, "synthetic block carries 2 txs");
        opps
    };

    let opps = run(&dir);
    assert!(
        !opps.is_empty(),
        "imbalanced synthetic pools must yield arbitrage opportunities"
    );

    // Kind present: the synthetic V2 pair set fires the arb family.
    assert!(
        opps.iter()
            .any(|o| matches!(o.strategy, Strategy::TwoHopArb | Strategy::MultiHopArb)),
        "expected at least one arb-family opportunity"
    );

    // Every opportunity that survived `retain_with_rejections` is net-positive.
    for opp in &opps {
        assert!(
            opp.expected_profit > U256::from(opp.gas_cost_wei),
            "retained op must be net-positive: profit={} gas={}",
            opp.expected_profit,
            opp.gas_cost_wei
        );
        let cid = opp
            .canonical_id
            .as_deref()
            .unwrap_or_else(|| panic!("opp must carry a canonical id"));
        assert!(!cid.is_empty(), "canonical id must be non-empty");
        assert!(
            cid.starts_with(&format!("{:?}", opp.strategy)),
            "canonical id must start with the strategy name: {cid}"
        );
        assert!(
            cid.contains("0x"),
            "canonical id must embed pool addresses: {cid}"
        );
    }
    let mut ids: Vec<&String> = opps.iter().flat_map(|o| o.canonical_id.as_ref()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(
        ids.len(),
        opps.iter().filter(|o| o.canonical_id.is_some()).count(),
        "canonical ids must be unique within a block"
    );

    // Determinism: a fresh identical runner produces the identical sequence.
    let opps2 = run(&dir);
    assert_eq!(
        opps.len(),
        opps2.len(),
        "golden run_block must be deterministic"
    );
    for (a, b) in opps.iter().zip(opps2.iter()) {
        assert_eq!(a.block_number, b.block_number);
        assert_eq!(a.tx_index, b.tx_index);
        assert_eq!(a.strategy, b.strategy, "strategy must be deterministic");
        assert_eq!(
            a.expected_profit, b.expected_profit,
            "expected_profit must be deterministic on a pinned gas model"
        );
        assert_eq!(
            a.gas_cost_wei, b.gas_cost_wei,
            "gas_cost_wei must be deterministic on a pinned gas model"
        );
        assert_eq!(a.canonical_id, b.canonical_id);
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

    let (opps, stats) = runner.run_range(&resolved, None).unwrap();

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
            gas_model: GasModel::Distribution(90),
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

    /// Build a canonical Uniswap V3 `Mint` log, matching
    /// v3-core `IUniswapV3PoolEvents`:
    ///   topics: [sig, owner, tickLower, tickUpper]
    ///   data:   [sender, amount, amount0, amount1]
    fn v3_mint_log(pool: Address, lower: i32, upper: i32, amount: u128) -> ExecutedLog {
        // A 32-byte topic holding an int24: left-padded, sign-extended when negative.
        let tick_topic = |v: i32| -> B256 {
            let fill = if v < 0 { 0xffu8 } else { 0x00u8 };
            let mut w = [fill; 32];
            w[28..32].copy_from_slice(&v.to_be_bytes());
            B256::from(w)
        };
        let mut data = Vec::new();
        // sender
        data.extend_from_slice(&[0u8; 32]);
        // amount (uint128, left-padded into a 32-byte word)
        let mut w = [0u8; 32];
        w[16..32].copy_from_slice(&amount.to_be_bytes());
        data.extend_from_slice(&w);
        // amount0, amount1
        data.extend_from_slice(&[0u8; 32]);
        data.extend_from_slice(&[0u8; 32]);
        ExecutedLog {
            address: pool,
            topics: vec![
                V3_MINT_TOPIC,
                B256::ZERO,
                tick_topic(lower),
                tick_topic(upper),
            ],
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
    assert_eq!(
        opps_wide.len(),
        1,
        "Window=5 should detect JitArb (gap=4 ≤ 5)"
    );

    // Window = 1 — gap=4 > 1 → NOT detected
    let mut detector_narrow = JitArbDetector::new(42).with_proximity_window(1);
    for i in 0..=5 {
        detector_narrow.process_tx(i, &logs_for_tx(i), Some(sender), &pm);
    }
    let opps_narrow = detector_narrow.detect(12345, &pm, 0, &gas_cfg);
    assert!(
        opps_narrow.is_empty(),
        "Window=1 should NOT detect JitArb (gap=4 > 1)"
    );
}
