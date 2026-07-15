use alloy::primitives::{address, b256, Address, B256, Bytes};
use mev_scout_core::data::ExecutedLog;
use mev_scout_core::mev::detectors::jit::JitDetector;
use mev_scout_core::mev::detectors::jit_arb::JitArbDetector;
use mev_scout_core::mev::detectors::sandwich::SandwichDetector;
use mev_scout_core::pool::decoders::{V3_BURN_TOPIC, V3_MINT_TOPIC, V3_SWAP_TOPIC};
use mev_scout_core::pool::dex_type::DexType;
use mev_scout_core::pool::state::{PoolInfo, PoolManager, PoolState, UniswapV2PoolState, UniswapV3PoolState};
use mev_scout_core::types::Strategy;

mod common;
use common::*;

#[test]
fn test_sandwich_detection_synthetic() {
    let pool = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let alice = address!("1111111111111111111111111111111111111111");
    let bob = address!("2222222222222222222222222222222222222222");

    let v2_swap_topic: B256 =
        b256!("d78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822");

    let v2_swap_log =
        |pool: Address, amt0_in: u128, amt1_in: u128, amt0_out: u128, amt1_out: u128| -> ExecutedLog {
            let mut data = Vec::with_capacity(128);
            let mut buf = vec![0u8; 16];
            data.extend_from_slice(&buf);
            data.extend_from_slice(&amt0_in.to_be_bytes());
            buf = vec![0u8; 16];
            data.extend_from_slice(&buf);
            data.extend_from_slice(&amt1_in.to_be_bytes());
            buf = vec![0u8; 16];
            data.extend_from_slice(&buf);
            data.extend_from_slice(&amt0_out.to_be_bytes());
            buf = vec![0u8; 16];
            data.extend_from_slice(&buf);
            data.extend_from_slice(&amt1_out.to_be_bytes());
            ExecutedLog {
                address: pool,
                topics: vec![v2_swap_topic, B256::ZERO, B256::ZERO],
                data: data.into(),
            }
        };

    let usdc = address!("2791bca1f2de4661ed88a30c99a7a9449aa84174");
    let wmatic = address!("0d500b1d8e8ef31e21c99d1db9a6444d3adf1270");

    let mut pm = PoolManager::new();
    pm.add_pool(PoolState::UniswapV2(UniswapV2PoolState {
        info: PoolInfo {
            address: pool,
            token0: usdc,
            token1: wmatic,
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
        },
        reserve0: 1_000_000,
        reserve1: 1_000_000,
    }));

    let mut detector = SandwichDetector::new(42);
    let timestamp = 12345u64;
    let gas_cfg = default_gas_config();

    // Tx 0: alice frontruns — buys WMATIC (token0→token1)
    detector.process_tx(0, &[v2_swap_log(pool, 100, 0, 0, 90)], Some(alice), &pm);
    assert!(detector.detect(timestamp, &pm, 0, &gas_cfg).is_empty());

    // Tx 1: bob (victim) — buys WMATIC at worse price
    detector.process_tx(1, &[v2_swap_log(pool, 200, 0, 0, 170)], Some(bob), &pm);
    assert!(detector.detect(timestamp, &pm, 0, &gas_cfg).is_empty());

    // Tx 2: alice backruns — sells WMATIC (token1→token0)
    detector.process_tx(2, &[v2_swap_log(pool, 0, 85, 105, 0)], Some(alice), &pm);
    let opps = detector.detect(timestamp, &pm, 0, &gas_cfg);
    assert!(!opps.is_empty(), "Should detect sandwich");
    assert_eq!(opps[0].strategy, Strategy::Sandwich);
    assert_eq!(opps[0].pool_a, pool);
    assert_eq!(opps[0].victim_tx_index, Some(1));
    assert_eq!(opps[0].backrun_tx_index, Some(2));
    assert_eq!(opps[0].token_in, usdc);
    assert_eq!(opps[0].token_out, wmatic);

    // No duplicate
    assert!(detector.detect(timestamp, &pm, 0, &gas_cfg).is_empty());
}

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
    })));
    let gas_cfg = default_gas_config();
    let mut detector = JitDetector::new(42);
    let timestamp = 12345u64;

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

    fn v3_burn_log(pool: Address, lower: i32, upper: i32, amount: u128) -> ExecutedLog {
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
            topics: vec![V3_BURN_TOPIC, B256::ZERO, B256::ZERO],
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
    assert!(detector.detect(timestamp, 0, &gas_cfg, &pm).is_empty());

    // Tx 1: swap against it
    detector.process_tx(1, &[v3_swap_log(pool)], None, &pm);
    let mut opps = detector.detect(timestamp, 0, &gas_cfg, &pm);
    assert!(!opps.is_empty(), "Mint+Swap should trigger JIT detection");
    assert_eq!(opps[0].strategy, Strategy::Jit);
    assert_eq!(opps[0].pool_a, pool);
    assert_eq!(opps[0].tick_lower, Some(-1000));
    assert_eq!(opps[0].tick_upper, Some(1000));
    assert_eq!(opps[0].liquidity_amount, Some(1_000_000));

    // Tx 2: burn position
    detector.process_tx(2, &[v3_burn_log(pool, -1000, 1000, 1_000_000)], None, &pm);
    opps = detector.detect(timestamp, 0, &gas_cfg, &pm);
    assert_eq!(opps.len(), 1, "Burn should trigger full JIT emission");

    // No duplicate
    assert!(detector.detect(timestamp, 0, &gas_cfg, &pm).is_empty());
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
    };
    let mut pm = PoolManager::new();
    pm.add_pool(pool_info_to_state(pool_info.clone()));
    pm.init_from_rpc(&rpc, block_num).await;

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
    let opps = detector.detect(block_num, 0, &gas_cfg, &pm);
    eprintln!(
        "JIT detection on real V3 pool: {} opportunities (expected 0 without events)",
        opps.len()
    );

    // This test primarily validates that JitDetector works with real PoolManager state
    // even though we can't produce real V3 events without replaying a block.
    assert!(opps.is_empty(), "No JIT without any events");
}

#[test]
fn test_jit_arb_detection_synthetic() {
    use alloy::primitives::Address;

    let pool_p = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let pool_q = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    let sender = address!("1111111111111111111111111111111111111111");
    let wmatic = address!("0d500b1d8e8ef31e21c99d1db9a6444d3adf1270");
    let usdc = address!("2791bca1f2de4661ed88a30c99a7a9449aa84174");

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
            token0: wmatic,
            token1: usdc,
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
        },
        reserve0: 1_000_000,
        reserve1: 1_000_000,
    }));
    pm.add_pool(PoolState::UniswapV2(UniswapV2PoolState {
        info: PoolInfo {
            address: pool_q,
            token0: usdc,
            token1: address!("c2132d05d31c914a87c6611c10748aeb04b58e8f"),
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
        },
        reserve0: 1_000_000,
        reserve1: 1_000_000,
    }));

    let gas_cfg = default_gas_config();
    let mut detector = JitArbDetector::new(42);
    detector.process_tx(
        0,
        &[
            v3_mint_log(pool_p, -100, 100, 500_000),
            v3_swap_log(pool_p),
            v3_swap_log(pool_q),
        ],
        Some(sender),
        &pm,
    );

    let opps = detector.detect(12345, &pm, 0, &gas_cfg);
    assert_eq!(opps.len(), 1, "Should detect JitArb");
    assert_eq!(opps[0].strategy, Strategy::JitArb);
    assert_eq!(opps[0].pool_a, pool_p);
    assert_eq!(opps[0].pool_b, pool_q);
    assert_eq!(opps[0].liquidity_amount, Some(500_000));
    assert_eq!(opps[0].tick_lower, Some(-100));
    assert_eq!(opps[0].tick_upper, Some(100));
}
