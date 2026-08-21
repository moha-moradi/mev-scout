//! Flash loan event scanner.
//!
//! Scans block ranges for flash loan events across multiple protocols:
//! Aave V2/V3, Balancer V2, and Uniswap V3.

use alloy::primitives::Address;
use alloy::rpc::types::Log;

use super::events::{
    decode_aave_v2_flash, decode_aave_v3_flash, decode_balancer_flash, FlashLoanEvent,
    AAVE_V2_FLASH_LOAN_TOPIC, AAVE_V3_FLASH_LOAN_TOPIC, BALANCER_FLASH_LOAN_TOPIC, V3_FLASH_TOPIC,
};
use super::scanner::LogScanner;
use crate::rpc::RpcClient;

/// All flash loan event topics.
pub fn flash_loan_topics() -> Vec<alloy::primitives::B256> {
    vec![
        *AAVE_V2_FLASH_LOAN_TOPIC,
        *AAVE_V3_FLASH_LOAN_TOPIC,
        *BALANCER_FLASH_LOAN_TOPIC,
        *V3_FLASH_TOPIC,
    ]
}

/// Scan a block range for flash loan events.
pub async fn scan_flash_loans(
    rpc: &RpcClient,
    from_block: u64,
    to_block: u64,
    batch_size: u64,
    addresses: Option<&[Address]>,
) -> anyhow::Result<Vec<FlashLoanEvent>> {
    let scanner = LogScanner::new(rpc.clone()).with_batch_size(batch_size);
    let topics = flash_loan_topics();
    let logs = scanner.scan(from_block, to_block, &topics, addresses).await?;

    let mut events = Vec::with_capacity(logs.len());
    for log in &logs {
        if let Some(evt) = decode_flash_loan_log(log) {
            events.push(evt);
        }
    }

    Ok(events)
}

/// Try all known flash loan decoders on a log.
fn decode_flash_loan_log(log: &Log) -> Option<FlashLoanEvent> {
    if let Some(e) = decode_aave_v3_flash(log) {
        return Some(e);
    }
    if let Some(e) = decode_aave_v2_flash(log) {
        return Some(e);
    }
    if let Some(e) = decode_balancer_flash(log) {
        return Some(e);
    }
    // Uniswap V3 Flash — simple decode (sender, recipient, amount0, amount1, data)
    let topic = log.topics().first()?;
    if **topic == *V3_FLASH_TOPIC {
        let data = &log.data().data;
        let initiator = log.topics().get(1).map(|t| Address::from_slice(&t[12..])).unwrap_or(Address::ZERO);
        let target = log.topics().get(2).map(|t| Address::from_slice(&t[12..])).unwrap_or(Address::ZERO);
        let amount = if data.len() >= 32 {
            alloy::primitives::U256::from_be_slice(&data[0..32])
        } else {
            return None;
        };
        let fee = if data.len() >= 64 {
            Some(alloy::primitives::U256::from_be_slice(&data[32..64]))
        } else {
            None
        };
        return Some(FlashLoanEvent {
            block: log.block_number?,
            tx_hash: log.transaction_hash?,
            tx_index: log.transaction_index,
            log_index: log.log_index?,
            protocol: "uniswap_v3".to_string(),
            initiator,
            target,
            token: Address::ZERO,
            amount,
            fee,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flash_loan_topics_count() {
        assert_eq!(flash_loan_topics().len(), 4);
    }
}
