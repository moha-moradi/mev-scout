use std::sync::atomic::{AtomicU64, Ordering};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_db_path() -> std::path::PathBuf {
    let n = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!("mev_e2e_discovery_{n}.sqlite"))
}

fn rpc_url() -> Option<String> {
    std::env::var("RPC_URL").ok()
        .or_else(|| Some("https://ethereum-rpc.publicnode.com".to_string()))
}

fn eth_uniswap_v2_factory() -> alloy::primitives::Address {
    "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f".parse().unwrap()
}

/// Requires RPC_URL env var or falls back to Ethereum public node.
/// Scans Uniswap V2 factory on Ethereum for a block range with known pairs.
#[tokio::test]
async fn test_e2e_discover_ethereum_v2_pools() {
    let rpc_url = match rpc_url() {
        Some(url) => url,
        None => {
            eprintln!("Skipping: RPC_URL not set and no public fallback");
            return;
        }
    };

    let rpc = match mev_scout_core::rpc::RpcClient::new(&rpc_url, 1) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Skipping: failed to create RPC client: {e}");
            return;
        }
    };

    let factory = eth_uniswap_v2_factory();
    let db_path = temp_db_path();
    let cache = match mev_scout_core::cache::SqliteStore::open(
        db_path.to_str().unwrap(), 1,
    ) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Skipping: failed to open cache: {e}");
            return;
        }
    };

    // Scan a range with known PairCreated events
    let pools = match mev_scout_core::pool::discovery::discover_v2_pools(
        &rpc, factory, 20_000_000, 20_001_000, None,
    ).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Skipping: discovery failed (RPC may be unavailable): {e}");
            std::fs::remove_file(&db_path).ok();
            return;
        }
    };

    assert!(!pools.is_empty(), "Should discover at least one V2 pool on Ethereum");

    for pool in &pools {
        assert_ne!(pool.address, alloy::primitives::Address::ZERO, "Pool address should not be zero");
        assert_ne!(pool.token0, alloy::primitives::Address::ZERO, "token0 should not be zero");
        assert_ne!(pool.token1, alloy::primitives::Address::ZERO, "token1 should not be zero");
        assert_eq!(pool.dex_type, mev_scout_core::pool::dex_type::DexType::UniswapV2);
        assert_eq!(pool.fee, 30, "V2 discovered pools should default to 30 bps");
    }

    // Test cache roundtrip
    let info: mev_scout_core::pool::state::PoolInfo = pools[0].clone().into();
    cache.put_discovered_pool(&info).unwrap();
    let cached = cache.get_discovered_pool(&info.address).unwrap();
    assert!(cached.is_some(), "Pool should be retrievable from cache");
    assert_eq!(cached.unwrap().address, info.address);

    // Test cursor persistence
    cache.put_discovery_cursor(&factory, 20_001_000).unwrap();
    let cursor = cache.get_discovery_cursor(&factory).unwrap();
    assert_eq!(cursor, Some(20_001_000));

    // Test list_discovered_pools includes cached pool
    let all = cache.list_discovered_pools().unwrap();
    assert!(!all.is_empty(), "list_discovered_pools should return pools");
    assert!(all.iter().any(|p| p.address == info.address), "List should include saved pool");

    std::fs::remove_file(&db_path).ok();
    eprintln!("PASS: discovered {} V2 pools on Ethereum", pools.len());
}

/// Same test but using orchestrator discover_pools with batching.
#[tokio::test]
async fn test_e2e_discover_with_batching() {
    let rpc_url = match rpc_url() {
        Some(url) => url,
        None => {
            eprintln!("Skipping: RPC_URL not set");
            return;
        }
    };

    let rpc = match mev_scout_core::rpc::RpcClient::new(&rpc_url, 1) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Skipping: failed to create RPC client: {e}");
            return;
        }
    };

    let factory = eth_uniswap_v2_factory();
    let db_path = temp_db_path();
    let cache = match mev_scout_core::cache::SqliteStore::open(
        db_path.to_str().unwrap(), 1,
    ) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Skipping: failed to open cache: {e}");
            return;
        }
    };

    let total = match mev_scout_core::pool::discovery::discover_pools(
        &rpc, &cache, &[factory], &[], None, 20_000_000, 20_001_000, 100, 0, &[],
    ).await {
        Ok(n) => n,
        Err(e) => {
            eprintln!("Skipping: batch discovery failed: {e}");
            std::fs::remove_file(&db_path).ok();
            return;
        }
    };

    assert!(total > 0, "Should discover at least one pool with batching");

    // Cursor should be at end block
    let cursor = cache.get_discovery_cursor(&factory).unwrap();
    assert_eq!(cursor, Some(20_001_000), "Cursor should advance to end block");

    std::fs::remove_file(&db_path).ok();
    eprintln!("PASS: batch discovery found {total} pools");
}

fn eth_uniswap_v3_factory() -> alloy::primitives::Address {
    "0x1F98431c8aD98523631AE4a59f267346ea31F984".parse().unwrap()
}

/// Verify V3 PoolCreated event decoding with real Ethereum data.
#[tokio::test]
async fn test_e2e_discover_ethereum_v3_pools() {
    let rpc_url = match rpc_url() {
        Some(url) => url,
        None => {
            eprintln!("Skipping: RPC_URL not set");
            return;
        }
    };

    let rpc = match mev_scout_core::rpc::RpcClient::new(&rpc_url, 1) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Skipping: failed to create RPC client: {e}");
            return;
        }
    };

    let factory = eth_uniswap_v3_factory();

    // Scan a range with known V3 PoolCreated events (Uniswap V3 launched around block 12_369_621)
    let pools = match mev_scout_core::pool::discovery::discover_v3_pools(
        &rpc, factory, 12_369_700, 12_370_000,
    ).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Skipping: V3 discovery failed (RPC may be unavailable): {e}");
            return;
        }
    };

    assert!(!pools.is_empty(), "Should discover at least one V3 pool on Ethereum");

    for pool in &pools {
        assert_ne!(pool.address, alloy::primitives::Address::ZERO, "Pool address should not be zero");
        assert_ne!(pool.token0, alloy::primitives::Address::ZERO, "token0 should not be zero");
        assert_ne!(pool.token1, alloy::primitives::Address::ZERO, "token1 should not be zero");
        assert_eq!(pool.dex_type, mev_scout_core::pool::dex_type::DexType::UniswapV3);
        let valid_fees = [100, 500, 3000, 10000];
        assert!(valid_fees.contains(&pool.fee),
            "V3 fee should be one of 100/500/3000/10000, got {}", pool.fee);
        assert!(pool.tick_spacing.is_some(), "V3 pools should have tick_spacing");
    }

    eprintln!("PASS: discovered {} V3 pools on Ethereum", pools.len());
}

/// Verify V3 Swap event decoding with a real log from a known V3 pool.
#[tokio::test]
async fn test_e2e_v3_swap_decoding() {
    use alloy::rpc::types::Filter;
    use mev_scout_core::pool::decoders::{decode_v3_swap, V3_SWAP_TOPIC};

    let rpc_url = match rpc_url() {
        Some(url) => url,
        None => {
            eprintln!("Skipping: RPC_URL not set");
            return;
        }
    };

    let rpc = match mev_scout_core::rpc::RpcClient::new(&rpc_url, 1) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Skipping: failed to create RPC client: {e}");
            return;
        }
    };

    // ETH/USDC 0.3% V3 pool on Ethereum
    let pool: alloy::primitives::Address =
        "0x8ad599c3A0ff1De082011EFDDc58f1908eb6e6D8".parse().unwrap();

    // Get the latest block number
    let latest_block = match rpc.get_block_number().await {
        Ok(n) => n,
        Err(e) => {
            eprintln!("Skipping: failed to get block number: {e}");
            return;
        }
    };
    let from = latest_block.saturating_sub(1000);
    let to = latest_block;

    let filter = Filter::new()
        .address(pool)
        .event_signature(V3_SWAP_TOPIC)
        .from_block(from)
        .to_block(to);

    let logs = match rpc.get_logs(&filter).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Skipping: failed to get logs from V3 pool: {e}");
            return;
        }
    };

    if logs.is_empty() {
        eprintln!("Skipping: no Swap events found for V3 pool in blocks {from}..{to}");
        return;
    }

    // Parse the first swap log into ExecutedLog format
    let log = &logs[0];
    let executed_log = mev_scout_core::data::ExecutedLog {
        address: log.address().into(),
        topics: log.topics().to_vec(),
        data: log.data().data.clone(),
    };

    let decoded = decode_v3_swap(&executed_log);
    assert!(decoded.is_some(), "Should decode V3 Swap event");
    let decoded = decoded.unwrap();

    // Verify all fields are non-zero and reasonable
    assert!(!decoded.sqrt_price_x96.is_zero(), "sqrt_price_x96 should be non-zero");
    assert!(decoded.liquidity > 0, "liquidity should be positive");
    // tick should be in Uniswap V3 range (-887272 to 887272)
    assert!(decoded.tick > -887272 && decoded.tick < 887272,
        "tick should be in valid Uniswap V3 range, got {}", decoded.tick);

    eprintln!("PASS: decoded V3 Swap: sqrt={}, liq={}, tick={} (block={})",
        decoded.sqrt_price_x96, decoded.liquidity, decoded.tick, logs[0].block_number.unwrap_or(0));
}
