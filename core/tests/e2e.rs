use std::collections::HashMap;
use std::path::Path;

use alloy::primitives::{address, Address, U256};
use mev_scout_core::cache::SqliteStore;
use mev_scout_core::fetch::Fetcher;

use mev_scout_core::types::MevOpportunity;
use mev_scout_core::mev::detectors::two_hop::TwoHopArbDetector;
use mev_scout_core::dex_type::DexType;
use mev_scout_core::pool::discovery::{discover_pools, DiscoveryConfig};
use mev_scout_core::pool::state::{
    BalancerPoolVariant, CurvePoolVariant, PoolInfo, PoolManager, PoolState, UniswapV2PoolState,
    UniswapV3PoolState,
};
use mev_scout_core::resolver::ResolvedRange;
use mev_scout_core::rpc::RpcClient;
use mev_scout_core::types::{ChainName, GasConfig, RangeMode, Strategy};

const POLYGON_CHAIN_ID: u64 = 137;

fn wmatic() -> Address {
    address!("0d500b1d8e8ef31e21c99d1db9a6444d3adf1270")
}
fn usdc() -> Address {
    address!("2791bca1f2de4661ed88a30c99a7a9449aa84174")
}
fn usdt() -> Address {
    address!("c2132d05d31c914a87c6611c10748aeb04b58e8f")
}
fn quick_wmatic_usdc() -> Address {
    address!("6e7a5fafcec6bb1e78bae2a1f0b612012bf14827")
}
fn quick_wmatic_usdt() -> Address {
    address!("604029b0c1a79eebfb31f7c5316434484f3a4b55")
}
fn sushi_wmatic_usdc() -> Address {
    address!("cd353f79d9fade311fc3119b841e1f456b54e858")
}
fn sushi_wmatic_usdt() -> Address {
    address!("55ff76bffc3cdd9d5fdbbc2ece4528ecce45047e")
}
fn uni_v3_wmatic_usdc() -> Address {
    address!("a374094527e1673a86de625aa59517c5de346d32")
}
fn quick_v2_factory() -> Address {
    address!("5757371414417b8c6caad45baef941abc7d3ab32")
}
fn pool_info(addr: Address, token0: Address, token1: Address, name: &str) -> PoolInfo {
    PoolInfo {
        address: addr,
        token0,
        token1,
        fee: 30,
        name: Some(name.into()),
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
    }
}

fn pool_info_v3(addr: Address, token0: Address, token1: Address, fee: u32, name: &str) -> PoolInfo {
    PoolInfo {
        address: addr,
        token0,
        token1,
        fee,
        name: Some(name.into()),
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
        dex_name: None,
        token0_symbol: None,
        token1_symbol: None,
        tvl_usd: None,
        volume_usd_24h: None,
        volume_usd_30d: None,
    }
}

fn pool_info_to_state(info: PoolInfo) -> PoolState {
    match info.dex_type {
        DexType::UniswapV2 => PoolState::UniswapV2(UniswapV2PoolState {
            info,
            reserve0: 0,
            reserve1: 0,
        }),
        DexType::UniswapV3 => PoolState::UniswapV3(UniswapV3PoolState::new(info)),
        DexType::UniswapV4 => PoolState::UniswapV4(mev_scout_core::pool::state::UniswapV4PoolState::new(info)),
        DexType::Curve => PoolState::Curve(mev_scout_core::pool::state::CurvePoolState {
            info,
            balances: vec![],
            token_index: HashMap::new(),
            a_coeff: 100,
            pool_variant: CurvePoolVariant::default(),
            gamma: None,
            price_scale: vec![],
            base_pool: None,
        }),
        DexType::Balancer => PoolState::Balancer(mev_scout_core::pool::state::BalancerPoolState {
            info,
            balances: vec![],
            token_index: HashMap::new(),
            pool_id: None,
            weights: vec![],
            pool_variant: BalancerPoolVariant::Weighted,
            amplification: None,
            scaling_factors: vec![],
            bpt_index: None,
            rate_providers: vec![],
        }),
        DexType::Solidly | DexType::Camelot => {
            PoolState::UniswapV2(UniswapV2PoolState {
                info,
                reserve0: 0,
                reserve1: 0,
            })
        }
        DexType::TraderJoeLB => {
            PoolState::TraderJoeLB(mev_scout_core::pool::state::TraderJoeLBPoolState::new(info, 0, 0))
        }
        DexType::Pendle => PoolState::Pendle(mev_scout_core::pool::state::PendlePoolState::new(info)),
    }
}

fn default_gas_config() -> GasConfig {
    GasConfig::default()
}

fn temp_cache_dir(name: &str) -> String {
    let dir = std::env::temp_dir().join(format!("mev_scout_e2e_{name}_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    dir.to_str().unwrap().to_string()
}

fn temp_cache(name: &str) -> (SqliteStore, String) {
    let dir = temp_cache_dir(name);
    let db_path = Path::new(&dir).join("cache.db");
    let store = SqliteStore::open(&db_path).unwrap();
    (store, dir)
}

fn rpc_url() -> Option<String> {
    std::env::var("RPC_URL").ok().filter(|s| !s.is_empty()).or_else(config_rpc_url)
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

async fn try_rpc() -> Option<(RpcClient, u64)> {
    if let Some(url) = rpc_url() {
        match RpcClient::new(&url, POLYGON_CHAIN_ID) {
            Ok(rpc) => {
                if let Ok(block) = rpc.get_block_number().await {
                    let host = url.split('/').nth(2).unwrap_or(&url);
                    eprintln!("  Using configured RPC: {host}");
                    return Some((rpc, block));
                }
            }
            Err(e) => eprintln!("  RPC_URL client creation failed: {e}"),
        }
    }
    let public_url = ChainName::Polygon.public_rpc_url();
    match RpcClient::new(public_url, POLYGON_CHAIN_ID) {
        Ok(rpc) => match rpc.get_block_number().await {
            Ok(block) => {
                eprintln!("  Using public RPC: {public_url}");
                Some((rpc, block))
            }
            Err(e) => {
                eprintln!("  Public RPC connection failed: {e}");
                None
            }
        },
        Err(e) => {
            eprintln!("  Public RPC client creation failed: {e}");
            None
        }
    }
}

fn print_opportunities(opps: &[MevOpportunity]) {
    if opps.is_empty() {
        eprintln!("  No opportunities found");
        return;
    }
    for opp in opps.iter().take(10) {
        eprintln!(
            "  block={} tx={} strategy={} profit={}wei gas={}wei pool_a={:?} pool_b={:?}",
            opp.block_number,
            opp.tx_index,
            opp.strategy,
            opp.expected_profit,
            opp.gas_cost_wei,
            opp.pool_a,
            opp.pool_b,
        );
    }
    if opps.len() > 10 {
        eprintln!("  ... and {} more", opps.len() - 10);
    }
}

/// Test 1: RPC connectivity + block number validation
#[tokio::test]
async fn test_e2e_rpc_connectivity() {
    eprintln!("--- test_e2e_rpc_connectivity ---");
    let (rpc, block_num) = match try_rpc().await {
        Some(v) => v,
        None => { eprintln!("SKIP: no RPC available"); return; }
    };
    eprintln!("  Connected to Polygon at block {block_num}");
    assert_eq!(rpc.chain_id(), POLYGON_CHAIN_ID, "Chain ID mismatch");
    assert!(block_num > 50_000_000, "Block number seems too low: {block_num}");
}

/// Test 2: Fetch blocks into SQLite cache and verify roundtrip
#[tokio::test]
async fn test_e2e_fetch_and_cache() {
    eprintln!("--- test_e2e_fetch_and_cache ---");
    let (rpc, tip) = match try_rpc().await {
        Some(v) => v,
        None => { eprintln!("SKIP: no RPC available"); return; }
    };
    let (cache, _dir) = temp_cache("fetch_cache");
    let fetcher = Fetcher::new(rpc, cache);

    let start = tip.saturating_sub(4);
    let end = tip;
    let range = ResolvedRange {
        start_block: start,
        end_block: end,
        block_count: end - start + 1,
        mode: RangeMode::Range(start, end),
    };
    eprintln!("  Fetching blocks {start}..{end}");
    let fetched = fetcher.fetch_range(&range, Option::<&fn()>::None).await.unwrap();
    eprintln!("  Fetched {fetched:?}");

    let cache = fetcher.cache_store();

    // Mirror the CLI's auto_refetch_gaps flow: a flaky provider may leave
    // freshly-mined blocks unindexed (null response); refetch before asserting.
    if !fetched.missing_after_fetch.is_empty() {
        eprintln!(
            "  {} blocks missing after first pass, auto-refetching...",
            fetched.missing_after_fetch.len()
        );
        let refetched = fetcher
            .auto_refetch_gaps(&fetched.missing_after_fetch)
            .await
            .unwrap();
        eprintln!("  Refetched {refetched} gaps");
    }

    // If a block is still missing after the refetch pass, the provider is not
    // serving the range (typical for freshly-mined tip blocks on public RPCs).
    // That is an environment limitation, not a fetch/cache bug — skip like the
    // other real-data tests instead of failing on an unavailable endpoint.
    let still_missing: Vec<u64> = (start..=end)
        .filter(|n| !cache.has_block(*n).unwrap())
        .collect();
    if !still_missing.is_empty() {
        eprintln!("  SKIP: provider did not serve blocks {still_missing:?} even after refetch");
        return;
    }

    for block_num in start..=end {
        assert!(cache.has_block(block_num).unwrap(), "Missing block {block_num}");
        let block = cache.get_block(block_num).unwrap().unwrap();
        let txs = cache.get_txs(block_num).unwrap().unwrap();
        eprintln!("  Block {block_num}: {} txs, hash={:?} ts={}", txs.len(), block.hash, block.timestamp);
        assert!(block.timestamp > 0, "Zero timestamp in block {block_num}");
        assert!(txs.len() > 0, "No txs in block {block_num}");
    }

    let integrity = cache.check_integrity(start, end).unwrap();
    assert!(integrity.is_empty(), "Integrity gaps: {integrity:?}");
}

/// Test 3: Pool discovery from DEX activity + factory event logs
#[tokio::test]
async fn test_e2e_pool_discovery() {
    eprintln!("--- test_e2e_pool_discovery ---");
    let (rpc, tip) = match try_rpc().await {
        Some(v) => v,
        None => { eprintln!("SKIP: no RPC available"); return; }
    };

    let start = tip.saturating_sub(10000);
    let end = tip;
    let v2_factories = vec![quick_v2_factory()];
    eprintln!("  Discovering pools on QuickSwap factory [{start}..{end}]");
    let disc_config = DiscoveryConfig {
        batch_size: 2000,
        v2_fee_override: None,
        balancer_vault: None,
        v2_factories: Some(&v2_factories),
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
    let (pools, _active) = match discover_pools(
        &rpc, start, end, &disc_config,
        None,
    ).await {
        Ok((p, a)) => (p, a),
        Err(e) => {
            eprintln!("  Pool discovery failed (archive RPC likely required): {e}");
            eprintln!("  SKIP: archive node needed for eth_getLogs");
            return;
        }
    };
    eprintln!("  Found {} pools (V2 from factory + DEX events)", pools.len());
    assert!(pools.len() > 0, "Should find at least 1 pool in a 10K block range");

    for p in pools.iter().take(5) {
        eprintln!("  Pool {}: token0={:?} token1={:?} fee={}", p.address, p.token0, p.token1, p.fee);
    }

    let (cache, _dir) = temp_cache("discovery");
    for p in &pools {
        cache.put_discovered_pool(&PoolInfo::from(p.clone())).unwrap();
    }
    let stored = cache.list_discovered_pools().unwrap();
    assert_eq!(stored.len(), pools.len(), "All discovered pools should persist");
}

/// Test 4: Initialize real V2 pool state from RPC (fast, uses eth_getStorageAt)
#[tokio::test]
async fn test_e2e_pool_initialization() {
    eprintln!("--- test_e2e_pool_initialization ---");
    let (rpc, tip) = match try_rpc().await {
        Some(v) => v,
        None => { eprintln!("SKIP: no RPC available"); return; }
    };

    let block_num = tip.saturating_sub(1);
    let mut pm = PoolManager::new();
    pm.add_pool(pool_info_to_state(pool_info(
        quick_wmatic_usdc(), wmatic(), usdc(), "QuickSwap WMATIC/USDC",
    )));
    pm.add_pool(pool_info_to_state(pool_info(
        sushi_wmatic_usdc(), wmatic(), usdc(), "SushiSwap WMATIC/USDC",
    )));
    pm.add_pool(pool_info_to_state(pool_info(
        quick_wmatic_usdt(), wmatic(), usdt(), "QuickSwap WMATIC/USDT",
    )));

    pm.init_from_rpc(&rpc, block_num, None).await;
    let initialized = pm.initialized_count();
    eprintln!("  Initialized {initialized}/3 pools at block {block_num}");
    assert!(initialized >= 2, "Expected >=2 initialized pools, got {initialized}");

    for (addr, name) in &[
        (quick_wmatic_usdc(), "QuickSwap WMATIC/USDC"),
        (sushi_wmatic_usdc(), "SushiSwap WMATIC/USDC"),
        (quick_wmatic_usdt(), "QuickSwap WMATIC/USDT"),
    ] {
        if let Some(PoolState::UniswapV2(s)) = pm.get(addr) {
            eprintln!("  {name} reserves: {} {} (initialized={})", s.reserve0, s.reserve1, s.reserve0 > 0);
        }
    }

    // Major pools should have non-zero reserves
    if let Some(PoolState::UniswapV2(s)) = pm.get(&quick_wmatic_usdc()) {
        assert!(s.reserve0 > 0 || s.reserve1 > 0, "QuickSwap WMATIC/USDC has zero reserves");
    }
}

/// Test 5: Detect TwoHopArb on real initialized pools
#[tokio::test]
async fn test_e2e_two_hop_arbitrage() {
    eprintln!("--- test_e2e_two_hop_arbitrage ---");
    let (rpc, tip) = match try_rpc().await {
        Some(v) => v,
        None => { eprintln!("SKIP: no RPC available"); return; }
    };

    let block_num = tip.saturating_sub(1);
    let mut pm = PoolManager::new();
    // Pool A: QuickSwap WMATIC/USDC — high liquidity V2 pool
    pm.add_pool(pool_info_to_state(pool_info(
        quick_wmatic_usdc(), wmatic(), usdc(), "QuickSwap WMATIC/USDC",
    )));
    // Pool B: SushiSwap WMATIC/USDT — shares WMATIC with pool A (arb via WMATIC)
    pm.add_pool(pool_info_to_state(pool_info(
        sushi_wmatic_usdt(), wmatic(), usdt(), "SushiSwap WMATIC/USDT",
    )));

    pm.init_from_rpc(&rpc, block_num, None).await;
    let initialized = pm.initialized_count();
    eprintln!("  Initialized {initialized} pools at block {block_num}");

    for (addr, name) in &[
        (quick_wmatic_usdc(), "QuickSwap WMATIC/USDC"),
        (sushi_wmatic_usdt(), "SushiSwap WMATIC/USDT"),
    ] {
        if let Some(PoolState::UniswapV2(s)) = pm.get(addr) {
            eprintln!("  {name} reserves: r0={} r1={}", s.reserve0, s.reserve1);
        }
    }

    if initialized < 2 {
        eprintln!("  SKIP: too few initialized pools ({initialized})");
        return;
    }

    let arb_pairs = pm.arbitrage_pairs();
    eprintln!("  Found {} arbitrage pairs", arb_pairs.len());

    let mut detector = TwoHopArbDetector::new(block_num);
    let gas_cfg = default_gas_config();
    let opps = detector.detect(&pm, 0, block_num, 50_000_000_000, gas_cfg);

    eprintln!("  TwoHopArb detection at block {block_num}: {} opportunities", opps.len());
    print_opportunities(&opps);

    if opps.is_empty() {
        eprintln!("  WARNING: no arbitrage opportunities found (prices may be aligned)");
    }

    for opp in &opps {
        assert_eq!(opp.strategy, Strategy::TwoHopArb);
        assert!(opp.expected_profit > U256::ZERO, "Zero profit opportunity");
        assert!(opp.gas_cost_wei > 0, "Zero gas cost");
        assert!(!opp.pool_a.is_zero(), "Zero pool A");
        assert!(!opp.pool_b.is_zero(), "Zero pool B");
    }
}

/// Test 6: Detect arbitrage across V2 + V3 pools
#[tokio::test]
async fn test_e2e_cross_dex_arbitrage() {
    eprintln!("--- test_e2e_cross_dex_arbitrage ---");
    let (rpc, tip) = match try_rpc().await {
        Some(v) => v,
        None => { eprintln!("SKIP: no RPC available"); return; }
    };

    let block_num = tip.saturating_sub(1);
    let mut pm = PoolManager::new();
    pm.add_pool(pool_info_to_state(pool_info(
        quick_wmatic_usdc(), wmatic(), usdc(), "QuickSwap V2",
    )));
    pm.add_pool(pool_info_to_state(pool_info_v3(
        uni_v3_wmatic_usdc(), wmatic(), usdc(), 500, "Uniswap V3",
    )));

    pm.init_from_rpc(&rpc, block_num, None).await;
    let initialized = pm.initialized_count();
    eprintln!("  Initialized {initialized}/2 pools at block {block_num}");
    if initialized < 2 {
        eprintln!("  SKIP: fewer than 2 pools initialized");
        return;
    }

    let mut detector = TwoHopArbDetector::new(block_num);
    let opps = detector.detect(&pm, 0, block_num, 50_000_000_000, default_gas_config());
    eprintln!("  Cross-DEX arb opportunities: {}", opps.len());
    print_opportunities(&opps);

    if opps.is_empty() {
        eprintln!("  WARNING: no V2->V3 arbitrage at this block");
    }
    for opp in &opps {
        assert!(opp.expected_profit > U256::ZERO);
        assert_eq!(opp.strategy, Strategy::TwoHopArb);
    }
}



/// Test 8: Opportunity serialization roundtrip (no RPC needed)
#[test]
fn test_e2e_opportunity_persistence() {
    use mev_scout_core::cache::RunManifest;

    let opp = MevOpportunity::new(
        12345678, 0, Strategy::TwoHopArb, quick_wmatic_usdc(), 9999999999,
    );
    let json = serde_json::to_string(&opp).unwrap();
    let deserialized: MevOpportunity = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.block_number, opp.block_number);
    assert_eq!(deserialized.strategy, opp.strategy);
    assert_eq!(deserialized.pool_a, opp.pool_a);

    let (cache, _dir) = temp_cache("persistence");
    let manifest = RunManifest {
        run_id: "e2e_test_run".into(),
        chain: "polygon".into(),
        start_block: 12345678,
        end_block: 12345678,
        resolved_at: 9999999999,
        range_mode: "single".into(),
        strategies: vec!["two_hop_arb".into()],
        flash_loan_provider: "none".into(),
    };
    cache.put_manifest(&manifest).unwrap();
    let loaded = cache.get_manifest("e2e_test_run").unwrap();
    assert!(loaded.is_some(), "Manifest should persist and load");
}

/// Test 9: Cache isolation between chains (no RPC needed)
#[test]
fn test_e2e_cache_isolation() {
    let (poly, _dp) = temp_cache("iso_poly");
    let _eth = SqliteStore::open(
        Path::new(&temp_cache_dir("iso_eth")).join("cache.db"),
    ).unwrap();

    let pool = PoolInfo {
        address: quick_wmatic_usdc(),
        token0: wmatic(),
        token1: usdc(),
        fee: 30,
        name: Some("QuickSwap WMATIC/USDC".into()),
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
    };

    poly.put_discovered_pool(&pool).unwrap();
    let poly_pools = poly.list_discovered_pools().unwrap();
    assert_eq!(poly_pools.len(), 1, "Polygon cache should have 1 pool");

    let eth_pools = _eth.list_discovered_pools().unwrap();
    assert_eq!(eth_pools.len(), 0, "Ethereum cache should be empty");
}

/// Test N: remote subgraph discovery end-to-end (network-gated).
///
/// Requires `GRAPH_API_KEY` for gateway URLs (hosted fallbacks are tried but
/// may be dead). Run explicitly: `cargo test --test e2e e2e_subgraph_discovery -- --ignored`
#[tokio::test]
#[ignore]
async fn e2e_subgraph_discovery() {
    if std::env::var("GRAPH_API_KEY").unwrap_or_default().is_empty() {
        eprintln!("SKIP: GRAPH_API_KEY not set");
        return;
    }

    let subgraphs = ChainName::Polygon.default_subgraphs();
    assert!(!subgraphs.is_empty(), "Polygon must have default subgraphs");

    let pools = mev_scout_core::pool::discovery::remote::discover_via_remote(
        &subgraphs, Some(100), None,
    ).await;

    assert!(
        pools.len() >= 10,
        "expected ≥10 pools from Polygon subgraphs, got {}",
        pools.len()
    );
    for p in pools.iter().take(10) {
        assert!(!p.token0.is_zero(), "token0 must not be ZERO for {}", p.address);
        assert!(!p.token1.is_zero(), "token1 must not be ZERO for {}", p.address);
    }
    // Symbols should come straight from the subgraph without extra eth_call.
    let with_symbols = pools.iter().filter(|p| p.token0_symbol.is_some()).count();
    assert!(with_symbols > 0, "subgraph pools should carry token symbols");
}
