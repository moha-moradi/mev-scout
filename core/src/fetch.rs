//! Block data fetching with caching — downloads blocks from the RPC endpoint and stores them.

use std::sync::Arc;
use std::time::Instant;

use futures::future::try_join_all;
use tokio::sync::Semaphore;

use crate::cache::CacheStore;
use crate::resolver::ResolvedRange;
use crate::rpc::RpcClient;

#[derive(Debug, Default)]
pub struct FetchSummary {
    pub total_blocks: u64,
    pub fetched: u64,
    pub cached: u64,
    pub elapsed_secs: f64,
    pub missing_after_fetch: Vec<u64>,
}

pub struct Fetcher {
    rpc: RpcClient,
    cache: CacheStore,
    parallelism: usize,
}

impl Fetcher {
    pub fn new(rpc: RpcClient, cache: CacheStore) -> Self {
        Fetcher {
            rpc,
            cache,
            parallelism: 1,
        }
    }

    pub fn with_parallelism(mut self, n: usize) -> Self {
        self.parallelism = n.max(1);
        self
    }

    pub fn rpc_client(&self) -> &RpcClient {
        &self.rpc
    }

    pub fn cache_store(&self) -> &CacheStore {
        &self.cache
    }

    pub async fn fetch_range<F: Fn() + Sync>(
        &self,
        range: &ResolvedRange,
        progress: Option<&F>,
    ) -> anyhow::Result<FetchSummary> {
        let start = Instant::now();
        let cap = self.parallelism.min(20);
        let semaphore = Arc::new(Semaphore::new(cap));

        let mut summary = FetchSummary {
            total_blocks: range.block_count,
            ..Default::default()
        };

        // Process blocks using semaphore-based concurrency
        let mut tasks = Vec::new();
        for block_num in range.start_block..=range.end_block {
            let sem = semaphore.clone();
            tasks.push(async move {
                let _permit = sem.acquire_owned().await?;
                self.fetch_one_block(block_num).await
            });
        }

        let results: Vec<bool> = try_join_all(tasks).await?;
        for fetched in results {
            if fetched {
                summary.fetched += 1;
            } else {
                summary.cached += 1;
            }
            if let Some(tick) = progress {
                tick();
            }
        }

        // Integrity check
        summary.missing_after_fetch = self
            .cache
            .check_integrity(range.start_block, range.end_block)?;

        summary.elapsed_secs = start.elapsed().as_secs_f64();
        self.cache.flush()?;
        Ok(summary)
    }

    async fn fetch_one_block(
        &self,
        block_num: u64,
    ) -> anyhow::Result<bool> {
        if self.cache.has_block(block_num)? {
            return Ok(false);
        }
        let (block, txs) = self.rpc.get_block(block_num).await?;
        let receipts = self.rpc.get_receipts(block_num).await?;
        self.cache.put_block(block_num, &block)?;
        self.cache.put_txs(block_num, &txs)?;
        self.cache.put_receipts(block_num, &receipts)?;
        Ok(true)
    }

    pub async fn auto_refetch_gaps(&self, gaps: &[u64]) -> anyhow::Result<u64> {
        let mut refetched = 0u64;
        for &block_num in gaps {
            match self.fetch_one_block(block_num).await {
                Ok(true) => refetched += 1,
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!("Failed to refetch block {}: {}", block_num, e);
                }
            }
        }
        self.cache.flush()?;
        Ok(refetched)
    }
}


