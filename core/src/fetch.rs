use std::ops::Deref;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;

use futures::future::try_join_all;
use tokio::sync::Semaphore;

use alloy::primitives::Address;

use crate::cache::SqliteStore;
use crate::data::{BlockData, ReceiptData, TxData};
use crate::parquet_writer::ParquetWriter;
use crate::resolver::ResolvedRange;
use crate::rpc::RpcClient;
use crate::scan::ActivityScanner;

type WriteBatch = Vec<(u64, BlockData, Vec<TxData>, Vec<ReceiptData>)>;

#[derive(Debug, Default, Clone)]
pub struct FetchTiming {
    pub has_block_ms: f64,
    pub get_block_ms: f64,
    pub get_receipts_ms: f64,
    pub write_ms: f64,
    pub total_ms: f64,
    pub count: u64,
}

impl FetchTiming {
    fn record(&mut self, has_block: f64, rpc: f64, write: f64, total: f64) {
        self.has_block_ms += has_block;
        self.get_block_ms += rpc;
        self.write_ms += write;
        self.total_ms += total;
        self.count += 1;
    }

    pub fn summary(&self) -> String {
        if self.count == 0 {
            return "no data".to_string();
        }
        format!(
            "blocks={} avg(has_block={:.1}ms rpc={:.1}ms write={:.1}ms total={:.1}ms)",
            self.count,
            self.has_block_ms / self.count as f64,
            self.get_block_ms / self.count as f64,
            self.write_ms / self.count as f64,
            self.total_ms / self.count as f64,
        )
    }
}

#[derive(Debug, Default, Clone)]
pub struct FetchSummary {
    pub total_blocks: u64,
    pub fetched: u64,
    pub cached: u64,
    pub scanned: u64,
    pub relevant: u64,
    pub skipped: u64,
    pub elapsed_secs: f64,
    pub missing_after_fetch: Vec<u64>,
    pub timing: FetchTiming,
}

pub struct Fetcher {
    rpc: RpcClient,
    cache: SqliteStore,
    parallelism: usize,
    parquet: Option<ParquetWriter>,
    timing: Arc<Mutex<FetchTiming>>,
    write_buf: Arc<Mutex<WriteBatch>>,
    batch_rpc: bool,
}

impl Fetcher {
    pub fn new(rpc: RpcClient, cache: SqliteStore) -> Self {
        Fetcher {
            rpc,
            cache,
            parallelism: 1,
            parquet: None,
            timing: Arc::new(Mutex::new(FetchTiming::default())),
            write_buf: Arc::new(Mutex::new(Vec::new())),
            batch_rpc: true,
        }
    }

    pub fn with_parallelism(mut self, n: usize) -> Self {
        self.parallelism = n.max(1);
        self
    }

    pub fn with_batch_rpc(mut self, enabled: bool) -> Self {
        self.batch_rpc = enabled;
        self
    }

    /// Enable Parquet output. Writes one file per type under `dir`.
    pub fn with_parquet(mut self, dir: impl AsRef<Path>) -> Self {
        self.parquet = Some(ParquetWriter::new(dir));
        self
    }

    pub fn rpc_client(&self) -> &RpcClient {
        &self.rpc
    }

    pub fn cache_store(&self) -> &SqliteStore {
        &self.cache
    }

    /// Write all cached data in `start..=end` to Parquet files.
    fn flush_parquet(&self, start: u64, end: u64) -> anyhow::Result<()> {
        let pw = match &self.parquet {
            Some(pw) => pw,
            None => return Ok(()),
        };

        let mut blocks = Vec::new();
        let mut all_txs = Vec::new();
        let mut all_receipts = Vec::new();
        for n in start..=end {
            if let Some(block) = self.cache.get_block(n)? {
                blocks.push(block);
            }
            if let Some(txs) = self.cache.get_txs(n)? {
                all_txs.push(txs);
            }
            if let Some(receipts) = self.cache.get_receipts(n)? {
                all_receipts.push(receipts);
            }
        }

        if !blocks.is_empty() {
            pw.write_all_blocks(&blocks)?;
        }
        if !all_txs.is_empty() {
            pw.write_all_txs(&all_txs)?;
        }
        if !all_receipts.is_empty() {
            pw.write_all_receipts(&all_receipts)?;
        }

        tracing::info!(
            "Wrote Parquet cache: {} blocks, {} tx batches, {} receipt batches",
            blocks.len(),
            all_txs.len(),
            all_receipts.len(),
        );
        Ok(())
    }

    /// Fetch all blocks in the range, distributing work across providers.
    ///
    /// Uses `RpcClient::distribute_blocks()` to shard the range by provider weight.
    /// Each shard runs with its own concurrency semaphore. Results are merged into
    /// a single `FetchSummary`.
    pub async fn fetch_range<F: Fn() + Sync>(
        &self,
        range: &ResolvedRange,
        _progress: Option<&F>,
    ) -> anyhow::Result<FetchSummary> {
        let start = Instant::now();
        let cap = self.parallelism.min(50);

        let summary = Arc::new(tokio::sync::Mutex::new(FetchSummary {
            total_blocks: range.block_count,
            ..Default::default()
        }));

        // Distribute block range across providers
        let shards = self.rpc.distribute_blocks(range.start_block, range.end_block).await;

        let mut shard_handles = Vec::new();
        for (_provider_idx, blocks) in &shards {
            if blocks.is_empty() {
                continue;
            }

            // Allocate semaphore capacity proportional to provider weight
            let total_weight: f64 = shards.iter().map(|(_, b)| b.len() as f64).sum();
            let shard_cap = if total_weight > 0.0 {
                ((blocks.len() as f64 / total_weight) * cap as f64).ceil().max(1.0) as usize
            } else {
                1
            };
            let semaphore = Arc::new(Semaphore::new(shard_cap.min(cap)));

            let timing = self.timing.clone();
            let fetch = self as *const Self; // safe: &self outlives tasks

            for &block_num in blocks {
                let sem = semaphore.clone();
                let t = timing.clone();
                let summary_clone = summary.clone();
                shard_handles.push(async move {
                    let _permit = sem.acquire_owned().await?;
                    let fetched = unsafe { &*fetch }.fetch_one_block(block_num, t).await?;
                    let mut s = summary_clone.lock().await;
                    if fetched {
                        s.fetched += 1;
                    } else {
                        s.cached += 1;
                    }
                    Ok::<_, anyhow::Error>(())
                });
            }
        }

        try_join_all(shard_handles).await.map_err(|e| {
            tracing::warn!("Block fetch task failed: {e:#}");
            e
        })?;

        // Flush any remaining buffered writes, then write Parquet
        self.flush_write_buf()?;
        self.flush_parquet(range.start_block, range.end_block)?;

        // Integrity check
        let missing = self
            .cache
            .check_integrity(range.start_block, range.end_block)?;

        let t_flush = Instant::now();
        self.cache.flush()?;
        let flush_ms = t_flush.elapsed().as_secs_f64() * 1000.0;

        let mut final_summary = summary.lock().await.deref().clone();
        final_summary.missing_after_fetch = missing;
        final_summary.elapsed_secs = start.elapsed().as_secs_f64();
        if let Ok(t) = self.timing.lock() {
            final_summary.timing = t.clone();
        }
        tracing::info!(
            "fetch_range: {} blocks ({}/{}) flush={:.1}ms total={:.1}s | {}",
            final_summary.total_blocks, final_summary.fetched, final_summary.cached,
            flush_ms, final_summary.elapsed_secs,
            final_summary.timing.summary(),
        );
        Ok(final_summary)
    }

    fn flush_write_buf(&self) -> anyhow::Result<()> {
        let batch = {
            let mut buf = self.write_buf.lock().expect("write_buf mutex poisoned");
            if buf.is_empty() {
                return Ok(());
            }
            std::mem::take(&mut *buf)
        };
        self.cache.put_block_data_batch(&batch)
    }

    async fn fetch_one_block(
        &self,
        block_num: u64,
        timing: Arc<Mutex<FetchTiming>>,
    ) -> anyhow::Result<bool> {
        let t0 = Instant::now();
        let cached = self.cache.has_block(block_num)?;
        let t_has_block = t0.elapsed().as_secs_f64() * 1000.0;
        if cached {
            return Ok(false);
        }

        let t1 = Instant::now();
        let (block, txs, receipts) = if self.batch_rpc {
            self.rpc.get_block_and_receipts_batch(block_num).await?
        } else {
            let (block_res, receipts_res) = tokio::join!(
                self.rpc.get_block(block_num),
                self.rpc.get_receipts(block_num),
            );
            let (block, txs) = block_res?;
            (block, txs, receipts_res?)
        };
        let t_rpc = t1.elapsed().as_secs_f64() * 1000.0;

        let t2 = Instant::now();
        {
            let mut buf = self.write_buf.lock().expect("write_buf mutex poisoned");
            buf.push((block_num, block, txs, receipts));
            if buf.len() >= 10 {
                let batch = std::mem::take(&mut *buf);
                drop(buf);
                self.cache.put_block_data_batch(&batch)?;
            }
        }
        let t_write = t2.elapsed().as_secs_f64() * 1000.0;

        let total = t0.elapsed().as_secs_f64() * 1000.0;
        if let Ok(mut t) = timing.lock() {
            t.record(t_has_block, t_rpc, t_write, total);
        }
        Ok(true)
    }

    /// Fetch only blocks that have DEX pool activity.
    ///
    /// Uses `ActivityScanner` to first identify relevant blocks via `eth_getLogs`,
    /// then only fetches full block data for those blocks. Blocks without any
    /// DEX events are skipped entirely.
    ///
    /// Falls back to `fetch_range()` (all blocks) when `pool_addresses` is empty.
    pub async fn fetch_relevant<F: Fn() + Sync>(
        &self,
        range: &ResolvedRange,
        pool_addresses: &[Address],
        progress: Option<&F>,
    ) -> anyhow::Result<FetchSummary> {
        if pool_addresses.is_empty() {
            tracing::info!("No pool addresses provided, fetching all blocks");
            return self.fetch_range(range, progress).await;
        }

        let start = Instant::now();

        // Phase 1: Scan for DEX-active blocks
        tracing::info!(
            "Scanning {} blocks for DEX activity using {} pool addresses...",
            range.block_count,
            pool_addresses.len(),
        );
        let scanner = ActivityScanner::new(self.rpc.clone());
        let active_blocks = scanner
            .find_active_blocks(pool_addresses, range.start_block, range.end_block)
            .await?;

        let relevant = active_blocks.len() as u64;
        let skipped = range.block_count.saturating_sub(relevant);

        tracing::info!(
            "Activity scan complete: {} relevant blocks ({} skipped — no DEX events)",
            relevant,
            skipped,
        );

        if active_blocks.is_empty() {
            return Ok(FetchSummary {
                total_blocks: range.block_count,
                scanned: range.block_count,
                relevant: 0,
                skipped: range.block_count,
                ..Default::default()
            });
        }

        // Phase 2: Fetch only the active blocks
        let mut sorted: Vec<u64> = active_blocks.into_iter().collect();
        sorted.sort_unstable();

        let cap = self.parallelism.min(50);
        let semaphore = Arc::new(Semaphore::new(cap));

        let mut summary = FetchSummary {
            total_blocks: range.block_count,
            scanned: range.block_count,
            relevant,
            skipped,
            ..Default::default()
        };

        let timing = self.timing.clone();
        let mut tasks = Vec::new();
        for &block_num in &sorted {
            let sem = semaphore.clone();
            let t = timing.clone();
            tasks.push(async move {
                let _permit = sem.acquire_owned().await?;
                self.fetch_one_block(block_num, t).await
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

        // Flush any remaining buffered writes, then write Parquet
        self.flush_write_buf()?;
        let min = sorted.first().copied().unwrap_or(range.start_block);
        let max = sorted.last().copied().unwrap_or(range.end_block);
        self.flush_parquet(min, max)?;

        // Integrity check only on the blocks we attempted to fetch
        summary.missing_after_fetch = self
            .cache
            .check_integrity_range(&sorted)?;

        summary.elapsed_secs = start.elapsed().as_secs_f64();
        if let Ok(t) = self.timing.lock() {
            summary.timing = t.clone();
        }
        self.cache.flush()?;
        Ok(summary)
    }

    pub async fn auto_refetch_gaps(&self, gaps: &[u64]) -> anyhow::Result<u64> {
        let mut refetched = 0u64;
        let timing = self.timing.clone();
        for &block_num in gaps {
            match self.fetch_one_block(block_num, timing.clone()).await {
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
