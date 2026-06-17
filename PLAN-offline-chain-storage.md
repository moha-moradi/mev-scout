# Plan: Replace sled with SQLite + Parquet for Offline Chain Storage

## Goal
Store the entire requested block range (blocks, txs, receipts, logs, EVM state, pool state) locally so that `mev-scout run` works with **zero RPC calls**.

## Pipeline
```
RPC → [Our Parquet Writer] → Parquet files (.cache/parquet/*.parquet)
     → [Parquet Reader] → SQLite (.cache/mev-scout.sqlite)
     → EVM Replay (CachedRpcDb reads from SQLite)
```

## Key Design Decisions

### 1. cryo dependency: Option (c) — write our own RPC→Parquet writer
- We already have `RpcClient` with all needed methods
- `arrow`/`parquet` crates are straightforward (define schema → write record batches)
- Avoids heavyweight dep (cryo pulls in foundry EVM etc.)
- Full control over Parquet schema tailored to our access patterns

### 2. What goes through Parquet vs direct to SQLite
| Data | Parquet? | Reason |
|------|----------|--------|
| Blocks, Txs, Receipts, Logs | **Yes** | Fixed schema, bulk columnar → great zstd compression |
| Accounts (nonce, balance, code_hash) | **Yes** | Structured, fetchable by block range |
| Storage Slots | **No** → SQLite only | Too sparse, random access, no bulk benefit |
| Contract Code | **No** → SQLite only | Binary blobs, no columnar benefit |
| Pool State | **No** → SQLite only | Derived from `eth_call`, not raw RPC |

### 3. Fully replace sled — no backwards compat

---

## SQLite Schema

```sql
CREATE TABLE blocks (
    number INTEGER PRIMARY KEY,
    hash BLOB NOT NULL,
    timestamp INTEGER NOT NULL,
    base_fee_per_gas INTEGER,
    gas_limit INTEGER NOT NULL,
    gas_used INTEGER NOT NULL,
    coinbase BLOB NOT NULL
);

CREATE TABLE transactions (
    hash BLOB PRIMARY KEY,
    block_number INTEGER NOT NULL,
    tx_index INTEGER NOT NULL,
    from_addr BLOB NOT NULL,
    to_addr BLOB,
    input BLOB NOT NULL,
    value BLOB NOT NULL,
    gas_limit INTEGER NOT NULL,
    max_fee_per_gas INTEGER NOT NULL,
    max_priority_fee_per_gas INTEGER,
    nonce INTEGER NOT NULL,
    access_list BLOB,
    FOREIGN KEY (block_number) REFERENCES blocks(number)
);
CREATE INDEX idx_txs_block ON transactions(block_number);

CREATE TABLE receipts (
    tx_hash BLOB PRIMARY KEY,
    tx_index INTEGER NOT NULL,
    status INTEGER NOT NULL,
    gas_used INTEGER NOT NULL,
    cumulative_gas_used INTEGER NOT NULL,
    logs BLOB NOT NULL,
    contract_address BLOB,
    FOREIGN KEY (tx_hash) REFERENCES transactions(hash)
);

CREATE TABLE accounts (
    block_number INTEGER NOT NULL,
    address BLOB NOT NULL,
    nonce INTEGER NOT NULL,
    balance BLOB NOT NULL,
    code_hash BLOB NOT NULL,
    PRIMARY KEY (block_number, address)
);

CREATE TABLE storage_slots (
    block_number INTEGER NOT NULL,
    address BLOB NOT NULL,
    slot BLOB NOT NULL,
    value BLOB NOT NULL,
    PRIMARY KEY (block_number, address, slot)
);

CREATE TABLE contract_code (
    address BLOB PRIMARY KEY,
    code BLOB NOT NULL
);

CREATE TABLE pool_info (
    address BLOB PRIMARY KEY,
    token0 BLOB NOT NULL,
    token1 BLOB NOT NULL,
    fee INTEGER NOT NULL,
    dex_type INTEGER NOT NULL,
    tick_spacing INTEGER,
    creation_block INTEGER NOT NULL,
    pool_id BLOB
);

CREATE TABLE pool_states (
    address BLOB NOT NULL,
    block_number INTEGER NOT NULL,
    state_data BLOB NOT NULL,
    PRIMARY KEY (address, block_number)
);

CREATE TABLE discovery_cursors (
    factory BLOB NOT NULL,
    block_number INTEGER NOT NULL,
    PRIMARY KEY (factory)
);

CREATE TABLE run_manifests (
    run_id TEXT PRIMARY KEY,
    chain TEXT NOT NULL,
    start_block INTEGER NOT NULL,
    end_block INTEGER NOT NULL,
    resolved_at INTEGER NOT NULL,
    range_mode TEXT NOT NULL,
    strategies TEXT NOT NULL,
    flash_loan_provider TEXT NOT NULL
);
```

---

## Files to Change (complete list)

| # | File | Status | Change |
|---|------|--------|--------|
| 1 | `core/Cargo.toml` | ✅ | Remove `sled`. Add `rusqlite` (bundled), `arrow`, `parquet` (with zstd). Keep `bincode` |
| 2 | `core/src/cache.rs` | ✅ | Full rewrite: `SqliteStore` with 11 tables, all CRUD methods, 17 tests |
| 3 | `core/src/fetch.rs` | ✅ | `SqliteStore` + optional `ParquetWriter` via `with_parquet()`, `flush_parquet()` |
| 4 | `core/src/replay.rs` | ✅ | `CacheStore` → `SqliteStore` in `CachedRpcDb`, `BlockReplayer`, tests |
| 5 | `core/src/run.rs` | ✅ | `Option<&CacheStore>` → `Option<&SqliteStore>` |
| 6 | `core/src/pool/discovery.rs` | ✅ | `&CacheStore` → `&SqliteStore` |
| 7 | `core/src/cli.rs` | ✅ | `--db-path` + `--parquet-dir` args, `--cache-dir` removed |
| 8 | `core/src/config.rs` | ✅ | `db_path`, `parquet_dir` fields, `cache_dir` removed |
| 9 | `core/src/lib.rs` | ✅ | `pub mod parquet_writer` added |
| 10 | `cli/src/main.rs` | ✅ | All `CacheStore` → `SqliteStore`, `with_parquet()` wired |
| 11 | `core/tests/e2e_discovery.rs` | ✅ | `CacheStore` → `SqliteStore`, dir paths → file paths |
| 12 | `core/src/parquet_writer.rs` | ✅ | New: blocks/txs/receipts/accounts → zstd Parquet, 4 tests |

---

## Performance Summary

| Aspect | Current (sled) | Proposed (SQLite + Parquet) |
|--------|----------------|-----------------------------|
| Fetch speed | ✅ Faster (1-pass) | ⚠️ 2-pass (Parquet → SQLite) |
| Replay speed | ✅ Faster sync KV | ⚠️ Async bridge overhead (~2-5ms/block cold) |
| Storage size | ⚠️ No compression | ✅ Parquet zstd: 5-10x smaller |
| Portability | ❌ Dir tree | ✅ Single .sqlite file |
| Queryability | ❌ Blob only | ✅ SQL queries |
| Offline-ready | ❌ Needs complex prefetch | ✅ Parquet IS portable cache |
| Accuracy | ✅ Same | ✅ Same |

---

## Implementation Status — All Complete

1. ✅ **`core/Cargo.toml`** — sled removed, rusqlite + arrow + parquet added, bincode kept
2. ✅ **`core/src/cache.rs`** — `SqliteStore` with 11 tables, 17 unit tests
3. ✅ **`core/src/parquet_writer.rs`** — new Parquet writer (blocks/txs/receipts/accounts), 4 tests
4. ✅ **`core/src/fetch.rs`** — `SqliteStore` + `with_parquet()` + `flush_parquet()`
5. ✅ **`core/src/replay.rs`** — `SqliteStore` type, sync bridge for revm `Database` trait
6. ✅ **`core/src/run.rs`** — `Option<&SqliteStore>` in `init_pools`
7. ✅ **`core/src/pool/discovery.rs`** — `&SqliteStore` in `discover_pools`
8. ✅ **`core/src/cli.rs`** — `--db-path` + `--parquet-dir`
9. ✅ **`core/src/config.rs`** — `db_path` + `parquet_dir` fields
10. ✅ **`cli/src/main.rs`** — all `CacheStore::open` → `SqliteStore::open`, `with_parquet()` wired
11. ✅ **Tests** — `e2e_discovery.rs` updated, replay tests use `.sqlite` files
