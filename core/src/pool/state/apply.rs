use alloy::primitives::{b256, Address, B256, U256};
use crate::data::ExecutedLog;
use crate::pool::decoders;
use crate::pool::state::manager::PoolManager;
use crate::pool::state::pool_types::PoolState;
use crate::utils::u128_from_be_bytes;

/// Event signature for Uniswap V2 Swap event
pub(crate) const SWAP_TOPIC: B256 = b256!("d78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822");
/// Event signature for Uniswap V2 Sync event
pub(crate) const SYNC_TOPIC: B256 = b256!("1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1");

impl PoolManager {
    /// Update a V2 pool's reserves using amounts from a Swap event.
    pub fn apply_v2_swap(
        &mut self,
        address: &Address,
        amount0_in: u128,
        amount1_in: u128,
        amount0_out: u128,
        amount1_out: u128,
    ) {
        if let Some(PoolState::UniswapV2(state)) = self.pools.get_mut(address) {
            state.reserve0 = state.reserve0.wrapping_add(amount0_in).wrapping_sub(amount0_out);
            state.reserve1 = state.reserve1.wrapping_add(amount1_in).wrapping_sub(amount1_out);
        }
    }

    /// Update a V2 pool's reserves from a Sync event (authoritative override).
    pub fn apply_v2_sync(&mut self, address: &Address, reserve0: u128, reserve1: u128) {
        if let Some(PoolState::UniswapV2(state)) = self.pools.get_mut(address) {
            state.reserve0 = reserve0;
            state.reserve1 = reserve1;
        }
    }

    /// Update a V3 pool's state from a Swap event.
    pub fn apply_v3_swap(
        &mut self,
        address: &Address,
        sqrt_price_x96: U256,
        tick: i32,
        liquidity: u128,
        amount0: i128,
        amount1: i128,
    ) {
        if let Some(PoolState::UniswapV3(state)) = self.pools.get_mut(address) {
            state.sqrt_price_x96 = sqrt_price_x96;
            state.tick = tick;
            state.liquidity = liquidity;

            // Update fee growth global based on swap amounts.
            // The negative amount is the input (trader sends to pool).
            // Fee = input * fee_tier / 1_000_000, added to feeGrowthGlobal.
            let fee_tier = state.info.fee as u128;
            if amount0 < 0 {
                let input = amount0.unsigned_abs();
                let fee = input.saturating_mul(fee_tier) / 1_000_000u128;
                if fee > 0 && liquidity > 0 {
                    let inc = (U256::from(fee) << 128) / U256::from(liquidity);
                    state.fee_growth_global_0_x128 = state.fee_growth_global_0_x128.saturating_add(inc);
                }
            }
            if amount1 < 0 {
                let input = amount1.unsigned_abs();
                let fee = input.saturating_mul(fee_tier) / 1_000_000u128;
                if fee > 0 && liquidity > 0 {
                    let inc = (U256::from(fee) << 128) / U256::from(liquidity);
                    state.fee_growth_global_1_x128 = state.fee_growth_global_1_x128.saturating_add(inc);
                }
            }
        }
    }

    /// Update a V3 pool's tick liquidity from a Mint or Burn event.
    pub fn apply_v3_mint_burn(
        &mut self,
        address: &Address,
        tick_lower: i32,
        tick_upper: i32,
        amount: i128,
    ) {
        if let Some(PoolState::UniswapV3(state)) = self.pools.get_mut(address) {
            *state.ticks.entry(tick_lower).or_insert(0) += amount;
            *state.ticks.entry(tick_upper).or_insert(0) -= amount;
            if amount > 0 {
                state.liquidity = state.liquidity.saturating_add(amount as u128);
            } else {
                state.liquidity = state.liquidity.saturating_sub((-amount) as u128);
            }
        }
    }

    /// Process a list of executed logs from a single transaction, updating pool state
    /// for any Swap or Sync events emitted by tracked pools.
    pub fn update_from_logs(&mut self, logs: &[ExecutedLog]) {
        for log in logs {
            if log.topics.is_empty() {
                continue;
            }
            if !self.known_set.contains(&log.address) {
                continue;
            }
            let topic0 = log.topics[0];

            // V2 Swap
            if topic0 == SWAP_TOPIC {
                self.process_v2_swap_log(log);
                continue;
            }
            // V2 Sync
            if topic0 == SYNC_TOPIC {
                self.process_v2_sync_log(log);
                continue;
            }
            // V3 Swap
            if topic0 == decoders::V3_SWAP_TOPIC {
                self.process_v3_swap_log(log);
                continue;
            }
            // V3 Mint/Burn
            if topic0 == *decoders::V3_MINT_TOPIC || topic0 == decoders::V3_BURN_TOPIC {
                self.process_v3_mint_burn_log(log);
                continue;
            }
            // Curve TokenExchange
            if topic0 == *decoders::CURVE_TOKEN_EXCHANGE_TOPIC
                || topic0 == *decoders::CURVE_V2_TOKEN_EXCHANGE_TOPIC
            {
                self.process_curve_swap_log(log);
                continue;
            }
            // Balancer Swap
            if topic0 == *decoders::BALANCER_SWAP_TOPIC {
                self.process_balancer_swap_log(log);
                continue;
            }
        }
    }

    fn process_v2_swap_log(&mut self, log: &ExecutedLog) {
        if !self.pools.contains_key(&log.address) {
            return;
        }
        if log.data.len() < 128 {
            return;
        }
        let amt0_in = u128_from_be_bytes(&log.data[..32]);
        let amt1_in = u128_from_be_bytes(&log.data[32..64]);
        let amt0_out = u128_from_be_bytes(&log.data[64..96]);
        let amt1_out = u128_from_be_bytes(&log.data[96..128]);
        self.apply_v2_swap(&log.address, amt0_in, amt1_in, amt0_out, amt1_out);
    }

    fn process_v2_sync_log(&mut self, log: &ExecutedLog) {
        if !self.pools.contains_key(&log.address) {
            return;
        }
        if log.data.len() < 64 {
            return;
        }
        let r0 = u128_from_be_bytes(&log.data[..32]);
        let r1 = u128_from_be_bytes(&log.data[32..64]);
        self.apply_v2_sync(&log.address, r0, r1);
    }

    fn process_v3_swap_log(&mut self, log: &ExecutedLog) {
        if !self.pools.contains_key(&log.address) {
            return;
        }
        if let Some(decoded) = decoders::decode_v3_swap(log) {
            self.apply_v3_swap(
                &log.address,
                decoded.sqrt_price_x96,
                decoded.tick,
                decoded.liquidity,
                decoded.amount0,
                decoded.amount1,
            );
        }
    }

    fn process_v3_mint_burn_log(&mut self, log: &ExecutedLog) {
        if !self.pools.contains_key(&log.address) {
            return;
        }
        if let Some(decoded) = decoders::decode_v3_mint_burn(log) {
            self.apply_v3_mint_burn(
                &log.address,
                decoded.tick_lower,
                decoded.tick_upper,
                decoded.amount,
            );
        }
    }

    fn process_curve_swap_log(&mut self, log: &ExecutedLog) {
        if !self.pools.contains_key(&log.address) {
            return;
        }
        if let Some(decoded) = decoders::decode_curve_swap(log) {
            if let Some(PoolState::Curve(state)) = self.pools.get_mut(&log.address) {
                let coin_sold = decoded.coin_sold as usize;
                let coin_bought = decoded.coin_bought as usize;
                if coin_sold < state.balances.len() && coin_bought < state.balances.len() {
                    state.balances[coin_sold] =
                        state.balances[coin_sold].saturating_add(decoded.amount_sold);
                    state.balances[coin_bought] =
                        state.balances[coin_bought].saturating_sub(decoded.amount_bought);
                }
            }
        }
    }

    fn process_balancer_swap_log(&mut self, log: &ExecutedLog) {
        if !self.pools.contains_key(&log.address) {
            return;
        }
        if let Some(decoded) = decoders::decode_balancer_swap(log) {
            if let Some(PoolState::Balancer(state)) = self.pools.get_mut(&log.address) {
                let idx_in = state.token_index.get(&decoded.token_in);
                let idx_out = state.token_index.get(&decoded.token_out);
                if let (Some(&i_in), Some(&i_out)) = (idx_in, idx_out) {
                    if i_in < state.balances.len() && i_out < state.balances.len() {
                        state.balances[i_in] =
                            state.balances[i_in].saturating_add(decoded.amount_in);
                        state.balances[i_out] =
                            state.balances[i_out].saturating_sub(decoded.amount_out);
                    }
                }
            }
        }
    }
}
