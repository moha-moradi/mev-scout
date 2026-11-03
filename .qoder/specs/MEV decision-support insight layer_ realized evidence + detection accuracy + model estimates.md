# MEV decision-support insight layer: realized evidence + detection accuracy + model estimates

**خلاصه (فارسی):** هدف، «بات شبیه‌سازی‌شده» نیست؛ هدف این است که کاوشگر بتواند بگوید **کدام استراتژی، روی کدام استخر، با چه سود خالص و چه هزینه‌ای (gas + کارمزد وام آنی + تمرکز رقابت) ارزش انجام دارد.** امروز این سؤال پاسخ داده نمی‌شود، چون هزینه‌ها در سطح استخر جمع نمی‌شوند، استخر بُعد مستقلی ندارد، مخرجِ سرمایه (TVL) در دیتابیس دیگری و اغلب خالی است، و هیچ‌جا اندازه گرفته نمی‌شود که آیا کاشفِ ما فرصت را *قبل از وقوع* دیده بود یا نه. این پلن یک لایه‌ی insight سه‌تکه می‌سازد: (۱) شواهد محقق‌شده، (۲) دقت کشف (precision/recall/kalibration)، (۳) تخمین مدل‌محور. همه تک‌زنجیره، بدون امتیازدهیِ وزن‌دار، با خروجی JSON/CSV؛ UI فاز آخر است.

## Context

Two earlier artifacts were reviewed against this goal and **neither reaches it**:

- The capability review (bot feasibility) is diagnostic only — its value here is that it proved detector `expected_profit` is an unverified claim, which makes it unsafe as a decision input without a realism tier.
- The previous P0–P3 "verified paper fills" plan improves one input's honesty but ships no aggregation, no pool dimension, no cost rollup, and no cross-run evidence — so it cannot answer "where should I operate?". It is retained here only as **Stage 3**, repositioned from "P&L headline" to "estimate column".

Hard limits verified in the code today (all file:line from this session):

- Pool aggregates return `StatsRow{ops, gross_usd, net_usd}` only — no volume/gas/flash-fee per group ([store/query.rs:156–229](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/query.rs#L156)), and the CSV writer blanks the cost columns ([commands/explorer/report.rs:361–370](file:///D:/gitlab.dte.repo/mev-scout/cli/src/commands/explorer/report.rs#L361)). `mev_ops` does carry `gas_cost_usd` (schema.rs:352) and `flashloan_fee_usd` (:84), so the data exists but is never rolled up per pool.
- Pools are not a dimension: they live inside `mev_ops.route_json` and are parsed per query via `json_each` ([query.rs:166–168](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/query.rs#L166)); there is no pool table in the explorer DB.
- Capital denominator is cross-DB and mostly empty: `pool_info(...tvl_usd, volume_usd_24h, volume_usd_30d, fee, dex_type)` is in the **cache** DB ([cache/store/mod.rs:149–160](file:///D:/gitlab.dte.repo/mev-scout/core/src/cache/store/mod.rs#L149)), filled only by `--enrich` from GeckoTerminal/DexScreener ([jobs/discover.rs:99–145](file:///D:/gitlab.dte.repo/mev-scout/core/src/jobs/discover.rs#L99)), and **no explorer command reads it**.
- No candidate↔realized comparison exists: no query touches `opportunities` and `mev_ops` together, and the two `canonical_id` schemes are incompatible except for backrun ([explorer/canonical.rs:14–16](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/canonical.rs#L14) calls exact matching "aspirational"; detector form at [types/opportunity.rs:157–162](file:///D:/gitlab.dte.repo/mev-scout/core/src/types/opportunity.rs#L157)).
- Columns that already exist in schema but are never written, which directly starve ranking: `swaps.dex`, `swaps.sender` (insert omits them, [store/facts.rs:249–253](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/facts.rs#L249)), `transfers.is_native`, `blocks.producer` (schema.rs:276–279).
- Quality is blended by default: `Confidence{Exact,Inferred}` and `PnlBasis` are mixed into every aggregate unless `--confidence` is passed (:255–263), `pnl_basis` is stored inside `details_json` and re-parsed per row (:216–226), and `config.explorer.mev_tolerance_pct` is used only by offline gates — never surfaced in explorer output.
- Only daily granularity and `MAX`/`SUM` — no p50/p90, no win-rate, no concentration.

**Scope decisions (confirmed):** three evidence tiers; single chain per invocation (no cross-chain rollup, `mev_ops` keeps no `chain` column); descriptive metrics and ranked tables only, **no weighted score**; deliver the data + CLI first, HTML dashboard last.

## Architecture

One economics query layer, three evidence columns that are never summed together.

```
Tier 1 REALIZED   mev_ops + op_pools + pool_info(TVL)      → what actually paid, per pool/kind/pair/dex
Tier 2 DETECTED   opportunities ⟷ mev_ops match table      → did we see it first, and was our number right
Tier 3 MODEL      paper/sim.rs state-exact re-quote         → best estimate for pools with thin history
                    (arb only; labeled model, never blended)
```

**Attribution rule (the decision that keeps the numbers honest).** A multi-leg op touches several pools; grouping by `json_each` counts that op's net once per leg, which silently inflates every pool total. Adopt one rule and encode it in the table: `op_pools.role = 'primary'` for the leg carrying the largest `amount_in` USD-equivalent, `'leg'` for the rest. Money columns (`gross/gas/flash/net`) aggregate **primary only**; touch-level metrics (`ops`, `distinct pools co-occurring`, `searchers`) aggregate **all roles**. Every row of output prints which basis it used.

## Stage 0 — cheap fixes to existing gaps (no behavior risk)

**Files:** [core/src/explorer/store/facts.rs](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/facts.rs), [core/src/explorer/decode/swap.rs](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/decode/swap.rs), [cli/src/cli.rs](file:///D:/gitlab.dte.repo/mev-scout/cli/src/cli.rs), [cli/src/commands/explorer/report.rs](file:///D:/gitlab.dte.repo/mev-scout/cli/src/commands/explorer/report.rs)

- Write the columns the schema already declares: `swaps.dex` (from the resolved `Amm`/`DexType`), `swaps.sender` (already on `SwapFact`'s tx), `transfers.is_native`. These unlock DEX-level and sender-level ranking without any migration.
- Add `--format table|csv|json` to explorer args (today format is reachable only through `config.output.output`, [types/strategy.rs:341–358](file:///D:/gitlab.dte.repo/mev-scout/core/src/types/strategy.rs#L341)) so the insight layer is machine-readable for the later UI.
- Fix the CSV cost blanking (:361–370) and print the tolerance/`--confidence` basis in every report header so a reader knows whether a number is exact or inferred.

**Done when:** a re-index of one existing window shows non-NULL `swaps.dex`/`sender`, and CSV/JSON carry the same cost columns the table does.

## Stage 1 — realized economics per pool / kind / pair / dex

**Files:** [core/src/explorer/store/schema.rs](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/schema.rs) (migration `v11_op_pools`, appended to `MIGRATIONS` :27–38, using `ensure_column`/`table_exists` at :665/:69), [store/facts.rs](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/facts.rs), [store/query.rs](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/query.rs), [store/mod.rs](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/mod.rs) (`StatsRow` :95–100), new [core/src/explorer/store/economics.rs](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/economics.rs), new [cli/src/commands/explorer/economics.rs](file:///D:/gitlab.dte.repo/mev-scout/cli/src/commands/explorer/economics.rs).

- `op_pools(op_id, pool, leg_index, role, amm, token_in, token_out)` + index `(pool, op_id)`, written at persist time next to `route_json` (facts.rs:301–311), plus an idempotent **backfill** from existing `route_json` via `json_each` so already-indexed history is rankable.
- `EconomicsRow` — per group (`pool | kind | subkind | pair | dex`), over a window: `ops`, `active_days`, `days_hit/days_seen` (recurrence), `volume_usd`, `gross_usd`, `gas_usd`, `flash_fee_usd`, `net_usd`, `net_p50_usd`, `net_p90_usd`, `share_positive_ops`, `distinct_searchers`, `top_searcher_share`, `unpriced_pct`, `flash_fee_unpriced_pct`, `exact_pct/inferred_pct`, `tvl_usd`, `volume_usd_30d`, `net_per_tvl_pct`, `tvl_coverage`. Percentiles computed in Rust from an ordered row set (SQLite has no `percentile_disc` here) — no new dependency.
- TVL/volume joined in Rust from the cache DB `pool_info` by address (read via [cache/store/pools.rs:23/55](file:///D:/gitlab.dte.repo/mev-scout/core/src/cache/store/pools.rs#L23)); when missing, the row prints `net_per_tvl = n/a (tvl not enriched)` rather than a zero, and the command prints overall `tvl_coverage`.
- `mev-scout explorer economics [--window 7d] [--group-by pool] [--tier realized] [--kind …] [--confidence …] [--min-ops 3] [--top 25] [--format …]`, defaulting to a table sorted by `net_usd` with the quality columns visible; `--min-ops` protects against ranking a pool that had one lucky op.
- Reuse `report_by_kind`/`top_pools_filtered` SQL shapes rather than replacing them; leave `explorer` (revenue report) untouched so existing habits keep working.

**Done when:** one command, one chain, answers "top-20 pools by realized net after gas and flash fee, with how often they recur, how concentrated the searchers are, and how much of the sample is exact — plus profit per unit of TVL where TVL is known."

## Stage 2 — detection accuracy: did we catch it, and was our number right

**Files:** new [core/src/explorer/match.rs](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/match.rs), [core/src/explorer/canonical.rs](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/canonical.rs), [core/src/jobs/live.rs](file:///D:/gitlab.dte.repo/mev-scout/core/src/jobs/live.rs) + [jobs/report.rs](file:///D:/gitlab.dte.repo/mev-scout/core/src/jobs/report.rs), schema migration `v12_detection_matches`.

- **Match key, not canonical_id.** Realized and detector ids disagree by construction, so match on a coarse, both-sides-computable key: `(chain, family, pool_set_hash, profit_token, block_number ∈ [b-Δ, b])` where `family` collapses `MevKind`↔`Strategy` (arb-family: `ArbAtomic` ↔ `TwoHopArb|MultiHopArb`; `Jit` ↔ `Jit`; `Backrun` ↔ `Backrun` — the one pair that already aligns, opportunity.rs:165–167) and `Δ` defaults to 3 blocks, configurable. Add `explorer::canonical::family_of(kind)` and `types::Strategy::family()`; keep `explorer_canonical_id` unchanged for exact-match cases and record which method produced a hit (`exact | coarse | none`).
- New table `detection_matches(opp_canonical_id, mev_op_id, method, delta_blocks, expected_profit_wei, realized_net_usd, detected_ts_block, realized_tx_index, created_at)`, populated by a `match_runs(store, cache, window)` pass that can be run on demand and after every `live --loop` session (called from `LiveContext::persist_results`, reusing `take_rejections`-style drain precedent [pipeline/runner.rs:334](file:///D:/gitlab.dte.repo/mev-scout/core/src/pipeline/runner.rs#L334)).
- Normalize wei→USD with the locally stored hourly price table (`prices(hour, token, usd)`, hour bucket at [explorer/pricing.rs:258–261](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/pricing.rs#L258)) so candidate wei and realized USD are comparable; emit `price_source = local | missing`.
- Metrics per strategy family: **precision** (candidates with a realized match ÷ candidates), **recall over the recorded window** (realized ops we had emitted a candidate for, before their `tx_index`), **calibration** (distribution of `realized_net / expected_profit`, p50/p90), **lead time** (blocks between our detection and the realized tx), and `rejected_candidates` used as the "we saw it and refused" denominator (`--tier detected` reads them via [explorer/reject.rs](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/reject.rs)).
- **Truth that must be in the UI text:** recall is only computable forward. History that was never scanned has no candidate rows, so the first runs show precision and calibration but `recall = n/a (no candidate stream recorded before <block>)`. To make it accumulate, `[live].record_rejections`/`record_rejections` becomes default-on for loop sessions and `live --loop` is documented as the observation role.
- Surface as `--tier detected` rows in the same `explorer economics` table, so a pool can read "high realized net, low recall" = your detector is the bottleneck, not the opportunity.

**Done when:** one table shows, per strategy family, precision/recall/calibration/lead-time over a window, with `n/a` printed honestly where no candidate stream exists.

## Stage 3 — model tier (the former P0–P3, re-scoped as an estimate)

Keep the state-exact simulator, but its output is an **estimate column**, never a P&L headline: re-quote each candidate's own legs over a scratch copy of the pool state it was detected on (`quote_swap_post`/`apply_post_state` built from the tick-tracking loop in `core/src/pool/math/v3.rs`, `PoolManager::subset`, reusing `two_hop_profit_at` [two_hop.rs:255](file:///D:/gitlab.dte.repo/mev-scout/core/src/mev/detectors/two_hop.rs#L255) and `walk_quote` [multi_hop.rs:467](file:///D:/gitlab.dte.repo/mev-scout/core/src/mev/detectors/multi_hop.rs#L467)), consume liquidity across candidates ordered by `tx_index`, and enforce `out ≥ borrowed + flash_loan_fee(borrowed)` with new `FillSkipReason` arms (`SimRepayShortfall`, `SimPriceConsumed`, `SimPathUnavailable`, `SimNotQuotable`) instead of the `PoolConflict` set heuristic ([paper/ledger.rs:183](file:///D:/gitlab.dte.repo/mev-scout/core/src/paper/ledger.rs#L183)). Seeds captured at `finalize_block_opportunities` (:447), `sync_block_from_logs` (:852) and [mempool.rs:58](file:///D:/gitlab.dte.repo/mev-scout/core/src/mev/detectors/mempool.rs#L58); `LedgerPolicy::apply_simulated` added beside `apply`, with `--sim state` opt-in in `live` (default unchanged) and session-wide seed accumulation so `update_ledger`'s recompute-from-scratch invariant holds. Persist `est_*` columns and expose them as `--tier model`, showing `n/a` for `Jit`/`Backrun`/liquidation, which the simulator cannot price.

## Stage 4 — UI (deferred, contract fixed now)

Static HTML generated from the `--format json` output of Stage 1–3 (no server, no new dependency). Because the JSON contract is defined in Stage 1–2, the UI adds no query work. Ranked tables, per-row tier badges, quality coverage bars, and an explicit "these tiers are not summable" header.

## Verification

1. `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`.
2. **Attribution invariant (the one that catches inflation):** for a window, `SUM(net_usd) over op_pools WHERE role='primary'` equals `report_window_overview` net. A test asserts it; a regression here would silently make every pool look richer.
3. Unit tests: `op_pools` backfill from hand-written `route_json` (reuse `test_fixtures.rs` under the `test-util` feature); percentile helper against known vectors; `family_of`/`Strategy::family` mapping table; `net_per_tvl` when TVL is NULL → `n/a`, not 0; match key stable under pool-order permutation; coarse matcher pairs a synthetic candidate with a synthetic realized op at `Δ=3` and rejects at `Δ=4`.
4. Offline integration: `core/tests/economics_corpus.rs` built on `prep_synthetic_cache` ([tests/common/setup.rs:220](file:///D:/gitlab.dte.repo/mev-scout/core/tests/common/setup.rs#L220)) + `pools_from_swaps` seeding, asserting the three tiers produce three independent columns and that `--min-ops` filters singletons. Network paths stay behind `MEV_SCOUT_E2E=1` + `RPC_URL` (setup.rs:28).
5. End-to-end on real data, single chain: `mev-scout explorer index --days 7` → `mev-scout explorer economics --group-by pool --window 7d --format json > econ.json` → `--group-by kind`, `--group-by pair`, `--group-by dex` → `mev-scout live --loop --duration 30m` (records candidates) → `economics --tier detected` and confirm precision/calibration appear while recall reads `n/a` with the honest reason; re-run a week later and confirm recall becomes computable.
6. Spot-check against reality: for the top-3 pools, `mev_ops.tx_hash` of the largest realized net should exist within `--window` of a matching candidate once a candidate stream exists — manually inspect the first matches to confirm the coarse key isn't matching noise.

## Out of scope

Cross-chain comparison (one DB per chain retained; `mev_ops` gets no `chain` column; a `--chains all` rollup would need schema.rs:22–26's documented rebuild, which this plan deliberately avoids). Weighted attractiveness scoring (docs/mev_strategies.md:2525+ stays prose; the layer outputs metrics, you decide). Liquidation revival (retired from execution, [types/strategy.rs:82–88](file:///D:/gitlab.dte.repo/mev-scout/core/src/types/strategy.rs#L82)) and its ranking. Builder-tip/MEV-boost cost attribution (currently discarded as `builder_tip` rejects, so per-op cost stays partial — flagged, not modeled). DEX/LP swap fees as an explicit cost line (no column in `swaps`). Any signing/broadcast.

## Honest residual gaps

- Stage 1 gives you *where the money actually went*; it cannot tell you *whether you could have taken it* — that is Stage 2, and Stage 2 is forward-only: expect weeks of `live --loop` recording before recall means anything.
- `net_per_tvl` is only as good as `--enrich` coverage; the coverage column is mandatory in every output so a TVL-blind pool never looks efficient by accident.
- Gas per op is USD-only (`gas_cost_usd`); wei lives in `txs` and is never joined to `mev_ops`, so cross-chain gas comparison (if ever wanted) needs that join first.
- Tier 3 remains a quote-level model: no revert semantics, no fee-on-transfer tax inside quotes (`is_fot_pair` is a filter), no hook/Curve/V4 execution fidelity, and own-fill overlays are additive at each candidate's own `tx_index`, not globally order-consistent.
- Coarse matching can produce false positives on very active pools; `method` and `delta_blocks` are exposed per row so a reader can discount `coarse` hits, and the recall figure should be read as a band until enough exact-id (backrun-style) alignment exists.