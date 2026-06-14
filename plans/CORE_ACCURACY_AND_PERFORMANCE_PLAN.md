# Core Module Accuracy & Performance Plan

This plan covers accuracy-critical and performance-critical improvements in `core/src/` excluding the `mev/` and `pool/` subdirectories (which have their own plans). Prioritization: **accuracy first** (simulation correctness, data integrity), then **speed/optimization** (throughput and resource usage, excluding block-level parallelization).

---

## Priority Matrix

| # | File | Issue | Category | Severity | Effort |
|---|------|-------|----------|----------|--------|
| A1 | `aggregate.rs:92-94` | DEX metrics assign ALL opps to ALL dexes | Accuracy | **Critical** | Medium |
| A2 | `utils.rs:8` | `u128_from_be_bytes` panics on short slices | Accuracy | **Critical** | Trivial |
| A3 | `run.rs:156` | `self.pool_manager` used after `std::mem::take` | Accuracy | **Critical** | Small |
| A4 | `types.rs:283-299` + `config.rs` | Global `gas_limit` silently ignored by cost computation | Accuracy | **Critical** | Small |
| A5 | `replay.rs:638-640` | Receipt verification only compares `topic[0]` | Accuracy | High | Small |
| A6 | `replay.rs:260-261, 346-347` | Unknown code hash returns empty bytecode | Accuracy | High | Small |
| A7 | `replay.rs:633` | Zip-based log comparison can misalign | Accuracy | High | Small |
| A8 | `replay.rs:260` | `code_by_hash` doesn't consult sled cache | Accuracy | High | Small |
| A9 | `validation.rs` | Duplicated validation logic across `validate_and_resolve_for` and `validate_replay` | Accuracy | High | Small |
| P1 | `coingecko.rs:102` | HTTP client recreated per request — no connection reuse | Performance | Medium | Trivial |
| P2 | `cache.rs` | No `flush()` after write operations — data loss risk on crash | Performance | Medium | Small |
| P3 | `aggregate.rs` | Multiple redundant iterations and repeated `wei_to_eth` conversions | Performance | Medium | Medium |
| P4 | `replay.rs:382-389` | Sequential RPC calls for Polygon precompiles | Performance | Medium | Small |
| P5 | `cli.rs:60` | `--block` accepts 0 at CLI level (caught late by validation) | Maintenance | Low | Trivial |
| P6 | `rpc.rs:90-120` | `retry_call` doesn't distinguish retryable vs non-retryable errors | Performance | Low | Small |

---

## Accuracy: Critical

### A1: DEX Metrics Assign ALL Opportunities to ALL Dexes

**File:** `aggregate.rs:92-94`

**Problem:**
```rust
for opp in opportunities {
    let sname = ui_strategy_name(opp.strategy).to_string();
    by_strategy.entry(sname).or_default().push(opp);

    for dex_meta in dexes {
        by_dex.entry(dex_meta.name.clone()).or_default().push(opp);
    }
}
```

Every opportunity is pushed into **every** DEX bucket regardless of which DEX it touched. The test at `aggregate.rs:373` acknowledges this: *"aggregate() assigns ALL opps to ALL dexes"*. This makes the following metrics completely wrong:
- `by_dex[N].opportunities` — inflated by N×
- `by_dex[N].revenue` — double-counts each opportunity for every configured DEX
- `by_dex[N].avg_profit` — based on wrong totals
- `by_dex[N].profitable` — each opportunity counted for every DEX
- Sorting by revenue is meaningless

Per the existing test comment, *"both get the same total revenue (4 ETH)"* — confirming this defect.

**Fix:**

Build a reverse pool-address-to-DEX-name lookup table and pass it to `aggregate()`. The `DexMeta` struct already has `name` and `fork`. Add a `pools: Vec<Address>` field to `DexMeta` (or pass a separate `HashMap<Address, &str>`):

```rust
pub struct DexMeta {
    pub name: String,
    pub fork: String,
    pub tx_count: usize,
    pub pool_addresses: Vec<Address>,  // NEW — pools belonging to this DEX
}

pub fn aggregate(
    opportunities: &[MevOpportunity],
    dexes: &[DexMeta],
    usd_price: f64,
) -> AggregationResult {
    // Build reverse lookup: pool_address -> dex_name
    let mut pool_to_dex: HashMap<Address, &str> = HashMap::new();
    for dex in dexes {
        for &addr in &dex.pool_addresses {
            pool_to_dex.insert(addr, &dex.name);
        }
    }

    for opp in opportunities {
        let sname = ui_strategy_name(opp.strategy).to_string();
        by_strategy.entry(sname).or_default().push(opp);

        // Only push to dexes that actually have this pool
        if let Some(dex_name) = pool_to_dex.get(&opp.pool_a) {
            by_dex.entry(dex_name.to_string()).or_default().push(opp);
        } else if let Some(dex_name) = pool_to_dex.get(&opp.pool_b) {
            by_dex.entry(dex_name.to_string()).or_default().push(opp);
        }
    }
    // ... rest unchanged ...
}
```

Alternatively, if `MevOpportunity` already carries pool DEX info in its `pool_a`/`pool_b` addresses, the reverse lookup could be built from the pool registry directly.

**Caller change** (`run.rs` or the binary that constructs `DexMeta`):
- Populate `DexMeta.pool_addresses` when building DEX metadata
- Or, if that information isn't available at the call site, pass `pool_to_dex` as a separate parameter

**Tests:**
- `test_dex_metrics_correctly_partitioned`: Create 2 dexes, 3 opportunities (1 involving DEX A, 2 involving DEX B). Verify `by_dex` counts are [1, 2] not [3, 3].
- `test_dex_metrics_unknown_pool`: Opportunity with pool address not in any DEX. Verify it's excluded from all DEX buckets but still counted in summary.

---

### A2: `u128_from_be_bytes` Panics on Short Slices

**File:** `utils.rs:5-10`

**Problem:**
```rust
pub fn u128_from_be_bytes(bytes: &[u8]) -> u128 {
    let start = bytes.len().saturating_sub(16);
    let mut buf = [0u8; 16];
    buf.copy_from_slice(&bytes[start..start + 16]);  // PANICS when bytes.len() < 16
    u128::from_be_bytes(buf)
}
```

When `bytes.len() < 16`, `start = 0` and the slice `bytes[0..16]` is out of bounds → **runtime panic**. The docstring says *"If the slice is shorter than 16 bytes, leading bytes are treated as zero"* but the implementation doesn't handle this case.

**Fix:**
```rust
pub fn u128_from_be_bytes(bytes: &[u8]) -> u128 {
    let mut buf = [0u8; 16];
    let len = bytes.len().min(16);
    buf[16 - len..].copy_from_slice(&bytes[bytes.len().saturating_sub(len)..]);
    u128::from_be_bytes(buf)
}
```

This correctly right-pads short slices with leading zeros (big-endian).

**Tests:**
- `test_u128_from_be_bytes_short_slice_5_bytes`: Input of 5 bytes (e.g., `[0x00, 0x00, 0x00, 0x00, 0x2a]` — value 42). Verify no panic and result is 42.
- `test_u128_from_be_bytes_short_slice_1_byte`: Input `[0x2a]`. Verify result is 42.
- `test_u128_from_be_bytes_empty_slice`: Empty input. Verify result is 0.
- `test_u128_from_be_bytes_long_slice`: 32-byte input (existing test). Verify no regression.

---

### A3: `self.pool_manager` Used After `std::mem::take`

**File:** `run.rs:149-156`

**Problem:**
```rust
let pool_manager = std::mem::take(&mut self.pool_manager);  // line 149 — empties self.pool_manager
let pool_manager = RefCell::new(pool_manager);              // line 150 — wraps taken value
// ... 5 lines of setup ...
jit_detector.seed_pool_tick_cache(&self.pool_manager);       // line 156 — reads EMPTY PoolManager!
```

At line 149, `std::mem::take(&mut self.pool_manager)` replaces `self.pool_manager` with `PoolManager::default()` (all fields zeroed/empty). The JIT detector's `seed_pool_tick_cache` iterates over all V3 pools to populate its tick → tick_spacing mapping. With an empty pool manager, **no V3 pools are ever seeded**.

Consequences:
- `JitDetector::get_pre_swap_tick()` (jit.rs:132) falls back to the post-swap tick as an estimate of the pre-swap tick
- The "was this mint in range" check always passes for the first swap
- **False-positive JIT detections** on V3 pools for the first swap in each block
- **Missed JIT detections** when the first swap genuinely crosses the mint range but the tick estimate is inaccurate

**Fix:**

Move `seed_pool_tick_cache` before the `std::mem::take`:

```rust
// line 140-156 restructured:
let pool_addrs: std::collections::HashSet<_> =
    self.pool_manager.pool_addresses().into_iter().collect();
let token_addrs: std::collections::HashSet<_> =
    self.pool_manager.token_addresses().into_iter().collect();

let mut jit_detector = JitDetector::new(block_num);
jit_detector.seed_pool_tick_cache(&self.pool_manager);  // MOVED HERE — before take

let pool_manager = std::mem::take(&mut self.pool_manager);
let pool_manager = RefCell::new(pool_manager);
// ...
```

**Tests:**
- `test_jit_v3_pool_seeding`: Create `BacktestRunner` with V3 pools. Run `run_block()` on a block with a V3 swap. Before fix: first V3 swap always matches all active mints. After fix: only mints whose range the swap actually crosses are matched.
- Existing JIT tests should pass unchanged.

---

### A4: Global `Config.gas_limit` Silently Ignored

**Files:** `config.rs:64`, `types.rs:283-299`

**Problem:**

`Config.gas_limit` (default 200_000, settable via TOML or `--gas-limit` CLI) is stored as a field but `GasConfig::gas_limit_for_strategy()` at `types.rs:283-299` uses **hardcoded per-strategy defaults** and never reads the global limit:

```rust
pub fn gas_limit_for_strategy(
    &self,
    strategy: Strategy,
    overrides: &std::collections::HashMap<String, u64>,
) -> u64 {
    let key = strategy.to_string();
    if let Some(&limit) = overrides.get(&key) {
        return limit;  // per-strategy override from gas_limits map works
    }
    match strategy {
        Strategy::TwoHopArb => 150_000,     // hardcoded
        Strategy::MultiHopArb => 300_000,   // hardcoded
        Strategy::Jit => 300_000,           // hardcoded
        Strategy::JitArb => 350_000,        // hardcoded
        Strategy::Sandwich => 200_000,      // hardcoded
    }
}
```

Users who tune `gas_limit` in their config file or via `--gas-limit` see **zero effect** on gas cost calculations. This silently produces incorrect profit estimates for anyone expecting their configured gas limit to be used.

Three semantic options:

**Option A: Apply global limit as a cap/multiplier**

```rust
pub fn gas_limit_for_strategy(
    &self,
    strategy: Strategy,
    overrides: &std::collections::HashMap<String, u64>,
) -> u64 {
    let key = strategy.to_string();
    if let Some(&limit) = overrides.get(&key) {
        return limit;
    }
    let default = match strategy {
        Strategy::TwoHopArb => 150_000,
        // ...
    };
    // Apply global gas_limit as a multiplier: effective_limit = default * self.gas_limit / 200_000
    let ratio = (self.gas_limit as u128).saturating_mul(1_000_000) / 200_000u128;
    let effective = (default as u128).saturating_mul(ratio) / 1_000_000;
    effective as u64
}
```

**Option B: Global limit replaces all per-strategy defaults**

```rust
pub fn gas_limit_for_strategy(
    &self,
    strategy: Strategy,
    overrides: &std::collections::HashMap<String, u64>,
) -> u64 {
    let key = strategy.to_string();
    if let Some(&limit) = overrides.get(&key) {
        return limit;
    }
    self.gas_limit  // Use global limit for all strategies
}
```

Simple and obvious, but loses per-strategy nuance.

**Option C: Remove `self.gas_limit` from `GasConfig`** (it already has per-strategy overrides via the HashMap). Keep only `overrides` and hardcoded defaults. Remove the CLI/config field to avoid confusion.

**Recommendation:** Option A — it respects the user's intent (tuning overall gas cost magnitude) while preserving per-strategy defaults as the baseline.

**Tests:**
- `test_gas_limit_global_applies`: Set `GasConfig { gas_limit: 400_000 }`. For `Strategy::TwoHopArb`, verify returned limit > 150_000 (scaled up by 2×).
- `test_gas_limit_global_unchanged_at_default`: `GasConfig { gas_limit: 200_000 }`. Verify per-strategy defaults are unchanged.
- `test_gas_limit_global_zero_not_allowed`: `GasConfig { gas_limit: 0 }`. Verify it doesn't produce zero-cost (clamp at minimum).

---

## Accuracy: High

### A5: Receipt Verification Only Compares `topic[0]`

**File:** `replay.rs:638-640`

**Problem:**
```rust
if !l.data.topics().is_empty() && !r.topics.is_empty()
    && l.data.topics()[0] != r.topics[0]
```

Only the first log topic is compared between revm execution and cached receipts. Solidity indexed event parameters beyond topic[0] (topics[1..] for `event Transfer(address indexed from, address indexed to, uint256 value)`) are silently ignored. This means receipt verification can pass even when indexed event data differs, masking simulation inaccuracies.

**Fix:** Compare all topics:
```rust
if l.data.topics() != r.topics.as_slice() {
    mismatches.push(format!("log[{}].topics", i));
}
```

Revm's `Log.data.topics()` returns `&[B256]` and `LogData.topics` returns `Vec<B256>`. Their `PartialEq` compares element-by-element.

**Tests:**
- `test_verify_receipt_log_topics_full_match`: Exec log with topics `[A, B]`, receipt log with `[A, B]`. No mismatch.
- `test_verify_receipt_log_topics_second_mismatch`: Exec `[A, B]`, receipt `[A, C]`. Mismatch reported for topics.
- `test_verify_receipt_log_topics_length_mismatch`: Exec `[A, B]`, receipt `[A]`. Mismatch on length.

---

### A6: Unknown Code Hash Returns Empty Bytecode

**File:** `replay.rs:260-261, 346-347`

**Problem:**
```rust
fn code_by_hash(&mut self, code_hash: B256) -> Result<Bytecode, Self::Error> {
    if code_hash == KECCAK_EMPTY {
        return Ok(Bytecode::new());
    }
    if let Some(code) = self.codes.get(&code_hash) {
        return Ok(code.clone());
    }
    tracing::warn!(?code_hash, "code_by_hash: unknown code hash");
    Ok(Bytecode::new())  // ← returns empty bytecode!
}
```

When `CachedRpcDb` encounters a code hash not in its in-memory `self.codes` cache, it silently returns empty bytecode. This means revm executes the contract as a no-op (immediately returns success with no state changes), producing **incorrect simulation results**:
- Wrong state transitions
- Missing logs
- Wrong gas usage
- Transactions that should revert may appear to succeed

Same issue exists in `code_by_hash_ref` at line 346.

**Root cause:** The in-memory `self.codes: HashMap<B256, Bytecode>` is populated:
1. In `basic()` at line 198 — when code is found via cache or RPC
2. In `basic()` at line 227-228 — when code is fetched from RPC via `get_proof`

But `code_by_hash` is called by revm independently of `basic()` — revm may call `basic()` for one address, then `code_by_hash()` for a different address that happens to have the same code hash. If that code hash wasn't already cached, the lookup fails.

**Fix:**
```rust
fn code_by_hash(&mut self, code_hash: B256) -> Result<Bytecode, Self::Error> {
    if code_hash == KECCAK_EMPTY {
        return Ok(Bytecode::new());
    }
    if let Some(code) = self.codes.get(&code_hash) {
        return Ok(code.clone());
    }
    // Attempt to find the code in sled cache by scanning stored codes
    // (or return an error instead of empty bytecode)
    Err(DbError(anyhow::anyhow!(
        "code_by_hash: unknown code hash {code_hash:?}"
    )))
}
```

This changes the behavior from "silent incorrect simulation" to "fail-fast with error". The error will be caught by `replay_each_filtered`'s error handling (line 716-728 in `replay_to`, line 837-850 in `replay_each`, line 949-962 in `replay_each_filtered`), where it produces a `Revert` execution result.

An alternative approach: keep a `HashMap<B256, Address>` mapping code hashes back to their contract addresses, so `code_by_hash` can fetch from sled via the address path. This requires storing the mapping during `basic()` calls.

**Tests:**
- `test_code_by_hash_known`: Insert a code + hash via `basic()`. Call `code_by_hash()` with that hash. Verify it returns the code.
- `test_code_by_hash_unknown_errors`: Call `code_by_hash()` with a hash that's never been inserted. Verify it returns `Err` (not empty bytecode).
- `test_code_by_hash_empty`: Call with `KECCAK_EMPTY`. Verify it returns empty bytecode (success).

---

### A7: Zip-Based Log Comparison Can Misalign

**File:** `replay.rs:633`

**Problem:**
```rust
for (i, (l, r)) in exec_logs_filtered.iter().zip(receipt_logs.iter()).enumerate() {
    if l.address != r.address {
        mismatches.push(format!("log[{}].address", i));
    }
```

`zip` stops at the shorter iterator. If the two filtered lists have the same length but different log positions (due to system-address filtering removing different logs from each side), the element-by-element comparison will:
- Compare log 0 with log 0 (wrong if log 0 doesn't correspond)
- Miss mismatches for logs after the first positional divergence
- Potentially report false mismatches for otherwise-identical logs

This is a real concern for Polygon blocks where system contract logs (0x1001, 0x1010) interleave with user logs in unpredictable patterns.

**Fix:**

Replace the zip-based comparison with an index-based approach that accounts for potential misalignment:

```rust
// Approach: compare by log address first to establish alignment
let mut receipt_idx = 0;
for (i, exec_log) in exec_logs_filtered.iter().enumerate() {
    // Find the matching receipt log by address
    let matching_receipt = receipt_logs.iter().skip(receipt_idx).find(|r| r.address == exec_log.address);
    match matching_receipt {
        None => {
            mismatches.push(format!("log[{}]: no matching receipt log for address {}", i, exec_log.address));
        }
        Some(matched_r) => {
            // Compare topics (all of them — see A5 fix)
            let exec_topics = exec_log.data.topics();
            if exec_topics != matched_r.topics.as_slice() {
                mismatches.push(format!("log[{}].topics mismatch", i));
            }
        }
    }
}
```

A simpler but less robust alternative: iterate indexed instead of zipped:
```rust
let compare_count = exec_logs_filtered.len().min(receipt_logs.len());
for i in 0..compare_count {
    let l = &exec_logs_filtered[i];
    let r = &receipt_logs[i];
    // ... existing comparison ...
}
```

This at least won't skip trailing entries, though positional mismatches remain possible.

**Tests:**
- `test_verify_receipt_logs_same_count_misaligned`: Create exec_logs `[A, B]`, receipt_logs `[B, A]`. Verify that mismatches are correctly reported (or the method handles gracefully).
- `test_verify_receipt_logs_extra_exec_log`: 3 exec logs, 2 receipt logs (different counts). Count check at line 626 catches this.

---

### A8: `code_by_hash` Doesn't Consult Sled Cache

**File:** `replay.rs:253-262`

**Problem:**

When a code hash misses the in-memory `self.codes` HashMap, `code_by_hash` returns empty bytecode without checking sled. The `basic()` method at lines 192-199 does check sled for code by address, but `code_by_hash` has no address context — it only has the hash.

If the in-memory cache is cleared (e.g., between blocks), `code_by_hash` will fail to find any code even if it's stored in sled.

**Fix:**

Maintain a reverse mapping from `code_hash` to the contract address that produced it, populated during `basic()` calls:

```rust
pub struct CachedRpcDb {
    // ... existing fields ...
    code_hash_to_address: HashMap<B256, Address>,  // NEW
}
```

Then in `code_by_hash`:
```rust
fn code_by_hash(&mut self, code_hash: B256) -> Result<Bytecode, Self::Error> {
    if code_hash == KECCAK_EMPTY {
        return Ok(Bytecode::new());
    }
    if let Some(code) = self.codes.get(&code_hash) {
        return Ok(code.clone());
    }
    // Try to find via sled using the stored address mapping
    if let Some(address) = self.code_hash_to_address.get(&code_hash) {
        if let Ok(Some(code_bytes)) = self.cache.get_code(*address) {
            if keccak256(&code_bytes) == code_hash {
                let bytecode = Bytecode::new_raw(code_bytes);
                self.codes.insert(code_hash, bytecode.clone());
                return Ok(bytecode);
            }
        }
    }
    tracing::warn!(?code_hash, "code_by_hash: unknown code hash");
    Err(DbError(anyhow::anyhow!("code_by_hash: unknown code hash {code_hash:?}")))
}
```

**Tests:**
- `test_code_by_hash_via_sled`: Insert code into sled via `put_code()`. Call `basic()` to populate the hash→address mapping. Clear `self.codes`. Call `code_by_hash()`. Verify it finds the code via sled.
- `test_code_by_hash_no_address_mapping`: Call `code_by_hash()` with no prior `basic()` call. Verify it returns error (A6 behavior).

---

### A9: Duplicated Validation Logic

**File:** `validation.rs`

**Problem:**

The following validation logic is duplicated between `validate_and_resolve_for()` (line 263-276) and `validate_replay()` (line 181-193):
1. RPC URL empty check
2. RPC URL scheme check (`http://` or `https://`)

Additionally, `count_set_flags()` and from/to pairing checks are duplicated in both paths.

This means a fix to one path might not be applied to the other, leading to inconsistent validation behavior. For example, if the RPC URL validation were enhanced to check for `localhost` being allowed in dev mode, it would need to be applied in both places.

**Fix:**

Extract shared helpers:
```rust
fn validate_rpc_url(url: &str) -> Result<(), ValidationError> {
    if url.trim().is_empty() {
        return Err(ValidationError::Message(
            "Error: --rpc URL cannot be empty.".to_string(),
        ));
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(ValidationError::Message(format!(
            "Error: --rpc URL '{}' must start with http:// or https://.",
            url
        )));
    }
    Ok(())
}

fn validate_from_to_pairing(from: Option<u64>, to: Option<u64>) -> Result<(u64, u64), ValidationError> {
    match (from, to) {
        (Some(_), None) | (None, Some(_)) => Err(ValidationError::Message(
            "Error: --from-block and --to-block must be used together.".to_string(),
        )),
        (Some(f), Some(t)) if t <= f => Err(ValidationError::Message(format!(
            "Error: --to-block ({t}) must be greater than --from-block ({f})."
        ))),
        (Some(f), Some(t)) => Ok((f, t)),
        (None, None) => Err(ValidationError::Message(
            "Error: no block range specified.".to_string(),
        )),
    }
}
```

**Tests:**
- All existing validation tests pass unchanged (behavior-preserving refactor).

---

## Performance & Optimization

### P1: HTTP Client Recreated Per Request

**File:** `coingecko.rs:102`

**Problem:**
```rust
async fn fetch_price(&self, asset_id: &str) -> Result<f64, anyhow::Error> {
    let client = reqwest::Client::new();  // created on EVERY call
    // ...
}
```

`reqwest::Client::new()` creates a new connection pool, DNS resolver, and TLS session cache on every `fetch_price` call. This means:
- No TCP connection reuse (each request opens a new connection)
- Repeated TLS handshakes (expensive)
- No HTTP/2 multiplexing
- Each `Client::new()` allocates memory for internal state

**Fix:**

Store a `reqwest::Client` in `PriceCache`:
```rust
#[derive(Debug)]
pub struct PriceCache {
    entries: std::collections::HashMap<String, PriceEntry>,
    ttl: std::time::Duration,
    api_key: Option<String>,
    client: reqwest::Client,  // NEW
}

impl PriceCache {
    pub fn new(api_key: Option<String>) -> Self {
        Self {
            entries: std::collections::HashMap::new(),
            ttl: std::time::Duration::from_secs(300),
            api_key,
            client: reqwest::Client::new(),  // created once
        }
    }

    async fn fetch_price(&self, asset_id: &str) -> Result<f64, anyhow::Error> {
        // ... use self.client instead of creating a new one
        let resp = self.client.get(&url).send().await?;
        // ...
    }
}
```

Note: `reqwest::Client` implements `Clone` via `Arc` internally, so this doesn't affect the `Clone` derive on `PriceCache`. However, it does mean `PriceCache` will be `Clone`-able only if `Client` is `Clone` (which it is via `Arc`).

Since `PriceCache` derives `Debug`, this works because `reqwest::Client` also implements `Debug`.

**Tests:**
- `test_price_cache_client_reuse`: Call `usd_price` twice. Verify only one `Client` is created (can't easily test without mocking, but no functionality change).
- Existing tests pass unchanged.

---

### P2: No `flush()` After Write Operations

**File:** `cache.rs`

**Problem:**

Sled uses a write-ahead log (WAL) for durability, but written data may be in an internal buffer and not yet flushed to disk. In the event of a process crash:
- Block data fetched from RPC (potentially hours of work) could be lost
- Pool discovery state could be lost, requiring re-scan
- Run manifests could be lost, breaking run history

The sled documentation recommends calling `flush()` after critical writes.

**Fix:**

Add `flush()` calls after bulk write operations. The key insertion points:

1. **After `fetch_range` completes** (`fetch.rs:94`):
```rust
// In fetch_range, after the loop:
self.cache.flush()?;
```

2. **After pool discovery** (in `pool/discovery.rs`, after storing pools to sled):
```rust
cache.flush()?;
```

Add a `flush()` method to `CacheStore`:
```rust
impl CacheStore {
    /// Flush pending writes to disk for durability.
    pub fn flush(&self) -> anyhow::Result<()> {
        Ok(self.db.flush()?)
    }
}
```

Not every individual `put_block`/`put_tx`/`put_receipt` call needs `flush()` (that would be very slow). Instead, flush at the end of each block-range fetch or at configurable intervals.

**Tests:**
- `test_cache_flush_persists_data`: Write data to cache, call `flush()`, simulate crash by dropping the cache, reopen at same path. Verify data is present.
- `test_cache_no_flush_no_data_loss`: Existing behavior — WAL still provides some durability, just not guaranteed.

---

### P3: Multiple Redundant Iterations in `aggregate.rs`

**File:** `aggregate.rs`

**Problem:**

The same opportunity data is iterated multiple times at the top level:

| Lines | Purpose | Iterations |
|-------|---------|------------|
| 98-101 | `gross_revenue` sum | 1 pass |
| 102-105 | `total_gas` sum | 1 pass |
| 108-114 | `profitable_count` filter | 1 pass |
| 116-119 | `best_single_opp` max | 1 pass |
| 217-225 | wei sums | 1 pass |

**Total: 5 full passes** over `opportunities`.

Inside the per-strategy loop, each strategy's opps are iterated 4 times:
| Lines | Purpose |
|-------|---------|
| 127 | `strat_gross` sum |
| 128 | `strat_gas` sum |
| 131-135 | `profitable` count |
| 136-139 | `best_opp` max |
| 147-149 | wei sums |

Same 4-pass pattern in the per-DEX metrics.

Additionally, `wei_to_eth()` (u128→f64 division) is called repeatedly for the same opportunity value across different passes.

**Fix:**

Single-pass aggregation:
```rust
pub fn aggregate(/* ... */) -> AggregationResult {
    let mut total_profitable = 0usize;
    let mut gross_revenue = 0.0f64;
    let mut total_gas = 0.0f64;
    let mut best_single_opp = 0.0f64;
    let mut summary_gross_wei: u128 = 0;
    let mut summary_gas_wei: u128 = 0;

    // Single pass: compute all summary metrics
    for opp in opportunities {
        let profit_wei = opp.expected_profit.to::<u128>();
        let profit_eth = wei_to_eth(profit_wei);
        let gas_eth = wei_to_eth(opp.gas_cost_wei);

        gross_revenue += profit_eth;
        total_gas += gas_eth;
        summary_gross_wei += profit_wei;
        summary_gas_wei += opp.gas_cost_wei;

        if profit_eth - gas_eth > 0.0 {
            total_profitable += 1;
        }

        if profit_eth > best_single_opp {
            best_single_opp = profit_eth;
        }

        // Per-strategy accumulation (HashMap of accumulators)
        // Per-DEX accumulation
    }
    // ...
}
```

This reduces 5 passes to 1 pass for the summary, and similar improvements for strategy/DEX sub-groups.

**Tests:**
- All existing aggregate tests pass unchanged (behavior-preserving refactor).
- New: `test_aggregate_large_dataset_equal_results`: Generate 10k opportunities, run old vs new implementation, verify identical results.

---

### P4: Sequential RPC Calls for Polygon Precompiles

**File:** `replay.rs:382-389`

**Problem:**
```rust
let code_09 = block_on(rpc.get_code_no_retry(addr_from_last_byte(0x09), prev_block))
    .unwrap_or_default();
let code_0a = block_on(rpc.get_code_no_retry(addr_from_last_byte(0x0a), prev_block))
    .unwrap_or_default();
let code_0b = block_on(rpc.get_code_no_retry(addr_from_last_byte(0x0b), prev_block))
    .unwrap_or_default();
let code_0c = block_on(rpc.get_code_no_retry(addr_from_last_byte(0x0c), prev_block))
    .unwrap_or_default();
```

Four sequential RPC calls, each adding ~100-500ms latency. For a backtest replaying thousands of Polygon blocks, this latency is incurred once per block (when `register_polygon_precompiles` is called before each block replay).

**Fix:**

Parallelize with `futures::join!`:
```rust
use futures::future::join;

let (code_09, code_0a, code_0b, code_0c) = tokio::task::block_in_place(|| {
    handle.block_on(async {
        let f1 = rpc.get_code_no_retry(addr_from_last_byte(0x09), prev_block);
        let f2 = rpc.get_code_no_retry(addr_from_last_byte(0x0a), prev_block);
        let f3 = rpc.get_code_no_retry(addr_from_last_byte(0x0b), prev_block);
        let f4 = rpc.get_code_no_retry(addr_from_last_byte(0x0c), prev_block);
        join!(f1, f2, f3, f4)
    })
});
let code_09 = code_09.unwrap_or_default();
let code_0a = code_0a.unwrap_or_default();
let code_0b = code_0b.unwrap_or_default();
let code_0c = code_0c.unwrap_or_default();
```

This reduces the latency from `4 × RTT` to `1 × RTT` (the slowest response).

**Caching consideration:** The precompile code at addresses 0x09–0x0c rarely changes between blocks. Could be cached in `CacheStore` and only re-fetched when the block number crosses a version boundary.

**Tests:**
- No functional change — precompile registration results should be identical.
- `test_register_polygon_precompiles_parallel`: Verify all 4 codes are fetched and registered correctly.

---

### P5: `--block` Accepts 0 at CLI Level

**File:** `cli.rs:60-62`

**Problem:**
```rust
/// Single specific block number (>0)
#[arg(long, value_name = "NUMBER")]
pub block: Option<u64>,
```

Clap accepts `0` for `--block`, but validation.rs rejects it (line 106-112). The error is caught later rather than at argument parsing time.

**Fix:**

Add `value_parser` to reject 0 at the CLI level:
```rust
/// Single specific block number (>0)
#[arg(long, value_name = "NUMBER", value_parser = clap::value_parser!(u64).range(1..))]
pub block: Option<u64>,
```

Apply the same to `--days` (1..365 range), `--blocks` (1.. range), `--from-block` (1..), `--to-block` (1..).

**Tests:**
- CLI tool will display an error immediately for `--block 0` without reaching validation logic.
- Existing validation tests still pass (they test `Config` struct directly, not CLI parsing).

---

### P6: `retry_call` Doesn't Distinguish Retryable vs Non-Retryable Errors

**File:** `rpc.rs:90-120`

**Problem:**

```rust
async fn retry_call<F, Fut, T>(&self, f: F) -> anyhow::Result<T> {
    let mut last_err = None;
    for attempt in 0..=self.retry.max_retries {
        match f().await {
            Ok(val) => return Ok(val),
            Err(e) => {
                // Always retries, even for non-retryable errors
                tracing::warn!(...);
                last_err = Some(e);
                if attempt < self.retry.max_retries {
                    let delay = ...;
                    sleep(delay).await;
                }
            }
        }
    }
    // ...
}
```

All errors are treated as retryable. A 400 Bad Request (client error) or 404 Not Found should fail immediately, while 503 Service Unavailable, 429 Too Many Requests, or timeout errors should be retried.

**Fix:**

Add error classification:
```rust
fn is_retryable(err: &anyhow::Error) -> bool {
    let err_str = err.to_string().to_lowercase();
    // Retry on network errors, timeouts, rate limits, server errors
    err_str.contains("timeout")
        || err_str.contains("rate limit")
        || err_str.contains("429")
        || err_str.contains("503")
        || err_str.contains("connection refused")
        || err_str.contains("connection reset")
        || err_str.contains("eof")
        || err_str.contains("temporary")
}

async fn retry_call<F, Fut, T>(&self, f: F) -> anyhow::Result<T> {
    let mut last_err = None;
    for attempt in 0..=self.retry.max_retries {
        match f().await {
            Ok(val) => return Ok(val),
            Err(e) => {
                if !is_retryable(&e) {
                    return Err(e);  // fail immediately for non-retryable errors
                }
                tracing::warn!("RPC call failed (attempt {}/{}): {:?}", ...);
                last_err = Some(e);
                if attempt < self.retry.max_retries {
                    let delay = ...;
                    sleep(delay).await;
                }
            }
        }
    }
    // ...
}
```

**Tests:**
- `test_retry_call_non_retryable_error`: Mock a 404 error. Call `retry_call`. Verify it fails after 0 retries (1 attempt total).
- `test_retry_call_retryable_then_succeeds`: Mock 2 retryable errors then success. Verify 3 attempts total and final success.
- `test_retry_call_all_retryable_fail`: Mock 5 retryable errors. Verify 6 attempts and final failure.

---

## Implementation Order

| Phase | Items | Focus | Effort |
|-------|-------|-------|--------|
| **1** | A1, A2, A3, A4 | Critical accuracy bugs | 2-3 days |
| **2** | A5, A6, A7, A8, A9 | High-accuracy fixes | 3-4 days |
| **3** | P1, P2, P3, P4 | Performance optimizations | 2-3 days |
| **4** | P5, P6 | Maintenance & polish | 0.5 day |

**Total:** ~8-10 days for all items. Phases 1-2 (accuracy) should be prioritized before 3-4 (performance).

---

## File Change Summary

| File | Changes |
|------|---------|
| `core/src/aggregate.rs` | Fix DEX partitioning (A1); single-pass aggregation (P3) |
| `core/src/utils.rs` | Fix short-slice panic in `u128_from_be_bytes` (A2) |
| `core/src/run.rs` | Move `seed_pool_tick_cache` before `std::mem::take` (A3) |
| `core/src/types.rs` | Apply global `gas_limit` in `gas_limit_for_strategy` (A4) |
| `core/src/config.rs` | Optionally remove `gas_limit` field if Option C chosen (A4) |
| `core/src/replay.rs` | Compare all log topics (A5); fail on unknown code hash (A6); index-based log comparison (A7); sled-backed `code_by_hash` (A8); parallelize precompile RPC calls (P4) |
| `core/src/validation.rs` | Extract shared RPC URL and range validation (A9) |
| `core/src/cache.rs` | Add `flush()` method (P2) |
| `core/src/fetch.rs` | Call `flush()` after bulk fetch (P2) |
| `core/src/coingecko.rs` | Store `reqwest::Client` in `PriceCache` (P1) |
| `core/src/rpc.rs` | Classify retryable vs non-retryable errors (P6) |
| `core/src/cli.rs` | Add `value_parser` constraints for block range args (P5) |
