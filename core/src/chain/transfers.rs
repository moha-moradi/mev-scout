//! ERC-20 Transfer event scanner.
//!
//! Scans block ranges for ERC-20 Transfer events, useful for whale detection
//! and token flow analysis.

use alloy::primitives::Address;

use super::events::{decode_transfer, TransferEvent, TRANSFER_TOPIC};
use super::scanner::LogScanner;
use crate::rpc::RpcClient;

/// Scan a block range for ERC-20 Transfer events.
///
/// When `token_addresses` is provided, only transfers from those tokens are
/// returned (address-filtered scan). When `None`, all ERC-20 transfers are
/// captured (topic-only scan).
pub async fn scan_transfers(
    rpc: &RpcClient,
    from_block: u64,
    to_block: u64,
    batch_size: u64,
    token_addresses: Option<&[Address]>,
) -> anyhow::Result<Vec<TransferEvent>> {
    let scanner = LogScanner::new(rpc.clone()).with_batch_size(batch_size);
    let logs = scanner
        .scan_topic(from_block, to_block, TRANSFER_TOPIC, token_addresses)
        .await?;

    let mut events = Vec::with_capacity(logs.len());
    for log in &logs {
        if let Some(evt) = decode_transfer(log) {
            events.push(evt);
        }
    }

    Ok(events)
}

/// Scan for large ERC-20 transfers above a minimum value threshold.
///
/// Useful for whale detection without post-filtering every transfer.
pub async fn scan_whale_transfers(
    rpc: &RpcClient,
    from_block: u64,
    to_block: u64,
    batch_size: u64,
    min_value: alloy::primitives::U256,
    token_addresses: Option<&[Address]>,
) -> anyhow::Result<Vec<TransferEvent>> {
    let all = scan_transfers(rpc, from_block, to_block, batch_size, token_addresses).await?;
    Ok(all.into_iter().filter(|e| e.value >= min_value).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_topic_matches_erc20() {
        assert_eq!(
            TRANSFER_TOPIC,
            B256::from(alloy::primitives::b256!(
                "ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"
            ))
        );
    }
}
