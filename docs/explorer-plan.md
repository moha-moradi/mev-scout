# MEV Explorer — Implementation Plan

> **Status:** Planned (not yet implemented)
> **Goal:** Add a self-built, realized-MEV explorer to mev-scout — analogous to https://explorer.mev.zone (`mevlive` + historical sections) but chain-agnostic and fully owned by us. Target chains: **Avalanche first** (independent ground truth available via web tools), then **Polygon**, then **BSC + Ethereum** — all four share the same classifier, only configuration differs. Scope: **live + 30-day backfill**, **paid RPC** (Alchemy/drpc/GetBlock — already configured).
>
> **Why:** Cross-validate the *opportunity* pipeline (`run` / `live`) against *actually realized* MEV on-chain. This quantifies recall/precision of the hunting pipeline and provides independent market intelligence (who extracts, how much, where).

---

## Table of Contents

1. [Background: How MEV Explorers Actually Work](#1-background-how-mev-explorers-actually-work)
2. [Design Decision: Logs-First, Traces On-Demand](#2-design-decision-logs-first-traces-on-demand)
3. [Architecture Overview](#3-architecture-overview)
4. [New Core Module — `core/src/explorer/`](#4-new-core-module--coresrcexplorer)
5. [Storage — `SqliteStore` Extensions](#5-storage--sqlitestore-extensions)
6. [CLI Surface — `explore` Subcommands](#6-cli-surface--explore-subcommands)
7. [Cross-Validation Methodology (`explore verify`)](#7-cross-validation-methodology-explore-verify)
8. [RPC Additions](#8-rpc-additions)
9. [Phases & Deliverables](#9-phases--deliverables)
10. [Chain-Specific Notes & Backfill Economics](#10-chain-specific-notes--backfill-economics)
11. [Known Limitations](#11-known-limitations)
12. [Beyond the Four Target Chains (Future)](#12-beyond-the-four-target-chains-future)

---

## 1. Background: How MEV Explorers Actually Work

Two fundamentally different views of MEV exist, and this project sits at their intersection:

| | **Opportunity detection** (existing `run` / `live`) | **Realized MEV exploration** (this plan) |
|---|---|---|
| Question | "What *could* have been extracted in this block?" | "What *was* extracted, by whom, at whose expense?" |
| Method | Simulate/replay + quote optimal input | Classify historical txs into MEV patterns; compute realized profit |
| Evidence | revm replay (`BlockReplayer`), pool quotes | On-chain tx patterns (logs), per-address balance deltas |
| Reference impls | Flashbots-style search tooling | explorer.mev.zone (Avalanche), EigenPhi |

Avalanche's explorer and EigenPhi classify realized MEV primarily from **node traces** (`callTracer`, `prestateTracer`) — the reason they run their own nodes. Traces are not strictly required, however:

- **Sandwich attacks, JIT liquidity, and liquidations classify cleanly from logs alone.**
- **Atomic arbs classify from `Transfer` + `Swap` logs** via per-address net token deltas.
- **On-demand `debug_traceTransaction` (prestateTracer, diffMode)** gives exact per-tx profits for verification of individual transactions — cheap when done per-tx, expensive when done for a whole backfill.

### The four MEV types in scope (MVP)

| Type | Log-level signature | Notes |
|---|---|---|
| **Sandwich** | Same EOA/contract opens + closes a position around a victim swap, one block, same pool. Attacker's front-run swap and back-run swap are directionally opposite on the same pair; victim swap sits between them by block position. | Victim loss = value difference vs. counterfactual execution. V1 MVP: report attacker profit + victim swap size. |
| **Atomic arb** | Address participates in ≥ 2 swaps within one tx and exits with a positive net delta of a single "profit" token (typically WMATIC/WETH/stable). Profit = net delta × USD price − gas. | Heuristic on `Transfer` + `Swap` logs; see §11 limits. |
| **Liquidation** | `LiquidationCall` event on Aave V3 (pool address already in `core/data/chains.toml`); Compound-fork `LiquidateBorrow` equivalents. | Simplest classifier; near-zero false positives. |
| **JIT liquidity** | V3 `Mint` + `Burn` for the same position (same owner/tick range) within one block, bracketing swaps. | Log-only; no traces needed. |

---

## 2. Design Decision: Logs-First, Traces On-Demand

**Decision:** the primary classification engine is **logs-only**; traces are an optional per-transaction verification path, never a backfill requirement.

Rationale:

1. **Cost** — full-trace backfill of 30 days of Polygon (~1.3M blocks) via paid RPC is the single largest cost driver by 10–100x vs. `eth_getBlockReceipts` + `eth_getLogs`. Marginal accuracy gain at the aggregate level does not justify it.
2. **Architecture fit** — the entire existing stack is logs-based (`ActivityScanner`, decoders, `PoolManager`, revm replay for state reconstruction). Zero trace code exists today; every trace call would be net-new surface area.
3. **Precision where it matters** — individual txs worth auditing get exact numbers via on-demand prestateDiff (§8), closing the attribution gap case-by-case.

**Rejected alternatives:**
- *Full-trace backfill*: cost-prohibitive at Polygon block volume (see §10).
- *Local revm-based profit extraction for all txs*: the replayer exists (`core/src/replay/replayer.rs`) but replaying every DEX-touching tx on Polygon at backfill scale is far slower and RPC-heavy (state fetches through `CachedRpcDb`). Reserved as a possible later optimization for targeted re-analysis.
- *Third-party explorer APIs* (EigenPhi, mev.zone): explicitly out of scope — we build our own.

---

## 3. Architecture Overview

```
                    ┌────────────────────────────────────────────────┐
                    │                    CLI                         │
                    │   explore live / backfill / stats / tx / verify│
                    └───────────────┬────────────────────────────────┘
                                    │
        ┌───────────────────────────┼─────────────────────────────┐
        │                   core/src/explorer/                    │
        │  ┌──────────────┐  ┌──────────────┐  ┌───────────────┐  │
        │  │ classifier.rs│  │  profit.rs   │  │   labels.rs   │  │
        │  │ (4 detectors)│→ │ net deltas + │  │ searcher bot  │  │
        │  │              │  │ USD pricing  │  │ label DB      │  │
        │  └──────┬───────┘  └──────────────┘  └───────────────┘  │
        └─────────┼───────────────────────────────────────────────┘
                  │ reuses
   ┌──────────────┼──────────────────────────────────────────────┐
   │  Existing core (no structural changes required)             │
   │  RpcClient ─ get_block_and_receipts_batch / get_logs        │
   │  decoders ─ V2/V3/Curve/Balancer/LB/Pendle Swap,Sync,Mint,  │
   │             Burn + NEW raw ERC-20 Transfer + LiquidationCall│
   │  PoolManager ─ pool registry, token index                   │
   │  SqliteStore ─ mev_events, blocks_classified, labels        │
   └─────────────────────────────────────────────────────────────┘
```

Key principle: **the explorer consumes the same per-block data stream the scanner already produces** (block + receipts → decoded events), but applies *pattern detectors over realized txs* instead of *opportunity simulation*.

---

## 4. New Core Module — `core/src/explorer/`

A **separate namespace** from `core/src/mev/detectors/` (which are opportunity detectors). Naming keeps the conceptual split explicit: `mev::` = what could be made; `explorer::` = what was made.

### 4.1 `classifier.rs`

Per-block input: block header + ordered txs with receipts + decoded logs + `PoolManager` state for the block. Output: `Vec<MevEvent>`.

```rust
pub enum MevKind { Sandwich, AtomicArb, Liquidation, Jit }

pub struct MevEvent {
    pub block: u64,
    pub tx_index: u32,          // sandwich spans multiple txs → see MevBundle
    pub tx_hash: B256,
    pub kind: MevKind,
    pub searcher: Address,      // labeled EOA or arb contract
    pub victim: Option<Address>,        // sandwich only
    pub pool_ids: Vec<PoolId>,          // pools involved
    pub profit_token: Option<Address>,
    pub profit_amount: U256,            // net delta, pre-gas
    pub profit_usd: f64,
    pub gas_cost_native: f64,
    pub gas_cost_usd: f64,
    pub confidence: f32,                // 0..1 heuristic confidence
}

pub struct MevBundle {
    pub kind: MevKind,                  // Sandwich bundles front-run + victim + back-run
    pub txs: Vec<MevEvent>,
    pub attacker: Address,
}
```

Classifier pipeline per block (ordered):

1. **Liquidation pass** — exact event match (`LiquidationCall` topic on configured Aave/Compound addresses from `chains.toml`). Zero-heuristic, confidence 1.0.
2. **Swap attribution** — for every tx, collect swap events (already decoded by `core/src/pool/decoders.rs`) and ERC-20 Transfers (new decoder, §4.4). Build per-address net token deltas.
3. **Atomic arb pass** — addresses with ≥ 2 swaps in one tx and positive single-token net delta. Filter out pools' own fee accruals and WETH deposit/withdraw noise.
4. **Sandwich pass** — cross-tx, same-block pattern match on (attacker, pool, pair): front-run buy → victim → back-run sell. Position by tx index. Attach victim.
5. **JIT pass** — V3 `Mint`/`Burn` same position same block.

### 4.2 `profit.rs`

- Net ERC-20 delta per address from `Transfer` logs scoped to the tx (from/to bookkeeping; skip mints/burns of pool LP tokens).
- USD conversion via existing quote math (`core/src/pool/math/core.rs`) using the pool state at that block; stablecoin midpoint for pricing tokens without direct WMATIC pairs.
- Gas cost from receipt: `gasUsed × effectiveGasPrice` (receipts already fetched by `get_block_and_receipts_batch`).
- Net profit = gross delta USD − gas USD. Persist both.

### 4.3 `labels.rs`

- Searcher label DB (SQLite table, §5): address → label, first_seen block, source.
- Bootstrap: seed a static list of well-known Polygon MEV bots/contracts (hand-curated at implementation time).
- Growth loop: every confirmed classifier hit auto-inserts the address as `source = "classifier"`. Confidence-weighted repeat hits promote the label.

### 4.4 Decoder additions (small, in `core/src/pool/decoders.rs` or a sibling `explorer/decoders.rs`)

- Raw ERC-20 `Transfer` decoding (topic `0xddf252...`) — pool-scoped to keep volume manageable.
- Liquidation event topics per lending protocol family: Aave-style `LiquidationCall` (Aave V3 on Avalanche/Polygon/ETH) and Compound-fork `LiquidateBorrow` (Benqi, Trader Joe lending on Avalanche; Venus on BSC); Compound V3 (Comet) uses `AbsorbDebt` — so this is a per-protocol event registry, not one hardcoded topic.
- These complement — do not replace — the existing DEX swap decoders.

---

## 5. Storage — `SqliteStore` Extensions

Extend `core/src/cache/store/` (entry: `core/src/cache/store/mod.rs:48`) with new submodules:

```sql
-- One row per classified event (arb/liq/JIT) or bundle member (sandwich legs)
CREATE TABLE mev_events (
    id INTEGER PRIMARY KEY,
    block INTEGER NOT NULL,
    tx_index INTEGER,
    tx_hash TEXT NOT NULL,
    kind TEXT NOT NULL,             -- sandwich | atomic_arb | liquidation | jit
    searcher TEXT NOT NULL,
    victim TEXT,
    pool_ids TEXT,                  -- JSON array
    profit_token TEXT,
    profit_amount TEXT,             -- U256 as decimal string
    profit_usd REAL,
    gas_cost_usd REAL,
    confidence REAL,
    run_id TEXT                     -- backfill batch id (reuse RunManifest convention)
);
CREATE INDEX idx_mev_events_block ON mev_events(block);
CREATE INDEX idx_mev_events_kind ON mev_events(kind);
CREATE INDEX idx_mev_events_searcher ON mev_events(searcher);

-- Backfill/live checkpointing (gap-safe resume, mirrors existing integrity handling)
CREATE TABLE blocks_classified (
    block INTEGER PRIMARY KEY,
    classified_at INTEGER NOT NULL,
    event_count INTEGER NOT NULL
);

CREATE TABLE searcher_labels (
    address TEXT PRIMARY KEY,
    label TEXT NOT NULL,
    source TEXT NOT NULL,           -- static | classifier
    first_seen_block INTEGER
);
```

**Storage discipline (important):** the backfill classifies in-stream and persists **only** `mev_events` + `blocks_classified` — *not* raw receipts. Raw-receipt caching of 1.3M Polygon blocks would bloat SQLite to tens of GB. Live mode may keep its existing raw block cache behavior (the scanner needs it); a retention policy prunes raw cache older than N days while `mev_events` is kept forever.

---

## 6. CLI Surface — `explore` Subcommands

New `ExploreArgs` (clap) wired into `cli/src/cli.rs` and the `CliCommand` trait / dispatch (`cli/src/commands/mod.rs:31`, `:81`). Setup sequence mirrors the existing `live` command (`cli/src/commands/live.rs:46-134`): config validation → `init_rpc` (`cli/src/rpc_setup.rs:10`) → `SqliteStore::open` → pool list → `GasConfig` → `PoolManager` → `init_pools`. **No revm `BlockReplayer` needed** (logs-only), which also makes the explorer much cheaper than `live` per block.

```
mev-scout explore live
    Poll eth_blockNumber (HTTP polling, same pattern as existing live.rs:250),
    fetch block + receipts (get_block_and_receipts_batch, client.rs:976),
    classify, render a mevlive-style table (comfy-table, reuse display.rs render helpers),
    persist to mev_events. Flags: --loop / --duration / --poll-interval-ms (same semantics as live).

mev-scout explore backfill --days 30 [--from-block N] [--to-block N]
    Resumable parallel classification over historical ranges.
    Checkpoints per block in blocks_classified; gap auto-resume mirrors
    auto_refetch_gaps (core/src/fetch/fetcher.rs) + integrity.rs patterns.
    Classify-in-stream; no raw receipt retention.

mev-scout explore stats [--window day|week|month] [--kind ...]
    Aggregated sections from mev_events:
      daily MEV by type, top searchers (7d), top pools/pairs, victim losses,
      USD totals. Pure SQL over the table — instant.

mev-scout explore tx <hash> [--trace]
    Detail view: decoded swaps, net deltas, classifier verdict, USD profit.
    --trace: on-demand debug_traceTransaction prestateDiff → exact profit
    recompute → stores verified=true + corrected numbers on the event.

mev-scout explore verify --run-id <id>
    Cross-validation vs opportunity pipeline (§7).
```

Output formats: terminal table (primary, like `run`/`live`) + JSON export to `results/` reusing the `ResultsFile`/`save_results_json` conventions (`core/src/types/opportunity.rs:218`, `cli/src/display.rs:35`) so downstream tooling treats both pipelines uniformly. `run_id` convention: `explore_{epoch}` / `explore_backfill_{epoch}`.

---

## 7. Cross-Validation Methodology (`explore verify`)

Loads `results/{run_id}.json` (opportunities) and `mev_events` (realized) from the DB; joins on **block number** (and pool overlap where possible).

Report dimensions:

- **Recall** — of blocks where the explorer saw realized MEV of type X, in how many did the scanner flag an opportunity of type X? *"Explorer saw realized arb in 412 blocks; scanner flagged 380 → 92% recall."*
- **Precision signal** — blocks where the scanner flagged opportunities but the explorer found no realized MEV: candidate false positives **or** real opportunities that were unprofitable-in-practice / outcompeted. Tagged separately, not silently merged.
- **Magnitude comparison** — scanner's estimated profit vs. explorer's realized profit per matched event → calibration curve for the estimator (gas model, price model accuracy).
- **Per-type breakdown** — sandwich/arbs/liq/JIT reported independently; e.g., liquidations should be near-perfect matches, arb estimates the interesting calibration signal.

This is the primary deliverable of the whole project: a continuously measurable feedback loop for the hunting pipeline.

### 7.1 External ground truth (Avalanche) — why we build on Avalanche first

The classifier is chain-agnostic, so it is **validated first on the chain where an independent web tool can act as ground truth**: explorer.mev.zone (Avalanche-specific realized-MEV explorer) plus Snowtrace for tx-level inspection. No equivalent web tool covers Polygon/BSC realized MEV, so correctness must be established here before re-targeting.

Method:

1. Sample several non-contiguous windows (e.g., 6 × 1 hour spread across different days/volatility regimes).
2. Run `explore` over those windows on C-Chain; export events.
3. Compare against mev.zone's recorded history for the same windows:
   - **Event counts per type** — arb within ±20%; sandwich and liquidation essentially exact (unambiguous on-chain signatures)
   - **Tx-level overlap** — sandwiches and liquidations should match ≥ 90% by tx hash
   - **Searcher address overlap** and searcher-level USD profit totals reconciled within ~±15% (mev.zone may use trace-based attribution, so modest per-event profit deltas are expected and informative, not disqualifying)
4. Discrepancies triaged via Snowtrace (inspect the actual txs; where needed `explore tx --trace`).

Once this agreement is demonstrated, the same binary is trusted on Polygon/BSC/ETH — the only per-chain changes are configuration (§10).

---

## 8. RPC Additions

Net-new to `core/src/rpc/client.rs` (the codebase has **zero** trace support today):

- `debug_trace_transaction(hash, {tracer: "prestateTracer", tracerConfig: {diffMode: true}})` — used **only** by `explore tx --trace`. Single-tx, on-demand, negligible cost. Verify provider support flags in config (Alchemy on Polygon supports it; drpc/GetBlock should be capability-probed like the existing archive detection at client.rs:1361).
- No `debug_traceBlock*` anywhere in this plan.

---

## 9. Phases & Deliverables

Build order principle: **validate on the chain with ground truth (Avalanche) → scale to the primary hunting chain (Polygon) → breadth (BSC, Ethereum)**. All classifier code from Phase 1 onward is chain-agnostic; re-targeting a chain is configuration work (§10), not algorithm work.

### Phase 1 — Live explorer on Avalanche (classifier + external validation)
- `core/src/explorer/`: classifier (all 4 types), profit, minimal labels
- Decoder additions: raw Transfer + per-protocol liquidation events (§4.4)
- `explore live` command (one-shot + loop), terminal table + JSON export
- SQLite tables `mev_events`, `searcher_labels`
- **Accept:** mevlive-style table renders classified events for new C-Chain blocks with USD profits; §7.1 agreement thresholds met against explorer.mev.zone; `run live` unchanged and unaffected.

### Phase 2 — Polygon: live + 30-day backfill + stats
- Re-target via the already-complete Polygon config in `chains.toml` — no classifier changes
- `explore backfill --days 30`: resumable, classify-in-stream, `blocks_classified` checkpoints
- `explore stats` day/week/month sections
- Raw-cache retention policy
- **Accept:** 30-day Polygon history classified; stats sections queryable instantly; process resumes cleanly after interruption.

### Phase 3 — Cross-validation + tx detail
- `explore verify --run-id` precision/recall/calibration report (§7) vs the opportunity pipeline
- `explore tx <hash> [--trace]` + `debug_trace_transaction` in `RpcClient`
- Label DB growth loop
- **Accept:** verify produces the §7 report for any existing run; a spot-checked sample (≥ 20 events) of `--trace`-verified profits matches prestateDiff-derived deltas.

### Phase 4 — BSC + Ethereum
- Config completion: BSC (PancakeSwap V2/V3 + forks factory registry, Venus liquidations, WBNB pricing, reorg-depth config), Ethereum (Uniswap V2/V3/V4 + Curve/Balancer factories, Aave V3 + Compound V3 liquidations, WETH pricing)
- Receipt-endpoint capability probing per provider per chain (matters most on BSC); batch/concurrency tuning for BSC's large blocks
- Optional 30-day backfills per chain
- **Accept:** all four chains served by the same binary with per-chain config only; per-chain stats sections available.

### Documentation deliverable (with Phase 1)
- `docs/explorer-design.md` — the classification methodology written out (pattern definitions, heuristics, confidence scoring, known blind spots, per-chain adaptation notes per §10). The point: understanding *how* these systems work is an explicit goal; writing the rules down enforces that.

---

## 10. Chain-Specific Notes & Backfill Economics

### 10.1 What transfers across chains — and what doesn't

- **Transfers:** all four target chains share standard EVM execution semantics — EIP-1559-style fee fields, ERC-20 `Transfer` logs, Uniswap-style `Swap`/`Mint`/`Burn` events, per-tx receipts. **The classification algorithm is identical on every chain**; only configuration varies (factories, lending markets, native wrapper, stablecoin set, reorg depth).
- **Does not transfer:** mempool/pending-MEV views (out of scope regardless), cross-domain MEV (§11.3), and the per-chain quirks below.

### 10.2 Avalanche C-Chain — start here
- ~2s blocks, dynamic fees; DEX set already understood by the codebase (LB/Liquidity-Book decoders cover Trader Joe, plus Uniswap-style forks; `avalanche` is already fully defined in `chains.toml`).
- **Quirk:** C-Chain blocks can contain *atomic transactions* (C-Chain ↔ X/P-Chain imports/exports) that don't follow standard EVM receipt/log semantics — the classifier must skip non-EVM tx types gracefully rather than assume every tx has DEX-shaped logs.
- WAVAX as native wrapper; USDC/USDT stable set for USD pricing.
- **Ground truth:** explorer.mev.zone (live + historical sections) and Snowtrace — the entire reason Phase 1 targets this chain.

### 10.3 Polygon — primary hunting chain
- **Block volume:** 2s blocks → ~43,200 blocks/day → **~1.3M blocks / 30 days**. Nearly every block has DEX activity, so log-first filtering (`fetch_relevant`) saves little here — plan for effectively full block+receipt fetching.
- **No Flashbots:** MEV flows through the open mempool + private RPCs (bloXroute, Merkle) and **Fastlane** (validator MEV redistribution). Irrelevant for historical classification; relevant only if "pending MEV" features are ever wanted.
- WMATIC wrapper; reorg depth small (Bor finality is fast).

### 10.4 BSC
- Block cadence has shrunk repeatedly (3s → 1.5s Lorentz → 0.75s Maxwell, 2025) with very large gas limits (~100M+) and dense DEX activity → the **heaviest per-block receipt payloads** of the four; batch/concurrency tuning and per-provider limits matter most here.
- PancakeSwap V2/V3 (+ forks) dominate; heavily sandwiched chain.
- Historically deeper reorgs than Polygon/ETH → per-chain safe-depth config for live re-classification.
- Trace support on paid RPCs is the spottiest of the four chains → the logs-first design (§2) pays off most here. If a provider lacks `eth_getBlockReceipts`, fall back to range `eth_getLogs` (existing `probe_get_logs_limit` machinery handles limit discovery).

### 10.5 Ethereum L1
- 12s blocks; richest MEV and most diverse pool set (Uniswap V2/V3/V4, Curve, Balancer, …).
- **Private orderflow (Flashbots/builder bundles) does not hurt realized-MEV classification:** every landed bundle is fully visible in receipts/logs after inclusion. The blindness applies only to pre-inclusion views (out of scope).
- WETH wrapper; deepest token liquidity for USD conversion.

### 10.6 Backfill economics (30 days, all four chains)

| Chain | Block time | ~Blocks / 30d | Receipt payload weight | Est. backfill wall-clock @ ~10 req/s aggregate |
|---|---|---|---|---|
| Avalanche C-Chain | ~2s | ~1.3M | moderate | ~1.5–2 days |
| Polygon PoS | 2s | ~1.3M | moderate | ~1.5–2 days |
| BSC | 0.75–3s (hardfork-dependent) | ~0.9M–3.5M | **heavy** | ~3–5+ days |
| Ethereum L1 | 12s | ~216k | light | < 1 day |

- Cost driver ranking on every chain: `eth_getBlockReceipts` (bulk) ≫ single-tx prestateDiff (on-demand only) ≫ everything else. Traces never touch the backfill path.
- Provider rotation/cooldown machinery in `RpcClient` (`core/src/rpc/middleware.rs:88`) already handles per-provider rate limits; figures assume a conservative aggregate across the configured providers and improve with purchased RPS.
- Reorg handling: live mode re-classifies the last few blocks on reorg, with **per-chain depth config** (small on Polygon/Avalanche/ETH; larger on BSC).

---

## 11. Known Limitations (accepted, documented, revisit-able)

1. **Multi-hop arb attribution is heuristic.** Chained arbs across contracts can split/mask net deltas; logs-only sees token flows, not intent. Confidence scores + `--trace` verification cover the audit path.
2. **WETH/WMATIC wrap noise.** Deposit/withdraw pairs can masquerade as deltas — filtered explicitly in `profit.rs`, but edge cases will exist.
3. **Non-atomic / CEX–DEX MEV invisible.** Cross-domain extraction is out of scope by design (logs-only, per-block).
4. **Victim-loss figures are approximations** in V1 (counterfactual execution requires simulation; deferred — report attacker profit + victim swap size first).
5. **Pre-inclusion (pending) MEV is blind on private-orderflow chains.** Realized classification is unaffected — landed bundles are fully visible in receipts — but any future mempool/pending features would undercount on Ethereum/BSC relative to Polygon/Avalanche.
6. **Labeled-bot dependence for pretty output.** Unlabeled searchers still classify fine (addresses are primary keys); labels are UX sugar, not load-bearing for the math.

---

## 12. Beyond the Four Target Chains (Future)

After BSC + Ethereum (Phase 4), further chains are **configuration-only additions**. `core/data/chains.toml` already defines `arbitrum`, `base`, and `optimism` alongside the four targets, and the classifier needs no code changes for them (log patterns + generic ERC-20 deltas):

- DEX factory registry per chain (factories, fee tiers, Algebra forks, Balancer vault) — partially present
- Lending market addresses per chain (Aave V3, Compound forks/V3, L2 lending markets) — needs completion
- Pricing chain-of-trust (native wrapper per chain) parameterized in `profit.rs`
- L2 nuances (Arbitrum / OP-stack block cadence, sequencer behavior) affect throughput economics, not classification correctness
- Chain-specific precompile/state quirks only matter if revm paths are ever added for a chain (the Polygon replayer already handles BLS12-377 — `replayer.rs:77`)

The `ChainName` enum + config registry carry all per-chain variation (`core/src/types/chain.rs:45`).

---

## Appendix A — Existing Code Reuse Map

| Need | Reuse from | Reference |
|---|---|---|
| CLI command plumbing | `CliCommand` trait, dispatch | `cli/src/commands/mod.rs:31`, `:81` |
| Setup sequence template | `live` command | `cli/src/commands/live.rs:46-134` |
| RPC client (receipts, batching, rate limits) | `RpcClient` | `core/src/rpc/client.rs:125`; batch `:976`; receipts `:959` |
| Block+receipt fetching | `Fetcher::fetch_relevant` / `ActivityScanner` | `core/src/fetch/fetcher.rs:78`; `core/src/pipeline/scanner.rs:116` |
| Pool registry & state | `PoolManager` | `core/src/pool/state/manager.rs:62` |
| Swap/log decoding | decoders | `core/src/pool/decoders.rs:9-33` |
| Quote/USD math | quote engine | `core/src/pool/math/core.rs:53`, `:200` |
| Gas cost accounting | gas module | `core/src/mev/gas.rs:12` |
| Storage | `SqliteStore`, manifests, integrity | `core/src/cache/store/mod.rs:48`, `:27` |
| Output | `MevOpportunity`, `ResultsFile`, table render | `core/src/types/opportunity.rs:17`, `:218`; `cli/src/display.rs:35`, `:73` |
| Chain config registry | `chains.toml`, `ChainName` | `core/data/chains.toml`; `core/src/types/chain.rs:45` |
