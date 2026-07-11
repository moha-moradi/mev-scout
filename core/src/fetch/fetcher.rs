use std::ops::Deref;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;

use futures::future::try_join_all;
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use tokio::sync::Semaphore;

use alloy::primitives::Address;

use crate::cache::SqliteStore;
use crate::data::{BlockData, ReceiptData, TxData};
use crate::resolver::ResolvedRange;
use crate::rpc::client::extract_selector;
use crate::rpc::RpcClient;
use crate::pipeline::ActivityScanner;
use crate::sigs::SignatureResolver;

type WriteBatch = Vec<(u64, BlockData, Vec<TxData>, Vec<ReceiptData>)>;

/// Helper type: per-transaction signature data (4-byte selector, optional method name).
type TxSigEntry = ([u8; 4], Option<String>);
/// Helper type: per-receipt per-log event signature.
type EventSigEntry = Option<String>;

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
    sig_resolver: Option<Arc<SignatureResolver>>,
    parallelism: usize,
    block_concurrency: usize,
    timing: Arc<Mutex<FetchTiming>>,
    write_buf: Arc<Mutex<WriteBatch>>,
    batch_rpc: bool,
}

impl Fetcher {
    pub fn new(rpc: RpcClient, cache: SqliteStore) -> Self {
        Fetcher {
            rpc,
            cache,
            sig_resolver: None,
            parallelism: 1,
            block_concurrency: 1,
            timing: Arc::new(Mutex::new(FetchTiming::default())),
            write_buf: Arc::new(Mutex::new(Vec::new())),
            batch_rpc: true,
        }
    }

    pub fn with_parallelism(mut self, n: usize) -> Self {
        self.parallelism = n.max(1);
        self
    }

    /// Set how many blocks are fetched concurrently within a single contiguous range.
    /// Default is 1 (sequential). Increase to pipeline RPC requests for higher throughput.
    /// The RPC client's rate limiter will still throttle to the configured RPS limit.
    pub fn with_block_concurrency(mut self, n: usize) -> Self {
        self.block_concurrency = n.max(1);
        self
    }

    pub fn with_batch_rpc(mut self, enabled: bool) -> Self {
        self.batch_rpc = enabled;
        self
    }

    /// Enable signature resolution. When set, 4-byte selectors and event topic
    /// hashes are resolved to human-readable names during fetch.
    pub fn with_sig_resolver(mut self, resolver: SignatureResolver) -> Self {
        self.sig_resolver = Some(Arc::new(resolver));
        self
    }

    pub fn rpc_client(&self) -> &RpcClient {
        &self.rpc
    }

    pub fn cache_store(&self) -> &SqliteStore {
        &self.cache
    }

    /// Fetch all blocks in the range, using batch missing-block query + contiguous range batching.
    ///
    /// 1. Single DB query to determine exactly which blocks are missing
    /// 2. Group missing blocks into contiguous runs
    /// 3. Distribute each run across providers by weight, pinning each shard to its provider
    /// 4. Fetch each provider shard concurrently
    /// 5. Wait for all range tasks, integrity check
    pub async fn fetch_range<F: Fn() + Sync>(
        &self,
        range: &ResolvedRange,
        progress: Option<&F>,
    ) -> anyhow::Result<FetchSummary> {
        let start = Instant::now();

        // Phase 1: Single DB query — determine exactly which blocks are missing
        let missing = self.cache.missing_blocks_in_range(range.start_block, range.end_block)?;
        let cached_count = range.block_count.saturating_sub(missing.len() as u64);

        if missing.is_empty() {
            return Ok(FetchSummary {
                total_blocks: range.block_count,
                fetched: 0,
                cached: cached_count,
                elapsed_secs: start.elapsed().as_secs_f64(),
                ..Default::default()
            });
        }

        // Phase 2: Group missing blocks into contiguous runs
        let ranges = crate::cache::SqliteStore::contiguous_ranges(&missing);

        // Phase 3: For each contiguous run, distribute across providers by weight
        let cap = self.parallelism.min(50).max(1);
        let semaphore = Arc::new(Semaphore::new(cap));

        let summary = Arc::new(tokio::sync::Mutex::new(FetchSummary {
            total_blocks: range.block_count,
            cached: cached_count,
            ..Default::default()
        }));

        let timing = self.timing.clone();
        let fetch = self as *const Self;

        let mut range_handles = Vec::new();
        let mut shards_info = Vec::new();

        for &(run_start, run_end) in &ranges {
            // Distribute this contiguous run across providers
            let shards = unsafe { &*fetch }.rpc.distribute_blocks(run_start, run_end).await;

            for &(provider_idx, shard_start, shard_end) in &shards {
                shards_info.push((provider_idx, shard_start, shard_end));
                let sem = semaphore.clone();
                let s = summary.clone();
                let t = timing.clone();
                range_handles.push(async move {
                    let _permit = sem.acquire_owned().await?;
                    let n = unsafe { &*fetch }
                        .fetch_contiguous_range_pinned(shard_start, shard_end, provider_idx, t, progress)
                        .await?;
                    let mut sum = s.lock().await;
                    sum.fetched += n;
                    if let Some(tick) = progress {
                        tick();
                    }
                    Ok::<_, anyhow::Error>(())
                });
            }
        }

        tracing::info!(
            "fetch_range: distributing {} missing blocks across {} provider shards: {}",
            missing.len(),
            shards_info.len(),
            shards_info
                .iter()
                .map(|(pi, s, e)| format!("p{}[{}-{}]({})", pi, s, e, e - s + 1))
                .collect::<Vec<_>>()
                .join(", "),
        );

        try_join_all(range_handles).await.map_err(|e| {
            tracing::warn!("Range fetch task failed: {e:#}");
            e
        })?;

        // Flush any remaining buffered writes
        self.flush_write_buf()?;

        // Integrity check
        let missing_after = self
            .cache
            .check_integrity(range.start_block, range.end_block)?;

        let t_flush = Instant::now();
        self.cache.flush()?;
        let flush_ms = t_flush.elapsed().as_secs_f64() * 1000.0;

        let mut final_summary = summary.lock().await.deref().clone();
        final_summary.missing_after_fetch = missing_after;
        final_summary.elapsed_secs = start.elapsed().as_secs_f64();
        if let Ok(t) = self.timing.lock() {
            final_summary.timing = t.clone();
        }
        tracing::info!(
            "fetch_range: {} blocks ({}/{}) shards={} flush={:.1}ms total={:.1}s | {}",
            final_summary.total_blocks, final_summary.fetched, final_summary.cached,
            shards_info.len(), flush_ms, final_summary.elapsed_secs,
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
        self.cache.put_block_data_batch(&batch, None, None)
    }

    /// Fetch a contiguous range of blocks concurrently (no has_block checks).
    ///
    /// All blocks in `start..=end` are assumed missing — the caller must have
    /// filtered them already (e.g. via `missing_blocks_in_range`).
    /// Returns the number of blocks successfully fetched.
    ///
    /// Blocks are fetched concurrently up to `self.block_concurrency` at a time.
    /// The RPC client's rate limiter enforces the RPS limit across all concurrent
    /// workers, so the effective throughput is bottlenecked by RPS × latency,
    /// not by sequential blocking.
    async fn fetch_contiguous_range<F: Fn() + Sync>(
        &self,
        start: u64,
        end: u64,
        timing: Arc<Mutex<FetchTiming>>,
        progress: Option<&F>,
    ) -> anyhow::Result<u64> {
        let total_blocks = end.saturating_sub(start) + 1;
        let cap = self.block_concurrency.max(1).min(total_blocks as usize);
        if cap <= 1 {
            // Fall back to simple sequential fetch when concurrency is 1
            return self.fetch_contiguous_range_sequential(start, end, timing, progress).await;
        }

        // Clone shared state for concurrent tasks
        let rpc = self.rpc.clone();
        let cache = self.cache.clone();
        let write_buf = self.write_buf.clone();
        let batch_rpc = self.batch_rpc;
        let sig_resolver = self.sig_resolver.clone();

        let sem = Arc::new(Semaphore::new(cap));
        let mut tasks: FuturesUnordered<_> = FuturesUnordered::new();
        let mut completed = 0u64;

        for block_num in start..=end {
            let permit = sem.clone().acquire_owned().await?;
            let rpc = rpc.clone();
            let cache = cache.clone();
            let write_buf = write_buf.clone();
            let t = timing.clone();
            let sig = sig_resolver.clone();

            tasks.push(async move {
                let _permit = permit;
                let t0 = Instant::now();

                let (block, txs, receipts) = if batch_rpc {
                    rpc.get_block_and_receipts_batch(block_num).await?
                } else {
                    let (block_res, receipts_res) = tokio::join!(
                        rpc.get_block(block_num),
                        rpc.get_receipts(block_num),
                    );
                    let (block, txs) = block_res?;
                    (block, txs, receipts_res?)
                };
                let t_rpc = t0.elapsed().as_secs_f64() * 1000.0;

                let t2 = Instant::now();
                if let Some(ref resolver) = sig {
                    let resolver: &SignatureResolver = resolver.as_ref();
                    let tx_sigs = resolve_tx_sigs(&txs, resolver);
                    let event_sigs = resolve_event_sigs(&receipts, resolver);
                    cache.put_block_data(block_num, &block, &txs, &receipts, Some(&tx_sigs), Some(&event_sigs))?;
                } else {
                    let mut buf = write_buf.lock().expect("write_buf mutex poisoned");
                    buf.push((block_num, block, txs, receipts));
                    if buf.len() >= 10 {
                        let batch = std::mem::take(&mut *buf);
                        drop(buf);
                        cache.put_block_data_batch(&batch, None, None)?;
                    }
                }
                let t_write = t2.elapsed().as_secs_f64() * 1000.0;

                let total = t0.elapsed().as_secs_f64() * 1000.0;
                if let Ok(mut tm) = t.lock() {
                    tm.record(0.0, t_rpc, t_write, total);
                }

                Ok::<_, anyhow::Error>(())
            });

            // Drain completed tasks as they finish to keep memory bounded
            // Poll whenever we hit the concurrency cap or it's the last block
            while tasks.len() >= cap || (block_num == end && !tasks.is_empty()) {
                match tasks.next().await {
                    Some(Ok(())) => {
                        if let Some(tick) = progress { tick(); }
                        completed += 1;
                    }
                    Some(Err(e)) => return Err(e),
                    None => break,
                }
            }
        }

        // Drain remaining tasks
        while let Some(result) = tasks.next().await {
            match result {
                Ok(()) => {
                    if let Some(tick) = progress { tick(); }
                    completed += 1;
                }
                Err(e) => return Err(e),
            }
        }

        Ok(completed)
    }

    /// Sequential fallback for `fetch_contiguous_range` when `block_concurrency` is 1.
    async fn fetch_contiguous_range_sequential<F: Fn() + Sync>(
        &self,
        start: u64,
        end: u64,
        timing: Arc<Mutex<FetchTiming>>,
        progress: Option<&F>,
    ) -> anyhow::Result<u64> {
        let mut fetched = 0u64;
        for block_num in start..=end {
            let t0 = Instant::now();
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
            let t_rpc = t0.elapsed().as_secs_f64() * 1000.0;

            let t2 = Instant::now();
            if let Some(ref resolver) = self.sig_resolver {
                let resolver: &SignatureResolver = resolver.as_ref();
                let tx_sigs = resolve_tx_sigs(&txs, resolver);
                let event_sigs = resolve_event_sigs(&receipts, resolver);
                self.cache.put_block_data(block_num, &block, &txs, &receipts, Some(&tx_sigs), Some(&event_sigs))?;
            } else {
                let mut buf = self.write_buf.lock().expect("write_buf mutex poisoned");
                buf.push((block_num, block, txs, receipts));
                if buf.len() >= 10 {
                    let batch = std::mem::take(&mut *buf);
                    drop(buf);
                    self.cache.put_block_data_batch(&batch, None, None)?;
                }
            }
            let t_write = t2.elapsed().as_secs_f64() * 1000.0;

            let total = t0.elapsed().as_secs_f64() * 1000.0;
            if let Ok(mut t) = timing.lock() {
                t.record(0.0, t_rpc, t_write, total);
            }
            if let Some(tick) = progress {
                tick();
            }
            fetched += 1;
        }
        Ok(fetched)
    }

    /// Fetch a contiguous range of blocks concurrently, pinned to a specific provider.
    ///
    /// Same as `fetch_contiguous_range` but calls `rpc.get_block_and_receipts_batch_for`
    /// to use a specific provider instead of weighted random selection.
    async fn fetch_contiguous_range_pinned<F: Fn() + Sync>(
        &self,
        start: u64,
        end: u64,
        provider_idx: usize,
        timing: Arc<Mutex<FetchTiming>>,
        progress: Option<&F>,
    ) -> anyhow::Result<u64> {
        let total_blocks = end.saturating_sub(start) + 1;
        // Size the semaphore to the provider's RPS rather than the global
        // block_concurrency. This avoids spawning more tasks than the rate
        // limiter can serve, reducing queueing overhead and memory pressure.
        let provider_rps = self.rpc.get_provider_rps(provider_idx).await;
        let cap = (self.block_concurrency.max(1))
            .min(provider_rps.ceil() as usize)
            .min(total_blocks as usize)
            .max(1);

        let rpc = self.rpc.clone();
        let cache = self.cache.clone();
        let write_buf = self.write_buf.clone();
        let batch_rpc = self.batch_rpc;
        let sig_resolver = self.sig_resolver.clone();

        let sem = Arc::new(Semaphore::new(cap));
        let mut tasks: FuturesUnordered<_> = FuturesUnordered::new();
        let mut completed = 0u64;

        for block_num in start..=end {
            let permit = sem.clone().acquire_owned().await?;
            let rpc = rpc.clone();
            let cache = cache.clone();
            let write_buf = write_buf.clone();
            let t = timing.clone();
            let sig = sig_resolver.clone();

            tasks.push(async move {
                let _permit = permit;
                let t0 = Instant::now();

                let (block, txs, receipts) = if batch_rpc {
                    rpc.get_block_and_receipts_batch_for(provider_idx, block_num).await?
                } else {
                    let (block_res, receipts_res) = tokio::join!(
                        rpc.get_block(block_num),
                        rpc.get_receipts(block_num),
                    );
                    let (block, txs) = block_res?;
                    (block, txs, receipts_res?)
                };
                let t_rpc = t0.elapsed().as_secs_f64() * 1000.0;

                let t2 = Instant::now();
                if let Some(ref resolver) = sig {
                    let resolver: &SignatureResolver = resolver.as_ref();
                    let tx_sigs = resolve_tx_sigs(&txs, resolver);
                    let event_sigs = resolve_event_sigs(&receipts, resolver);
                    cache.put_block_data(block_num, &block, &txs, &receipts, Some(&tx_sigs), Some(&event_sigs))?;
                } else {
                    let mut buf = write_buf.lock().expect("write_buf mutex poisoned");
                    buf.push((block_num, block, txs, receipts));
                    if buf.len() >= 10 {
                        let batch = std::mem::take(&mut *buf);
                        drop(buf);
                        cache.put_block_data_batch(&batch, None, None)?;
                    }
                }
                let t_write = t2.elapsed().as_secs_f64() * 1000.0;

                let total = t0.elapsed().as_secs_f64() * 1000.0;
                if let Ok(mut tm) = t.lock() {
                    tm.record(0.0, t_rpc, t_write, total);
                }

                Ok::<_, anyhow::Error>(())
            });

            while tasks.len() >= cap || (block_num == end && !tasks.is_empty()) {
                match tasks.next().await {
                    Some(Ok(())) => {
                        if let Some(tick) = progress { tick(); }
                        completed += 1;
                    }
                    Some(Err(e)) => return Err(e),
                    None => break,
                }
            }
        }

        while let Some(result) = tasks.next().await {
            match result {
                Ok(()) => {
                    if let Some(tick) = progress { tick(); }
                    completed += 1;
                }
                Err(e) => return Err(e),
            }
        }

        Ok(completed)
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
        if let Some(ref resolver) = self.sig_resolver {
            let tx_sigs = resolve_tx_sigs(&txs, resolver);
            let event_sigs = resolve_event_sigs(&receipts, resolver);
            self.cache.put_block_data(block_num, &block, &txs, &receipts, Some(&tx_sigs), Some(&event_sigs))?;
        } else {
            let mut buf = self.write_buf.lock().expect("write_buf mutex poisoned");
            buf.push((block_num, block, txs, receipts));
            if buf.len() >= 10 {
                let batch = std::mem::take(&mut *buf);
                drop(buf);
                self.cache.put_block_data_batch(&batch, None, None)?;
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

        // Phase 2: Group active blocks into contiguous ranges and fetch
        let mut sorted: Vec<u64> = active_blocks.into_iter().collect();
        sorted.sort_unstable();
        let ranges = crate::cache::SqliteStore::contiguous_ranges(&sorted);

        let cap = self.parallelism.min(50).max(1);
        let semaphore = Arc::new(Semaphore::new(cap));

        let summary = Arc::new(tokio::sync::Mutex::new(FetchSummary {
            total_blocks: range.block_count,
            scanned: range.block_count,
            relevant,
            skipped,
            ..Default::default()
        }));

        let timing = self.timing.clone();
        let fetch = self as *const Self;
        let mut range_handles = Vec::new();
        for &(run_start, run_end) in &ranges {
            let sem = semaphore.clone();
            let s = summary.clone();
            let t = timing.clone();
            range_handles.push(async move {
                let _permit = sem.acquire_owned().await?;
                let n = unsafe { &*fetch }.fetch_contiguous_range(run_start, run_end, t, progress).await?;
                let mut sum = s.lock().await;
                sum.fetched += n;
                Ok::<_, anyhow::Error>(())
            });
        }

        try_join_all(range_handles).await?;

        // Flush any remaining buffered writes
        self.flush_write_buf()?;

        // Integrity check only on the blocks we attempted to fetch
        let missing_after = self
            .cache
            .check_integrity_range(&sorted)?;

        let mut final_summary = summary.lock().await.deref().clone();
        final_summary.missing_after_fetch = missing_after;
        final_summary.elapsed_secs = start.elapsed().as_secs_f64();
        if let Ok(t) = self.timing.lock() {
            final_summary.timing = t.clone();
        }
        self.cache.flush()?;
        Ok(final_summary)
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

// ---- Signature resolution helpers (Phase 3) ----

/// Resolve 4-byte selectors for all transactions in a block.
fn resolve_tx_sigs(txs: &[TxData], resolver: &SignatureResolver) -> Vec<TxSigEntry> {
    txs.iter()
        .map(|tx| {
            match extract_selector(&tx.input) {
                Some(sel) => {
                    let name = resolver.resolve_method(&sel).ok().flatten();
                    (sel, name)
                }
                None => ([0u8; 4], None),
            }
        })
        .collect()
}

/// Resolve event signatures for all logs in a block's receipts.
fn resolve_event_sigs(receipts: &[ReceiptData], resolver: &SignatureResolver) -> Vec<Vec<EventSigEntry>> {
    receipts
        .iter()
        .map(|r| {
            r.logs
                .iter()
                .map(|log| {
                    log.topics
                        .first()
                        .and_then(|topic| resolver.resolve_event(topic).ok().flatten())
                })
                .collect()
        })
        .collect()
}


