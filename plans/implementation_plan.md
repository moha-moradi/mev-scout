# MEV Scout — Comprehensive Implementation Plan

**43 strategies · 5 phases · 3 chains (Polygon, BSC, Avalanche) · Zero capital (flash loans)**

---

## Overview

| Metric | Value |
|--------|-------|
| Total strategies | 43 (46 unimplemented − 3 excluded) |
| Excluded permanently | TWAP manipulation, NFT floor arb, governance MEV |
| Phases | 0–4 (following Section 11 of `mev_strategies_complete_v2.md`) |
| Target chains | Polygon (137), BSC (56), Avalanche (43114) |
| Capital model | Flash loans only (Balancer 0% fee priority, Aave 9bps fallback) |
| Execution infra | Executor contract + bundle submission + block scheduler |

---

## Phase 0 — Foundation (Stage 0 → 1)

Build before any strategy. Revenue-producing sub-phase first.

### Infrastructure: `core/src/execution/` (new module)

```
core/src/execution/
  mod.rs                  — Module declarations
  executor.rs             — Executor contract interface/ABI
  bundle.rs               — Flashbots/private relay bundle submission
  scheduler.rs            — Block scheduler for time-gated strategies
  contract.rs             — Solidity executor contract (separate forge project)
```

| Component | Description | Depends on |
|-----------|-------------|------------|
| `AlloyProvider` | WSS + HTTP alloy provider | Nothing (3P) |
| `revm` fork sim | Fork-at-block simulation | alloy |
| `redb` storage | Pool registry, position tracking | Nothing (3P) |
| Event subscription | `PairCreated`, `PoolCreated`, `Swap`, `Borrow`, `Transfer` | alloy WSS |
| Gas estimator | Live base fee + priority fee + PGA premium | alloy |
| Executor contract | Flash loan callback + `call-anything` interface | Solidity |

### First revenue strategies (build after infra basics work):

| # | Strategy | File | ~Lines | Capital | Tests | Build time |
|---|----------|------|--------|---------|-------|------------|
| 1 | **skim() capture** | `mev/detectors/skim.rs` | ~150 | None | Yes | 1-2 days |
| 2 | **Interest accrual liq** | `mev/detectors/interest_liq.rs` | ~200 | Low | Yes | 3-5 days |

### `skim.rs` — Detector outline

```rust
pub struct SkimDetector;
impl SkimDetector {
    pub fn detect(&self, pm: &PoolManager, gas_config: GasConfig)
        -> Vec<MevOpportunity>;
    fn check_skim(pool: &PoolState) -> Option<u128>; // balanceOf - reserve
}
```

- Iterates all V2 pools in `PoolManager`
- For each: call `balanceOf(pair)` vs `getReserves()` — excess = skim opportunity
- AMPL rebase tokens predictable (daily ~2 UTC) — pre-stage skim tx
- Returns `MevOpportunity` with `strategy: Strategy::Skim`

### `interest_liq.rs` — Detector outline

```rust
pub struct InterestAccrualDetector;
impl InterestAccrualDetector {
    pub fn detect(block_number, pm, gas_config) -> Vec<MevOpportunity>;
    fn check_borrower_health(token, amount, reserve_data) -> Option<HealthFactor>;
}
```

- Tracks borrowers whose health factor is falling due to interest accrual
- No real-time reactivity — pure block scheduling
- Schedule liquidation tx at the exact block where HF crosses ~1.0
- Low competition (other liquidators wait for HF < 1.0)

---

## Phase 1 — Capital-free Production (Stage 1 → 2)

After Phase 0 confirmed generating live revenue.

### Strategy additions:

| # | Strategy | File | ~Lines | Capital | Chain | Depends on |
|---|----------|------|--------|---------|-------|------------|
| 3 | **Flash loan liq** (Aave V3) | `mev/detectors/liquidation.rs` (extend) | +200 | None | All | Executor |
| 4 | **Backrunning** | `mev/detectors/backrun.rs` | ~300 | Low | All | Bundle relay |
| 5 | **MakerDAO OSM preview** | `mev/detectors/makerdao_osm.rs` | ~250 | None | ETH L1* | Block scheduler |
| 6 | **Synthetix flag+delayed** | `mev/detectors/synthetix_flag.rs` | ~200 | Medium | Optimism* | Block scheduler |
| 7 | **GMX v1/v2 keeper** | `mev/detectors/gmx_keeper.rs` | ~250 | None | Avalanche | Position indexer |

\* Note: MakerDAO OSM is ETH L1 only; Synthetix is Optimism. Build detectors — deploy on available chains where protocols exist.

### `backrun.rs` — Detector outline

```rust
pub struct BackrunDetector {
    mempool_txs: Vec<TxData>,
    executed_txs: Vec<(u64, TxData)>,
}
impl BackrunDetector {
    pub fn capture_tx(tx: &TxData);
    pub fn detect(block_number, pm, gas_config) -> Vec<MevOpportunity>;
    fn classify_target(tx: &TxData) -> Option<BackrunClass>;
    fn compute_backrun(target_pool, swap_details, pm) -> Option<MevOpportunity>;
}

enum BackrunClass {
    LargeSwap(PoolAddress, TokenIn, TokenOut, Amount),
    SyncCall(PoolAddress),
    Liquidation(PoolAddress, Debtor),
}
```

### Extended liquidation detector (`liquidation.rs`)
- Add flash loan routing (Balancer 0% fee first, Aave fallback)
- Add `LiquidationExecutor` integration
- Add multi-collateral support (borrower may have multiple assets)

---

## Phase 2 — Chain-specific Expansion (Stage 2)

Build by chain, not by strategy. One chain to production before starting next.

### Avalanche C-Chain (highest priority)

| # | Strategy | File | ~Lines | Capital | Notes |
|---|----------|------|--------|---------|-------|
| 8 | **Joe V2 LB arbitrage** | `mev/detectors/joe_v2_lb.rs` | ~300 | Low | Bin-level price detection |
| 9 | **GMX v2 keeper** (extend) | `mev/detectors/gmx_keeper.rs` | +100 | None | Extend Phase 1 keeper |
| 10 | **Pharaoh epoch** | `mev/detectors/pharaoh_epoch.rs` | ~200 | Medium | Same as Aerodrome |
| 11 | **Long-tail SPFA arb** | `mev/detectors/long_tail.rs` | ~400 | Low | Graph-based arb |
| — | Backrunning (Phase 1) | Already built | — | — | Deploy on Avalanche |

### BSC

| # | Strategy | File | ~Lines | Capital | Notes |
|---|----------|------|--------|---------|-------|
| 12 | **PancakeSwap token snipe** | `mev/detectors/token_launch.rs` | ~250 | Low | PairCreated events |
| 13 | **Venus flash loan liq** | `mev/detectors/liquidation.rs` (extend) | +150 | None | Venus protocol adapter |
| 14 | **Sandwich (via 48Club)** | `mev/detectors/sandwich.rs` (extend) | +100 | Medium | Only after 48Club access |
| — | Backrunning (Phase 1) | Already built | — | — | Deploy on BSC |

### Polygon

| # | Strategy | File | ~Lines | Capital | Notes |
|---|----------|------|--------|---------|-------|
| 15 | **Long-tail SPFA arb** | `mev/detectors/long_tail.rs` | Same as Avalanche | Low | QuickSwap + Uni V3 |
| 16 | **Oracle-latency liq** | `mev/detectors/oracle_latency_liq.rs` | ~200 | Medium | AAVE V3 on Polygon |
| — | Backrunning (Phase 1) | Already built | — | — | Deploy on Polygon |

### Cross-chain strategies (all chains, shared infra):

| # | Strategy | File | ~Lines | Capital | Notes |
|---|----------|------|--------|---------|-------|
| — | sync() race | `mev/detectors/sync_race.rs` | ~80 | None | Tricky |
| — | Init price snipe | `mev/detectors/init_price.rs` | ~150 | Low | Event-driven |
| — | FoT token arb | `mev/detectors/fot_arb.rs` | ~150 | Low | Uses FOT_TOKENS registry |
| — | Rebase token arb | `mev/detectors/rebase_arb.rs` | ~150 | Low | Uses REBASE_TOKENS registry |

---

## Phase 3 — Capital-intensive Strategies (Stage 2, capital accumulation)

After Phase 2 generates consistent revenue. Requires Medium+ capital for some strategies; flash-loan-first where possible.

| # | Strategy | File | ~Lines | Capital | Flash OK? |
|---|----------|------|--------|---------|-----------|
| 17 | **Stablecoin depeg arb** | `mev/detectors/stablecoin_depeg.rs` | ~200 | High | Partially |
| 18 | **Curve pool imbalance** | `mev/detectors/curve_imbalance.rs` | ~250 | Medium | Yes |
| 19 | **AAVE partial liq opt** | `mev/detectors/aave_partial_liq.rs` | ~200 | Medium | Yes |
| 20 | **Lido oracle front-run** | `mev/detectors/lido_oracle.rs` | ~200 | Medium | No — needs pre-position |
| 21 | **GMX V2 ADL front-run** | `mev/detectors/gmx_adl.rs` | ~250 | Low | Yes |
| 22 | **Pendle PT/YT** | `mev/detectors/pendle_pt_yt.rs` | ~300 | Medium | Partially |
| 23 | **Velodrome/Aerodrome epoch** | `mev/detectors/velodrome_epoch.rs` | ~200 | Medium | No — needs pre-position |
| 24 | **Balancer rate provider** | `mev/detectors/balancer_rate.rs` | ~200 | Medium | Yes |
| 25 | **V2+V3 JIT liquidity** | `mev/detectors/jit.rs` (extend) | +200 | High | No |
| 26 | **Statistical arb/pairs** | `mev/detectors/stat_arb.rs` | ~300 | Medium | Yes |
| 27 | **CEX-DEX arb** | `mev/detectors/cex_dex.rs` | ~250 | High | No |
| 28 | **MakerDAO Clip auction** | `mev/detectors/makerdao_clip.rs` | ~200 | None | Yes |
| 29 | **Liquity recovery mode** | `mev/detectors/liquity_recovery.rs` | ~200 | Low | Yes |
| 30 | **Liquity stability pool** | `mev/detectors/liquity_stability.rs` | ~200 | Medium | Partially |

---

## Phase 4 — Full-spectrum (Stage 3)

After Phases 1-3 generating reliable revenue.

| # | Strategy | File | ~Lines | Capital | Flash OK? |
|---|----------|------|--------|---------|-----------|
| 31 | **Cascading liq engineering** | `mev/detectors/cascading_liq.rs` | ~400 | High (flash OK) | Yes |
| 32 | **JIT + arb combo** | `mev/detectors/jit_arb.rs` (extend) | +200 | High | No |
| 33 | **Multi-block MEV** | `mev/detectors/multi_block.rs` | ~350 | None | Yes |
| 34 | **PBS/MEV-Boost** | `mev/detectors/pbs_mev_boost.rs` | ~300 | High | No |
| 35 | **ERC-4337 bundler MEV** | `mev/detectors/erc4337_bundler.rs` | ~250 | Low | Yes |
| 36 | **Batch auction/CoW** | `mev/detectors/batch_auction.rs` | ~200 | Medium | Yes |
| 37 | **Solver/intent MEV** | `mev/detectors/solver_intent.rs` | ~250 | Medium | Yes |
| 38 | **Bridge MEV** | `mev/detectors/bridge_mev.rs` | ~300 | High | No |
| 39 | **L2 sequencer MEV** | `mev/detectors/l2_sequencer.rs` | ~200 | Low | Yes |
| 40 | **Cross-chain arb** | `mev/detectors/cross_chain.rs` | ~350 | High | No |
| 41 | **Morpho Blue market state** | `mev/detectors/morpho_blue.rs` | ~200 | Medium | Yes |
| 42 | **V4 hook MEV** | `mev/detectors/v4_hook_mev.rs` | ~300 | Low | Yes |
| 43 | **Convex gauge vote epoch** | `mev/detectors/convex_gauge.rs` | ~200 | High | No |

---

## Module Structure (final)

```
core/src/mev/
  mod.rs                            — pub mod detectors; pub mod execution;
  detectors/
    mod.rs                          — +25 new sub-modules
    skim.rs                         — NEW: skim() capture
    sync_race.rs                    — NEW: sync() race
    init_price.rs                   — NEW: init price snipe
    backrun.rs                      — NEW: standalone backrunning
    long_tail.rs                    — NEW: SPFA/Bellman-Ford arb
    cex_dex.rs                      — NEW: CEX-DEX arb (detector only)
    stat_arb.rs                     — NEW: statistical pairs arb
    v3_range_snipe.rs               — NEW: V3 range order snipe
    interest_liq.rs                 — NEW: interest accrual liquidation
    oracle_latency_liq.rs           — NEW: oracle-latency liquidation
    lst_depeg_liq.rs                — NEW: LST depeg collateral liq
    aave_partial_liq.rs             — NEW: AAVE partial liq optimizer
    synthetix_flag.rs               — NEW: Synthetix flag+delayed
    liquity_recovery.rs             — NEW: Liquity recovery mode
    liquity_stability.rs            — NEW: Liquity stability pool
    makerdao_osm.rs                 — NEW: MakerDAO OSM preview
    makerdao_clip.rs                — NEW: MakerDAO Clip auction
    gmx_keeper.rs                   — NEW: GMX v1/v2 keeper
    perp_keeper.rs                  — NEW: perp protocol keeper
    bad_debt.rs                     — NEW: bad debt prevention
    nft_collateral_liq.rs           — NEW: NFT collateral liq
    rebase_arb.rs                   — NEW: rebase token arb
    fot_arb.rs                      — NEW: fee-on-transfer arb
    stablecoin_depeg.rs             — NEW: stablecoin depeg arb
    bridge_mev.rs                   — NEW: bridge MEV
    l2_sequencer.rs                 — NEW: L2 sequencer MEV
    cross_chain.rs                  — NEW: cross-chain arb
    curve_imbalance.rs              — NEW: Curve pool imbalance
    airdrop_mev.rs                  — NEW: airdrop MEV
    erc4337_bundler.rs              — NEW: AA bundler MEV
    velodrome_epoch.rs              — NEW: Velodrome/Aerodrome epoch
    pendle_pt_yt.rs                 — NEW: Pendle PT/YT spread
    balancer_rate.rs                — NEW: Balancer rate provider
    gmx_adl.rs                      — NEW: GMX V2 ADL front-run
    lido_oracle.rs                  — NEW: Lido oracle report
    morpho_blue.rs                  — NEW: Morpho Blue market state
    v4_hook_mev.rs                  — NEW: Uniswap V4 hook MEV
    convex_gauge.rs                 — NEW: Convex gauge vote epoch
    joe_v2_lb.rs                    — NEW: Trader Joe V2 LB
    solver_intent.rs                — NEW: solver/intent MEV
    batch_auction.rs                — NEW: batch auction MEV
    token_launch.rs                 — NEW: token launch snipe
    cascading_liq.rs                — NEW: cascading liquidation
    multi_block.rs                  — NEW: multi-block MEV
    pbs_mev_boost.rs                — NEW: PBS/MEV-Boost block building
    # Existing (unchanged):
    cross_block.rs
    jit.rs / jit_arb.rs
    liquidation.rs                  — EXTENDED: flash loan routing
    mempool.rs
    multi_hop.rs / two_hop.rs
    sandwich.rs
  execution/
    mod.rs
    live.rs                         — EXTENDED: strategy routing
    executor.rs                     — NEW: executor contract interface
    bundle.rs                       — NEW: bundle submission
    scheduler.rs                    — NEW: block scheduler
```

---

## Strategy Enum Changes (`types/strategy.rs`)

Add ~27 new variants (some Phase 4 strategies use existing variants):

```rust
pub enum Strategy {
    // Existing (7)
    TwoHopArb, MultiHopArb, Jit, JitArb, Sandwich, Liquidation, CrossBlockArb,
    // Phase 0 (2)
    Skim, InterestLiq,
    // Phase 1 (5)
    Backrun, MakerDaoOsm, SynthetixFlag, GmxKeeper, FlashLoanLiq,
    // Phase 2 (8)
    LongTailArb, InitPriceSnipe, SyncRace, RebaseArb, FotArb,
    JoeV2Lb, TokenLaunchSnipe, Sandwich48Club,
    // Phase 3 (13)
    StablecoinDepeg, CurveImbalance, AavePartialLiq, LidoOracle,
    GmxAdl, PendlePtYt, VelodromeEpoch, BalancerRateProvider,
    JitLiquidityExt, StatArbPairs, CexDex, MakerDaoClip,
    LiquityRecovery, LiquityStability,
    // Phase 4 (10)
    CascadingLiq, JitArbCombo, MultiBlockMev, PbsBoost,
    Erc4337Bundler, BatchAuction, SolverIntent, BridgeMev,
    L2Sequencer, CrossChainArb, MorphoBlue, V4HookMev, ConvexGauge,
}
```

---

## Infrastructure Build Priority

```
1. revm simulation             ← multiplier for safety
2. redb pool + position DB     ← foundation for all strategies
3. Flash loan executor         ← unlocks all capital-free strategies
4. Bundle submission layer     ← unlocks ordering (backrun, sandwich)
5. Block scheduler             ← unlocks time-gated (interest accrual, OSM)
6. AvalancheGo IPC             ← latency edge on Avalanche (highest ROI)
7. Beacon chain slot watcher   ← multi-block MEV (Phase 4)
```

---

## Testing Strategy

Per-phase test files following existing patterns:

| Test file | Coverage |
|-----------|----------|
| `tests/skim.rs` | skim() capture, sync() race |
| `tests/liquidation.rs` | Interest accrual, flash loan liq, cascading |
| `tests/backrun.rs` | Backrunning classification, profit estimation |
| `tests/oracle_arb.rs` | Rebase arb, FoT arb, depeg arb |
| `tests/chain_specific.rs` | Joe V2 LB, Pendle, Velodrome, Curve |
| `tests/execution.rs` | Bundle building, executor contract ABI |

---

## Chain Protocol Matrix

| Protocol | Polygon | BSC | Avalanche | Notes |
|----------|:-------:|:---:|:---------:|-------|
| Uniswap V2 | QuickSwap, SushiSwap | PancakeSwap, SushiSwap | Trader Joe V1 | Shared |
| Uniswap V3 | ✅ | ✅ | ✅ | Shared |
| Aave V3 | ✅ | ✅ | ✅ | All chains |
| Trader Joe V2 | ❌ | ❌ | ✅ | Avalanche native |
| GMX | ❌ | ❌ | ✅ | Avalanche native |
| PancakeSwap | ❌ | ✅ | ❌ | BSC native |
| QuickSwap | ✅ | ❌ | ❌ | Polygon native |
| Balancer V2 | ✅ | ✅ | ✅ | All chains |
| Curve | ✅ | ❌ | ❌ | Polygon only |

---

## Implementation Order (Recommended)

```
Phase 0a: Infra (alloy, revm, redb, events, gas estimator)
Phase 0b: Executor contract + skim() capture + interest accrual liq
Phase 0c: Bundle submission layer
Phase 0d: Block scheduler

--- Gate: real revenue from skim() + interest liq ---

Phase 1a: Flash loan liquidation (extend existing)
Phase 1b: Backrunning
Phase 1c: GMX keeper (Avalanche)
Phase 1d: Cross-chain sync race, init price, rebase, FoT

--- Gate: stable revenue on Avalanche ---

Phase 2a: Avalanche (Joe V2 LB, Pharaoh epoch, long-tail SPFA)
Phase 2b: Polygon (long-tail SPFA, oracle-latency liq)
Phase 2c: BSC (token snipe, Venus liq)
Phase 2d: Sandwich via 48Club (if access granted)

--- Gate: 2+ chains profitable ---

Phase 3: Capital-generating strategies (use accumulated profits)
Phase 4: Full-spectrum (cascading, multi-block, V4 hooks, etc.)
```

---

**Total estimated new code:** ~8,000-10,000 lines of Rust + ~500 lines Solidity + test suite

**Build time estimate:** ~3-4 months for a single developer working full-time through Phase 2c
