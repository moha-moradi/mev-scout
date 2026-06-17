# Pool Discovery Module — Improvement Plan

## Current State

### Auto-discovery runs inside `run` command (`cli/src/main.rs:346-374`)

When `pool_discovery_start_block` is configured, the `run` command:
1. Scans V2/V3 factory events from `start_block` to `resolved.start_block - 1`
2. Saves discovered pools to local cache (SQLite)
3. Loads all pools from cache into `PoolManager`
4. Proceeds to fetch/replay

This couples two concerns: discovery (scanning chain history for pools) and
execution (replaying blocks and detecting MEV).

### What happens when pools don't exist at the target block

Pools discovered in a prior run (or from a different start block) are cached and
loaded on every run. When `init_from_rpc()` tries to fetch reserves at
`start_block - 1`, pools that didn't exist yet fail silently with a generic
"Failed to fetch state" warning. They get zero reserves, but still:
- Take up space in `PoolManager`
- Inflate `token_index` → creates bogus arbitrage pairs
- Bloat the transaction filter set (more addresses to check per tx)

---

## Plan

### 1. Remove auto-discovery from `run` command

**File:** `cli/src/main.rs:346-374`

Remove the auto-discovery block inside the `run` command. Discovery becomes a
separate upfront step via `mev-scout discover`.

**Rationale:**
- Clean separation: discovery scans chain history; run replays blocks and detects MEV
- No risk of discovery failures aborting a run
- Users explicitly choose when/how to discover pools
- The `replay` command already doesn't auto-discover — this makes `run` consistent

**What users need to do instead:**
```bash
# Step 1: Discover pools (one-time or periodic)
mev-scout discover --from 0 --to 50000000

# Step 2: Run backtest using discovered pools
mev-scout run --from 50000000 --to 50001000
```

**Graceful degradation:** If `run` is called with no pools loaded (no cache, no
registry), emit a clear message:
```
No pools found. Run `mev-scout discover` first or configure a pools registry.
```
And skip MEV detection (or exit with a warning).

---

### 2. Filter pools that don't exist at the target block before init

**Files:** `core/src/run.rs`, `core/src/pool/state.rs`

**What:** Before `init_from_rpc()`, skip pools whose `creation_block > target_block`
and also call `eth_getCode` as a safety net for pools without a known creation block.

**Two-layer filter:**

**Layer 1 (cheap, no RPC):**  
Compare `pool.creation_block > block_num`. Pools discovered at block N+1
can't have existed at block N. Skip them immediately.

```rust
for info in &pools {
    if info.creation_block > 0 && info.creation_block > block_num {
        tracing::debug!("Skipping pool {} (created at block {}, before target {})",
            info.address, info.creation_block, block_num);
        continue;
    }
    add_pool_to_manager(pool_manager, info.clone());
}
```

**Layer 2 (RPC, safety net):**  
For the remaining pools, call `eth_getCode` concurrently before
`init_from_rpc()`. Pools with zero code at the target block are filtered out.

```rust
pub async fn filter_existing_pools(
    rpc: &RpcClient,
    pools: &[PoolInfo],
    block_num: u64,
) -> Vec<PoolInfo> {
    // Concurrent eth_getCode calls (semaphore pattern)
    // Return only pools with non-empty code at block_num
}
```

**Rationale:**
- Layer 1 is free (no RPC), uses already-available metadata
- Layer 2 catches edge cases: self-destructed pools, pools without
  `creation_block` set, registry pools loaded from JSON
- Still log the count of skipped pools

---

### 3. Use `creation_block` during pool load for smart filtering

**File:** `core/src/run.rs:72-109`

When loading pools from cache/registry, sort by `creation_block` and skip
pools whose creation is after the target block. This reduces PoolManager size
and avoids wasting RPC calls on pools that can't possibly have state.

---

### 4. Upgrade zero sqrt price assertion to runtime warning

**File:** `core/src/pool/state.rs:489-493`

```rust
if sqrt.is_zero() {
    tracing::warn!("V3 pool {} initialized with zero sqrt price", addr);
}
debug_assert!(!sqrt.is_zero(), "V3 pool {} ...", addr);
```

Zero sqrt price should never happen, but if it does, release builds should
log it, not silently pass.

---

### 5. Add warnings for malformed log events in discovery

**File:** `core/src/pool/discovery.rs:68-69, 111-112`

Before `continue`, add `tracing::warn!` with the log's block number, data
length, and topics count.

---

### 6. Use factory deployment blocks as default discovery start

**File:** `core/src/config.rs`

Update default `pool_discovery_start_block` per chain to avoid genesis scans:

| Chain     | Suggested start | Earliest factory            |
|-----------|----------------|------------------------------|
| Ethereum  | 10,000,835     | Uniswap V2 factory deploy    |
| Polygon   | 49,100,000     | QuickSwap deploy             |
| BSC       | 5,063,800      | PancakeSwap V2 deploy        |
| Arbitrum  | 172,000        | Uniswap V3 deploy            |
| Avalanche | 4,200,000      | Trader Joe deploy            |
| Base      | 96,000         | Aerodrome factory            |
| Optimism  | 10,827,000     | Uniswap V3 deploy            |

---

### 7. Make batch discovery resilient

**File:** `core/src/pool/discovery.rs:167-169, 195-197`

Replace `?` propagation with error logging + continue. Do NOT advance the
cursor on failure so the batch is retried next run.

---

### 8. Add explicit pool deduplication

**File:** `core/src/run.rs:82-84`

Log when a pool address exists in both registry and cache, and skip the
duplicate.

---

## What's NOT in scope

- **Curve and Balancer state fetching** — excluded entirely as discussed
- **Per-factory V2 fee overrides** — useful but separate concern
- **Cache error handling in `discover` subcommand** — minor, separate ticket

---

## Summary of changes by file

| File | Change |
|------|--------|
| `cli/src/main.rs:346-374` | Remove auto-discovery block from `run` |
| `cli/src/main.rs` (after removal) | Emit warning if no pools loaded + guide user to `discover` |
| `core/src/run.rs:72-109` | Add Layer 1 filter (creation_block check), Layer 2 filter (eth_getCode) |
| `core/src/pool/state.rs` | Add `filter_existing_pools()` public method |
| `core/src/pool/state.rs:489-493` | Upgrade zero sqrt to runtime warning |
| `core/src/pool/discovery.rs:68-69` | Warn on malformed V2 logs |
| `core/src/pool/discovery.rs:111-112` | Warn on malformed V3 logs |
| `core/src/pool/discovery.rs:167-169, 195-197` | Resilient batch discovery (log + continue) |
| `core/src/config.rs` | Use factory deploy blocks as default start |
