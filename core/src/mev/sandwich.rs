//! Sandwich attack detection — identifies buy-sell pairs that sandwich a victim transaction.

use std::collections::{HashMap, HashSet};
use alloy::primitives::{b256, Address, B256, U256};
use crate::data::ExecutedLog;
use crate::mev::opportunity::MevOpportunity;
use crate::pool::decoders::{
    decode_balancer_swap, decode_curve_swap, decode_v3_swap,
    BALANCER_SWAP_TOPIC, CURVE_TOKEN_EXCHANGE_TOPIC, CURVE_V2_TOKEN_EXCHANGE_TOPIC,
    V3_SWAP_TOPIC,
};
use crate::mev::two_hop::{balancer_output_amount, curve_output_amount};
use crate::pool::math::constant_product_output_amount;
use crate::pool::state::{calldata_gas_estimate, PoolManager, PoolState};
use crate::pool::v3_quote::{estimate_v3_swap_gas, quote_v3_exact_in};
use crate::types::{GasConfig, Strategy};
use crate::utils::u128_from_be_bytes;

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
    amount_out: u128,
    /// token_in address (None for unknown)
    token_in: Option<Address>,
    /// token_out address (None for unknown)
    token_out: Option<Address>,
}

/// Detects sandwich attacks on Uniswap V2 and V3 pools.
///
/// Pattern: [front_run_buy, victim_swap, backrun_sell] by the same sender.
/// Call `process_tx()` for each tx, then `detect()` once per block.
pub struct SandwichDetector {
    swap_records: Vec<SwapRecord>,
    emitted: HashSet<(Address, usize)>,
    block_number: u64,
}

impl SandwichDetector {
    pub fn new(block_number: u64) -> Self {
        SandwichDetector {
            swap_records: Vec::new(),
            emitted: HashSet::new(),
            block_number,
        }
    }

    /// Process a single transaction's swap logs.
    /// Call BEFORE `detect()` for each tx in block order.
    pub fn process_tx(
        &mut self,
        tx_index: usize,
        logs: &[ExecutedLog],
        sender: Option<Address>,
        pool_manager: &PoolManager,
    ) {
        let Some(sender) = sender else { return };

        for log in logs {
            if log.topics.is_empty() {
                continue;
            }
            let t0 = log.topics[0];

            if t0 == V2_SWAP_TOPIC {
                self.process_v2_swap(log, tx_index, sender, pool_manager);
            } else if t0 == *V3_SWAP_TOPIC {
                self.process_v3_swap(log, tx_index, sender, pool_manager);
            } else if t0 == *CURVE_TOKEN_EXCHANGE_TOPIC || t0 == *CURVE_V2_TOKEN_EXCHANGE_TOPIC {
                self.process_curve_swap(log, tx_index, sender, pool_manager);
            } else if t0 == *BALANCER_SWAP_TOPIC {
                self.process_balancer_swap(log, tx_index, sender, pool_manager);
            }
        }
    }

    fn process_v2_swap(&mut self, log: &ExecutedLog, tx_index: usize, sender: Address, pool_manager: &PoolManager) {
        if log.data.len() < 128 {
            return;
        }

        let amt0_in = u128_from_be_bytes(&log.data[..32]);
        let amt1_in = u128_from_be_bytes(&log.data[32..64]);
        let amt0_out = u128_from_be_bytes(&log.data[64..96]);
        let amt1_out = u128_from_be_bytes(&log.data[96..128]);

        let (direction, amount_in, amount_out, token_in, token_out) =
            if amt0_in > 0 && amt1_out > 0 {
                let info = pool_manager.get(&log.address).map(|p| p.info());
                let ti = info.map(|i| i.token0);
                let to = info.map(|i| i.token1);
                (SwapDirection::Token0ForToken1, amt0_in, amt1_out, ti, to)
            } else if amt1_in > 0 && amt0_out > 0 {
                let info = pool_manager.get(&log.address).map(|p| p.info());
                let ti = info.map(|i| i.token1);
                let to = info.map(|i| i.token0);
                (SwapDirection::Token1ForToken0, amt1_in, amt0_out, ti, to)
            } else {
                return;
            };

        self.swap_records.push(SwapRecord {
            tx_index,
            sender,
            pool: log.address,
            direction,
            amount_in,
            amount_out,
            token_in,
            token_out,
        });
    }

    fn process_v3_swap(&mut self, log: &ExecutedLog, tx_index: usize, sender: Address, pool_manager: &PoolManager) {
        if let Some(decoded) = decode_v3_swap(log) {
            // Determine direction and amounts from signed values
            let (direction, amount_in, amount_out, token_in, token_out) =
                if decoded.amount0 > 0 && decoded.amount1 < 0 {
                    let info = pool_manager.get(&log.address).map(|p| p.info());
                    let ti = info.map(|i| i.token0);
                    let to = info.map(|i| i.token1);
                    (SwapDirection::Token0ForToken1, decoded.amount0 as u128, decoded.amount1.unsigned_abs(), ti, to)
                } else if decoded.amount0 < 0 && decoded.amount1 > 0 {
                    let info = pool_manager.get(&log.address).map(|p| p.info());
                    let ti = info.map(|i| i.token1);
                    let to = info.map(|i| i.token0);
                    (SwapDirection::Token1ForToken0, decoded.amount1 as u128, decoded.amount0.unsigned_abs(), ti, to)
                } else {
                    return;
                };

            self.swap_records.push(SwapRecord {
                tx_index,
                sender,
                pool: log.address,
                direction,
                amount_in,
                amount_out,
                token_in,
                token_out,
            });
        }
    }

    fn process_curve_swap(&mut self, log: &ExecutedLog, tx_index: usize, sender: Address, pool_manager: &PoolManager) {
        if let Some(decoded) = decode_curve_swap(log) {
            let pool = pool_manager.get(&log.address);
            let token_in = pool.and_then(|p| {
                if let PoolState::Curve(curve) = p {
                    curve.token_index.iter()
                        .find(|(_, &idx)| idx == decoded.coin_sold as usize)
                        .map(|(addr, _)| *addr)
                } else { None }
            });
            let token_out = pool.and_then(|p| {
                if let PoolState::Curve(curve) = p {
                    curve.token_index.iter()
                        .find(|(_, &idx)| idx == decoded.coin_bought as usize)
                        .map(|(addr, _)| *addr)
                } else { None }
            });
            let direction = if Some(true) == token_in.and_then(|ti| {
                pool.map(|p| p.info().token0 == ti)
            }) {
                SwapDirection::Token0ForToken1
            } else {
                SwapDirection::Token1ForToken0
            };
            self.swap_records.push(SwapRecord {
                tx_index,
                sender,
                pool: log.address,
                direction,
                amount_in: decoded.amount_sold,
                amount_out: decoded.amount_bought,
                token_in,
                token_out,
            });
        }
    }

    fn process_balancer_swap(&mut self, log: &ExecutedLog, tx_index: usize, sender: Address, pool_manager: &PoolManager) {
        if let Some(decoded) = decode_balancer_swap(log) {
            let pool = pool_manager.get(&log.address);
            let token_in = Some(decoded.token_in);
            let token_out = Some(decoded.token_out);
            let direction = if Some(true) == token_in.and_then(|ti| {
                pool.map(|p| p.info().token0 == ti)
            }) {
                SwapDirection::Token0ForToken1
            } else {
                SwapDirection::Token1ForToken0
            };
            self.swap_records.push(SwapRecord {
                tx_index,
                sender,
                pool: log.address,
                direction,
                amount_in: decoded.amount_in,
                amount_out: decoded.amount_out,
                token_in,
                token_out,
            });
        }
    }

    /// Compute profit from a matched frontrun/backrun pair.
    /// Returns (normalized_profit_in_native, raw_profit_in_profit_token).
    /// raw_profit is None when profit_token is already wrapped native (no conversion needed).
    fn compute_sandwich_profit(
        &self,
        front: &SwapRecord,
        back: &SwapRecord,
        token_in: Address,
        token_out: Address,
        pool_manager: &PoolManager,
    ) -> (U256, Option<U256>) {
        if back.amount_out <= front.amount_in {
            return (U256::ZERO, None);
        }
        let profit_raw = back.amount_out - front.amount_in;
        let profit_token = match front.direction {
            SwapDirection::Token0ForToken1 => token_in,
            SwapDirection::Token1ForToken0 => token_out,
        };
        if pool_manager.is_wrapped_native(&profit_token) {
            return (U256::from(profit_raw), None);
        }
        // Try converting from profit token to the other token (which may be wrapped native)
        let native_token = match front.direction {
            SwapDirection::Token0ForToken1 => token_out,
            SwapDirection::Token1ForToken0 => token_in,
        };
        if pool_manager.is_wrapped_native(&native_token) {
            // Find a reference pool that trades profit_token ↔ native_token
            if let Some(ref_pool) = pool_manager.find_pair_pool(&profit_token, &native_token) {
                let converted = match pool_manager.get(&ref_pool) {
                    Some(crate::pool::state::PoolState::UniswapV2(v2)) => {
                        let (reserve_in, reserve_out) =
                            if v2.info.token0 == profit_token {
                                (v2.reserve0, v2.reserve1)
                            } else {
                                (v2.reserve1, v2.reserve0)
                            };
                        constant_product_output_amount(
                            profit_raw, reserve_in, reserve_out, v2.info.fee,
                        )
                    }
                    Some(crate::pool::state::PoolState::UniswapV3(v3)) => {
                        let zero_for_one = v3.info.token0 == profit_token;
                        quote_v3_exact_in(v3, profit_raw, zero_for_one)
                    }
                    Some(crate::pool::state::PoolState::Curve(curve)) => {
                        if curve.token_index.contains_key(&profit_token)
                            && curve.token_index.contains_key(&native_token)
                        {
                            curve_output_amount(profit_raw, curve, profit_token, native_token)
                        } else { None }
                    }
                    Some(crate::pool::state::PoolState::Balancer(bal)) => {
                        if let (Some(&idx_in), Some(&idx_out)) = (
                            bal.token_index.get(&profit_token),
                            bal.token_index.get(&native_token),
                        ) {
                            let reserve_in = bal.balances[idx_in];
                            let reserve_out = bal.balances[idx_out];
                            let default_w = 1_000_000_000_000_000_000u128;
                            let (w_in, w_out) = if bal.weights.len() == bal.balances.len() && !bal.weights.is_empty() {
                                (bal.weights[idx_in], bal.weights[idx_out])
                            } else {
                                (default_w, default_w)
                            };
                            balancer_output_amount(
                                profit_raw, reserve_in, reserve_out, w_in, w_out, bal.info.fee,
                            )
                        } else { None }
                    }
                    _ => {
                        // Last resort: V2-style spot reserve ratio
                        pool_manager.get_v2_state(&ref_pool).and_then(|state| {
                            let (reserve_sell, reserve_buy) =
                                if state.info.token0 == profit_token {
                                    (state.reserve0, state.reserve1)
                                } else {
                                    (state.reserve1, state.reserve0)
                                };
                            if reserve_sell > 0 {
                                Some(profit_raw.saturating_mul(reserve_buy).saturating_div(reserve_sell))
                            } else { None }
                        })
                    }
                };
                if let Some(converted) = converted {
                    return (U256::from(converted), Some(U256::from(profit_raw)));
                }
            }
        }
        (U256::ZERO, Some(U256::from(profit_raw)))
    }

    /// Detect sandwich patterns from accumulated swap records.
    /// Call AFTER `process_tx()` for all txs in the block.
    pub fn detect(
        &mut self,
        timestamp: u64,
        pool_manager: &PoolManager,
        base_fee_per_gas: u128,
        gas_config: &GasConfig,
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

                self.emitted.insert(dedup_key);

                let (token_in, token_out) = match (front.token_in, front.token_out) {
                    (Some(ti), Some(to)) => (ti, to),
                    _ => {
                        let pool_info = pool_manager.get(&front.pool)
                            .map(|p| p.info());
                        match pool_info {
                            Some(info) => match front.direction {
                                SwapDirection::Token0ForToken1 => (info.token0, info.token1),
                                SwapDirection::Token1ForToken0 => (info.token1, info.token0),
                            },
                            None => (Address::ZERO, Address::ZERO),
                        }
                    }
                };

                let (profit_wei, raw_profit) = self.compute_sandwich_profit(front, back, token_in, token_out, pool_manager);

                // Per-opportunity gas: front-run swap + back-run swap on the pool
                // H7: Use direction-aware V3 estimate for each swap leg.
                let pool_gas = pool_manager.get(&front.pool)
                    .map(|p| match p {
                        PoolState::UniswapV3(v3) => {
                            let front_dir = front.direction == SwapDirection::Token0ForToken1;
                            let back_dir = back.direction == SwapDirection::Token0ForToken1;
                            estimate_v3_swap_gas(v3, front_dir)
                                .saturating_add(estimate_v3_swap_gas(v3, back_dir))
                        }
                        other => other.gas_estimate().saturating_mul(2),
                    })
                    .unwrap_or(80_000u64.saturating_mul(2));
                let calldata = calldata_gas_estimate(1);
                let gas_limit = 40_000 + calldata + pool_gas;
                let gas_cost_wei = gas_config.compute_gas_cost_with_limit(gas_limit, base_fee_per_gas);

                opportunities.push(MevOpportunity {
                    block_number: self.block_number,
                    tx_index: front.tx_index,
                    strategy: Strategy::Sandwich,
                    pool_a: front.pool,
                    pool_b: Address::ZERO,
                    token_in,
                    token_out,
                    input_amount: U256::from(front.amount_in),
                    expected_profit: profit_wei,
                    raw_profit,
                    profit_slippage_p1: None,
                    profit_slippage_m1: None,
                    profit_slippage_p2: None,
                    profit_slippage_m2: None,
                    pga_adjusted_profit: None,
                    gas_cost_wei,
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, B256};
    use crate::data::ExecutedLog;
    use crate::pool::decoders::V3_SWAP_TOPIC;
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

    fn v3_swap_log(pool: Address, amt0: i128, amt1: i128, tick: i32) -> ExecutedLog {
        let mut data = Vec::with_capacity(160);
        let mut b = [0u8; 32];
        let amt0_be = amt0.to_be_bytes();
        b[16..32].copy_from_slice(&amt0_be);
        data.extend_from_slice(&b);
        let amt1_be = amt1.to_be_bytes();
        b = [0u8; 32];
        b[16..32].copy_from_slice(&amt1_be);
        data.extend_from_slice(&b);
        b = [0u8; 32];
        let sqrt = U256::from(1u128 << 96);
        b.copy_from_slice(&sqrt.to_be_bytes::<32>());
        data.extend_from_slice(&b);
        b = [0u8; 32];
        b[16..32].copy_from_slice(&1_000_000u128.to_be_bytes());
        data.extend_from_slice(&b);
        let tick_be = tick.to_be_bytes();
        b = [0u8; 32];
        b[28..32].copy_from_slice(&tick_be);
        data.extend_from_slice(&b);
        ExecutedLog {
            address: pool,
            topics: vec![V3_SWAP_TOPIC, B256::ZERO, B256::ZERO],
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
            creation_block: 0,
            pool_id: None,
            factory: None,
            },
            reserve0: 1_000_000,
            reserve1: 1_000_000,
        }));
        pm
    }

    fn gas_cfg() -> GasConfig { GasConfig::default() }

    #[test]
    fn test_empty_detector_returns_nothing() {
        let mut detector = SandwichDetector::new(1);
        let pm = PoolManager::new();
        let opps = detector.detect(100, &pm, 50_000_000_000, &gas_cfg());
        assert!(opps.is_empty());
    }

    #[test]
    fn test_sandwich_detected() {
        let mut detector = SandwichDetector::new(1);
        let pm = make_pm_with_pool(pool_a(), address!("cccccccccccccccccccccccccccccccccccccccc"), address!("dddddddddddddddddddddddddddddddddddddddd"));

        detector.process_tx(0, &[v2_swap_log(pool_a(), 100, 0, 0, 90)], Some(alice()), &pm);
        assert!(detector.detect(100, &pm, 50_000_000_000, &gas_cfg()).is_empty());

        detector.process_tx(1, &[v2_swap_log(pool_a(), 200, 0, 0, 170)], Some(bob()), &pm);
        assert!(detector.detect(100, &pm, 50_000_000_000, &gas_cfg()).is_empty());

        detector.process_tx(2, &[v2_swap_log(pool_a(), 0, 85, 105, 0)], Some(alice()), &pm);
        let opps = detector.detect(100, &pm, 50_000_000_000, &gas_cfg());
        assert_eq!(opps.len(), 1);

        let opp = &opps[0];
        assert_eq!(opp.strategy, Strategy::Sandwich);
        assert_eq!(opp.pool_a, pool_a());
        assert_eq!(opp.tx_index, 0);
        assert_eq!(opp.victim_tx_index, Some(1));
        assert_eq!(opp.backrun_tx_index, Some(2));
        assert_ne!(opp.token_in, Address::ZERO);
        assert_ne!(opp.token_out, Address::ZERO);
        assert!(opp.gas_cost_wei > 0, "Gas cost should be > 0");
    }

    #[test]
    fn test_different_eoa_no_sandwich() {
        let mut detector = SandwichDetector::new(1);
        let pm = PoolManager::new();

        detector.process_tx(0, &[v2_swap_log(pool_a(), 100, 0, 0, 90)], Some(alice()), &pm);
        detector.process_tx(1, &[v2_swap_log(pool_a(), 200, 0, 0, 170)], Some(bob()), &pm);
        detector.process_tx(2, &[v2_swap_log(pool_a(), 0, 85, 105, 0)], Some(address!("3333333333333333333333333333333333333333")), &pm);

        let opps = detector.detect(100, &pm, 50_000_000_000, &gas_cfg());
        assert!(opps.is_empty());
    }

    #[test]
    fn test_same_direction_backrun_no_sandwich() {
        let mut detector = SandwichDetector::new(1);
        let pm = PoolManager::new();

        detector.process_tx(0, &[v2_swap_log(pool_a(), 100, 0, 0, 90)], Some(alice()), &pm);
        detector.process_tx(1, &[v2_swap_log(pool_a(), 200, 0, 0, 170)], Some(bob()), &pm);
        detector.process_tx(2, &[v2_swap_log(pool_a(), 300, 0, 0, 250)], Some(alice()), &pm);

        let opps = detector.detect(100, &pm, 50_000_000_000, &gas_cfg());
        assert!(opps.is_empty());
    }

    #[test]
    fn test_no_duplicate_emission() {
        let mut detector = SandwichDetector::new(1);
        let pm = PoolManager::new();

        detector.process_tx(0, &[v2_swap_log(pool_a(), 100, 0, 0, 90)], Some(alice()), &pm);
        detector.process_tx(1, &[v2_swap_log(pool_a(), 200, 0, 0, 170)], Some(bob()), &pm);
        detector.process_tx(2, &[v2_swap_log(pool_a(), 0, 85, 105, 0)], Some(alice()), &pm);

        let opps = detector.detect(100, &pm, 50_000_000_000, &gas_cfg());
        assert_eq!(opps.len(), 1);

        let opps2 = detector.detect(100, &pm, 50_000_000_000, &gas_cfg());
        assert!(opps2.is_empty());
    }

    #[test]
    fn test_multiple_pools_independent() {
        let mut detector = SandwichDetector::new(1);
        let pm = PoolManager::new();

        detector.process_tx(0, &[v2_swap_log(pool_a(), 100, 0, 0, 90)], Some(alice()), &pm);
        detector.process_tx(1, &[v2_swap_log(pool_a(), 200, 0, 0, 170)], Some(bob()), &pm);
        detector.process_tx(2, &[v2_swap_log(pool_a(), 0, 85, 105, 0)], Some(alice()), &pm);

        detector.process_tx(3, &[v2_swap_log(pool_b(), 50, 0, 0, 45)], Some(alice()), &pm);
        detector.process_tx(4, &[v2_swap_log(pool_b(), 100, 0, 0, 85)], Some(bob()), &pm);

        let opps = detector.detect(100, &pm, 50_000_000_000, &gas_cfg());
        assert_eq!(opps.len(), 1);
        assert_eq!(opps[0].pool_a, pool_a());
    }

    #[test]
    fn test_single_tx_no_detection() {
        let mut detector = SandwichDetector::new(1);
        let pm = PoolManager::new();
        detector.process_tx(0, &[v2_swap_log(pool_a(), 100, 0, 0, 90)], Some(alice()), &pm);
        let opps = detector.detect(100, &pm, 50_000_000_000, &gas_cfg());
        assert!(opps.is_empty());
    }

    #[test]
    fn test_two_txs_no_detection() {
        let mut detector = SandwichDetector::new(1);
        let pm = PoolManager::new();
        detector.process_tx(0, &[v2_swap_log(pool_a(), 100, 0, 0, 90)], Some(alice()), &pm);
        detector.process_tx(1, &[v2_swap_log(pool_a(), 200, 0, 0, 170)], Some(bob()), &pm);
        let opps = detector.detect(100, &pm, 50_000_000_000, &gas_cfg());
        assert!(opps.is_empty());
    }

    #[test]
    fn test_interleaved_pool_swaps_no_false_positive() {
        let mut detector = SandwichDetector::new(1);
        let pm = PoolManager::new();

        detector.process_tx(0, &[v2_swap_log(pool_a(), 100, 0, 0, 90)], Some(alice()), &pm);
        detector.process_tx(1, &[v2_swap_log(pool_b(), 50, 0, 0, 45)], Some(bob()), &pm);
        detector.process_tx(2, &[v2_swap_log(pool_a(), 0, 85, 105, 0)], Some(alice()), &pm);

        let opps = detector.detect(100, &pm, 50_000_000_000, &gas_cfg());
        assert!(opps.is_empty());
    }

    #[test]
    fn test_sandwich_profit_computed() {
        let mut detector = SandwichDetector::new(1);
        let wmatic = address!("0x0d500b1d8e8ef31e21c99d1db9a6444d3adf1270");
        let usdc = address!("3333333333333333333333333333333333333333");
        let pool_addr = pool_a();
        let pm = make_pm_with_pool(pool_addr, wmatic, usdc).with_wrapped_native(wmatic);

        detector.process_tx(0, &[v2_swap_log(pool_addr, 0, 100, 95, 0)], Some(alice()), &pm);
        detector.process_tx(1, &[v2_swap_log(pool_addr, 0, 200, 180, 0)], Some(bob()), &pm);
        detector.process_tx(2, &[v2_swap_log(pool_addr, 82, 0, 0, 108)], Some(alice()), &pm);

        let opps = detector.detect(100, &pm, 50_000_000_000, &gas_cfg());
        assert_eq!(opps.len(), 1);
        assert!(opps[0].expected_profit > U256::ZERO);
        assert!(opps[0].gas_cost_wei > 0);
    }

    #[test]
    fn test_v3_sandwich_detected() {
        let mut detector = SandwichDetector::new(1);
        let pm = PoolManager::new();

        // V3 swaps: Token0ForToken1 (amt0 > 0, amt1 < 0)
        detector.process_tx(0, &[v3_swap_log(pool_a(), 100, -90, 0)], Some(alice()), &pm);
        detector.process_tx(1, &[v3_swap_log(pool_a(), 200, -170, 5)], Some(bob()), &pm);
        detector.process_tx(2, &[v3_swap_log(pool_a(), -85, 105, 10)], Some(alice()), &pm);

        let opps = detector.detect(100, &pm, 50_000_000_000, &gas_cfg());
        assert_eq!(opps.len(), 1, "V3 sandwich should be detected");
        let opp = &opps[0];
        assert_eq!(opp.strategy, Strategy::Sandwich);
        assert_eq!(opp.pool_a, pool_a());
        assert_eq!(opp.victim_tx_index, Some(1));
        assert_eq!(opp.backrun_tx_index, Some(2));
    }

    #[test]
    fn test_v3_sandwich_same_direction_no_detection() {
        let mut detector = SandwichDetector::new(1);
        let pm = PoolManager::new();

        // All same direction — not a sandwich
        detector.process_tx(0, &[v3_swap_log(pool_a(), 100, -90, 0)], Some(alice()), &pm);
        detector.process_tx(1, &[v3_swap_log(pool_a(), 200, -170, 5)], Some(bob()), &pm);
        detector.process_tx(2, &[v3_swap_log(pool_a(), 300, -250, 10)], Some(alice()), &pm);

        let opps = detector.detect(100, &pm, 50_000_000_000, &gas_cfg());
        assert!(opps.is_empty(), "Same direction is not a sandwich");
    }
}
