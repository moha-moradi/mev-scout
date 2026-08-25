# Pool Discovery Remediation Plan (A→B→C→D)

**Goal:** widen the scannable pool universe and make every discovered pool priceable,
without increasing RPC load (free-tier public RPCs stay as-is). Every phase is either
zero-RPC, HTTP-only, or a small one-time RPC spend that is permanently cached.

**Context / motivation:**
- Activity scan (`classify_dex_event`, `discovery/mod.rs`) only covers V2/V3/Curve/Balancer
  swaps — V4, TraderJoe LB and Pendle pools are invisible unless created inside the scanned window.
- **Confirmed bug:** `pipeline/scanner.rs:41` defines
  `V4_SWAP = keccak256("Swap(address,address,int256,int256,uint160,uint128,int24)")` — identical
  to V3's signature string, so it hashes to exactly the same topic as V3. Real V4 PoolManager emits
  `Swap(bytes32,address,int128,int128,uint160,uint128,int24,uint24)`. V4 activity detection is currently a no-op.
- Remote pools (GeckoTerminal) carry `fee: 0` / no tick_spacing → wrong quote math for remote-sourced CL pools.
- `infer_dex_type` misclassifies (QuickSwap→V2 fallback); `merge_from` never reconciles conflicting `dex_type`.
- Remote/hybrid pools are never persisted to SQLite (`put_discovered_pool` runs only in
  `discover_and_cache`), so they vanish between runs.

---

## Phase A — Zero-RPC-cost correctness fixes

### A1. Fix V4 swap topic + wire into discovery
- [ ] Correct `V4_SWAP` in `core/src/pipeline/scanner.rs:41`:
      `Swap(bytes32,address,int128,int128,uint160,uint128,int24,uint24)` — verify against v4-core before committing.
- [ ] Add unit test asserting `V4_SWAP != V3_SWAP`.
- [ ] Verify TraderJoe LB swap signature against the real LBPair ABI (current 5-param form looks suspicious).
- [ ] Add topics to discovery's `all_dex_topics` + `classify_dex_event` (`discovery/mod.rs:476,724`);
      `fast_topics` stays V2/V3-only.
- [ ] V4 hits keyed by poolId (`topics[1]`), matched against Initialize-event scan (`v4.rs`) /
      SQLite cache. **Exclude** `DexType::UniswapV4` from Phase-2 metadata fetch tasks
      (singleton PoolManager — per-pool `token0()` calls would burn RPC on guaranteed reverts).

### A2. Same treatment for TraderJoe LB + Pendle
- [ ] Reuse `TRADER_JOE_LB_SWAP`; TJ LB pairs are per-pair contracts so Phase-2 metadata works —
      needs `tokenX()`/`tokenY()` selectors instead of `token0()`/`token1()`.
- [ ] Verify Pendle `SwapPT`/`SwapYT` signatures against Pendle market ABI, then add.

### A3. dex_type reconciliation
- [ ] `merge_from` (`discovery/mod.rs:207`): never let a remote-derived type overwrite an
      on-chain-derived type; never let the `infer_dex_type` V2-fallback survive a conflict.
- [ ] Fix `geckoterminal.rs:288-298`: Algebra labels → `UniswapV3`; remove blind QuickSwap→V2 mapping.

---

## Phase B — Remote-source hardening (HTTP only, zero RPC)

### B1. Persist remote/hybrid pools
- [ ] In `cli/src/commands/discover.rs` after the merge phase (~line 361), loop merged pools through
      `put_discovered_pool` (`cache/store/pools.rs:6`). Skip zero-token entries.
- [ ] Effect: hybrid unions survive across runs; incremental mode and downstream scans see full universe.

### B2. GeckoTerminal per-DEX ladder rung *(chosen over DexScreener)*
- [ ] New `fetch_pools_for_dex(network, dex)` in `remote/geckoterminal.rs` using
      `networks/{n}/dexes/{dex}/pools`.
- [ ] Fallback when chain-wide query under-delivers; also classification source
      (per-DEX label → correct `DexType` by construction).

### B3. Filter/parsing hygiene
- [ ] `min_tvl` filter (`geckoterminal.rs:174-176`): keep pools with `tvl=None`
      (only drop when TVL known-and-below-min).
- [ ] `network_slug` (`geckoterminal.rs:22-33`): return error on unknown chains instead of
      silently defaulting to `polygon_pos`.
- [ ] CLI Phase-3 dedup (`discover.rs:337-342`): HashMap instead of O(n²) linear scan.

---

## Phase C — Make remote pools priceable (small one-time RPC, amortized) *(included, gated)*

### C1. Multicall fee/tickSpacing backfill
- [ ] New `core/src/rpc/multicall.rs` using Multicall3 (`0xcA11bde05977b3631167028862bE2a173976CA11`,
      same address on all 7 supported chains): one `eth_call` resolves
      `token0()/token1()/fee()/tickSpacing()` for ~50–100 pools.
- [ ] Runs **only** for remote-sourced concentrated-Liquidity pools missing fields;
      results cached forever ("N×4 calls" → "⌈N/100⌉×1 calls").

### C2. CLI gate
- [ ] New flag `--resolve-remote-metadata`; off by default so offline/remote-only workflows stay RPC-free.

---

## Phase D — Universe breadth (config-level)
- [ ] Extend `types/chain.rs` default factories:
      PancakeSwap V3 (Polygon, Arbitrum), BiSwap (BSC), Ramses (Arbitrum), Uniswap V2 (Base).
      Addresses verified during implementation; each is one line.
- [ ] RPC cost proportional only to new pools created within scanned windows.

---

## Verification
- [ ] Unit tests: topic-hash assertions; `classify_dex_event` with synthetic logs per DEX type;
      `merge_from` dex_type conflict rules; SQLite round-trip for persisted remote pools.
- [ ] Wiremock tests for new GT endpoint (pattern: `geckoterminal.rs:357`).
- [ ] `cargo test -p mev-scout-core -p mev-scout-cli`.
- [ ] Manual smoke: small `--blocks 500` discovery run; compare pool counts/type distribution pre/post.

## Decisions (2026-08-25)
- B2 direction: **GeckoTerminal per-DEX queries** (no DexScreener).
- Phase C: **included**, gated behind `--resolve-remote-metadata`.
- Order: **A → B → C → D**.
