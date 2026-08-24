//! DEX swap event scanner.
//!
//! Scans block ranges for swap events across V2/V3/Solidly/Curve pools.
//! Replaces `QUERY_TRADES_IN_RANGE`.

use alloy::primitives::{Address, B256};
use alloy::rpc::types::Log;

use super::events::{
    decode_curve_exchange, decode_pendle_swap, decode_trader_joe_lb_swap, decode_uniswap_v2_swap,
    decode_uniswap_v3_swap, TradeEvent, CURVE_TOKEN_EXCHANGE_TOPIC, CURVE_V2_TOKEN_EXCHANGE_TOPIC,
    PENDLE_MARKET_SWAP_TOPIC, SOLIDLY_SWAP_TOPIC, TRADER_JOE_LB_SWAP_LEGACY_TOPIC,
    TRADER_JOE_LB_SWAP_TOPIC, V2_SWAP_TOPIC, V3_SWAP_TOPIC, V4_SWAP_TOPIC,
};
use super::scanner::LogScanner;
use crate::rpc::RpcClient;

/// Swap event topics per DEX family.
pub fn trade_topics() -> Vec<B256> {
    vec![
        V2_SWAP_TOPIC,
        V3_SWAP_TOPIC,
        *V4_SWAP_TOPIC,
        *SOLIDLY_SWAP_TOPIC,
        *TRADER_JOE_LB_SWAP_TOPIC,
        *TRADER_JOE_LB_SWAP_LEGACY_TOPIC,
        *PENDLE_MARKET_SWAP_TOPIC,
        *CURVE_TOKEN_EXCHANGE_TOPIC,
        *CURVE_V2_TOKEN_EXCHANGE_TOPIC,
    ]
}

/// Scan a block range for DEX swap events.
///
/// Returns decoded `TradeEvent`s. Token addresses are `Address::ZERO` when
/// the pool token mapping is not available (caller should resolve from pool state).
pub async fn scan_trades(
    rpc: &RpcClient,
    from_block: u64,
    to_block: u64,
    batch_size: u64,
    pool_addresses: Option<&[Address]>,
) -> anyhow::Result<Vec<TradeEvent>> {
    let scanner = LogScanner::new(rpc.clone()).with_batch_size(batch_size);
    let topics = trade_topics();
    let logs = scanner.scan(from_block, to_block, &topics, pool_addresses).await?;

    let mut events = Vec::with_capacity(logs.len());
    for log in &logs {
        if let Some(evt) = decode_trade_log(log) {
            events.push(evt);
        }
    }

    Ok(events)
}

/// Classify and decode a swap event log based on its topic.
fn decode_trade_log(log: &Log) -> Option<TradeEvent> {
    let topic = log.topics().first()?;
    let pool = log.address();

    if **topic == V2_SWAP_TOPIC {
        return decode_uniswap_v2_swap(log, pool);
    }
    if **topic == *TRADER_JOE_LB_SWAP_TOPIC || **topic == *TRADER_JOE_LB_SWAP_LEGACY_TOPIC {
        return decode_trader_joe_lb_swap(log, pool, **topic == *TRADER_JOE_LB_SWAP_LEGACY_TOPIC);
    }
    if **topic == *PENDLE_MARKET_SWAP_TOPIC {
        return decode_pendle_swap(log, pool);
    }
    if **topic == V3_SWAP_TOPIC || **topic == *V4_SWAP_TOPIC || **topic == *SOLIDLY_SWAP_TOPIC {
        return decode_uniswap_v3_swap(log, pool);
    }
    if **topic == *CURVE_TOKEN_EXCHANGE_TOPIC || **topic == *CURVE_V2_TOKEN_EXCHANGE_TOPIC {
        return decode_curve_exchange(log, pool);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trade_topics_count() {
        assert_eq!(trade_topics().len(), 9);
    }
}
