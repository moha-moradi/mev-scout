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
        let now = Utc::now().timestamp() as u64;
        let target_ts = now - days * 86400;

        let start = self
            .binary_search_timestamp(target_ts, 0, tip)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::RangeMode;

    #[test]
    fn test_mode_string_days() {
        let r = ResolvedRange { start_block: 10, end_block: 20, block_count: 11, mode: RangeMode::Days(7) };
        assert_eq!(r.mode_string(), "days_7");
    }

    #[test]
    fn test_mode_string_blocks() {
        let r = ResolvedRange { start_block: 100, end_block: 199, block_count: 100, mode: RangeMode::Blocks(100) };
        assert_eq!(r.mode_string(), "blocks_100");
    }

    #[test]
    fn test_mode_string_single() {
        let r = ResolvedRange { start_block: 42, end_block: 42, block_count: 1, mode: RangeMode::Single(42) };
        assert_eq!(r.mode_string(), "block_42");
    }

    #[test]
    fn test_mode_string_range() {
        let r = ResolvedRange { start_block: 5, end_block: 10, block_count: 6, mode: RangeMode::Range(5, 10) };
        assert_eq!(r.mode_string(), "range_5_10");
    }

    #[test]
    fn test_summary() {
        let r = ResolvedRange { start_block: 1000, end_block: 2000, block_count: 1001, mode: RangeMode::Blocks(1001) };
        let s = r.summary();
        assert!(s.contains("1000"));
        assert!(s.contains("2000"));
        assert!(s.contains("1001"));
    }
}
