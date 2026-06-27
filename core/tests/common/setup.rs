use std::path::Path;

use alloy::primitives::{address, Address, B256, Bytes, U256};
use mev_scout_core::cache::SqliteStore;
use mev_scout_core::data::{BlockData, ReceiptData, TxData};
use mev_scout_core::mev::detectors::multi_hop::MultiHopArbDetector;
use mev_scout_core::mev::detectors::two_hop::TwoHopArbDetector;
use mev_scout_core::pipeline::BacktestRunner;
use mev_scout_core::pool::dex_type::DexType;
use mev_scout_core::pool::state::{
    BalancerPoolVariant, PoolInfo, PoolManager, PoolState, UniswapV2PoolState, UniswapV3PoolState,
};
use mev_scout_core::replay::BlockReplayer;
use mev_scout_core::rpc::RpcClient;
use mev_scout_core::types::GasConfig;

pub fn rpc_url() -> Option<String> {
    std::env::var("RPC_URL").ok()
}

pub fn pool_info_to_state(info: PoolInfo) -> PoolState {
    match info.dex_type {
        DexType::UniswapV2 => PoolState::UniswapV2(UniswapV2PoolState {
            info,
            reserve0: 0,
            reserve1: 0,
        }),
        DexType::UniswapV3 => PoolState::UniswapV3(UniswapV3PoolState::new(info)),
        DexType::Curve => PoolState::Curve(mev_scout_core::pool::state::CurvePoolState {
            info,
            balances: vec![],
            token_index: std::collections::HashMap::new(),
            a_coeff: 100,
            pool_variant: mev_scout_core::pool::state::CurvePoolVariant::Plain,
            gamma: None,
            price_scale: vec![],
            base_pool: None,
        }),
        DexType::Balancer => PoolState::Balancer(mev_scout_core::pool::state::BalancerPoolState {
            info,
            balances: vec![],
            token_index: std::collections::HashMap::new(),
            pool_id: None,
            weights: vec![],
            pool_variant: BalancerPoolVariant::Weighted,
            amplification: None,
            bpt_index: None,
            scaling_factors: vec![],
        }),
    }
}

pub fn wmatic() -> Address {
    address!("0d500b1d8e8ef31e21c99d1db9a6444d3adf1270")
}

pub fn usdc() -> Address {
    address!("2791bca1f2de4661ed88a30c99a7a9449aa84174")
}

pub fn usdt() -> Address {
    address!("c2132d05d31c914a87c6611c10748aeb04b58e8f")
}

pub fn matic_usdc_pool() -> Address {
    address!("6e7a5fafcec6bb1e78bae2a1f0b612012bf14827")
}

pub fn matic_usdt_pool() -> Address {
    address!("604029b0c1a79eebfb31f7c5316434484f3a4b55")
}

pub fn pool_info(addr: Address, token0: Address, token1: Address, name: &str) -> PoolInfo {
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
    }
}

pub fn default_gas_config() -> GasConfig {
    GasConfig::default()
}

pub fn two_hop_detect(pm: &PoolManager, block: u64, ts: u64) -> Vec<mev_scout_core::types::MevOpportunity> {
    let mut d = TwoHopArbDetector::new(block);
    d.detect(pm, 0, ts, 50_000_000_000, default_gas_config())
}

pub fn multi_hop_detect(pm: &PoolManager, block: u64, ts: u64) -> Vec<mev_scout_core::types::MevOpportunity> {
    let mut d = MultiHopArbDetector::new(block);
    d.detect(pm, 0, ts, 50_000_000_000, GasConfig::default())
}

pub fn make_pool(addr: Address, token0: Address, token1: Address, r0: u128, r1: u128) -> PoolState {
    PoolState::UniswapV2(UniswapV2PoolState {
        info: PoolInfo {
            address: addr,
            token0,
            token1,
            fee: 30,
            name: None,
            dex_type: DexType::UniswapV2,
            tick_spacing: None,
            creation_block: 0,
            pool_id: None,
            factory: None,
        },
        reserve0: r0,
        reserve1: r1,
    })
}

pub fn temp_test_dir(name: &str) -> String {
    let dir = std::env::temp_dir().join(format!("mev_scout_int_{name}_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    dir.to_str().unwrap().to_string()
}

pub fn prep_synthetic_cache(dir: &str, block_num: u64, tx_count: usize) -> SqliteStore {
    let db_path = Path::new(dir).join("cache.db");
    let cache = SqliteStore::open(&db_path, 1).unwrap();

    let block = BlockData {
        number: block_num,
        hash: B256::ZERO,
        timestamp: 12345678,
        base_fee_per_gas: Some(50_000_000_000),
        gas_limit: 30_000_000,
        gas_used: 100_000 * tx_count as u64,
        coinbase: Address::ZERO,
    };
    cache.put_block(block_num, &block).unwrap();

    let mut txs = Vec::new();
    let mut receipts = Vec::new();
    for i in 0..tx_count {
        let mut hash_bytes = [0u8; 32];
        hash_bytes[0..8].copy_from_slice(&block_num.to_be_bytes());
        hash_bytes[8..16].copy_from_slice(&(i as u64).to_be_bytes());
        let tx_hash = B256::from(hash_bytes);

        txs.push(TxData {
            hash: tx_hash,
            index: i as u64,
            from: Address::ZERO,
            to: Some(Address::repeat_byte(0x42)),
            input: Bytes::new(),
            value: U256::ZERO,
            gas_limit: 100_000,
            max_fee_per_gas: 50_000_000_000,
            max_priority_fee_per_gas: None,
            nonce: i as u64,
            access_list: vec![],
        });
        receipts.push(ReceiptData {
            tx_hash,
            tx_index: i as u64,
            status: true,
            gas_used: 100_000,
            cumulative_gas_used: 100_000 * (i as u64 + 1),
            logs: vec![],
            contract_address: None,
        });
    }
    cache.put_txs(block_num, &txs).unwrap();
    cache.put_receipts(block_num, &receipts).unwrap();

    cache
}

pub fn synthetic_arb_pools() -> PoolManager {
    let mut pm = PoolManager::new();
    pm.add_pool(make_pool(
        address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        usdc(),
        wmatic(),
        1_000_000_000_000,
        2_000_000_000_000_000_000u128,
    ));
    pm.add_pool(make_pool(
        address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        usdt(),
        wmatic(),
        1_000_000_000_000,
        500_000_000_000_000_000u128,
    ));
    pm.with_wrapped_native(wmatic())
}

pub fn make_synthetic_runner(dir: &str, block_num: u64, gas_config: GasConfig) -> BacktestRunner {
    let cache = prep_synthetic_cache(dir, block_num, 2);
    let handle = tokio::runtime::Handle::current();
    let rpc = RpcClient::new("http://0.0.0.0:1", 1).unwrap();
    let replayer = BlockReplayer::new(handle, cache, rpc, 1);

    let pm = synthetic_arb_pools();

    BacktestRunner::new(replayer, pm, gas_config)
}
