# Performance Optimization Plan — Scaling to 30 Days of Polygon

## Current Codebase Audit

The current codebase matches the plan in several places, but the storage-backend direction needs to change from SQLite to RocksDB. The main audit findings are:

| Area | Current state | Plan implication |
|------|---------------|------------------|
| `core/Cargo.toml:19-31` | Uses `sled = "0.34"` and `bincode = "1"`; no `rayon`, `hotpath`, `lru`, SQLite, or RocksDB dependency. | Add `rayon`, profiling/dev tooling, `lru`, and `rust-rocksdb`. Keep sled only as a legacy backend. |
| `core/src/cache.rs:38-59` | `CacheStore` is a concrete sled-backed store with one flat key namespace. | Introduce a backend abstraction and add a RocksDB backend with column families. |
| `core/src/run.rs:270-291` | `run_range()` is still a sequential `for` loop. | Parallel replay is not implemented and remains the largest expected win. |
| `core/src/pool/state.rs:816-829` | `PoolManager` already implements `Clone`. | The plan's "make PoolManager clonable" requirement is already satisfied; parallel replay still needs a safe fork/clone strategy. |
| `core/src/run.rs:30-34` | `BacktestRunner` is not `Clone`. | Parallel replay must either add a safe cloning/forking method or avoid cloning the whole runner. |
| `core/src/replay.rs:109-136` | `CachedRpcDb` implements `Clone`, but still uses revm's mutable `Database` trait. | Thread-safety must be validated under Rayon; per-block DB instances should not share mutable runtime maps. |
| `core/src/replay.rs:639-650` | Polygon system logs are filtered only during receipt verification. | Phase 5d should add the same system-address filter to replay filtering, not assume it is already present. |
| `core/src/fetch.rs:59-97` | Fetch uses a fixed semaphore capped at 20 concurrent requests. | Adaptive concurrency and batch RPC are not implemented. |
| `core/src/rpc.rs:233-317` | No JSON-RPC batch API and no `get_storage_at_batch()` helper. | Phase 3 and Phase 5c need RPC-client additions before callers can batch receipts or storage slots. |
| `core/src/pool/state.rs:492-647` | Pool init uses concurrent `eth_call`/storage fallback per pool, but no reserve-history cache. | Add an L1/L2 reserve cache keyed by chain, pool, and block. |
| `core/src/config.rs:76-77` | Config has `cache_dir` only; no backend selector. | Add `cache_backend = "rocksdb" | "sled"` with RocksDB as the default after migration. |
| `cli/src/main.rs:248-250` | Opens `CacheStore` directly from `cache_dir`. | CLI/config must pass backend settings and support lazy migration from sled to RocksDB. |

### Items in the original plan that are no longer accurate

1. **SQLite should not be the recommended replacement.** The current store is already a key-value cache with prefix scans. RocksDB maps to that model with less schema impedance than SQLite.
2. **"No concurrent reader isolation" for sled should be softened.** Sled supports concurrent reads, but it still has weaker compaction control and one large tree bottleneck for this workload.
3. **Parallel replay pseudo-code assumes `BacktestRunner` can be cloned.** The current runner is not `Clone`; `PoolManager` is already `Clone`, but the runner/replayer/database fork design still needs explicit work.
4. **Batch RPC pseudo-code assumes APIs that do not exist yet.** `RpcClient` has no `batch_call()` or `get_storage_at_batch()`.
5. **System-address filtering is incomplete.** It exists for receipt comparison, but not for deciding whether a transaction should enter the EVM replay path.
6. **Hotpath commands may need fallback tooling.** If `hotpath` does not work cleanly with the current toolchain, use `cargo flamegraph`, `perf`, or tracing timers while keeping the same profiling goals.

---

## Bottleneck Analysis

| Phase | Current design | Est. cost (1.3M blocks) | % of total |
|-------|---------------|------------------------|------------|
| RPC fetch | Fixed 20-request semaphore, per-block `eth_getBlock` + `eth_getBlockReceipts` | ~3.5 hr | 10% |
| Sled I/O | Single-tree LSM, limited compaction tuning, bincode serde per access | ~2 hr | 6% |
| EVM replay (sequential) | `run_range()` iterates blocks 1-by-1, fresh EVM context per block | ~24-36 hr | 80% |
| Pool state init | `init_pools()` fetches reserves concurrently but without reserve-history cache | ~0.5 hr | 1% |
| RPC fallback (replay) | `CachedRpcDb` makes `eth_getProof` / `eth_getStorageAt` on cache miss | ~1 hr | 3% |

**Dominant term: sequential block replay.** Everything else is secondary, but RocksDB is the correct storage target before scaling replay across many threads.

---

## Phase 1: Profile First

Before any optimization, establish baselines with the same workload and cache state used for production runs.

```bash
# Install hotpath if available
cargo install hotpath

# Profile replay of a single block (micro-benchmark)
hotpath record -- cargo test --release replay_single_block -- --nocapture
hotpath report replay_single_block --flamegraph

# Profile run_range for 100 blocks (meso-benchmark)
hotpath record -- cargo run --release -- run --blocks 100 --chain polygon
hotpath report run_100_blocks --flamegraph

# Profile fetch for 1000 blocks
hotpath record -- cargo run --release -- fetch --blocks 1000 --chain polygon
hotpath report fetch_1000_blocks --flamegraph
```

Fallback profiling commands if `hotpath` is unavailable:

```bash
cargo flamegraph --bin mev-scout -- run --blocks 100 --chain polygon
cargo flamegraph --bin mev-scout -- fetch --blocks 1000 --chain polygon
```

**Profiling integration (`core/Cargo.toml`):**

```toml
[dev-dependencies]
hotpath = "0.1"
```

Add a focused benchmark suite around:

- `BlockReplayer::replay_each_filtered`
- `CachedRpcDb::basic`
- `CachedRpcDb::storage`
- `BacktestRunner::run_range`
- `PoolManager::init_from_rpc`

**What to measure:** instructions retired, cache misses, branch mispredictions, syscall count per block, RPC latency distribution, RPC error rate, and cache write amplification.

---

## Phase 2: Parallel Block Replay (Rayon)

**Biggest win.** `run_range()` at `core/src/run.rs:270-291` is a sequential `for` loop over blocks. Each block is independent from the perspective of detected opportunities, but pool-state handling must be designed carefully.

### Design

Add a parallel range runner without changing the sequential runner semantics:

```rust
// core/src/run.rs
use rayon::prelude::*;

pub fn run_range_par(
    &mut self,
    resolved: &ResolvedRange,
    thread_count: usize,
) -> anyhow::Result<Vec<MevOpportunity>> {
    let base_pool_manager = self.pool_manager.clone();
    let base_replayer = self.replayer.clone_for_block_replay();

    let mut block_results: Vec<(u64, Vec<MevOpportunity>)> =
        (resolved.start_block..=resolved.end_block)
            .collect::<Vec<_>>()
            .par_iter()
            .with_max_len(thread_count)
            .filter_map(|&block_num| {
                let mut runner = BacktestRunner::fork_for_block(
                    base_replayer.clone(),
                    base_pool_manager.clone(),
                    self.gas_config,
                );

                match runner.run_block(block_num) {
                    Ok(opps) => Some((block_num, opps)),
                    Err(e) => {
                        tracing::error!("Block {} failed: {:?}", block_num, e);
                        None
                    }
                }
            })
            .collect();

    block_results.sort_by_key(|(block_num, _)| *block_num);
    Ok(block_results.into_iter().flat_map(|(_, opps)| opps).collect())
}
```

### Requirements

1. **Add `rayon` dependency.** `core/Cargo.toml` currently has no Rayon dependency.
2. **Make replay components clonable/forkable.** `PoolManager` already implements `Clone` at `core/src/pool/state.rs:816-829`. `CachedRpcDb` already implements `Clone` at `core/src/replay.rs:121-136`, but `BacktestRunner` does not. Add explicit `Clone` or `fork_for_block()` methods only where the cloned state is safe to share.
3. **Do not share mutable EVM state.** Each worker must own its own `CacheDB<CachedRpcDb>` and its own runtime maps (`accounts`, `codes`, `storage`, `code_hash_to_address`).
4. **Preserve result ordering.** Parallel execution should return opportunities sorted by block number and transaction index, matching the sequential `run_range()` output contract.
5. **Validate thread-safety with compile-time assertions and tests.** Add tests that assert the relevant types are `Send` where Rayon requires it.

### Pool State Forking Strategy

| Option | Description | Pros | Cons |
|--------|-------------|------|------|
| **A. Snapshot fetch** | Each parallel task clones the base `PoolManager` and initializes reserve state at its block number. | Correct and simplest. | More RPC calls if reserve cache is empty. |
| **B. Reserve history cache** | Cache `getReserves()` / V3 state results in RocksDB keyed by `pool_reserves:{chain_id}:{pool}:{block}`. | Reuses work across runs and parallel workers. | More persistent writes; needs invalidation rules. |
| **C. Sequential pool forward** | Run pool state forward from the earliest block in the range and snapshot at intervals. | Fewer RPC calls. | Complex, memory-heavy, and risky for parallel correctness. |

**Recommendation:** Start with Option A for correctness, then add Option B immediately after the RocksDB backend exists. Do not implement Option C unless profiling proves reserve initialization dominates after Option B.

### Thread count heuristic

```rust
fn optimal_thread_count(range_size: u64) -> usize {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);

    let memory_limited = 8usize;
    cores.min(memory_limited).max(1)
}
```

### Expected speedup

| Cores | Speedup | Est. time (1.3M blocks) |
|-------|---------|------------------------|
| 1 (current) | 1× | 30 hr |
| 4 | 3.5× | 8.5 hr |
| 8 | 6× | 5 hr |
| 16 | 9× | 3.3 hr |

Amdahl's law: the serial fraction is currently underestimated because pool init and storage I/O are not fully parallel. Treat these numbers as targets, not guarantees.

---

## Phase 3: Smart Batched RPC Fetch

### Adaptive Concurrency

Current implementation uses a fixed semaphore capped at 20 concurrent requests in `core/src/fetch.rs:59-97`.

Replace the fixed cap with adaptive concurrency based on RPC latency and error rate:

```rust
// core/src/fetch.rs
pub struct AdaptiveFetcher {
    base_concurrency: usize,
    max_concurrency: usize,
    latency_window: VecDeque<Duration>,
    error_rate: f64,
}

impl AdaptiveFetcher {
    fn adjust_concurrency(&mut self, latency: Duration, success: bool) {
        const WINDOW: usize = 100;
        const SLOW_MS: u64 = 500;
        const FAST_MS: u64 = 200;

        self.latency_window.push_back(latency);
        if self.latency_window.len() > WINDOW {
            self.latency_window.pop_front();
        }

        let avg_latency = self.latency_window.iter().sum::<Duration>()
            / self.latency_window.len() as u32;

        if !success || avg_latency.as_millis() as u64 > SLOW_MS {
            self.base_concurrency = (self.base_concurrency / 2).max(1);
            self.error_rate = (self.error_rate * 0.9) + 0.1;
        } else if avg_latency.as_millis() as u64 < FAST_MS && self.error_rate < 0.01 {
            self.base_concurrency = (self.base_concurrency + 1).min(self.max_concurrency);
            self.error_rate *= 0.9;
        }
    }
}
```

### Batch receipt fetching

Current implementation calls `eth_getBlockReceipts` once per missing block. Some RPC providers support JSON-RPC batch requests.

Add a batch API to `RpcClient` before changing `Fetcher`:

```rust
// core/src/rpc.rs
pub async fn get_receipts_batch(
    &self,
    block_numbers: &[u64],
    batch_size: usize,
) -> anyhow::Result<Vec<Vec<ReceiptData>>> {
    let mut all = Vec::with_capacity(block_numbers.len());

    for chunk in block_numbers.chunks(batch_size) {
        let requests: Vec<_> = chunk
            .iter()
            .map(|block| JsonRpcRequest::new("eth_getBlockReceipts", vec![to_hex(*block)]))
            .collect();

        let responses: Vec<Vec<TransactionReceipt>> = self.batch_call(requests).await?;
        all.extend(
            responses
                .into_iter()
                .map(|receipts| receipts.iter().map(alloy_receipt_to_receipt_data).collect()),
        );
    }

    Ok(all)
}
```

Implementation notes:

- `RpcClient` currently has no `batch_call()` helper.
- Batch size should be configurable and conservative, starting around 25-50 requests.
- If a provider rejects batch requests, fall back to the existing per-block path and log a warning once.

### Pre-fetch pipelining

Overlap fetch and discovery where possible. Tokio already overlaps requests through `try_join_all`, but the semaphore can still create head-of-line blocking. Adaptive concurrency should throttle before queueing all block tasks.

---

## Phase 4: Sled → Replace with RocksDB

Sled is the wrong long-term tool for multi-GB historical replay caches:

- Limited compaction control for high write volume.
- One large tree for all key families unless the application creates multiple trees manually.
- Prefix scans are useful, but RocksDB column families and prefix extractors map more naturally to this workload.
- Sled is no longer an active strategic fit for the scale described in this plan.

### Recommended backend: RocksDB via `rust-rocksdb`

```toml
# core/Cargo.toml
rust-rocksdb = { version = "0.23", features = ["lz4", "zstd", "snappy"] }
```

### Why RocksDB instead of SQLite

RocksDB is the better replacement for this codebase because the current cache is already a key-value store:

- **Direct migration shape.** Existing keys are already namespaced strings like `block:137:123`. RocksDB column families preserve that model without introducing SQL tables, BLOB columns, and query translation.
- **Column families match cache domains.** Blocks, transactions, receipts, accounts, slots, code, manifests, discovery data, pool reserves, and checkpoints can each be a column family.
- **Better write throughput.** Replay lazily writes many account and storage-slot values. RocksDB is designed for high-volume random writes with tunable memtables, WAL behavior, and compaction.
- **Better compaction control.** Level/universal compaction, per-CF options, compression, and write-buffer tuning are first-class.
- **Efficient range scans.** Prefix iterators and column-family iterators support existing `list_manifests()` and `list_discovered_pools()` behavior.
- **Concurrent readers.** Multiple readers can iterate while writers append/compact.
- **Compression.** LZ4/Zstd reduce disk footprint for serialized block, account, and slot data.

SQLite is still useful for relational reporting, but it is not the right primary cache backend for this workload. Keep SQLite out of the core cache path unless a future feature needs SQL analytics.

### Cache column-family design

```rust
// core/src/storage/mod.rs
pub enum CacheColumnFamily {
    Blocks,
    Txs,
    Receipts,
    Accounts,
    Slots,
    Codes,
    Manifests,
    DiscoveredPools,
    DiscoveryCursors,
    PoolReserves,
    Checkpoints,
}
```

### Backend abstraction

```rust
// core/src/storage/mod.rs
pub trait CacheBackend: Send + Sync {
    fn get(&self, cf: CacheColumnFamily, key: &[u8]) -> anyhow::Result<Option<Vec<u8>>>;

    fn put(&self, cf: CacheColumnFamily, key: &[u8], value: &[u8]) -> anyhow::Result<()>;

    fn delete(&self, cf: CacheColumnFamily, key: &[u8]) -> anyhow::Result<()>;

    fn scan_prefix(
        &self,
        cf: CacheColumnFamily,
        prefix: &[u8],
    ) -> anyhow::Result<Vec<(Vec<u8>, Vec<u8>)>>;

    fn flush(&self) -> anyhow::Result<()>;
}
```

### Backend implementations

| Backend | Purpose |
|--------|---------|
| `SledBackend` | Legacy adapter preserving existing sled behavior and allowing old cache directories to keep working. |
| `RocksDbBackend` | Default production backend for new runs. |

### RocksDB backend tuning

Use conservative defaults first, then tune after profiling:

```rust
let mut opts = rocksdb::Options::default();
opts.create_if_missing(true);
opts.create_missing_column_families(true);
opts.increase_parallelism(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4) as i32);
opts.optimize_level_style_compaction(512 * 1024 * 1024);
opts.set_compression_type(rocksdb::DBCompressionType::Lz4);
```

Per-column-family tuning should be added after the first working migration:

- `write_buffer_size`: 128-512 MiB
- `target_file_size_base`: 64-256 MiB
- `max_write_buffer_number`: 3-6
- `prefix_extractor`: only for CFs that need prefix scans
- `compression`: LZ4 for speed, Zstd for smaller cache size

### Migration strategy

1. Add `core/src/storage/mod.rs` with `CacheBackend`, `CacheColumnFamily`, `SledBackend`, and `RocksDbBackend`.
2. Change `CacheStore` from a concrete sled wrapper into a facade over `Arc<dyn CacheBackend>`.
3. Keep the existing public `CacheStore` methods so callers do not need to change immediately.
4. Add config: `cache_backend = "rocksdb" | "sled"`.
5. Default new installations to RocksDB.
6. Add lazy migration: when RocksDB misses a key and a sled cache exists, read from sled and backfill RocksDB.
7. Add a one-time explicit migration command only if lazy migration proves too slow.
8. Keep sled tests as legacy-backend tests and add RocksDB temp-dir tests for every `CacheStore` method.

### RocksDB key mapping

Keep the existing string key format where possible:

| Existing key | RocksDB column family |
|-------------|-----------------------|
| `block:{chain_id}:{block_num}` | `Blocks` |
| `txs:{chain_id}:{block_num}` | `Txs` |
| `receipts:{chain_id}:{block_num}` | `Receipts` |
| `account:{chain_id}:{block_num}:{address}` | `Accounts` |
| `slot:{chain_id}:{block_num}:{address}:{slot}` | `Slots` |
| `code:{chain_id}:{address}` | `Codes` |
| `manifest:{run_id}` | `Manifests` |
| `discovery:{chain_id}:pool:{address}` | `DiscoveredPools` |
| `discovery:{chain_id}:cursor:{factory}` | `DiscoveryCursors` |
| `pool_reserves:{chain_id}:{pool}:{block}` | `PoolReserves` |
| `checkpoint:{run_id}` | `Checkpoints` |

---

## Phase 5: EVM Replay Micro-Optimizations

### 5a. Reserve cache for `init_pools()`

`PoolManager::init_from_rpc()` fetches reserve/state via RPC and has no persistent cache. Add a reserve cache after the RocksDB backend exists.

```rust
// core/src/pool/state.rs
pub struct CachedPoolReserves {
    cache: CacheStore,
    chain_id: u64,
    block_cache: LruCache<(Address, u64), PoolInitResult>,
}
```

Key format:

```text
pool_reserves:{chain_id}:{pool_address}:{block_number}
```

Behavior:

1. Check in-memory LRU cache first.
2. Check RocksDB `PoolReserves` column family.
3. If missing, fetch via `getReserves()` / V3 state / storage fallback.
4. Store result in RocksDB and the in-memory LRU cache.

### 5b. Lazy code loading during replay

`CachedRpcDb::basic()` at `core/src/replay.rs:176-257` calls `eth_getProof()` and then fetches code only when `code_hash != KECCAK_EMPTY`. The current implementation already skips `eth_getCode` for EOA accounts in the RPC-miss path, but it should be made explicit and tested.

Keep this behavior:

```rust
if code_hash != KECCAK_EMPTY && !self.codes.contains_key(&code_hash) {
    // fetch code only for contracts
}
```

Add a unit test proving that a `KECCAK_EMPTY` account does not call `get_code`.

### 5c. Batch storage reads

`CachedRpcDb::storage()` at `core/src/replay.rs:278-298` fetches one slot at a time. Add a batch path behind the existing single-slot API.

```rust
// core/src/rpc.rs
pub async fn get_storage_at_batch(
    &self,
    address: Address,
    slots: &[U256],
    block: u64,
    batch_size: usize,
) -> anyhow::Result<Vec<U256>> {
    let mut results = Vec::with_capacity(slots.len());

    for chunk in slots.chunks(batch_size) {
        let requests: Vec<_> = chunk
            .iter()
            .map(|slot| JsonRpcRequest::new("eth_getStorageAt", vec![
                format!("{address:?}"),
                format!("{slot:#x}"),
                format!("{block:#x}"),
            ]))
            .collect();

        let responses: Vec<U256> = self.batch_call(requests).await?;
        results.extend(responses);
    }

    Ok(results)
}
```

Then update `CachedRpcDb::storage()` to batch uncached slots that are requested close together. If revm does not expose natural batches, keep a small coalescing window inside `CachedRpcDb`.

### 5d. Polygon system contract filters

Polygon system contracts are currently filtered only in receipt verification:

- `0x0000000000000000000000000000000000001001`
- `0x0000000000000000000000000000000000001010`

Move the address list into a shared helper and use it in `replay_each_filtered`:

```rust
// core/src/replay.rs
const POLYGON_SYSTEM_ADDRS: [Address; 2] = [
    address!("0000000000000000000000000000000000001001"),
    address!("0000000000000000000000000000000000001010"),
];

fn is_polygon_system_addr(addr: &Address) -> bool {
    POLYGON_SYSTEM_ADDRS.contains(addr)
}
```

Add the filter to `core/src/run.rs` so transactions touching only Polygon system addresses do not enter full EVM replay.

---

## Phase 6: Incremental/Resumable Backtests

### Checkpoint every N blocks

Serialize pool state, last processed block, and run progress to the RocksDB `Checkpoints` column family.

```rust
// core/src/run.rs
pub struct Checkpoint {
    run_id: String,
    block_num: u64,
    pool_manager: PoolManager,
    progress: f64,
    updated_at: u64,
}

impl BacktestRunner {
    fn save_checkpoint(&self, run_id: &str, block_num: u64) -> anyhow::Result<()> {
        let checkpoint = Checkpoint {
            run_id: run_id.to_string(),
            block_num,
            pool_manager: self.pool_manager.clone(),
            progress: 0.0,
            updated_at: chrono::Utc::now().timestamp() as u64,
        };

        self.replayer.cache_store().put_checkpoint(&checkpoint)
    }

    fn load_checkpoint(&self, run_id: &str) -> anyhow::Result<Option<Checkpoint>> {
        self.replayer.cache_store().get_checkpoint(run_id)
    }
}
```

When interrupted, resume from the last checkpoint instead of starting over.

### Run manifest extension

`RunManifest` at `core/src/cache.rs:23-36` stores run metadata. Extend it with:

```rust
pub checkpoint_block: Option<u64>,
pub cache_backend: String,
```

Keep the field optional so old manifests deserialize cleanly.

---

## Phase 7: Implementation Order & Effort Estimate

| # | Task | Effort | Speedup | Risk |
|---|------|--------|---------|------|
| 1 | Profile with hotpath/flamegraph and record baseline metrics | 1 day | — | Low |
| 2 | Add `CacheBackend` abstraction plus `RocksDbBackend` and legacy `SledBackend` | 4 days | 1.5-2× on I/O-heavy runs | Medium |
| 3 | Parallel block replay with Rayon and safe pool-state forking | 3 days | **6-8×** | Medium |
| 4 | Reserve-history cache in RocksDB + in-memory LRU | 1 day | 1.1× | Low |
| 5 | Adaptive RPC fetch concurrency | 1 day | 1.2× | Low |
| 6 | Batch receipt and storage RPC helpers with fallback | 1 day | 1.2-1.5× | Medium |
| 7 | Incremental checkpoints | 2 days | N/A (UX/reliability) | Low |
| 8 | EOA/code-loading test and Polygon system-address replay filter | 0.5 day | 1.1× | Low |

**Total:** ~13-15 days for an expected 8-12× throughput improvement after profiling confirms the bottleneck mix.

**Recommended sprint order:** 1 → 2 → 3 → 4 → 5 → 6 → 7 → 8

RocksDB comes before large-scale parallel replay stress testing because parallel workers will amplify storage write pressure.

---

## File Change Summary

| File | Change |
|------|--------|
| `core/Cargo.toml` | Add `rayon`, `hotpath` dev dependency, `lru`, and `rust-rocksdb`; keep `sled` only for legacy backend support. |
| `core/src/storage/mod.rs` | Create backend abstraction, column-family enum, `RocksDbBackend`, and legacy `SledBackend`. |
| `core/src/cache.rs` | Convert `CacheStore` into a facade over `Arc<dyn CacheBackend>` while preserving existing method names. |
| `core/src/run.rs` | Add `run_range_par()`, safe block-runner forking, RocksDB checkpoint save/load, and Polygon system-address replay filter. |
| `core/src/replay.rs` | Add cache-store access for `BlockReplayer`, validate `CachedRpcDb` Send requirements, add storage batching hooks, and share Polygon system-address helper. |
| `core/src/rpc.rs` | Add JSON-RPC batch helper, `get_receipts_batch()`, and `get_storage_at_batch()` with fallback behavior. |
| `core/src/fetch.rs` | Replace fixed semaphore with adaptive concurrency and use batched receipt fetching where supported. |
| `core/src/pool/state.rs` | Add reserve-history cache for V2/V3 initialization and use RocksDB-backed persistence. |
| `core/src/config.rs` | Add `cache_backend` config field with default `rocksdb`; keep `sled` for migration. |
| `core/src/lib.rs` | Add `pub mod storage;`. |
| `cli/src/main.rs` | Initialize optional Rayon thread pool, pass backend config into `CacheStore`, and expose migration/resume behavior. |
| `mev-scout.example.toml` | Update cache comments from sled-only to RocksDB default with sled legacy option. |

---

## Validation Plan

| Area | Validation |
|------|------------|
| Storage abstraction | Existing `CacheStore` tests must pass with both `SledBackend` and `RocksDbBackend`. |
| RocksDB migration | A run using an existing sled cache must lazily backfill RocksDB and produce identical fetched data. |
| Parallel replay | Compare `run_range()` and `run_range_par()` on 100, 1,000, and 10,000 blocks; outputs must match exactly. |
| Thread safety | Add compile-time assertions for `Send` on replay worker types and run Miri only for small unit tests if feasible. |
| RPC batching | Run against a mock JSON-RPC server that records batch size; verify fallback when batch requests are rejected. |
| Pool reserve cache | Force repeated `init_pools()` calls for the same pool/block and assert only the first call hits RPC. |
| Polygon system filter | Add receipt and replay-filter tests for `0x1001` and `0x1010`. |
| Checkpoints | Interrupt simulation after a checkpoint, reload, and verify resumed output matches a full run. |
