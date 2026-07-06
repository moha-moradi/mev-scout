//! Sandwich attack detection — identifies buy-sell pairs that sandwich a victim transaction.

use std::collections::{HashMap, HashSet};
use alloy::primitives::{b256, Address, B256, U256};
use crate::data::ExecutedLog;
use crate::types::MevOpportunity;
use crate::pool::decoders::{
    decode_balancer_swap, decode_curve_swap, decode_v3_swap,
    BALANCER_SWAP_TOPIC, CURVE_TOKEN_EXCHANGE_TOPIC, CURVE_V2_TOKEN_EXCHANGE_TOPIC,
    V3_SWAP_TOPIC,
};
use crate::pool::math::quote_exact_in;
use crate::pool::math::constant_product_output_amount;
use crate::pool::state::{calldata_gas_estimate, PoolManager, PoolState};
use crate::pool::math::v3::{estimate_v3_swap_gas, quote_v3_exact_in};
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

    /// Compute raw profit (back.amount_out - front.amount_in) at a given frontrun input,
    /// using the actual pool formula to re-quote both legs. Handles each DEX type's
    /// unique pricing function (x*y=k for V2, tick-walking for V3, StableSwap for Curve,
    /// weighted product for Balancer).
    fn sandwich_raw_profit_at(
        pool_manager: &PoolManager,
        front: &SwapRecord,
        victim: &SwapRecord,
        back: &SwapRecord,
        token_in: Address,
        token_out: Address,
        front_in_adj: u128,
    ) -> Option<u128> {
        let pool = pool_manager.get(&front.pool)?;
        let front_dir_is_t0t1 = front.direction == SwapDirection::Token0ForToken1;

        // --- Frontrun at adjusted input ---
        let front_out_adj = match pool {
            PoolState::UniswapV2(v2) => {
                // Current pool state includes original frontrun + victim.
                // Reverse-apply original frontrun to get pre-frontrun reserves.
                let (r0_pre, r1_pre) = if front_dir_is_t0t1 {
                    (v2.reserve0.saturating_sub(front.amount_in),
                     v2.reserve1.saturating_add(front.amount_out))
                } else {
                    (v2.reserve0.saturating_add(front.amount_out),
                     v2.reserve1.saturating_sub(front.amount_in))
                };
                let (r_in, r_out) = if front_dir_is_t0t1 {
                    (r0_pre, r1_pre)
                } else {
                    (r1_pre, r0_pre)
                };
                constant_product_output_amount(front_in_adj, r_in, r_out, v2.info.fee)?
            }
            PoolState::UniswapV3(v3) => {
                quote_v3_exact_in(v3, front_in_adj, front_dir_is_t0t1)?
            }
            _ => {
                let (ti, to) = if front_dir_is_t0t1 {
                    (token_in, token_out)
                } else {
                    (token_out, token_in)
                };
                quote_exact_in(pool, ti, to, front_in_adj)?
            }
        };

        // --- Backrun at adjusted amount ---
        // The backrun sells what the frontrun bought (front_out_adj) in the opposite direction,
        // after the victim swap moves the price in the same direction as the frontrun.
        let back_out_adj = match pool {
            PoolState::UniswapV2(v2) => {
                // For V2 we can compute exactly: reverse frontrun, apply adjusted, apply victim.
                let (r0_pre, r1_pre) = if front_dir_is_t0t1 {
                    (v2.reserve0.saturating_sub(front.amount_in),
                     v2.reserve1.saturating_add(front.amount_out))
                } else {
                    (v2.reserve0.saturating_add(front.amount_out),
                     v2.reserve1.saturating_sub(front.amount_in))
                };
                // Apply adjusted frontrun
                let (r0_af, r1_af) = if front_dir_is_t0t1 {
                    (r0_pre.saturating_add(front_in_adj),
                     r1_pre.saturating_sub(front_out_adj))
                } else {
                    (r0_pre.saturating_sub(front_out_adj),
                     r1_pre.saturating_add(front_in_adj))
                };
                // Apply victim (same direction as frontrun, historical amounts)
                let (r0_av, r1_av) = if front_dir_is_t0t1 {
                    (r0_af.saturating_add(victim.amount_in),
                     r1_af.saturating_sub(victim.amount_out))
                } else {
                    (r0_af.saturating_sub(victim.amount_out),
                     r1_af.saturating_add(victim.amount_in))
                };
                // Backrun sells front_out_adj in opposite direction
                let (r_in, r_out) = if front_dir_is_t0t1 {
                    (r1_av, r0_av)
                } else {
                    (r0_av, r1_av)
                };
                constant_product_output_amount(front_out_adj, r_in, r_out, v2.info.fee)?
            }
            _ => {
                // Non-V2: estimate backrun using the same relative exchange rate
                // as the historical backrun, which accounts for the victim swap's effect.
                let back_rate = (back.amount_out as u128).saturating_mul(1_000_000)
                    .saturating_div((back.amount_in as u128).max(1));
                front_out_adj.saturating_mul(back_rate).saturating_div(1_000_000)
            }
        };

        Some(back_out_adj.saturating_sub(front_in_adj))
    }

    /// Normalize raw profit to native token using the best available price feed
    /// (same multi-DEX logic as compute_sandwich_profit).
    fn normalize_profit_to_native(
        pool_manager: &PoolManager,
        profit_token: Address,
        native_token: Address,
        profit_raw: u128,
    ) -> Option<U256> {
        let ref_pool = pool_manager.find_pair_pool(&profit_token, &native_token)?;
        let pool = pool_manager.get(&ref_pool)?;
        let converted = match pool {
            PoolState::UniswapV2(v2) => {
                let (r_in, r_out) = if v2.info.token0 == profit_token {
                    (v2.reserve0, v2.reserve1)
                } else {
                    (v2.reserve1, v2.reserve0)
                };
                constant_product_output_amount(profit_raw, r_in, r_out, v2.info.fee)?
            }
            _ => quote_exact_in(pool, profit_token, native_token, profit_raw)?,
        };
        Some(U256::from(converted))
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
                    Some(pool) => {
                        quote_exact_in(pool, profit_token, native_token, profit_raw)
                    }
                    _ => None,
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

                // H9: Compute slippage by re-quoting through the pool at adjusted frontrun amounts
                let profit_token = match front.direction {
                    SwapDirection::Token0ForToken1 => token_in,
                    SwapDirection::Token1ForToken0 => token_out,
                };
                let native_token = match front.direction {
                    SwapDirection::Token0ForToken1 => token_out,
                    SwapDirection::Token1ForToken0 => token_in,
                };
                let sandwich_slippage = |pct: u128| -> Option<U256> {
                    if front.amount_in == 0 { return None; }
                    let adj_in = (front.amount_in as u128).saturating_mul(pct) / 100;
                    if adj_in == 0 { return None; }
                    let raw_adj = Self::sandwich_raw_profit_at(
                        pool_manager, front, victim, back,
                        token_in, token_out, adj_in,
                    )?;
                    // Normalize to native using the same path as compute_sandwich_profit
                    if pool_manager.is_wrapped_native(&profit_token) {
                        return Some(U256::from(raw_adj));
                    }
                    if pool_manager.is_wrapped_native(&native_token) {
                        return Self::normalize_profit_to_native(pool_manager, profit_token, native_token, raw_adj);
                    }
                    Some(U256::from(raw_adj))
                };

                opportunities.push(MevOpportunity {
                    canonical_id: None,
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
                    profit_slippage_p1: sandwich_slippage(101),
                    profit_slippage_m1: sandwich_slippage(99),
                    profit_slippage_p2: sandwich_slippage(102),
                    profit_slippage_m2: sandwich_slippage(98),
                    gas_cost_wei,
                    timestamp,
                    path: None,
                    tick_lower: None,
                    tick_upper: None,
                    liquidity_amount: None,
                    victim_tx_index: Some(victim.tx_index),
                    backrun_tx_index: Some(back.tx_index),
                    mempool_only: false,
                    confidence: None,
                });
            }
        }

        opportunities
    }
}

