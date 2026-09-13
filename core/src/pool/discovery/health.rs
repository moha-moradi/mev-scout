//! Pool liveness probes — per-DEX health-check call layout and response decode.
//!
//! Every DEX gets a `health_probe(pool, balancer_vault) -> Option<Probe>`
//! (ASCII-only deploy-time class) and a `probe_alive(dex_type, bytes)` verdict.
//! Metric / Fluid have no verified on-chain health probe (Metric's pool ABI is
//! unpublished; Fluid reserves live in the Liquidity layer) — they probe as
//! `None` and are kept without a liveness verdict.

use alloy::primitives::{Address, Bytes, U256};

use super::{DexType, DiscoveredPool};
use crate::pool::selectors::{
    CURVE_BALANCES_U256, GET_POOL_TOKENS, GET_RESERVES, INF_CL_SLOT0, LB_GET_ACTIVE_ID,
    PENDLE_READ_STATE, V3_SLOT0,
};

/// One probe target as prepared for the RPC: destination contract + calldata.
pub(super) struct HealthProbe {
    pub(super) to: Address,
    pub(super) data: Bytes,
}

/// Build the liveness probe for `pool`. `None` means "no probeable on-chain
/// signal" (the pool is kept without a verdict).
pub(super) fn health_probe(
    pool: &DiscoveredPool,
    balancer_vault: Option<Address>,
) -> Option<HealthProbe> {
    match pool.dex_type {
        DexType::UniswapV2 | DexType::Solidly | DexType::Camelot => {
            // getReserves() — probe reserves for non-zero r0/r1
            Some(HealthProbe {
                to: pool.address,
                data: GET_RESERVES.clone(),
            })
        }
        DexType::UniswapV3 | DexType::UniswapV4 => {
            // slot0() — check sqrtPriceX96 != 0
            Some(HealthProbe {
                to: pool.address,
                data: V3_SLOT0.clone(),
            })
        }
        DexType::PancakeInfinity => {
            // Singleton CLPoolManager — health = getSlot0(poolId) on the
            // manager (the synthetic pool key is not a contract address).
            let (manager, pool_id) = pool.factory.zip(pool.pool_id)?;
            let mut calldata = Vec::with_capacity(36);
            calldata.extend_from_slice(&INF_CL_SLOT0);
            calldata.extend_from_slice(&pool_id);
            Some(HealthProbe {
                to: manager,
                data: Bytes::from(calldata),
            })
        }
        DexType::TraderJoeLB => {
            // getActiveId() — non-zero active bin means the pool is live
            Some(HealthProbe {
                to: pool.address,
                data: LB_GET_ACTIVE_ID.clone(),
            })
        }
        DexType::Pendle => {
            // readState(address) — check if totalPt > 0 (non-zero reserves = active market)
            let mut calldata = Vec::with_capacity(36);
            calldata.extend_from_slice(&PENDLE_READ_STATE);
            calldata.extend_from_slice(&[0u8; 32]); // address(0) as router param
            Some(HealthProbe {
                to: pool.address,
                data: Bytes::from(calldata),
            })
        }
        DexType::Curve => {
            // balances(uint256) — check token0 balance
            let mut calldata = Vec::with_capacity(36);
            calldata.extend_from_slice(&CURVE_BALANCES_U256);
            calldata.extend_from_slice(&[0u8; 32]); // index 0
            Some(HealthProbe {
                to: pool.address,
                data: Bytes::from(calldata),
            })
        }
        DexType::Balancer => {
            // getPoolTokens(bytes32) on vault — check if any balance is non-zero
            let (vault, pool_id) = balancer_vault.zip(pool.pool_id)?;
            let mut calldata = Vec::with_capacity(36);
            calldata.extend_from_slice(&GET_POOL_TOKENS);
            calldata.extend_from_slice(&pool_id);
            Some(HealthProbe {
                to: vault,
                data: Bytes::from(calldata),
            })
        }
        DexType::Metric | DexType::Fluid => None,
    }
}

/// Decode a probe response into a liveness verdict for `dex_type`.
///
/// `false` (Dead) means the pool was probed but reported drained/paused;
/// probing RPC errors are handled by the caller as *unknown* (pool kept).
pub(super) fn probe_alive(dex_type: &DexType, bytes: &[u8]) -> bool {
    match dex_type {
        // slot0 / getActiveId / readState / balances: first 32 bytes non-zero
        // covers sqrtPriceX96, the max-active-bin id, totalPt, and token0
        // balance respectively.
        DexType::UniswapV3
        | DexType::UniswapV4
        | DexType::TraderJoeLB
        | DexType::Pendle
        | DexType::Curve => bytes.len() >= 32 && !bytes[..32].iter().all(|b| *b == 0),
        DexType::Balancer => {
            // getPoolTokens: (address[], uint256[] balances, uint256) — decode
            // the balances dynamic array and check any balance is non-zero.
            if bytes.len() < 96 {
                return false;
            }
            let balances_offset = U256::from_be_slice(&bytes[32..64]).as_limbs()[0] as usize;
            let token_count_offset = 64 + balances_offset;
            if token_count_offset + 32 > bytes.len() {
                return false;
            }
            let token_count =
                U256::from_be_slice(&bytes[token_count_offset..token_count_offset + 32]).as_limbs()
                    [0] as usize;
            let balances_start = token_count_offset + 32;
            for j in 0..token_count {
                let off = balances_start + j * 32;
                if off + 32 <= bytes.len() {
                    let bal = U256::from_be_slice(&bytes[off..off + 32]);
                    if !bal.is_zero() {
                        return true;
                    }
                }
            }
            false
        }
        // getReserves (V2/Solidly/Camelot): r0(32) + r1(32) + blockTimestamp(32)
        _ => {
            bytes.len() >= 64
                && (!bytes[..32].iter().all(|b| *b == 0) || !bytes[32..64].iter().all(|b| *b == 0))
        }
    }
}
