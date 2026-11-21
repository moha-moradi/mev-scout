//! `MevOpportunity` → flash-loan plan the executor contract can run (ABI v3).
use std::collections::HashMap;

use alloy::primitives::{Address, Bytes, U256};

use crate::dex_type::DexType;
use crate::paper::sim::pools_of_opp;
use crate::pool::math::fee::FeeTier;
use crate::pool::state::{PoolManager, PoolState};
use crate::types::{FlashLoanProvider, MevOpportunity, SavaxDirection, Strategy};

use super::venues;

/// 0 = UniswapV2-style `swap`. 1 = UniswapV3 exact-input `pool.swap`.
pub const LEG_V2: u8 = 0;
pub const LEG_V3: u8 = 1;
/// Generic call with optional ERC-20 approval (`spender`).
pub const LEG_CALL: u8 = 2;
/// Wrap the executor's whole native balance into WETH.
pub const LEG_WRAP_ALL: u8 = 3;
/// Unwrap `amountIn` WETH into native.
pub const LEG_UNWRAP: u8 = 4;
/// Payable call with `amountIn` native value.
pub const LEG_CALL_WITH_VALUE: u8 = 5;
/// Transfer `amountIn` tokenIn to `target`, then call (V2-style pre-transfer).
pub const LEG_CALL_WITH_TRANSFER: u8 = 6;

/// Generic-call legs require a precomputed `minOut`; leave 10 bps of slack for
/// float-based Curve/Balancer quoters.
const MIN_OUT_BPS: u128 = 9_990;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecLeg {
    pub target: Address,
    pub token_in: Address,
    pub token_out: Address,
    pub spender: Address,
    pub kind: u8,
    pub fee: u32,
    pub min_out: U256,
    pub data: Bytes,
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
    /// Venue has no executor leg for this route: no router configured,
    /// or V4/Pancake/Metric/Fluid (explicitly unsupported in this build).
    VenueUnsupported,
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::StrategyUnsupported => "strategy unsupported",
            Self::ProviderUnsupported => "flash provider unsupported",
            Self::NoVault => "no balancer vault",
            Self::ZeroAmount => "zero borrow",
            Self::PathUnavailable => "path unavailable",
            Self::VenueUnsupported => "venue unsupported",
        })
    }
}

/// Router addresses and LB pair-family map for generic venue legs.
///
/// Unset routers turn the venue into an explicit [`PlanError::VenueUnsupported`]
/// (mapped to a counted `ExecVenueUnsupported` skip), never a silent drop.
#[derive(Debug, Clone, Default)]
pub struct VenueRouters {
    pub lb_router_v21: Option<Address>,
    pub lb_router_v22: Option<Address>,
    /// LB factory → `Path.versions` value (2 = V2.1, 3 = V2.2).
    pub lb_factory_versions: HashMap<Address, u8>,
    pub pendle_router: Option<Address>,
}

impl VenueRouters {
    /// Pair family for a pool. Unknown factories default to V2.1, which is the
    /// only family configured by default.
    fn lb_version(&self, factory: Option<Address>) -> u8 {
        factory
            .and_then(|f| self.lb_factory_versions.get(&f).copied())
            .unwrap_or(2)
    }

    fn lb_router(&self, version: u8) -> Option<Address> {
        match version {
            2 => self.lb_router_v21,
            3 => self.lb_router_v22,
            _ => None,
        }
    }
}

/// Build a Balancer flash plan. Supports V2/V3 fast paths and generic
/// Curve/Balancer/Solidly-stable/LB/Pendle/sAVAX legs.
#[allow(clippy::too_many_arguments)]
pub fn plan_from_opportunity(
    opp: &MevOpportunity,
    pools: &PoolManager,
    routers: &VenueRouters,
    vault: Address,
    provider: FlashLoanProvider,
    executor: Address,
    wrapped_native: Address,
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
    let amount = u128_of(opp.input_amount).ok_or(PlanError::ZeroAmount)?;
    if amount == 0 {
        return Err(PlanError::ZeroAmount);
    }

    let path = pools_of_opp(opp);
    if path.is_empty() {
        return Err(PlanError::PathUnavailable);
    }
    let tokens = route_tokens(opp, &path, pools)?;

    let legs = if let Some(savax) = &opp.savax_route {
        plan_savax_legs(
            opp,
            savax,
            &path,
            &tokens,
            amount,
            pools,
            routers,
            vault,
            executor,
            wrapped_native,
            deadline,
        )?
    } else {
        let mut legs = Vec::with_capacity(path.len());
        let mut current = amount;
        for (i, pool_addr) in path.iter().enumerate() {
            let state = pools.get(pool_addr).ok_or(PlanError::PathUnavailable)?;
            let token_in = *tokens.get(i).ok_or(PlanError::PathUnavailable)?;
            let token_out = *tokens.get(i + 1).ok_or(PlanError::PathUnavailable)?;
            let (leg, out) = build_leg(
                state, *pool_addr, token_in, token_out, current, routers, vault, executor, deadline,
            )?;
            legs.push(leg);
            current = out;
        }
        legs
    };

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

#[allow(clippy::too_many_arguments)]
fn plan_savax_legs(
    opp: &MevOpportunity,
    route: &crate::types::SavaxRoute,
    path: &[Address],
    tokens: &[Address],
    amount: u128,
    pools: &PoolManager,
    routers: &VenueRouters,
    vault: Address,
    executor: Address,
    wrapped_native: Address,
    deadline: u64,
) -> Result<Vec<ExecLeg>, PlanError> {
    let state = pools.get(&route.pool).ok_or(PlanError::PathUnavailable)?;
    let (t0, t1) = state.token_pair();
    let savax = if t0 == wrapped_native {
        t1
    } else if t1 == wrapped_native {
        t0
    } else {
        return Err(PlanError::PathUnavailable);
    };
    if route.rate_wad.is_zero() {
        return Err(PlanError::PathUnavailable);
    }
    let rate = route.rate_wad;
    let one = U256::from(10u64.pow(18));
    let mut legs = Vec::new();
    match route.direction {
        SavaxDirection::Redeem => {
            let (pool_leg, shares) = build_leg(
                state,
                route.pool,
                wrapped_native,
                savax,
                amount,
                routers,
                vault,
                executor,
                deadline,
            )?;
            legs.push(pool_leg);
            let shares = U256::from(shares);
            let native_out = shares.saturating_mul(rate) / one;
            legs.push(ExecLeg {
                target: savax,
                token_in: savax,
                token_out: Address::ZERO,
                spender: savax,
                kind: LEG_CALL,
                fee: 0,
                min_out: min_out(native_out),
                data: venues::savax_redeem(shares),
            });
            legs.push(ExecLeg {
                target: Address::ZERO,
                token_in: Address::ZERO,
                token_out: wrapped_native,
                spender: Address::ZERO,
                kind: LEG_WRAP_ALL,
                fee: 0,
                min_out: min_out(native_out),
                data: Bytes::new(),
            });
        }
        SavaxDirection::Mint => {
            let shares = U256::from(amount).saturating_mul(one) / rate;
            legs.push(ExecLeg {
                target: Address::ZERO,
                token_in: wrapped_native,
                token_out: Address::ZERO,
                spender: Address::ZERO,
                kind: LEG_UNWRAP,
                fee: 0,
                min_out: U256::from(amount),
                data: Bytes::new(),
            });
            legs.push(ExecLeg {
                target: savax,
                token_in: Address::ZERO,
                token_out: savax,
                spender: Address::ZERO,
                kind: LEG_CALL_WITH_VALUE,
                fee: 0,
                min_out: min_out(shares),
                data: venues::savax_submit(),
            });
            let shares_u128 = u128_of(shares).ok_or(PlanError::PathUnavailable)?;
            let (pool_leg, _out) = build_leg(
                state,
                route.pool,
                savax,
                wrapped_native,
                shares_u128,
                routers,
                vault,
                executor,
                deadline,
            )?;
            legs.push(pool_leg);
        }
    }
    let _ = (path, tokens, opp);
    Ok(legs)
}

#[allow(clippy::too_many_arguments)]
fn build_leg(
    state: &PoolState,
    pool_addr: Address,
    token_in: Address,
    token_out: Address,
    amount_in: u128,
    routers: &VenueRouters,
    vault: Address,
    executor: Address,
    deadline: u64,
) -> Result<(ExecLeg, u128), PlanError> {
    let info = state.info();
    let amount = U256::from(amount_in);

    // Solidly/Camelot stable pools are stored as CurvePoolState but use the
    // V2-style pre-transfer `swap(amount0Out, amount1Out, to, data)` interface.
    if info.is_stable == Some(true) && matches!(info.dex_type, DexType::Solidly | DexType::Camelot)
    {
        let out = state
            .quote_dir(amount_in, token_in, token_out)
            .ok_or(PlanError::PathUnavailable)?;
        let (t0, _) = state.token_pair();
        let (a0, a1) = if token_in == t0 {
            (0u128, out)
        } else {
            (out, 0u128)
        };
        return Ok((
            ExecLeg {
                target: pool_addr,
                token_in,
                token_out,
                spender: Address::ZERO,
                kind: LEG_CALL_WITH_TRANSFER,
                fee: 0,
                min_out: min_out(U256::from(out)),
                data: venues::solidly_swap(U256::from(a0), U256::from(a1), executor),
            },
            out,
        ));
    }

    match state {
        PoolState::UniswapV2(_)
            if matches!(
                info.dex_type,
                DexType::UniswapV2 | DexType::Solidly | DexType::Camelot
            ) =>
        {
            let out = state
                .quote_dir(amount_in, token_in, token_out)
                .ok_or(PlanError::PathUnavailable)?;
            let fee = match info.fee_tier() {
                FeeTier::Bps(b) if (1..10_000).contains(&b) => b,
                FeeTier::Unset | FeeTier::Free => 30,
                _ => return Err(PlanError::PathUnavailable),
            };
            Ok((
                ExecLeg {
                    target: pool_addr,
                    token_in,
                    token_out,
                    spender: Address::ZERO,
                    kind: LEG_V2,
                    fee,
                    min_out: U256::ZERO,
                    data: Bytes::new(),
                },
                out,
            ))
        }
        PoolState::UniswapV3(_) if info.dex_type == DexType::UniswapV3 => {
            let out = state
                .quote_dir(amount_in, token_in, token_out)
                .ok_or(PlanError::PathUnavailable)?;
            let fee = match info.fee_tier() {
                FeeTier::Ppm(p) if p <= 0x00ff_ffff => p,
                _ => return Err(PlanError::PathUnavailable),
            };
            Ok((
                ExecLeg {
                    target: pool_addr,
                    token_in,
                    token_out,
                    spender: Address::ZERO,
                    kind: LEG_V3,
                    fee,
                    min_out: U256::ZERO,
                    data: Bytes::new(),
                },
                out,
            ))
        }
        PoolState::Curve(curve) => {
            let i = *curve
                .token_index
                .get(&token_in)
                .ok_or(PlanError::PathUnavailable)?;
            let j = *curve
                .token_index
                .get(&token_out)
                .ok_or(PlanError::PathUnavailable)?;
            let out = state
                .quote_dir(amount_in, token_in, token_out)
                .ok_or(PlanError::PathUnavailable)?;
            let underlying = matches!(
                curve.pool_variant,
                crate::pool::state::CurvePoolVariant::Meta
            ) && curve.base_pool.is_some();
            Ok((
                ExecLeg {
                    target: pool_addr,
                    token_in,
                    token_out,
                    spender: pool_addr,
                    kind: LEG_CALL,
                    fee: 0,
                    min_out: min_out(U256::from(out)),
                    data: venues::curve_exchange(
                        i as i128,
                        j as i128,
                        amount,
                        min_out(U256::from(out)),
                        underlying,
                    ),
                },
                out,
            ))
        }
        PoolState::Balancer(balancer) => {
            let pool_id = balancer.pool_id.ok_or(PlanError::PathUnavailable)?;
            let out = state
                .quote_dir(amount_in, token_in, token_out)
                .ok_or(PlanError::PathUnavailable)?;
            Ok((
                ExecLeg {
                    target: vault,
                    token_in,
                    token_out,
                    spender: vault,
                    kind: LEG_CALL,
                    fee: 0,
                    min_out: min_out(U256::from(out)),
                    data: venues::balancer_swap(
                        pool_id,
                        token_in,
                        token_out,
                        amount,
                        min_out(U256::from(out)),
                        executor,
                        deadline,
                    ),
                },
                out,
            ))
        }
        PoolState::TraderJoeLB(lb) => {
            let version = routers.lb_version(lb.info.factory);
            let router = routers
                .lb_router(version)
                .ok_or(PlanError::VenueUnsupported)?;
            let family = match version {
                2 => venues::lb::Version::V2_1,
                3 => venues::lb::Version::V2_2,
                _ => return Err(PlanError::VenueUnsupported),
            };
            let out = state
                .quote_dir(amount_in, token_in, token_out)
                .ok_or(PlanError::PathUnavailable)?;
            Ok((
                ExecLeg {
                    target: router,
                    token_in,
                    token_out,
                    spender: router,
                    kind: LEG_CALL,
                    fee: 0,
                    min_out: min_out(U256::from(out)),
                    data: venues::lb_swap_exact_tokens_for_tokens(
                        amount,
                        min_out(U256::from(out)),
                        token_in,
                        token_out,
                        lb.bin_step,
                        executor,
                        deadline,
                        family,
                    ),
                },
                out,
            ))
        }
        PoolState::Pendle(pendle) => {
            let router = routers.pendle_router.ok_or(PlanError::VenueUnsupported)?;
            if pendle.pt_address.is_zero() || pendle.sy_address.is_zero() {
                return Err(PlanError::PathUnavailable);
            }
            let out = state
                .quote_dir(amount_in, token_in, token_out)
                .ok_or(PlanError::PathUnavailable)?;
            let data = if token_in == pendle.pt_address && token_out == pendle.sy_address {
                venues::pendle_swap_exact_pt_for_sy(
                    executor,
                    pool_addr,
                    amount,
                    min_out(U256::from(out)),
                )
            } else if token_in == pendle.sy_address && token_out == pendle.pt_address {
                venues::pendle_swap_exact_sy_for_pt(
                    executor,
                    pool_addr,
                    amount,
                    min_out(U256::from(out)),
                )
            } else {
                // Underlying-token legs need the SY wrap/unwrap route, which
                // this build does not model. Explicit skip, never a guess.
                return Err(PlanError::PathUnavailable);
            };
            Ok((
                ExecLeg {
                    target: router,
                    token_in,
                    token_out,
                    spender: router,
                    kind: LEG_CALL,
                    fee: 0,
                    min_out: min_out(U256::from(out)),
                    data,
                },
                out,
            ))
        }
        // No router configured in this build: explicit unsupported venue.
        PoolState::UniswapV4(_)
        | PoolState::PancakeInfinity(_)
        | PoolState::Metric(_)
        | PoolState::Fluid(_) => Err(PlanError::VenueUnsupported),
        _ => Err(PlanError::VenueUnsupported),
    }
}

fn route_tokens(
    opp: &MevOpportunity,
    path: &[Address],
    pools: &PoolManager,
) -> Result<Vec<Address>, PlanError> {
    if let Some(tokens) = &opp.route_tokens {
        if tokens.len() == path.len() + 1 {
            return Ok(tokens.clone());
        }
    }
    let mut tokens = Vec::with_capacity(path.len() + 1);
    tokens.push(opp.token_in);
    let mut current = opp.token_in;
    for addr in path {
        let pool = pools.get(addr).ok_or(PlanError::PathUnavailable)?;
        current = pool
            .walk_next_token(current)
            .ok_or(PlanError::PathUnavailable)?;
        tokens.push(current);
    }
    Ok(tokens)
}

fn u128_of(v: U256) -> Option<u128> {
    (v <= U256::from(u128::MAX)).then(|| v.to::<u128>())
}

fn min_out(v: U256) -> U256 {
    v.saturating_mul(U256::from(MIN_OUT_BPS)) / U256::from(10_000u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::state::{CurvePoolState, CurvePoolVariant, PoolInfo, UniswapV2PoolState};
    use alloy::primitives::address;
    use std::collections::HashMap;

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
            reserve0: 1_000_000,
            reserve1: 1_000_000,
        })
    }

    fn plan(opp: &MevOpportunity, pm: &PoolManager) -> Result<ExecPlan, PlanError> {
        plan_with_routers(opp, pm, &VenueRouters::default())
    }

    fn plan_with_routers(
        opp: &MevOpportunity,
        pm: &PoolManager,
        routers: &VenueRouters,
    ) -> Result<ExecPlan, PlanError> {
        plan_from_opportunity(
            opp,
            pm,
            routers,
            address!("0x00000000000000000000000000000000000000cc"),
            FlashLoanProvider::Balancer,
            address!("0x00000000000000000000000000000000000000ee"),
            address!("0x00000000000000000000000000000000000000dd"),
            U256::from(1u64),
            12,
        )
    }

    #[test]
    fn two_hop_v2_plan_walks_tokens() {
        let t0 = address!("0x0000000000000000000000000000000000000001");
        let t1 = address!("0x0000000000000000000000000000000000000002");
        let pa = address!("0x00000000000000000000000000000000000000aa");
        let pb = address!("0x00000000000000000000000000000000000000bb");
        let mut pm = PoolManager::new();
        pm.add_pool(v2(pa, t0, t1));
        pm.add_pool(v2(pb, t0, t1));
        let mut opp = MevOpportunity::new(1, 0, Strategy::TwoHopArb, pa, 0);
        opp.pool_b = pb;
        opp.token_in = t0;
        opp.token_out = t0;
        opp.route_tokens = Some(vec![t0, t1, t0]);
        opp.input_amount = U256::from(1_000u64);
        let plan = plan(&opp, &pm).expect("plan");
        assert_eq!(plan.legs.len(), 2);
        assert_eq!(plan.legs[0].token_in, t0);
        assert_eq!(plan.legs[0].token_out, t1);
        assert_eq!(plan.legs[0].kind, LEG_V2);
        assert_eq!(plan.legs[0].fee, 30);
        assert_eq!(plan.legs[0].target, pa);
        assert_eq!(plan.legs[1].target, pb);
        assert_eq!(plan.borrow_token, t0);
        assert_eq!(plan.min_profit, U256::from(1u64));
        assert_eq!(plan.deadline, 12);
    }

    #[test]
    fn jit_is_unsupported() {
        let mut opp = MevOpportunity::new(1, 0, Strategy::Jit, Address::ZERO, 0);
        opp.input_amount = U256::from(1u64);
        let err = plan(&opp, &PoolManager::new()).unwrap_err();
        assert_eq!(err, PlanError::StrategyUnsupported);
    }

    #[test]
    fn curve_leg_builds_generic_call() {
        let t0 = address!("0x0000000000000000000000000000000000000001");
        let t1 = address!("0x0000000000000000000000000000000000000002");
        let pa = address!("0x00000000000000000000000000000000000000aa");
        let mut token_index = HashMap::new();
        token_index.insert(t0, 0usize);
        token_index.insert(t1, 1usize);
        let curve = PoolState::Curve(CurvePoolState {
            info: PoolInfo {
                address: pa,
                token0: t0,
                token1: t1,
                fee: 4,
                dex_type: DexType::Curve,
                ..Default::default()
            },
            balances: vec![1_000_000, 1_000_000],
            token_index,
            a_coeff: 100,
            pool_variant: CurvePoolVariant::Plain,
            gamma: None,
            price_scale: vec![],
            base_pool: None,
        });
        let mut pm = PoolManager::new();
        pm.add_pool(curve);
        let mut opp = MevOpportunity::new(1, 0, Strategy::TwoHopArb, pa, 0);
        opp.token_in = t0;
        opp.token_out = t1;
        opp.route_tokens = Some(vec![t0, t1]);
        opp.input_amount = U256::from(1_000u64);
        let plan = plan(&opp, &pm).expect("plan");
        assert_eq!(plan.legs.len(), 1);
        assert_eq!(plan.legs[0].kind, LEG_CALL);
        assert_eq!(plan.legs[0].spender, pa);
        assert!(!plan.legs[0].data.is_empty());
        assert!(plan.legs[0].min_out > U256::ZERO);
    }

    fn lb_pool(addr: Address, t0: Address, t1: Address, factory: Address) -> PoolState {
        PoolState::TraderJoeLB(crate::pool::state::TraderJoeLBPoolState {
            info: PoolInfo {
                address: addr,
                token0: t0,
                token1: t1,
                fee: 20,
                dex_type: DexType::TraderJoeLB,
                factory: Some(factory),
                ..Default::default()
            },
            active_id: 0,
            bin_step: 20,
            reserve_x: 1_000_000,
            reserve_y: 1_000_000,
        })
    }

    #[test]
    fn lb_without_router_is_explicitly_unsupported() {
        let t0 = address!("0x0000000000000000000000000000000000000001");
        let t1 = address!("0x0000000000000000000000000000000000000002");
        let pa = address!("0x00000000000000000000000000000000000000aa");
        let mut pm = PoolManager::new();
        pm.add_pool(lb_pool(pa, t0, t1, Address::ZERO));
        let mut opp = MevOpportunity::new(1, 0, Strategy::TwoHopArb, pa, 0);
        opp.token_in = t0;
        opp.token_out = t1;
        opp.route_tokens = Some(vec![t0, t1]);
        opp.input_amount = U256::from(1_000u64);
        assert_eq!(plan(&opp, &pm).unwrap_err(), PlanError::VenueUnsupported);
    }

    #[test]
    fn lb_leg_uses_versioned_router() {
        let t0 = address!("0x0000000000000000000000000000000000000001");
        let t1 = address!("0x0000000000000000000000000000000000000002");
        let pa = address!("0x00000000000000000000000000000000000000aa");
        let factory = address!("0x00000000000000000000000000000000000000f1");
        let router21 = address!("0x00000000000000000000000000000000000000b1");
        let router22 = address!("0x00000000000000000000000000000000000000b2");
        let mut pm = PoolManager::new();
        pm.add_pool(lb_pool(pa, t0, t1, factory));
        let mut opp = MevOpportunity::new(1, 0, Strategy::TwoHopArb, pa, 0);
        opp.token_in = t0;
        opp.token_out = t1;
        opp.route_tokens = Some(vec![t0, t1]);
        opp.input_amount = U256::from(1_000u64);

        let mut routers = VenueRouters {
            lb_router_v21: Some(router21),
            lb_router_v22: Some(router22),
            ..Default::default()
        };
        let plan = plan_with_routers(&opp, &pm, &routers).expect("v2.1 plan");
        assert_eq!(plan.legs[0].target, router21);
        assert_eq!(plan.legs[0].kind, LEG_CALL);
        assert_eq!(plan.legs[0].spender, router21);

        routers.lb_factory_versions.insert(factory, 3);
        let plan = plan_with_routers(&opp, &pm, &routers).expect("v2.2 plan");
        assert_eq!(plan.legs[0].target, router22);
    }

    #[test]
    fn pendle_leg_requires_router_and_pt_sy_pair() {
        let pt = address!("0x0000000000000000000000000000000000000001");
        let sy = address!("0x0000000000000000000000000000000000000002");
        let pa = address!("0x00000000000000000000000000000000000000aa");
        let router = address!("0x00000000000000000000000000000000000000b3");
        let pendle = PoolState::Pendle(crate::pool::state::PendlePoolState {
            info: PoolInfo {
                address: pa,
                token0: pt,
                token1: sy,
                fee: 10,
                dex_type: DexType::Pendle,
                ..Default::default()
            },
            pt_address: pt,
            sy_address: sy,
            total_pt: 1_000_000,
            total_sy: 1_000_000,
        });
        let mut pm = PoolManager::new();
        pm.add_pool(pendle);
        let mut opp = MevOpportunity::new(1, 0, Strategy::TwoHopArb, pa, 0);
        opp.token_in = pt;
        opp.token_out = sy;
        opp.route_tokens = Some(vec![pt, sy]);
        opp.input_amount = U256::from(1_000u64);

        assert_eq!(plan(&opp, &pm).unwrap_err(), PlanError::VenueUnsupported);
        let routers = VenueRouters {
            pendle_router: Some(router),
            ..Default::default()
        };
        let plan = plan_with_routers(&opp, &pm, &routers).expect("pendle plan");
        assert_eq!(plan.legs[0].target, router);
        assert_eq!(plan.legs[0].kind, LEG_CALL);
        assert_eq!(plan.legs[0].spender, router);
    }

    fn savax_pair_pool(addr: Address, wrapped: Address, savax: Address) -> PoolState {
        v2(addr, wrapped, savax)
    }

    #[test]
    fn savax_plan_builds_redeem_and_mint_legs() {
        let wrapped = address!("0x0000000000000000000000000000000000000001");
        let savax = address!("0x0000000000000000000000000000000000000002");
        let pa = address!("0x00000000000000000000000000000000000000aa");
        let mut pm = PoolManager::new();
        pm.add_pool(savax_pair_pool(pa, wrapped, savax));

        let mut opp = MevOpportunity::new(1, 0, Strategy::MultiHopArb, pa, 0);
        opp.token_in = wrapped;
        opp.token_out = wrapped;
        opp.route_tokens = Some(vec![wrapped, savax]);
        opp.input_amount = U256::from(1_000u64);
        opp.savax_route = Some(crate::types::SavaxRoute {
            pool: pa,
            direction: SavaxDirection::Redeem,
            rate_wad: U256::from(1_100_000_000_000_000_000u64),
            shares: U256::ZERO,
            min_out_wad: U256::ZERO,
        });
        let redeem = plan_from_opportunity(
            &opp,
            &pm,
            &VenueRouters::default(),
            address!("0x00000000000000000000000000000000000000cc"),
            FlashLoanProvider::Balancer,
            address!("0x00000000000000000000000000000000000000ee"),
            wrapped,
            U256::from(1u64),
            12,
        )
        .expect("redeem plan");
        assert_eq!(
            redeem.legs.iter().map(|l| l.kind).collect::<Vec<_>>(),
            vec![LEG_V2, LEG_CALL, LEG_WRAP_ALL]
        );

        let mut mint = opp.clone();
        mint.savax_route.as_mut().unwrap().direction = SavaxDirection::Mint;
        mint.route_tokens = Some(vec![savax, wrapped]);
        let mint_plan = plan_from_opportunity(
            &mint,
            &pm,
            &VenueRouters::default(),
            address!("0x00000000000000000000000000000000000000cc"),
            FlashLoanProvider::Balancer,
            address!("0x00000000000000000000000000000000000000ee"),
            wrapped,
            U256::from(1u64),
            12,
        )
        .expect("mint plan");
        assert_eq!(
            mint_plan.legs.iter().map(|l| l.kind).collect::<Vec<_>>(),
            vec![LEG_UNWRAP, LEG_CALL_WITH_VALUE, LEG_V2]
        );
    }

    #[test]
    fn balancer_leg_targets_the_vault() {
        use crate::pool::state::{BalancerPoolState, BalancerPoolVariant};
        let t0 = address!("0x0000000000000000000000000000000000000001");
        let t1 = address!("0x0000000000000000000000000000000000000002");
        let pa = address!("0x00000000000000000000000000000000000000aa");
        let vault = address!("0x00000000000000000000000000000000000000cc");
        let mut token_index = HashMap::new();
        token_index.insert(t0, 0usize);
        token_index.insert(t1, 1usize);
        let mut pm = PoolManager::new();
        pm.add_pool(PoolState::Balancer(BalancerPoolState {
            info: PoolInfo {
                address: pa,
                token0: t0,
                token1: t1,
                fee: 30,
                dex_type: DexType::Balancer,
                ..Default::default()
            },
            balances: vec![10u128.pow(18), 10u128.pow(18)],
            token_index,
            pool_id: Some([1u8; 32]),
            weights: vec![10u128.pow(18), 10u128.pow(18)],
            pool_variant: BalancerPoolVariant::Weighted,
            amplification: None,
            bpt_index: None,
            scaling_factors: vec![],
            rate_providers: vec![None, None],
            rate_cache: vec![None, None],
        }));
        let mut opp = MevOpportunity::new(1, 0, Strategy::TwoHopArb, pa, 0);
        opp.token_in = t0;
        opp.token_out = t1;
        opp.route_tokens = Some(vec![t0, t1]);
        opp.input_amount = U256::from(10u128.pow(15));
        let plan = plan(&opp, &pm).expect("balancer plan");
        assert_eq!(plan.legs.len(), 1);
        assert_eq!(plan.legs[0].kind, LEG_CALL);
        assert_eq!(plan.legs[0].target, vault);
        assert_eq!(plan.legs[0].spender, vault);
    }
}
