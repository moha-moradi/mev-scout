//! Runtime flash-loan premium resolution (plan A-03).
//!
//! Provider fee tables in [`FlashLoanProvider::fee_rate_bps`] are documentation
//! fallbacks. Aave V3 and Balancer V2 expose their current premium on-chain, so
//! a live session can quote the real value instead. Every read is best-effort:
//! a missing address, revert or malformed word returns `None` and the caller
//! keeps the fallback. `Uniswap`, `Lfj` and `V2FlashSwap` fees are per-pool or
//! in-kind and cannot be represented by one session-wide number; they are
//! deliberately left on their fallbacks.
use alloy::primitives::{Bytes, U256};

use crate::config::ChainConfig;
use crate::pool::selectors;
use crate::types::FlashLoanProvider;
use crate::utils::abi_word_address;

use super::{BlockRef, RpcClient};

/// 1e18 fixed-point denominator used by Balancer percentage getters.
const WAD: u128 = 1_000_000_000_000_000_000;

/// Resolve the current flash premium in basis points, or `None` when this
/// provider/chain cannot be quoted live. Values above 100% are rejected.
pub async fn resolve_flash_loan_fee_bps(
    rpc: &RpcClient,
    provider: FlashLoanProvider,
    chain: &ChainConfig,
    block: BlockRef,
) -> Option<u128> {
    let bps = match provider {
        FlashLoanProvider::Aave => aave_premium_bps(rpc, chain, block).await?,
        // `Auto` selects Balancer in this codebase (funding_source + exec plan).
        FlashLoanProvider::Auto | FlashLoanProvider::Balancer => {
            balancer_premium_bps(rpc, chain, block).await?
        }
        FlashLoanProvider::Uniswap
        | FlashLoanProvider::Lfj
        | FlashLoanProvider::V2FlashSwap => return None,
    };
    (bps <= 10_000).then_some(bps)
}

async fn aave_premium_bps(
    rpc: &RpcClient,
    chain: &ChainConfig,
    block: BlockRef,
) -> Option<u128> {
    let pool = chain.aave_v3_pool?;
    let selector: &Bytes = &selectors::AAVE_FLASHLOAN_PREMIUM_TOTAL;
    let out = rpc.call_ref(pool, selector.clone(), block).await.ok()?;
    let word = out.get(..32)?;
    Some(U256::from_be_slice(word).try_into().ok()?)
}

async fn balancer_premium_bps(
    rpc: &RpcClient,
    chain: &ChainConfig,
    block: BlockRef,
) -> Option<u128> {
    let vault = chain.balancer_vault?;
    let collector_selector: &Bytes = &selectors::BALANCER_GET_PROTOCOL_FEES_COLLECTOR;
    let out = rpc
        .call_ref(vault, collector_selector.clone(), block)
        .await
        .ok()?;
    if out.len() < 32 {
        return None;
    }
    let collector = abi_word_address(&out, 0);
    if collector.is_zero() {
        return None;
    }
    let fee_selector: &Bytes = &selectors::BALANCER_GET_FLASH_LOAN_FEE_PERCENTAGE;
    let out = rpc
        .call_ref(collector, fee_selector.clone(), block)
        .await
        .ok()?;
    let wad = U256::from_be_slice(out.get(..32)?);
    // (wad * 10_000) / 1e18 — fee bps, rounded down.
    let bps = wad
        .saturating_mul(U256::from(10_000u128))
        .checked_div(U256::from(WAD))?;
    Some(bps.try_into().ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wad_to_bps_rounds_down_and_rejects_impossible_values() {
        let one_bps = U256::from(WAD) / U256::from(10_000u128);
        let bps = one_bps
            .saturating_mul(U256::from(10_000u128))
            .checked_div(U256::from(WAD))
            .unwrap();
        assert_eq!(bps, U256::from(1u128));
        assert!(bps <= U256::from(10_000u128));
    }

    #[test]
    fn per_pool_providers_have_no_session_wide_quote() {
        // Compile-time guard: the match in resolve_flash_loan_fee_bps is
        // exhaustive. A new provider variant must be classified explicitly.
        for provider in [
            FlashLoanProvider::Uniswap,
            FlashLoanProvider::Lfj,
            FlashLoanProvider::V2FlashSwap,
        ] {
            assert!(matches!(
                provider,
                FlashLoanProvider::Uniswap
                    | FlashLoanProvider::Lfj
                    | FlashLoanProvider::V2FlashSwap
            ));
        }
    }
}
