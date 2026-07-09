//! Block range resolution — converts user-facing range modes (days, blocks, single, range) into concrete block numbers.

use chrono::{DateTime, Utc};

use crate::error;
use crate::rpc::RpcClient;
use crate::types::RangeMode;

#[derive(Debug, Clone)]
pub struct ResolvedRange {
    pub start_block: u64,
    pub end_block: u64,
    pub block_count: u64,
    pub mode: RangeMode,
}

impl ResolvedRange {
    pub fn mode_string(&self) -> String {
        match &self.mode {
            RangeMode::Days(n) => format!("days_{}", n),
            RangeMode::Blocks(n) => format!("blocks_{}", n),
            RangeMode::Single(n) => format!("block_{}", n),
            RangeMode::Range(a, b) => format!("range_{}_{}", a, b),
        }
    }

    pub fn summary(&self) -> String {
        format!(
            "Resolved range: blocks {}–{} ({} blocks)",
            self.start_block, self.end_block, self.block_count
        )
    }
}

pub struct RangeResolver {
    rpc: RpcClient,
}

impl RangeResolver {
    pub fn new(rpc: RpcClient) -> Self {
        RangeResolver { rpc }
    }

    pub fn rpc_client(&self) -> &RpcClient {
        &self.rpc
    }

    pub async fn resolve(&self, mode: &RangeMode) -> error::Result<ResolvedRange> {
        match mode {
            RangeMode::Days(n) => self.resolve_days(*n).await,
            RangeMode::Blocks(n) => self.resolve_blocks(*n).await,
            RangeMode::Single(n) => Ok(ResolvedRange {
                start_block: *n,
                end_block: *n,
                block_count: 1,
                mode: *mode,
            }),
            RangeMode::Range(a, b) => Ok(ResolvedRange {
                start_block: *a,
                end_block: *b,
                block_count: b - a + 1,
                mode: *mode,
            }),
        }
    }

    async fn resolve_days(&self, days: u64) -> error::Result<ResolvedRange> {
        let tip = self.rpc.get_block_number().await?;
        let tip_ts = self.rpc.get_block_timestamp(tip).await?;
        let target_ts = tip_ts.saturating_sub(days * 86400);

        let estimated_blocks = self.estimate_window_blocks(tip, tip_ts, target_ts).await?;
        let estimated_start = tip.saturating_sub(estimated_blocks);
        let margin = (estimated_blocks / 2).max(100_000);
        let lo = estimated_start.saturating_sub(margin);
        let hi = std::cmp::min(estimated_start + margin, tip);

        let start = self
            .binary_search_timestamp(target_ts, lo, hi)
            .await?;

        let start_dt: DateTime<Utc> =
            DateTime::from_timestamp(
                self.rpc.get_block_timestamp(start).await? as i64,
                0,
            )
            .unwrap_or_default();
        let end_dt: DateTime<Utc> =
            DateTime::from_timestamp(
                self.rpc.get_block_timestamp(tip).await? as i64,
                0,
            )
            .unwrap_or_default();

        tracing::info!(
            "Days filter resolved: blocks {}–{} ({} blocks, from {} to {})",
            start,
            tip,
            tip - start + 1,
            start_dt.format("%Y-%m-%d"),
            end_dt.format("%Y-%m-%d"),
        );

        Ok(ResolvedRange {
            start_block: start,
            end_block: tip,
            block_count: tip - start + 1,
            mode: RangeMode::Days(days),
        })
    }

    async fn resolve_blocks(&self, blocks: u64) -> error::Result<ResolvedRange> {
        let tip = self.rpc.get_block_number().await?;
        let start = if blocks > tip { 0 } else { tip - blocks + 1 };

        tracing::info!(
            "Blocks filter resolved: blocks {}–{} ({} blocks)",
            start,
            tip,
            tip - start + 1,
        );

        Ok(ResolvedRange {
            start_block: start,
            end_block: tip,
            block_count: tip - start + 1,
            mode: RangeMode::Blocks(blocks),
        })
    }

    /// Estimate the number of blocks in the time window `[target_ts, tip_ts]`
    /// by sampling the last 1000 blocks (or fewer if the chain is young) and
    /// extrapolating the block production rate.
    ///
    /// Uses integer arithmetic throughout so sub-second block times are handled
    /// correctly without precision loss: `estimated_blocks = total_elapsed * sample_count / sample_elapsed`.
    async fn estimate_window_blocks(&self, tip: u64, tip_ts: u64, target_ts: u64) -> error::Result<u64> {
        const SAMPLE_SIZE: u64 = 1000;
        let sample = if tip > SAMPLE_SIZE { tip - SAMPLE_SIZE } else { 0 };
        let sample_ts = self.rpc.get_block_timestamp(sample).await?;
        let sample_elapsed = tip_ts.saturating_sub(sample_ts);
        let sample_count = tip - sample;
        let total_elapsed = tip_ts.saturating_sub(target_ts);
        if sample_elapsed == 0 {
            Ok(sample_count)
        } else {
            Ok(total_elapsed * sample_count / sample_elapsed)
        }
    }

    async fn binary_search_timestamp(
        &self,
        target_ts: u64,
        mut lo: u64,
        mut hi: u64,
    ) -> error::Result<u64> {
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let ts = self.rpc.get_block_timestamp(mid).await?;
            if ts < target_ts {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        Ok(lo)
    }
}


