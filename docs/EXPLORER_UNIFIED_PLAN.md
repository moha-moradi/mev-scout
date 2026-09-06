# MEV Explorer — Unified Plan (Master Document)

> **Status:** DRAFT — Owner: mev-scout team — Last updated: 2026-09-06
>
> This document **merges and supersedes** the three prior explorer plans:
> - `docs/EXPLORER_EXECUTION_PLAN.md` — execution plan, Polygon-first, full indexer + store + CLI
> - `docs/explorer-plan.md` — implementation plan, logs-first realized-MEV classifier, chain-agnostic
> - `docs/EXPLORER_PLAN.md` — design plan, results-layer (`opportunities` table) + cross-validation
>
> **Goal:** Add a **self-built, realized-MEV explorer** to `mev-scout` — comparable in surface to
> https://explorer.mev.zone (Avalanche) — **without any third-party MEV API** (no EigenPhi, no
> mev.zone API). All data is reconstructed from raw chain data via RPC. Primary deliverable: a set
> of `mev-scout explorer` CLI commands that (a) index MEV operations from blocks, (b) render a live
> feed + historical stats/leaderboards, and (c) cross-validate "what was *actually extracted* on-chain"
> (explorer ground truth) vs "what `run`/`live` *would have detected*" (our opportunity scanner) —
> quantifying recall/precision of the hunting pipeline and providing independent market intelligence
> (who extracts, how much, where).
>
> The `explorer` command is **distinct from the existing `live` command** (a streaming backtest
> scanner), and it is **not a new detection pipeline**: it consumes the same per-block data stream
> the scanner already produces and applies *pattern detectors over realized txs* instead of
> *opportunity simulation*.
>
> **Target chains:** Polygon (137) first — primary hunting chain, default config — then Avalanche,
> BSC, Ethereum; architecture stays chain-generic (the `ChainName` enum already covers 7 chains).
> The Avalanche-first sequencing alternative (external ground truth via explorer.mev.zone) is
> documented in §16 D2. Scope: **live + 30-day backfill**, **paid RPC** (Alchemy/drpc/GetBlock —
> already configured).

---

## Table of Contents

1. [Background: How MEV Explorers Actually Work](#1-background-how-mev-explorers-actually-work)
2. [Goals & Non-Goals](#2-goals--non-goals)
3. [Taxonomy (MEV kinds in scope)](#3-taxonomy-mev-kinds-in-scope)
4. [Existing Foundation & Reuse Map](#4-existing-foundation--reuse-map)
5. [Design Decisions](#5-design-decisions)
6. [Data Sources, Volume & Backfill Economics](#6-data-sources-volume--backfill-economics)
7. [Architecture](#7-architecture)
8. [Core Module Design & Detection Methodology](#8-core-module-design--detection-methodology)
9. [Storage](#9-storage)
10. [CLI Surface](#10-cli-surface)
11. [Cross-Validation Methodology](#11-cross-validation-methodology)
12. [RPC Additions](#12-rpc-additions)
13. [Chain Coverage & Per-Chain Notes](#13-chain-coverage--per-chain-notes)
14. [Unified Roadmap (Phases & Acceptance Criteria)](#14-unified-roadmap-phases--acceptance-criteria)
15. [Risks & Known Limitations](#15-risks--known-limitations)
16. [Open Questions & Decision Points](#16-open-questions--decision-points)
17. [References](#17-references)
18. [Key File References](#18-key-file-references)

---

## 1. Background: How MEV Explorers Actually Work

Two fundamentally different views of MEV exist, and this project sits at their intersection:

| | **Opportunity detection** (existing `run` / `live`) | **Realized MEV exploration** (this plan) |
|---|---|---|
| Question | "What *could* have been extracted in this block?" | "What *was* extracted, by whom, at whose expense?" |
| Method | Simulate/replay + quote optimal input | Classify historical txs into MEV patterns; compute realized profit |
| Evidence | revm replay (`BlockReplayer`), pool quotes | On-chain tx patterns (logs), per-address balance deltas |
| Reference impls | Flashbots-style search tooling | explorer.mev.zone (Avalanche), EigenPhi |

### 1.1 How explorer.mev.zone works, and why ours is different

- mev.zone is the explorer of the **Avalanche MEV Zone auction**: searchers submit bundles with
  bids → builders assemble candidate blocks → validators pick the best reward+burn block. Because
  the MEV Zone operator **sees the auction**, its explorer knows exact routes, profits, and senders
  without inference.
- Polygon PoS has **no bundle/relay layer**. Ordering is Bor block producers + priority-fee
  auctions + private RPCs (bloXroute, Merkle) + **Fastlane** (validator MEV redistribution). There
  is no bid/bundle stream to tap.
- Therefore our explorer is a **forensic reconstruction**: replay blocks, decode swaps and
  transfers, classify patterns, and attribute profit from token balance deltas. Every number is
  "attributed MEV" with an explicit confidence level. This is the same model used by EigenPhi,
  libMEV, and (historically) Flashbots' `mev-inspect-py`.

### 1.2 Traces vs logs

EigenPhi and the Avalanche explorer classify realized MEV primarily from **node traces**
(`callTracer`, `prestateTracer`) — the reason they run their own nodes. Traces are **not strictly
required**, however:

- **Sandwich attacks, JIT liquidity, and liquidations classify cleanly from logs alone.**
- **Atomic arbs classify from `Transfer` + `Swap` logs** via per-address net token deltas.
- **On-demand `debug_traceTransaction` (prestateTracer, diffMode)** gives exact per-tx profits for
  verification of individual transactions — cheap when done per-tx, expensive when done for a whole
  backfill (10–100x the cost of `eth_getBlockReceipts` + `eth_getLogs`).

### 1.3 What mev.zone-style explorers show (views → backend requirements)

| View | Backend requirement |
|---|---|
| Arbitrage list | Backfill history, run arbitrage detection on every settled block |
| Liquidation list | Backfill history, run liquidation detection on every settled block |
| MEV Live | Real-time feed of detections at/near chain tip (in our v1: tail of the indexed store) |
| Daily stats (1D/1W/1M/1Y) | Aggregate per-block detections into period buckets |
| Leaderboard (top senders / profit / ops) | Tx sender (`from`) resolution + period aggregation |
| Most profitable token / avg profit | Token + profit aggregation |

### 1.4 Core conceptual layers (the "prerequisites & analysis")

| Layer | Purpose | mev.zone example | mev-scout equivalent |
|---|---|---|---|
| Data ingestion | RPC/archive access, mempool/pending capture, block logs | fetch blocks/txs/logs, pending block | `Fetcher`, `mempool.rs`, `RpcClient` |
| Detection | Arb/sandwich/JIT/liquidation per block | arbitrage + liquidation lists | `mev/detectors/*` (opportunity) + new `explorer::` classifiers (realized) |
| Persistence | Store detections for querying | DB of opportunities | `SqliteStore` + per-run `ResultsFile` (JSON only today) + new explorer store |
| Aggregation/analytics | Period stats, leaderboards, token/route rollups | Daily stats, leaderboard, MEV Live | `pipeline/aggregate.rs` (per-run only today) + new explorer queries |
| Presentation | Web dashboard / API over aggregates | explorer UI | *(not built — CLI command in v1; web UI optional later)* |

**MEV Live specifically** (pre-inclusion flavor) requires pending-tx capture + instant detection on
mempool state, plus a chain-tip feed. This maps to mev-scout's `mev/mempool.rs`
(`capture_pending_block`, `detect_pending_opportunities`) and `cli/commands/live.rs`. In our v1 the
`explorer live` feed is served from the indexed store (realized ops); a mempool-based variant is an
optional mode (§16 D10).

---

## 2. Goals & Non-Goals

**Goals**

1. Self-developed MEV detection/explorer pipeline, end-to-end, from RPC data. Full transparency of
   methodology (detection rules, confidence levels, pricing).
2. `explorer live`: mev.zone-style live feed of extracted MEV ops (time, kind, token, route,
   profit, sender) — comparable to `/mevlive`.
3. Historical sections: last 1d / 7d / 30d / 1Y aggregates, daily breakdown, leaderboards (senders,
   tokens, pools), most profitable token, highest single profit, per-tx operation detail.
4. Cross-validation harness: compare "what was actually extracted on-chain" (explorer ground truth)
   vs "what `run`/`live` would have detected" (our opportunity scanner) → recall, precision signal,
   missed-value USD, profit calibration.
5. Chain-generic architecture: the pipeline (fetch → detect → persist → aggregate → validate) runs
   on whatever chain the config selects; re-targeting a chain is configuration work, not algorithm
   work.

**Non-Goals (v1)**

- No mempool/pre-trade detection (already covered separately by `detectors/mempool.rs`); optional
  mempool-based `explorer live` mode deferred (§16 D10).
- No web frontend in v1 (CLI only; web UI is the optional final phase).
- No CEX-DEX (non-atomic) profit attribution in v1 — methodologically opaque; listed as
  `unknown`/excluded. Revisit in v2.
- Multi-chain in v1 = one chain at a time (Polygon first); the design stays chain-generic.
- No block-level traces in the backfill path (logs-only; see §5).

---

## 3. Taxonomy (MEV kinds in scope)

| Kind | Definition | Log-level signature | Confidence |
|---|---|---|---|
| `arb_atomic` | Single tx whose token-flow graph closes a cycle (≥1 or ≥2 pools) ending in the start token; residual = gross profit | Address participates in ≥2 swaps within one tx and exits with a positive net delta of a single "profit" token (typically WMATIC/WETH/stable). Filter out pools' own fee accruals and WETH/WMATIC deposit/withdraw noise. | `exact` |
| `sandwich` | Same sender performs two opposite-direction swaps on the same pool within one block, with a third-party (victim) swap between them; front-run buy → victim → back-run sell ordered by tx index | Same EOA/contract opens + closes a position around a victim swap, one block, same pool. Attacker profit = back-run output − front-run input netted in USD; victim swap size recorded. | `exact` |
| `liquidation` | Known liquidation events (Aave `LiquidationCall`, Morpho, Compound-fork `LiquidateBorrow`, Comet `AbsorbDebt`); profit = seized − repaid (+ subsequent same-tx swap of collateral) | Exact event match on configured lending-pool addresses from `chains.toml`. Zero-heuristic. | `exact` |
| `jit` | Mint + burn of a concentrated position in the same block around a large swap (fee capture) | V3 `Mint` + `Burn` for the same position (same owner/tick range) within one block, bracketing swaps. | `exact` |
| `jit_arb` | JIT + follow-up arb in same tx/block | Combination of the two patterns above in one tx/block. | `exact` |
| `unknown` | Profitable pattern not matching any rule (incl. probable CEX-DEX bots) | Profitable per balance deltas, no closed-cycle/known-event structure. | `inferred` |

---

## 4. Existing Foundation & Reuse Map

### 4.1 What mev-scout already provides (the hardest parts)

- **Chains** (configured in `core/data/chains.toml`): polygon (137), avalanche (43114), bsc (56),
  arbitrum (42161), base (8453), ethereum (1), optimism (10). **Polygon is fully wired**: V2/V3/V4
  factories, Balancer Vault, Aave V3, WMATIC, BLS precompiles.
- **Strategies**: `TwoHopArb`, `MultiHopArb`, `Jit`, `JitArb`, `Sandwich`, `Liquidation`
  (`core/src/types/strategy.rs`).
- **Live/pending detection**: `core/src/mev/mempool.rs` + `cli/src/commands/live.rs`.
- **Aggregation**: `core/src/pipeline/aggregate.rs` (`aggregate_with_prices`) — per-run summary,
  strategy + DEX metrics in wei and USD.
- **Persistence**: `SqliteStore` (blocks, receipts, transfers, pools) + per-run `ResultsFile` JSON.
- **Results data model**: `ResultsFile` + `MevOpportunity` (`core/src/types/opportunity.rs`) carry
  exactly the fields an explorer needs: `block_number`, `tx_index`, `strategy`, `pool_a`/`pool_b`,
  `token_in`/`token_out`, `input_amount`, `expected_profit`, `gas_cost_wei`, `path`, `timestamp`,
  `mempool_only`, `confidence`, plus `canonical_id` (L9 dedup) via `compute_canonical_id`.

### 4.2 Gaps vs a mev.zone-style explorer (not yet in the repo)

1. **Queryable opportunity store** — results are flat per-run JSON; no cross-run aggregate store.
2. **Run-to-run cross-validation** — comparing the same range detected by `run` vs `live` vs
   `explorer` (dedup by `canonical_id`).
3. **Period aggregation views** (1D/1W/1M/1Y) and **leaderboards** (sender/token/pool).
4. **Forensic realized-MEV layer** — no indexer over settled blocks, no realized classifier, no
   balance-delta profit attribution, no explorer store/CLI.

### 4.3 Reuse map (merged)

| Explorer need | Reuse from | Reference |
|---|---|---|
| CLI command plumbing | `CliCommand` trait, dispatch | `cli/src/commands/mod.rs:31`, `:81` |
| Setup sequence template | `live` command | `cli/src/commands/live.rs:46-134` |
| RPC client (receipts, batching, rate limits, retries) | `RpcClient`, middleware | `core/src/rpc/client.rs:125`, batch `:976`, receipts `:959`; `core/src/rpc/middleware.rs:88`; `cli/src/rpc_setup.rs:10` |
| Block+receipt fetching | `Fetcher::fetch_relevant` / `ActivityScanner` | `core/src/fetch/fetcher.rs:78`; `core/src/pipeline/scanner.rs:116` |
| Config (chain, providers, output fmt) | `core/src/config/` + `mev-scout.toml` | `output = table|csv|json` key |
| Chain metadata + factories | `ChainName` registry | `core/src/types/chain.rs:45`; `core/data/chains.toml` |
| Event/topic decoding | sigs resolver/downloader, decoders | `core/src/sigs/`; `core/src/pool/decoders.rs:9-33` |
| Transfers / trades extraction | chain modules | `core/src/chain/{transfers,trades,events}.rs` |
| Liquidation / flash-loan sources | chain modules | `core/src/chain/liquidations.rs`, `core/src/chain/flashloans.rs` |
| Opportunity MEV detectors | detectors | `core/src/mev/detectors/{sandwich,two_hop,multi_hop,liquidation,jit,jit_arb}.rs` |
| Pool registry & state | `PoolManager` | `core/src/pool/state/manager.rs:62` |
| Quote / USD math | quote engine | `core/src/pool/math/core.rs:53`, `:200` |
| Block/state replay (targeted re-analysis only) | replayer, cache | `core/src/replay/` (`replayer.rs:77` `spec_id_for_block`), `core/src/cache/` |
| Gas cost accounting | gas modules | `core/src/mev/gas.rs:12`; `core/src/pipeline/gas.rs` (`historical_exact`) |
| USD pricing | CoinGecko | `core/src/coingecko.rs` |
| Storage | `SqliteStore`, manifests, integrity | `core/src/cache/store/mod.rs:48`, `:27`; gap-resume mirrors `auto_refetch_gaps` (`core/src/fetch/fetcher.rs`) + `integrity.rs` |
| Token metadata | token cache | `core/src/cache/token_cache.rs` |
| Output | `MevOpportunity`, `ResultsFile`, table render | `core/src/types/opportunity.rs:17`, `:218`; `cli/src/display.rs:35`, `:73` (comfy-table) |
| Results I/O reference | run/report commands | `cli/src/commands/run.rs`, `cli/src/commands/report.rs` |

**New code lives in:** `core/src/explorer/` (ingest, decode, classifier, profit, labels, store,
queries) and `cli/src/commands/explorer.rs` (subcommands). A **separate namespace** from
`core/src/mev/detectors/` (which are opportunity detectors) keeps the conceptual split explicit:
`mev::` = what could be made; `explorer::` = what was made.

---

## 5. Design Decisions

### 5.1 Logs-first, traces on-demand

**Decision:** the primary classification engine is **logs-only** (receipts) in v1; traces are an
optional per-transaction verification path, never a backfill requirement.

Logs-only coverage (≈ all of the v1 taxonomy):

- **Swaps**: V2/V3/V4 `Swap`, Curve `TokenExchange`, Balancer, Solidly/LB — all emit logs.
- **Transfers**: ERC20 `Transfer` (+ tx `value` for native POL accounting).
- **Liquidations**: Aave/Morpho/Compound-family events.
- **Sandwich/arb structure**: derived from swaps + transfers per tx/block.
- **Gas/tips**: from receipt `gasUsed`, `effectiveGasPrice`; priority fee = effective − base.

Traces are used only for:

1. **Per-tx on-demand verification** — `debug_traceTransaction` (prestateTracer, diffMode) behind
   `explorer show <TX> --trace`: exact profit recompute, cheap, negligible cost.
2. **Internal native POL transfers** (unwrapped profit) — requires `debug_traceBlockByNumber`
   (callTracer); **deferred to v2**. Verify per-provider support with `explorer doctor`; Alchemy
   supports `debug_*` on Polygon; drpc/GetBlock support is probed at runtime, not assumed.

Rationale:

1. **Cost** — full-trace backfill of 30 days of Polygon (~1.3M blocks) via paid RPC is the single
   largest cost driver by 10–100x vs `eth_getBlockReceipts` + `eth_getLogs`. Marginal accuracy gain
   at the aggregate level does not justify it.
2. **Architecture fit** — the entire existing stack is logs-based (ActivityScanner, decoders,
   `PoolManager`, revm replay for state reconstruction). Zero trace code exists today; every trace
   call would be net-new surface area.
3. **Precision where it matters** — individual txs worth auditing get exact numbers via on-demand
   prestateDiff (§12), closing the attribution gap case-by-case.

**Rejected alternatives:**

- *Full-trace backfill*: cost-prohibitive at Polygon block volume (§6).
- *Local revm-based profit extraction for all txs*: the replayer exists (`core/src/replay/replayer.rs`)
  but replaying every DEX-touching tx at backfill scale is far slower and RPC-heavy (state fetches
  through `CachedRpcDb`). Reserved as a possible later optimization for targeted re-analysis.
- *Third-party explorer APIs* (EigenPhi, mev.zone): explicitly out of scope — we build our own.

### 5.2 Key design rules

- **Decode-and-discard**: never persist raw traces/logs; only extracted facts.
- **Idempotent writes**: `(block_number, tx_index, log_index)` primary keys; re-running a range must
  be a no-op.
- **Reorg safety**: index block N only when head ≥ N + `confirmations` (default 6 ≈ 12s); on reorg
  detect (hash mismatch), delete ≥ fork block and re-index. Per-chain safe-depth config (small on
  Polygon/Avalanche/ETH; larger on BSC); live mode re-classifies the last few blocks on reorg.
- **No revm `BlockReplayer` in the explorer path** (logs-only) — the explorer is much cheaper per
  block than `live`.

### 5.3 Naming

The unified command group is **`explorer`** (majority across the source plans). The `explore`
variant from one plan maps 1:1 (`backfill`→`index`, `week`→`stats --window`, `leaderboard`→`top`,
`tx`→`show`, `verify`→`validate`). Final choice (`explorer` vs `explore` vs `inspect`) is tracked
as §16 D1.

---

## 6. Data Sources, Volume & Backfill Economics

### 6.1 Polygon volume math (2s blocks ⇒ 43,200 blocks/day)

- Receipts: 1 call per block via `alchemy_getTransactionReceipts` (or per-tx otherwise).
- At ~90 rps aggregate budget (9 providers), full-day live ≈ 45–50k calls/day → fine.
- 7-day backfill ≈ 320k blocks ⇒ ~400–600k calls incl. headers/retries ⇒ few hours.
- 30-day backfill ≈ 1.3M blocks (§6.3 table).
- SQLite size estimate: ~5–20 GB/month for facts (swaps dominate); prune `transfers` older than N
  days (ops + swaps retained).
- Nearly every Polygon block has DEX activity, so log-first filtering (`fetch_relevant`) saves
  little — plan for effectively full block+receipt fetching.

### 6.2 Cost driver ranking (every chain)

`eth_getBlockReceipts` (bulk) ≫ single-tx prestateDiff (on-demand only) ≫ everything else. Traces
never touch the backfill path. Provider rotation/cooldown machinery in `RpcClient`
(`core/src/rpc/middleware.rs:88`) already handles per-provider rate limits; figures below assume a
conservative aggregate across configured providers and improve with purchased RPS.

### 6.3 Backfill economics (30 days, four target chains)

| Chain | Block time | ~Blocks / 30d | Receipt payload weight | Est. backfill wall-clock @ ~10 req/s aggregate |
|---|---|---|---|---|
| Avalanche C-Chain | ~2s | ~1.3M | moderate | ~1.5–2 days |
| Polygon PoS | 2s | ~1.3M | moderate | ~1.5–2 days |
| BSC | 0.75–3s (hardfork-dependent) | ~0.9M–3.5M | **heavy** | ~3–5+ days |
| Ethereum L1 | 12s | ~216k | light | < 1 day |

---

## 7. Architecture

```
RPC providers (mev-scout.toml, rate-limited, middleware rotation)
        │  headers + receipts (+ optional traces: per-tx only in v1)
        ▼
explorer::ingest   ── block/tx/receipt fetch, reorg-aware, confirmation lag
        ▼
explorer::decode   ── swap/transfer/liquidation/jit facts  (reuse pool decoders + sigs
        │                                                     + NEW raw Transfer + liquidation registry)
        ▼
explorer::detect   ── classifier: liq pass → swap attribution → arb cycles →
        │             sandwich → JIT                              (reuse + new accounting detectors)
        ▼
explorer::account  ── per-tx token deltas → gross profit → gas → USD → net
        ▼
explorer::store    ── SQLite (WAL) v1 → Postgres/ClickHouse v2 (same schema)
        │             + results layer (opportunities table fed from ResultsFile)
        ▼
explorer::query    ── live / stats / top / show / validate / export  → display.rs
```

Key principle: **the explorer consumes the same per-block data stream the scanner already
produces** (block + receipts → decoded events), but applies *pattern detectors over realized txs*
instead of *opportunity simulation*. No revm `BlockReplayer` is needed (logs-only).

---

## 8. Core Module Design & Detection Methodology

### 8.1 `classifier.rs`

Per-block input: block header + ordered txs with receipts + decoded logs + `PoolManager` state for
the block. Output: `Vec<MevEvent>`.

```rust
pub enum MevKind { ArbAtomic, Sandwich, Liquidation, Jit, JitArb, Unknown }

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
    pub confidence: Confidence,         // exact | inferred (score 0..1 for inferred)
}

pub struct MevBundle {
    pub kind: MevKind,                  // Sandwich bundles front-run + victim + back-run
    pub txs: Vec<MevEvent>,
    pub attacker: Address,
}
```

Classifier pipeline per block (ordered):

1. **Liquidation pass** — exact event match (`LiquidationCall` etc. on configured lending-pool
   addresses from `chains.toml`). Zero-heuristic, confidence exact.
2. **Swap attribution** — for every tx, collect swap events (already decoded by
   `core/src/pool/decoders.rs`) and ERC-20 Transfers (new decoder, §8.4). Build per-address net
   token deltas.
3. **Atomic arb pass** — addresses with ≥2 swaps in one tx and positive single-token net delta;
   closed walk in the swap edge list ending in the start token. Filter pools' fee accruals and
   WETH/WMATIC wrap noise.
4. **Sandwich pass** — cross-tx, same-block pattern match on (attacker, pool, pair): front-run buy →
   victim → back-run sell. Position by tx index. Attach victim.
5. **JIT pass** — V3 `Mint`/`Burn` same position same block; `jit_arb` when combined with a
   same-tx/block arb; leftovers with profit → `unknown`/`inferred`.

### 8.2 `profit.rs` (accounting — the core primitive)

1. Collect ERC20 `Transfer` logs + native value for the tx.
2. Build per-address per-token delta map (from/to bookkeeping; skip mints/burns of pool LP tokens;
   explicitly filter WETH/WMATIC deposit/withdraw pairs).
3. EOA sender (or its deployed contract) = candidate searcher.
4. Gross profit = positive delta of the searcher in the tx's **profit token** (token that appears in
   a closed cycle, or the residual token after netting flash-loan borrow/repay — flashloans already
   detected by `chain/flashloans.rs`). Profit-token priority: USDC → USDT → DAI → WMATIC → WETH →
   (long-tail via route pricing).
5. Gas cost from receipt: `gasUsed × effectiveGasPrice` (receipts already fetched by
   `get_block_and_receipts_batch`); USD via the `historical_exact` gas model; priority fee =
   effective − base.
6. Net profit = gross − gas (USD) − flash-loan fees. Persist both gross and net.

**Arb (cycle) detection**: decode swaps of the tx; the swap sequence forms a directed edge list over
tokens/pools; a closed walk starting/ending in the same token with the searcher's net positive delta
⇒ `arb_atomic`. Two-hop vs multi-hop inherits from the existing detectors, but the explorer path
uses **accounted balances**, not pool-state simulation — no archive state needed, hence logs-only
feasibility.

**Sandwich**: group txs in a block by pool; find pattern (swap A: tokenX→Y by EOA e) → (victim swap
X→Y) → (swap Y→X by same e). Profit = back-run output − front-run input netted in USD; victim hashes
recorded. User-harm USD = victim's slippage vs counterfactual execution price without the sandwich
(computable from pool reserves reconstructed by our pipeline) — **v1 reports attacker profit +
victim swap size**; counterfactual simulation deferred.

**Liquidation**: event-driven; profit = collateral seized − debt repaid (+ same-tx swap of
collateral); reuse `chain/liquidations.rs` seeds (Aave V3 Polygon pool
`0x794a61358D6845594F94dc1DB02A252b5b4814aD` first).

### 8.3 `labels.rs`

- Searcher label DB (SQLite table, §9): address → label, entity, evidence, first_seen block, source.
- Bootstrap: seed a static list (routers, DEX factories, Aave/Morpho, known MEV bot contracts,
  hand-curated at implementation time).
- Clustering: same deployer bytecode, same funder, same nonce stream; reuse `chain/labels.rs`.
- Growth loop: every confirmed classifier hit auto-inserts the address as `source = "classifier"`;
  confidence-weighted repeat hits promote the label. Compounding asset — must start collecting from
  day one of live indexing.
- Labels are UX sugar, not load-bearing for the math (unlabeled searchers still classify; addresses
  are primary keys).

### 8.4 Decoder additions (in `core/src/pool/decoders.rs` or sibling `explorer/decoders.rs`)

- Raw ERC-20 `Transfer` decoding (topic `0xddf252...`) — pool-scoped to keep volume manageable.
- Liquidation event topics per lending-protocol family: Aave-style `LiquidationCall` (Aave V3 on
  Avalanche/Polygon/ETH), Compound-fork `LiquidateBorrow` (Benqi, Trader Joe lending on Avalanche;
  Venus on BSC), Compound V3 (Comet) `AbsorbDebt` — a per-protocol event registry, not one
  hardcoded topic.
- These complement — do not replace — the existing DEX swap decoders.

### 8.5 Pricing

- **Live**: CoinGecko (`coingecko.rs`), cached hourly in the `prices` table. Chain-aware and correct
  (`coingecko_asset_id` / `coingecko_platform`): polygon→matic-network / polygon-pos,
  avalanche→avalanche-2, bsc→binancecoin, ethereum→ethereum.
- **Backfill**: DefiLlama hourly close (batch-friendly) — avoid free-tier throttling.
- **Long-tail fallback**: on-chain stable-route pricing using our pool math (WMATIC/WPOL, WETH,
  USDC, USDT, DAI legs) — flagged `source=onchain`. Exclude tokens with no stable leg from USD
  stats (report in token units).

---

## 9. Storage

SQLite v1 (WAL; Postgres-portable schema). Extend `core/src/cache/store/` (entry:
`core/src/cache/store/mod.rs:48`).

### 9.1 Explorer store (forensic layer)

```sql
CREATE TABLE blocks(
  block_number INTEGER PRIMARY KEY, block_hash TEXT NOT NULL, ts INTEGER NOT NULL,
  producer TEXT, base_fee_gwei REAL, tx_count INTEGER, indexed_at INTEGER NOT NULL);

CREATE TABLE txs(
  hash TEXT PRIMARY KEY, block_number INTEGER NOT NULL, tx_index INTEGER NOT NULL,
  "from" TEXT NOT NULL, "to" TEXT, success INTEGER NOT NULL,
  gas_used INTEGER, effective_gas_price_gwei REAL, priority_fee_gwei REAL,
  value_native TEXT, FOREIGN KEY(block_number) REFERENCES blocks(block_number));
CREATE INDEX txs_block ON txs(block_number);

CREATE TABLE transfers(
  block_number INTEGER, tx_index INTEGER, log_index INTEGER,
  token TEXT, "from" TEXT, "to" TEXT, amount TEXT, is_native INTEGER DEFAULT 0,
  PRIMARY KEY(block_number, tx_index, log_index));
CREATE INDEX transfers_token ON transfers(token, block_number);

CREATE TABLE swaps(
  block_number INTEGER, tx_index INTEGER, log_index INTEGER,
  pool TEXT NOT NULL, dex TEXT, amm TEXT,           -- amm: v2|v3|v4|curve|balancer|solidly|lb
  token_in TEXT NOT NULL, token_out TEXT NOT NULL,
  amount_in TEXT NOT NULL, amount_out TEXT NOT NULL, sender TEXT,
  PRIMARY KEY(block_number, tx_index, log_index));
CREATE INDEX swaps_pool ON swaps(pool, block_number);

-- One row per classified op (arb/liq/jit/jit_arb/unknown) or bundle (sandwich).
-- (a.k.a. mev_events in the earlier classifier-oriented plan; mev_ops is canonical here.)
CREATE TABLE mev_ops(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  block_number INTEGER NOT NULL, tx_index INTEGER, tx_hash TEXT NOT NULL, ts INTEGER NOT NULL,
  kind TEXT NOT NULL CHECK(kind IN ('arb_atomic','sandwich','liquidation','jit','jit_arb','unknown')),
  eoa TEXT NOT NULL, contract TEXT,                 -- searcher clustering target
  confidence TEXT NOT NULL CHECK(confidence IN ('exact','inferred')),
  canonical_id TEXT,                                -- explorer-side canonical form (§11.1.1 item 0);
                                                    -- enables T1 join to opportunity.canonical_id
  profit_token TEXT, profit_amount TEXT, profit_usd REAL,
  gas_cost_usd REAL, net_profit_usd REAL,
  route_json TEXT,                                  -- [{pool,dex,token_in,token_out,amount_in,amount_out}]
  victim_hashes TEXT,                               -- JSON array (sandwiches)
  details_json TEXT,                                -- kind-specific (liq bonus, JIT fees, bundle legs, …)
  detector TEXT NOT NULL, created_at INTEGER NOT NULL);
CREATE INDEX mev_ops_block ON mev_ops(block_number);
CREATE INDEX mev_ops_sender ON mev_ops(eoa, block_number);
CREATE INDEX mev_ops_kind_ts ON mev_ops(kind, ts);
CREATE INDEX mev_ops_canonical ON mev_ops(canonical_id);

CREATE TABLE labels(
  address TEXT PRIMARY KEY, kind TEXT, name TEXT, entity TEXT,
  evidence TEXT, first_seen_block INTEGER, source TEXT);  -- source: static | classifier

CREATE TABLE prices(
  hour INTEGER NOT NULL, token TEXT NOT NULL, usd REAL NOT NULL, source TEXT,
  PRIMARY KEY(hour, token));

-- Chain-level checkpointing
CREATE TABLE sync_state(
  chain_id INTEGER PRIMARY KEY, head INTEGER, indexed_to INTEGER, last_indexed_at INTEGER);

-- Per-block gap-safe resume (mirrors auto_refetch_gaps + integrity.rs patterns)
CREATE TABLE blocks_classified(
  block INTEGER PRIMARY KEY,
  classified_at INTEGER NOT NULL,
  event_count INTEGER NOT NULL);
```

Sandwich representation: one `mev_ops` row per bundle with victim hashes + legs in
`victim_hashes`/`details_json` (MevBundle), or per-leg rows sharing a bundle id — decided at
implementation time. When bundling: `tx_index` = front-run tx index (bundle anchor); full leg
indices live in `details_json`.

Aggregate views (daily counts, per-kind profit, sender leaderboards, most profitable token) are SQL
views in the store, queried by the reporting commands. Token metadata (decimals/symbol) reuses
`cache/token_cache.rs`.

### 9.2 Results layer (`opportunities` table)

A queryable cross-run store for the opportunity pipeline's own detections (fed from `ResultsFile`),
used by `validate` and the period rollups:

```sql
CREATE TABLE opportunities(
  run_id TEXT, chain TEXT,
  block_number INTEGER NOT NULL, tx_index INTEGER,
  strategy TEXT NOT NULL,
  pool_a TEXT, pool_b TEXT,
  token_in TEXT, token_out TEXT,
  input_amount TEXT, expected_profit TEXT,
  gas_cost_wei TEXT, path TEXT,
  timestamp INTEGER, mempool_only INTEGER, confidence TEXT,
  sender TEXT,
  tx_hash TEXT,                                   -- actual extractor tx, when known (§11.1.1)
  detection_path TEXT,                            -- replay | log_only | pending (see note below)
  canonical_id TEXT                               -- L9 dedup key (compute_canonical_id)
);
CREATE INDEX opportunities_block ON opportunities(chain, block_number);
CREATE INDEX opportunities_canonical ON opportunities(canonical_id);
```

- Insert from `ResultsFile`; the shared save path of `run`/`live` writes here too (keep the existing
  JSON export as-is).
- Query by range / sender / token / window.
- **`sender` and `tx_hash` require a struct change.** Neither field exists on `MevOpportunity`
  today (opportunity.rs:16-86) — `sender` is dropped at every detector construction site and there
  is no `tx_hash` at all. Sender-based joins and tx-granularity matching are **impossible without
  adding these fields** and threading them through the runner stamping (§4.3). Phase 0 must scope
  this struct change as a prerequisite of §11, not an optional nicety.
- **`detection_path` exists because `run` and `live` are not equivalent.** `live`'s log-only
  synthesis path (runner.rs:566-662) produces **only** TwoHop/MultiHop arb — sandwich/JIT/JitArb/
  liquidation are skipped. Recording `replay | log_only | pending` per opportunity makes a
  `run`-vs-`live` gap attributable to *algorithmic path coverage*, not to M7 scanner-ran-never
  coverage. `validate` must not assume the two are interchangeable.

### 9.3 Rejection capture (`rejected_candidates` table)

Cross-validation of false negatives ("explorer saw a realized op; scanner reported nothing") is
only explainable if the scanner's **negative space** is observable. Today `run`/`live` discard
candidates that fail quoting/gas/threshold filters, so a true coverage gap is indistinguishable
from a filtered candidate. Opt-in debug recording closes this:

```sql
CREATE TABLE rejected_candidates(
  run_id TEXT, chain TEXT,
  block_number INTEGER NOT NULL, tx_index INTEGER,
  strategy TEXT NOT NULL,
  pool_a TEXT, pool_b TEXT, path TEXT,
  token_in TEXT, token_out TEXT,
  input_amount TEXT, expected_profit TEXT,
  expected_profit_usd REAL, gas_cost_wei TEXT,
  reject_reason TEXT NOT NULL,        -- no_pool | no_path | quote_nonpositive | below_min_profit |
                                      -- gas_dominates | state_stale | sim_failed | other
  detail TEXT,                        -- free-form context (e.g. computed profit vs threshold)
  created_at INTEGER NOT NULL
);
CREATE INDEX rejected_block ON rejected_candidates(chain, block_number);
CREATE INDEX rejected_reason ON rejected_candidates(reject_reason);
```

- Populated by `run --record-rejections` / `live --record-rejections` (debug flag, default off to
  bound write volume; recommended ON for any window later fed to `explorer validate`).
- `explorer validate` degrades gracefully to block-level recall only when rejections are absent
  for the window (§11.1.2).
- Retention: mirrors the `opportunities` policy; prune or aggregate rows older than the
  validation horizon.

### 9.4 Storage discipline & retention

- The backfill classifies in-stream and persists **only** extracted facts (`mev_ops`, `swaps`,
  checkpoints) — **not** raw receipts/logs. Raw-receipt caching of 1.3M Polygon blocks would bloat
  SQLite to tens of GB.
- Live mode may keep its existing raw block cache behavior (the scanner needs it); a retention
  policy prunes raw cache older than N days while `mev_ops` is kept forever.
- `transfers` retention: propose 30 days hot, aggregate-only older (§16 D5).
- SQLite write contention during backfill: single-writer task + WAL + batched transactions; readers
  are WAL-safe.

---

## 10. CLI Surface

New `explorer` subcommand group, wired into `cli/src/cli.rs` (`Explorer(ExplorerArgs)` as a
`Subcommand` container) and `cli/src/commands/` (`explorer.rs`, registered in `commands/mod.rs`).
Setup sequence mirrors the existing `live` command (`cli/src/commands/live.rs:46-134`): config
validation → `init_rpc` (`cli/src/rpc_setup.rs:10`) → `SqliteStore::open` → pool list → `GasConfig`
→ `PoolManager` → `init_pools`.

All commands respect existing `output = table|csv|json` and `mev-scout.toml` provider config. New
optional toml section `[explorer]` (db path, confirmations, backfill batch). Output formats:
terminal table (primary, like `run`/`live`) + JSON export to `results/` reusing the
`ResultsFile`/`save_results_json` conventions, so downstream tooling treats both pipelines
uniformly. `run_id` convention: `explorer_{epoch}` / `explorer_backfill_{epoch}`.

```
mev-scout explorer doctor
    Probe every configured provider: latest block, archive (eth_getProof @ old block),
    traces (debug_traceBlockByNumber on recent), multicall support, bulk-receipt endpoint
    (eth_getBlockReceipts / alchemy_getTransactionReceipts), observed rps.
    → prints capability matrix; gates Phase 0.

mev-scout explorer index [--from N] [--to M] [--days 30] [--live] [--confirmations 6]
    Backfill and/or stream-index blocks into the store. Resumable parallel classification
    over historical ranges; checkpoints per block in blocks_classified; gap auto-resume
    mirrors auto_refetch_gaps (fetcher.rs) + integrity.rs patterns.
    Live mode: follow head − confirmations; decode → detect → account → insert.
    Idempotent; resumable via sync_state. Classify-in-stream; no raw receipt retention.

mev-scout explorer live [--kinds arb_atomic,sandwich,liquidation] [--min-profit-usd 1]
                        [--loop] [--duration S] [--poll-interval-ms MS] [--compare <run_id>]
    mev.zone "MEV Live"-style streaming table (tail of the store or channel from a running
    `index --live`): time | kind | token | route (A→B→C via DEX) | profit USD | sender.
    Optional mempool-based pending-capture mode (structured stream via mempool.rs +
    BacktestRunner) — see §16 D10. Cross-validation baseline vs `mev-scout live`.

mev-scout explorer stats [--since 1d|7d|30d|all] [--window day|week|month|year] [--kind …]
    Overview: op count per kind, total/avg/net profit, gas+tips burned, operated tokens,
    most profitable token, highest single profit, daily breakdown table, top searchers (7d),
    top pools/pairs, victim losses, USD totals. Pure SQL over the store — instant.

mev-scout explorer top [--by sender|token|pool] [--metric profit|ops] [--since …]
    Leaderboards (mev.zone "Leaderboard"/"Senders" sections). Requires tx `from` resolution
    from cached receipts (no extra RPC; lazy fallback only if missing).

mev-scout explorer show <TX_HASH> [--trace]
    Operation detail: full route with per-hop amounts, gross vs gas vs net, confidence,
    related victim txs (sandwich), links into existing replay tooling.
    --trace: on-demand debug_traceTransaction (prestateTracer diffMode) → exact profit
    recompute → stores verified=true + corrected numbers on the event.

mev-scout explorer explain <TX_HASH>
    Per-miss drill-down (false-negative triage): shows the realized op next to the scanner's
    closest recorded candidates (`rejected_candidates`) for the same block, the inferred miss
    cause (§11.1.2), and a deep-verification path via `explorer show <TX> --trace`.

mev-scout explorer validate [--since 7d] [--strategy all] [--run <id>] [--live <id>] […]
                            [--match-window N] [--tier-2] [--threshold-sweep] [--rerun-misses]
                            [--emit-missing-pools]
    Cross-validation: join on-chain extracted ops (ground truth) vs `run`/`live` opportunity
    detections cached in results/ + the opportunities table → recall (count- and USD-weighted),
    missed-value USD, false-positive disambiguation, profit calibration, time-to-extraction by
    competitors, missing-pool report. Output feeds the weekly report (`report` command) as a new
    section. (Methodology: §11.)
    --match-window N   match opportunities to realized ops within ±N blocks (default 0 for `run`
                       backtests; 1 recommended for `live`, whose pending-block opportunities may
                       land in the next block — §11.1.1).
    --tier-2           include pool-overlap+profit-token matches in headline recall (§11.1.1).
    --threshold-sweep  report recall (count + USD) as a function of the min-profit threshold.
    --rerun-misses     re-execute the scanner (debug, rejections recorded) over the top-N missed
                       blocks by USD to attach per-miss causes (§11.1.2).
    --emit-missing-pools  write pools seen in realized ops but absent from `PoolManager`
                       (config/coverage gap → feeds chains.toml / pool discovery).

mev-scout explorer export --format json|csv --since … [--kinds …] [--out FILE]
    Bulk export for external analysis (Dune-style notebooks).
```

Alias map from the earlier plans: `backfill` → `index` (historical mode); `week` → `stats
--window`; `leaderboard` → `top`; `tx` → `show`; `verify` → `validate`. Naming decision: §16 D1.

---

## 11. Cross-Validation Methodology

This is the **primary deliverable** of the whole project: a continuously measurable feedback loop
for the hunting pipeline.

### 11.1 Internal: realized vs opportunity

Loads `results/{run_id}.json` (opportunities) and/or the `opportunities` table, plus `mev_ops`
(realized) from the explorer store; joins on **block number** (and pool overlap where possible);
opportunity matching by **`canonical_id` + `block_number`** (canonical_id stability across
`run`/`live`/`explorer` is the linchpin — confirmed in Phase 0). Arbitrary N result files may be
compared over overlapping ranges (default: the two most recent `run` + `live` files).

Report dimensions:

- **Recall** — of blocks where the explorer saw realized MEV of type X, in how many did the scanner
  flag an opportunity of type X? *"Explorer saw realized arb in 412 blocks; scanner flagged 380 →
  92% recall."*
- **Precision signal** — blocks where the scanner flagged opportunities but the explorer found no
  realized MEV: candidate false positives **or** real opportunities that were unprofitable-in-
  practice / outcompeted. **Tagged separately, not silently merged.**
- **Magnitude comparison** — scanner's estimated profit vs explorer's realized profit per matched
  event → calibration curve for the estimator (gas model, price model accuracy).
- **Missed-value USD** — realized profit the scanner did not surface.
- **Time-to-extraction by competitors** — latency signal per strategy.
- **Per-type breakdown** — sandwich/arbs/liq/JIT reported independently; e.g., liquidations should
  be near-perfect matches; arb estimates are the interesting calibration signal.

Discrepancies must be explainable: each >1% miss gets a categorized cause in the weekly report
(new section produced by `report`).

### 11.1.1 Matching rules (anti-blindness for recall)

Recall is only as trustworthy as the join. The blindness modes below must be handled explicitly:

0. **The join key is not yet defined on the explorer side — T1 is aspirational until it is.**
   `canonical_id` (opportunity.rs:90-114) is a *block-agnostic, sender-agnostic* dedup string built
   from simulated pools/route; it is **not** a handshake key two pipelines can compare unless both
   compute it from the **same canonical route facts**. Phase 0 must pin an **explorer-side canonical
   form** — a function `MevEvent(route, pools, tokens, kind) → canonical_id` mirroring
   `compute_canonical_id` over *realized* flows (first/last pool + endpoint tokens; for sandwich the
   victim/backrun block-tx indices; for JIT the tick range; for liquidation the borrower+liquidator
   pair rather than asset pair). Until that function exists and is shown to overlap `run`/`live`
   IDs, T1 is empty and only T2/T3 are meaningful. This is the linchpin Phase 0 must confirm, and
   it cannot be confirmed until the explorer classifier emits its own canonical string (§8.1/§8.2).
1. **Identity joins undercount.** `canonical_id` is computed from opportunity simulation
   (pools/route), while explorer ops are reconstructed from realized flows — routes rarely match
   exactly (searchers use different hops/routers). Report recall at **three tiers**, never merge
   them:
   - **T1 exact**: same canonical_id (+ block).
   - **T2 overlap**: pool overlap ≥1 + same profit token + block within match window.
   - **T3 block-level**: any op of the same kind in the same block (upper bound; always reported,
     since unattributed multi-hop arbs can mask identity).
   Headline recall = T1∪T2 (per-type); T3 shown as the ceiling with the T2→T3 gap attributed to
   route-attribution limits (§15.2.1), not scanner blindness.

2. **Latency skew for `live`.** `live` detects opportunities on the *pending* block; execution
   lands in block N+1..N+k. Default match window for `live`-based recall: ±1 block
   (`--match-window`), and pending-only opportunities (`mempool_only=1`) must not be counted as
   misses for realized ops the scanner never had state for.
3. **One-to-many and many-to-one.** A scanner opportunity may be realized as several explorer ops
   (multiple bots race the same edge) and vice versa (bundled multi-strategy tx). Join on
   (block, kind, pool-set) with explicit 1:N/N:1 marking in the report; never dedupe silently —
   races *are* the signal for time-to-extraction.
4. **Match on native units; convert to USD only at the report layer.** The scanner's
   `expected_profit` is **gross, pre-gas, normalized to a native/token unit** (opportunity.rs:39)
   with **estimated** gas; the explorer's is a **net USD** figure from receipt-observed
   `gas_used × effectiveGasPrice` (plan §8.2.5-6). These are not directly comparable. The join and
   profit calibration (magnitude comparison, §11.1) must be done in **token/native units** on
   matched (block, kind, pool-set, profit-token); USD conversion applies only at the aggregation
   and USD-weighted-recall (§11.1.3) layers. A **gas-model calibration** column (`expected
   gas_cost_wei` vs realized `gas_cost_usd`) is required, since gas is the single largest noise
   source between the two views — feed it to M4 and the `--threshold-sweep` operating point.

### 11.1.2 False-negative (miss) taxonomy — the core diagnostic

The question "backtest said no opportunity, explorer says one happened" decomposes into a
**disjoint, exhaustive cause set**; every missed realized op gets exactly one category, and the
weekly report shows the distribution. This is what turns recall from a number into an
improvement loop.

| Cause | Definition | Detection signal | Fix owner |
|---|---|---|---|
| **M1 pool-gap** | A pool in the realized route is absent from `PoolManager` (not in chains.toml / discovery). Scanner never saw the edge. | `--emit-missing-pools` report: realized-op pools ∩ ¬PoolManager | config / pool discovery |
| **M2 fee-tier / venue-gap** | Pool known but wrong fee tier, or venue class not supported (Curve/Balancer/LB on non-ETH chains). | missing-pool report filtered to venue class | config / decoders |
| **M3 pricing/threshold** | Opportunity was detectable but estimated profit fell below `min_profit_usd`, or USD pricing error (long-tail token) pushed it under. | rejected_candidates with reason `below_min_profit` / `quote_nonpositive` matching the realized op's pool+block; threshold sweep curve | config / pricing |
| **M4 gas-model error** | Estimated gas cost exceeded true cost (or vice versa) → candidate killed by `gas_dominates`. | rejected reason `gas_dominates` vs realized gas_cost_usd delta | gas model |
| **M5 quote/AMM-math error** | Quote engine's expected output diverges from realized amounts on the same pools (state drift, wrong fee, tick math). | rejected `quote_nonpositive` where realized op shows positive profit on same pool set; calibration curve tail | pool math |
| **M6 competition / latency** | Opportunity existed and was even detected by us, but a racer extracted it first within the same block — a *detection success, execution miss*. | matched T1/T2 opportunity with competitor's realized op in same block; time-to-extraction metric | execution layer (out of explorer scope; reported) |
| **M7 scanner coverage** | Scanner never ran over the block (gaps in `opportunities`/results coverage for the window). | blocks_classified ∩ ¬opportunities-coverage; gap report | pipeline / ops |
| **M8 false-ground-truth** | Explorer itself mis-attributed (e.g. `unknown`-bucket noise, wrap noise) — the "miss" is explorer error. | sampled audit of misses; `show --trace` verification on top-USD misses | classifier |

Rules:

- **Disjointness:** classification order M1→M8 (first match wins); documented so counts sum to the
  miss total.
- **No rejections recorded ⇒ cause `unknown-coverage`**, reported separately — never silently
  bucketed into M3/M5 (§9.3).
- **Sampling discipline:** manual audit required for the top-N misses by USD per week (≥20 or 10%,
  whichever smaller) — keeps M8 honest.
- Every M-category maps to a concrete owner in the roadmap (§14 Phase 5 acceptance extends to:
  "each >1% miss has a cause **and** the M-distribution is printed").

**Ground-truth trust bound:** M8 implies recall computed here is a *lower bound* only if explorer
false negatives are bounded too. Add a reciprocal audit: sample blocks the scanner flagged as
profitable where the explorer found nothing (precision-signal set, §11.1) and hand-check a few —
if the explorer missed them, both directions undercount and the headline numbers must carry that
caveat.

### 11.1.3 Recall that matters: USD-weighted, threshold-swept, over time

- **Count-recall flatters trivial ops.** Primary metric: **USD-weighted recall**
  (realized profit USD captured by matched opportunities ÷ total realized profit USD). A scanner
  catching 90% of ops but 30% of value is worse than the inverse — report both.
- **Threshold sweep:** recall(count) and recall(USD) as a function of `min_profit_usd`
  (`--threshold-sweep`), so the operator picks the operating point on the curve instead of
  guessing. Expected shape: recall(USD) ≫ recall(count) at high thresholds.
- **Recall over time:** bucket recall by week — a degrading trend is the earliest signal of pool
  drift (new venues appearing) before USD numbers make it obvious.
- **Asymmetry note:** realized MEV has survivorship bias (only *profitable* extractions land);
  the scanner sees unprofitable candidates too. Never compare scanner opportunity counts to
  explorer op counts directly — always via the M-taxonomy and matched sets.

### 11.2 External ground truth (Avalanche)

The classifier is chain-agnostic, so it can be **validated against an independent web tool**:
explorer.mev.zone (Avalanche-specific realized-MEV explorer) plus Snowtrace for tx-level
inspection. No equivalent web tool covers Polygon/BSC realized MEV.

Method:

1. Sample several non-contiguous windows (e.g., 6 × 1 hour spread across different days/volatility
   regimes).
2. Run the explorer over those windows on C-Chain; export events.
3. Compare against mev.zone's recorded history for the same windows:
   - **Event counts per type** — arb within ±20%; sandwich and liquidation essentially exact
     (unambiguous on-chain signatures).
   - **Tx-level overlap** — sandwiches and liquidations should match ≥90% by tx hash.
   - **Searcher address overlap** and searcher-level USD profit totals reconciled within ~±15%
     (mev.zone may use trace-based attribution, so modest per-event profit deltas are expected and
     informative, not disqualifying).
4. Discrepancies triaged via Snowtrace (inspect the actual txs; where needed `explorer show --trace`).

Once this agreement is demonstrated, the same binary is trusted on Polygon/BSC/ETH — the only
per-chain changes are configuration (§13).

### 11.3 Per-chain verification tools (web-based, no external MEV API)

- **Avalanche**: compare `stats`/`top` aggregates against explorer.mev.zone public stats (arb &
  liquidation counts, avg/total profit, top senders); cross-check individual txs on snowtrace.io.
- **Polygon**: cross-check profitable swaps / liquidation txs on polygonscan.com; mempool sanity on
  polygon.vision (tx pool visualizer).
- **BSC**: BSCScan lag-check + pancakeswap.finance pool prices for manual spot checks.
- **Ethereum**: etherscan.io tx-level verification; public MEV dashboards only as a sanity
  reference (not a data source).

---

## 12. RPC Additions

Net-new to `core/src/rpc/client.rs` (the codebase has **zero** trace support today):

- `debug_trace_transaction(hash, {tracer: "prestateTracer", tracerConfig: {diffMode: true}})` —
  used **only** by `explorer show <TX> --trace`. Single-tx, on-demand, negligible cost. Verify
  provider support flags in config (Alchemy on Polygon supports it; drpc/GetBlock should be
  capability-probed like the existing archive detection at `client.rs:1361`).
- **No `debug_traceBlock*` anywhere in the v1 backfill path** (deferred to v2 for internal native
  transfers; per-provider support confirmed via `explorer doctor`).
- Bulk receipts: `alchemy_getTransactionReceipts` where available; per-tx receipts otherwise; on
  BSC, if a provider lacks `eth_getBlockReceipts`, fall back to range `eth_getLogs` (existing
  `probe_get_logs_limit` machinery handles limit discovery).

---

## 13. Chain Coverage & Per-Chain Notes

**The method is chain-agnostic.** The pipeline (fetch → decode → classify → persist → aggregate →
validate) runs on whatever chain the config selects; the explorer introduces no chain-specific
algorithm logic. Per-chain wiring already exists for the four target chains and more.

### 13.1 Chain coverage matrix (current `core/data/chains.toml`)

| Chain | chain_id | Factories / venues configured | Native (wrapped) | Aave V3 | Notes |
|---|---|---|---|---|---|
| **avalanche** | 43114 | V3, V2, Trader Joe LB, V4, Balancer | WAVAX `0xB31f…` | ✅ | Core of mev.zone — **best online-verifiable** (external ground truth) |
| **polygon** | 137 | V3 (×2), V2 (×5), V4, Balancer | WMATIC `0x0d50…` | ✅ | Default chain; BLS precompiles handled; **primary target** |
| **bsc** | 56 | V3, V2, Trader Joe, Pendle, V4, Balancer | WBNB `0xbb4C…` | ✅ | `uniswap_v2_default_fee = 25` set |
| **ethereum** | 1 | V3, V2 (×3), Curve, V4, Trader Joe, Pendle, Balancer | WETH `0xC02a…` | ✅ | Only chain with `curve_registry` today |

### 13.2 What transfers across chains — and what doesn't

- **Transfers:** all four target chains share standard EVM execution semantics — EIP-1559-style fee
  fields, ERC-20 `Transfer` logs, Uniswap-style `Swap`/`Mint`/`Burn` events, per-tx receipts. **The
  classification algorithm is identical on every chain**; only configuration varies (factories,
  lending markets, native wrapper, stablecoin set, reorg depth).
- **Does not transfer:** mempool/pending-MEV views (out of scope regardless), cross-domain MEV
  (§15), and the per-chain quirks below.

### 13.3 Chain-specific caveats (must be handled; do NOT block the design)

1. **EVM spec fidelity — Polygon-only today.** `spec_id_for_block` (`core/src/replay/replayer.rs:77`)
   maps hardfork → block number only for chain 137. Every other chain replays with `SpecId::NEXT`
   (latest). Fine for recent blocks; historically inaccurate for older ETH/BSC/Avalanche blocks
   (BSC worst case: pre-BEP-119 gas accounting). Optional accuracy work item: per-chain hardfork
   tables. Not required for first passes, which scan recent blocks near head.
2. **Mempool depth varies by chain.** `capture_pending_block` uses `eth_getBlockByNumber("pending",
   true)`. Depth: Ethereum (best) > Polygon/BSC ≈ > Avalanche (limited public mempool). Any
   mempool-based `explorer live` feed density is chain-dependent and may frequently be empty on
   Avalanche; it must never error (capture returns `None` on RPC failure). Cross-checks on
   Avalanche should compare **settled-block** detection, not mempool.
3. **Curve coverage is Ethereum-only in config.** Other chains have Curve but no factory registry
   configured; add registries to `chains.toml` if those pools matter. Not blocking.
4. **CoinGecko pricing is chain-aware and correct** (§8.5). Native-token USD aggregation works on
   all target chains.

### 13.4 Per-chain notes

**Avalanche C-Chain** (~2s blocks, dynamic fees; LB/Liquidity-Book decoders cover Trader Joe plus
Uniswap-style forks; fully defined in `chains.toml`):
- **Quirk:** C-Chain blocks can contain *atomic transactions* (C-Chain ↔ X/P-Chain imports/exports)
  that don't follow standard EVM receipt/log semantics — the classifier must skip non-EVM tx types
  gracefully rather than assume every tx has DEX-shaped logs.
- WAVAX as native wrapper; USDC/USDT stable set. **Ground truth:** explorer.mev.zone + Snowtrace —
  the reason to run the external-validation pass here.

**Polygon — primary hunting chain:**
- 2s blocks → ~43,200 blocks/day → **~1.3M blocks / 30 days**; effectively full block+receipt
  fetching (§6.1).
- No Flashbots: MEV flows through the open mempool + private RPCs (bloXroute, Merkle) and
  **Fastlane**. Irrelevant for historical classification; relevant only for future pending features.
- WMATIC wrapper; reorg depth small (Bor finality fast).

**BSC:**
- Block cadence shrunk repeatedly (3s → 1.5s Lorentz → 0.75s Maxwell, 2025) with very large gas
  limits (~100M+) and dense DEX activity → **heaviest per-block receipt payloads** of the four;
  batch/concurrency tuning and per-provider limits matter most.
- PancakeSwap V2/V3 (+ forks) dominate; heavily sandwiched chain.
- Historically deeper reorgs → per-chain safe-depth config for live re-classification.
- Trace support on paid RPCs is the spottiest of the four → the logs-first design pays off most.

**Ethereum L1:**
- 12s blocks; richest MEV and most diverse pool set (Uniswap V2/V3/V4, Curve, Balancer, …).
- **Private orderflow (Flashbots/builder bundles) does not hurt realized-MEV classification:** every
  landed bundle is fully visible in receipts/logs after inclusion. Blindness applies only to
  pre-inclusion views (out of scope).
- WETH wrapper; deepest token liquidity for USD conversion.

### 13.5 Polygon constants & seeds

| Item | Value |
|---|---|
| Chain id | 137 |
| Block time | ~2s (43,200 blocks/day) |
| WMATIC/WPOL | `0x0d500B1d8E8eF31E21C99d1Db9A6444d3ADf1270` |
| POL | `0x455e53CBB86018Ac2B8092FdCd39d8444aFFC3F6` |
| WETH | `0x7ceB23fD6bC0adD59E62ac25578270cFf1b9f619` |
| USDC (native) | `0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359` |
| USDC.e | `0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174` |
| USDT | `0xc2132D05D31c914a87C6611C10748AEb04B58e8F` |
| DAI | `0x8f3Cf7ad23Cd3CaDbD9735AFf958023239c6A063` |
| Profit-token priority list | USDC → USDT → DAI → WMATIC → WETH → (long-tail via route pricing) |
| V2 Swap topic0 | `0xd78ad95f…9d822` (standard; forks reuse) |
| V3 Swap topic0 | `0xc42079f9…ca67` |
| ERC20 Transfer topic0 | `0xddf252ad…b3ef` |
| Aave V3 Polygon pool | `0x794a61358D6845594F94dc1DB02A252b5b4814aD` |

(Full 32-byte topics live in `core/src/sigs/` — the resolver already computes them.)

### 13.6 Beyond the four target chains (future)

After BSC + Ethereum, further chains are **configuration-only additions**. `core/data/chains.toml`
already defines `arbitrum`, `base`, and `optimism` alongside the four targets; the classifier needs
no code changes for them (log patterns + generic ERC-20 deltas):

- DEX factory registry per chain (factories, fee tiers, Algebra forks, Balancer vault) — partially
  present.
- Lending market addresses per chain (Aave V3, Compound forks/V3, L2 lending markets) — needs
  completion.
- Pricing chain-of-trust (native wrapper per chain) parameterized in `profit.rs`.
- L2 nuances (Arbitrum / OP-stack block cadence, sequencer behavior) affect throughput economics,
  not classification correctness.
- Chain-specific precompile/state quirks matter only if revm paths are ever added for a chain (the
  Polygon replayer already handles BLS12-377 — `replayer.rs:77`).

---

## 14. Unified Roadmap (Phases & Acceptance Criteria)

Merged from the three source plans. Build-order principle: **probe capabilities → build the
indexer → classify → account → report → validate → roll out chains → (optional) UI**. All
classifier code from Phase 1 onward is chain-agnostic; re-targeting a chain is configuration work
(§13), not algorithm work.

### Phase 0 — Capability probe + prerequisites (est. 1–2 days)

- `explorer doctor` implemented (capability matrix: latest block, archive, traces, multicall,
  bulk-receipt endpoints, observed rps).
- Spike: fetch receipts for 1k recent Polygon blocks; measure latency/cost per provider; confirm
  the logs-only assumption on a sample containing V2/V3/Curve/Balancer swaps.
- Prerequisite confirmations: which fields are populated in `MevOpportunity` per strategy (profit,
  path, sender availability); receipt data (tx `from`) present in the SQLite store for leaderboard
  sender resolution; `canonical_id` stability across `run` vs `live` vs `explorer` for the same
  block (linchpin of `validate`); **scope the `MevOpportunity` struct change to add `sender` +
  `tx_hash` + `detection_path`** (absent today, required for tx-granularity join and sender-based
  metrics, §9.2); **whether `run`/`live` can emit rejected candidates with a
  reason enum today (reject-side instrumentation for miss attribution, §9.3/§11.1.2 — if not,
  scope the hook as part of Phase 1)**; assess whether per-chain EVM spec mapping is needed for the
  historical windows of interest (baseline with recent blocks first).
- (If Avalanche-first, §16 D2) Baseline Avalanche `stats`/`top` snapshot against explorer.mev.zone
  public stats to calibrate definitions (profit units, sender = tx `from`, period bucketing).
- **Done when:** capability matrix printed; storage/backfill cost estimate validated.

### Phase 1 — Indexer core + persistence (est. ~1–1.5 weeks)

- `explorer::ingest` + `explorer::store` (schema §9), reorg handling, resume, checkpointing.
- Decode: swaps + transfers + liquidation events for Polygon factories; decoder additions (raw
  ERC-20 Transfer + per-protocol liquidation registry, §8.4).
- `opportunities` results table + `rejected_candidates` table (§9.3, §9.4) + migration in
  `core/src/cache/store/`; insert from `ResultsFile`; update the shared save path so `run`/`live`
  results also land in the table, and `run`/`live --record-rejections` persists filtered
  candidates (the negative space `validate` needs for miss attribution, §11.1.2).
- **`MevOpportunity` struct change**: add `sender`, `tx_hash`, and `detection_path` (§9.2) and
  thread them through the runner stamping — prerequisite for tx-granularity join / sender metrics
  and for attributing `run`-vs-`live` coverage differences.
- **`mev_ops` must carry `canonical_id`** (§9.1) computed by the explorer-side canonical form
  (§11.1.1 item 0) so T1 matching is actually computable; register the canonical-form function here,
  not deferred to Phase 5.
- **Done when:** `explorer index --from -50000 --to -1000` completes idempotently on mainnet
  Polygon; `swaps` counts match a manual `eth_getLogs` sample within 0%; `run`/`live` results are
  persisted to the `opportunities` table.

### Phase 2 — Detectors → mev_ops (est. ~1 week)

- Explorer-mode accounting detectors (§8.1 pipeline: liquidation pass, swap attribution, arb cycles,
  sandwich, JIT/`jit_arb`, `unknown`) writing `mev_ops` with confidence levels.
- **Done when:** on a known sandwich-heavy range, ≥95% of manually labeled sandwiches are detected
  with <2% false positives (manual audit of 100 ops).

### Phase 3 — Accounting: profit, gas, USD, labels (est. ~1 week)

- Balance-delta attribution, flash-loan netting, pricing backfill (DefiLlama hourly + CoinGecko live
  + on-chain stable-route fallback), tips/gas USD, label DB + growth loop (§8.2–8.5).
- **Done when:** 100 sampled arb ops reconcile manually (±1% USD) against EigenPhi or a hand-
  computed sheet; `net_profit_usd` populated for ≥99% of `exact` ops; a spot-checked sample (≥20
  events) of `--trace`-verified profits matches prestateDiff-derived deltas.

### Phase 4 — Reporting commands + tests (est. 3–5 days)

- `live`, `stats`, `top`, `show`, `export` + aggregate views, `display.rs` integration.
- Tests: unit (aggregation helpers, leaderboard, period bucketing, validate matching) + one CLI test
  per subcommand mirroring existing `cli/tests/*` + a cross-validation fixture (same block range
  from `run` and `live` → `validate` reports a high match rate and sane profit deltas).
- **Done when:** `stats --since 7d` reproduces the mev.zone home-page metric set on our Polygon
  data; `live` streams new ops within ~2 confirmations of block inclusion; tests green.

### Phase 5 — Cross-validation harness (est. 3–5 days)

- `explorer validate`: canonical_id joins + realized-vs-opportunity recall/precision/calibration
  report (§11.1); weekly report section.
- **Miss taxonomy implementation** (§11.1.2): M1–M8 attribution engine + `explorer explain <TX>`;
  missing-pool report; USD-weighted + threshold-swept recall (§11.1.3).
- Rejection capture live in `run`/`live` (`--record-rejections` → `rejected_candidates`, §9.3).
- External ground-truth pass on Avalanche per §11.2 (moves earlier if Avalanche-first sequencing is
  chosen, §16 D2).
- Documentation deliverable: `docs/explorer-design.md` — the classification methodology written out
  (pattern definitions, heuristics, confidence scoring, known blind spots, per-chain adaptation
  notes). Writing the rules down enforces the "understand how these systems work" goal.
- **Done when:** the weekly report includes recall/missed-value/latency per strategy and
  discrepancies are explainable (each >1% miss has a categorized M1–M8 cause and the cause
  distribution is printed); §11.2 agreement thresholds met on the Avalanche sample (if executed);
  `validate` produces the §11 report for any existing run; `explorer explain` reconstructs a cause
  for ≥80% of the top-20 missed ops by USD on a sampled Polygon window.

### Phase 6 — Chain rollout + verification (config-only per chain)

- Sequencing per §16 D2: **Polygon first** (execution-plan default), then Avalanche (external
  validation pass), then BSC, then Ethereum — or the Avalanche-first alternative.
- **Avalanche**: full pass — `stats`/`top` vs explorer.mev.zone public stats; `live` (expect sparse
  feed if mempool mode); `validate` on settled-block ranges.
- **Polygon / BSC**: internal cross-validation (`validate` + spot checks on polygonscan/bscscan);
  no public per-chain MEV explorer exists for direct parity.
- **Ethereum**: internal cross-validation; sanity reference against public MEV dashboards (not as a
  data source).
- BSC config completion (PancakeSwap V2/V3 + forks factory registry, Venus liquidations, WBNB
  pricing, reorg-depth config); Ethereum config completion (Curve/Balancer factories, Aave V3 +
  Compound V3 liquidations); receipt-endpoint capability probing per provider per chain (matters
  most on BSC); batch/concurrency tuning for BSC's large blocks; optional 30-day backfills per
  chain; raw-cache retention policy.
- **Done when:** all four chains served by the same binary with per-chain config only; per-chain
  stats sections available; Avalanche agreement thresholds met (if executed).

### Phase 7 — (Optional, later) Web UI + store scale-out

- axum REST + WS over the same store; Next.js dashboard (overview, live feed, op detail, sender
  profile). Migrate the store to Postgres/ClickHouse if/when needed (schema is portable).
- Brief usage docs only if explicitly requested.
- **Done when:** parity with CLI sections; no business logic duplicated outside `core/src/explorer`.

---

## 15. Risks & Known Limitations

### 15.1 Risks & mitigations

| Risk | Mitigation |
|---|---|
| RPC quota/cost for backfill + live | Provider fan-out with existing RPS middleware; `alchemy_getTransactionReceipts` batching; logs-only v1; option of dedicated node later |
| Long-tail token pricing noise | Stable-route on-chain fallback; mark `source`; exclude tokens with no stable leg from USD stats (report in token units) |
| Misattribution (proxy/factory bots) | Confidence field + labeler evidence; never present `inferred` as exact in UI |
| CEX-DEX opacity | Excluded in v1; `unknown` bucket; explicit methodology note |
| Reorgs on Bor (and deeper on BSC) | Confirmation lag (default 6) + hash-mismatch re-index; per-chain safe-depth config |
| **Secrets** | **`mev-scout.toml` currently contains live API keys in-repo — move to env/file outside git before any sharing** |
| SQLite write contention during backfill | Single-writer task + WAL + batched transactions; readers are WAL-safe |

### 15.2 Known limitations (accepted, documented, revisit-able)

1. **Multi-hop arb attribution is heuristic.** Chained arbs across contracts can split/mask net
   deltas; logs-only sees token flows, not intent. Confidence scores + `--trace` verification cover
   the audit path.
2. **WETH/WMATIC wrap noise.** Deposit/withdraw pairs can masquerade as deltas — filtered explicitly
   in `profit.rs`, but edge cases will exist.
3. **Non-atomic / CEX–DEX MEV invisible.** Cross-domain extraction is out of scope by design
   (logs-only, per-block).
4. **Victim-loss figures are approximations** in v1 (counterfactual execution requires simulation;
   deferred — report attacker profit + victim swap size first).
5. **Pre-inclusion (pending) MEV is blind on private-orderflow chains.** Realized classification is
   unaffected — landed bundles are fully visible in receipts — but any future mempool/pending
   features would undercount on Ethereum/BSC relative to Polygon/Avalanche.
6. **Labeled-bot dependence for pretty output.** Unlabeled searchers still classify fine (addresses
   are primary keys); labels are UX sugar, not load-bearing for the math.
7. **Ground truth is one-sided.** The explorer sees only *landed, profitable* extractions
   (survivorship bias): an opportunity our scanner found that nobody extracted is invisible to it,
   and mis-attributed ops (M8, §11.1.2) can fake misses. Cross-validation numbers are therefore
   bounded estimates, not exact — the reciprocal audit (§11.1.2) and `--trace` verification keep
   both directions honest.

---

## 16. Open Questions & Decision Points

| # | Question | Recommendation / notes |
|---|---|---|
| D1 | **Naming**: `explorer` vs `explore` vs `inspect` for the subcommand group | `explorer` (2 of 3 source plans; used throughout this doc) |
| D2 | **Chain order**: Polygon-first (execution plan; primary hunting chain, default config, project priority) vs Avalanche-first (external ground truth via mev.zone + Snowtrace; validate the whole surface where it is publicly checkable, then trust the binary elsewhere) | This doc defaults to **Polygon-first** (most recent plan) with the **Avalanche external-validation pass** executed inside Phase 5; switch to full Avalanche-first by reordering Phases if external validation is deemed the bigger risk |
| D3 | GetBlock/drpc `debug_*` support on Polygon | Confirm via `explorer doctor` (affects v2 native-transfer coverage only) |
| D4 | Should `explorer live` also emit ops into the same `results/` artifacts used by `report`, or stay read-only over the store? | Separate streams; join at `validate` |
| D5 | Retention policy for `transfers` | Propose: 30 days hot, aggregate-only older |
| D6 | Results-layer storage: persist opportunities into the SQLite `opportunities` table or stay JSON-driven by scanning `export_path`? | SQLite table **and** keep the existing JSON export |
| D7 | Sender resolution for leaderboards | Cached receipts first (no extra RPC); lazy fallback only if missing |
| D8 | Scope of period stats: aggregate persisted results only, or also *run* backtests on demand over N days when no results exist? | Aggregate persisted results first; add on-demand backfill as an option |
| D9 | Cross-validation input: restrict to `run` vs `live` `ResultsFile`s, or allow arbitrary N result files over a range? | Arbitrary N, defaulting to the two most recent `run` + `live` files |
| D10 | `explorer live` mode: store-tail feed (realized ops from the indexer; v1 default) vs mempool pending-capture feed (mev.zone pre-inclusion flavor; density chain-dependent, sparse on Avalanche) | Store-tail in v1; mempool mode optional later |
| D11 | Rejection recording (`--record-rejections`) default-off everywhere, or on-by-default for `live`? | Default-off; auto-enable for windows later passed to `validate` (documented convention); retention per §9.3 |
| D12 | Default match window + tier policy for `validate` | `run`: window 0, T1∪T2 headline; `live`: window ±1; T3 always printed as ceiling; overrides via flags (§11.1.1) |
| D13 | Miss-taxonomy depth: full M1–M8 attribution for every miss, or only top-N by USD? | Full M1–M8 for all misses (cheap: store joins); manual audit sampling restricted to top-N by USD (§11.1.2) |
| D14 | `MevOpportunity` struct change (`sender` + `tx_hash` + `detection_path`) — cross-cutting, touches 9 detector construction sites + runner stamping | Do it in Phase 1: required for tx-granularity join, sender metrics, and run-vs-live coverage attribution (§9.2, §11.1.1). Backward-compatible (new optional fields, default `None`); backfill only populates where known |

---

## 17. References (methodology, not APIs)

- Flashbots `mev-inspect-py` (archived) — schema/taxonomy reference (swaps, arbitrages, liquidations,
  miner payments).
- EigenPhi methodology pages — classification definitions & known limitations.
- libMEV — open multi-chain explorer (prior art for labels/leaderboards).
- mev.zone docs (gitbook) — auction model reference (Avalanche-specific data advantage).
- Public block explorers for spot checks: Snowtrace, Polygonscan (+ polygon.vision), BSCScan (+
  pancakeswap.finance), Etherscan.

## 18. Key File References

- `core/data/chains.toml` — chain config (polygon, avalanche, bsc, ethereum, + arbitrum/base/optimism)
- `core/src/types/opportunity.rs` — `MevOpportunity`, `ResultsFile`, `compute_canonical_id`
- `core/src/types/strategy.rs` — `Strategy`, gas/flash-loan models
- `core/src/types/chain.rs` — `ChainName` registry (`:45`)
- `core/src/cache/` (`store/*`, `token_cache.rs`) — `SqliteStore` (add `mev_ops`/`opportunities` tables here), token metadata
- `core/src/replay/replayer.rs` — `spec_id_for_block` (`:77`, per-chain EVM spec, Polygon-only today)
- `core/src/pipeline/aggregate.rs` — per-run aggregation to extend
- `core/src/pipeline/gas.rs` — `historical_exact` gas model; `core/src/mev/gas.rs:12`
- `core/src/mev/mempool.rs` — pending-tx capture (optional `explorer live` mempool mode)
- `core/src/rpc/client.rs` — `RpcClient` (`:125`, batch `:976`, receipts `:959`, archive probe `:1361`); `core/src/rpc/middleware.rs:88`
- `core/src/fetch/fetcher.rs` — `fetch_relevant` (`:78`), `auto_refetch_gaps`
- `core/src/pipeline/scanner.rs` — `ActivityScanner` (`:116`)
- `core/src/pool/state/manager.rs` — `PoolManager` (`:62`); `core/src/pool/decoders.rs:9-33`; `core/src/pool/math/core.rs:53`, `:200`
- `core/src/chain/{transfers,trades,events,liquidations,flashloans,labels}.rs` — extraction/seed sources
- `core/src/coingecko.rs` — USD pricing
- `cli/src/cli.rs`, `cli/src/commands/mod.rs` — CLI wiring to extend (`:31`, `:81`)
- `cli/src/commands/live.rs` — setup-sequence reference (`:46-134`) and pending-capture pattern (`:250`)
- `cli/src/commands/run.rs`, `cli/src/commands/report.rs` — results I/O reference
- `cli/src/display.rs` — table rendering (`:35`, `:73`)
- `cli/src/rpc_setup.rs` — `init_rpc` (`:10`)
- Existing docs: `CLI_EXECUTION_PLAN.md`, `mev_strategies.md`
