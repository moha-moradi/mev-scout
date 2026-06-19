//! JIT (just-in-time) liquidity detection — identifies liquidity added before a swap and removed after.

use std::collections::{HashMap, HashSet};
use alloy::primitives::{Address, U256};
use crate::data::ExecutedLog;
use crate::pool::decoders::{decode_v3_mint_burn, decode_v3_swap, V3_SWAP_TOPIC, V3_MINT_TOPIC, V3_BURN_TOPIC};
use crate::mev::opportunity::MevOpportunity;
use crate::pool::state::PoolManager;
use crate::types::{GasConfig, Strategy};

/// Tracks an active V3 Mint event that hasn't been fully processed.
#[derive(Debug, Clone)]
struct ActiveMint {
    mint_tx_index: usize,
    tick_lower: i32,
    tick_upper: i32,
    amount: u128,
    sender: Option<Address>,
    swapped: bool,
    /// Has the corresponding Burn been seen for this specific position?
    burned: bool,
    /// Cumulative swap volume (absolute token_in amounts) from swaps
    /// that traded within this mint's tick range.
    swap_volume: u128,
    /// Snapshot of feeGrowthGlobal0X128 at mint time for computing actual fees.
    fee_growth_snapshot_0_x128: U256,
    /// Snapshot of feeGrowthGlobal1X128 at mint time.
    fee_growth_snapshot_1_x128: U256,
}

/// Detects Just-In-Time (JIT) liquidity provision on Uniswap V3.
///
/// Stateful per block: accumulates V3 events across sequential txs.
/// After each tx in block order, call `process_tx()` then `detect()`.
///
/// Patterns detected:
/// - **Full JIT:** Mint → Swap → Burn (complete cycle in one block)
/// - **Partial JIT:** Mint → Swap (liquidity deployed, swap traded against it,
///   but no burn detected within the block)
pub struct JitDetector {
    /// Pool address → active mints on that pool
    active_mints: HashMap<Address, Vec<ActiveMint>>,
    /// Track emitted mints by (pool, mint_tx_index, burned) to avoid duplicates
    emitted: HashSet<(Address, usize, bool)>,
    /// Current block number
    block_number: u64,
    /// Last-known tick per V3 pool (pre-swap approximation)
    pool_tick_cache: HashMap<Address, i32>,
}

impl JitDetector {
    pub fn new(block_number: u64) -> Self {
        JitDetector {
            active_mints: HashMap::new(),
            emitted: HashSet::new(),
            block_number,
            pool_tick_cache: HashMap::new(),
        }
    }

    /// Seed the tick cache from the pool manager's current V3 state.
    /// Call after `PoolManager::init_from_rpc` to set initial ticks.
    pub fn seed_pool_tick_cache(&mut self, pool_manager: &PoolManager) {
        for addr in pool_manager.pool_addresses() {
            if let Some(state) = pool_manager.get_v3_state(&addr) {
                self.pool_tick_cache.insert(addr, state.tick);
            }
        }
    }

    /// Process a single transaction's logs and optional sender address.
    /// Call BEFORE `detect()` for each tx in block order.
    /// `pm` is used to snapshot fee growth from V3 pool state at mint time.
    pub fn process_tx(
        &mut self,
        tx_index: usize,
        logs: &[ExecutedLog],
        sender: Option<Address>,
        pm: &PoolManager,
    ) {
        let mut mints_and_burns: Vec<(&ExecutedLog, &str)> = Vec::new();
        let mut swap_decoded: Vec<(&ExecutedLog, i32, u128, u128)> = Vec::new();

        for log in logs {
            if log.topics.is_empty() {
                continue;
            }
            let t0 = log.topics[0];
            if t0 == *V3_MINT_TOPIC || t0 == V3_BURN_TOPIC {
                let kind = if t0 == *V3_MINT_TOPIC { "mint" } else { "burn" };
                mints_and_burns.push((log, kind));
            } else if t0 == V3_SWAP_TOPIC {
                if let Some(decoded) = decode_v3_swap(log) {
                    // Determine absolute swap amount (input = the positive amount)
                    let amount_in = if decoded.amount0 > 0 {
                        decoded.amount0 as u128
                    } else {
                        decoded.amount1 as u128
                    };
                    swap_decoded.push((log, decoded.tick, amount_in, decoded.liquidity));
                }
            }
        }

        // Process Mint/Burn first (state changes)
        for (log, kind) in &mints_and_burns {
            let Some(decoded) = decode_v3_mint_burn(log) else { continue };
            match *kind {
                "mint" => {
                    if decoded.amount > 0 {
                        let (fg0, fg1) = pm.get_v3_state(&log.address)
                            .map(|s| (s.fee_growth_global_0_x128, s.fee_growth_global_1_x128))
                            .unwrap_or((U256::ZERO, U256::ZERO));
                        self.active_mints
                            .entry(log.address)
                            .or_default()
                            .push(ActiveMint {
                                mint_tx_index: tx_index,
                                tick_lower: decoded.tick_lower,
                                tick_upper: decoded.tick_upper,
                                amount: decoded.amount as u128,
                                sender,
                                swapped: false,
                                burned: false,
                                swap_volume: 0,
                                fee_growth_snapshot_0_x128: fg0,
                                fee_growth_snapshot_1_x128: fg1,
                            });
                    }
                }
                _ => {
                    if let Some(mints) = self.active_mints.get_mut(&log.address) {
                        for mint in mints.iter_mut() {
                            if mint.burned { continue; }
                            if let Some(s) = sender {
                                if mint.sender != Some(s) { continue; }
                            }
                            if mint.tick_lower == decoded.tick_lower
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
        }

        // Process swaps with tick-range overlap
        for (log, post_tick, amount_in, _liquidity) in &swap_decoded {
            let pre_tick = self.pool_tick_cache.get(&log.address).copied().unwrap_or(*post_tick);
            if let Some(mints) = self.active_mints.get_mut(&log.address) {
                for mint in mints.iter_mut() {
                    // Check if the swap price crossed this mint's range.
                    // A position is active when the pool's current tick
                    // is within [tick_lower, tick_upper). We check both
                    // the pre-swap and post-swap tick: if either is in range,
                    // the position was touched by this swap.
                    let in_range_pre = pre_tick >= mint.tick_lower && pre_tick < mint.tick_upper;
                    let in_range_post = *post_tick >= mint.tick_lower && *post_tick < mint.tick_upper;
                    if in_range_pre || in_range_post {
                        mint.swapped = true;
                        mint.swap_volume = mint.swap_volume.saturating_add(*amount_in);
                    }
                }
            }
            self.pool_tick_cache.insert(log.address, *post_tick);
        }
    }

    /// Returns new JIT opportunities detected since the last call.
    /// Call AFTER `process_tx()` for each tx.
    pub fn detect(
        &mut self,
        timestamp: u64,
        base_fee_per_gas: u128,
        gas_config: &GasConfig,
        pool_manager: &PoolManager,
    ) -> Vec<MevOpportunity> {
        let mut opportunities = Vec::new();

        let pool_addrs: Vec<Address> = self.active_mints.keys().copied().collect();
        for pool in &pool_addrs {
            let Some(mints) = self.active_mints.get(pool) else { continue };
            let pool_fee = pool_manager.get(pool)
                .map(|p| p.info().fee)
                .unwrap_or(3000);
            for mint in mints {
                let dedup_key = (*pool, mint.mint_tx_index, mint.burned);
                if self.emitted.contains(&dedup_key) {
                    continue;
                }

                // Full JIT: Mint → Swap → Burn
                if mint.swapped && mint.burned {
                    self.emitted.insert(dedup_key);
                    opportunities.push(Self::build_opp(
                        self.block_number, *pool, mint, timestamp, true,
                        base_fee_per_gas, gas_config, pool_fee, pool_manager,
                    ));
                // Partial JIT: Mint → Swap (no burn yet, or no burn in this block)
                } else if mint.swapped && !mint.burned {
                    self.emitted.insert(dedup_key);
                    opportunities.push(Self::build_opp(
                        self.block_number, *pool, mint, timestamp, false,
                        base_fee_per_gas, gas_config, pool_fee, pool_manager,
                    ));
                }
            }
        }

        opportunities
    }

    fn build_opp(
        block_number: u64,
        pool: Address,
        mint: &ActiveMint,
        timestamp: u64,
        _burned: bool,
        base_fee_per_gas: u128,
        gas_config: &GasConfig,
        pool_fee: u32,
        pool_manager: &PoolManager,
    ) -> MevOpportunity {
        let pool_tokens = pool_manager.get(&pool).map(|p| {
            let info = p.info();
            (info.token0, info.token1)
        });
        // Estimate fee revenue earned by the JIT position.
        // Compute both raw fees (dimensionally inconsistent sum of token0+token1)
        // and normalized fees (each fee component converted to wrapped native).
        let (raw_fees, normalized_fees) = 'calc: {
            if let Some(v3) = pool_manager.get_v3_state(&pool) {
                let d0 = v3.fee_growth_global_0_x128.saturating_sub(mint.fee_growth_snapshot_0_x128);
                let d1 = v3.fee_growth_global_1_x128.saturating_sub(mint.fee_growth_snapshot_1_x128);
                if !d0.is_zero() || !d1.is_zero() {
                    let fee0_u256: U256 = U256::from(mint.amount) * d0 >> 128;
                    let fee1_u256: U256 = U256::from(mint.amount) * d1 >> 128;
                    let fee0_raw = fee0_u256.to::<u128>();
                    let fee1_raw = fee1_u256.to::<u128>();
                    let raw_total = fee0_raw.saturating_add(fee1_raw);
                    // Normalize each fee component to native
                    let (t0, t1) = pool_tokens.unwrap_or((Address::ZERO, Address::ZERO));
                    let fee0_native = pool_manager.normalize_to_native(t0, fee0_raw).unwrap_or(fee0_raw);
                    let fee1_native = pool_manager.normalize_to_native(t1, fee1_raw).unwrap_or(fee1_raw);
                    let norm_total = fee0_native.saturating_add(fee1_native);
                    break 'calc (raw_total, norm_total);
                }
            }
            // Volume-based fallback when fee growth deltas are not available
            if pool_fee > 0 && mint.swap_volume > 0 && mint.amount > 0 {
                let pool_liquidity = pool_manager
                    .get_v3_state(&pool)
                    .map(|s| s.liquidity)
                    .unwrap_or(0);
                let raw = if pool_liquidity > 0 && mint.amount < pool_liquidity {
                    (mint.swap_volume as u128)
                        .saturating_mul(pool_fee as u128)
                        .saturating_mul(mint.amount as u128)
                        .saturating_div(1_000_000u128)
                        .saturating_div(pool_liquidity as u128)
                } else {
                    (mint.swap_volume as u128)
                        .saturating_mul(pool_fee as u128)
                        .saturating_div(1_000_000)
                };
                // Normalize fallback estimate using token0 as reference
                let (t0, _) = pool_tokens.unwrap_or((Address::ZERO, Address::ZERO));
                let norm = pool_manager.normalize_to_native(t0, raw).unwrap_or(raw);
                break 'calc (raw, norm);
            }
            (0u128, 0u128)
        };
        // Per-opportunity gas: JIT involves Mint + Swap (+ optionally Burn).
        let pool_gas = pool_manager.get(&pool)
            .map(|p| p.gas_estimate())
            .unwrap_or(60_000);
        let gas_limit = if _burned {
            40_000 + pool_gas + 150_000 + 150_000
        } else {
            40_000 + pool_gas + 150_000
        };
        let gas_cost_wei = gas_config.compute_gas_cost_with_limit(gas_limit, base_fee_per_gas);
        // raw_profit = Some(raw) when normalization actually converted the value
        let raw_profit = if normalized_fees != raw_fees { Some(U256::from(raw_fees)) } else { None };
        MevOpportunity {
            block_number,
            tx_index: mint.mint_tx_index,
            strategy: Strategy::Jit,
            pool_a: pool,
            pool_b: Address::ZERO,
            token_in: pool_tokens.map(|(t0, _)| t0).unwrap_or(Address::ZERO),
            token_out: pool_tokens.map(|(_, t1)| t1).unwrap_or(Address::ZERO),
            input_amount: U256::from(mint.amount),
            expected_profit: U256::from(normalized_fees),
            raw_profit,
            profit_slippage_p1: None,
            profit_slippage_m1: None,
            profit_slippage_p2: None,
            profit_slippage_m2: None,
            pga_adjusted_profit: None,
            gas_cost_wei,
            timestamp,
            path: None,
            tick_lower: Some(mint.tick_lower),
            tick_upper: Some(mint.tick_upper),
            liquidity_amount: Some(mint.amount),
            victim_tx_index: None,
            backrun_tx_index: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, B256};
    use crate::data::ExecutedLog;
    use crate::pool::state::{PoolInfo, PoolState, UniswapV3PoolState};
    use crate::pool::dex_type::DexType;

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

    fn v3_swap_log_with_amounts(pool: Address, amt0: i128, amt1: i128, tick: i32) -> ExecutedLog {
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
        b[16..32].copy_from_slice(&100_000_000u128.to_be_bytes());
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

    fn v3_swap_log(pool: Address) -> ExecutedLog {
        v3_swap_log_with_amounts(pool, 0, 0, 0)
    }

    fn pool_a() -> Address { address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa") }
    fn pool_b() -> Address { address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb") }

    fn make_pm_with_v3_pool(addr: Address, fee: u32) -> PoolManager {
        make_pm_with_v3_pool_and_liquidity(addr, fee, 1_000_000_000)
    }

    fn make_pm_with_v3_pool_and_liquidity(addr: Address, fee: u32, liquidity: u128) -> PoolManager {
        let mut pm = PoolManager::new();
        pm.add_pool(PoolState::UniswapV3(UniswapV3PoolState {
            info: PoolInfo {
                address: addr,
                token0: Address::ZERO,
                token1: Address::ZERO,
                fee,
                name: None,
                dex_type: DexType::UniswapV3,
                tick_spacing: Some(60),
                creation_block: 0,
                pool_id: None,
            },
            sqrt_price_x96: U256::from(1u128 << 96),
            tick: 0,
            liquidity,
            ticks: std::collections::BTreeMap::new(),
            fee_growth_global_0_x128: U256::ZERO,
            fee_growth_global_1_x128: U256::ZERO,
        }));
        pm
    }

    fn gas_cfg() -> GasConfig { GasConfig::default() }

    #[test]
    fn test_empty_detector_returns_nothing() {
        let mut detector = JitDetector::new(1);
        let pm = PoolManager::new();
        let opps = detector.detect(100, 50_000_000_000, &gas_cfg(), &pm);
        assert!(opps.is_empty());
    }

    #[test]
    fn test_mint_swap_burn_detected() {
        let mut detector = JitDetector::new(1);
        let pm = make_pm_with_v3_pool_and_liquidity(pool_a(), 3000, 500_000);

        // Tx 0: Mint on pool A
        detector.process_tx(0, &[v3_mint_log(pool_a(), -100, 100, 500_000)], None, &pm);
        assert!(detector.detect(100, 50_000_000_000, &gas_cfg(), &pm).is_empty(), "Mint alone is not JIT");

        // Tx 1: Swap on pool A
        detector.process_tx(1, &[v3_swap_log_with_amounts(pool_a(), 100_000, -99_000, 0)], None, &pm);
        let mut opps = detector.detect(100, 50_000_000_000, &gas_cfg(), &pm);
        assert_eq!(opps.len(), 1, "Mint+Swap should emit partial JIT");

        let opp = &opps[0];
        assert_eq!(opp.strategy, Strategy::Jit);
        assert_eq!(opp.pool_a, pool_a());
        assert_eq!(opp.tick_lower, Some(-100));
        assert_eq!(opp.tick_upper, Some(100));
        assert_eq!(opp.liquidity_amount, Some(500_000));
        assert!(opp.expected_profit > U256::ZERO, "Profit should be > 0");
        assert!(opp.gas_cost_wei > 0, "Gas cost should be > 0");

        // Tx 2: Burn matching the mint
        detector.process_tx(2, &[v3_burn_log(pool_a(), -100, 100, 500_000)], None, &pm);
        opps = detector.detect(100, 50_000_000_000, &gas_cfg(), &pm);
        assert_eq!(opps.len(), 1, "Burn should emit full JIT");
        assert_eq!(opps[0].tx_index, 0, "Should reference the mint tx index");
    }

    #[test]
    fn test_multiple_pools_independent() {
        let mut detector = JitDetector::new(1);
        let pm = make_pm_with_v3_pool(pool_a(), 3000);

        detector.process_tx(0, &[v3_mint_log(pool_a(), -100, 100, 500_000)], None, &pm);
        detector.process_tx(1, &[v3_mint_log(pool_b(), -200, 200, 1_000_000)], None, &pm);
        detector.process_tx(2, &[v3_swap_log_with_amounts(pool_a(), 100_000, -99_000, 0)], None, &pm);

        let opps = detector.detect(100, 50_000_000_000, &gas_cfg(), &pm);
        assert_eq!(opps.len(), 1);
        assert_eq!(opps[0].pool_a, pool_a());
    }

    #[test]
    fn test_no_duplicate_emission() {
        let mut detector = JitDetector::new(1);
        let pm = make_pm_with_v3_pool(pool_a(), 3000);

        detector.process_tx(0, &[v3_mint_log(pool_a(), -100, 100, 500_000)], None, &pm);
        detector.process_tx(1, &[v3_swap_log_with_amounts(pool_a(), 100_000, -99_000, 0)], None, &pm);

        let opps = detector.detect(100, 50_000_000_000, &gas_cfg(), &pm);
        assert_eq!(opps.len(), 1);

        let opps2 = detector.detect(100, 50_000_000_000, &gas_cfg(), &pm);
        assert!(opps2.is_empty(), "Should not re-emit same opportunity");
    }

    #[test]
    fn test_mint_only_no_detection() {
        let mut detector = JitDetector::new(1);
        let pm = make_pm_with_v3_pool(pool_a(), 3000);
        detector.process_tx(0, &[v3_mint_log(pool_a(), -100, 100, 500_000)], None, &pm);
        let opps = detector.detect(100, 50_000_000_000, &gas_cfg(), &pm);
        assert!(opps.is_empty());
    }

    #[test]
    fn test_swap_burn_without_mint_no_detection() {
        let mut detector = JitDetector::new(1);
        let pm = make_pm_with_v3_pool(pool_a(), 3000);
        detector.process_tx(0, &[v3_burn_log(pool_a(), -100, 100, 500_000)], None, &pm);
        detector.process_tx(1, &[v3_swap_log_with_amounts(pool_a(), 100_000, -99_000, 0)], None, &pm);
        let opps = detector.detect(100, 50_000_000_000, &gas_cfg(), &pm);
        assert!(opps.is_empty());
    }

    #[test]
    fn test_multiple_mints_only_relevant_marked() {
        let mut detector = JitDetector::new(1);
        let _pm = make_pm_with_v3_pool(pool_a(), 3000);

        // Two mints at different tick ranges
        detector.process_tx(0, &[v3_mint_log(pool_a(), -200, -100, 500_000)], None, &_pm);
        detector.process_tx(1, &[v3_mint_log(pool_a(), 100, 200, 500_000)], None, &_pm);

        // Swap at tick 150 — only second mint should be marked
        detector.process_tx(2, &[v3_swap_log_with_amounts(pool_a(), 100_000, -99_000, 150)], None, &_pm);

        let mints = detector.active_mints.get(&pool_a()).unwrap();
        assert!(!mints[0].swapped, "First mint (-200..-100) should not be swapped");
        assert!(mints[1].swapped, "Second mint (100..200) should be swapped");
        assert!(mints[1].swap_volume > 0, "Swap volume should be tracked");
    }

    #[test]
    fn test_swap_out_of_range_no_mark() {
        let mut detector = JitDetector::new(1);
        let _pm = make_pm_with_v3_pool(pool_a(), 3000);

        detector.process_tx(0, &[v3_mint_log(pool_a(), -100, 100, 500_000)], None, &_pm);
        // Swap at tick 500 — far outside range
        detector.process_tx(1, &[v3_swap_log_with_amounts(pool_a(), 100_000, -99_000, 500)], None, &_pm);

        let mints = detector.active_mints.get(&pool_a()).unwrap();
        assert!(!mints[0].swapped, "Mint should not be marked as swapped");
    }

    #[test]
    fn test_burn_different_sender_no_match() {
        let mut detector = JitDetector::new(1);
        let _pm = make_pm_with_v3_pool(pool_a(), 3000);
        let sender1 = address!("1111111111111111111111111111111111111111");
        let sender2 = address!("2222222222222222222222222222222222222222");

        detector.process_tx(0, &[v3_mint_log(pool_a(), -100, 100, 500_000)], Some(sender1), &_pm);
        // Burn from sender2 — should NOT match mint from sender1
        detector.process_tx(1, &[v3_burn_log(pool_a(), -100, 100, 500_000)], Some(sender2), &_pm);

        let mints = detector.active_mints.get(&pool_a()).unwrap();
        assert!(!mints[0].burned, "Burn from different sender should not match");
    }

    #[test]
    fn test_burn_same_sender_matches() {
        let mut detector = JitDetector::new(1);
        let _pm = make_pm_with_v3_pool(pool_a(), 3000);
        let sender1 = address!("1111111111111111111111111111111111111111");

        detector.process_tx(0, &[v3_mint_log(pool_a(), -100, 100, 500_000)], Some(sender1), &_pm);
        // Burn from same sender — should match
        detector.process_tx(1, &[v3_burn_log(pool_a(), -100, 100, 500_000)], Some(sender1), &_pm);

        let mints = detector.active_mints.get(&pool_a()).unwrap();
        assert!(mints[0].burned, "Burn from same sender should match");
    }

    #[test]
    fn test_profit_scales_with_swap_volume_and_fee() {
        let mut detector = JitDetector::new(1);
        // Pool liquidity matches mint amount so the position captures 100% of fees
        let pm = make_pm_with_v3_pool_and_liquidity(pool_a(), 3000, 500_000);

        detector.process_tx(0, &[v3_mint_log(pool_a(), -100, 100, 500_000)], None, &pm);
        // Swap with 1_000_000 volume at 0.3% fee = 3_000 expected profit (full share)
        detector.process_tx(1, &[v3_swap_log_with_amounts(pool_a(), 1_000_000, -997_000, 0)], None, &pm);

        let opps = detector.detect(100, 50_000_000_000, &gas_cfg(), &pm);
        assert_eq!(opps.len(), 1);
        // 1_000_000 * 3000 / 1_000_000 = 3000
        assert_eq!(opps[0].expected_profit, U256::from(3000u64));
    }
}
