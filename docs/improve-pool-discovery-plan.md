# Pool Discovery Module — Improvement Plan

## Problem Analysis

After analyzing the pool discovery module, I found several issues that affect correctness, robustness, and performance when pools discovered via on-chain scanning don't exist in the replay/run context:

### Core Issues

**1. Pools created after the target block are still loaded**
Pools discovered by a previous run (which ran discovery up to a later block) are cached and loaded on every subsequent run. When `BacktestRunner::init_pools()` calls `init_from_rpc()` at `start_block - 1`, pools that didn't exist yet at that block fail the `eth_call` silently. They get zero reserves but still occupy space in `PoolManager`, inflate `token_index` (which creates bogus arbitrage pairs), and bloat the transaction filter set.

*Relevant code:*
- `core/src/run.rs:72-109` — `init_pools()` loads all discovered pools regardless of their `creation_block`
- `core/src/pool/state.rs:473-500` — Failed init logs generic warning, pool stays with zero reserves
- `core/src/pool/state.rs:425-433` — `initialized_count()` only counts pools with >0 reserves, so they're silently excluded

**2. No contract-code verification before reserve fetching**
There's no `eth_getCode` check to verify a discovered pool address is a valid contract before attempting to fetch reserves. If a pool was self-destructed or the address is garbage, the `eth_call` (and storage fallback) will fail, producing the generic "Failed to fetch state for pool" warning. The user can't distinguish "pool didn't exist yet" from "RPC error" from "pool was destroyed."

**3. Hardcoded 0.3% fee for all V2 pools** (`core/src/pool/discovery.rs:81`)
All V2 pools are assigned fee=30 (0.3%). Forks like PancakeSwap (0.25%), some Trader Joe pools (0.20%), or fee-tier V2 forks like FraxSwap get incorrect fee rates, leading to inaccurate profit calculations.

**4. Curve and Balancer pool state fetching not implemented** (`core/src/pool/state.rs:519-523`)
These pool types are discoverable and get loaded into `PoolManager`, but `fetch_pool_state()` always returns `None` for them (comment: "not yet implemented"). They always have zero balances, wasting memory and polluting `token_index`.

### Moderate Issues

**5. Malformed log events silently skipped** (`core/src/pool/discovery.rs:68-69, 111-112`)
When log data is too short or topics are missing, the log is silently `continue`'d. No warning is emitted.

**6. Zero sqrt price only caught in debug builds** (`core/src/pool/state.rs:489-493`)
`debug_assert!(!sqrt.is_zero())` means in release builds a V3 pool with zero sqrt price passes through silently. This should be a runtime check.

**7. Discovery cursor starts at block 0 for all chains** (`core/src/config.rs`)
The first run on any chain scans from genesis block. On Ethereum (block 0 to ~22M+) this can take days. Start blocks should be the earliest factory deployment block.

**8. Cache save errors silently discarded in `discover` subcommand** (`cli/src/main.rs:1048, 1052, 1055`)
`let _ = cache.put_discovered_pool(&info);` discards any error from sled writes. If the cache directory is corrupted or full, the user won't know their discovery results weren't saved.

**9. Batch discovery failure is fatal during auto-discovery in `run`** (`core/src/pool/discovery.rs:167-169, 195-197`)
The `?` operator propagates batch errors upward. A single RPC timeout or rate-limit error aborts the entire discovery process.

**10. No pool deduplication between cache and registry** (`core/src/run.rs:82-84`)
`PoolManager::add_pool()` uses `pools.insert(addr, state)` — a `HashMap` insert. If a pool address appears in both the JSON registry and the sled cache, the second load overwrites the first with no warning.

---

## Proposed Improvements

### A. Filter pools that don't exist at the target block before initialization

**Files:** `core/src/run.rs`, `core/src/pool/state.rs`

**What:** Before `init_from_rpc()`, verify each pool exists at the target block via `eth_getCode`. Skip pools with zero code at that block (they didn't exist yet).

**Implementation:**
- Add `filter_pools_existing()` method on `PoolManager`:
  ```rust
  pub async fn filter_pools_existing(rpc: &RpcClient, pools: &[PoolInfo], block_num: u64) -> Vec<PoolInfo>
  ```
  - Calls `rpc.get_code(pool.address, block_num)` for each pool
  - Returns only those with non-empty code
  - Uses concurrent calls (semaphore pattern, like `init_from_rpc`)
- Call it in `BacktestRunner::init_pools()` between cache load and `init_from_rpc()`
- Log `"Filtered out N pools that didn't exist at block {block}"`

**Tradeoffs:**
- + More accurate pool state, less noise in results
- + Smaller PoolManager → faster arbitrage pair computation, faster tx filtering
- - Additional RPC calls — one `eth_getCode` per pool (cheap, ~200 bytes response)
- - Slightly longer init time (~100ms per 100 pools with concurrency=10)

---

### B. Upgrade zero sqrt price assertion to runtime warning

**File:** `core/src/pool/state.rs:489-493`

**What:** Change to:
```rust
if sqrt.is_zero() {
    tracing::warn!("V3 pool {} initialized with zero sqrt price", addr);
}
debug_assert!(!sqrt.is_zero(), "V3 pool {} initialized with zero sqrt price", addr);
```
Both the runtime warning and debug assertion fire.

---

### C. Add configuration for per-factory V2 fee overrides

**Files:** `core/src/config.rs`, `core/src/pool/discovery.rs`

**What:** Allow configuring custom fee rates per V2 factory address.

**Implementation:**
- Add to `ChainConfig`:
  ```rust
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub uniswap_v2_factory_fees: Option<HashMap<String, u32>>,
  ```
- Pass `&HashMap<Address, u32>` (or similar) through `discover_pools()` → `discover_v2_pools()`
- Fee lookup falls back to 30 bps if factory not in map
- Update default configs for known forks (PancakeSwap=25, etc.)

**Tradeoffs:**
- + Correct fee rates for V2 forks → more accurate profit estimates
- - Adds configuration complexity
- - Need to research and maintain per-factory fee data

---

### D. Add warnings for malformed log events in discovery

**File:** `core/src/pool/discovery.rs:68-69, 111-112`

**What:** Replace:
```rust
if log_data.data.len() < 64 || topics.len() < 3 {
    continue;
}
```
With:
```rust
if log_data.data.len() < 64 || topics.len() < 3 {
    tracing::warn!("Skipping malformed V2 PairCreated log at block {:?}: data.len={}, topics.len={}",
        log.block_number, log_data.data.len(), topics.len());
    continue;
}
```
(Same for V3)

---

### E. Handle Curve & Balancer pools (Option B recommended)

**File:** `core/src/pool/state.rs:519-523`, `core/src/run.rs`

**Option A (implement state fetching):** Larger effort. Requires:
- Curve: `get_virtual_price()`, `balances(uint256)`, and `A()` calls
- Balancer: `getPoolTokens(bytes32 poolId)` via vault
- Token index mapping from on-chain responses

**Option B (skip at load time, recommended for now):** In `add_pool_to_manager()`, skip Curve/Balancer pools with a `tracing::warn!`:
```rust
DexType::Curve | DexType::Balancer => {
    tracing::warn!("Skipping {} pool {}: state fetching not yet implemented",
        info.dex_type.label(), info.address);
    return;
}
```
This prevents them from consuming memory and clogging `token_index` while state fetching is unimplemented.

---

### F. Fix silently discarded cache errors in `discover` subcommand

**File:** `cli/src/main.rs:1048, 1052, 1055`

**What:** Change:
```rust
let _ = cache.put_discovered_pool(&info);
```
To use `tracing::warn!` on error:
```rust
if let Err(e) = cache.put_discovered_pool(&info) {
    tracing::warn!("Failed to cache pool {}: {}", info.address, e);
}
```
Same for cursor saves.

---

### G. Use factory deployment blocks as default discovery start

**File:** `core/src/config.rs`

**What:** Update the `default_chains()` function to set `pool_discovery_start_block` to the earliest factory deployment block per chain.

**Blocks to use:**
| Chain | Suggested start block | Rationale |
|-------|---------------------|-----------|
| Ethereum | 10,000,835 | Uniswap V2 factory deploy |
| Polygon | 49,100,000 | QuickSwap factory deploy (~Aug 2021) |
| BSC | 5,063,800 | PancakeSwap V2 factory deploy |
| Arbitrum | 172,000 | Uniswap V3 factory deploy |
| Avalanche | 4,200,000 | Trader Joe factory deploy |
| Base | 96,000 | Aerodrome factory (at genesis-ish) |
| Optimism | 10,827,000 | Uniswap V3 deploy |

Users can override to 0 in config if they want full historical.

---

### H. Add resilience to batch discovery failures

**File:** `core/src/pool/discovery.rs:167-169, 195-197`

**What:** Wrap each batch call with error handling instead of `?`:
```rust
match discover_v2_pools(rpc, factory, current, end).await {
    Ok(pools) => {
        for pool in &pools {
            let info: PoolInfo = pool.clone().into();
            cache.put_discovered_pool(&info)?;
        }
        total += pools.len();
        cache.put_discovery_cursor(&factory, end)?;
    }
    Err(e) => {
        tracing::warn!("V2 discovery batch {factory} blocks {current}..{end} failed: {e}");
        // Don't advance cursor — retry this batch next time
    }
}
```
Same for V3. Note: do NOT advance cursor on failure so the batch is retried next run.

---

### I. Add explicit pool deduplication

**File:** `core/src/run.rs:82-84`

**What:** When loading from cache, check for duplicates against already-loaded registry pools:
```rust
for info in &pools {
    if pool_manager.get(&info.address).is_some() {
        tracing::debug!("Skipping duplicate pool {} (already loaded from registry)", info.address);
        continue;
    }
    add_pool_to_manager(pool_manager, info.clone());
}
```

---

## Recommended Priority Ordering

### Phase 1 — Core correctness (single PR)
- **A** — Filter pools that don't exist at target block
- **B** — Runtime check for zero sqrt price
- **D** — Warn on malformed log events
- **E (Option B)** — Skip Curve/Balancer at load time
- **H** — Resilient batch discovery
- **I** — Explicit deduplication

### Phase 2 — Performance (follow-up PR)
- **G** — Factory deployment blocks as defaults
- **F** — Fix silently discarded errors

### Phase 3 — Features (future)
- **C** — Per-factory V2 fee overrides
- **E (Option A)** — Full Curve/Balancer state fetching
