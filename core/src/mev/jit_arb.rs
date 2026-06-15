//! JIT arbitrage detection — identifies arbitrage trades that sandwich a JIT liquidity event.

use std::collections::{HashMap, HashSet};
use alloy::primitives::{Address, U256};
use crate::data::ExecutedLog;
use crate::pool::decoders::{decode_v3_mint_burn, decode_v3_swap, V3_SWAP_TOPIC, V3_MINT_TOPIC, V3_BURN_TOPIC};
use crate::pool::math::constant_product_output_amount;
use crate::pool::v3_quote::quote_v3_exact_in;
use crate::pool::state::{PoolManager, PoolState};
use crate::mev::opportunity::MevOpportunity;
use crate::types::{GasConfig, Strategy};

#[derive(Debug, Clone)]
struct SwapEvent {
    tx_index: usize,
    pool: Address,
    sender: Address,
    amount_in: u128,
    /// The token that was sold (input token of the swap)
    token_in: Address,
}

#[derive(Debug, Clone)]
struct JitArbMint {
    mint_tx_index: usize,
    tick_lower: i32,
    tick_upper: i32,
    amount: u128,
    sender: Address,
    swapped: bool,
    burned: bool,
}

pub struct JitArbDetector {
    active_mints: HashMap<Address, Vec<JitArbMint>>,
    swap_events: Vec<SwapEvent>,
    emitted: HashSet<(Address, usize, Address)>,
    block_number: u64,
    proximity_window: usize,
}

impl JitArbDetector {
    pub fn new(block_number: u64) -> Self {
        JitArbDetector {
            active_mints: HashMap::new(),
            swap_events: Vec::new(),
            emitted: HashSet::new(),
            block_number,
            proximity_window: 1,
        }
    }

    pub fn with_proximity_window(mut self, window: usize) -> Self {
        self.proximity_window = window;
        self
    }

    pub fn process_tx(&mut self, tx_index: usize, logs: &[ExecutedLog], sender: Option<Address>, pm: &PoolManager) {
        let sender = match sender {
            Some(s) => s,
            None => return,
        };

        for log in logs {
            if log.topics.is_empty() {
                continue;
            }
            let t0 = log.topics[0];

            if t0 == *V3_MINT_TOPIC {
                if let Some(decoded) = decode_v3_mint_burn(log) {
                    if decoded.amount > 0 {
                        self.active_mints
                            .entry(log.address)
                            .or_default()
                            .push(JitArbMint {
                                mint_tx_index: tx_index,
                                tick_lower: decoded.tick_lower,
                                tick_upper: decoded.tick_upper,
                                amount: decoded.amount as u128,
                                sender,
                                swapped: false,
                                burned: false,
                            });
                    }
                }
            }

            if t0 == V3_BURN_TOPIC {
                if let Some(decoded) = decode_v3_mint_burn(log) {
                    if let Some(mints) = self.active_mints.get_mut(&log.address) {
                        for mint in mints.iter_mut() {
                            if mint.burned { continue; }
                            if mint.sender == sender
                                && mint.tick_lower == decoded.tick_lower
                                && mint.tick_upper == decoded.tick_upper
                                && mint.mint_tx_index <= tx_index
                            {
                                mint.burned = true;
                                break;
                            }
                        }
                    }
                }
            }

            if t0 == V3_SWAP_TOPIC {
                let (amount_in, token_in) = if let Some(decoded) = decode_v3_swap(log) {
                    let amt = if decoded.amount0 > 0 { decoded.amount0 as u128 }
                               else { decoded.amount1 as u128 };
                    // Determine which token was sold from the swap event.
                    // For V3: amount0 > 0 → token0 sold (pool receives token0),
                    //          amount1 > 0 → token1 sold.
                    let sold = if decoded.amount0 > 0 {
                        pm.get(&log.address).map(|p| p.info().token0).unwrap_or(Address::ZERO)
                    } else {
                        pm.get(&log.address).map(|p| p.info().token1).unwrap_or(Address::ZERO)
                    };
                    (amt, sold)
                } else {
                    (0, Address::ZERO)
                };

                self.swap_events.push(SwapEvent {
                    tx_index,
                    pool: log.address,
                    sender,
                    amount_in,
                    token_in,
                });

                if let Some(mints) = self.active_mints.get_mut(&log.address) {
                    for mint in mints.iter_mut() {
                        if mint.sender == sender && mint.mint_tx_index <= tx_index {
                            mint.swapped = true;
                        }
                    }
                }
            }
        }
    }

    pub fn detect(
        &mut self,
        timestamp: u64,
        pm: &PoolManager,
        base_fee_per_gas: u128,
        gas_config: &GasConfig,
    ) -> Vec<MevOpportunity> {
        let mut opportunities = Vec::new();
        let pool_addrs: Vec<Address> = self.active_mints.keys().copied().collect();

        for &pool_p in &pool_addrs {
            let Some(mints) = self.active_mints.get(&pool_p) else { continue };
            for mint in mints {
                let dedup_key = (pool_p, mint.mint_tx_index, mint.sender);
                if self.emitted.contains(&dedup_key) || !mint.swapped {
                    continue;
                }

                let swaps_on_p: Vec<&SwapEvent> = self.swap_events.iter()
                    .filter(|s| s.pool == pool_p && s.sender == mint.sender && s.tx_index >= mint.mint_tx_index)
                    .collect();
                if swaps_on_p.is_empty() {
                    continue;
                }

                for swap_p in &swaps_on_p {
                    for swap_q in &self.swap_events {
                        if swap_q.pool == pool_p || swap_q.sender != mint.sender {
                            continue;
                        }
                        let p_idx = swap_p.tx_index;
                        let q_idx = swap_q.tx_index;
                        let max_idx = p_idx.max(q_idx);
                        let min_idx = p_idx.min(q_idx);
                        if max_idx - min_idx > self.proximity_window {
                            continue;
                        }
                            if pools_share_token(pm, pool_p, swap_q.pool) {
                                self.emitted.insert(dedup_key);

                            let arb_profit = estimate_arb_profit(pm, swap_p, swap_q);

                            opportunities.push(Self::build_opp(
                                self.block_number, pool_p, swap_q.pool, mint, timestamp,
                                U256::from(arb_profit), base_fee_per_gas, gas_config, pm,
                            ));
                            break;
                        }
                    }
                    if !opportunities.is_empty() { break; }
                }
            }
        }

        opportunities
    }

    fn build_opp(
        block_number: u64,
        jit_pool: Address,
        arb_pool: Address,
        mint: &JitArbMint,
        timestamp: u64,
        expected_profit: U256,
        base_fee_per_gas: u128,
        gas_config: &GasConfig,
        pm: &PoolManager,
    ) -> MevOpportunity {
        let gas_cost_wei = gas_config.compute_gas_cost(
            Strategy::JitArb,
            base_fee_per_gas,
            &HashMap::new(),
        );
        // Populate token_in/token_out from the JIT pool — both tokens are involved
        // in the liquidity provision and subsequent arb swap.
        let token_in = pm.get(&jit_pool).map(|p| p.info().token0).unwrap_or(Address::ZERO);
        let token_out = pm.get(&jit_pool).map(|p| p.info().token1).unwrap_or(Address::ZERO);
        MevOpportunity {
            block_number,
            tx_index: mint.mint_tx_index,
            strategy: Strategy::JitArb,
            pool_a: jit_pool,
            pool_b: arb_pool,
            token_in,
            token_out,
            input_amount: U256::from(mint.amount),
            expected_profit,
            gas_cost_wei,
            timestamp,
            path: Some(vec![jit_pool, arb_pool]),
            tick_lower: Some(mint.tick_lower),
            tick_upper: Some(mint.tick_upper),
            liquidity_amount: Some(mint.amount),
            victim_tx_index: None,
            backrun_tx_index: None,
        }
    }
}

fn pools_share_token(pm: &PoolManager, pool_a: Address, pool_b: Address) -> bool {
    let Some(info_a) = pm.get(&pool_a).map(|p| p.info()) else { return false };
    let Some(info_b) = pm.get(&pool_b).map(|p| p.info()) else { return false };
    info_a.token0 == info_b.token0
        || info_a.token0 == info_b.token1
        || info_a.token1 == info_b.token0
        || info_a.token1 == info_b.token1
}

/// Find the address of the token shared between two pools.
fn shared_token(pm: &PoolManager, pool_a: Address, pool_b: Address) -> Option<Address> {
    let info_a = pm.get(&pool_a)?.info();
    let info_b = pm.get(&pool_b)?.info();
    if info_a.token0 == info_b.token0 || info_a.token0 == info_b.token1 {
        Some(info_a.token0)
    } else if info_a.token1 == info_b.token0 || info_a.token1 == info_b.token1 {
        Some(info_a.token1)
    } else {
        None
    }
}

/// Convert a swap's `amount_in` (denominated in `swap.token_in`) to an equivalent
/// amount of `shared_token` using the pool's reserves or state.
///
/// If the swap already sells the shared token, returns `amount_in` directly.
/// Otherwise, simulates a swap from `token_in` → `shared_token` on the same pool
/// to estimate the received amount.
fn convert_to_shared_token(pm: &PoolManager, swap: &SwapEvent, shared: Address) -> u128 {
    if swap.token_in == shared || swap.amount_in == 0 {
        return swap.amount_in;
    }
    match pm.get(&swap.pool) {
        Some(PoolState::UniswapV2(v2)) => {
            let (reserve_in, reserve_out) = if v2.info.token0 == swap.token_in {
                (v2.reserve0, v2.reserve1)
            } else {
                (v2.reserve1, v2.reserve0)
            };
            constant_product_output_amount(swap.amount_in, reserve_in, reserve_out, v2.info.fee)
                .unwrap_or(0)
        }
        Some(PoolState::UniswapV3(v3)) => {
            let zero_for_one = v3.info.token0 == swap.token_in;
            quote_v3_exact_in(v3, swap.amount_in, zero_for_one).unwrap_or(0)
        }
        _ => 0,
    }
}

/// Estimate arbitrage profit between two swaps on pools that share a token.
///
/// Converts both swap amounts to the shared token denomination using each
/// pool's own pricing, then returns the absolute difference.
fn estimate_arb_profit(pm: &PoolManager, swap_p: &SwapEvent, swap_q: &SwapEvent) -> u128 {
    let shared = match shared_token(pm, swap_p.pool, swap_q.pool) {
        Some(s) => s,
        None => return 0,
    };
    let p_val = convert_to_shared_token(pm, swap_p, shared);
    let q_val = convert_to_shared_token(pm, swap_q, shared);
    if p_val > q_val { p_val - q_val } else { q_val - p_val }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, B256};
    use crate::pool::state::{PoolManager, UniswapV2PoolState, PoolInfo, PoolState};

    fn pool_p() -> Address { address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa") }
    fn pool_q() -> Address { address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb") }
    fn wmatic() -> Address { address!("0d500b1d8e8ef31e21c99d1db9a6444d3adf1270") }
    fn usdc() -> Address { address!("2791bca1f2de4661ed88a30c99a7a9449aa84174") }
    fn sender() -> Address { address!("1111111111111111111111111111111111111111") }

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
        ExecutedLog { address: pool, topics: vec![*V3_MINT_TOPIC, B256::ZERO, B256::ZERO], data: data.into() }
    }

    fn v3_swap_log(pool: Address) -> ExecutedLog {
        let mut data = Vec::with_capacity(160);
        data.extend_from_slice(&[0u8; 32]);
        data.extend_from_slice(&[0u8; 32]);
        let sqrt = U256::from(1u128 << 96);
        let mut b = [0u8; 32];
        b.copy_from_slice(&sqrt.to_be_bytes::<32>());
        data.extend_from_slice(&b);
        b = [0u8; 32];
        b[16..32].copy_from_slice(&1_000_000u128.to_be_bytes());
        data.extend_from_slice(&b);
        b = [0u8; 32];
        b[28..32].copy_from_slice(&0i32.to_be_bytes());
        data.extend_from_slice(&b);
        ExecutedLog { address: pool, topics: vec![V3_SWAP_TOPIC, B256::ZERO, B256::ZERO], data: data.into() }
    }

    fn make_pm() -> PoolManager {
        let mut pm = PoolManager::new();
        pm.add_pool(PoolState::UniswapV2(UniswapV2PoolState {
            info: PoolInfo {
                address: pool_p(), token0: wmatic(), token1: usdc(), fee: 30, name: None,
                dex_type: crate::pool::dex_type::DexType::UniswapV2, tick_spacing: None,
                creation_block: 0,
            },
            reserve0: 1_000_000, reserve1: 1_000_000,
        }));
        pm.add_pool(PoolState::UniswapV2(UniswapV2PoolState {
            info: PoolInfo {
                address: pool_q(), token0: usdc(),
                token1: address!("c2132d05d31c914a87c6611c10748aeb04b58e8f"),
                fee: 30, name: None,
                dex_type: crate::pool::dex_type::DexType::UniswapV2, tick_spacing: None,
                creation_block: 0,
            },
            reserve0: 1_000_000, reserve1: 1_000_000,
        }));
        pm
    }

    fn gas_cfg() -> GasConfig { GasConfig::default() }

    #[test]
    fn test_empty_detector_returns_nothing() {
        let mut detector = JitArbDetector::new(1);
        let pm = PoolManager::new();
        let opps = detector.detect(100, &pm, 50_000_000_000, &gas_cfg());
        assert!(opps.is_empty());
    }

    #[test]
    fn test_mint_and_arb_same_tx() {
        let mut detector = JitArbDetector::new(1);
        let pm = make_pm();
        detector.process_tx(0, &[
            v3_mint_log(pool_p(), -100, 100, 500_000),
            v3_swap_log(pool_p()),
            v3_swap_log(pool_q()),
        ], Some(sender()), &pm);
        let opps = detector.detect(100, &pm, 50_000_000_000, &gas_cfg());
        assert_eq!(opps.len(), 1, "Same-tx Mint+arb should be detected");
        assert_eq!(opps[0].strategy, Strategy::JitArb);
        assert_eq!(opps[0].pool_a, pool_p());
        assert_eq!(opps[0].pool_b, pool_q());
        assert_eq!(opps[0].liquidity_amount, Some(500_000));
        assert!(opps[0].gas_cost_wei > 0, "Gas cost should be > 0");
    }

    #[test]
    fn test_mint_then_arb_cross_tx() {
        let mut detector = JitArbDetector::new(1);
        let pm = make_pm();
        detector.process_tx(0, &[v3_mint_log(pool_p(), -100, 100, 500_000)], Some(sender()), &pm);
        assert!(detector.detect(100, &pm, 50_000_000_000, &gas_cfg()).is_empty(), "Mint alone should not trigger JitArb");
        detector.process_tx(1, &[v3_swap_log(pool_p()), v3_swap_log(pool_q())], Some(sender()), &pm);
        let opps = detector.detect(100, &pm, 50_000_000_000, &gas_cfg());
        assert_eq!(opps.len(), 1, "Cross-tx Mint+arb should be detected");
    }

    #[test]
    fn test_different_sender_no_detection() {
        let mut detector = JitArbDetector::new(1);
        let pm = make_pm();
        let other = address!("2222222222222222222222222222222222222222");
        detector.process_tx(0, &[v3_mint_log(pool_p(), -100, 100, 500_000)], Some(sender()), &pm);
        detector.process_tx(1, &[v3_swap_log(pool_p()), v3_swap_log(pool_q())], Some(other), &pm);
        let opps = detector.detect(100, &pm, 50_000_000_000, &gas_cfg());
        assert!(opps.is_empty(), "Different sender should not trigger JitArb");
    }

    #[test]
    fn test_no_token_share_no_detection() {
        let mut detector = JitArbDetector::new(1);
        let pm = {
            let mut pm = make_pm();
            let pool_r = address!("cccccccccccccccccccccccccccccccccccccccc");
            pm.add_pool(PoolState::UniswapV2(UniswapV2PoolState {
                info: PoolInfo {
                    address: pool_r,
                    token0: address!("8f3cf7ad23cd3cadbd9735aff958023239c6a063"),
                    token1: address!("53e0bca35ec356bd5dddfebbd1fc0fd03fabad39"),
                    fee: 30, name: None,
                    dex_type: crate::pool::dex_type::DexType::UniswapV2, tick_spacing: None,
                    creation_block: 0,
                },
                reserve0: 1_000_000, reserve1: 1_000_000,
            }));
            pm
        };
        detector.process_tx(0, &[
            v3_mint_log(pool_p(), -100, 100, 500_000),
            v3_swap_log(pool_p()),
            v3_swap_log(address!("cccccccccccccccccccccccccccccccccccccccc")),
        ], Some(sender()), &pm);
        let opps = detector.detect(100, &pm, 50_000_000_000, &gas_cfg());
        assert!(opps.is_empty(), "No token sharing should not trigger JitArb");
    }

    #[test]
    fn test_no_duplicate_emission() {
        let mut detector = JitArbDetector::new(1);
        let pm = make_pm();
        detector.process_tx(0, &[
            v3_mint_log(pool_p(), -100, 100, 500_000),
            v3_swap_log(pool_p()),
            v3_swap_log(pool_q()),
        ], Some(sender()), &pm);
        assert_eq!(detector.detect(100, &pm, 50_000_000_000, &gas_cfg()).len(), 1);
        assert!(detector.detect(100, &pm, 50_000_000_000, &gas_cfg()).is_empty(), "Should not re-emit");
    }

    #[test]
    fn test_jitarb_proximity_window_2() {
        let mut detector = JitArbDetector::new(1).with_proximity_window(3);
        let pm = make_pm();
        detector.process_tx(0, &[v3_mint_log(pool_p(), -100, 100, 500_000)], Some(sender()), &pm);
        // Swaps at tx 1 and 4 — gap of 3, within window of 3
        detector.process_tx(1, &[v3_swap_log(pool_p())], Some(sender()), &pm);
        detector.process_tx(4, &[v3_swap_log(pool_q())], Some(sender()), &pm);
        let opps = detector.detect(100, &pm, 50_000_000_000, &gas_cfg());
        assert_eq!(opps.len(), 1, "Should detect with window=3");

        // With window=1, gap of 3 should NOT be detected
        let mut detector2 = JitArbDetector::new(1).with_proximity_window(1);
        detector2.process_tx(0, &[v3_mint_log(pool_p(), -100, 100, 500_000)], Some(sender()), &pm);
        detector2.process_tx(1, &[v3_swap_log(pool_p())], Some(sender()), &pm);
        detector2.process_tx(4, &[v3_swap_log(pool_q())], Some(sender()), &pm);
        assert!(detector2.detect(100, &pm, 50_000_000_000, &gas_cfg()).is_empty(), "Should NOT detect with window=1");
    }

    #[test]
    fn test_gas_cost_computed() {
        let mut detector = JitArbDetector::new(1);
        let pm = make_pm();
        detector.process_tx(0, &[
            v3_mint_log(pool_p(), -100, 100, 500_000),
            v3_swap_log(pool_p()),
            v3_swap_log(pool_q()),
        ], Some(sender()), &pm);
        let opps = detector.detect(100, &pm, 50_000_000_000, &gas_cfg());
        assert_eq!(opps.len(), 1);
        assert!(opps[0].gas_cost_wei > 0);
    }
}
