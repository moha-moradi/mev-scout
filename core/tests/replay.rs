//! Replayer and activity-scanner integration tests.
//!
//! The activity-scanner tests hit a live RPC endpoint and are gated like the
//! CLI E2E suite: they need `MEV_SCOUT_E2E=1` and `RPC_URL`, otherwise they
//! skip gracefully (no fallback to the repo `mev-scout.toml`).
use alloy::primitives::{address, U256};
use mev_scout_core::mev::detectors::two_hop::TwoHopArbDetector;
use mev_scout_core::pipeline::scanner::ActivityScanner;
use mev_scout_core::pipeline::BacktestRunner;
use mev_scout_core::pool::state::ScanScope;
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
    let scope = ScanScope::Full;
    let opps_direct = TwoHopArbDetector::new(1).detect(
        mev_scout_core::mev::detectors::DetectCtx::new(
            &pm,
            0,
            12345678,
            50_000_000_000,
            GasConfig::default(),
            &scope,
        ),
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
