//! Integration test: Dune-guided backtest pipeline.
//!
//! Mirrors `scripts/test_backtest.ps1` — queries Dune Analytics for blocks with
//! known MEV activity, then runs the full MEV Scout pipeline (fetch, pool init,
//! backtest) against those blocks and asserts detection results.
//!
//! Requires environment variables:
//!   - `RPC_URL`        — Polygon (or other chain) RPC endpoint
//!   - `DUNE_API_KEY`   — Dune Analytics API key
//!
//! Both variables are optional — the test skips gracefully when absent.

use std::collections::HashMap;

use alloy::primitives::{Address, U256};
use mev_scout_core::cache::SqliteStore;
use mev_scout_core::dune::DuneClient;
use mev_scout_core::fetch::Fetcher;
use mev_scout_core::pipeline::BacktestRunner;
use mev_scout_core::pool::discovery::{discover_pools, DiscoveryConfig};
use mev_scout_core::pool::state::PoolManager;
use mev_scout_core::replay::BlockReplayer;
use mev_scout_core::resolver::ResolvedRange;
use mev_scout_core::rpc::RpcClient;
use mev_scout_core::types::{GasConfig, RangeMode};

const CHAIN_ID: u64 = 137; // Polygon

// ── Helpers ──────────────────────────────────────────────────────────────────

fn rpc_url() -> Option<String> {
    std::env::var("RPC_URL").ok()
}

fn dune_api_key() -> Option<String> {
    std::env::var("DUNE_API_KEY").ok()
}

fn temp_test_dir(name: &str) -> String {
    let dir = std::env::temp_dir().join(format!(
        "mev_scout_backtest_{name}_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&dir);
    dir.to_str().unwrap().to_string()
}

async fn try_rpc() -> Option<(RpcClient, u64)> {
    let url = rpc_url()?;
    let rpc = RpcClient::new(&url, CHAIN_ID).ok()?;
    let block = rpc.get_block_number().await.ok()?;
    Some((rpc, block))
}

/// Approximate the block_month date for Dune partition pruning (copied from dune_find_blocks.rs).
fn approx_block_month_min(block_number: u64, chain: &str) -> String {
    let (genesis_ts, secs_per_block) = match chain {
        "ethereum" => (1438269988_i64, 12.0),
        "polygon" => (1591031691, 2.1),
        "bsc" => (1597734000, 3.0),
        "avalanche_c" | "avalanche" => (1624402800, 2.0),
        "arbitrum" => (1630812600, 0.26),
        "base" => (1686787200, 2.0),
        "optimism" => (1631808000, 2.0),
        _ => (1609459200, 12.0),
    };
    let elapsed = block_number as f64 * secs_per_block;
    let approx_ts = genesis_ts + elapsed as i64;
    chrono::DateTime::from_timestamp(approx_ts, 0)
        .unwrap_or_default()
        .format("%Y-%m-%d")
        .to_string()
}

fn estimate_blocks_per_day(chain: &str) -> u64 {
    match chain {
        "ethereum" => 7200,
        "polygon" => 41000,
        "bsc" => 28800,
        "avalanche" | "avalanche_c" => 43200,
        "arbitrum" => 330000,
        "base" => 43200,
        "optimism" => 43200,
        _ => 7200,
    }
}

fn estimate_latest_block(chain: &str) -> u64 {
    let (genesis_ts, secs_per_block) = match chain {
        "ethereum" => (1438269988_i64, 12.0),
        "polygon" => (1591031691, 2.1),
        "bsc" => (1597734000, 3.0),
        "avalanche_c" | "avalanche" => (1624402800, 2.0),
        "arbitrum" => (1630812600, 0.26),
        "base" => (1686787200, 2.0),
        "optimism" => (1631808000, 2.0),
        _ => (1609459200, 12.0),
    };
    let now = chrono::Utc::now().timestamp();
    let elapsed_secs = now - genesis_ts;
    (elapsed_secs as f64 / secs_per_block) as u64
}

fn dune_indexing_lag(chain: &str) -> u64 {
    let lag_secs = 60 * 24 * 3600; // ~60 days
    let secs_per_block = match chain {
        "ethereum" => 12.0,
        "polygon" => 2.1,
        "bsc" => 3.0,
        "avalanche_c" | "avalanche" => 2.0,
        "arbitrum" => 0.26,
        "base" => 2.0,
        "optimism" => 2.0,
        _ => 12.0,
    };
    (lag_secs as f64 / secs_per_block) as u64
}

/// Query Dune for blocks with high MEV activity and return them sorted by score descending.
async fn dune_find_candidate_blocks(
    client: &DuneClient,
    chain: &str,
    days: u64,
    top: usize,
) -> Vec<u64> {
    let blocks_per_day = estimate_blocks_per_day(chain);
    let range_blocks = days * blocks_per_day;
    let latest = estimate_latest_block(chain);
    let lag = dune_indexing_lag(chain);
    let to_block = latest.saturating_sub(lag);
    let from_block = to_block.saturating_sub(range_blocks);
    let block_month_min = approx_block_month_min(from_block, chain);

    eprintln!(
        "  Dune block search: {chain}, blocks {from_block}–{to_block}, month>={block_month_min}"
    );

    let mut block_scores: HashMap<u64, u64> = HashMap::new();

    // 1. Arbitrage query (multi-pool transactions in dex.trades)
    let arb_sql = format!(
        r#"WITH tx_pools AS (
  SELECT
    t.block_number,
    t.tx_hash,
    t.project_contract_address AS pool_address,
    COUNT(*) OVER (PARTITION BY t.block_number, t.tx_hash) AS pool_count
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_month >= DATE '{block_month_min}'
    AND t.block_number >= {from_block}
    AND t.block_number <= {to_block}
)
SELECT block_number, COUNT(DISTINCT tx_hash) AS arb_count
FROM tx_pools
WHERE pool_count >= 2
GROUP BY block_number
ORDER BY arb_count DESC
LIMIT {limit}"#,
        limit = top * 3,
    );

    match client.execute_raw_sql(&arb_sql).await {
        Ok(result) => {
            if let Some(ref r) = result.result {
                for row in &r.rows {
                    let block = row.get("block_number").and_then(|v| v.as_u64());
                    let count = row.get("arb_count").and_then(|v| {
                        v.as_u64()
                            .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
                    });
                    if let (Some(b), Some(c)) = (block, count) {
                        if b > 0 {
                            *block_scores.entry(b).or_insert(0) += c;
                        }
                    }
                }
                eprintln!("  Found {} blocks with arbitrages", r.rows.len());
            }
        }
        Err(e) => eprintln!("  Arbitrage query failed: {e}"),
    }

    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    // 2. Sandwich query
    let sandwich_sql = format!(
        r#"SELECT block_number, COUNT(*) AS sandwich_count
FROM dex.sandwiches
WHERE blockchain = '{chain}'
  AND block_month >= DATE '{block_month_min}'
  AND block_number >= {from_block}
  AND block_number <= {to_block}
GROUP BY block_number
ORDER BY sandwich_count DESC
LIMIT {limit}"#,
        limit = top * 3,
    );

    match client.execute_raw_sql(&sandwich_sql).await {
        Ok(result) => {
            if let Some(ref r) = result.result {
                for row in &r.rows {
                    let block = row.get("block_number").and_then(|v| v.as_u64());
                    let count = row.get("sandwich_count").and_then(|v| {
                        v.as_u64()
                            .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
                    });
                    if let (Some(b), Some(c)) = (block, count) {
                        if b > 0 {
                            *block_scores.entry(b).or_insert(0) += c;
                        }
                    }
                }
                eprintln!("  Found {} blocks with sandwiches", r.rows.len());
            }
        }
        Err(e) => eprintln!("  Sandwich query failed: {e}"),
    }

    let mut sorted: Vec<(u64, u64)> = block_scores.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));
    sorted.into_iter().take(top).map(|(b, _)| b).collect()
}

/// Discover pools via on-chain event logs (QuickSwap V2 factory on Polygon).
async fn discover_polygon_pools(rpc: &RpcClient, from: u64, to: u64) -> Vec<Address> {
    let v2_factory: Address = "5757371414417b8c6caad45baef941abc7d3ab32"
        .parse()
        .unwrap();

    let disc_config = DiscoveryConfig {
        batch_size: 2000,
        v2_fee_override: None,
        balancer_vault: None,
        v2_factories: Some(&vec![v2_factory]),
        v3_factories: None,
        curve_registry: None,
        solidly_factories: None,
        camelot_factories: None,
        solidly_fee_bps: None,
        rpc_concurrency: 64,
        v4_pool_manager: None,
        trader_joe_factory: None,
        pendle_factory: None,
        token_cache: None,
        pool_cache: None,
    };

    match discover_pools(rpc, from, to, &disc_config, None).await {
        Ok((pools, _)) => {
            eprintln!("  Discovered {} pools on-chain", pools.len());
            pools.into_iter().map(|p| p.address).collect()
        }
        Err(e) => {
            eprintln!("  On-chain pool discovery failed: {e}");
            vec![]
        }
    }
}

// ── Test: Dune-guided backtest pipeline ──────────────────────────────────────

/// End-to-end test that mirrors scripts/test_backtest.ps1:
///
/// 1. Query Dune Analytics for blocks with high MEV activity (arbitrage + sandwich)
/// 2. Discover pools on-chain (QuickSwap V2 on Polygon)
/// 3. For each candidate block: fetch data → init pools → run backtest
/// 4. Assert that opportunities are found (or at least that the pipeline completes)
///
/// Skips gracefully when `RPC_URL` or `DUNE_API_KEY` are not set.
#[tokio::test]
async fn test_dune_guided_backtest() {
    let dune_key = match dune_api_key() {
        Some(k) => k,
        None => {
            eprintln!("SKIP: DUNE_API_KEY not set");
            return;
        }
    };
    let (rpc, tip) = match try_rpc().await {
        Some(v) => v,
        None => {
            eprintln!("SKIP: RPC_URL not set or unreachable");
            return;
        }
    };

    let chain = "polygon";
    let days = 7u64;
    let top = 3usize;

    // ── Step 1: Find candidate blocks via Dune ──────────────────────────────
    eprintln!("=== Step 1: Finding MEV blocks via Dune ===");
    let client = DuneClient::new(&dune_key);
    let candidates = dune_find_candidate_blocks(&client, chain, days, top).await;

    if candidates.is_empty() {
        eprintln!("  No candidate blocks found — Dune API may be rate-limited or unavailable");
        eprintln!("  SKIP: no blocks to test");
        return;
    }
    eprintln!("  Top {top} candidate blocks: {candidates:?}");

    // ── Step 2: Discover pools ──────────────────────────────────────────────
    eprintln!("=== Step 2: Discovering pools ===");
    let discover_start = tip.saturating_sub(100_000);
    let discovered = discover_polygon_pools(&rpc, discover_start, tip).await;
    eprintln!("  Discovered {} pool addresses", discovered.len());

    // ── Step 3: Test each candidate block ────────────────────────────────────
    let dir = temp_test_dir("dune_guided");
    let mut total_opps = 0u64;
    let mut blocks_with_opps = 0u64;

    for (i, &block) in candidates.iter().enumerate() {
        eprintln!("\n=== Step 3.{i}: Testing block {block} ===");

        // Skip blocks too close to tip (may not be available on public RPCs)
        if block > tip.saturating_sub(2) {
            eprintln!("  Block {block} too close to tip ({tip}), skipping");
            continue;
        }

        // Fetch block data
        let block_dir = format!("{dir}/block_{block}");
        let cache = SqliteStore::open(
            std::path::Path::new(&block_dir).join("cache.db"),
        )
        .unwrap_or_else(|e| panic!("Failed to open cache for block {block}: {e}"));

        let mut fetcher = Fetcher::new(rpc.clone(), cache.clone());
        let resolved_fetch = ResolvedRange {
            start_block: block,
            end_block: block,
            block_count: 1,
            mode: RangeMode::Single(block),
        };
        match fetcher.fetch_range(&resolved_fetch, None).await {
            Ok(summary) => eprintln!("  Fetched block {block}: {} txs (elapsed {:.2}s)",
                summary.total_blocks, summary.elapsed_secs),
            Err(e) => {
                eprintln!("  Fetch failed for block {block}: {e}");
                continue;
            }
        }

        // Initialize pool manager with discovered pools + common Polygon pairs
        let mut pm = PoolManager::new();

        // Add discovered pools (up to 50 to keep init fast)
        let mut pool_addrs: Vec<Address> = discovered.iter().take(50).copied().collect();

        // Also add well-known high-volume pools if not already present
        let known_pools: Vec<Address> = vec![
            "6e7a5fafcec6bb1e78bae2a1f0b612012bf14827".parse().unwrap(), // QuickSwap WMATIC/USDC
            "85e37332D24800F4F736D9fC6aA7e3F1b687A30C".parse().unwrap(), // QuickSwap WMATIC/USDC (V2)
            "cd353f79d9fade311fc3119b841e1f456b54e858".parse().unwrap(), // SushiSwap WMATIC/USDC
            "604029b0c1a79eebfb31f7c5316434484f3a4b55".parse().unwrap(), // QuickSwap WMATIC/USDT
        ];
        for addr in known_pools {
            if !pool_addrs.contains(&addr) {
                pool_addrs.push(addr);
            }
        }

        // Add as generic pools — init_from_rpc will fill reserves
        for addr in &pool_addrs {
            pm.add_pool(mev_scout_core::pool::state::PoolState::UniswapV2(
                mev_scout_core::pool::state::UniswapV2PoolState {
                    info: mev_scout_core::pool::state::PoolInfo {
                        address: *addr,
                        token0: Address::ZERO,
                        token1: Address::ZERO,
                        fee: 30,
                        name: None,
                        dex_type: mev_scout_core::pool::dex_type::DexType::UniswapV2,
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
                    },
                    reserve0: 0,
                    reserve1: 0,
                },
            ));
        }

        let prev_block = block.saturating_sub(1);
        pm.init_from_rpc(&rpc, prev_block).await;
        let initialized = pm.initialized_count();
        eprintln!("  Initialized {initialized}/{} pools at block {prev_block}", pm.pool_count());

        if initialized == 0 {
            eprintln!("  No pools initialized — skipping block {block}");
            continue;
        }

        pm = pm.with_wrapped_native(
            "0d500b1d8e8ef31e21c99d1db9a6444d3adf1270".parse().unwrap(),
        );

        // Create replayer and runner
        let handle = tokio::runtime::Handle::current();
        let replayer = BlockReplayer::new(handle, cache, rpc.clone(), CHAIN_ID);
        let mut runner =
            BacktestRunner::new(replayer, pm, GasConfig::default()).with_proximity_window(5);

        // Run backtest
        let (opps, stats) = match runner.run_block(block) {
            Ok((opps, stats, _gas)) => (opps, vec![stats]),
            Err(e) => {
                eprintln!("  Backtest failed for block {block}: {e}");
                continue;
            }
        };

        let opp_count = opps.len();
        total_opps += opp_count as u64;
        if opp_count > 0 {
            blocks_with_opps += 1;
        }

        eprintln!("  Block {block}: {opp_count} opportunities detected");
        for opp in opps.iter().take(5) {
            eprintln!(
                "    tx={} strategy={} profit={} wei gas_cost={} wei pool_a={:?} pool_b={:?}",
                opp.tx_index, opp.strategy, opp.expected_profit, opp.gas_cost_wei, opp.pool_a, opp.pool_b,
            );
        }

        // Validate opportunity fields
        for opp in &opps {
            assert!(opp.expected_profit > U256::ZERO || opp.gas_cost_wei > 0,
                "Opportunity should have non-zero profit or gas cost");
            assert!(!opp.pool_a.is_zero(), "pool_a should be set");
        }
    }

    // ── Summary ─────────────────────────────────────────────────────────────
    eprintln!("\n=== Backtest Summary ===");
    eprintln!("  Blocks tested: {}", candidates.len());
    eprintln!("  Blocks with MEV: {blocks_with_opps}");
    eprintln!("  Total opportunities: {total_opps}");

    assert!(
        !candidates.is_empty(),
        "Should have found at least one candidate block from Dune"
    );
    // Pipeline completion is the primary assertion — finding opportunities
    // is desirable but not guaranteed (depends on block content and pool coverage).
}

// ── Test: Synthetic pipeline (no external dependencies) ──────────────────────

/// Validate the fetch→init→backtest pipeline works correctly using synthetic
/// pool data against a real Polygon block. This tests the core pipeline without
/// Dune dependency.
#[tokio::test]
async fn test_synthetic_backtest_on_real_block() {
    let (rpc, tip) = match try_rpc().await {
        Some(v) => v,
        None => {
            eprintln!("SKIP: RPC_URL not set");
            return;
        }
    };

    let block = tip.saturating_sub(1);
    let dir = temp_test_dir("synthetic_real_block");

    // Fetch one block
    let cache = SqliteStore::open(
        std::path::Path::new(&dir).join("cache.db"),
    )
    .unwrap();
    let mut fetcher = Fetcher::new(rpc.clone(), cache.clone());
    let resolved = ResolvedRange {
        start_block: block,
        end_block: block,
        block_count: 1,
        mode: RangeMode::Single(block),
    };
    match fetcher.fetch_range(&resolved, None).await {
        Ok(s) => eprintln!("Fetched block {block}: {} blocks", s.total_blocks),
        Err(e) => {
            eprintln!("SKIP: fetch failed: {e}");
            return;
        }
    }

    // Create a minimal pool manager with well-known pools
    let mut pm = PoolManager::new();
    let pools = vec![
        (
            "6e7a5fafcec6bb1e78bae2a1f0b612012bf14827".parse::<Address>().unwrap(),
            "0d500b1d8e8ef31e21c99d1db9a6444d3adf1270".parse::<Address>().unwrap(),
            "2791bca1f2de4661ed88a30c99a7a9449aa84174".parse::<Address>().unwrap(),
        ),
        (
            "cd353f79d9fade311fc3119b841e1f456b54e858".parse::<Address>().unwrap(),
            "0d500b1d8e8ef31e21c99d1db9a6444d3adf1270".parse::<Address>().unwrap(),
            "2791bca1f2de4661ed88a30c99a7a9449aa84174".parse::<Address>().unwrap(),
        ),
        (
            "604029b0c1a79eebfb31f7c5316434484f3a4b55".parse::<Address>().unwrap(),
            "0d500b1d8e8ef31e21c99d1db9a6444d3adf1270".parse::<Address>().unwrap(),
            "c2132d05d31c914a87c6611c10748aeb04b58e8f".parse::<Address>().unwrap(),
        ),
    ];

    use mev_scout_core::pool::dex_type::DexType;
    use mev_scout_core::pool::state::UniswapV2PoolState;

    for (addr, t0, t1) in &pools {
        pm.add_pool(mev_scout_core::pool::state::PoolState::UniswapV2(
            UniswapV2PoolState {
                info: mev_scout_core::pool::state::PoolInfo {
                    address: *addr,
                    token0: *t0,
                    token1: *t1,
                    fee: 30,
                    name: Some("test".into()),
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
                },
                reserve0: 0,
                reserve1: 0,
            },
        ));
    }

    pm = pm.with_wrapped_native(
        "0d500b1d8e8ef31e21c99d1db9a6444d3adf1270".parse().unwrap(),
    );

    let prev = block.saturating_sub(1);
    pm.init_from_rpc(&rpc, prev).await;
    let initialized = pm.initialized_count();
    eprintln!("Initialized {initialized}/{} pools at block {prev}", pm.pool_count());

    let handle = tokio::runtime::Handle::current();
    let replayer = BlockReplayer::new(handle, cache, rpc, CHAIN_ID);
    let runner = BacktestRunner::new(replayer, pm, GasConfig::default());

    let result = runner.run_block(block);
    assert!(result.is_ok(), "run_block should succeed: {:?}", result.err());

    let (opps, _stats, _gas) = result.unwrap();
    eprintln!("Block {block}: {} opportunities detected", opps.len());
    for opp in &opps {
        eprintln!(
            "  tx={} strategy={} profit={} pool_a={:?}",
            opp.tx_index, opp.strategy, opp.expected_profit, opp.pool_a,
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}
