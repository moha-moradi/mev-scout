# Roadmap to 100%: mev-scout (chain/strategy prioritization + capital-free MEV)

> Target: satisfy **all four sub-system goals at 100%** and the overall goal
> (data-driven chain + strategy prioritization).
> Scope boundary (inherited from `docs/implementation_plan_capital_free.md`):
> **the capital-free, non-CEX strategy set that the implementation plan actually
> sequenced** (20 strategies, §2) on the 7 wired chains
> (polygon, avalanche, bsc, arbitrum, base, ethereum, optimism). "100%" below is
> defined *relative to this scope* — the broader catalogued strategies are either
> capital-required (out of scope), permanently deprioritized, or unvalidated
> discovery targets that we do **not** build until measured demand justifies them.
> Priority scheme: **goals 2+3 (revm what-if simulation + paper realism) land first, on
> the already-shipped 6 strategies**, then report scanners and detector expansion by
> measured value (see §6).
> Companion docs: `docs/mev_strategies.md`, `docs/implementation_plan_capital_free.md`,
> `docs/mev_detection_and_opportunity_engine_spec.md`, `docs/ARCHITECTURE.md`.

---

## 1. North star — what "100%" means, measurably

### Overall goal — chain & strategy prioritization
**100% =** one command (`mev-scout prioritize`) that produces, from **our own
measured data** (not Dune/estimation), a ranked table of
(chain × strategy) opportunities with projected monthly net $, gas cost, competition
proxy, head-to-head chain comparison, and an explicit recommended implementation
order. Every capital-free strategy in scope appears once; excluded strategies appear
in a separate "excluded" section with the reason.

| Goal | 100% acceptance criteria (all must hold) |
|---|---|
| **G0 Overall** | `prioritize` command ships; output derived from first-party measurements (historical reports + paper P&L), documented confidence per cell, and reproduces within 1 run. Ranking updates automatically as new data lands. |
| **G1 Historical profitability/cost** | Every in-scope strategy has a runnable per-chain, per-window **report** (`scan --kind <…>`, data-backed): past-month N opportunities, volume, gross, gas, flash-fee, net USD, block/pool coverage %, pricing source, and mode (A settled / B latent / C sim). No **new** strategy ships a detector before its report exists, **unless** the report is structurally impossible (see table below). (Existing-6 strategies are exempt retroactively — their sim/paper work proceeds in M0–M2.) |
| **G2 revm simulation of latent opportunities** | A **what-if executor** can inject any hypothetical tx/bundle/state on top of a replayed block and re-run revm with realistic gas+order. Every mode-B strategy (balance drift, OSM next-price, Clip decay, interest accrual, multi-hop temporal depeg) has a detector that *searches unexecuted opportunities* and a backtest validating ≥1 real historical event with simulated profit ≈ paper profit. |
| **G3 Paper-money realism** | Paper fills are **re-executed through revm on the actual block state** (post-tx), with slippage, revert probability, inclusion/competition model (`winning_bid_premium` actually used), token-level wallet (not native-gas-only), and all in-scope strategies wallet-normalized (Liquidation included). `paper` P&L ≈ what an executed bundle would return, verified against real executed txs on a validation corpus. |
| **G4 Knowledge** | No stale docs; every implemented strategy has a **"measured data"** section (opportunity counts, revenue, gas, chain fit) replacing qualitative guess/Dune; a new engineer can ship a new detector end-to-end by following the detector template; design decisions are documented with why/why-not. |

**Progress metric (single number):** % of the in-scope strategy matrix
`(20 in-scope strategies × 7 chains)` that is **measured** (report or backtest) and
cycles through the pipeline: report → detector → backtest → paper → prioritize row.

---

## 2. In-scope strategy matrix (the 100% universe)

Exactly the capital-free set sequenced in `docs/implementation_plan_capital_free.md`
(per-strategy reportability table + Phases 1–6). A strategy is "done" at 100% when
all 5 pipeline stages exist.

| # | Strategy | Mode | Primary chain(s) | Stages needed (report → detector → sim → paper) |
|---|---|---|---|---|
| S1 | 4.4 flash-loan atomic liquidation | A | Polygon first | report → detector (extend `liquidation.rs`) |
| S2 | 4.13 interest-accrual liquidation | A/B→C | all lending chains | report → detector → scheduler → sim |
| S3 | 20 refinance / debt-mgmt arb | A→C | lending chains | report → detector |
| S4 | New-gen lending liq (Aave V4, Liquity V2, Sky, Euler V2, Morpho, Silo V2, Spark) | A | all lending chains | report (rides flash-liq fingerprint) → detector |
| S5 | 1.1 `skim()` capture | A + B | All V2 chains | report → detector → sim |
| S6 | 1.2 `sync()` race (follow-on value) | C | All V2 chains | detector → sim |
| S7 | 2.1 backrunning | C ($) | Polygon first (48Club/bloXroute) | detector → sim → order-flow |
| S8 | 7.11 V4 hook MEV | A→C | Base/ETH | detector → sim |
| S9 | 4.2 Maker OSM preview + `kick()` | A/B–C | ETH L1 | report → detector → scheduler → sim |
| S10 | 4.10 Maker Clip Dutch `take()` | A→C | ETH L1 | report → detector → scheduler → sim |
| S11 | 7.8 GMX V2 ADL front-run | A→C | Arbitrum | report (index) → detector |
| S12 | 4.11 GMX v1 keeper race | A→C | Arbitrum, Avalanche | report (index) → detector |
| S13 | 23 Fluid (Instadapp) vault liquidation | A→C | multichain | report → detector |
| S14 | 21 automation/keeper-network race | A→C | chain-generic | report → detector |
| S15 | 4.1 cascading liquidation | C | ETH L1 | detector → sim (+ dep graph) |
| S16 | 22 owner-side liq-protection salvage | A→C | ETH L1 | detector → sim |
| S17 | 2.2 long-tail multi-hop arb (negative-cycle ext) | C | BSC/Base | detector → sim (Bellman-Ford) |
| S18 | 5.3 rebase token arb (rides `balance_drift`) | B | ETH, Avalanche | detector (mode B) |
| S19 | 25 crvUSD LLAMMA soft-liq arb | A→C | ETH L1 | report → detector → band sim |
| S20 | 26 Compound V3 absorb + buyCollateral | A | ETH, Base, Arb, Polygon | report → detector (flash buy) |

> Mode-A "reports only" row captured inside class: S4. 
> **Deliberately-not-in-scope** (out of the implementation-plan set): Pendle PT/YT,
> Morpho Blue market-state, Lido oracle front-run, Curve imbalance, Convex gauge,
> Velodrome/Aerodrome epoch, Trader Joe LB, statistical/pairs arb (capital leg),
> FoT token arb — listed in `mev_strategies.md` but **not sequenced in the plan**;
> build only if measured demand (from modes A/B reports of adjacent strategies)
> justifies them.
> **Deferred-optional in plan:** ERC-4337 bundler (needs alt-mempool), multi-block MEV
> (needs validator relations) — explicit non-goals in this roadmap.
> **Excluded from the capital-free build but already in the engine:** Sandwich (needs
> ordering sim; classified only — see §8).

---

## 3. Current baseline (state of the art today)

| Capability | Status | Key files |
|---|---|---|
| Backtest runner (per-tx detector loop, gas distribution, calibration) | DONE | `core/src/pipeline/runner.rs` |
| revm `BlockReplayer` + `CachedRpcDb` (historical replay, lazy state) | DONE | `core/src/replay/{replayer,db}.rs` |
| Arb quoting/sim (analytical 2-hop, BFS N-hop ≤4, slippage bands, FOT filter, gas+flash fees) | DONE | `core/src/mev/detectors/{arb_common,two_hop,multi_hop}.rs`, `core/src/pool/*` |
| Detectors: TwoHop, MultiHop, Jit, JitArb, Sandwich, Liquidation(Aave V3) | DONE | `core/src/mev/detectors/*.rs` |
| Explorer ingest/classify/store, `explorer index|backfill|report|stats` (realized-MEV forensics, USD pricing CoinGecko/DefiLlama) | DONE | `core/src/explorer/*`, `cli/src/commands/explorer/*` |
| Paper ledger (`paper run|live|sim|stats`) | DONE (6 strategies; native-gas-only; no exec resim; Liquidation excluded) | `core/src/paper/*`, `core/src/jobs/paper.rs` |
| Mempool capture (pending block, arb-only detection) | PARTIAL | `core/src/mev/detectors/mempool.rs` |
| 7 chains wired (Aave V3 + Balancer + DEX factories) | DONE | `core/data/chains.toml`, `core/src/config/defaults.rs` |
| Strategy enum + GasConfig + FlashLoanProvider | DONE (6 variants) | `core/src/types/strategy.rs` |
| Docs/knowledge | STRONG | `ARCHITECTURE.md` §2/§4.7 synced (`explorer backfill|report|validate` documented; `explorer-{chain}.sqlite` naming); plan Phase-0 "no backfill" premise corrected |

## 4. Current gaps (what blocks 100%)

| # | Gap | Blocks |
|---|---|---|
| G1 | No `scan --kind skim\|flash-liq\|maker-keeper\|gmx-adl` per-strategy reports; only generic realized-MEV classification | G1-G0 |
| G2 | No `balance_drift.rs` (balance-vs-reserve accounting) | S5, S6, S18, skim report |
| G3 | No `scheduler.rs` (block-scheduled actions) | S2, S9, S10 |
| G4 | Strategy wiring: no `Skim/SyncRace/…` variants → no classifier `MevKind` → no paper accounting (plan §"three spots") | every new strategy (S1–S20) |
| G5 | No what-if revm executor (inject our bundles on top of replay) | G2, G3, S7, S15, S17 |
| G6 | Paper realism: credit pre-exec `expected_profit`, no slippage/inclusion, no token wallet, Liquidation not native | G3 |
| G7 | `aggregate.rs` summarizer ungated (`#![allow(dead_code)]`) — P&L summaries not wired to CLI | G3-G0 |
| G8 | No order-flow channels (MEV-Share/Fiber/bloXroute/48Club) — zero code | S7, S14 |
| G9 | Missing ChainConfig addresses: Maker OSM/Clip/join, GMX DataStore/Reader, Fluid vault, Liquity V2/Sky/Euler V2/Morpho, keeper registries | S4, S9–S14 |
| G10 | Cross-chain aggregation + `prioritize` command absent | G0 |
| G11 | No measured-data sections per implemented strategy (docs/knowledge gap) | G4 |

---

## 5. Workstreams (A–H) — the build backbone

Each workstream lists concrete deliverables, key files, and its own done-definition.
Order within the build sequence is in §6; workstreams are the *units* of work.

### WS-A — Strategy plumbing (unblocks EVERYTHING downstream)
**Objective:** the plan's own requirement — a new strategy plugs in at exactly
(1) `runner.rs` construction (~L430-440), (2) full-replay invocation (~L492-583),
(3) log-only `sync_block_from_logs`, plus classifier `MevKind`, plus paper accounting
(`ledger.rs::is_native_eligible` + token wallet), plus `strategies` config list.

**Deliverables:**
- Extend `Strategy` enum (`core/src/types/strategy.rs:71`) + `strum` names:
  `Skim, SyncRace, InterestLiq, RefinanceArb, NewGenLendingLiq, Backrun, V4HookMev,
  MakerOsmKick, MakerClipTake, GmxAdl, GmxKeeper, FluidLiq, AutomationKeeper,
  CascadingLiq, OwnerSalvage, RebaseArb`.
- Introduce a **detector trait** (if not yet extracted) shared by all detectors so
  new strategies implement detection + (optional) latent-reconstruction + (optional)
  sim-path uniformly. Give each a `.kind()` → `MevKind`.
- Extend explorer classifier/`store` so each variant persists and reports cleanly
  (migrate SQLite `mev_ops.kind` enum — new table or TEXT kind with lookup).
- Extend `is_native_eligible`→ token-wallet aware (see WS-F).
- Multi-chain: move new protocol addresses into `ChainConfig` (`config/defaults.rs`)
  + `chains.toml` + `core/src/types/chain.rs` default tables, gated per chain.
- **Done =** a "hello world" new strategy ships end-to-end (detector stub →
  classifier → paper row) with a passing `core/tests/sandwich.rs`-style detector test.

### WS-B — Structural primitives
**Deliverables:**
- `core/src/mev/detectors/balance_drift.rs`: event-driven token-balance ledger per
  pool (`Transfer` topic `ddf252ad…` + `Sync` + `Mint`/`Burn`), computing
  `balance − reserve`; live cross-check `RpcClient::call balanceOf` (`70a08231`
  selector already in `sigs/fallback_data.rs`). Document blind spot: share-price
  rebase tokens (stETH/wstETH/rETH) drift without `Transfer` → periodic `balanceOf`
  polling for known such tokens; 30-day scan budget via delta-chunked `getLogs`.
- `core/src/mev/scheduler.rs`: min-heap `(block, Action)` + `process_due(block)`;
  used by interest accrual, OSM poke, Clip take-block. Pure, unit-tested.
- **Done =** both compile, unit-tested; skim latent report (S5) uses balance_drift;
  a synthetic liquidation test uses scheduler.

### WS-C — Historical market scanners (G1; the "Dune-but-ours" layer)
**Objective:** per-strategy past-month **N opps / $ revenue** without a live bot.
Extend `explorer backfill`/ingest with classify-in-stream fingerprints and ranged
`--from/--to` (already supported); add `scan --kind <skim|flash-liq|maker-keeper|gmx-adl|refinance|fluid-liq|newgen-liq|v4-hook>`.

**Deliverables (fingerprints modeled on `docs/implementation_plan_capital_free.md` §reportability):**
- skim report: realized `skim` + outbound `Transfer` with payer = pair, excluding
  swap/mint/burn flow → realized $; latent `balance − reserve` via balance_drift (A+B).
- flash-liq report: flash borrow + liquidate + repay in same tx (protocol-generic →
  extends to Aave V4 / Liquity V2 / Sky / Euler V2 / Morpho / Fluid via `ChainConfig`).
- refinance report: repay-on-A + borrow-on-B same tx (DeFi Saver / Instadapp class).
- maker-keeper report: Maker `kick`/`Take` (+ Clip block/auction params) frequency +
  notional; calm-month zeros expected.
- gmx report: self-indexed `LiquidatePosition`/ADL logs (Dune empty).
- USD pricing reuse: `core/src/explorer/pricing.rs` (CoinGecko live, DefiLlama backfill).
- Synthetic log fixtures for every fingerprint (`core/tests/explorer_*` style).
- Wired into `explorer report` windows + a **`coverage`** metric per kind/window so
  users know how much of the window was scanned.
- **Done =** every "Report? = Yes" cell in the plan's reportability table has a
  shipped scanner + fixture test + at least one real-window run stored in explorer DB.

### WS-D — Detector expansion (S1–S20, plan Phases 1–6)
Built on WS-A/B; each detector consumes the plumbing. Full detector catalog is in
`docs/implementation_plan_capital_free.md`. Highlights by phase:
- **Phase 1:** `skim.rs` (runs before sync each block; V4 has no skim), `sync_race.rs`.
- **Phase 2:** `interest_liq.rs` (forward HF model; requires `variableBorrowRate` in
  `AaveReserveData` — extend `liquidation.rs:43-48`); flash-loan liquidation extension
  (`FlashLoanProvider` + Morpho/V4/Balancer 0% sources); `refinance_arb.rs`;
  Silo V2 / Spark arms; `compound_v3.rs` (absorb → buyCollateral).
- **Phase 3:** `backrun.rs` on mempool + revm post-state; **prerequisite: order-flow
  channel** (WS-G). Until licensed channel exists: mode-C paper-only.
- **Phase 4:** `v4_hook_mev.rs` using existing `hook_address` + `UniswapV4PoolState`;
  derive hook flags from address byte 17 (`0x08` = beforeSwap); `llamma_arb.rs`
  (crvUSD band + oracle EMA).
- **Phase 5:** `makerdao_osm.rs` (read OSM slot 4 via `get_storage_at`; mempool-watch
  `poke()` and co-bundle `kick()`s), `makerdao_clip.rs` (decay sim + join-adapter
  flash), `gmx_adl.rs` + `gmx_keeper.rs` (share position table + ref-price model),
  `fluid_liq.rs`, `automation_keeper.rs`.
- **Phase 6:** `cascading_liq.rs` (cross-protocol dep graph + flash routing),
  `salvage.rs`, `multi_hop.rs` → negative-cycle Bellman-Ford extension + wider token
  universe, `rebase_arb.rs` on balance_drift (S18).
- **Done =** detector matrix §2 rows with "detector" stage green + detector tests.

### WS-E — What-if revm simulation (G2; the "simulate the unnoticed" kernel)
**Objective:** go beyond replaying history — **evaluate hypothetical executions.**
- Expose a **bundle executor** on top of `BlockReplayer`/`CachedRpcDb`: load a
  replayed block's final `CacheDB`, apply a candidate tx/bundle (mint-and-expect
  addresses for our contracts + hooks), run revm, return post-state + logs + gas.
  Reuse `register_polygon_precompiles` and spec rules.
- Add **state override API** (account storage/balance set) for mode-B reconstruction
  (e.g., inject OSM next-price, drift-like fake balances for `balanceOf` checks).
- Add **execution simulation to paper fills** (see WS-F) and a **score module**:
  for each latent opportunity, `simulated_profit − gas − flash − impact − slippage`.
- **Staged done-definition:**
  - *Stage 1 (M0, existing 6 strategies)*: bundle executor + state override API +
    unit tests injecting a known profitable bundle on a replayed block and asserting
    profit; `verify_receipt` parity on replay keeps the state honest.
  - *Stage 2 (after WS-B/chain configs)*: ≥1 latent strategy (skim / OSM / Clip /
    multi-hop temporal) backtest hits a **real historical event**, simulated-profit ≈
    paper-profit within configurable tolerance; validation corpus shipped (WS-H2).

### WS-F — Paper realism (G3)
- **Exec re-simulation at fill time:** when ledger accepts a fill, re-execute the
  opportunity's route/bundle through revm on that block's post-state; use actual
  returned tokens + gas, not `expected_profit`. (WS-E provides the executor.)
- **Token-level wallet:** track balances per token + native; liquidations settle in
  collateral token then swap (or hold); net in native via pricing; replace
  `is_native_eligible` gating.
- **Competition/inclusion model:** enable & tune `winning_bid_premium`
  (`GasConfig`, currently hard-set to `0.0` in `jobs/run.rs:151`); per-strategy fill
  probability; mempool-only fills get inclusion-rating; MEV-Share rebates net-ledgered.
- **Wire `aggregate.rs`** (currently `#![allow(dead_code)]`) into `paper stats` and
  `report` so per-strategy/per-chain net P&L summaries exist.
- **Done =** paper `net ≈ verified executed bundle` on the validation corpus for all
  wallet-normalized strategies; Liquidation now settlable; a `paper` gap-analysis
  report lists remaining realism deltas and their $ impact estimate.

### WS-G — Chain/order-flow infrastructure
- **Order-flow channels (Phase 3 prerequisite):** integrate ≥1 licensed channel per
  target chain — MEV-Share (ETH), bloXroute BDN (Polygon), 48Club (BSC) — as an
  abstract `OrderFlowBackend` (subscribe pending, submit bundles, receive refunds).
  Until licensed: mode-C paper only, honestly labeled.
- **ChainConfig additions:** Maker OSM/Clip/join, GMX DataStore/Reader, Fluid vault,
  Liquity V2 / Sky / Euler V2 / Morpho addresses, keeper registries (Gelato/Keep3r/
  Chainlink/DFS), all in `chains.toml` + defaults per chain. Add per-chain RPC
  capability expectations (archive depth ⇒ FullReplay vs LogOnly, `detect_state_horizon`).
- **Done =** every in-scope strategy's protocol addresses resolvable via `ChainConfig`;
  order-flow backends exist for the 3 declared chains (even if behind feature flags).

### WS-H — Cross-chain prioritization tooling (G0 + G4)
- `mev-scout prioritize [--chains …]` : aggregates explorer `report` + paper P&L +
  detector coverage across chains into a ranked `(chain × strategy)` table with
  projected monthly net $, gas, coverage %, confidence, and a recommended order.
  Persist a snapshot (SQLite) to track trend.
- Validate ranking against `docs/mev_strategies.md` §9/§10/§17/§19 hypotheses
  (e.g., backrun highest-opportunity on Polygon, Avalanche lowest competition) —
  the doc becomes the cross-check, data is the source of truth.
- **Docs sync (G4):** `ARCHITECTURE.md` §2/§4.7 is updated — `explorer backfill|report|validate` are
  documented and the Phase-0 "no backfill" premise in the plan is corrected (note: a standalone
  `scan --kind …` report surface still does not exist — G1); remaining: add a **measured-data**
  section to each implemented strategy in `mev_strategies.md`; keep a **detector template** doc
  + checklist so a new strategy needs ≤1 known path.
- **Done (G0/G4)** = `prioritize` output table is derivable from the explorer+paper
  stores alone; no doc in `docs/` contradicts implemented code; every implemented
  strategy section shows measured numbers.

### WS-H2 — Validation corpus & test matrix (cross-cutting)
- Build a **golden events corpus**: known historical events per strategy
  (e.g., a known margin-call block, a Clip take, a rebase day, a V2 drift) hard-coded
  as `(chain, block_range, expected_outcome)` fixtures gated on `MEV_SCOUT_E2E=1` +
  `RPC_URL` (`core/tests/common/setup.rs`).
- Extend `core/tests/` with synthetic fixtures per fingerprint (WS-C), detector-driven
  tests (WS-D), what-if replay tests (WS-E), paper-realism reconciliation tests (WS-F),
  and a `prioritize`-output golden test (WS-H).
- **Done =** CI (or gated E2E) green on the full matrix; every strategy has ≥1
  synthetic + (where data exists) ≥1 on-chain validation test.
- **Partial (first slice landed).** Real-block corpora now exist and assert derived
  facts (`kind` present, `min_ops` floor, `searcher` identity, USD band — never exact
  amounts): `core/tests/explorer_corpus.rs` (explorer classification, 13 cases over
  Avalanche/Polygon/Ethereum), `core/tests/mev_corpus.rs` (detector, with a
  detector-vs-realized verdict pass-rate), `core/tests/paper_corpus.rs`. New cases are
  recorded, not hand-guessed — `MEV_SCOUT_RECORD=1` re-prints observed per-kind /
  per-searcher / USD facts without asserting (plus `_CHAIN` / `_FROM` / `_TO` to hunt a
  window outside the corpus). Offline synthetic tier stays in
  `core/tests/explorer_golden.rs` + `core/src/explorer/golden.rs`.
  **Remaining:** `jit` / `jit_arb` have no on-chain case in any seed window (recorded
  as 0 ops), and there is no corpus entry per strategy for the non-MEV ones listed
  above (rebase day, Clip take, V2 drift).

---

## 6. Build sequence & milestones

Priority scheme: **goal-leverage first** — land what-if simulation (G2) and paper
realism (G3) on the **already-shipped 6 strategies** before expanding the strategy
set. That gives realistic profit estimation the earliest, on data we already produce.
Then expansion runs report-first (G1): report scanners → detectors → per-strategy
sim/paper, each feeding the prioritization table (G0).

Dependency spine: **WS-E (what-if) → WS-F (paper realism) → [existing-6 validated]**,
then **WS-A (plumbing) → WS-B (primitives) → WS-C (scanners) → WS-D (detectors,
phased) → WS-G (order-flow) → WS-H (prioritize)**. WS-F is gated on WS-E; WS-D is
gated on WS-A/B; WS-C scanners for maker/GMX are gated on WS-A chain addresses.

| Milestone | Work | Unblocks / Moves | Est. |
|---|---|---|---|
| **M0** | WS-E stage 1: what-if bundle executor + state override API on existing 6 strategies | G2 kernel; unblocks WS-F | 1–1.5 w |
| **M1** | WS-F v1: paper exec re-simulation at fill, token-level wallet (Liquidation settleable), competition/inclusion model (`winning_bid_premium` un-hardcoded), `aggregate.rs` wired into `paper stats`/`report` | **G3 on existing 6** | 1.5–2 w |
| **M2** | WS-H2 first slice: reconciliation corpus (sim vs paper vs real executed txs on the 6) | G3/G2 validity proof | 3–5 d |
| **M3** | WS-A plumbing: strategy variants + detector trait + classifier/paper hooks + `ChainConfig` schema (Maker/GMX/Fluid/new-gen/Silo/Spark/Comet/LLAMMA) | every new strategy | 3–5 d |
| **M4** | WS-B primitives: `balance_drift.rs` + `scheduler.rs` (+ tests) | S2,S5,S6,S9,S10,S18 | 3–5 d |
| **M5** | WS-C scanners: skim, flash-liq (+new-gen/Silo/Spark), refinance, Compound V3 absorb — Polygon/ETH first, `scan` CLI + fixtures, 30-day run | G1: S1,S3,S4,S5,S20 measured | 4–6 d |
| **M6** | WS-C scanners: maker-keeper, gmx (self-indexed) | G1: S9–S12 measured | 3–5 d |
| **M7** | WS-D Ph1+2: skim, sync, interest_liq, flash-liq ext, refinance, Silo, Compound V3 detectors (report-first already satisfied) | S1–S6,S4,S20 detector stage | 1.5–2 w |
| **M8** | WS-D Ph3: backrun detector (+ order-flow channel contractual) | S7 | 3–5 d (detector) |
| **M9** | WS-D Ph4: v4_hook_mev + llamma_arb | S8, S19 | 1–1.5 w |
| **M10** | WS-D Ph5: maker/gmx/fluid/automation detectors | S9–S14 | 2 w |
| **M11** | WS-D Ph6: cascading/salvage/Bellman-Ford multi-hop/rebase + WS-E stage 2 latent backtests | S15–S18; G2 fully | 2–3 w |
| **M12** | WS-G order-flow integration (MEV-Share/BloXroute/48Club) + remaining ChainConfig | S7 live, S14 | 1–2 w |
| **M13** | WS-H `prioritize` + docs sync + validation corpus full-matrix | G0, G4, G1 fully measured | 1 w |

Total estimate: **~3.5–4.5 months** at dedicated effort. M0–M2 can run while M3/M4
are prepared; M5–M6 and M9–M11 parallelize once WS-A lands.

> Fastest meaningful win (goal-leverage): **M0 + M1** — what-if executor + paper
> re-execution deliver a *realistic* P&L on the 6 strategies the engine already runs,
> directly satisfying G3 and proving the G2 kernel, before any new strategy is built.
> First prioritization numbers arrive at **M5** (scan reports, Polygon-first).

---

## 7. KPIs — how we know we're at 100%

| Goal | KPI (target) |
|---|---|
| G0 | `prioritize` output fully derived from stores; no hand-edited ranking |
| G1 | 100% of §2 "report" cells green; coverage % ≥ 90 per window on target chains |
| G2 | What-if executor merges ≥1 latent strategy backtest matching paper P&L within ±20% on the validation corpus |
| G3 | Paper-fill re-sim executed; net-USD < threshold discrepancy vs executed bundles in corpus; Liquidation settleable |
| G4 | Zero stale doc statements (CI lint for known-stale anchors); every implemented strategy has measured-data section |

Suggested cadence: rerun `scan`/`backfill` monthly per chain; `paper live --loop`
day-runs; `prioritize` weekly; rebalance which chain/strategy gets implementation
attention based on the measured table rather than the doc.

---

## 8. Risks, assumptions, non-goals

**Assumptions**
- Archive/full node availability per chain determines FullReplay vs LogOnly coverage
  (`detect_state_horizon`); latent mode-B needs storage reads (OSM slots, reserve
  history) — covered by `CachedRpcDb` fallback where archive absent.
- Order-flow channels are licensable for the target chains; until then, Phase 3/5
  detector value is shown as mode-C paper only (honest labeling).
- RPC cost budget for 30-day `getLogs` scans on active pairs is within operational
  limits (delta-chunking; restrict to drift-bearing pairs).

**Major risks**
- Revm what-if on Polygon precompiles/state gaps producing wrong sim results → mitigate
  via `verify_receipt` parity + validation corpus reconciliation (WS-H2, WS-E done-def).
- Classifier fingerprint ambiguity (e.g., flash-liq vs plain arb) double-counting →
  explicit fingerprint rules per scanner (WS-C) + fixture tests.
- Scope creep into capital-required strategies (JIT/stat-arb capital leg, CEX–DEX) →
  hard excluded; only capital-free legs of otherwise-capital strategies count.

**Non-goals (explicit)**
- ERC-4337 bundler MEV, multi-block MEV (per plan, needs external infra/business).
- Governance MEV, NFT-floor arb, TWAP manipulation (permanently deprioritized §15).
- A UI/dashboard (CLI + SQLite remain the only surfaces).
- Capital-carrying portfolio logic in paper (paper accounts only gas+fees+slippage for
  capital-free strategies; JIT/stat-arb/CEX-DEX stays out).

---

## 9. Immediate next steps (next 2 weeks)

1. **M0 WS-E stage 1** — bundle executor + state override API on `BlockReplayer`/
   `CachedRpcDb`; unit test injecting a known profitable bundle on a replayed block;
   keep `verify_receipt` parity. Reuse `register_polygon_precompiles` + spec rules.
2. **M1 WS-F v1 (start in parallel)** — paper exec re-simulation at fill
   (consume WS-E), token-level wallet incl. Liquidation settlement,
   un-hardcode `winning_bid_premium` (`jobs/run.rs:151`), wire `aggregate.rs`
   into `paper stats`/`report`.
3. **M3 WS-A (stub prep only, second lane)** — draft `Strategy` enum additions +
   detector trait shape + `ChainConfig` schema fields (Maker/GMX/Fluid/new-gen);
   no wiring yet.
4. Keep `core/tests/e2e.rs`, `backtest.rs`, `sandwich.rs` patterns; every PR adds a
   test per §WS-H2 (M2 reconciliation fixtures first).