# mevlog-Based Real-Data Testing Plan

## Goal

Use `mevlog-rs` to discover real on-chain blocks containing MEV patterns, then replay those blocks through the existing MEV detection engines for end-to-end integration tests. This transforms placeholder tests into meaningful assertions against real blockchain data.

## Setup (one-time)

```bash
# Install dependencies (~5 min compilation)
cargo install mevlog --locked
cargo install cryo_cli --locked

# Index last 50K Polygon blocks (~5-15 min depending on RPC)
mevlog block-txs -b latest:latest-50000 --chain-id=137

# Run the new tests
RPC_URL=<polygon_rpc> cargo test --test integration -- test_mevlog_ --nocapture
```

## Dependency Change

**`core/Cargo.toml`** — add under `[dev-dependencies]`:

```toml
rusqlite = { version = "0.31", features = ["bundled"] }
```

## DeFi Event Topics (for SQL)

These hex values match the `topic0` BLOB column in mevlog's `logs` table. They are used in `X'...'` SQLite BLOB literals.

| Event | Keccak256 Signature | Topic Hex (without `0x`) | Source |
|---|---|---|---|
| V2 Swap | `Swap(address,uint256,uint256,uint256,uint256,address)` | `d78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822` | `sandwich.rs:16` |
| V3 Swap | `Swap(address,address,int256,int256,uint160,uint128,int24)` | `c42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67` | `decoders.rs:9` |
| V3 Burn | `Burn(address,address,int24,int24,uint128,uint256,uint256)` | `0c396cd989a39f4459b5fa1aed6a9a8dcdbc45908acfd67e028cd568da98982c` | `decoders.rs:15` |
| V3 Mint | `Mint(address,address,int24,int24,uint128,uint256,uint256)` | (computed via `keccak256` — hardcode at test time) | `decoders.rs:12` |

## SQL Discovery Queries

Each query finds candidate blocks for a specific MEV pattern.

### JIT (V3 Mint → Swap → Burn)

```sql
-- Find blocks with all three V3 events on the same pool
SELECT block_number, address
FROM logs
WHERE topic0 = X'<V3_MINT_TOPIC>'
  AND block_number BETWEEN ? AND ?
INTERSECT
SELECT block_number, address
FROM logs
WHERE topic0 = X'c42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67'
  AND block_number BETWEEN ? AND ?
INTERSECT
SELECT block_number, address
FROM logs
WHERE topic0 = X'0c396cd989a39f4459b5fa1aed6a9a8dcdbc45908acfd67e028cd568da98982c'
  AND block_number BETWEEN ? AND ?
ORDER BY block_number DESC
LIMIT 5;
```

Post-filter: verify Mint's `tx_index` ≤ Swap's `tx_index` ≤ Burn's `tx_index` when in different transactions.

### Sandwich (V2 Swap × 3, consecutive, same sender frontrun+backrun)

```sql
SELECT l1.block_number, l1.address AS pool,
       l1.tx_index AS frontrun_idx,
       l2.tx_index AS victim_idx,
       l3.tx_index AS backrun_idx
FROM logs l1
JOIN logs l2 ON l1.block_number = l2.block_number
    AND l1.address = l2.address
    AND l2.tx_index = l1.tx_index + 1
JOIN logs l3 ON l1.block_number = l3.block_number
    AND l1.address = l3.address
    AND l3.tx_index = l2.tx_index + 1
JOIN transactions t1 ON l1.tx_hash = t1.tx_hash
JOIN transactions t2 ON l2.tx_hash = t2.tx_hash
JOIN transactions t3 ON l3.tx_hash = t3.tx_hash
WHERE l1.topic0 = X'd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822'
  AND l2.topic0 = X'd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822'
  AND l3.topic0 = X'd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822'
  AND t1.from_address = t3.from_address
  AND t2.from_address != t1.from_address
  AND l1.block_number BETWEEN ? AND ?
GROUP BY l1.block_number, l1.address
LIMIT 10;
```

Note: BLOB comparisons for `from_address` need 20-byte hex values.

### Most Active Block (for Arbitrage detection)

```sql
SELECT block_number, COUNT(*) AS event_count
FROM logs
WHERE topic0 IN (
    X'd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822',
    X'c42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67'
)
  AND block_number BETWEEN ? AND ?
GROUP BY block_number
ORDER BY event_count DESC
LIMIT 3;
```

Arb detectors don't need a specific event pattern — they analyze pool state at a given block to find price differences. A block with high swap activity is likely to have price dislocations.

### JitArb (JIT + arbitrage on connected pool)

Two-step:
1. Find JIT candidates (same query as JIT above)
2. For each JIT candidate, query for same-sender swaps on pools sharing a token with the JIT pool

## Test Structure

All additions go into **`core/tests/integration.rs`** (appended at the end).

### Helpers

```
// ═══════════════════════════════════════════════════════════════
// mevlog Discovery Helpers
// ═══════════════════════════════════════════════════════════════

struct JitCandidate {
    block_number: u64,
    pool_address: Address,
    token0: Address,
    token1: Address,
    fee: u32,
    tick_spacing: i32,
}

struct SandwichCandidate {
    block_number: u64,
    pool: Address,
    frontrun_tx: usize,
    victim_tx: usize,
    backrun_tx: usize,
}

fn mevlog_db_path(chain_id: u64) -> PathBuf
fn open_mevlog_db(chain_id: u64) -> Option<Connection>
fn find_jit_candidates(conn: &Connection, start: u64, end: u64) -> Vec<JitCandidate>
fn find_sandwich_candidates(conn: &Connection, start: u64, end: u64) -> Vec<SandwichCandidate>
fn find_most_active_block(conn: &Connection, start: u64, end: u64) -> Option<u64>
```

### Tests

#### 1. `test_mevlog_jit_detection`

```
- Skip if no RPC_URL / no mevlog DB / no JIT candidates
- For each candidate:
  - Init pool via RPC (init_from_rpc at candidate block)
  - Seed JitDetector tick cache
  - Replay block via BlockReplayer
  - Feed events → JitDetector.process_tx() per tx
  - Call detector.detect()
  - Assert: opps.len() >= 1
  - Assert: strategy == Strategy::Jit
  - Assert: pool_a == candidate pool
  - Assert: expected_profit > 0
  - Assert: tick_lower, tick_upper, liquidity_amount are Some
- Report "Found N JIT opportunities across M candidates"
```

#### 2. `test_mevlog_sandwich_detection`

```
- Skip if no RPC_URL / no mevlog DB / no Sandwich candidates
- For each candidate:
  - Init pool via RPC
  - Replay block via BlockReplayer
  - Feed events → SandwichDetector.process_tx() per tx
  - Call detector.detect()
  - Assert: opps.len() >= 1
  - Assert: strategy == Strategy::Sandwich
  - Assert: pool_a matches, victim_tx_index matches expected
  - Assert: expected_profit > 0
```

#### 3. `test_mevlog_jit_arb_detection`

```
- Skip if no RPC_URL / no mevlog DB / no JIT candidates
- For each JIT candidate with same-sender swaps on connected pools:
  - Init both pools via RPC
  - Replay block
  - Feed events → JitArbDetector.process_tx() per tx
  - Call detector.detect()
  - Assert: JitArb opportunities found
  - Assert: pool_a == JIT pool, pool_b == arb pool
```

#### 4. `test_mevlog_arb_detection`

```
- Skip if no RPC_URL / no mevlog DB / no active block found
- Initialize known Polygon pools (QuickSwap WMATIC/USDC, SushiSwap WMATIC/USDC,
  Uniswap V3 WMATIC/USDC 0.05%, QuickSwap WMATIC/USDT, QuickSwap USDC/USDT)
  at the discovered active block
- Run TwoHopArbDetector.detect()
- Run MultiHopArbDetector.detect()
- Assert: arb opportunities with positive profit
- Assert: multi-hop paths have path.len() >= 2
```

### Skip Logic

| Scenario | Behavior |
|---|---|
| `RPC_URL` env var not set | `eprintln!("Skipping: RPC_URL not set"); return;` |
| mevlog DB not found at `~/.mevlog/mevlog-txs-v1-137.db` | `eprintln!("Skipping: mevlog DB not found. Run `mevlog block-txs -b latest:latest-50000 --chain-id=137` first"); return;` |
| No candidates in indexed range | `eprintln!("Skipping: no candidates found. Try indexing a wider range."); return;` |
| Pool init returns 0 initialized | `eprintln!("Skipping candidate: pool not initialized"); continue;` |
| Block replay fails | `eprintln!("Skipping candidate: replay failed"); continue;` |
| No opportunities detected on a candidate | `eprintln!("Candidate block {n}: no opps (prices aligned?)");` — info only |

## Files Changed

| File | Change | Est. Lines |
|---|---|---|
| `core/Cargo.toml` | Add `rusqlite` dev-dep with `bundled` feature | +1 |
| `core/tests/integration.rs` | Append helpers + 4 tests | ~350 |

No modifications to existing tests or source code.

## Workflow

1. **Setup**: install mevlog + cryo, index Polygon blocks (one-time)
2. **Develop**: run specific tests with `cargo test test_mevlog_`
3. **Iterate**: re-index if blocks age out of the cached range
4. **CI**: requires `RPC_URL` secret + `mevlog` binary cached or installed in CI runner
