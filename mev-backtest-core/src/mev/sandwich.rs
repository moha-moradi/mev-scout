use std::collections::HashMap;
use alloy::primitives::{b256, Address, B256, U256};
use crate::data::ExecutedLog;
use crate::mev::opportunity::MevOpportunity;
use crate::pool::state::PoolManager;
use crate::types::Strategy;

/// Uniswap V2 Swap event topic
const V2_SWAP_TOPIC: B256 =
    b256!("d78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SwapDirection {
    Token0ForToken1,
    Token1ForToken0,
}

#[derive(Debug, Clone)]
struct SwapRecord {
    tx_index: usize,
    sender: Address,
    pool: Address,
    direction: SwapDirection,
    amount_in: u128,
    #[allow(dead_code)]
    amount_out: u128,
}

pub struct SandwichDetector {
    swap_records: Vec<SwapRecord>,
    emitted: Vec<(Address, usize)>,
    block_number: u64,
}

impl SandwichDetector {
    pub fn new(block_number: u64) -> Self {
        SandwichDetector {
            swap_records: Vec::new(),
            emitted: Vec::new(),
            block_number,
        }
    }

    pub fn process_tx(
        &mut self,
        tx_index: usize,
        logs: &[ExecutedLog],
        sender: Option<Address>,
    ) {
        let Some(sender) = sender else { return };

        for log in logs {
            if log.topics.is_empty() || log.topics[0] != V2_SWAP_TOPIC {
                continue;
            }
            if log.data.len() < 128 {
                continue;
            }

            let amt0_in = u128_from_be_bytes_32(&log.data[..32]);
            let amt1_in = u128_from_be_bytes_32(&log.data[32..64]);
            let amt0_out = u128_from_be_bytes_32(&log.data[64..96]);
            let amt1_out = u128_from_be_bytes_32(&log.data[96..128]);

            let (direction, amount_in, amount_out) =
                if amt0_in > 0 && amt1_out > 0 {
                    (SwapDirection::Token0ForToken1, amt0_in, amt1_out)
                } else if amt1_in > 0 && amt0_out > 0 {
                    (SwapDirection::Token1ForToken0, amt1_in, amt0_out)
                } else {
                    continue;
                };

            self.swap_records.push(SwapRecord {
                tx_index,
                sender,
                pool: log.address,
                direction,
                amount_in,
                amount_out,
            });
        }
    }

    pub fn detect(
        &mut self,
        timestamp: u64,
        pool_manager: &PoolManager,
    ) -> Vec<MevOpportunity> {
        let mut opportunities = Vec::new();

        let mut pool_records: HashMap<Address, Vec<&SwapRecord>> = HashMap::new();
        for record in &self.swap_records {
            pool_records.entry(record.pool).or_default().push(record);
        }

        for records in pool_records.values() {
            for window in records.windows(3) {
                let front = &window[0];
                let victim = &window[1];
                let back = &window[2];

                let dedup_key = (front.pool, front.tx_index);
                if self.emitted.contains(&dedup_key) {
                    continue;
                }

                if front.sender != back.sender {
                    continue;
                }
                if front.direction != victim.direction {
                    continue;
                }
                if front.direction == back.direction {
                    continue;
                }

                self.emitted.push(dedup_key);

                let pool_info = pool_manager.get(&front.pool)
                    .map(|p| p.info());
                let (token_in, token_out) = match pool_info {
                    Some(info) => match front.direction {
                        SwapDirection::Token0ForToken1 => (info.token0, info.token1),
                        SwapDirection::Token1ForToken0 => (info.token1, info.token0),
                    },
                    None => (Address::ZERO, Address::ZERO),
                };

                opportunities.push(MevOpportunity {
                    block_number: self.block_number,
                    tx_index: front.tx_index,
                    strategy: Strategy::Sandwich,
                    pool_a: front.pool,
                    pool_b: Address::ZERO,
                    token_in,
                    token_out,
                    input_amount: U256::from(front.amount_in),
                    expected_profit: U256::ZERO,
                    gas_cost_wei: 0,
                    timestamp,
                    path: None,
                    tick_lower: None,
                    tick_upper: None,
                    liquidity_amount: None,
                    victim_tx_index: Some(victim.tx_index),
                    backrun_tx_index: Some(back.tx_index),
                });
            }
        }

        opportunities
    }
}

fn u128_from_be_bytes_32(bytes: &[u8]) -> u128 {
    let start = bytes.len().saturating_sub(16);
    let mut buf = [0u8; 16];
    buf.copy_from_slice(&bytes[start..start + 16]);
    u128::from_be_bytes(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, B256};
    use crate::data::ExecutedLog;
    use crate::pool::state::{PoolInfo, PoolState, UniswapV2PoolState};
    use crate::pool::dex_type::DexType;

    fn encode_u256(val: u128) -> Vec<u8> {
        let mut buf = vec![0u8; 16];
        buf.extend_from_slice(&val.to_be_bytes());
        buf
    }

    fn v2_swap_log(pool: Address, amt0_in: u128, amt1_in: u128, amt0_out: u128, amt1_out: u128) -> ExecutedLog {
        let mut data = Vec::with_capacity(128);
        data.extend_from_slice(&encode_u256(amt0_in));
        data.extend_from_slice(&encode_u256(amt1_in));
        data.extend_from_slice(&encode_u256(amt0_out));
        data.extend_from_slice(&encode_u256(amt1_out));
        ExecutedLog {
            address: pool,
            topics: vec![V2_SWAP_TOPIC, B256::ZERO, B256::ZERO],
            data: data.into(),
        }
    }

    fn pool_a() -> Address { address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa") }
    fn pool_b() -> Address { address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb") }
    fn alice() -> Address { address!("1111111111111111111111111111111111111111") }
    fn bob() -> Address { address!("2222222222222222222222222222222222222222") }

    fn make_pm_with_pool(pool_addr: Address, t0: Address, t1: Address) -> PoolManager {
        let mut pm = PoolManager::new();
        pm.add_pool(PoolState::UniswapV2(UniswapV2PoolState {
            info: PoolInfo {
                address: pool_addr,
                token0: t0,
                token1: t1,
                fee: 30,
                name: None,
                dex_type: DexType::UniswapV2,
                tick_spacing: None,
            },
            reserve0: 1_000_000,
            reserve1: 1_000_000,
        }));
        pm
    }

    #[test]
    fn test_empty_detector_returns_nothing() {
        let mut detector = SandwichDetector::new(1);
        let pm = PoolManager::new();
        let opps = detector.detect(100, &pm);
        assert!(opps.is_empty());
    }

    #[test]
    fn test_sandwich_detected() {
        let mut detector = SandwichDetector::new(1);
        let pm = make_pm_with_pool(pool_a(), address!("cccccccccccccccccccccccccccccccccccccccc"), address!("dddddddddddddddddddddddddddddddddddddddd"));

        detector.process_tx(0, &[v2_swap_log(pool_a(), 100, 0, 0, 90)], Some(alice()));
        assert!(detector.detect(100, &pm).is_empty());

        detector.process_tx(1, &[v2_swap_log(pool_a(), 200, 0, 0, 170)], Some(bob()));
        assert!(detector.detect(100, &pm).is_empty());

        detector.process_tx(2, &[v2_swap_log(pool_a(), 0, 85, 105, 0)], Some(alice()));
        let opps = detector.detect(100, &pm);
        assert_eq!(opps.len(), 1);

        let opp = &opps[0];
        assert_eq!(opp.strategy, Strategy::Sandwich);
        assert_eq!(opp.pool_a, pool_a());
        assert_eq!(opp.tx_index, 0);
        assert_eq!(opp.victim_tx_index, Some(1));
        assert_eq!(opp.backrun_tx_index, Some(2));
        assert_ne!(opp.token_in, Address::ZERO);
        assert_ne!(opp.token_out, Address::ZERO);
    }

    #[test]
    fn test_different_eoa_no_sandwich() {
        let mut detector = SandwichDetector::new(1);
        let pm = PoolManager::new();

        detector.process_tx(0, &[v2_swap_log(pool_a(), 100, 0, 0, 90)], Some(alice()));
        detector.process_tx(1, &[v2_swap_log(pool_a(), 200, 0, 0, 170)], Some(bob()));
        detector.process_tx(2, &[v2_swap_log(pool_a(), 0, 85, 105, 0)], Some(address!("3333333333333333333333333333333333333333")));

        let opps = detector.detect(100, &pm);
        assert!(opps.is_empty());
    }

    #[test]
    fn test_same_direction_backrun_no_sandwich() {
        let mut detector = SandwichDetector::new(1);
        let pm = PoolManager::new();

        detector.process_tx(0, &[v2_swap_log(pool_a(), 100, 0, 0, 90)], Some(alice()));
        detector.process_tx(1, &[v2_swap_log(pool_a(), 200, 0, 0, 170)], Some(bob()));
        detector.process_tx(2, &[v2_swap_log(pool_a(), 300, 0, 0, 250)], Some(alice()));

        let opps = detector.detect(100, &pm);
        assert!(opps.is_empty());
    }

    #[test]
    fn test_no_duplicate_emission() {
        let mut detector = SandwichDetector::new(1);
        let pm = PoolManager::new();

        detector.process_tx(0, &[v2_swap_log(pool_a(), 100, 0, 0, 90)], Some(alice()));
        detector.process_tx(1, &[v2_swap_log(pool_a(), 200, 0, 0, 170)], Some(bob()));
        detector.process_tx(2, &[v2_swap_log(pool_a(), 0, 85, 105, 0)], Some(alice()));

        let opps = detector.detect(100, &pm);
        assert_eq!(opps.len(), 1);

        let opps2 = detector.detect(100, &pm);
        assert!(opps2.is_empty());
    }

    #[test]
    fn test_multiple_pools_independent() {
        let mut detector = SandwichDetector::new(1);
        let pm = PoolManager::new();

        detector.process_tx(0, &[v2_swap_log(pool_a(), 100, 0, 0, 90)], Some(alice()));
        detector.process_tx(1, &[v2_swap_log(pool_a(), 200, 0, 0, 170)], Some(bob()));
        detector.process_tx(2, &[v2_swap_log(pool_a(), 0, 85, 105, 0)], Some(alice()));

        detector.process_tx(3, &[v2_swap_log(pool_b(), 50, 0, 0, 45)], Some(alice()));
        detector.process_tx(4, &[v2_swap_log(pool_b(), 100, 0, 0, 85)], Some(bob()));

        let opps = detector.detect(100, &pm);
        assert_eq!(opps.len(), 1);
        assert_eq!(opps[0].pool_a, pool_a());
    }

    #[test]
    fn test_single_tx_no_detection() {
        let mut detector = SandwichDetector::new(1);
        let pm = PoolManager::new();
        detector.process_tx(0, &[v2_swap_log(pool_a(), 100, 0, 0, 90)], Some(alice()));
        let opps = detector.detect(100, &pm);
        assert!(opps.is_empty());
    }

    #[test]
    fn test_two_txs_no_detection() {
        let mut detector = SandwichDetector::new(1);
        let pm = PoolManager::new();
        detector.process_tx(0, &[v2_swap_log(pool_a(), 100, 0, 0, 90)], Some(alice()));
        detector.process_tx(1, &[v2_swap_log(pool_a(), 200, 0, 0, 170)], Some(bob()));
        let opps = detector.detect(100, &pm);
        assert!(opps.is_empty());
    }

    #[test]
    fn test_interleaved_pool_swaps_no_false_positive() {
        let mut detector = SandwichDetector::new(1);
        let pm = PoolManager::new();

        detector.process_tx(0, &[v2_swap_log(pool_a(), 100, 0, 0, 90)], Some(alice()));
        detector.process_tx(1, &[v2_swap_log(pool_b(), 50, 0, 0, 45)], Some(bob()));
        detector.process_tx(2, &[v2_swap_log(pool_a(), 0, 85, 105, 0)], Some(alice()));

        let opps = detector.detect(100, &pm);
        assert!(opps.is_empty());
    }
}
