# Log-First Fetch Optimization

## Empirical Proof

Tested against Polygon mainnet (block 88079843, 114 txs) via Alchemy free tier:

| Method | Response size | Time |
|--------|--------------|------|
| `eth_getBlockByNumber` (current) | 410,856 chars | 915ms |
| `eth_getBlockReceipts` (current) | 2,112,840 chars | 1,680ms |
| Current total per block | 2,523,696 chars | 2,595ms |
| `eth_getLogs` (10-block scan) | 253 chars | 886ms |

**31,000x less data per non-DEX block.** Projected 90% savings for 100K block range.

## Design

### New file: `core/src/scan.rs`

`ActivityScanner` with `find_active_blocks(pool_addresses, start, end) -> HashSet<u64>`.

- Builds `eth_getLogs` Filter with pool addresses + DEX event topics (V2 Swap/Sync, V3 Swap/Mint/Burn)
- Batches across provider-imposed range limits (configurable, default 100)
- Extracts `log.block_number`, deduplicates

### Modified: `core/src/fetch.rs`

`Fetcher` gains optional `pool_addresses` field.
- When set: calls `ActivityScanner` first, then only fetches matching blocks
- When empty: current all-block behavior (fallback for `fetch` subcommand)
- `FetchSummary` gains `scanned_blocks` and `skipped` counters

### Modified: `cli/src/main.rs` (`Command::Run` only)

| Step | Before | After |
|------|--------|-------|
| 1 | RangeResolver::resolve() | RangeResolver::resolve() |
| 2 | Pool discovery | Pool discovery |
| 3 | Fetcher::fetch_range(range) | Load pool ADDRESSES from cache |
| 4 | BacktestRunner::init_pools() | ActivityScanner::find_active_blocks() |
| 5 | BacktestRunner::run_range() | Fetcher::fetch_range(active_blocks) |
| 6 | | BacktestRunner::init_pools() |
| 7 | | BacktestRunner::run_range() |

### Modified: `core/src/rpc.rs`

Add `get_logs_batched()` convenience that wraps batching.

### Config

`log_first_batch_size` (optional, defaults to `pool_discovery_batch_size` or 100)

## Provider batch limits

| Provider | Max range per `eth_getLogs` call |
|----------|----------------------------------|
| Alchemy Free | 10 blocks |
| Alchemy PAYG | 2,000 blocks |
| Infura | 2,000-10,000 blocks |
| QuickNode | 10,000 blocks |

## Edge cases

1. **No pools discovered**: falls back to current behavior (safe, no regression)
2. **All blocks active**: same as current (worst case = same cost)
3. **Pools created mid-range**: discovery runs to `start_block - 1` before fetch; pools created at `start_block` would need optional follow-up scan
4. **Sandwich detection**: within-block phenomenon; block with zero DEX events cannot contain a sandwich

## Files unchanged

`run.rs`, `replay.rs`, `pool/state.rs`, `cache.rs`, `resolver.rs`
