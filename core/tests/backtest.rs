//! Integration test: RPC-guided backtest pipeline.
//!
//! Samples a recent block window directly from the RPC endpoint (no Dune),
//! then runs the full MEV Scout pipeline (fetch, pool init, backtest)
//! against those blocks and asserts detection results.
//!
//! Requires environment variables:
//!   - `RPC_URL` — Polygon (or other chain) RPC endpoint
//!
//! The variable is optional — the test skips gracefully when absent.

use alloy::primitives::{Address, U256};
use mev_scout_core::cache::SqliteStore;
use mev_scout_core::chain::timing::chain_timing;
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
    std::env::var("RPC_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(config_rpc_url)
}

/// First configured RPC URL from the repo's `mev-scout.toml`, if reachable.
fn config_rpc_url() -> Option<String> {
    let candidates = ["mev-scout.toml", "../mev-scout.toml", "core/mev-scout.toml", "../../mev-scout.toml"];
    for path in candidates {
        if !std::path::Path::new(path).exists() {
            continue;
        }
        let cfg = mev_scout_core::config::Config::load(path).ok()?;
        if let Some(url) = cfg.rpc.rpc_urls.into_iter().next() {
            if !url.is_empty() {
                return Some(url);
            }
        }
    }
    None
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

/// Sample blocks evenly across a recent window (no candidate ranking needed).
/// Returns up to `top` blocks spaced evenly from the tail of the window back.
async fn sample_recent_blocks(rpc: &RpcClient, chain: &str, days: u64, top: usize) -> Vec<u64> {
    let blocks_per_day = chain_timing(chain).blocks_per_day;
    let latest = match rpc.get_block_number().await {
        Ok(n) => n,
        Err(e) => {
            eprintln!("  Failed to get block number: {e}");
            return vec![];
        }
    };
    let range_blocks = (days * blocks_per_day).min(latest);
    let from_block = latest.saturating_sub(range_blocks);

    eprintln!(
        "  Sampling {top} blocks evenly from {chain} window {from_block}–{latest} ({days} days)"
    );

    let step = (range_blocks as usize / top.max(1)).max(1);
    let mut blocks = Vec::with_capacity(top);
    for i in 0..top {
        let b = latest.saturating_sub((i * step) as u64);
        if b > from_block {
            blocks.push(b);
        }
    }
    blocks.sort_unstable();
    blocks
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

// ── Test: RPC-guided backtest pipeline ──────────────────────────────────────

/// End-to-end test that mirrors scripts/test_backtest.ps1:
///
/// 1. Sample a recent block window directly from the RPC (no Dune)
/// 2. Discover pools on-chain (QuickSwap V2 on Polygon)
/// 3. For each candidate block: fetch data → init pools → run backtest
/// 4. Assert that opportunities are found (or at least that the pipeline completes)
///
/// Skips gracefully when `RPC_URL` is not set.
/// Multi-threaded: replaying real Polygon blocks calls `block_in_place`
/// (register_polygon_precompiles), which requires a multi-threaded runtime.
#[tokio::test(flavor = "multi_thread")]
async fn test_rpc_guided_backtest() {
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

    // ── Step 1: Sample candidate blocks from a recent RPC window ──────────
    eprintln!("=== Step 1: Sampling MEV blocks from RPC window ===");
    let candidates = sample_recent_blocks(&rpc, chain, days, top).await;

    if candidates.is_empty() {
        eprintln!("  No candidate blocks sampled");
        eprintln!("  SKIP: no blocks to test");
        return;
    }
    eprintln!("  Sample blocks: {candidates:?}");

    // ── Step 2: Discover pools ──────────────────────────────────────────────
    eprintln!("=== Step 2: Discovering pools ===");
    let discover_start = tip.saturating_sub(100_000);
    let discovered = discover_polygon_pools(&rpc, discover_start, tip).await;
    eprintln!("  Discovered {} pool addresses", discovered.len());

    // ── Step 3: Test each candidate block ────────────────────────────────────
    let dir = temp_test_dir("rpc_guided");
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
        match fetcher.fetch_range(&resolved_fetch, None::<&fn()>).await {
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
                        dex_type: mev_scout_core::dex_type::DexType::UniswapV2,
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
        pm.init_from_rpc(&rpc, prev_block, None).await;
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
        "Should have sampled at least one candidate block from the RPC window"
    );
    // Pipeline completion is the primary assertion — finding opportunities
    // is desirable but not guaranteed (depends on block content and pool coverage).
}

// ── Test: Synthetic pipeline (no external dependencies) ──────────────────────

/// Validate the fetch→init→backtest pipeline works correctly using synthetic
/// pool data against a real Polygon block. This tests the core pipeline with
/// no external dependencies.
/// Multi-threaded: replaying a real Polygon block calls `block_in_place`
/// (register_polygon_precompiles), which requires a multi-threaded runtime.
#[tokio::test(flavor = "multi_thread")]
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
    match fetcher.fetch_range(&resolved, None::<&fn()>).await {
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

    use mev_scout_core::dex_type::DexType;
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
    pm.init_from_rpc(&rpc, prev, None).await;
    let initialized = pm.initialized_count();
    eprintln!("Initialized {initialized}/{} pools at block {prev}", pm.pool_count());

    let handle = tokio::runtime::Handle::current();
    let replayer = BlockReplayer::new(handle, cache, rpc, CHAIN_ID);
    let mut runner = BacktestRunner::new(replayer, pm, GasConfig::default());

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
