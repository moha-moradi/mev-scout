# MEV Scout - Improvement Plan

## 1. Fix Dune SQL Deprecated Endpoint ✅ DONE

### Problem
Every Dune query fails with `Deprecated query engine` because `execute_raw_sql` hits the retired `POST /v1/sql/execute` endpoint.

### Root Cause
`core/src/dune/client.rs:139-191` — `execute_raw_sql_with_performance` calls `/v1/sql/execute` directly.

### Fix Applied
Modified `execute_raw_sql_with_performance` to use the **save-then-execute** pattern:
- `execute_raw_sql` now calls `get_or_create_query_id` then `execute_query_by_id_with_performance`
- `create_query` now specifies `"engine": "dune_sql"` (Dune Engine v2)
- Changed default performance tier from `"small"` to `""` (let Dune decide)
- **Result**: 0 compiler warnings (previously 6 dead-code warnings)

### Affected Callers (12+ locations)
| File | Context |
|------|---------|
| `core/src/cache/token_cache.rs:94` | Token cache bulk-load |
| `core/src/dune/token_discovery.rs:58` | Token discovery CLI |
| `core/src/dune/pool_discovery.rs:38,78,124` | V2/V3 pool discovery |
| `core/src/dune/report.rs:719` | Strategy report queries |
| `core/src/dune/audit.rs:102` | Audit queries |
| `cli/src/commands/dune_check.rs:38` | `dune check` command |
| `cli/src/commands/dune_find_blocks.rs:106,151,237,282,326` | Block search |
| `cli/src/commands/dune_query.rs:364,434` | Ad-hoc query command |

### Prerequisite
`get_or_create_query_id` creates public saved queries via `POST /v1/query`. This requires Dune API key with **Read/Write** scope and **Analyst plan**.

---

## 2. Fix V3 Tick Map Initialization (HIGH PRIORITY)

### Problem
14 V3 pools initialized with empty tick maps, making reserve simulation inaccurate for V3 paths.

### Root Cause
V3 pools need tick data loaded from on-chain `slot0` or subgraph. Current init only loads reserves.

### Fix
- Option A: Lazy-load ticks on first swap simulation via `eth_call` to `swap()` calldata
- Option B: Batch-fetch tick data from Uniswap V3 subgraph during pool init
- Option C: Fetch `slot0` and populate tick map from `tickBitmap` at init time

### Impact
Without this, V3 two-hop arb opportunities may have incorrect profit estimates.

---

## 3. Set Default Priority Fee ✅ DONE

### Problem
`priority_fee_gwei=0` causes profit overestimation. The tool warns about this on every run.

### Fix Applied
Changed default in `cli/src/cli.rs:161` from `0.0` to `1.0`.

---

## 4. Candidate Pruning / Profit Pre-filter (MEDIUM PRIORITY)

### Problem
Block 92246855 generated 65+ candidates for tx 0 alone, causing 536s runtime for 1 block.

### Fix
Add a `--min-candidates-per-tx` flag or auto-prune:
- Skip candidates where `input_amount < threshold` (dust)
- Skip candidates where estimated gas cost > estimated profit (impossible arbs)
- Cap at N candidates per tx, keeping only top-profit ones

---

## 5. Gas Used Mismatch Handling (LOW PRIORITY)

### Problem
~40 txs show `gas_used (exec=X, receipt=Y)` where X != Y (sometimes 2x difference).

### Root Cause
`eth_estimateGas` overestimates vs actual `receipt.gasUsed`. Some txs use estimation fallback.

### Fix
- Use `receipt.gasUsed` directly when available (already in receipt data)
- Only fall back to estimation when receipt is missing

---

## 6. Dune `eth_getLogs` Batch Size (LOW PRIORITY)

### Problem
Alchemy free tier limits `eth_getLogs` to 10-block range. Auto-adaptation works (500 -> 100) but logs noisy warnings.

### Fix
Set default `batch_size` to 100 for Alchemy chains, or detect provider tier and adjust silently.

---

## 7. Config Display "RPC not set" ✅ DONE

### Problem
CLI prints "RPC not set" despite `rpc_urls` being configured in TOML.

### Root Cause
The `--rpc` flag and `rpc_urls` config are separate fields. The display logic only checked `rpc_url` (single/legacy).

### Fix Applied
Added `effective_rpc_display()` method that shows `"N provider(s) configured"` when `rpc_urls` or `rpc_url` is set, or `"No RPC configured — using public fallbacks"` otherwise.

---

## Execution Order

1. ~~**Dune fix**~~ ✅ — Unblocks token cache, pool discovery, reports, audits
2. **V3 tick maps** — Improves simulation accuracy
3. ~~**Priority fee default**~~ ✅ — Quick fix, immediate accuracy gain
4. **Candidate pruning** — Major performance improvement
5. **Gas used handling** — Correctness improvement
6. **Batch size** — UX polish
7. ~~**RPC display**~~ ✅ — UX polish
