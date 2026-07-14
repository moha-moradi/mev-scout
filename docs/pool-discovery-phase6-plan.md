# Pool Discovery Module — Phase 6+ Improvement Plan

> **Objective:** Close remaining gaps across accuracy, coverage, and data
> infrastructure so the discovery module fully supports all 53 MEV strategies.
>
> **Prior work:** Phases 1–5 (RPC survival, code quality, correctness, CLI,
> features) are **complete**. This document covers the next layer.

---

## Status of the 5 Original Objectives

| # | Objective | Status | Gaps |
|---|-----------|--------|------|
| 1 | Operate smoothly on public RPCs | ✅ MET | — |
| 2 | Be fast | ✅ MET | — |
| 3 | Be accurate and comprehensive | ⚠️ PARTIAL | Missing V4, Trader Joe LB, Pendle. Solidly/Camelot flags discarded. |
| 4 | Data for run/replay modules | ⚠️ PARTIAL | Balancer `pool_id` silently None for event-discovered pools. |
| 5 | Robust data infrastructure for 53 strategies | ⚠️ PARTIAL | No token flags (rebase/FoT), no Balancer rate providers, no protocol monitors. |

---

## Phase 6 — Critical Correctness (P0)

> These are active bugs or silent data losses that affect existing functionality.

### 6.1 Fix Balancer `pool_id` gap for event-discovered pools

**Problem:** Balancer pools discovered via Swap events (not vault
`PoolRegistered` scan) have `pool_id: None`. `init_from_rpc` silently skips
them — pools are discovered but never initialized.

**Root cause:** `classify_dex_event` extracts `(pool_address, token0, token1)`
from Swap events but cannot derive the `pool_id` (which is a vault-level
bytes32 computed from `pool_address` + pool index). Only the vault scan at
`discovery.rs:437-482` populates `pool_id`.

**Fix:**
1. After Phase 1 event scan collects Balancer pool addresses, add a **pool_id
   resolution step** that calls `Vault.getPoolId(pool_address)` for each
   unique Balancer pool address found.
2. This is a single `eth_call` per pool — batch with `buffer_unordered`.
3. Add `BALANCER_VAULT_GET_POOL_ID_SELECTOR` constant (selector for
   `getPool(address)` which returns `(bytes32, address[])`).

**Files:** `core/src/pool/discovery.rs` (add resolution step after Phase 1),
`core/src/pool/state/factory.rs` (line 292 skip guard).

**Effort:** 30 min

---

### 6.2 Propagate Solidly `is_stable` flag

**Problem:** Solidly `PairCreated(address, address, bool is_stable, address pair)`
emits a `bool` distinguishing stable (StableSwap) from volatile (constant-product)
pools. The decoder at `discovery.rs:529-545` **discards this flag**. Stable pools
get treated as `x*y=k` — wrong quotes.

**Fix:**
1. Add `is_stable: Option<bool>` to `DiscoveredPool` and `PoolInfo`.
2. In the Solidly factory decode closure, extract the `bool` from `log.topics[3]`.
3. In `add_pool_to_manager`, when `DexType::Solidly` and `info.is_stable == Some(true)`,
   initialize with `CurvePoolState` (variant `Stable`) or a new
   `SolidlyStablePoolState` using the Solidly StableSwap invariant.
4. For the initial implementation, use a simple 2-token StableSwap with A=200
   (Solidly's default amplification).

**Files:** `core/src/pool/discovery.rs`, `core/src/pool/state/pool_types.rs`,
`core/src/pool/state/factory.rs`, `core/src/pool/state/manager.rs`,
`core/src/pipeline/runner.rs`, `core/src/cache/store.rs`.

**Effort:** 2 hours (includes StableSwap invariant in `pool/math/`)

---

### 6.3 Propagate Camelot `fee` and `is_stable` flags

**Problem:** Camelot `PairCreated(address pair, address token0, address token1,
uint256 fee, bool stable)` has per-pool custom fees and stable/volatile
distinction. Both are currently discarded.

**Fix:**
1. In the Camelot factory decode closure (`discovery.rs:549-570`), extract
   `fee` from `log.data[0..32]` (u256) and `stable` from `log.data[32..64]`.
2. Set `DiscoveredPool.fee` from the extracted value instead of hardcoded 0.
3. Set `DiscoveredPool.is_stable` from the `bool`.
4. Use same `is_stable` logic as Solidly (6.2) for initialization.

**Files:** `core/src/pool/discovery.rs`

**Effort:** 30 min

---

## Phase 7 — New DEX Coverage (P0)

> These add discovery for protocols referenced in high-value strategies.

### 7.1 Uniswap V4 pool discovery

**Strategy:** 7.11 Uniswap V4 hook MEV (profitability 7/10, competition 2/10).

**Architecture:**
- V4 uses a singleton `PoolManager` contract (not per-pool contracts).
- Pools are identified by `bytes32 poolKey` (token0, token1, fee, hooks).
- The `PoolManager` emits `Initialize(bytes32 indexed id, Address indexed currency0,
  Address indexed currency1, uint24 fee, int24 tickSpacing, Address hooks)`.
- Hook capabilities are encoded in the last 3 bytes of the hook address.

**Implementation:**
1. Add `DexType::UniswapV4` (discriminant 8).
2. Add topic constant: `V4_INITIALIZE_TOPIC = keccak256("Initialize(bytes32,Address,Address,uint24,int24,Address)")`.
3. Add `hook_address: Option<Address>` to `DiscoveredPool` and `PoolInfo`.
4. Decode V4 Initialize events from the PoolManager singleton address.
5. Extract: `pool_id` (the bytes32 key), `token0`, `token1`, `fee`, `tick_spacing`,
   `hook_address`.
6. Add `PoolState::UniswapV4(UniswapV4PoolState)` variant with fields:
   `sqrt_price_x96`, `tick`, `liquidity` (same as V3 but initialized from
   the singleton's `getPool()` view function).
7. In `init_from_rpc`: call `PoolManager.getPool(poolKey)` to fetch initial state.
8. In `update_from_logs` (`apply.rs`): decode V4 Swap events from the singleton
   (identified by matching poolKey in event data).

**Hook classification helper:**
```rust
pub fn classify_v4_hooks(hook_address: Address) -> V4HookFlags {
    let byte17 = hook_address.0[17];
    V4HookFlags {
        before_initialize: byte17 & 0x01 != 0,
        after_initialize:  byte17 & 0x02 != 0,
        before_swap:       byte17 & 0x04 != 0,
        after_swap:        byte17 & 0x08 != 0,
        before_add_liq:    byte17 & 0x10 != 0,
        after_add_liq:     byte17 & 0x20 != 0,
        before_remove_liq: byte17 & 0x40 != 0,
        after_remove_liq:  byte17 & 0x80 != 0,
    }
}
```

**Config:** Add `v4_pool_manager: Option<Address>` to `DiscoveryConfig`.

**Files:** `core/src/pool/dex_type.rs`, `core/src/pool/discovery.rs`,
`core/src/pool/state/pool_types.rs`, `core/src/pool/state/factory.rs`,
`core/src/pool/state/apply.rs`, `core/src/pool/state/manager.rs`,
`core/src/pipeline/runner.rs`, `core/src/cache/store.rs`,
`cli/src/cli.rs`, `cli/src/commands/discover.rs`.

**Effort:** 4–6 hours (includes V4 pool state, event decoding, and hook flags)

---

### 7.2 Trader Joe V2 Liquidity Book discovery

**Strategy:** 7.13 Trader Joe V2 LB (profitability 7/10, competition 2/10).

**Architecture:**
- Each LB pair is a separate contract (not singleton).
- LBFactory emits `LBPairCreated(address lbPair, address tokenX, address tokenY,
  uint256 activeId, address[] bins)` when a new pair is created.
- Each pair has a `binStep` (discrete price steps in basis points: 10, 25, 100).

**Implementation:**
1. Add `DexType::TraderJoeLB` (discriminant 9).
2. Add topic constant: `TRADER_JOE_LB_PAIR_CREATED_TOPIC = keccak256(
   "LBPairCreated(address,address,address,uint256,address[])")`.
3. Add `bin_step: Option<u32>` to `DiscoveredPool` and `PoolInfo`.
4. Decode LBPairCreated events from the LBFactory address.
5. Add `PoolState::TraderJoeLB(TraderJoeLBPoolState)` variant with fields:
   `active_id`, `bin_step`, `reserve_x`, `reserve_y`.
6. In `init_from_rpc`: call `LBPair.getActiveId()` and `LBPair.getBin(activeId)`.
7. In `update_from_logs`: decode `Swap(address,uint256,uint256,address,address)`
   events from LB pairs.

**Config:** Add `trader_joe_factory: Option<Address>` to `DiscoveryConfig`.

**Files:** Same as V4 plus `core/src/pool/math/` for LB math.

**Effort:** 4–6 hours (includes LB bin math)

---

### 7.3 Pendle Finance AMM discovery

**Strategy:** 7.6 Pendle PT/YT yield spread (profitability 6/10, competition 2/10).

**Architecture:**
- Pendle AMM (SY/PT/YT) creates markets with `expiry` timestamps.
- The AMM is a modified `logistic UAMM` — each market has a PT token, a SY token.
- Events: `NewMarket(address indexed market, address indexed PT, uint256 expiry)`
  from the Pendle factory.

**Implementation:**
1. Add `DexType::Pendle` (discriminant 10).
2. Add `maturity_timestamp: Option<u64>` to `DiscoveredPool` and `PoolInfo`.
3. Decode `NewMarket` events from the Pendle factory.
4. Add `PoolState::Pendle(PendlePoolState)` with fields: `pt_address`,
   `sy_address`, `expiry`, `total_pt`, `total_sy`.
5. In `init_from_rpc`: call `PendleMarket.readState()` for reserves.
6. In `update_from_logs`: decode Pendle swap events.

**Config:** Add `pendle_factory: Option<Address>` to `DiscoveryConfig`.

**Effort:** 3–4 hours

---

## Phase 8 — Data Enrichment (P1)

> Add metadata fields that downstream strategies need.

### 8.1 Add `is_fot` (fee-on-transfer) token detection

**Strategy:** 5.4 FoT token arbitrage (profitability 4/10, competition 3/10).
Also relevant for accurate V2 quoting (reserves diverge from implied price).

**Implementation:**
1. Add `is_fot: Option<bool>` to `PoolInfo`.
2. During health check or post-discovery, for V2 pools, simulate a tiny swap
   via `eth_call` and compare output — if `output < expected * 0.99`, flag as FoT.
3. Alternative (cheaper): check if the token contract has
   `transferFrom` returning a non-bool or if the balance changes don't match
   the transfer amount. Use `eth_call` on `balanceOf(pair)` before and after
   a simulated `transferFrom`.

**Simplest approach:** Maintain a hardcoded `FOT_TOKENS: LazyLock<HashSet<Address>>`
set of known FoT tokens (USDT on some chains, SafeMoon forks, etc.). This costs
zero RPC calls.

**Files:** `core/src/pool/state/pool_types.rs`, `core/src/pool/discovery.rs`

**Effort:** 1 hour

---

### 8.2 Add `is_rebase` token flag

**Strategy:** 5.3 Rebase token arbitrage (profitability 5/10, competition 3/10).

**Implementation:**
1. Add `is_rebase: Option<bool>` to `PoolInfo`.
2. Known rebase tokens: AMPL, stETH, reth, cbETH, wstETH (wrapped, not rebase).
3. Same `LazyLock<HashSet<Address>>` pattern as 8.1.
4. Used by V2 skim() detection (strategy 1.1): if pool holds a rebase token,
   `balanceOf(pair) > reserve` periodically.

**Files:** `core/src/pool/state/pool_types.rs`

**Effort:** 30 min

---

### 8.3 Add Balancer `rate_providers` field

**Strategy:** 7.7 Balancer rate provider staleness (profitability 6/10, competition 3/10).

**Implementation:**
1. Add `rate_providers: Vec<Option<Address>>` to `BalancerPoolState`.
2. During `init_from_rpc` → `fetch_balancer_state`, for each token in the pool,
   call `Vault.getPoolTokenInfo(poolId)` which returns rate provider addresses.
3. Store alongside token addresses. Downstream strategy reads
   `rate_providers[i]` and calls `getRate()` to compare against the pool's
   cached rate.

**Files:** `core/src/pool/state/pool_types.rs`, `core/src/pool/state/factory.rs`

**Effort:** 1 hour

---

### 8.4 Add `underlying_tokens` for multi-token pools

**Pools affected:** Curve (3+ tokens), Balancer (2–8 tokens), Pendle (PT+SY+yield).

**Implementation:**
1. Add `underlying_tokens: Option<Vec<Address>>` to `PoolInfo`.
2. For Curve: populate from `get_balances()` during init.
3. For Balancer: populate from `Vault.getPoolTokens(poolId)`.
4. `token0`/`token1` remain as the primary pair for display; `underlying_tokens`
   provides the full set for strategies that need it.

**Files:** `core/src/pool/state/pool_types.rs`, `core/src/pool/state/factory.rs`

**Effort:** 1 hour

---

### 8.5 Solidly per-protocol fee configuration

**Problem:** Velodrome/Aerodrome fees are dynamic (governed by veNFT voters).
The 30 bps default is approximate.

**Implementation:**
1. Add `solidly_fee_bps: Option<u32>` to `DiscoveryConfig`.
2. CLI flag `--solidly-fee-bps 30` (default 30).
3. During Solidly factory decode, use `config.solidly_fee_bps.unwrap_or(30)`.

**Files:** `core/src/pool/discovery.rs`, `cli/src/cli.rs`

**Effort:** 15 min

---

## Phase 9 — Protocol Monitor Infrastructure (P1-P2)

> Strategy categories 4 (Liquidations) and 7 (Protocol Niches) need on-chain
> state monitors beyond pool discovery. These are not pool discovery per se but
> are prerequisites for the strategies document.

### 9.1 Lending protocol position indexers

**Strategies:** 4.1–4.15 (all liquidation strategies).

**Required monitors:**
| Protocol | Events to index | State to track |
|----------|----------------|---------------|
| AAVE V3 | Borrow, Repay, LiquidationCall, Supply, Withdraw | Position health, collateral/debt |
| Compound V3 | AbsorbCollateral, BuyCollateral, Supply, Withdraw, Absorb | Market utilization, position health |
| Morpho Blue | Supply, Withdraw, Borrow, Repay, Liquidate | Utilization, health factor |
| Liquity | OpenTrove, CloseTrove, RedeemCollateral, Liquidate | TCR, individual ICR |

**Implementation approach:**
- New module: `core/src/monitor/` with `lending.rs`.
- Each protocol gets a `XxxPositionIndexer` struct that:
  - Scans historical events for open positions
  - Maintains a `HashMap<Address, Position>` in memory
  - Provides `health_factor(address, block)` query
- Pre-fetched at backtest start, updated per-block during replay.

**Files:** New `core/src/monitor/` module.

**Effort:** 8–12 hours (across all protocols)

---

### 9.2 Oracle price monitors

**Strategies:** 4.2 (MakerDAO OSM), 4.3 (oracle-latency), 7.9 (Lido oracle).

**Required monitors:**
| Protocol | Storage/Event | Timing |
|----------|--------------|--------|
| MakerDAO OSM | Storage slot 4 (`_next` price) | 1-hour delay |
| Chainlink | `LatestRoundData` | Heartbeat + deviation |
| Lido AccountingOracle | `ReportSubmitted` events | Daily |

**Implementation:**
- New `core/src/oracle/` module with `chainlink.rs`, `maker_osm.rs`, `lido.rs`.
- Each provides `get_price(asset, block) -> f64`.
- Maker OSM provides `get_next_price(asset) -> (f64, u64)` (price + effective block).

**Files:** New `core/src/oracle/` module.

**Effort:** 6–8 hours

---

### 9.3 LST/DeFi-specific monitors

**Strategies:** 7.5 (Velodrome epoch), 7.8 (GMX V2 ADL), 7.10 (Morpho),
7.12 (Convex/Curve gauge).

**Required monitors:**
| Protocol | Data needed |
|----------|------------|
| Velodrome/Aerodrome | Epoch timestamp, gauge weights, bribe claims |
| GMX V2 | `reservedUsd`/`poolAmount` ratio for ADL |
| Convex/Curve | Gauge weight votes, CRV emission rates |

**Implementation:** Lower priority — add after Phase 7 discovery is live.
Each is a standalone monitor struct in `core/src/monitor/`.

**Effort:** 4–6 hours each (deferred)

---

## Phase 10 — Curve Multi-Token Display (P2)

### 10.1 Propagate full token list for Curve/Balancer

**Problem:** `PoolInfo.token0/token1` only holds 2 tokens. Curve 3pool has
DAI/USDC/USDT but only the first 2 are stored. Runtime state is correct
(`balances: Vec<u128>`), but cached/display data is incomplete.

**Fix:**
1. Use `underlying_tokens` field (8.4) for the full list.
2. In `discovery.rs` CLI display, show all tokens for Curve/Balancer pools.

**Effort:** 30 min (after 8.4)

---

### 10.2 Propagate Balancer variant at discovery time

**Problem:** Balancer pools discovered as generic `Balancer`. Variant
(Weighted/Stable/ComposableStable) detected at init time — wastes RPC round-trip.

**Fix:**
1. In the Balancer vault scan, read `pool_type` byte from the
   `PoolRegistered` event data and map to `BalancerPoolVariant`.
2. Store in `DiscoveredPool` as an extension field or pass through to
   `PoolInfo`.

**Effort:** 30 min

---

## Implementation Order

| Step | Phase | Item | Effort | Depends on |
|------|-------|------|--------|------------|
| 1 | 6.1 | Balancer pool_id fix | 30 min | — |
| 2 | 6.2 | Solidly is_stable flag | 2 hr | — |
| 3 | 6.3 | Camelot fee + is_stable | 30 min | 6.2 (shared field) |
| 4 | 8.5 | Solidly per-protocol fee | 15 min | — |
| 5 | 8.1 | is_fot token flag | 1 hr | — |
| 6 | 8.2 | is_rebase token flag | 30 min | 8.1 (same pattern) |
| 7 | 8.3 | Balancer rate_providers | 1 hr | 6.1 |
| 8 | 8.4 | underlying_tokens | 1 hr | — |
| 9 | 7.1 | Uniswap V4 discovery | 5 hr | — |
| 10 | 7.2 | Trader Joe LB discovery | 5 hr | — |
| 11 | 7.3 | Pendle discovery | 3 hr | 8.4 |
| 12 | 10.1 | Curve multi-token display | 30 min | 8.4 |
| 13 | 10.2 | Balancer variant propagation | 30 min | — |
| 14 | 9.1 | Lending protocol indexers | 10 hr | — |
| 15 | 9.2 | Oracle price monitors | 7 hr | — |
| 16 | 9.3 | Protocol-specific monitors | 15 hr | 9.1, 9.2 |

**Total estimated effort:** ~54 hours

**Recommended sprints:**
- **Sprint 1 (Phase 6):** Balancer fix + Solidly/Camelot flags = ~3 hr
- **Sprint 2 (Phase 8):** Token flags + Balancer rate_providers = ~3 hr
- **Sprint 3 (Phase 7):** V4 + Trader Joe LB = ~10 hr
- **Sprint 4 (Phase 7+10):** Pendle + display fixes = ~4 hr
- **Sprint 5 (Phase 9):** Protocol monitors = ~32 hr

---

## New Files to Create

| File | Purpose | Phase |
|------|---------|-------|
| `core/src/pool/math/velodrome.rs` | Solidly StableSwap invariant | 6.2 |
| `core/src/pool/math/lb.rs` | Trader Joe LB bin math | 7.2 |
| `core/src/pool/math/pendle.rs` | Pendle logistic UAMM math | 7.3 |
| `core/src/monitor/mod.rs` | Protocol monitor module root | 9 |
| `core/src/monitor/lending.rs` | AAVE/Compound/Morpho/Liquity position indexers | 9.1 |
| `core/src/oracle/mod.rs` | Oracle module root | 9.2 |
| `core/src/oracle/chainlink.rs` | Chainlink price feed reader | 9.2 |
| `core/src/oracle/maker_osm.rs` | MakerDAO OSM preview reader | 9.2 |
| `core/src/oracle/lido.rs` | Lido oracle report monitor | 9.2 |

## Files to Modify

| File | Changes | Phase |
|------|---------|-------|
| `core/src/pool/dex_type.rs` | Add `UniswapV4`, `TraderJoeLB`, `Pendle` variants | 7 |
| `core/src/pool/discovery.rs` | Solidly/Camelot flag extraction, V4/LB/Pendle factory scans, Balancer pool_id resolution | 6, 7, 8 |
| `core/src/pool/state/pool_types.rs` | Add `is_stable`, `hook_address`, `bin_step`, `maturity_timestamp`, `underlying_tokens`, `is_fot`, `is_rebase` to PoolInfo. New PoolState variants. | 6, 7, 8 |
| `core/src/pool/state/factory.rs` | Solidly stable init, V4/LB/Pendle init, Balancer rate_providers | 6, 7, 8 |
| `core/src/pool/state/apply.rs` | V4/LB/Pendle event-to-state application | 7 |
| `core/src/pool/state/manager.rs` | V4/LB/Pendle pool state handling | 7 |
| `core/src/pipeline/runner.rs` | V4/LB/Pendle in `add_pool_to_manager` | 7 |
| `core/src/cache/store.rs` | New DexType discriminants, new PoolInfo fields | 7, 8 |
| `cli/src/cli.rs` | V4/LB/Pendle factory address flags, `--solidly-fee-bps` | 7, 8 |
| `cli/src/commands/discover.rs` | New factory scan integration | 7 |
| `core/tests/e2e.rs` | New DexType variants in match arms | 7 |
| `core/tests/common/setup.rs` | New DexType variants in match arms | 7 |
| `core/src/dune/pool_discovery.rs` | V4/LB/Pendle in Dune classification | 7 |

---

## Verification Checklist

After each sprint, run:
```bash
cargo check
cargo test
```

### Per-sprint success criteria:

**Sprint 1 (Phase 6):**
- [ ] All Balancer pools have `pool_id` populated (event + vault discovered)
- [ ] Solidly stable pools initialize with StableSwap invariant
- [ ] Camelot pools have correct fee from factory event
- [ ] `cargo test` passes (38+/39)

**Sprint 2 (Phase 8):**
- [ ] Known FoT tokens flagged in PoolInfo
- [ ] Known rebase tokens flagged in PoolInfo
- [ ] Balancer `rate_providers` populated during init
- [ ] `underlying_tokens` populated for Curve 3+ token pools

**Sprint 3 (Phase 7):**
- [ ] `DexType::UniswapV4` discovery from PoolManager events
- [ ] V4 hook flags decoded and stored
- [ ] `DexType::TraderJoeLB` discovery from LBFactory events
- [ ] `binStep` extracted and stored
- [ ] Both initialize with correct pool state
- [ ] CLI accepts `--v4-pool-manager` and `--trader-joe-factory`

**Sprint 4 (Phase 7+10):**
- [ ] `DexType::Pendle` discovery with maturity timestamp
- [ ] Curve multi-token display in CLI output
- [ ] Balancer variant propagated at discovery time

**Sprint 5 (Phase 9):**
- [ ] AAVE V3 position indexer produces health factors
- [ ] MakerDAO OSM preview reads next price from storage
- [ ] Lido oracle tracks committee submissions

---

## Strategy Coverage After Full Completion

| Strategy | Category | Infrastructure Needed | Phase |
|----------|----------|----------------------|-------|
| 1.1 skim() capture | V2 Mechanics | `is_rebase` flag, V2 pool state | 8.2 ✅ |
| 1.2 sync() race | V2 Mechanics | V2 pool state | Already ✅ |
| 1.3 Flash swap arb | V2 Mechanics | V2 pool state | Already ✅ |
| 1.4 Init price snipe | V2 Mechanics | Factory event monitoring | Already ✅ |
| 2.1–2.4 Order Flow | Order Flow | Pool graph registry | Already ✅ |
| 3.1–3.4 Bundle/Positional | Bundle | V3 tick data | Already ✅ |
| 4.1–4.15 Liquidations | Liquidations | Lending protocol monitors | 9.1 |
| 5.1 Stablecoin depeg | Oracle/Peg | Multi-DEX price tracking | Already ✅ |
| 5.2 TWAP manipulation | Oracle/Peg | V2/V3 pool state | Already ✅ |
| 5.3 Rebase token arb | Oracle/Peg | `is_rebase` flag | 8.2 |
| 5.4 FoT token arb | Oracle/Peg | `is_fot` flag | 8.1 |
| 6.1–6.4 Cross-Domain | Cross-Domain | Per-chain discovery | Already ✅ |
| 7.1 Curve imbalance | Protocol | Curve pool state | Already ✅ |
| 7.5 Velodrome epoch | Protocol | Solidly `is_stable` + epoch monitor | 6.2, 9.3 |
| 7.6 Pendle PT/YT | Protocol | Pendle discovery | 7.3 |
| 7.7 Balancer rate staleness | Protocol | Balancer `rate_providers` | 8.3 |
| 7.8 GMX V2 ADL | Protocol | GMX monitor | 9.3 |
| 7.9 Lido oracle | Protocol | Lido oracle monitor | 9.2 |
| 7.10 Morpho Blue | Protocol | Morpho indexer | 9.1 |
| 7.11 Uniswap V4 hook | Protocol | V4 discovery + hook flags | 7.1 |
| 7.12 Convex/Curve gauge | Protocol | Gauge monitor | 9.3 |
| 7.13 Trader Joe LB | Protocol | LB discovery + bin math | 7.2 |
| 8.1–8.5 Emerging | Emerging | Various | Deferred |
