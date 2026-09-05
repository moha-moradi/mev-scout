# MEV Explorer — Design & Implementation Plan

**Goal:** Add a new `explorer` CLI command to `mev-scout` (self-built, no external MEV APIs),
modeled on the dashboard features of `https://explorer.mev.zone` (mev.zone for Avalanche):
the `MEV Live` feed, daily/weekly stats, and leaderboards.

This command is **distinct from the existing `live` command** (which is a streaming
backtest scanner). The new `explorer` command adds a **queryable/aggregating result layer**
plus **cross-validation** against `run`/`live` output, so we can confirm detection consistency
across code paths.

---

## Part A — Research: how mev.zone-style explorers actually work

What explorer.mev.zone shows and what each view requires:

| View | Backend requirement |
|---|---|
| Arbitrage list | Backfill history, run arbitrage detection on every settled block |
| Liquidation list | Backfill history, run liquidation detection on every settled block |
| MEV Live | Real-time pending-tx capture + instant detection on mempool state, tailing chain tip |
| Daily stats (1D/1W/1M/1Y) | Aggregate per-block detections into period buckets |
| Leaderboard (top senders / profit / ops) | Tx sender (from) resolution + period aggregation |
| Most profitable token / avg profit | Token + profit aggregation |

### Core conceptual layers (the "prerequisites & analysis")

| Layer | Purpose | mev.zone example | mev-scout equivalent |
|---|---|---|---|
| Data ingestion | RPC/archive access, mempool/pending capture, block logs | fetch blocks/txs/logs, pending block | `Fetcher`, `mempool.rs`, `RpcClient` |
| Detection | Arb/sandwich/JIT/liquidation per block | arbitrage + liquidation lists | `mev/detectors/*` |
| Persistence | Store detections for querying | DB of opportunities | `SqliteStore` + `ResultsFile` (per-run JSON only today) |
| Aggregation/analytics | Period stats, leaderboards, token/route rollups | Daily stats, leaderboard, MEV Live | `pipeline/aggregate.rs` (per-run only today) |
| Presentation | Web dashboard / API over aggregates | explorer UI | *(not built — CLI command instead)* |

**MEV Live specifically** requires pending-tx capture + instant detection on mempool state,
plus a chain-tip feed. This maps directly to mev-scout's `mev/mempool.rs`
(`capture_pending_block`, `detect_pending_opportunities`) and the `cli/commands/live.rs`.

---

## Part B — What mev-scout already provides (foundation for Polygon + 6 other chains)

mev-scout already supplies the hardest parts — a chain-agnostic detection engine + persistence:

- **Chains** (configured in `core/data/chains.toml`): polygon (137), avalanche (43114),
  bsc (56), arbitrum (42161), base (8453), ethereum (1), optimism (10).
  **Polygon is fully wired**: V2/V3/V4 factories, Balancer Vault, Aave V3, WMATIC, BLS precompiles.
- **Strategies**: `TwoHopArb`, `MultiHopArb`, `Jit`, `JitArb`, `Sandwich`, `Liquidation`
  (`core/src/types/strategy.rs`).
- **Live/pending detection**: `core/src/mev/mempool.rs` + `cli/src/commands/live.rs`.
- **Aggregation**: `core/src/pipeline/aggregate.rs` (`aggregate_with_prices`) — per-run summary,
  strategy + DEX metrics in wei and USD.
- **Persistence**: `SqliteStore` (blocks, receipts, transfers, pools) + per-run `ResultsFile` JSON.
- **Results data model**: `ResultsFile` + `MevOpportunity` (`core/src/types/opportunity.rs`)
  carry exactly the fields an explorer needs: `block_number`, `tx_index`, `strategy`,
  `pool_a`/`pool_b`, `token_in`/`token_out`, `input_amount`, `expected_profit`,
  `gas_cost_wei`, `path`, `timestamp`, `mempool_only`, `confidence`, plus `canonical_id`
  (L9 dedup) via `compute_canonical_id`.

### Gaps vs a mev.zone-style explorer (not yet in the repo)

1. **Queryable opportunity store** — results are flat per-run JSON; no cross-run aggregate store.
2. **Run-to-run cross-validation** — comparing the same range detected by `run` vs `live` vs
   `explorer` (dedup by `canonical_id`).
3. **Period aggregation views** (1D/1W/1M/1Y) and **leaderboards** (sender/token).
4. An **`explorer` command with sub-commands**.

---

## Part C — Design: new `explorer` command (own implementation, no external APIs)

The existing `live` command stays untouched. The new command surface:

```
mev-scout explorer <SUBCOMMAND>
  ├── live           # opportunistic feed equivalent of mev.zone's /mevlive
  ├── week           # last-7-days aggregate (window pluggable: day/week/month/year)
  ├── leaderboard    # top senders / top tokens by profit or operations
  └── validate       # cross-validate explorer detections vs `run`/`live` results
```

### Shared foundation (new)

**New `opportunities` SQLite table + module** (`core/src/cache/store/`):
- Schema: `run_id, chain, block_number, tx_index, strategy, pool_a, pool_b,
  token_in, token_out, input_amount, expected_profit, gas_cost_wei, path,
  timestamp, mempool_only, confidence, sender`.
- Insert from `ResultsFile`; query by range / sender / token / window.

**Aggregation helpers** (new `core/src/explorer/` or added to `pipeline/`):
- `aggregate_period(results, window)` — total operations, total/avg profit, daily stats,
  most profitable token.
- `leaderboard(results, by: Sender|Token, metric: Profit|Ops)`.

### Sub-command details

**1. `explorer live`** — cross-validating `/mevlive`.
- Reuse `mempool.rs` pending capture + `BacktestRunner` (like `cmd_live`), but emit a
  **structured stream** of opportunity records (JSON lines / table) labeled with `run_id`.
- Optional `--compare <run_id>`: diff against a prior run's results.
- Key difference from `live`: purpose is the explorer feed + cross-validation, not the
  streaming backtest loop.

**2. `explorer week [--days N | --window day|week|month|year]`** — aggregate view.
- Read all `run_*.json` results (and/or the `opportunities` table) within the window.
- Print period stats (total ops, total/avg profit, most profitable token, daily stats)
  mirroring mev.zone's overview — all computed locally.

**3. `explorer leaderboard [--by sender|token] [--metric profit|ops] [--window ...]`** —
- Top senders / top tokens by profit and operation count over a window.
- Requires tx `from` resolution from cached receipts (no extra RPC beyond what `live`/`run`
  already use; see decision point below).

**4. `explorer validate --run <id> --live <id> [--range ...]`** — the user's primary ask:
- Load ≥2 `ResultsFile`s covering overlapping blocks.
- Match opportunities by `canonical_id` + `block_number`.
- Report match / no-match / extra / missing with profit deltas → checks that `explorer`
  detections are consistent with `run`/`live`.

---

## Part D — Feasibility & chain coverage (BSC, Polygon, Avalanche, Ethereum)

**The method is chain-agnostic.** The pipeline (fetch → detect → persist → aggregate →
validate) runs on whatever chain the config selects; the explorer command does not introduce
chain-specific logic beyond what `mev-scout` already has. Per-chain wiring already exists for
all four target chains and more.

### Chain coverage matrix (current `core/data/chains.toml`)

| Chain | chain_id | Factories / venues configured | Native (wrapped) | Aave V3 | Notes |
|---|---|---|---|---|---|
| **avalanche** | 43114 | V3, V2, Trader Joe LB, V4, Balancer | WAVAX `0xB31f…` | ✅ | Core of mev.zone — **best online-verifiable** |
| **polygon** | 137 | V3 (×2), V2 (×5), V4, Balancer | WMATIC `0x0d50…` | ✅ | Default chain; BLS precompiles handled |
| **bsc** | 56 | V3, V2, Trader Joe, Pendle, V4, Balancer | WBNB `0xbb4C…` | ✅ | `uniswap_v2_default_fee = 25` set |
| **ethereum** | 1 | V3, V2 (×3), Curve, V4, Trader Joe, Pendle, Balancer | WETH `0xC02a…` | ✅ | Only chain with `curve_registry` today |

Everything the explorer needs (detection engine, mempool capture, per-run aggregation,
`ResultsFile` persistence) is already present and chain-generic. The explorer adds a
queryable/aggregating result layer + sub-command surface + cross-validation — **not a new
detection pipeline**. No external MEV API is used: everything is computed from own RPC fetch +
revm replay + own aggregation.

### Chain-specific caveats (must be handled, do NOT block the design)

1. **EVM spec fidelity — Polygon-only today.**
   `spec_id_for_block` (core/src/replay/replayer.rs:77) maps hardfork → block number **only for
   chain 137**. Every other chain replays everything with `SpecId::NEXT` (latest). This is fine
   for recent blocks but historically inaccurate for older ETH/BSC/Avalanche blocks; BSC is the
   worst case (pre‑BEP‑119 gas accounting differs from the EVM spec revm models). Scope: an
   **optional accuracy work item** — add per-chain hardfork tables for ETH (Berlin/London→C…→Prague),
   BSC (BEP-119 → block 63,437,960), Avalanche (Cortina/Durango/Hobbiton equivalents). Not
   required for the first Avalanche pass (which scans recent blocks near head).

2. **Mempool depth varies by chain.**
   `capture_pending_block` uses `eth_getBlockByNumber("pending", true)`. Availability/depth:
   Ethereum (best) > Polygon/BSC ≈ > Avalanche C-chain (limited public mempool). Therefore
   `explorer live` **feed density is chain-dependent**, and on Avalanche it may frequently be
   empty. It should never error (the capture already returns `None` on RPC failure), but the
   `validate` cross-check on Avalanche should compare **settled-block** detection, not mempool.

3. **Curve coverage is Ethereum-only in config.** Other chains have Curve but no factory
   registry configured. If Avalanche/BSC/Polygon Curve pools matter, add their registries to
   `chains.toml`. mev.zone itself focuses on Trader Joe LB + liquidations on Avalanche, so this
   is not blocking for Avalanche parity.

4. **CoinGecko pricing is chain-aware and correct** (`coingecko_asset_id` / `coingecko_platform`):
   polygon→matic-network / polygon-pos, avalanche→avalanche-2, bsc→binancecoin, ethereum→ethereum.
   Native-token USD aggregation works on all target chains.

### Per-chain verification approach (web-based tools, no external MEV API)

- **Avalanche (priority)**: compare `explorer week`/`leaderboard` aggregates against
  `https://explorer.mev.zone` public stats (arbitrage & liquidation counts, avg/total profit,
  top senders); cross-check individual txs on `https://snowtrace.io`. This is why Avalanche is
  the recommended first target.
- **Polygon**: cross-check profitable swaps / liquidation txs on `https://polygonscan.com`;
  mempool sanity on `https://polygon.vision` (tx pool visualizer).
- **BSC**: BSCScan lag-check + `https://pancakeswap.finance` pool prices for manual spot checks.
- **Ethereum**: `https://etherscan.io` tx-level verification, public MEV dashboards only as a
  sanity reference (not a data source).

### Recommended sequencing (Avalanche first)

1. **Avalanche** — highest verifiability (public mev.zone + Snowtrace), Trader Joe LB + Aave
   already wired. Validate the whole explorer surface end-to-end.
2. **Polygon / BSC** — internal cross-validation via `explorer validate` (run vs live vs
   explorer) since no public per-chain MEV explorer matches them; spot-check on block explorers.
3. **Ethereum** — largest venue set (incl. Curve); same internal cross-validation.

This keeps phase-by-phase risk low and bounds each chain's verification to tools we already trust.

---

## Part E — Concrete implementation phases

### Phase 0 — prerequisites / exploratory notes
- Confirm which fields are already populated in `MevOpportunity` for each strategy
  (profit, path, sender availability).
- Confirm receipt data (tx `from`) is present in the SQLite store for leaderboard sender
  resolution.
- Confirm `canonical_id` stability across `run` vs `live` vs `explorer` for the same block
  (this is the linchpin of `validate`).
- (Optional, per Part D caveat 1) Assess whether per-chain EVM spec mapping is needed for the
  historical windows of interest; baseline with recent-blocks scanning first.
- (Avalanche first) Baseline Avalanche `week`/`leaderboard` snapshot against
  explorer.mev.zone public stats to calibrate definitions (profit units, sender = tx `from`,
  period bucketing) before implementing the other chains.

### Phase 1 — persistence: `opportunities` table
- New schema + migration in `core/src/cache/store/`.
- New module: insert from `ResultsFile`, query by range/sender/token/window.
- Update `run`/`live` (or a shared save path) so results are also written to this table.

### Phase 2 — aggregation helpers
- `aggregate_period`, `daily_stats`, `leaderboard` in a new `core/src/explorer/` module,
  reusing `pipeline/aggregate.rs` where possible.

### Phase 3 — CLI wiring
- `cli/src/cli.rs`: add `Explorer(ExplorerArgs)` as a `Subcommand` container holding
  `live` / `week` / `leaderboard` / `validate` sub-args.
- `cli/src/commands/`: add `explorer.rs`, register module + dispatch in
  `commands/mod.rs`.

### Phase 4 — implement sub-commands
- `explorer live` — pending capture + structured stream + optional `--compare`.
- `explorer week` — aggregate over window from store/JSON.
- `explorer leaderboard` — sender/token rollups.
- `explorer validate` — canonical_id cross-validation vs `run`/`live`.

### Phase 5 — tests
- Unit: aggregation helpers, leaderboard, period bucketing, validate matching.
- CLI: one test per sub-command, mirroring existing `cli/tests/*`.
- Cross-validation fixture: same block range from `run` and `live` → expect `validate` to
  report high match rate and sane profit deltas.

### Phase 6 — chain rollout + verification (Avalanche first, then others)
- **Avalanche**: full pass — `explorer week`/`leaderboard` vs explorer.mev.zone public stats;
  `explorer live` (expect sparse feed: shallow public mempool); `explorer validate` on
  settled-block ranges.
- **Polygon / BSC**: internal cross-validation (`validate` + spot checks on polygonscan /
  bscscan). No public per-chain MEV explorer exists for direct parity.
- **Ethereum**: internal cross-validation; sanity reference against public MEV dashboards (not
  as a data source).
- (Optional) Per-chain EVM spec tables for accurate historical replay (Part D, caveat 1).

### Phase 7 — docs
- Brief usage section (only if explicitly requested).

---

## Open questions / decision points (confirm before implementing)

1. **Storage strategy**: persist opportunities into the SQLite `opportunities` table
   (recommended — enables queryability + period rollups), or stay JSON-driven by scanning
   `export_path`? Recommendation: SQLite table **and** keep existing JSON export.
2. **Sender resolution**: leaderboards need tx `from`. Rely on cached receipts (no extra RPC)
   vs resolve lazily. Recommendation: cached receipts first, lazy fallback only if missing.
3. **Scope of `week`**: aggregate from persisted results only, or should `explorer week`
   also *run* backtests on demand over N days (like `run --days N`) when no results exist?
   Recommendation: aggregate persisted results first; add on-demand backfill as an option.
4. **Cross-validation input**: restrict `validate` to `run` vs `live` `ResultsFile`s, or allow
   arbitrary N result files over a range? Recommendation: arbitrary N, with a default of
   the two most recent `run` + `live` files.

---

## Key file references

- `core/data/chains.toml` — chain config (polygon etc.)
- `core/src/types/opportunity.rs` — `MevOpportunity`, `ResultsFile`, `compute_canonical_id`
- `core/src/types/strategy.rs` — `Strategy`, gas/flash-loan models
- `core/src/cache/` (`store/*`) — `SqliteStore` (add `opportunities` table here)
- `core/src/replay/replayer.rs` — `spec_id_for_block` (per-chain EVM spec, Polygon-only today)
- `core/src/pipeline/aggregate.rs` — per-run aggregation to extend
- `core/src/mev/mempool.rs` — pending-tx capture for `explorer live`
- `cli/src/cli.rs`, `cli/src/commands/mod.rs` — CLI wiring to extend
- `cli/src/commands/live.rs` — reference for the live feed implementation
- `cli/src/commands/run.rs`, `cli/src/commands/report.rs` — reference for results I/O
- Existing docs: `CLI_EXECUTION_PLAN.md`, `mev_strategies.md`
