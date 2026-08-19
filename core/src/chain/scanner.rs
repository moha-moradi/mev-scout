//! Chunked `eth_getLogs` range scanner.
//!
//! Provides a reusable building block for topic-only or topic+address scans
//! over arbitrary block ranges. Chunk sizing, retry, and RPC rate limiting
//! are handled by the underlying `RpcClient`.

use alloy::primitives::{Address, B256};
use alloy::rpc::types::{Filter, Log};

use crate::rpc::RpcClient;

/// Default batch size for `eth_getLogs` requests. Conservative for public
/// RPCs that typically cap at 2 000–10 000 blocks per query.
const DEFAULT_BATCH_SIZE: u64 = 500;

/// A chunked event-log scanner that respects RPC batch limits.
pub struct LogScanner {
    rpc: RpcClient,
    batch_size: u64,
}

impl LogScanner {
    pub fn new(rpc: RpcClient) -> Self {
        Self {
            rpc,
            batch_size: DEFAULT_BATCH_SIZE,
        }
    }

    pub fn with_batch_size(mut self, n: u64) -> Self {
        self.batch_size = n.max(1);
        self
    }

    /// Scan a block range for logs matching the given topics.
    ///
    /// When `addresses` is `Some`, only logs from those contracts are returned.
    /// When `None`, the filter is topic-only (scans all contracts).
    pub async fn scan(
        &self,
        from_block: u64,
        to_block: u64,
        topics: &[B256],
        addresses: Option<&[Address]>,
    ) -> anyhow::Result<Vec<Log>> {
        let mut all_logs = Vec::new();
        let mut current = from_block;

        while current <= to_block {
            let batch_end = (current + self.batch_size - 1).min(to_block);

            let mut filter = Filter::new()
                .event_signature(topics.to_vec())
                .from_block(current)
                .to_block(batch_end);

            if let Some(addrs) = addresses {
                if !addrs.is_empty() {
                    filter = filter.address(addrs.to_vec());
                }
            }

            match self.rpc.get_logs(&filter).await {
                Ok(logs) => {
                    all_logs.extend(logs);
                }
                Err(e) => {
                    tracing::warn!(
                        "Log scan failed for blocks {current}..{batch_end}: {e:#}. \
                         Falling back to single-block scan for this batch."
                    );
                    for b in current..=batch_end {
                        let mut single = Filter::new()
                            .event_signature(topics.to_vec())
                            .from_block(b)
                            .to_block(b);
                        if let Some(addrs) = addresses {
                            if !addrs.is_empty() {
                                single = single.address(addrs.to_vec());
                            }
                        }
                        if let Ok(logs) = self.rpc.get_logs(&single).await {
                            all_logs.extend(logs);
                        }
                    }
                }
            }

            if batch_end >= to_block {
                break;
            }
            current = batch_end + 1;
        }

        Ok(all_logs)
    }

    /// Convenience: scan for a single topic (most common case).
    pub async fn scan_topic(
        &self,
        from_block: u64,
        to_block: u64,
        topic: B256,
        addresses: Option<&[Address]>,
    ) -> anyhow::Result<Vec<Log>> {
        self.scan(from_block, to_block, &[topic], addresses).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_batch_size_is_conservative() {
        assert_eq!(DEFAULT_BATCH_SIZE, 500);
    }

    #[test]
    fn with_batch_size_clamps_to_minimum() {
        let rpc = RpcClient::new("http://localhost:8545", 1).unwrap();
        let scanner = LogScanner::new(rpc).with_batch_size(0);
        assert_eq!(scanner.batch_size, 1);
    }
}
