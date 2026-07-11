# mev-scout Fetch Benchmarking — Remaining Tasks

## Current Status

**Best result (100 blocks):** 14.4s with gzip + 3 providers at 20 RPS
**Best result (1000 blocks):** 352.9s (~6 min) — avg RPC 15s/block — **target is ≤60s**

## Problem: Free providers throttle under sustained load

Short burst tests (100 blocks) show providers handle 20+ RPS. But over 1000+ blocks,
avg RPC latency jumps from ~1.5s (burst) to ~15s (sustained). Rate limiter at 20 RPS
is no longer the bottleneck — the providers themselves are throttling after ~300 requests.

### Evidence
| Test | Time | Avg RPC/block | Notes |
|------|------|---------------|-------|
| 100 blocks, 20 RPS each | 14.4s | 5.4s | No throttling |
| 1000 blocks, 5 RPS each | 360.9s | 20.1s | Rate limiter bottleneck |
| 1000 blocks, 20 RPS each | 352.9s | 15.1s | Provider throttling, not rate limiter |

Raising RPS from 5→20 barely improved 1000-block time (361s→353s), proving the
bottleneck is provider-side throttling, not our rate limiter.

## TODO Items

### 1. Add more providers to distribute sustained load
Test and add 5 new free providers:
- `https://rpc.ankr.com/polygon` (Ankr)
- `https://1rpc.io/matic` (1RPC)
- `https://endpoints.omniatech.io/v1/matic/mainnet/public` (OmniAtech)
- `https://polygon.meowrpc.com` (MeowRPC)
- `https://polygon-rpc.com`

Each provider needs to pass the compatibility test:
```bash
# Must return valid JSON with both eth_getBlockByNumber and eth_getBlockReceipts
curl -s -X POST -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_getBlockReceipts","params":["0x55D47D0"],"id":1}' \
  https://NEW_PROVIDER_URL
```

Then add working ones to `mev-scout.toml`:
```toml
rpc_urls = [
  "https://polygon-bor-rpc.publicnode.com",
  "https://polygon.drpc.org",
  "https://polygon-public.nodies.app",
  "https://NEW_PROVIDER_1",
  "https://NEW_PROVIDER_2",
  # ... etc
]
rpc_rps = [20.0, 20.0, 20.0, 20.0, 20.0, ...]
```

Also update `rpc_workers` to match provider count (e.g., 6 for 6 providers).

### 2. Investigate per-provider semaphore (optional)
Currently each shard runs `block_concurrency` (25) concurrent tasks against one provider.
The rate limiter (token bucket) throttles to 20 RPS, but all 25 tasks compete for tokens.
A per-provider semaphore matching the RPS could reduce queuing overhead.

Location: `core/src/fetch/fetcher.rs` — `fetch_contiguous_range_pinned()` at line ~434.

### 3. Investigate gzip compression savings
Gzip helped 100-block time (16-18s → 14.4s). But at 1000-block scale the savings are
masked by provider throttling. Could measure response sizes to quantify compression ratio.

### 4. Consider response caching / ETags
Free providers may support ETags or conditional requests. Could reduce bandwidth and
potentially bypass some throttling by reusing partial responses.

### 5. Consider WebSocket providers
Some free providers offer WSS endpoints which may have higher rate limits and lower
latency than HTTP. Would require changes to the alloy transport layer.

### 6. Re-test quiknode.pro with equal weight
Previous test showed quiknode.pro made things worse when given weight=5 (got 61 of 100
blocks). With provider-pinned distribution and equal shards, it might perform better.
Config change:
```toml
rpc_urls = [..., "https://rpc-mainnet.matic.quiknode.pro/<KEY>"]
rpc_rps = [20.0, 20.0, 20.0, 20.0]
```

### 7. EIP-7702 full support (not just filtering)
Current fix filters out 0x7f transactions. Full support would require:
- `core/src/data/types.rs`: Add `tx_type: u8` to `TxData` struct
- `core/src/rpc/client.rs:811-836`: Map tx type in `alloy_tx_to_tx_data()`
- `core/src/replay/replayer.rs:268`: Use actual tx_type instead of hardcoded 0
- `core/src/rpc/client.rs:448`: Add `clean_block_transactions` to `get_pending_block()`

### 8. Benchmark with 5+ providers (target: ≤60s for 1000 blocks)
Once enough providers are added (6+), each handling ~167 blocks with some parallelism
should bring total time down. The math: 1000 blocks / 6 providers = ~167 blocks each.
At 20 RPS sustained (optimistic), that's ~8.3s rate-limited + ~1.5s/block × 167 = ~259s.
Still likely above 60s with free providers — may need 10+ providers or paid tiers.

## Files Modified This Session
- `core/Cargo.toml`: reqwest 0.12→0.13, added `gzip` feature
- `core/src/rpc/client.rs`: Added `build_http_client()`, changed both factory methods to use `AlloyRpcClient::new_http_with_client()` with shared gzip-enabled reqwest client
- `mev-scout.toml`: rpc_rps set to [20.0, 20.0, 20.0], rps_limit=60.0
