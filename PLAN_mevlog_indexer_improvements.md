# Plan: Adopt mevlog-rs Indexer Patterns

Adopt four patterns from `mevlog-rs`'s `index` command into `mev-scout`'s `fetch` flow to improve efficiency, queryability, and data richness.

---

## Phase 1 — Batch Missing-Block Query + Contiguous Range Batching

**Problem:** `Fetcher::fetch_range()` (and `fetch_relevant()`) dispatches the *entire* requested block range to providers as individual concurrent tasks. Each task calls `SqliteStore::has_block()` which executes **two SQL queries** per block (`SELECT COUNT(*) FROM blocks` + `SELECT COUNT(*) FROM block_meta`). For a 10K block range with 90% cache hit, this is **20K unnecessary round-trips**. RPC providers also receive scattered block numbers instead of contiguous sequences.

**Solution:** Query the cache once upfront to determine exactly which blocks are missing, group them into contiguous runs, and batch-fetch each run.

### Files to change

| File | Change |
|------|--------|
| `core/src/cache/store.rs` | Add `get_cached_blocks_in_range(start, end)` → `Vec<u64>` with one SQL query |
| `core/src/cache/store.rs` | Add `missing_blocks_in_range(start, end)` → `Vec<u64>` (uses set diff) |
| `core/src/cache/store.rs` | Add `contiguous_ranges(blocks: &[u64])` → `Vec<(u64, u64)>` |
| `core/src/fetch/fetcher.rs` | Rewrite `fetch_range()` to use the three methods above |
| `core/src/fetch/fetcher.rs` | Add `fetch_contiguous_range(start, end)` for batch sequential fetch |
| `core/src/fetch/fetcher.rs` | Update `fetch_relevant()` similarly |

### Detailed spec

#### `store.rs` — new methods

```rust
/// Single query: return all cached block numbers in [start, end].
pub fn get_cached_blocks_in_range(&self, start: u64, end: u64) -> anyhow::Result<Vec<u64>> {
    let conn = self.conn();
    let mut stmt = conn.prepare(
        "SELECT number FROM blocks
         INNER JOIN block_meta USING(number)
         WHERE number BETWEEN ?1 AND ?2 AND txs_fetched = 1
         ORDER BY number"
    )?;
    // ... query_map and collect
}

/// Returns blocks in [start, end] that are NOT in the cache.
pub fn missing_blocks_in_range(&self, start: u64, end: u64) -> anyhow::Result<Vec<u64>> {
    let cached = self.get_cached_blocks_in_range(start, end)?;
    let set: HashSet<u64> = cached.into_iter().collect();
    Ok((start..=end).filter(|n| !set.contains(n)).collect())
}

/// Group sorted, deduped block numbers into contiguous (start, end) inclusive ranges.
pub fn contiguous_ranges(blocks: &[u64]) -> Vec<(u64, u64)> {
    let mut ranges = Vec::new();
    for &block in blocks {
        match ranges.last_mut() {
            Some(last) if block == last.1 + 1 => last.1 = block,
            _ => ranges.push((block, block)),
        }
    }
    ranges
}
```

#### `fetcher.rs` — new `fetch_range()` flow

```
1. Call cache.missing_blocks_in_range(start, end)       ← single DB query
2. If no missing blocks, return cached-only summary
3. Call contiguous_ranges(missing)                        ← group into runs
4. For each contiguous (run_start, run_end):
   a. Determine provider shard (same weighted distribution)
   b. Spawn a task that fetches run_start..=run_end sequentially
5. Wait for all range tasks to complete
6. Integrity check on the full missing set (backward compat)
```

### Idempotency

No changes needed — `INSERT OR REPLACE` is already used for blocks/txs/receipts.

### Tests

- Unit test `contiguous_ranges()` with gaps, singletons, empty input
- Integration: fetch a range, then re-fetch same range — should show 0 new blocks
- Integration: partially cache a range, re-fetch — should only fetch missing

---

## Phase 2 — Event Log Normalization

**Problem:** Logs are serialized as an opaque bincode blob inside the `receipts` table. Querying by event type, address, or topic requires deserializing every receipt blob. ERC20 transfer amounts are not extracted, requiring manual parsing.

**Solution:** Add a normalized `logs` table with indexed topic columns, populated during fetch alongside the existing blob. Extract ERC20 `Transfer` amounts at ingestion time.

### Files to change

| File | Change |
|------|--------|
| `core/src/data/types.rs` | Add `NormalizedLog` struct (optional, can use tuple) |
| `core/src/cache/store.rs` | Add `logs` table to `initialize_tables()` |
| `core/src/cache/store.rs` | Add `put_logs_batch()` + helpers |
| `core/src/cache/store.rs` | Modify `put_block_data` / `put_block_data_batch` to dual-write logs |
| `core/src/cache/store.rs` | Add `get_logs_for_block()` / `get_logs_for_tx()` query methods |
| `core/src/cache/store.rs` | Add `decode_erc20_amount()` static helper |

### Schema addition

```sql
CREATE TABLE IF NOT EXISTS logs (
    block_number INTEGER NOT NULL,
    tx_index     INTEGER NOT NULL,
    log_index    INTEGER NOT NULL,
    address      BLOB NOT NULL,              -- 20 bytes
    topic0       BLOB,                        -- 32 bytes
    topic1       BLOB,                        -- 32 bytes
    topic2       BLOB,                        -- 32 bytes
    topic3       BLOB,                        -- 32 bytes
    data         BLOB NOT NULL,
    erc20_amount BLOB,                        -- 32 byte U256, NULL for non-Transfer
    event_sig    TEXT,                        -- human-readable name, NULL if unknown
    PRIMARY KEY (block_number, log_index)
);
CREATE INDEX IF NOT EXISTS idx_logs_address ON logs(address);
CREATE INDEX IF NOT EXISTS idx_logs_topic0 ON logs(topic0);
```

### ERC20 amount extraction

```rust
/// Transfer(address,address,uint256) topic hash
const TRANSFER_EVENT_TOPIC: B256 =
    b256!("ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef");

fn decode_erc20_amount(log: &LogData) -> Option<U256> {
    if log.topics.first() == Some(&TRANSFER_EVENT_TOPIC) && log.data.len() >= 32 {
        Some(U256::from_be_slice(&log.data[log.data.len()-32..]))
    } else {
        None
    }
}
```

### Dual-write in `put_block_data` / `put_block_data_batch`

```
for each receipt:
    serialize logs blob → write to receipts.logs (unchanged)
    for each log in receipt.logs:
        insert into logs table (block_number, tx_index, log_index, address,
                                topic0-3, data, erc20_amount, event_sig)
```

### Backward compatibility

- `receipts.logs` blob remains untouched — existing code reading from it works unchanged
- `logs` table is additive — old DBs without the table get it created on next `open()`
- Old fetched blocks without rows in `logs` table: the blob path still serves data
- A `reindex` like command (future work) could backfill `logs` for old blocks

### Impact on pipeline

**Zero.** Detectors (`runner.rs`) read `ExecutedLog` from the in-memory replayer output, not from the cache. The replayer (`replayer.rs`) reads `ReceiptData` from cache and converts `LogData` → `ExecutedLog` internally. The blob deserialization path in `get_receipts()` still works identically.

---

## Phase 3 — Signature Resolution at Fetch Time

**Problem:** Transaction `input` bytes are stored as-is. There is no way to query "all `swap(uint256,uint256,address,bytes)` calls" without external tooling. Log topics are also opaque without resolving the event signature.

**Solution:** Download the pre-built `mevlog-sigs-v5.db` (zstd-compressed SQLite with ~50K method selectors + ~10K event topic hashes). Resolve both 4-byte selectors and event topic0 at fetch time, storing resolved names alongside the data.

### Files to change

| File | Change |
|------|--------|
| `core/Cargo.toml` | Add `hex`, `ruzstd` dependencies |
| `core/src/sigs/mod.rs` | New module: `downloader`, `resolver` |
| `core/src/sigs/downloader.rs` | Download + zstd-decompress + cache sig DB |
| `core/src/sigs/resolver.rs` | `resolve_method()`, `resolve_event()`, LRU cache |
| `core/src/cache/store.rs` | Add columns `sig_hash`, `sig_name` to `transactions` |
| `core/src/cache/store.rs` | Migration: `ALTER TABLE transactions ADD COLUMN ...` |
| `core/src/cache/store.rs` | Update `put_block_data` / `put_block_data_batch` to write sig columns |
| `core/src/fetch/fetcher.rs` | Load sig DB path, pass through to write methods |
| `core/src/rpc/client.rs` | In conversion: extract first 4 bytes of `input` as selector |

### New module structure

```
core/src/sigs/
├── mod.rs          # Module root, re-exports
├── downloader.rs   # Download + decompress + cache sig DB
├── resolver.rs     # Method + event signature lookup with in-memory LRU
```

### Signature DB downloader

```rust
/// Download mevlog-sigs-v5.db.zst from CDN, decompress, cache at ~/.mev-scout/sigs-v5.db
pub async fn ensure_signature_db() -> Result<PathBuf> {
    let cache_dir = config_path()?;  // ~/.mev-scout
    let db_path = cache_dir.join("mevlog-sigs-v5.db");

    if db_path.exists() {
        return Ok(db_path);
    }

    // Download from CDN (same URL as mevlog-rs)
    let url = "https://d39my35jed0oxi.cloudfront.net/mevlog-sigs-v5.db.zst";
    let resp = reqwest::get(url).await?;

    // Decompress zstd stream to db_path
    // ...
    Ok(db_path)
}
```

### Signature resolver

```rust
pub struct SignatureResolver {
    db_path: PathBuf,
    method_cache: RwLock<HashMap<[u8; 4], Option<String>>>,
    event_cache: RwLock<HashMap<B256, Option<String>>>,
}

impl SignatureResolver {
    pub fn new(db_path: PathBuf) -> Self;

    /// Resolve a 4-byte function selector to a human-readable method signature.
    pub fn resolve_method(&self, selector: &[u8; 4]) -> Result<Option<String>> {
        // Check cache → query sigs DB via rusqlite → cache result
    }

    /// Resolve a 32-byte event topic hash to a human-readable event signature.
    pub fn resolve_event(&self, topic: &B256) -> Result<Option<String>> {
        // Same pattern
    }
}
```

### Schema migration

```sql
-- Add columns to transactions table (idempotent)
ALTER TABLE transactions ADD COLUMN sig_hash BLOB;   -- 4-byte selector, NULL for CREATE/ETH-transfer
ALTER TABLE transactions ADD COLUMN sig_name TEXT;    -- human-readable, e.g. "transfer(address,uint256)"
```

Use `PRAGMA table_info(transactions)` to detect whether columns already exist before running ALTER.

### Integration into `put_block_data`

```
In put_block_data / put_block_data_batch:

for each tx in txs:
    selector = tx.input.first_four_bytes()        // first 4 bytes of calldata
    sig_name = resolver.resolve_method(&selector) // if selector exists
    // Store sig_hash + sig_name alongside existing tx columns

for each receipt:
    for each log in receipt.logs:
        event_sig = resolver.resolve_event(&log.topics[0])  // if topic0 exists
        // Store event_sig in the logs table (Phase 2)
```

### Fetcher integration

```rust
pub struct Fetcher {
    rpc: RpcClient,
    cache: SqliteStore,
    sig_resolver: Option<SignatureResolver>,  // new field
    // ...
}

pub async fn fetch_one_block(&self, block_num: u64, ...) -> Result<bool> {
    // ... existing fetch logic ...

    // After getting txs + receipts, optionally resolve signatures
    let sig_hashes = if let Some(ref resolver) = self.sig_resolver {
        Some(txs.iter().map(|tx| {
            if tx.input.len() >= 4 {
                let mut sel = [0u8; 4];
                sel.copy_from_slice(&tx.input[..4]);
                (sel, resolver.resolve_method(&sel).ok().flatten())
            } else {
                ([0u8; 4], None)
            }
        }).collect::<Vec<_>>())
    } else {
        None
    };

    // Write to cache with signature data
    self.cache.put_block_data(block_num, &block, &txs, &receipts, sig_hashes)?;
}
```

### Backward compatibility

- `sig_hash` / `sig_name` columns are nullable, default NULL
- Old rows without sig data work fine — they just show NULL
- The `SignatureResolver` field in `Fetcher` is `Option` — if the sig DB download fails, fetch continues without resolution
- CLI flag `--no-sig-resolve` can skip the download entirely

---

## Dependency Graph

```
Phase 1 (batch query + contiguous)
     │
     ▼
Phase 2 (log normalization)  ←── can start after or parallel to Phase 1
     │
     ▼
Phase 3 (signature resolution)  ←── requires Phase 2's event_sig column
```

Phase 1 and 2 are independent and can be implemented in parallel. Phase 3 depends on Phase 2 for the `event_sig` column in the `logs` table, but the `sig_hash`/`sig_name` columns in `transactions` are independent.

---

## Execution Order (Recommended)

### Step 1 — Phase 1 (batch + contiguous)
- **Effort:** Medium (~half day)
- **Files:** 2 (`store.rs`, `fetcher.rs`)
- **Risk:** Low — purely additive optimization, no schema change
- **Verification:** `cargo test`; manual: `mev-scout fetch --blocks 10000 --chain base` twice (second run should show all cached)

### Step 2 — Phase 2 (log normalization)
- **Effort:** Large (~1 day)
- **Files:** 2 (`types.rs`, `store.rs`)
- **Risk:** Medium — new table, dual-write path
- **Verification:** `cargo test`; manual: check SQLite has `logs` table with rows after fetch

### Step 3 — Phase 3 (signature resolution)
- **Effort:** Medium (~1 day)
- **Files:** 5+ (`Cargo.toml`, new `sigs/` module, `store.rs`, `fetcher.rs`, `client.rs`)
- **Risk:** Medium — network dependency for sig DB download
- **Verification:** Check `transactions` table has non-NULL `sig_name` after fetch; check `logs` table has non-NULL `event_sig`

---

## Future work (out of scope for this plan)

- **`reindex` / gap-fill command** — scan existing block range, backfill `logs` table rows for old blocks, re-resolve signatures
- **`--live` index mode** — watch for new blocks and auto-fetch with configurable `--keep` purge
- **`signature` field on `TxData`** — could be added to the wire-format struct if needed for downstream use
- **Embedded minimal signature DB** — ship a small set of common signatures (ERC20, Uniswap, etc.) as a fallback when CDN download fails
