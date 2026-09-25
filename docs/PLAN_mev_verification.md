# Verification hardening plan — detectors, paper, pipeline

**Goal:** extend the explorer verification treatment to the other money-claiming modules —
a pure per-op profit verdict for detectors (A), paper↔executed-profit reconciliation (B), a
fixed cross-check + wiring for pipeline outputs (C), and a gated real-block corpus (D).

Companion plan: `docs/PLAN_explorer_verification.md` (explorer — implemented). This plan
reuses its three-part structure (pure offline verdict fn → re-exposed cross-check report →
gated real-block corpus) and its machinery where possible:
`trace_verdict` (`core/src/jobs/trace.rs:68`), `explorer validate` T1/T2/T3
(`core/src/explorer/validate.rs:316`), and the `explorer_corpus.rs` harness.

All three sections below assume the conventions already chosen for explorer:
- **Strictness:** assert-style — non-zero exit on `Fail`, configurable tolerance.
- **Test tiers:** hybrid — offline/deterministic golden tests + `MEV_SCOUT_E2E=1`+`RPC_URL`-
  gated real-block corpus (gate via `core/tests/common/setup.rs:28-33`).

---

## A — Detector profit verification gate (`mev_verdict`)

Today detector `expected_profit` is validated only by filters (zero/gas/min-profit,
`pipeline/runner.rs:183-222`). No realized-vs-expected check exists outside the explorer
trace path. The explorer `show --trace` verdict is realized-only (RPC-side, per executed tx)
and never computed for a detector opportunity.

### Changes (`core`)

1. New `core/src/mev/verdict.rs`:
   - `mev_verdict(expected_net_wei, realized_net_wei, err_pct, tolerance_pct, abs_wei_tol)
     -> MevVerdict` where:
     - `Pass` if `|err_pct| <= tol` (or, if `expected_net ~= 0`, `|exp - realized| <= abs_wei_tol`)
     - `Fail(reason)` if `|err_pct| > tol` (classifier over/under-estimate; positive = over-estimate)
     - `Unverifiable(reason)` when no realized figure exists (no tx_hash matched, unpriced op,
       missing trace) — **not** a fail, degraded coverage.
   - `expected_net_wei = expected_profit - gas_cost_wei` (identical arithmetic to
     `core/src/paper/ledger.rs:61-65`).
   - Realized source per op: explorer `MevOpRow.net_profit_usd` /
     `details_json.trace_native_delta_wei` joined by `tx_hash` (`ops_for_tx`,
     `core/src/explorer/store.rs:1089`; `ops_in_range` at `:1106`).
2. `core/src/config/settings.rs`: add `ExplorerConfig.mev_tolerance_pct` (default **20.0**)
   and `mev_error_usd_tol` (default **0.50**), same pattern as
   `trace_tolerance_pct`/`trace_error_usd_tol` (`settings.rs:210-245`).
3. Unit-test `mev_verdict` exhaustively: pass / over-est / under-est / zero-expected abs-band /
   no-realized → Unverifiable (model the five-test matrix of `trace.rs:380-445`).

### Changes (`cli`)

4. `cli/src/commands/explorer/show.rs`: where the tx is a detector op, run `mev_verdict`
   against the matched realized op and print the verdict.
   **Exit non-zero via `anyhow::bail!`** on `Fail`; `Unverifiable` prints a warning.
   Reuse the existing `--tolerance-pct` override flag (`cli/src/cli.rs:123-125`).
5. Offline determinism lives in the core verdict tests (step 3); the per-tx CLI path needs a
   warm cache + realized op, so gate any CLI assertion under `MEV_SCOUT_E2E=1`.

---

## C — Pipeline cross-check repair + re-exposure (smallest, do first)

### C.1 — Fix production T2 case bug (prerequisite for everything)

`strategy_to_kind` (`core/src/explorer/validate.rs:29-38`) matches **PascalCase**
(`"TwoHopArb"`, `"Sandwich"`, ...), but production persists **snake_case**:
`Strategy` uses strum `Display` (`core/src/types/strategy.rs:71-82`) and is written via
`opp.strategy.to_string()` (`core/src/explorer/results.rs:40`). Consequence: T2's
`matching_strategy` resolves `None` for every production row — matches are silently skipped.

- Normalize case in `strategy_to_kind` (e.g. `s.to_lowercase()`, or `Strategy::from_str`
  fallback).
- Regression test seeding the strategy through the production serializer path
  (`results.rs:40`) and asserting T2 matches.
- Note: the T1 join also depends on shared re-keying between `MevOpportunity.canonical_id`
  and `explorer_canonical_id` (`canonical.rs:14-16,23-88`); keep the existing test-driven
  re-key, don't widen scope.

### C.2 — Golden offline opportunity fixture

`BacktestRunner::run_block` (`core/src/pipeline/runner.rs:401`) is deterministic given a warm
cache + pinned gas model. But `GasModel::Distribution` feeds `percentile_gas_price` from prior
blocks (`runner.rs:949`), so single-block determinism requires pinning `GasConfig`
(`historical_exact` or an explicit `GasCalibrationSnapshot`, (`types/gas.rs:59,111-124`)).

- Assert `run_block` output (kind present, `expected_profit > gas_cost_wei`, canonical ids
  well-formed) on a fixed synthetic cache block via `common::setup::prep_synthetic_cache`
  (`core/tests/common/setup.rs:204-289`) + `make_synthetic_runner`.
- Extend the existing suites in `core/tests/replay.rs` (`test_runner_run_block_synthetic`).

### C.3 — Wire `pipeline/aggregate.rs`

`aggregate.rs` computes P&L summary / ROI but is `#![allow(dead_code)]`
(`core/src/pipeline/aggregate.rs:4`) — nothing calls it (roadmap WS-F / gap G7,
`roadmap_to_100pct.md:110,227`).

- Surface `SummaryMetrics`/`StrategyMetrics` through `report` / `paper stats` output so the
  P&L summary path is covered by the validate cross-check.
- Cover `aggregate()` with offline unit tests (dedup by canonical_id, ROI formula `:262-266`).

---

## B — Paper ↔ executed-profit reconciliation

Paper today is pure arithmetic over analytic quotes. `policy.apply(&opps)`
(`core/src/jobs/paper.rs:107`) never sees an executed result; gas is the *modeled*
`gas_cost_wei` (estimated, `types/gas.rs:111-124`), never revm gas. Roadmap gaps G5/G6 and
WS-E/WS-F (`roadmap_to_100pct.md:199-231`) are exactly this missing ground truth. Verification
needs the executed figure to exist first.

### B.1 — Minimal what-if executor (WS-E Stage 1 slice)

New `core/src/replay/whatif.rs`:
- Build post-state `CacheDB<CachedRpcDb>` via `BlockReplayer::replay_to` (`replayer.rs:580`)
  (or the `replay_each_filtered` DB at `:640`, today ignored — `runner.rs:478`).
- Inject candidate bundle `TxEnv`, run with `transact` (retain journaled state; the current
  `transact_commit` in `build_executed_tx`/`exec_or_revert` throws away `ResultAndState`,
  `replayer.rs:349-510`).
- Read wallet balances via `CachedRpcDb` `DatabaseRef::basic_ref` (`replay/db.rs:231`) to
  compute per-address deltas + gas → **executed net per op**.
- Deliver `ExecutedNetMap { tx_hash / canonical_id -> (net_wei, gas_used, status) }`.

Nothing like this exists repo-wide: the only balance-delta extraction today is log-based
(`explorer/profit.rs::DeltaLedger::from_transfers`) and RPC-trace-based
(`jobs/trace.rs::parse_prestatediff_deltas`) — neither applies to a hypothetical fill.

### B.2 — Reconciliation verdict

- `paper_vs_executed(ledger: LedgerResult, executed: ExecutedNetMap) -> ReconReport`,
  `Pass/Fail/Unverifiable` per fill, same strict convention as A.
- `Unverifiable` when a fill has no executed counterpart (e.g. mempool-only op never on-chain),
  **not** a fail.

### B.3 — Gated reconciliation corpus

New `core/tests/paper_corpus.rs`: for a real window, run paper (`job_paper_sim`, loading via
`store.opportunities_by_run`, `explorer/store.rs:1183-1220`) vs what-if-executed nets
(available via `core/tests/explorer_corpus.rs`-style ingest); assert derived facts
(recon pass-rate ≥ floor per kind, every fill has `status=true` + `gas_used>0`, fills ⊆
executed set). This is the roadmap M2 reconciliation slice (`roadmap_to_100pct.md:291`) and
WS-F's done-definition (`:229-231`).

### B.4 — Roadmap leftovers to note, not block on

- Un-hardcode `winning_bid_premium` (hard-set `0.0` at `core/src/jobs/run.rs:151` and
  `core/src/jobs/live.rs:91`).
- Liquidation is excluded from paper (`is_native_eligible`, `ledger.rs:41-50`) — keep as-is,
  document in the corpus expectations.

---

## D — Real-block detector corpus (`core/tests/mev_corpus.rs`)

1. Clone the `explorer_corpus.rs` harness (`core/tests/explorer_corpus.rs`, `CorpusCase`
   at `:41-50`):

   ```
   CorpusCase { id, chain, from_block, to_block, kind, min_ops,
                searcher: Option<Address>, expected_verdict_rate: Option<f64> }
   ```

2. Harness gated on `MEV_SCOUT_E2E=1` + `RPC_URL` (reuse `setup.rs:28-33`):
   `Fetcher.fetch_range` → SqliteStore cache → `PoolManager::init_from_rpc` →
   `BacktestRunner::run_block` (or `run_range`), then assert per case.
3. Deterministic single-block runs require the pinned gas config from C.2.
4. Assertions are derived facts (kind present, min-op floor, searcher filter, verdict-pass
   rate band), **never exact profit amounts** — same risk note as explorer §D.
5. Seed cases by hand-picking windows on Avalanche/Polygon after a live run; deliberately
   include the **sandwich** and **jit** kinds — the gap in `explorer_corpus.rs:22-25` — and
   prefer windows overlapping the explorer corpus so the T1 cross-check (`validate`) is
   exercisable on the same blocks.

---

## Open decisions (to confirm before implementation)

1. **B scope:** build the what-if executor (B.1) as part of this plan, or defer B entirely to
   the WS-E/WS-F roadmap slice and ship A + C + D first? B is the largest item — it creates
   ground truth that does not exist today. Recommend: A → C → D now, B as its own slice.
2. **A realization:** for ops with no matched realized row, join on `tx_hash` only, or also
   fall back to canonical-id lookup in `ops_in_range`? Recommend tx_hash only (strictest).
3. **D rate floor:** assert per-kind `expected_verdict_rate` ≥ some floor, or per-op
   `Unverifiable`-tolerance only? Recommend a loose floor (e.g. ≥ 50% verifiable) to catch
   systemic mapping regressions without flaking on sparse windows.

Suggested implementation order: **C.1 (bug fix — unblocks T2) → C.2 → A → D → B**.