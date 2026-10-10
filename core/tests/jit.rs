//! JIT (just-in-time liquidity) detector integration tests.
//!
//! Holds the JIT cases that used to live in `sandwich.rs`; the remaining
//! sandwich coverage was removed along with the live detector.
//!
//! `test_real_v3_mint_swap_burn_detection` hits a live RPC endpoint and is
//! gated like the CLI E2E suite: it needs `MEV_SCOUT_E2E=1` and `RPC_URL`,
//! otherwise it skips gracefully (no fallback to the repo `mev-scout.toml`).
use alloy::primitives::{address, Address, Bytes, B256};
use mev_scout_core::data::ExecutedLog;
use mev_scout_core::dex_type::DexType;
use mev_scout_core::mev::detectors::jit::JitDetector;
use mev_scout_core::mev::detectors::DetectCtx;
use mev_scout_core::pool::decoders::{V3_BURN_TOPIC, V3_MINT_TOPIC, V3_SWAP_TOPIC};
use mev_scout_core::pool::state::{PoolInfo, PoolManager, PoolState, ScanScope, UniswapV3PoolState};
use mev_scout_core::types::Strategy;

mod common;
use common::*;

#[test]
fn test_jit_detection_synthetic() {
    let pool = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

    let mut pm = PoolManager::new();
    pm.add_pool(PoolState::UniswapV3(UniswapV3PoolState::new(PoolInfo {
        address: pool,
        token0: address!("0000000000000000000000000000000000000001"),
        token1: address!("0000000000000000000000000000000000000002"),
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
    })));
    let gas_cfg = default_gas_config();
    let mut detector = JitDetector::new(42);
    let timestamp = 12345u64;
    let scope = ScanScope::Full;
    let ctx = || DetectCtx::new(&pm, 0, timestamp, 0, gas_cfg, &scope);

    /// A 32-byte topic holding an int24 tick: left-padded, sign-extended when
    /// negative, exactly as a canonical Uniswap V3 pool emits it.
    fn tick_topic(v: i32) -> B256 {
        let fill = if v < 0 { 0xffu8 } else { 0x00u8 };
        let mut w = [fill; 32];
        w[28..32].copy_from_slice(&v.to_be_bytes());
        B256::from(w)
    }

    /// Canonical V3 `Mint` per v3-core `IUniswapV3PoolEvents`:
    /// topics [sig, owner, tickLower, tickUpper], data [sender, amount, amount0, amount1].
    fn v3_mint_log(pool: Address, lower: i32, upper: i32, amount: u128) -> ExecutedLog {
        let mut data = Vec::new();
        data.extend_from_slice(&[0u8; 32]); // sender
        let mut w = [0u8; 32];
        w[16..32].copy_from_slice(&amount.to_be_bytes());
        data.extend_from_slice(&w); // amount
        data.extend_from_slice(&[0u8; 32]); // amount0
        data.extend_from_slice(&[0u8; 32]); // amount1
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

    /// Canonical V3 `Burn`: same topics, but data has no leading `sender` word.
    fn v3_burn_log(pool: Address, lower: i32, upper: i32, amount: u128) -> ExecutedLog {
        let mut data = Vec::new();
        let mut w = [0u8; 32];
        w[16..32].copy_from_slice(&amount.to_be_bytes());
        data.extend_from_slice(&w); // amount
        data.extend_from_slice(&[0u8; 32]); // amount0
        data.extend_from_slice(&[0u8; 32]); // amount1
        ExecutedLog {
            address: pool,
            topics: vec![
                V3_BURN_TOPIC,
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

    // Tx 0: deploy liquidity
    detector.process_tx(0, &[v3_mint_log(pool, -1000, 1000, 1_000_000)], None, &pm);
    assert!(detector.detect(ctx()).is_empty());

    // Tx 1: swap against it
    detector.process_tx(1, &[v3_swap_log(pool)], None, &pm);
    let mut opps = detector.detect(ctx());
    assert!(!opps.is_empty(), "Mint+Swap should trigger JIT detection");
    assert_eq!(opps[0].strategy, Strategy::Jit);
    assert_eq!(opps[0].pool_a, pool);
    assert_eq!(opps[0].tick_lower, Some(-1000));
    assert_eq!(opps[0].tick_upper, Some(1000));
    assert_eq!(opps[0].liquidity_amount, Some(1_000_000));

    // Tx 2: burn position
    detector.process_tx(2, &[v3_burn_log(pool, -1000, 1000, 1_000_000)], None, &pm);
    opps = detector.detect(ctx());
    assert_eq!(opps.len(), 1, "Burn should trigger full JIT emission");

    // No duplicate
    assert!(detector.detect(ctx()).is_empty());
}

#[tokio::test]
async fn test_real_v3_mint_swap_burn_detection() {
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
        Ok(n) => n.saturating_sub(100),
        Err(e) => {
            eprintln!("Skipping: failed to get block number: {e}");
            return;
        }
    };

    // Real V3 pool: Uniswap V3 WMATIC/USDC 0.05%
    let pool_info = PoolInfo {
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
        dex_name: None,
        token0_symbol: None,
        token1_symbol: None,
        tvl_usd: None,
        volume_usd_24h: None,
        volume_usd_30d: None,
    };
    let mut pm = PoolManager::new();
    pm.add_pool(pool_info_to_state(pool_info.clone()));
    pm.init_from_rpc(&rpc, block_num, None).await;

    let initialized = pm.initialized_count();
    eprintln!(
        "V3 pool {} initialized={} at block {}",
        pool_info.address, initialized, block_num
    );

    if initialized == 0 {
        eprintln!("Skipping: V3 pool not initialized");
        return;
    }

    // We can't easily force a V3 Mint/Swap/Burn sequence from a test,
    // but we can verify the JitDetector compiles and processes empty data.
    let gas_cfg = default_gas_config();
    let mut detector = JitDetector::new(block_num);
    // Process empty data (no logs from this pool in this test block)
    detector.process_tx(0, &[], None, &pm);
    let scope = ScanScope::Full;
    let opps = detector.detect(DetectCtx::new(
        &pm,
        0,
        block_num,
        0,
        gas_cfg,
        &scope,
    ));
    eprintln!(
        "JIT detection on real V3 pool: {} opportunities (expected 0 without events)",
        opps.len()
    );

    // This test primarily validates that JitDetector works with real PoolManager state
    // even though we can't produce real V3 events without replaying a block.
    assert!(opps.is_empty(), "No JIT without any events");
}
