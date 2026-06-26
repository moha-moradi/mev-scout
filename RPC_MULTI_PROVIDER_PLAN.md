# Multi-RPC Load Distribution Plan

## Problem

We don't have a private RPC with high rate limits. Public/free RPCs have low RPS (~0.5-1 req/s burst). To fetch large block ranges (e.g., 1000 blocks) quickly, we need to distribute work across multiple RPC endpoints simultaneously.

## Current State

The codebase already has **round-robin provider fallback** in `RpcClient` (`core/src/rpc.rs:91-97`, `retry_call` at line 159), but it's designed for failover, not load distribution. Key limitations:

1. **Single rate limiter shared across all providers** (`rpc.rs:96`) — one RPS cap for all
2. **`effective_rpc_urls()` returns only 1 URL** (`config.rs:506-511`) — the user override, ignoring public fallbacks
3. **`Command::Run` doesn't include public fallbacks** (`main.rs:448-449`) — only `Command::Fetch` does
4. **`check_connection()` tests only the first working provider** — not per-endpoint
5. **`validate_rpc_url()` is format-only** (`validation.rs:60-73`) — no connectivity test
6. **Hardcoded endpoints** (`types.rs:33-49`) — no dynamic discovery from chainlist

## RPC Testing Results (Polygon, June 2026)

### Method Support & Performance

| # | Endpoint | blockNumber | getBlockReceipts | getLogs | getProof (archive) | eth_call | Chain ID | Avg Latency | Viable? |
|---|----------|:-----------:|:----------------:|:-------:|:------------------:|:--------:|:---------:|:-----------:|:-------:|
| 1 | `shared.ap-southeast-1.getblock.io/...` | ✅ | ✅ 148 rcpts | ✅ 2388 logs | ✅ archive | ✅ | 0x89 ✅ | ~3s | **✅ Best** |
| 2 | `polygon-mainnet.infura.io/v3/...` | ✅ | ✅ 148 rcpts | ✅ | ✅ archive | ✅ | 0x89 ✅ | ~7s | ✅ |
| 3 | `rpc.sentio.xyz/matic` | ✅ | ✅ 148 rcpts | ✅ 2388 logs (fastest) | ✅ archive | ✅ | 0x89 ✅ | ~2s | **✅ Best** |
| 4 | `matic.rpc.sentio.xyz` | ✅ | ✅ 148 rcpts | ✅ | ✅ archive | ✅ | 0x89 ✅ | ~2s | ✅ |
| 5 | `polygon.api.onfinality.io/public` | ✅ | ✅ | ✅ | ✅ archive | ✅ | 0x89 ✅ | ~3s | ✅ (slow) |
| 6 | `go.getblock.io/...` | ❌ 429 | ❌ | ❌ | ❌ | ❌ | ❌ | - | ❌ Rate-limited |
| 7 | `rpc.owlracle.info/poly/...` | ❌ 401 | ❌ | ❌ | ❌ | ❌ | ❌ | - | ❌ Unauthorized |
| 8 | `rpc.satelink.network/rpc/polygon` | ✅ | ✅ 148 rcpts | ❌ error | ✅ archive | ✅ | 0x89 ✅ | ~2s | ⚠️ Partial |
| 9 | `rpc.private.mev-x.com/polygon` | ✅ | ✅ (slow) | ❌ timeout | ❌ untested | ✅ | 0x89 ✅ | ~5s | ⚠️ Slow |
| 10 | `api.zan.top/polygon-mainnet` | ✅ | ✅ | ❌ timeout | ❌ untested | ✅ | 0x89 ✅ | ~4s | ⚠️ Partial |
| 11 | `polygon.lava.build` | ✅ | ✅ | ✅ 1083 logs | ✅ archive | ✅ | 0x89 ✅ | ~2s | **✅ Best** |
| 12 | `polygon-bor-rpc.publicnode.com` | ✅ | ✅ | ✅ | ✅ archive | ✅ | 0x89 ✅ | ~2s | ✅ (in codebase) |
| 13 | `poly.api.pocket.network` | ✅ | ✅ | ❌ timeout | ❌ untested | ✅ | 0x89 ✅ | ~4s | ⚠️ Partial |

### Observed RPS (burst of 10 concurrent requests)

| Endpoint | Burst RPS | Tier Limit |
|----------|:---------:|:-----------:|
| `polygon.lava.build` | ~0.9 | ~60 req/min public |
| `shared.ap-southeast-1.getblock.io` | ~0.6 | Depends on plan |
| `polygon-mainnet.infura.io` | ~0.7 | 100K req/day free |
| `rpc.sentio.xyz/matic` | ~0.6 | Public free tier |
| `matic.rpc.sentio.xyz` | ~0.8 | Public free tier |
| `rpc.satelink.network` | ~0.9 | Public free tier |
| `api.zan.top` | ~0.5 | Public free tier |
| `poly.api.pocket.network` | ~0.5 | Public free tier |

### Key Findings

1. **Best all-around endpoints**: `getblock.io` (#1), `rpc.sentio.xyz/matic` (#3), `polygon.lava.build` (#11) — all support full method set including archive (`eth_getProof`)
2. **Archive support**: All tested working endpoints support `eth_getProof` — great for replay/fact-check features
3. **Not usable**: `go.getblock.io` (429), `rpc.owlracle.info` (401)
4. **Already in codebase**: `polygon-bor-rpc.publicnode.com` already used as primary fallback — solid but not fastest
5. **Recommended additions**: `getblock.io` (best combination), `rpc.sentio.xyz/matic` (fastest), `polygon.lava.build` (consistent)

---

## Implementation Plan

**Design decisions (confirmed with user):**
- **RPC source**: Hardcoded + manual config (enrich `types.rs`, users can add in config.toml)
- **Distribution**: Shard block ranges (contiguous chunks per provider)
- **Per-Provider RPS**: Configurable per URL in config, with defaults

### Phase 1: `core/src/types.rs` — Enrich endpoint lists

Add a `ProviderEndpoint` struct and enrich `ChainName` with tested endpoints:

```rust
#[derive(Debug, Clone)]
pub struct ProviderEndpoint {
    pub url: &'static str,
    pub default_rps: f64,
    pub label: &'static str,
}
```

Add `public_rpc_endpoints(&self) -> &[ProviderEndpoint]` to `ChainName`. For Polygon, add the 9 working endpoints with their observed RPS. Keep `public_rpc_urls()` as a backward-compat wrapper.

### Phase 2: `core/src/config.rs` — Multi-URL configuration

- Add `rpc_urls: Vec<String>` to `Config` (default: empty)
- Add `rpc_rps: Vec<f64>` to `Config` (default: empty → auto-detect)
- Change `effective_rpc_urls()` to return all: `rpc_urls` + `rpc_url` (legacy) + public fallbacks
- Add `effective_provider_configs(chain_name) -> Vec<ProviderConfig>` merging user config with defaults

### Phase 3: `core/src/rpc.rs` — Provider-aware client

**New struct `ProviderState`:**
```rust
struct ProviderState {
    provider: RootProvider,
    rate_limiter: Option<Arc<RateLimiter>>,
    weight: f64,            // derived from RPS
    is_alive: bool,
    cooldown_until: Option<Instant>,
    consecutive_failures: u64,
    latency_ms: f64,
}
```

**Changes to `RpcClient`:**
- Replace `providers: Vec<RootProvider>` with `providers: Vec<ProviderState>`
- Remove single `rate_limiter`, each provider has its own
- Add `validate_all(endpoint_configs, expected_chain_id)` — per-endpoint health check
- `retry_call` picks provider by weighted random (weight = RPS if alive, 0 if dead/cooldown)
- Add `distribute_blocks(start, end) -> Vec<(usize, Vec<u64>)>` — splits range by provider weights

### Phase 4: `core/src/fetch.rs` — Distributed fetch

- `Fetcher` gains awareness of provider shards
- `fetch_range` calls `rpc.distribute_blocks()` then spawns one task per shard
- Each shard task runs independently with its own semaphore-based concurrency

### Phase 5: `cli/src/main.rs` — Wire multi-URL for all commands

For **all** commands (Run, Fetch, Replay, Discover), build the full multi-URL list: `user_rpc_urls + public_rpc_urls`. Pass per-provider RPS from config.

### Phase 6: `core/src/validation.rs` — Validate all endpoints

- Add `validate_rpc_endpoints()` — iterates all configured URLs, tests connectivity
- Extend `ValidationResult` with per-endpoint status

### Files affected

| File | Lines | Complexity |
|------|:-----:|:----------:|
| `core/src/types.rs` | +40 | Low |
| `core/src/config.rs` | +60 | Medium |
| `core/src/rpc.rs` | +250 | High |
| `core/src/fetch.rs` | +80 | Medium |
| `core/src/validation.rs` | +40 | Low |
| `cli/src/main.rs` | +30 | Low |
| **Total** | **~500** | |

### Backward compatibility

- `--rpc <URL>` flag works as before (single URL)
- `rpc_url` config field works as before
- New `--rpc-urls` (comma-separated) for multiple URLs
- New `--rpc-rps` (comma-separated) maps 1:1 for per-endpoint rate limits
- `RpcClient::new(url, chain_id)` still works (single provider)
- Default: when only `--rpc` given, used as sole provider (existing behavior preserved)
