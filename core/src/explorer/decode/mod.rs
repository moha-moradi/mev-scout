//! Receipt-log decoders for the explorer's forensic layer.
//!
//! Works directly on `LogData` entries from `ReceiptData` (the bulk-receipt
//! path) rather than `ExecutedLog` (the replay path), so the explorer never
//! needs the EVM replayer. Covers:
//! - raw ERC-20 `Transfer` (the accounting primitive for profit attribution)
//! - DEX swap events: V2, V3, V4, Curve, Balancer, Solidly, Trader Joe LB, Pendle,
//!   Fluid, Metric
//! - liquidation registry: Aave V3 `LiquidationCall` (+ Spark address alias),
//!   Compound V3 `Absorb`/`BuyCollateral`, Compound V2 `LiquidateBorrow`,
//!   Morpho Blue / Silo V2 / Euler V2 liquidations ( / /)
//! - V3 `Mint`/`Burn` (JIT positions)
//! - UniV2 `Sync`/`Mint`/`Burn` (skim exclusion)
//!
//! Swap token direction is resolved by pairing each swap log with the ERC-20
//! Transfer legs that move tokens into/out of the pool in the same tx
//! (registry-free, chain-generic). Pool-registry lookups can enrich later but
//! are not required for classification.
mod aggregator;
mod attach;
mod flash_loan;
mod jit;
mod liquidation;
mod sentinels;
mod signals;
mod swap;
mod transfer;

#[cfg(test)]
mod tests;

pub use aggregator::{brand_for_router, decode_aggregator_signal, dedup_aggregator_facts};
// Topic pins are consumed by `chain::events` tests and decode unit tests.
#[allow(unused_imports)]
pub use aggregator::{
    ODOS_SWAP_TOPIC, ONEINCH_SWAPPED_TOPIC, PARASWAP_SWAPPED_TOPIC, PARASWAP_SWAPPED_V3_TOPIC,
    ZRX_FILL_TOPIC,
};
pub use attach::attach_swap_tokens;
pub use flash_loan::decode_flash_loan;
pub use jit::{decode_lb_bins_liquidity, decode_v3_mint_burn};
pub use liquidation::{decode_buy_collateral, decode_liquidation, remap_liquidation_protocol};
pub use sentinels::{is_unresolved_token, TOKEN0_SENTINEL, TOKEN1_SENTINEL};
pub use signals::{
    decode_epoch_reward, decode_gmx_event, decode_keeper, decode_oracle_update,
    decode_rate_cache_update, decode_reserve_data, decode_user_op,
};
pub use swap::decode_swap;
pub use transfer::{decode_transfer, decode_v2_pair_op};

use alloy::primitives::B256;

/// Decoder family selected by topic0. One log maps to one family.
///
/// GMX is absent: EventEmitter logs key `eventNameHash` on `topics[1]`, and
/// topic0 varies across EventLog/EventLog1/EventLog2, so the caller runs
/// `decode_gmx_event` beside this match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LogFamily {
    Transfer,
    /// Aggregator topics emit both a signal and a swap edge.
    Aggregator,
    Swap,
    Liquidation,
    FlashLoan,
    BuyCollateral,
    V3Liquidity,
    LbLiquidity,
    V2PairOp,
    Oracle,
    Reserve,
    RateCache,
    Keeper,
    EpochReward,
    UserOp,
}

/// Topic0 dispatch. Arm order matches the historical decoder scan.
pub(crate) fn log_family(topic0: B256) -> Option<LogFamily> {
    use crate::chain::events::{
        AAVE_V2_FLASH_LOAN_TOPIC, AAVE_V3_FLASH_LOAN_TOPIC, AAVE_V3_LIQUIDATION_CALL_TOPIC,
        AAVE_V3_RESERVE_DATA_UPDATED_TOPIC, BALANCER_FLASH_LOAN_TOPIC,
        BALANCER_TOKEN_RATE_CACHE_UPDATED_TOPIC, CHAINLINK_ANSWER_UPDATED_TOPIC,
        CHAINLINK_LOG_TRIGGERED_TOPIC, CHAINLINK_UPKEEP_PERFORMED_TOPIC,
        COMPOUND_V2_LIQUIDATE_BORROW_TOPIC, COMPOUND_V3_ABSORB_TOPIC,
        COMPOUND_V3_BUY_COLLATERAL_TOPIC, DONASWAP_V3_SWAP_TOPIC, EULER_V2_LIQUIDATE_TOPIC,
        GELATO_EXEC_SUCCESS_TOPIC, INF_CL_SWAP_TOPIC, MORPHO_BLUE_FLASH_LOAN_TOPIC,
        MORPHO_BLUE_LIQUIDATE_TOPIC, NOTIFY_REWARD_TOPIC, SILO_V2_LIQUIDATION_CALL_TOPIC,
        TRADER_JOE_LB_SWAP_LEGACY_TOPIC, TRANSFER_TOPIC, UNI_V3_FLASH_TOPIC,
        USER_OPERATION_EVENT_TOPIC, V2_BURN_TOPIC, V2_MINT_TOPIC, V2_SWAP_TOPIC, V2_SYNC_TOPIC,
        V3_SWAP_TOPIC, V4_SWAP_TOPIC,
    };
    use crate::pool::decoders::{
        BALANCER_SWAP_TOPIC, CURVE_TOKEN_EXCHANGE_TOPIC, CURVE_V2_TOKEN_EXCHANGE_TOPIC,
        FLUID_SWAP_TOPIC, LB_DEPOSITED_TO_BINS_TOPIC, LB_SWAP_TOPIC, LB_WITHDRAWN_FROM_BINS_TOPIC,
        METRIC_SWAP_TOPIC, PENDLE_SWAP_TOPIC, SOLIDLY_SWAP_TOPIC, V3_BURN_TOPIC, V3_MINT_TOPIC,
    };

    match topic0 {
        t if t == TRANSFER_TOPIC => Some(LogFamily::Transfer),
        t if t == aggregator::ONEINCH_SWAPPED_TOPIC
            || t == aggregator::PARASWAP_SWAPPED_TOPIC
            || t == aggregator::PARASWAP_SWAPPED_V3_TOPIC
            || t == aggregator::ZRX_FILL_TOPIC
            || t == aggregator::ODOS_SWAP_TOPIC =>
        {
            Some(LogFamily::Aggregator)
        }
        t if t == V2_SWAP_TOPIC
            || t == V3_SWAP_TOPIC
            || t == *DONASWAP_V3_SWAP_TOPIC
            || t == *V4_SWAP_TOPIC
            || t == *INF_CL_SWAP_TOPIC
            || t == *CURVE_TOKEN_EXCHANGE_TOPIC
            || t == *CURVE_V2_TOKEN_EXCHANGE_TOPIC
            || t == *BALANCER_SWAP_TOPIC
            || t == *LB_SWAP_TOPIC
            || t == *TRADER_JOE_LB_SWAP_LEGACY_TOPIC
            || t == *PENDLE_SWAP_TOPIC
            || t == *FLUID_SWAP_TOPIC
            || t == *METRIC_SWAP_TOPIC
            || t == *SOLIDLY_SWAP_TOPIC =>
        {
            Some(LogFamily::Swap)
        }
        t if t == *AAVE_V3_LIQUIDATION_CALL_TOPIC
            || t == *COMPOUND_V3_ABSORB_TOPIC
            || t == *COMPOUND_V2_LIQUIDATE_BORROW_TOPIC
            || t == *MORPHO_BLUE_LIQUIDATE_TOPIC
            || t == *SILO_V2_LIQUIDATION_CALL_TOPIC
            || t == *EULER_V2_LIQUIDATE_TOPIC =>
        {
            Some(LogFamily::Liquidation)
        }
        t if t == *UNI_V3_FLASH_TOPIC
            || t == *AAVE_V3_FLASH_LOAN_TOPIC
            || t == *AAVE_V2_FLASH_LOAN_TOPIC
            || t == *BALANCER_FLASH_LOAN_TOPIC
            || t == *MORPHO_BLUE_FLASH_LOAN_TOPIC =>
        {
            Some(LogFamily::FlashLoan)
        }
        t if t == *COMPOUND_V3_BUY_COLLATERAL_TOPIC => Some(LogFamily::BuyCollateral),
        t if t == V3_MINT_TOPIC || t == V3_BURN_TOPIC => Some(LogFamily::V3Liquidity),
        t if t == *LB_DEPOSITED_TO_BINS_TOPIC || t == *LB_WITHDRAWN_FROM_BINS_TOPIC => {
            Some(LogFamily::LbLiquidity)
        }
        t if t == V2_SYNC_TOPIC || t == *V2_MINT_TOPIC || t == *V2_BURN_TOPIC => {
            Some(LogFamily::V2PairOp)
        }
        t if t == *CHAINLINK_ANSWER_UPDATED_TOPIC => Some(LogFamily::Oracle),
        t if t == *AAVE_V3_RESERVE_DATA_UPDATED_TOPIC => Some(LogFamily::Reserve),
        t if t == *BALANCER_TOKEN_RATE_CACHE_UPDATED_TOPIC => Some(LogFamily::RateCache),
        t if t == *GELATO_EXEC_SUCCESS_TOPIC
            || t == *CHAINLINK_UPKEEP_PERFORMED_TOPIC
            || t == *CHAINLINK_LOG_TRIGGERED_TOPIC =>
        {
            Some(LogFamily::Keeper)
        }
        t if t == *NOTIFY_REWARD_TOPIC => Some(LogFamily::EpochReward),
        t if t == *USER_OPERATION_EVENT_TOPIC => Some(LogFamily::UserOp),
        _ => None,
    }
}
