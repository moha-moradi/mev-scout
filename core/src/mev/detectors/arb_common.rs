//! Shared arbitrage-detection helpers used by both front-ends: the analytical
//! [`super::two_hop`] detector and the numeric [`super::multi_hop`] detector.
//!
//! Unifies the duplicated opportunity builder, profit normalization (C5),
//! slippage evaluation, dominant-DEX gas blend (H7), per-block dedup gate and
//! monotone-quote inversion — so a profit-normalization bug stays in one place
//! instead of two.

use std::collections::HashMap;

use alloy::primitives::{Address, U256};

use crate::dex_type::DexType;
use crate::pool::state::{check_dedup_key, PoolManager};
use crate::types::gas::GasCalibrationSnapshot;
use crate::types::{MevOpportunity, Strategy};

/// Percentage multipliers for the ±1%/±2% slippage evaluation.
const PCT_P1: u128 = 101;
const PCT_M1: u128 = 99;
const PCT_P2: u128 = 102;
const PCT_M2: u128 = 98;

/// Profits at ±1%/±2% slippage around the optimal input.
pub(super) struct SlippageProfits {
    pub(super) p1: Option<U256>,
    pub(super) m1: Option<U256>,
    pub(super) p2: Option<U256>,
    pub(super) m2: Option<U256>,
}

/// Evaluates the profit function at ±1%/±2% of `input`. Any single evaluation
/// returning `None` is reported as `None` (unprofitable/unknowable at that size).
pub(super) fn slippage_profits(
    input: u128,
    eval: impl Fn(u128) -> Option<U256>,
) -> SlippageProfits {
    if input == 0 {
        return SlippageProfits {
            p1: None,
            m1: None,
            p2: None,
            m2: None,
        };
    }
    SlippageProfits {
        p1: eval(input.saturating_mul(PCT_P1) / 100),
        m1: eval(input.saturating_mul(PCT_M1) / 100),
        p2: eval(input.saturating_mul(PCT_P2) / 100),
        m2: eval(input.saturating_mul(PCT_M2) / 100),
    }
}

/// Normalize an arbitrage profit to wrapped native when `token_in != token_out`,
/// falling back to `output_native - input_native` when direct normalization is
/// unavailable (C5). `token_in == token_out` is returned as-is (no raw variant).
pub(super) fn normalize_profit(
    pm: &PoolManager,
    token_in: Address,
    token_out: Address,
    profit: u128,
    input_amount: u128,
) -> (U256, Option<U256>) {
    if token_in == token_out {
        (U256::from(profit), None)
    } else {
        let raw = U256::from(profit);
        let native_profit = pm
            .normalize_to_native(token_out, profit)
            .or_else(|| {
                let total_output = input_amount.saturating_add(profit);
                let native_in = pm.normalize_to_native(token_in, input_amount)?;
                let native_out = pm.normalize_to_native(token_out, total_output)?;
                native_out.checked_sub(native_in)
            })
            .unwrap_or(profit);
        (U256::from(native_profit), Some(raw))
    }
}

/// Fee-on-transfer filter: quotes assume the full output is received, but
/// sell-tax tokens take a cut on transfer — producing phantom opportunities.
pub(super) fn is_fot_pair(pm: &PoolManager, token_in: Address, token_out: Address) -> bool {
    pm.is_taxed_token(&token_in) || pm.is_taxed_token(&token_out)
}

/// Per-block dedup gate: returns `true` when the opportunity should be emitted.
/// The same `(pool_a, pool_b, token_in, token_out)` key is emitted at most once
/// per block unless pool reserves shift by >0.1% (H2), which clears the gate.
pub(super) fn dedup_arb(
    seen: &mut HashMap<(Address, Address, Address, Address), (u128, u128)>,
    pm: &PoolManager,
    opp: &MevOpportunity,
) -> bool {
    let key = (opp.pool_a, opp.pool_b, opp.token_in, opp.token_out);
    check_dedup_key(seen, &key, pm, opp.pool_a, opp.pool_b)
}

/// Most frequent DEX type among the participating pools (tie → UniswapV2).
pub(super) fn dominant_dex_type(counts: &HashMap<DexType, usize>) -> DexType {
    counts
        .iter()
        .max_by_key(|(_, &c)| c)
        .map(|(&d, _)| d)
        .unwrap_or(DexType::UniswapV2)
}

/// Blend a structural gas estimate with the calibrated observation for the
/// dominant DEX shape when enough samples are available (H7).
pub(super) fn blend_gas_limit(
    calibration: &GasCalibrationSnapshot,
    dex_counts: &HashMap<DexType, usize>,
    hop_count: usize,
    analytic: u64,
) -> u64 {
    calibration.blended_gas_limit(dominant_dex_type(dex_counts), hop_count, analytic)
}

/// Shared arbitrary-arbitrage opportunity fields; the front-ends supply only
/// their differentiating values (strategy, address endpoints, path).
pub(super) struct ArbOpportunityInput {
    pub(super) strategy: Strategy,
    pub(super) block_number: u64,
    pub(super) tx_index: usize,
    pub(super) timestamp: u64,
    pub(super) pool_a: Address,
    pub(super) pool_b: Address,
    pub(super) token_in: Address,
    pub(super) token_out: Address,
    pub(super) input_amount: u128,
    pub(super) expected_profit: U256,
    pub(super) raw_profit: Option<U256>,
    pub(super) slippage: SlippageProfits,
    pub(super) gas_cost_wei: u128,
    pub(super) path: Option<Vec<Address>>,
}

/// Build a fully-populated `MevOpportunity` from the shared arb fields.
pub(super) fn build_arb_opportunity(input: ArbOpportunityInput) -> MevOpportunity {
    MevOpportunity {
        canonical_id: None,
        block_number: input.block_number,
        tx_index: input.tx_index,
        strategy: input.strategy,
        pool_a: input.pool_a,
        pool_b: input.pool_b,
        token_in: input.token_in,
        token_out: input.token_out,
        input_amount: U256::from(input.input_amount),
        expected_profit: input.expected_profit,
        raw_profit: input.raw_profit,
        profit_slippage_p1: input.slippage.p1,
        profit_slippage_m1: input.slippage.m1,
        profit_slippage_p2: input.slippage.p2,
        profit_slippage_m2: input.slippage.m2,
        gas_cost_wei: input.gas_cost_wei,
        timestamp: input.timestamp,
        path: input.path,
        tick_lower: None,
        tick_upper: None,
        liquidity_amount: None,
        victim_tx_index: None,
        backrun_tx_index: None,
        mempool_only: false,
        confidence: None,
        sender: None,
        tx_hash: None,
        detection_path: Some(super::REPLAY_PATH.to_string()),
    }
}

/// Smallest x in [1, max_input] whose monotone-increasing quote reaches `target`.
pub(super) fn invert_monotone_quote(
    quote: &impl Fn(u128) -> Option<u128>,
    target: u128,
    max_input: u128,
) -> Option<u128> {
    if target == 0 {
        return None;
    }
    if quote(max_input)? < target {
        return None;
    }
    let mut lo = 1u128;
    let mut hi = max_input;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if quote(mid).unwrap_or(0) >= target {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    Some(lo)
}
