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
//! - flash-loan events: Aave V2/V3, Balancer V2, Uniswap V3 `Flash`,
//!   Morpho Blue, LFJ/Trader Joe LB 2.0 and 2.1/2.2
//! - V3 `Mint`/`Burn` (JIT positions)
//! - UniV2 `Sync`/`Mint`/`Burn` (skim exclusion)
//!
//! Swap token direction is resolved by pairing each swap log with the ERC-20
//! Transfer legs that move tokens into/out of the pool in the same tx
//! (registry-free, chain-generic). Pool-registry lookups can enrich later but
//! are not required for classification.
mod actions;
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

pub use actions::{decode_lb_reward_accumulator, decode_lb_transfer_batch, decode_reward_claim};
pub(crate) use aggregator::aggregator_router_version;
pub use aggregator::dedup_aggregator_facts;
// Topic pins are consumed by `chain::events` tests and decode unit tests.
#[allow(unused_imports)]
pub use aggregator::{
    LEGACY_ROUTER_SWAP_TOPIC, ONEINCH_SWAPPED_TOPIC, PARASWAP_SWAPPED_TOPIC,
    PARASWAP_SWAPPED_V3_TOPIC, ZRX_FILL_TOPIC,
};
pub use attach::attach_swap_tokens;
pub use flash_loan::{decode_flash_loan, flash_loan_funding_tag, infer_v2_flash_swap};
pub use jit::{decode_lb_bins_liquidity, decode_v3_mint_burn};
pub use liquidation::{decode_buy_collateral, decode_liquidation, remap_liquidation_protocol};
pub use sentinels::{is_unresolved_token, TOKEN0_SENTINEL, TOKEN1_SENTINEL};
pub use signals::{
    decode_epoch_reward, decode_gmx_event, decode_keeper, decode_oracle_update,
    decode_rate_cache_update, decode_reserve_data,
};
pub use swap::decode_swap;
pub use transfer::{decode_transfer, decode_v2_pair_op};

use std::collections::HashMap;
use std::sync::OnceLock;

use alloy::primitives::B256;

/// Decoder family selected by topic0. One log maps to one family.
///
/// GMX is absent: EventEmitter logs key `eventNameHash` on `topics[1]`, and
/// topic0 varies across EventLog/EventLog1/EventLog2, so the caller runs
/// `decode_gmx_event` beside this match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LogFamily {
    Transfer,
    /// Aggregator topics emit a swap edge; router-internal legs never close
    /// an arbitrage cycle.
    Aggregator,
    Swap,
    Liquidation,
    FlashLoan,
    BuyCollateral,
    V3Liquidity,
    LbLiquidity,
    /// LB rewarder `ClaimRewards` (first-class `RewardClaim` action).
    RewardClaim,
    /// LB token `TransferBatch` — bin-token movement, not ERC-20 `Transfer`.
    LbTokenTransfer,
    /// Pharaoh DLMM `BinRewardAccumulatorsUpdated` (typed, not an action).
    LbRewardAccumulator,
    V2PairOp,
    Oracle,
    Reserve,
    RateCache,
    Keeper,
    EpochReward,
}

/// Exact-topic0 dispatch table, built once per process. Every arm of the
/// historical scanner is a single `topic0` equality, so a lookup table fully
/// replaces it and removes the linear scan per log.
///
/// Range or multi-topic predicates (none currently) must stay as plain
/// functions; they are not represented here.
fn topic_families() -> &'static HashMap<B256, LogFamily> {
    static TABLE: OnceLock<HashMap<B256, LogFamily>> = OnceLock::new();
    TABLE.get_or_init(|| {
        use crate::chain::events::{
            AAVE_V2_FLASH_LOAN_TOPIC, AAVE_V3_FLASH_LOAN_TOPIC, AAVE_V3_LIQUIDATION_CALL_TOPIC,
            AAVE_V3_RESERVE_DATA_UPDATED_TOPIC, BALANCER_FLASH_LOAN_TOPIC,
            BALANCER_TOKEN_RATE_CACHE_UPDATED_TOPIC, CHAINLINK_ANSWER_UPDATED_TOPIC,
            CHAINLINK_LOG_TRIGGERED_TOPIC, CHAINLINK_UPKEEP_PERFORMED_TOPIC,
            COMPOUND_V2_LIQUIDATE_BORROW_TOPIC, COMPOUND_V3_ABSORB_TOPIC,
            COMPOUND_V3_BUY_COLLATERAL_TOPIC, DONASWAP_V3_SWAP_TOPIC, EULER_V2_LIQUIDATE_TOPIC,
            GELATO_EXEC_SUCCESS_TOPIC, INF_CL_SWAP_TOPIC, LB_BIN_REWARD_UPDATED_TOPIC,
            LB_CLAIM_REWARDS_TOPIC, LB_TRANSFER_BATCH_TOPIC, LFJ_LB_FLASH_LOAN_TOPIC,
            LFJ_LB_FLASH_LOAN_V20_TOPIC, MORPHO_BLUE_FLASH_LOAN_TOPIC, MORPHO_BLUE_LIQUIDATE_TOPIC,
            NOTIFY_REWARD_TOPIC, SILO_V2_LIQUIDATION_CALL_TOPIC, TRADER_JOE_LB_SWAP_LEGACY_TOPIC,
            TRANSFER_TOPIC, UNI_V3_FLASH_TOPIC, V2_BURN_TOPIC, V2_MINT_TOPIC, V2_SWAP_TOPIC,
            V2_SYNC_TOPIC, V3_SWAP_TOPIC, V4_SWAP_TOPIC,
        };
        use crate::pool::decoders::{
            BALANCER_SWAP_TOPIC, CURVE_TOKEN_EXCHANGE_TOPIC, CURVE_V2_TOKEN_EXCHANGE_TOPIC,
            FLUID_SWAP_TOPIC, LB_DEPOSITED_TO_BINS_TOPIC, LB_SWAP_TOPIC,
            LB_WITHDRAWN_FROM_BINS_TOPIC, METRIC_SWAP_TOPIC, PENDLE_SWAP_TOPIC, SOLIDLY_SWAP_TOPIC,
            V3_BURN_TOPIC, V3_MINT_TOPIC,
        };

        let mut table = HashMap::new();
        table.insert(TRANSFER_TOPIC, LogFamily::Transfer);
        table.insert(aggregator::ONEINCH_SWAPPED_TOPIC, LogFamily::Aggregator);
        table.insert(aggregator::PARASWAP_SWAPPED_TOPIC, LogFamily::Aggregator);
        table.insert(aggregator::PARASWAP_SWAPPED_V3_TOPIC, LogFamily::Aggregator);
        table.insert(aggregator::ZRX_FILL_TOPIC, LogFamily::Aggregator);
        table.insert(aggregator::LEGACY_ROUTER_SWAP_TOPIC, LogFamily::Aggregator);
        table.insert(V2_SWAP_TOPIC, LogFamily::Swap);
        table.insert(V3_SWAP_TOPIC, LogFamily::Swap);
        table.insert(*DONASWAP_V3_SWAP_TOPIC, LogFamily::Swap);
        table.insert(*V4_SWAP_TOPIC, LogFamily::Swap);
        table.insert(*INF_CL_SWAP_TOPIC, LogFamily::Swap);
        table.insert(*CURVE_TOKEN_EXCHANGE_TOPIC, LogFamily::Swap);
        table.insert(*CURVE_V2_TOKEN_EXCHANGE_TOPIC, LogFamily::Swap);
        table.insert(*BALANCER_SWAP_TOPIC, LogFamily::Swap);
        table.insert(*LB_SWAP_TOPIC, LogFamily::Swap);
        table.insert(*TRADER_JOE_LB_SWAP_LEGACY_TOPIC, LogFamily::Swap);
        table.insert(*PENDLE_SWAP_TOPIC, LogFamily::Swap);
        table.insert(*FLUID_SWAP_TOPIC, LogFamily::Swap);
        table.insert(*METRIC_SWAP_TOPIC, LogFamily::Swap);
        table.insert(*SOLIDLY_SWAP_TOPIC, LogFamily::Swap);
        table.insert(*AAVE_V3_LIQUIDATION_CALL_TOPIC, LogFamily::Liquidation);
        table.insert(*COMPOUND_V3_ABSORB_TOPIC, LogFamily::Liquidation);
        table.insert(*COMPOUND_V2_LIQUIDATE_BORROW_TOPIC, LogFamily::Liquidation);
        table.insert(*MORPHO_BLUE_LIQUIDATE_TOPIC, LogFamily::Liquidation);
        table.insert(*SILO_V2_LIQUIDATION_CALL_TOPIC, LogFamily::Liquidation);
        table.insert(*EULER_V2_LIQUIDATE_TOPIC, LogFamily::Liquidation);
        table.insert(*UNI_V3_FLASH_TOPIC, LogFamily::FlashLoan);
        table.insert(*AAVE_V3_FLASH_LOAN_TOPIC, LogFamily::FlashLoan);
        table.insert(*AAVE_V2_FLASH_LOAN_TOPIC, LogFamily::FlashLoan);
        table.insert(*BALANCER_FLASH_LOAN_TOPIC, LogFamily::FlashLoan);
        table.insert(*MORPHO_BLUE_FLASH_LOAN_TOPIC, LogFamily::FlashLoan);
        table.insert(*LFJ_LB_FLASH_LOAN_TOPIC, LogFamily::FlashLoan);
        table.insert(*LFJ_LB_FLASH_LOAN_V20_TOPIC, LogFamily::FlashLoan);
        table.insert(*COMPOUND_V3_BUY_COLLATERAL_TOPIC, LogFamily::BuyCollateral);
        table.insert(V3_MINT_TOPIC, LogFamily::V3Liquidity);
        table.insert(V3_BURN_TOPIC, LogFamily::V3Liquidity);
        table.insert(*jit::V3_COLLECT_TOPIC, LogFamily::V3Liquidity);
        table.insert(*LB_DEPOSITED_TO_BINS_TOPIC, LogFamily::LbLiquidity);
        table.insert(*LB_WITHDRAWN_FROM_BINS_TOPIC, LogFamily::LbLiquidity);
        table.insert(LB_CLAIM_REWARDS_TOPIC, LogFamily::RewardClaim);
        table.insert(LB_TRANSFER_BATCH_TOPIC, LogFamily::LbTokenTransfer);
        table.insert(LB_BIN_REWARD_UPDATED_TOPIC, LogFamily::LbRewardAccumulator);
        table.insert(V2_SYNC_TOPIC, LogFamily::V2PairOp);
        table.insert(*V2_MINT_TOPIC, LogFamily::V2PairOp);
        table.insert(*V2_BURN_TOPIC, LogFamily::V2PairOp);
        table.insert(*CHAINLINK_ANSWER_UPDATED_TOPIC, LogFamily::Oracle);
        table.insert(*AAVE_V3_RESERVE_DATA_UPDATED_TOPIC, LogFamily::Reserve);
        table.insert(
            *BALANCER_TOKEN_RATE_CACHE_UPDATED_TOPIC,
            LogFamily::RateCache,
        );
        table.insert(*GELATO_EXEC_SUCCESS_TOPIC, LogFamily::Keeper);
        table.insert(*CHAINLINK_UPKEEP_PERFORMED_TOPIC, LogFamily::Keeper);
        table.insert(*CHAINLINK_LOG_TRIGGERED_TOPIC, LogFamily::Keeper);
        table.insert(*NOTIFY_REWARD_TOPIC, LogFamily::EpochReward);
        table
    })
}

/// Topic0 dispatch, backed by the static table.
pub(crate) fn log_family(topic0: B256) -> Option<LogFamily> {
    topic_families().get(&topic0).copied()
}

/// True when some decoder claims this log.
///
/// Mirrors the dispatch in `classify::decode_tx_logs_with_aliases` (family table
/// first, then the GMX name-hash path) so callers can record logs that fall
/// through every decoder instead of dropping them silently.
pub(crate) fn is_mapped_log(log: &crate::data::LogData) -> bool {
    log.topics
        .first()
        .is_some_and(|topic0| log_family(*topic0).is_some())
        || decode_gmx_event(log).is_some()
}
