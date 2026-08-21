//! Liquidation event scanner.
//!
//! Scans block ranges for on-chain liquidation events:
//! Aave V3 `LiquidationCall` and Compound V3 `Absorb`.

use alloy::primitives::{Address, U256};
use alloy::rpc::types::Log;

use super::events::{
    decode_aave_v3_liquidation, LiquidationEvent, AAVE_V3_LIQUIDATION_CALL_TOPIC,
    COMPOUND_V3_ABSORB_TOPIC,
};
use super::scanner::LogScanner;
use crate::rpc::RpcClient;

/// All liquidation event topics.
pub fn liquidation_topics() -> Vec<alloy::primitives::B256> {
    vec![*AAVE_V3_LIQUIDATION_CALL_TOPIC, *COMPOUND_V3_ABSORB_TOPIC]
}

/// Scan a block range for liquidation events.
pub async fn scan_liquidations(
    rpc: &RpcClient,
    from_block: u64,
    to_block: u64,
    batch_size: u64,
    addresses: Option<&[Address]>,
) -> anyhow::Result<Vec<LiquidationEvent>> {
    let scanner = LogScanner::new(rpc.clone()).with_batch_size(batch_size);
    let topics = liquidation_topics();
    let logs = scanner.scan(from_block, to_block, &topics, addresses).await?;

    let mut events = Vec::with_capacity(logs.len());
    for log in &logs {
        if let Some(evt) = decode_liquidation_log(log) {
            events.push(evt);
        }
    }

    Ok(events)
}

/// Try all known liquidation decoders on a log.
fn decode_liquidation_log(log: &Log) -> Option<LiquidationEvent> {
    if let Some(e) = decode_aave_v3_liquidation(log) {
        return Some(e);
    }
    let topic = log.topics().first()?;
    if **topic == *COMPOUND_V3_ABSORB_TOPIC {
        let absorber = log.topics().get(1).map(|t| Address::from_slice(&t[12..])).unwrap_or(Address::ZERO);
        let data = &log.data().data;
        let borrower = if data.len() >= 20 {
            Address::from_slice(&data[0..20])
        } else {
            Address::ZERO
        };
        let collateral_amount = if data.len() >= 52 {
            U256::from_be_slice(&data[20..52])
        } else {
            U256::ZERO
        };
        let debt_to_cover = if data.len() >= 84 {
            U256::from_be_slice(&data[52..84])
        } else {
            U256::ZERO
        };
        return Some(LiquidationEvent {
            block: log.block_number?,
            tx_hash: log.transaction_hash?,
            tx_index: log.transaction_index,
            log_index: log.log_index?,
            protocol: "compound_v3".to_string(),
            user: borrower,
            liquidator: absorber,
            collateral_asset: Address::ZERO,
            debt_asset: Address::ZERO,
            collateral_amount,
            debt_to_cover,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn liquidation_topics_count() {
        assert_eq!(liquidation_topics().len(), 2);
    }
}
