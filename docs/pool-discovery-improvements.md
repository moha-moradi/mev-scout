# Pool Discovery Module — Improvement Plan

> Focus: Make pool discovery robust on **public RPC endpoints** with aggressive rate limits, while cleaning up code quality and fixing correctness issues.

---

## Context

The pool discovery subsystem scans Ethereum event logs to find active DEX pools
across Uniswap V2/V3, Curve, Balancer, Dodo, Clipper, Solidly, and Camelot
protocols. It currently works but has reliability problems on public/free-tier
RPCs (drpc, CloudFlare, Ankr, etc.) that impose strict `eth_getLogs` block-range
limits and low RPS ceilings.

---

## Phase 1 — Public RPC Survival (P0)

### 1.1 Concurrency-limited metadata fetch

**File:** `core/src/pool/discovery.rs:664-665`

**Problem:**
`join_all(fetch_tasks)` fires every `eth_call` simultaneously. For a 10K-block
range this can be 5,000+ concurrent calls — public RPCs return 429 or drop the
connection.

**Plan:**
```rust
// BEFORE (fire all at once)
use futures::future::join_all;
let results = join_all(fetch_tasks).await;

// AFTER (bounded concurrency)
use futures::stream::{self, StreamExt};

let concurrency = std::env::var("MEV_SCOUT_RPC_CONCURRENCY")
    .ok()
    .and_then(|s| s.parse().ok())
    .unwrap_or(64);

let results: Vec<_> = stream::iter(fetch_tasks)
    .buffer_unordered(concurrency)
    .collect()
    .await;
```

Add `rpc_concurrency` to the config TOML under `[rpc]` with default 64.
Expose `--rpc-concurrency` CLI flag.

---

### 1.2 Configurable batch size for `eth_getLogs`

**File:** `core/src/pool/discovery.rs:119-133`

**Problem:**
The `batch_size` parameter controls the block range per `eth_getLogs` call.
Free-tier RPCs often cap at 5,000–10,000 blocks. The default should be safe.

**Plan:**
- Set default `batch_size` to **2,000** blocks (safe for all public RPCs).
- Document the constraint in the CLI help text.
- Add a validation warning if `batch_size > 5000` with a hint about public RPC limits.
- No code change needed — the parameter already exists. Just update defaults.

---

### 1.3 Retry with exponential backoff on RPC failures

**File:** `core/src/pool/discovery.rs:178-283`

**Problem:**
When `rpc.get_logs()` fails, the code logs a warning and tries the full topic
set once. If that also fails, the entire batch is **silently skipped**. On
public RPCs with intermittent rate limits, this means missing pools.

**Plan:**
Add a retry wrapper around `get_logs` calls in discovery:

```rust
const MAX_RETRIES: u32 = 3;
const BASE_DELAY_MS: u64 = 1_000;

async fn get_logs_with_retry(
    rpc: &RpcClient,
    filter: &Filter,
    batch_start: u64,
    batch_end: u64,
) -> anyhow::Result<Vec<Log>> {
    let mut last_err = None;
    for attempt in 0..MAX_RETRIES {
        match rpc.get_logs(filter).await {
            Ok(logs) => return Ok(logs),
            Err(e) => {
                if attempt < MAX_RETRIES - 1 {
                    let delay = BASE_DELAY_MS * 2u64.pow(attempt);
                    tracing::warn!(
                        "get_logs failed for {batch_start}..{batch_end} (attempt {}/{}): {:#}. \
                         Retrying in {}ms...",
                        attempt + 1, MAX_RETRIES, e, delay
                    );
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap())
}
```

Apply this to:
- The fast-path DEX activity scan (line 178)
- The fallback full-topic scan (line 222)
- All factory scans (lines 293, 335, 382, 429, 465, 506)

**Also:** Replace all `if let Ok(logs)` on factory scans with proper error
logging:
```rust
// BEFORE
if let Ok(logs) = rpc.get_logs(&v2_filter).await { ... }

// AFTER
match get_logs_with_retry(rpc, &v2_filter, current, batch_end).await {
    Ok(logs) => { ... }
    Err(e) => tracing::warn!("V2 factory scan failed for {current}..{batch_end}: {e:#}"),
}
```

---

### 1.4 Provider cooldown awareness in discovery

**File:** `core/src/pool/discovery.rs`

**Problem:**
The RPC client already has per-provider cooldown with exponential backoff
(`rpc/middleware.rs`). But the discovery loop doesn't know when all providers
are in cooldown — it just keeps firing requests that all fail.

**Plan:**
Add a `health_check` method to `RpcClient` that returns whether any provider
is available. Before each batch, check and if all providers are in cooldown,
sleep until one recovers:

```rust
// In the batch loop, before each get_logs call:
if !rpc.has_healthy_providers().await {
    tracing::warn!("All RPC providers in cooldown, waiting 5s...");
    tokio::time::sleep(Duration::from_secs(5)).await;
}
```

This requires adding `has_healthy_providers()` to `RpcClient`.

---

## Phase 2 — Code Quality (P1)

### 2.1 Extract event classification helper

**File:** `core/src/pool/discovery.rs`

**Problem:** The same topic0 → DexType classification appears twice (fast path
lines 186-211 and fallback path lines 229-272).

**Plan:** Extract into:
```rust
fn classify_dex_event(
    topic0: B256,
    log: &Log,
) -> Option<(DexType, Option<[u8; 32]>, Option<(Address, Address)>)> {
    // ... unified classification logic
}
```

Both the fast path and fallback call this single function.

---

### 2.2 Extract factory scan helper

**File:** `core/src/pool/discovery.rs:286-537`

**Problem:** 6 factory scan blocks with ~250 lines of near-identical code.

**Plan:** Extract a generic helper:
```rust
async fn scan_factory_creation_events(
    rpc: &RpcClient,
    factories: &[Address],
    topic: B256,
    from_block: u64,
    to_block: u64,
    decode: impl Fn(&Log) -> Option<DiscoveredPool>,
) -> anyhow::Result<Vec<DiscoveredPool>> {
    let filter = Filter::new()
        .address(factories.to_vec())
        .event_signature(topic)
        .from_block(from_block)
        .to_block(to_block);
    let logs = get_logs_with_retry(rpc, &filter, from_block, to_block).await?;
    Ok(logs.iter().filter_map(|log| decode(log)).collect())
}
```

Each factory type provides its own `decode` closure. This reduces ~250 lines to
~80 lines.

---

### 2.3 Introduce `DiscoveryConfig` struct

**File:** `core/src/pool/discovery.rs:119-133`

**Problem:** `discover_pools` takes 12 parameters.

**Plan:**
```rust
pub struct DiscoveryConfig {
    pub batch_size: u64,
    pub v2_fee_override: Option<u32>,
    pub balancer_vault: Option<Address>,
    pub v2_factories: Vec<Address>,
    pub v3_factories: Vec<Address>,
    pub curve_registry: Option<Address>,
    pub solidly_factories: Vec<Address>,
    pub camelot_factories: Vec<Address>,
}
```

Update `discover_pools`, `discover_and_cache`, and `discover_pools_with_sources`
signatures. Remove the dead `_v2_factory_fees` parameter.

---

### 2.4 Deduplicate Phase 3 lookup (O(n²) → O(n))

**File:** `core/src/pool/discovery.rs:674-675`

**Plan:** Add a `HashSet<Address>`:
```rust
let mut resolved_addrs: HashSet<Address> = factory_pools.keys().copied().collect();
// ...
for (addr, dex_type, ...) in results {
    if resolved_addrs.contains(&addr) {
        continue;
    }
    resolved_addrs.insert(addr);
    // ...
}
```

---

### 2.5 Preserve `creation_block` for event-discovered pools

**File:** `core/src/pool/discovery.rs`

**Plan:** Change `pool_hits` to track the earliest block number:
```rust
let mut pool_hits: HashMap<
    Address,
    (DexType, Option<[u8; 32]>, Option<(Address, Address)>, u64), // added u64 for first_seen_block
> = HashMap::new();
```

In the event scan, update `or_insert` to store `log.block_number.unwrap_or(0)`,
and use `entry` API to keep the minimum. In Phase 3, set `creation_block` from
this value instead of hardcoded `0`.

---

## Phase 3 — Correctness Fixes (P1-P2)

### 3.1 Solidly/Camelot fee handling

**File:** `core/src/pool/discovery.rs:482,523`

**Current:** Both default to `DexType::UniswapV2` with `v2_fee_override`.

**Plan:** Add `DexType::Solidly` and `DexType::Camelot` variants to the enum.
These are functionally V2 (constant-product or stable-swap) but have different
fee models. Update `dex_type.rs`:
```rust
pub enum DexType {
    UniswapV2,
    UniswapV3,
    Solidly,   // NEW: Velodrome, Aerodrome, Equalizer, Thena
    Camelot,   // NEW: Camelot V2
    Curve,
    Balancer,
    Dodo,
    Clipper,
}
```

Update `label()`, `is_concentrated_liquidity()`, and all downstream match arms.
For Solidly, default fee = 0.3% (30 bps). For Camelot, default fee = 0 (unknown
per-pair).

---

### 3.2 Balancer pool_type filter

**File:** `core/src/pool/discovery.rs:391-394`

**Current:** `pool_type > 1` skips ComposableStable (type 3).

**Plan:** Expand to include type 0 (Weighted), 1 (Weighted2Tokens), and
3 (ComposableStable). Skip only type 2+ (deprecated/unknown):
```rust
if pool_type > 3 {
    continue; // Skip unknown/deprecated pool types
}
```

---

### 3.3 Clipper token documentation

**File:** `core/src/pool/discovery.rs:191-206`

**Plan:** Add a doc comment noting that Clipper pools are multi-asset and the
extracted tokens are the swapped pair, not the full pool token set. No code
change needed — this is a known limitation.

---

### 3.4 `tick_spacing_from_fee` completion

**File:** `core/src/dune/pool_discovery.rs:50-58`

**Plan:** Add missing fee tiers:
```rust
pub fn tick_spacing_from_fee(fee: u32) -> i32 {
    match fee {
        100 => 10,
        200 => 4,
        400 => 4,
        500 => 10,
        2500 => 50,
        3000 => 60,
        10000 => 200,
        _ => 60,
    }
}
```

---

## Phase 4 — CLI Improvements (P2)

### 4.1 JSON output flag

**File:** `cli/src/commands/discover.rs`

**Plan:** Add `--json` to `DiscoverArgs`. When set, serialize `Vec<DiscoveredPool>`
to stdout instead of the human-readable table.

### 4.2 Cache instance reuse

**File:** `cli/src/commands/discover.rs:137,243`

**Plan:** Open `SqliteStore` once at the start and pass it to both
`discover_and_cache` and the Dune result caching at the end.

### 4.3 Progress bar for Dune phase

**File:** `cli/src/commands/discover.rs:156-198`

**Plan:** After clearing the on-chain progress bar, create a new indeterminate
spinner for the Dune phase.

---

## Phase 5 — Feature Additions (P3)

### 5.1 Incremental delta-scanning

**Plan:** Add `--incremental` flag. When set:
1. Query the cache for the latest `creation_block` across all cached pools.
2. Use that as `from_block` instead of the user-specified range.
3. Merge new discoveries with existing cached pools.

### 5.2 Pool health check post-discovery

**Plan:** After discovery, for each V2 pool call `getReserves()` and skip pools
with zero reserves. For V3, check `liquidity > 0` at the current tick. This
filters out drained/paused pools.

### 5.3 `--min-pools` early exit

**Plan:** If the user specifies `--min-pools 100` and we already have 100+ pools
from Dune, skip the on-chain scan to save RPC calls.

---

## Configuration Changes

Add to `mev-scout.toml`:
```toml
[rpc]
concurrency = 64          # Max concurrent metadata fetches (default 64)
batch_size = 2000          # eth_getLogs block range per call (default 2000)
retry_attempts = 3         # RPC retry count (default 3)
```

---

## Files to Modify

| File | Changes |
|------|---------|
| `core/src/pool/discovery.rs` | Phases 1–3: retry, concurrency, helpers, config struct |
| `core/src/pool/dex_type.rs` | Phase 3: add Solidly/Camelot variants |
| `core/src/pool/mod.rs` | Re-export new types |
| `core/src/dune/pool_discovery.rs` | Phase 3: tick_spacing completion |
| `core/src/rpc/client.rs` | Phase 1: add `has_healthy_providers()` |
| `cli/src/cli.rs` | Phase 4: add `--json`, `--rpc-concurrency` flags |
| `cli/src/commands/discover.rs` | Phase 4: JSON output, cache reuse, progress |
| `mev-scout.toml` | Phase 5: new config keys |
| `core/src/config/mod.rs` | Phase 5: parse new config keys |

---

## Testing Strategy

1. **Unit tests** for `classify_dex_event` and `scan_factory_creation_events`
2. **Integration test** with mock RPC that returns errors at specific intervals
   to verify retry logic
3. **Manual test** on a public RPC (e.g. drpc free tier) with a 50K block range
   to verify no rate-limit failures
4. **Benchmark** discovery time before/after concurrency changes

---

## Rollout Order

| Step | Phase | Estimated Effort |
|------|-------|-----------------|
| 1 | 1.1 Concurrency limit | 30 min |
| 2 | 1.3 Retry with backoff | 45 min |
| 3 | 1.2 Batch size defaults | 5 min |
| 4 | 1.4 Provider health check | 30 min |
| 5 | 2.1 Extract event classifier | 30 min |
| 6 | 2.2 Extract factory scan helper | 45 min |
| 7 | 2.3 DiscoveryConfig struct | 30 min |
| 8 | 2.4 Phase 3 dedup fix | 10 min |
| 9 | 2.5 creation_block preservation | 15 min |
| 10 | 3.1 Solidly/Camelot DexType | 45 min |
| 11 | 3.2 Balancer filter fix | 5 min |
| 12 | 3.4 tick_spacing completion | 5 min |
| 13 | 4.1 JSON output | 30 min |
| 14 | 4.2 Cache reuse | 10 min |
| 15 | 4.3 Dune progress bar | 15 min |

**Total estimated effort:** ~6 hours

---

## Success Criteria

- [ ] Pool discovery completes without RPC failures on drpc free tier (10K block
      range limit, ~10 RPS)
- [ ] Retry logic recovers from transient 429/connection errors
- [ ] No silent batch skips on RPC failure
- [ ] `discover_pools` parameter count reduced to ≤4 (config struct + rpc +
      block range + callback)
- [ ] All factory scan errors logged with `tracing::warn`
- [ ] `Solidly`/`Camelot` pools correctly typed
- [ ] Phase 3 dedup is O(n) not O(n²)
- [ ] `--json` flag produces valid JSON output
