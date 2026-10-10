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

pub use aggregator::dedup_aggregator_facts;
// Topic pins are consumed by `chain::events` tests and decode unit tests.
#[allow(unused_imports)]
pub use aggregator::{
    ONEINCH_SWAPPED_TOPIC, PARASWAP_SWAPPED_TOPIC, PARASWAP_SWAPPED_V3_TOPIC, ZRX_FILL_TOPIC,
};
pub use attach::attach_swap_tokens;
pub use flash_loan::decode_flash_loan;
pub use jit::{decode_lb_bins_liquidity, decode_v3_mint_burn};
pub use liquidation::{decode_buy_collateral, decode_liquidation, remap_liquidation_protocol};
pub use sentinels::{TOKEN0_SENTINEL, TOKEN1_SENTINEL};
pub use signals::{
    decode_epoch_reward, decode_gmx_event, decode_keeper, decode_oracle_update,
    decode_rate_cache_update, decode_reserve_data, decode_user_op,
};
pub use swap::decode_swap;
pub use transfer::{decode_transfer, decode_v2_pair_op};
