//! `MevOpportunity` → flash-loan plan the executor contract can run.
use alloy::primitives::{Address, U256};

use crate::dex_type::DexType;
use crate::paper::sim::pools_of_opp;
use crate::pool::math::fee::FeeTier;
use crate::pool::state::{PoolManager, PoolState};
use crate::types::{FlashLoanProvider, MevOpportunity, Strategy};

/// 0 = UniswapV2-style `swap`. 1 = UniswapV3 exact-input `pool.swap`.
pub const LEG_V2: u8 = 0;
pub const LEG_V3: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecLeg {
    pub pool: Address,
    pub token_in: Address,
    pub token_out: Address,
    pub kind: u8,
    pub fee: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecPlan {
    pub vault: Address,
    pub borrow_token: Address,
    pub amount: U256,
    pub min_profit: U256,
    pub deadline: u64,
    pub legs: Vec<ExecLeg>,
    pub provider: FlashLoanProvider,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanError {
    StrategyUnsupported,
    ProviderUnsupported,
    NoVault,
    ZeroAmount,
    PathUnavailable,
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::StrategyUnsupported => "strategy unsupported",
            Self::ProviderUnsupported => "flash provider unsupported",
            Self::NoVault => "no balancer vault",
            Self::ZeroAmount => "zero borrow",
            Self::PathUnavailable => "path unavailable",
        })
    }
}

/// Build a Balancer flash plan. Only Auto/Balancer and V2/V3-family legs.
pub fn plan_from_opportunity(
    opp: &MevOpportunity,
    pools: &PoolManager,
    vault: Address,
    provider: FlashLoanProvider,
    min_profit: U256,
    deadline: u64,
) -> Result<ExecPlan, PlanError> {
    if !matches!(
        opp.strategy,
        Strategy::TwoHopArb | Strategy::MultiHopArb | Strategy::Backrun
    ) {
        return Err(PlanError::StrategyUnsupported);
    }
    if !matches!(
        provider,
        FlashLoanProvider::Auto | FlashLoanProvider::Balancer
    ) {
        return Err(PlanError::ProviderUnsupported);
    }
    if vault.is_zero() {
        return Err(PlanError::NoVault);
    }
    if opp.input_amount.is_zero() {
        return Err(PlanError::ZeroAmount);
    }

    let path = pools_of_opp(opp);
    if path.is_empty() {
        return Err(PlanError::PathUnavailable);
    }

    let mut token_in = opp.token_in;
    if token_in.is_zero() {
        return Err(PlanError::PathUnavailable);
    }
    let mut legs = Vec::with_capacity(path.len());
    for pool in path {
        let state = pools.get(&pool).ok_or(PlanError::PathUnavailable)?;
        let (kind, fee, token_out) = classify_leg(state, token_in)?;
        legs.push(ExecLeg {
            pool,
            token_in,
            token_out,
            kind,
            fee,
        });
        token_in = token_out;
    }

    Ok(ExecPlan {
        vault,
        borrow_token: opp.token_in,
        amount: opp.input_amount,
        min_profit,
        deadline,
        legs,
        provider,
    })
}

fn classify_leg(state: &PoolState, token_in: Address) -> Result<(u8, u32, Address), PlanError> {
    let info = state.info();
    if info.is_stable == Some(true) {
        return Err(PlanError::PathUnavailable);
    }
    let token_out = other_token(state, token_in).ok_or(PlanError::PathUnavailable)?;
    match state {
        PoolState::UniswapV2(_)
            if matches!(
                info.dex_type,
                DexType::UniswapV2 | DexType::Solidly | DexType::Camelot
            ) =>
        {
            let fee = match info.fee_tier() {
                FeeTier::Bps(b) if (1..10_000).contains(&b) => b,
                FeeTier::Unset | FeeTier::Free => 30,
                _ => return Err(PlanError::PathUnavailable),
            };
            Ok((LEG_V2, fee, token_out))
        }
        PoolState::UniswapV3(_) if info.dex_type == DexType::UniswapV3 => {
            let fee = match info.fee_tier() {
                FeeTier::Ppm(p) if p <= 0x00ff_ffff => p,
                _ => return Err(PlanError::PathUnavailable),
            };
            Ok((LEG_V3, fee, token_out))
        }
        _ => Err(PlanError::PathUnavailable),
    }
}

fn other_token(state: &PoolState, token_in: Address) -> Option<Address> {
    let (t0, t1) = state.token_pair();
    if t0 == token_in {
        Some(t1)
    } else if t1 == token_in {
        Some(t0)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::state::{PoolInfo, UniswapV2PoolState};
    use alloy::primitives::address;

    fn v2(addr: Address, t0: Address, t1: Address) -> PoolState {
        PoolState::UniswapV2(UniswapV2PoolState {
            info: PoolInfo {
                address: addr,
                token0: t0,
                token1: t1,
                fee: 30,
                dex_type: DexType::UniswapV2,
                ..Default::default()
            },
            reserve0: 1,
            reserve1: 1,
        })
    }

    #[test]
    fn two_hop_v2_plan_walks_tokens() {
        let t0 = address!("0x0000000000000000000000000000000000000001");
        let t1 = address!("0x0000000000000000000000000000000000000002");
        let pa = address!("0x00000000000000000000000000000000000000aa");
        let pb = address!("0x00000000000000000000000000000000000000bb");
        let vault = address!("0x00000000000000000000000000000000000000cc");
        let mut pm = PoolManager::new();
        pm.add_pool(v2(pa, t0, t1));
        pm.add_pool(v2(pb, t0, t1));
        let mut opp = MevOpportunity::new(1, 0, Strategy::TwoHopArb, pa, 0);
        opp.pool_b = pb;
        opp.token_in = t0;
        opp.token_out = t0;
        opp.input_amount = U256::from(1_000u64);
        let plan = plan_from_opportunity(
            &opp,
            &pm,
            vault,
            FlashLoanProvider::Balancer,
            U256::from(1u64),
            12,
        )
        .expect("plan");
        assert_eq!(plan.legs.len(), 2);
        assert_eq!(plan.legs[0].token_in, t0);
        assert_eq!(plan.legs[0].token_out, t1);
        assert_eq!(plan.legs[0].kind, LEG_V2);
        assert_eq!(plan.legs[0].fee, 30);
        assert_eq!(plan.legs[1].token_in, t1);
        assert_eq!(plan.legs[1].token_out, t0);
        assert_eq!(plan.borrow_token, t0);
        assert_eq!(plan.min_profit, U256::from(1u64));
        assert_eq!(plan.deadline, 12);
    }

    #[test]
    fn jit_is_unsupported() {
        let mut opp = MevOpportunity::new(1, 0, Strategy::Jit, Address::ZERO, 0);
        opp.input_amount = U256::from(1u64);
        let err = plan_from_opportunity(
            &opp,
            &PoolManager::new(),
            address!("0x0000000000000000000000000000000000000001"),
            FlashLoanProvider::Auto,
            U256::from(1u64),
            12,
        )
        .unwrap_err();
        assert_eq!(err, PlanError::StrategyUnsupported);
    }

    #[test]
    fn curve_leg_is_unavailable() {
        let t0 = address!("0x0000000000000000000000000000000000000001");
        let t1 = address!("0x0000000000000000000000000000000000000002");
        let pa = address!("0x00000000000000000000000000000000000000aa");
        let mut pm = PoolManager::new();
        let mut curve = v2(pa, t0, t1);
        if let PoolState::UniswapV2(state) = &mut curve {
            state.info.dex_type = DexType::Curve;
        }
        pm.add_pool(curve);
        let mut opp = MevOpportunity::new(1, 0, Strategy::TwoHopArb, pa, 0);
        opp.token_in = t0;
        opp.input_amount = U256::from(1u64);
        let err = plan_from_opportunity(
            &opp,
            &pm,
            address!("0x0000000000000000000000000000000000000009"),
            FlashLoanProvider::Auto,
            U256::from(1u64),
            12,
        )
        .unwrap_err();
        assert_eq!(err, PlanError::PathUnavailable);
    }
}
