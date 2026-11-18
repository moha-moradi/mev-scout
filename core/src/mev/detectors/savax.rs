//! Benqi sAVAX exchange-rate arbitrage (explorer tag `savax_rate_arb`).
//!
//! Two directions:
//! * Redeem: buy sAVAX in an AMM below the protocol redemption rate, redeem.
//! * Mint: mint sAVAX at the protocol rate, sell above it in an AMM.
//!
//! The rate is fetched once per pass by `LiveContext`; no rate ⇒ no emission.
use alloy::primitives::{Address, U256};

use crate::mev::detectors::tagging::ArbTagPolicy;
use crate::pool::state::{PoolManager, PoolState};
use crate::types::opportunity::{compute_savax_canonical_id, SavaxDirection, SavaxRoute};
use crate::types::{tags, GasConfig, MevOpportunity, Strategy};

const SAVAX_GAS_LIMIT: u64 = 250_000;
const MIN_OUT_BPS: u128 = 9_990;

/// Scan every pool holding both sAVAX and wrapped native once per block.
pub fn detect_block(
    pm: &PoolManager,
    block_number: u64,
    tx_index: usize,
    timestamp: u64,
    base_fee_per_gas: u128,
    gas_config: GasConfig,
    policy: &ArbTagPolicy,
) -> Vec<MevOpportunity> {
    let (Some(savax), Some(rate)) = (policy.savax, policy.savax_rate_wad) else {
        return Vec::new();
    };
    let wrapped = policy.wrapped_native;
    if savax.is_zero() || wrapped.is_zero() || rate.is_zero() || savax == wrapped {
        return Vec::new();
    }
    let gas_cost_wei = gas_config.compute_gas_cost_with_limit(SAVAX_GAS_LIMIT, base_fee_per_gas);
    let mut out = Vec::new();
    for pool_addr in pm.pool_addresses() {
        let Some(state) = pm.get(&pool_addr) else {
            continue;
        };
        if !holds_both(state, savax, wrapped) {
            continue;
        }
        if let Some(opp) = detect_pool(
            state,
            pool_addr,
            block_number,
            tx_index,
            timestamp,
            gas_cost_wei,
            savax,
            wrapped,
            rate,
        ) {
            out.push(opp);
        }
    }
    out
}

fn holds_both(state: &PoolState, savax: Address, wrapped: Address) -> bool {
    let info = state.info();
    let mut tokens = vec![info.token0, info.token1];
    if let Some(underlying) = &info.underlying_tokens {
        tokens.extend(underlying.iter().copied());
    }
    tokens.contains(&savax) && tokens.contains(&wrapped)
}

#[allow(clippy::too_many_arguments)]
fn detect_pool(
    state: &PoolState,
    pool: Address,
    block_number: u64,
    tx_index: usize,
    timestamp: u64,
    gas_cost_wei: u128,
    savax: Address,
    wrapped: Address,
    rate: U256,
) -> Option<MevOpportunity> {
    let max_input = state.max_cycle_input() / 4;
    if max_input == 0 {
        return None;
    }
    let one = U256::from(10u64.pow(18));
    let gas = gas_cost_wei as i128;

    let redeem = best_input(max_input, |x| {
        let shares = state.quote_dir(x, wrapped, savax)?;
        let native = U256::from(shares).saturating_mul(rate) / one;
        let native = u128_of(native)?;
        Some(native as i128 - x as i128)
    });
    let mint = best_input(max_input, |x| {
        let shares = U256::from(x).saturating_mul(one) / rate;
        let shares = u128_of(shares)?;
        if shares == 0 {
            return None;
        }
        let out = state.quote_dir(shares, savax, wrapped)?;
        Some(out as i128 - x as i128)
    });

    let (direction, input, profit) = match (redeem, mint) {
        (Some((x, p)), Some((x2, p2))) => {
            if p >= p2 {
                (SavaxDirection::Redeem, x, p)
            } else {
                (SavaxDirection::Mint, x2, p2)
            }
        }
        (Some((x, p)), None) => (SavaxDirection::Redeem, x, p),
        (None, Some((x, p))) => (SavaxDirection::Mint, x, p),
        (None, None) => return None,
    };
    if profit <= gas {
        return None;
    }

    let (route_tokens, shares, min_out_wad) = match direction {
        SavaxDirection::Redeem => {
            let shares = state.quote_dir(input, wrapped, savax)?;
            let native = U256::from(shares).saturating_mul(rate) / one;
            (
                vec![wrapped, savax],
                U256::from(shares),
                native.saturating_mul(U256::from(MIN_OUT_BPS)) / U256::from(10_000u64),
            )
        }
        SavaxDirection::Mint => {
            let shares = U256::from(input).saturating_mul(one) / rate;
            (
                vec![savax, wrapped],
                shares,
                shares.saturating_mul(U256::from(MIN_OUT_BPS)) / U256::from(10_000u64),
            )
        }
    };

    let mut opp = MevOpportunity::new(
        block_number,
        tx_index,
        Strategy::MultiHopArb,
        pool,
        timestamp,
    );
    opp.pool_b = Address::ZERO;
    opp.token_in = wrapped;
    opp.token_out = wrapped;
    opp.input_amount = U256::from(input);
    opp.expected_profit = U256::from(profit as u128);
    opp.gas_cost_wei = gas_cost_wei;
    opp.path = Some(vec![pool]);
    opp.route_tokens = Some(route_tokens.clone());
    opp.savax_route = Some(SavaxRoute {
        pool,
        direction,
        rate_wad: rate,
        shares,
        min_out_wad,
    });
    opp.canonical_id = Some(compute_savax_canonical_id(pool, direction));
    opp.add_tag(tags::SAVAX_RATE_ARB);
    opp.details = Some(serde_json::json!({
        "savax_rate_wad": rate.to_string(),
        "savax_direction": direction.as_str(),
        "savax_route_tokens": route_tokens.iter().map(|a| format!("{a:#x}")).collect::<Vec<_>>(),
    }));
    Some(opp)
}

/// Concave profit search over `[1, hi]`. Returns the best (input, profit).
fn best_input(hi: u128, f: impl Fn(u128) -> Option<i128>) -> Option<(u128, i128)> {
    if hi < 2 {
        return f(hi).map(|p| (hi, p));
    }
    let mut lo = 1u128;
    let mut high = hi;
    for _ in 0..100 {
        if high <= lo + 2 {
            break;
        }
        let m1 = lo + (high - lo) / 3;
        let m2 = high - (high - lo) / 3;
        let p1 = f(m1).unwrap_or(i128::MIN);
        let p2 = f(m2).unwrap_or(i128::MIN);
        if p1 < p2 {
            lo = m1 + 1;
        } else {
            high = m2.saturating_sub(1);
        }
    }
    (lo..=high)
        .filter_map(|x| f(x).map(|p| (x, p)))
        .max_by_key(|(_, p)| *p)
}

fn u128_of(v: U256) -> Option<u128> {
    (v <= U256::from(u128::MAX)).then(|| v.to::<u128>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dex_type::DexType;
    use crate::pool::state::{PoolInfo, UniswapV2PoolState};
    use crate::types::{FlashLoanProvider, GasModel};
    use alloy::primitives::address;

    const WAVAX: Address = address!("b31f66aa3c1e785363f0875a1b74e27b85fd66c7");
    const SAVAX: Address = address!("2b2c81e08f1af8835a78bb2a90ae924ace0ea4be");
    const POOL: Address = address!("3000000000000000000000000000000000000003");

    fn gas() -> GasConfig {
        GasConfig {
            gas_limit: 2_500_000,
            gas_model: GasModel::Fixed,
            priority_fee_gwei: 0.0,
            flash_loan_provider: FlashLoanProvider::Balancer,
            winning_bid_premium: 0.0,
            percentile_gas_price: None,
            calibration: Default::default(),
        }
    }

    fn pm(reserve_savax: u128, reserve_wavax: u128) -> PoolManager {
        let mut pm = PoolManager::new();
        pm.add_pool(PoolState::UniswapV2(UniswapV2PoolState {
            info: PoolInfo {
                address: POOL,
                token0: SAVAX,
                token1: WAVAX,
                fee: 30,
                dex_type: DexType::UniswapV2,
                ..Default::default()
            },
            reserve0: reserve_savax,
            reserve1: reserve_wavax,
        }));
        pm
    }

    fn policy(rate: U256) -> ArbTagPolicy {
        ArbTagPolicy {
            wrapped_native: WAVAX,
            savax: Some(SAVAX),
            savax_rate_wad: Some(rate),
            ..Default::default()
        }
    }

    #[test]
    fn expensive_savax_pool_triggers_mint() {
        let e18 = 10u128.pow(18);
        let pm = pm(1_000 * e18, 2_000 * e18);
        let opps = detect_block(&pm, 1, 0, 0, 1, gas(), &policy(U256::from(e18)));
        assert_eq!(opps.len(), 1);
        let opp = &opps[0];
        assert!(opp.has_tag(tags::SAVAX_RATE_ARB));
        assert_eq!(
            opp.savax_route.as_ref().map(|r| r.direction),
            Some(SavaxDirection::Mint)
        );
        assert!(opp.expected_profit > U256::ZERO);
    }

    #[test]
    fn cheap_savax_pool_triggers_redeem() {
        let e18 = 10u128.pow(18);
        let pm = pm(2_000 * e18, 1_000 * e18);
        let opps = detect_block(&pm, 1, 0, 0, 1, gas(), &policy(U256::from(e18)));
        assert_eq!(opps.len(), 1);
        assert_eq!(
            opps[0].savax_route.as_ref().map(|r| r.direction),
            Some(SavaxDirection::Redeem)
        );
    }

    #[test]
    fn no_rate_means_no_emission() {
        let e18 = 10u128.pow(18);
        let pm = pm(1_000 * e18, 2_000 * e18);
        let opps = detect_block(&pm, 1, 0, 0, 1, gas(), &policy(U256::ZERO));
        assert!(opps.is_empty());
    }
}
