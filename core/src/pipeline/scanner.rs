//! DEX activity scanner — identifies blocks with DEX pool activity
//! using eth_getLogs. Enables log-first fetch optimization: instead of
//! fetching every block in a range, scan for DEX events first and only
//! fetch blocks that have relevant activity.
use std::collections::HashSet;

use alloy::primitives::Address;
use alloy::rpc::types::Filter;

use crate::error;
use crate::rpc::RpcClient;

/// DEX event topic signatures used for activity detection.
///
/// Every name is a re-export of [`crate::chain::events`]. Hashes are not
/// computed here.
pub(crate) mod topics {
    use alloy::primitives::B256;

    pub use crate::chain::events::{
        BALANCER_SWAP_TOPIC as BALANCER_SWAP, CURVE_TOKEN_EXCHANGE_TOPIC as CURVE_TOKEN_EXCHANGE,
        CURVE_TOKEN_EXCHANGE_UNDERLYING_TOPIC as CURVE_TOKEN_EXCHANGE_UNDERLYING,
        CURVE_V2_TOKEN_EXCHANGE_TOPIC as CURVE_V2_TOKEN_EXCHANGE,
        CURVE_V2_TOKEN_EXCHANGE_UNDERLYING_TOPIC as CURVE_V2_TOKEN_EXCHANGE_UNDERLYING,
        FLUID_SWAP_TOPIC as FLUID_SWAP, INF_CL_SWAP_TOPIC as INF_CL_SWAP,
        METRIC_SWAP_TOPIC as METRIC_SWAP, PENDLE_MARKET_SWAP_TOPIC as PENDLE_MARKET_SWAP,
        PENDLE_SWAP_PT_AND_SY_TOPIC as PENDLE_SWAP_PT_AND_SY,
        PENDLE_SWAP_PT_AND_TOKEN_TOPIC as PENDLE_SWAP_PT_AND_TOKEN,
        PENDLE_SWAP_YT_AND_SY_TOPIC as PENDLE_SWAP_YT_AND_SY,
        PENDLE_SWAP_YT_AND_TOKEN_TOPIC as PENDLE_SWAP_YT_AND_TOKEN,
        SOLIDLY_SWAP_TOPIC as SOLIDLY_SWAP,
        TRADER_JOE_LB_SWAP_LEGACY_TOPIC as TRADER_JOE_LB_SWAP_LEGACY,
        TRADER_JOE_LB_SWAP_TOPIC as TRADER_JOE_LB_SWAP, V2_SWAP_TOPIC as V2_SWAP,
        V2_SYNC_TOPIC as V2_SYNC, V3_BURN_TOPIC as V3_BURN, V3_MINT_TOPIC as V3_MINT,
        V3_SWAP_TOPIC as V3_SWAP, V4_SWAP_TOPIC as V4_SWAP,
    };

    /// All DEX event topic hashes for activity scanning.
    pub fn all_topics() -> Vec<B256> {
        vec![
            V2_SWAP,
            V2_SYNC,
            V3_SWAP,
            V3_MINT,
            V3_BURN,
            *CURVE_TOKEN_EXCHANGE,
            *CURVE_V2_TOKEN_EXCHANGE,
            *CURVE_TOKEN_EXCHANGE_UNDERLYING,
            *CURVE_V2_TOKEN_EXCHANGE_UNDERLYING,
            *BALANCER_SWAP,
            *V4_SWAP,
            *INF_CL_SWAP,
            *FLUID_SWAP,
            *METRIC_SWAP,
            *SOLIDLY_SWAP,
            *TRADER_JOE_LB_SWAP,
            *TRADER_JOE_LB_SWAP_LEGACY,
            *PENDLE_MARKET_SWAP,
            *PENDLE_SWAP_PT_AND_SY,
            *PENDLE_SWAP_YT_AND_SY,
            *PENDLE_SWAP_PT_AND_TOKEN,
            *PENDLE_SWAP_YT_AND_TOKEN,
        ]
    }
}

/// Scans block ranges for DEX pool activity using eth_getLogs.
///
/// Construct an `ActivityScanner`, configure the batch size, then call
/// `find_active_blocks` to discover which blocks in a range contain
/// DEX events. Only those blocks need full block data fetching.
pub struct ActivityScanner {
    rpc: RpcClient,
    batch_size: u64,
    /// Parallel `eth_getLogs` windows. Values above 1 are used only when the
    /// client has an unlimited provider; otherwise the scan stays serial.
    window_concurrency: usize,
}

impl ActivityScanner {
    pub fn new(rpc: RpcClient) -> Self {
        ActivityScanner {
            rpc,
            batch_size: 500,
            window_concurrency: 1,
        }
    }

    pub fn with_batch_size(mut self, n: u64) -> Self {
        self.batch_size = n.max(1);
        self
    }

    pub fn with_window_concurrency(mut self, n: usize) -> Self {
        self.window_concurrency = n.max(1);
        self
    }

    /// Find all blocks in [start_block, end_block] that have DEX pool events.
    ///
    /// Uses eth_getLogs with pool address + event topic filters, batched
    /// across the block range to respect provider-imposed range limits.
    ///
    /// Returns an empty set if no pool addresses are provided.
    ///
    /// On batch failure (e.g. range too large for provider), falls back
    /// to individual block scanning for that batch. An unlimited provider
    /// instead splits the window; it does not walk every block in a healthy
    /// wide range.
    pub async fn find_active_blocks(
        &self,
        pool_addresses: &[Address],
        start_block: u64,
        end_block: u64,
    ) -> error::Result<HashSet<u64>> {
        if pool_addresses.is_empty() {
            return Ok(HashSet::new());
        }

        let dex_topics = topics::all_topics();
        let parallel = self.window_concurrency > 1 && self.rpc.has_unlimited_provider().await;
        if parallel {
            return self
                .find_active_blocks_parallel(pool_addresses, &dex_topics, start_block, end_block)
                .await;
        }

        let mut active = HashSet::new();
        let mut current = start_block;

        while current <= end_block {
            let batch_end = (current + self.batch_size - 1).min(end_block);

            let filter = Filter::new()
                .address(pool_addresses.to_vec())
                .event_signature(dex_topics.clone())
                .from_block(current)
                .to_block(batch_end);

            match self.rpc.get_logs(&filter).await {
                Ok(logs) => {
                    for log in &logs {
                        if let Some(block_num) = log.block_number {
                            active.insert(block_num);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "Activity scan failed for blocks {current}..{batch_end}: {e:#}. \
                         Falling back to single-block scan for this batch."
                    );
                    for b in current..=batch_end {
                        let single = Filter::new()
                            .address(pool_addresses.to_vec())
                            .event_signature(dex_topics.clone())
                            .from_block(b)
                            .to_block(b);
                        if let Ok(logs) = self.rpc.get_logs(&single).await {
                            if logs.iter().any(|l| l.block_number == Some(b)) {
                                active.insert(b);
                            }
                        }
                    }
                }
            }

            if batch_end == end_block {
                break;
            }
            current = batch_end + 1;
        }

        Ok(active)
    }

    async fn find_active_blocks_parallel(
        &self,
        pool_addresses: &[Address],
        dex_topics: &[alloy::primitives::B256],
        start_block: u64,
        end_block: u64,
    ) -> error::Result<HashSet<u64>> {
        use futures::stream::{self, StreamExt};

        let mut windows = Vec::new();
        let mut current = start_block;
        while current <= end_block {
            let batch_end = (current + self.batch_size - 1).min(end_block);
            windows.push((current, batch_end));
            if batch_end == end_block {
                break;
            }
            current = batch_end + 1;
        }

        let parts: Vec<HashSet<u64>> = stream::iter(windows)
            .map(|(from, to)| async move {
                self.scan_window_splitting(pool_addresses, dex_topics, from, to)
                    .await
            })
            .buffer_unordered(self.window_concurrency.max(1))
            .collect()
            .await;

        let mut active = HashSet::new();
        for part in parts {
            active.extend(part);
        }
        Ok(active)
    }

    /// Halve a failed window. A single-block failure is skipped rather than
    /// expanding into a per-block walk of the parent range.
    async fn scan_window_splitting(
        &self,
        pool_addresses: &[Address],
        dex_topics: &[alloy::primitives::B256],
        from: u64,
        to: u64,
    ) -> HashSet<u64> {
        let filter = Filter::new()
            .address(pool_addresses.to_vec())
            .event_signature(dex_topics.to_vec())
            .from_block(from)
            .to_block(to);
        match self.rpc.get_logs(&filter).await {
            Ok(logs) => logs.iter().filter_map(|log| log.block_number).collect(),
            Err(e) if to > from => {
                tracing::warn!(
                    "Activity scan failed for blocks {from}..{to}: {e:#}. Splitting window..."
                );
                let mid = from + (to - from) / 2;
                let mut left =
                    Box::pin(self.scan_window_splitting(pool_addresses, dex_topics, from, mid))
                        .await;
                left.extend(
                    Box::pin(self.scan_window_splitting(pool_addresses, dex_topics, mid + 1, to))
                        .await,
                );
                left
            }
            Err(e) => {
                tracing::warn!("Activity scan skipping block {from}: {e:#}");
                HashSet::new()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::topics;
    use alloy::primitives::{b256, keccak256};

    /// Regression guard: the V4 Swap topic must NOT collapse onto the V3 topic.
    /// The original bug used the V3 signature string
    /// `Swap(address,address,int256,int256,uint160,uint128,int24)`, which
    /// keccak-hashes to exactly V3_SWAP, silently disabling V4 detection.
    #[test]
    fn v4_swap_topic_differs_from_v3() {
        assert_ne!(*topics::V4_SWAP, *topics::V3_SWAP);
        // Verified against v4-core PoolManager._swap:
        // emit Swap(id, msg.sender, delta.amount0, delta.amount1,
        //           result.sqrtPriceX96, result.liquidity, result.tick, swapFee)
        assert_eq!(
            *topics::V4_SWAP,
            b256!("40e9cecb9f5f1f1c5b9c97dec2917b7ee92e57ba5563708daca94dd84ad7112f")
        );
    }

    /// Pancake Infinity CL Swap topic: superset of V4's signature (adds a
    /// uint16 protocolFee word), so the two must hash to different topics and
    /// the Infinity one must be present in the activity-topic list.
    #[test]
    fn infinity_cl_swap_topic_is_distinct_and_scanned() {
        assert_ne!(*topics::INF_CL_SWAP, *topics::V4_SWAP);
        assert_ne!(*topics::INF_CL_SWAP, *topics::V3_SWAP);
        assert_eq!(
            *topics::INF_CL_SWAP,
            keccak256("Swap(bytes32,address,int128,int128,uint160,uint128,int24,uint24,uint16)")
        );
        assert!(topics::all_topics().contains(&*topics::INF_CL_SWAP));
    }
}
