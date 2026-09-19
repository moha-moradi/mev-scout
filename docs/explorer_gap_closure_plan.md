# Explorer Gap-Closure Plan

Single merged plan for the explorer module (realized-MEV forensics). This is the
implementation plan derived from the MEV specs (and subsumes the former
`docs/EXPLORER_ACCURACY_PLAN.md` content).

Reference specs:
- `docs/mev_detection_engine_spec.md`
- `docs/mev_opportunity_detection_and_pnl_spec.md`

Scope decisions (agreed): keep explorer **logs-only** for classification (no
REVM, no traces as classifier input), add **heuristic Backrun/Frontrun**
detectors, extend P&L with **flash-loan netting only** (slippage/protocol-fee/
builder-payment stay opportunity-side).

**Traces in Phase 0 are measurement-only** — offline reconciliation against
already-classified ops. They never feed `classify_block`.

---

## Cross-cutting rules (apply from day one)

### Kind priority / mutual exclusion
Sandwich already encodes front + victim(s) + back. Without exclusion, the same
legs will also match Frontrun and Backrun.

The list below is **kind precedence, not `classify.rs` pass order**. Pass order
stays as-is (liquidation first, then swap attribution, arb, sandwich, JIT, …);
the priority list only resolves which kind a tx/leg keeps when more than one
pass could claim it. First match wins; consumed legs/txs are removed from later
passes:

```text
Sandwich > Frontrun > Backrun > ArbAtomic > JitArb > Jit > Liquidation > Unknown
```

Update `kind_order` in `classify.rs` accordingly when new kinds land.

**JitArb is an upgrade, not a separate claim:** the Mint tx that also closes an
arb cycle upgrades the arb event to `JitArb` inside the arb pass
(classify.rs:218-222), independent of priority-list consumption; the ordering
`ArbAtomic > JitArb` only means a plain arb tx is never labeled `JitArb`.

### In-tx P&L pipeline order
Inside classify → persist for every searcher tx:

```text
ledger build
  → flash-loan netting (2.2)
  → select / emit profit tokens (incl. multi-token, 2.3)
  → real gas (2.1)
  → net_profit_usd = Σ token_usd − gas_usd − flashloan_fee_usd
```

### Arithmetic rules (§42)
Ledger balances, per-token residuals and net sums stay integer/fixed-point
(U256 wei; USD conversion only at persist/reporting). `f64` is allowed **only**
inside `details`-level evidence (victim degradation %, `fees_estimated`,
tick fractions) — never for accounting amounts.

### Reindex after classifier changes
Any change to classify/decode/P&L invalidates existing `mev_ops` rows for the
affected window. Before/after Phase-0 numbers are only comparable after a
reindex:

```bash
# Preferred for forensic DB: wipe ops for the window and reclassify from stored
# facts (blocks/txs/transfers/swaps) when that path exists; else:
explorer index --from <N> --to <M>   # or --days N after clearing mev_ops /
                                     # blocks_classified for that range
explorer validate --threshold-sweep --emit-missing-pools
```

Document the exact wipe+reindex recipe in `ARCHITECTURE.md` §4.11.

### Per-phase ship gates
Beyond `cargo test` / clippy: re-run the Phase-0 fixed-window recipe and record
delta. Do not merge a phase that regresses USD-recall or profit MAD without an
explicit documented trade-off (see 1.2 gate for the arb_likely cliff).

---

## Phase 0 — Measurability first (baseline)

Establish ground truth so every later change has a before/after number.

> **Status: code landed** (trace reconciliation, validate profit-error
> aggregation + review queue, `--review-csv`; wipe+reindex recipe + baseline
> template documented in ARCHITECTURE.md §4.11). **0.5 synthetic labeled set
> done** (`explorer::golden` + `explorer validate --golden-causal`). Remaining:
> **0.4 baseline run** (real numbers, needs RPC); chain-curated expansion of
> the 0.5 set beyond the synthetic CI fixtures.

1. **Trace reconciliation** (measurement only) — `core/src/jobs/trace.rs:86`
   stores a summary string and passes `trace_profit_usd = None`; nothing
   compares.
   - Compute the classifier's expected USD profit for the tx (event
     `profit_amount` × hourly price at block ts) and the trace's actual native
     delta (`summarize_prestatediff`, trace.rs:29).
   - Persist both plus a `profit_error_pct` via `mark_trace_verified`
     (store.rs:1365) — the field already exists.
2. **Profit-error aggregation in `validate`** — extend `TierRecall` /
   `ValidationReport` (validate.rs:142,185) with per-kind µ/MAD of
   `profit_error_pct` over trace-verified matches.
3. **Precision axis** — realized-side review queue: count `inferred` arb/unknown
   ops per kind as review candidates; keep scanner-side `precision_signal_count`
   as is. Optional `--review-csv` export.
4. **Baseline run + doc** — fixed-window recipe (e.g. Polygon last 7d):
   `explorer index --days 7` → `explorer validate --threshold-sweep
   --emit-missing-pools`, record recall / USD-recall / miss-taxonomy / MAD.
   Document in ARCHITECTURE.md §4.11. No hardcoded number in tests
   (data-dependent); test only report shape.
5. **Labeled golden set for Backrun/Frontrun** — opportunity-side has no
   mapping to these kinds, so validate can only emit `precision_signal`. Curate
   a small labeled set (≈10–20 blocks, hand-reviewed or external-explorer
   compared) used in Phase 6 and as the Phase-3 ship gate. Without this, Phase 0
   only measures arb/sandwich/liquidation/JIT.

**Expect:** report shape + persisted `profit_error_pct`; baseline numbers
checked into ARCHITECTURE.md (not into asserts).

---

## Phase 1 — Identification correctness

### 1.1 Registry-based swap direction (biggest lever)
> **Status: done.** `attach_swap_tokens` now takes `pool_tokens: &HashMap<Address,
> (Address, Address)>`, resolves sentinels from the registry (fallback to
> transfer pairing); `job_index` loads `cache.db::pool_info` once per run and
> threads it through `index_block`/`backfill_range`/`run_live`; V2+V3 registry
> unit tests added.
- Pass pool token0/token1 (from `cache.db::pool_info`; already loaded in the
  live job via `SqliteStore`) into `attach_swap_tokens` (decode.rs:390).
  Exact rules: V2/Solidly `amount0In>0 ⇒ token0 in`; LB `swapForY ⇒ token0 in`;
  Fluid `swap0to1 ⇒ token0 in`; V3/V4/Infinity/Metric sentinels mapped when the
  registry knows the pool. Transfer-pairing stays as fallback.
- Load the map once per range in `job_index` (core/src/jobs/index.rs:38) and
  thread through `index_block` (ingest.rs:112).
- Tests: V2 + V3 fixtures where transfer pairing is ambiguous but the registry
  resolves; assert the arb cycle is now detected.
- **Prerequisite for** the cycle generalisation (1.2) and for
  Backrun/Frontrun (Phase 3): unresolved swaps are silently dropped from
  cycle detection today (classify.rs:141-155).

**Expect:** more resolved edges; arb Exact recall ↑ on registry-known pools;
  miss-taxonomy shifts away from “unresolved swap”.

### 1.2 Arbitrage — generalized cycle detection (§8.1)
> **Status: code done** (incl. flow-ownership). `has_closed_cycle` (directed-graph
> DFS, ≥2 edges/≥2 distinct pools, edge-reuse guarded) replaces `is_closed_cycle`;
> `BlockInput` carries `arb_likely_parity` (default true, from
> `explorer.arb_likely_parity`); `filter_unresolved` is wired into `index_block`.
> **Flow-ownership (§7.1/§8.1) done:** `SwapFact.owner` is captured from the
> inbound transfer funding each swap (`attach_swap_tokens`, decode); the arb
> pass requires the closed cycle to be attributable to a single funder-owner
> that is a searcher candidate of the tx — `Exact` otherwise `unknown`, with an
> explicit unowned (recall) escape. Tests: `arb_cycle_single_flow_owner_is_exact`,
> `arb_cycle_mixed_flow_owner_is_not_exact`,
> `arb_cycle_owned_only_by_non_candidate_is_not_exact`.
> Remaining: ship-gate flip of `arb_likely_parity` is a later data step.
- `profit.rs:158` `is_closed_cycle` only detects a connected chain in log
  order. Add a proper directed-graph cycle walk over resolved swap edges
  (DFS from `edges[0].0`, returning to start, ≥2 pools) used by
  `classify.rs:141-151`. Multi-hop cycles whose swaps are interleaved then
  classify `Exact`.
- **Flow ownership (§7.1/§8.1):** the cycle must be attributable to one
  searcher's route — swap edges owned by an unrelated actor inside the same tx
  must not be mixed into a single closed cycle.
- **Remove the blanket `arb_likely`** (classify.rs:155):
  `arb_atomic`/exact only on a closed cycle ≥2 pools; unresolved/single-hop
  profitable residuals fall back to `unknown`. Wire the unused
  `filter_unresolved` (classify.rs:558) into ingest (ingest.rs:232) to
  suppress zero-net unknown noise.
- **Mevlive-parity flag + ship gate:** keep
  `explorer.arb_likely_parity = true|false` (default **true** until the gate
  passes). Flip default to `false` only when the Phase-0 window shows
  USD-recall drop ≤ agreed budget (document the number next to the baseline)
  **or** an explicit “precision over recall” decision is recorded. Do not
  land default=false on a blind cliff.

**Expect:** Exact arb on interleaved cycles; with parity=false, fewer false
  `arb_atomic` / more `unknown`; live feed quieter after `filter_unresolved`.

### 1.3 Liquidation — transfer reconciliation + real profit (§10.1)
- Cross-check `LiquidationFact.collateral_amount` / `debt_to_cover` against the
  tx's `DeltaLedger` (liquidator's collateral delta). Match ⇒ `Exact`;
  mismatch or missing transfer legs (Compound V3 `Absorb`) ⇒ `Exact` on event
  match with `TRANSFER_MISMATCH` reason.
- **Profit ≈ collateral_usd − debt_to_cover_usd**, both legs valued at persist
  time with hourly prices (store.rs:479). Do **not** subtract raw amounts when
  tokens differ. When either price is missing, or liquidator bonus / market-sale
  path makes the simple formula unreliable, mark `Inferred` + reason
  (`MULTI_ASSET_PRICING` / `LIQ_BONUS_APPROX`); keep collateral amount as
  display fallback. Recompute USD at persist, not in classify.
- Add **Compound V2 `liquidateBorrow`** to the decoder family — spec §10.1
  lists it explicitly; today only Aave V3 `LiquidationCall` + Compound V3
  `Absorb` are decoded.

**Expect:** profit_error MAD ↓ on liquidations that have both prices; Absorb
  path remains Exact with `TRANSFER_MISMATCH`.

> **Status: done.** Compound V2 `LiquidateBorrow` added to `decode_liquidation`
> (chain/events.rs topic + decode.rs branch). Liquidation pass in `classify.rs`
> builds a `DeltaLedger`, checks `reconciled`, and adds `TRANSFER_MISMATCH` when
> the collateral delta mismatches. `store.rs` `liquidation_pnl` computes
> `collateral_usd − debt_usd` using hourly prices and downgrades `Confidence` to
> `Inferred` for cross-asset (`LIQ_BONUS_APPROX`) or missing prices
> (`MULTI_ASSET_PRICING`); same-asset stays `Exact`. `event_tokens` now includes
> liquidation debt assets. Tests: `liquidation_compound_v2_decodes`,
> `liquidation_transfer_reconciled`, `liquidation_mismatch_records_reason`,
> `liquidation_pnl_cross_asset_is_inferred`, `liquidation_pnl_missing_price_falls_back`,
> `liquidation_pnl_same_asset_is_exact`. Verification: 55 explorer lib tests pass
> (post-1.3), full core + cli pass, clippy clean.

### 1.4 Sandwich — evidence + victim impact + gas (§11.2)
- Evidence block in `MevEvent.details` (`fold_sandwich`, classify.rs:359):
  `same_pool`, `direction_match`, `reverse_backrun`, `same_searcher`, plus
  logs-only victim degradation = relative diff of `victim` vs `front_run`
  execution prices (`amount_out/amount_in`). Recorded as evidence, **not** a
  classification gate (spec's own warning).
- Sum gas across front-run + back-run (currently back only); keep the full
  victim list (currently only last); tag contract-mediated legs in `details`
  when a labeled searcher is involved.
- Reason codes `REVERSE_DIRECTION`, `SAME_SEARCHER`,
  `VICTIM_EXECUTION_DEGRADED`. Keep both negative tests (plain round-trip,
  victim-before-front).
- **Profitability gate (§12.1.7):** require the sequence net > 0 after costs
  (front + back-run gas, flash-loan fee); add a negative test for a structured
  sandwich with negative net (victim too small vs combined gas).
- Consumed sandwich legs must not also emit Frontrun/Backrun (see kind
  priority).

**Expect:** sandwich gas closer to trace; evidence fields present; no double
  count with Phase 3 kinds.

> **Status: done.** `SandwichWalk` carries `victims: Vec<SwapLeg>` (full victim
> list + per-victim swap sizes); `fold_sandwich` now sums gas across front+back
> txs and emits an `evidence` block (`same_pool`, `direction_match`,
> `reverse_backrun`, `same_searcher`, `contract_mediated`), `victim_execution`
> degradation (`front_price`, `victim_price`, `degradation_pct`, logs-only,
> evidence gate never classifies), and `REVERSE_DIRECTION`/`SAME_SEARCHER`/
> `VICTIM_EXECUTION_DEGRADED`/`CONTRACT_MEDIATED` reasons. Contract mediation
> is inferred from `tx.to` on front/back (store-free). **Profitability gate:**
> `store.rs` drops a `Sandwich` when net (`profit − gas − flashloan_fee`) ≤ 0
> — never guesses on missing prices (`None` net is kept). Both negative tests
> (plain round-trip, victim-before-front) still hold; new store gate test
> `sandwich_loss_gate_skips_unprofitable` covers drop + keep paths;
> `sandwich_contract_mediated_legs_tagged` covers the contract tag.

### 1.5 JIT + JitArb — position-based + fee capture (§14/§15)
- **Persist** open Mint positions in SQLite (not process memory) keyed
  `(pool, owner, tickLower, tickUpper)` with opened_block, liquidity, and a
  block-window cap (e.g. 1,000 blocks); close on exact-liquidity Burn; emit
  block/tick range; prune stale. Recovers cross-block JIT across live restarts.
  Table lands in 5a-0 (executed immediately before 1.5).
- Add optional `tick: Option<i32>` to `SwapFact` (decode.rs), populated from
  V3/V4/Infinity swap data word 4 (decode.rs reads only 64 bytes today); keep
  transient (no `swaps` table migration). Require ≥1 in-window swap whose tick
  lands in `[tick_lower, tick_upper]` (validates range overlap).
- `fees_estimated` in details via documented range-fraction × volume ×
  pool-fee approximation, marked `Confidence::Inferred` (V3 fees aren't in
  logs; principal amounts aren't fees).
- Emit `SHORT_LP_LIFETIME` and `OVERLAPPING_TICK_RANGE` as reasons (§23); the
  block-window cap (>1,000 blocks) supplies the `SHORT_LP_LIFETIME` negative
  (§39 JIT reject: ordinary LP activity with long holding periods).
- `JitArb`: add `components: ["JIT","Arb"]` in details. **One composite event
  only** — when a tx upgrades to `JitArb`, the standalone `ArbAtomic` emission
  is suppressed so USD/P&L aggregates never double-count the same flow (§17).

**Expect:** cross-block JIT recall ↑ after restart; tick-overlap negatives hold.

> **Status: done.** `jit_open_positions` persisted via store methods
> (`record_jit_open`/`close_jit_position`/`open_positions`/`prune_jit_positions`,
> window `JIT_WINDOW_BLOCKS = 1_000`); `ingest` loads the window into
> `BlockInput.open_positions` and writes opens/closes/prunes per block;
> `reorg` deletes positions with `opened_block >= fork`. `SwapFact.tick` is
> populated from V3/V4/Infinity swap data word 4; `classify_jit` now pairs an
> in-block Mint→Burn **or** a prior-block open position with an in-block Burn,
> requires an in-window swap tick inside the range, emits `held_blocks` +
> `SHORT_LP_LIFETIME`/`OVERLAPPING_TICK_RANGE` reasons and a `fees_estimated`
> block (in-range volume × 30 bps, `confidence: "inferred"`); the detection
> event stays `Confidence::Exact`. `JitArb` carries
> `components: ["JIT","Arb"]` and suppresses the same-tx `ArbAtomic` to avoid
> double count. Known limitation: cross-block close uses exact-key + `>=`
> liquidity and cannot restore a position whose Burn is unwound by a reorg.
> Tests: `jit_open_positions_roundtrip`,
> `jit_cross_block_open_position_closed_by_burn`, `jit_requires_tick_overlap`,
> `jit_arb_upgraded_when_mint_tx_also_arbs`.

### 1.6 Decoders + generic cycle detector
- Add 0x/1inch/paraswap `Swapped` / `OrderFilled` topics to decode.rs.
- Add a transfer-graph cycle pass that upgrades profitable `unknown` candidates
  to resolved arb when a ≥3-entity closed cycle exists.
- **Dedup:** aggregator fill events often coexist with underlying DEX swap
  logs in the same tx. Prefer registry/DEX swap edges when both exist for the
  same pool/amounts; do not double-count edges in the cycle walk or inflate
  Exact arb.

**Expect:** more aggregator-mediated arbs; no duplicate Exact from fill+swap.

> **Status: done.** Added `Amm::Aggregator`; `decode_aggregator_swap` covers
> 1inch V4/V5 `Swapped`, Paraswap `Swapped`/`SwappedV3`, and 0x Exchange `Fill`
> (topic hashes verified via keccak in `topic_hashes_match_canonical_signatures`;
> 0x tokens left unresolved for transfer pairing). `dedup_aggregator_facts`
> runs inside `decode_tx_logs` after `attach_swap_tokens` and drops aggregator
> edges covered by a ≤3-hop DEX chain (200 bps direct / 400 bps multi-hop).
> `classify_block` pass 3b upgrades `Unknown` events to `ArbAtomic` (`Inferred`,
> reason `TRANSFER_CYCLE`) when the tx's transfer graph has a closed ≥3-entity
> cycle rooted at the searcher (`has_transfer_cycle`); 2-entity cycles never
> upgrade and parity-independent. Tests: `oneinch_swapped_decodes`,
> `paraswap_swapped_decodes`, `paraswap_swapped_v3_decodes`,
> `zrx_fill_decodes_amounts_tokens_unresolved`,
> `aggregator_dup_edge_removed_when_dex_pair_covers`,
> `transfer_cycle_upgrades_unknown_to_arb`, `two_entity_cycle_does_not_upgrade`.
> Verification: 64 explorer lib tests, full core + cli pass, clippy clean.

---

## Phase 2 — Profit accuracy

### 2.1 Real gas
> **Status: done.** `ingest` prefers receipt `effective_gas_price`, then
> legacy `gas_price`, then `base_fee + max_priority_fee`. RPC client + cache
> store already carry both fields.
- `ReceiptData` is built from alloy's `TransactionReceipt` at rpc/client.rs:1599,
  which already carries `effectiveGasPrice`; `TxData` at client.rs:1555 carries
  legacy `gasPrice`. Add both fields and prefer them over the
  `base_fee + max_priority_fee` approximation (ingest.rs:156-163).
- Backfill `cache/store/blocks.rs:313` and `core/tests/common/setup.rs:244`
  fixtures; default to legacy gas price so type-0 txs no longer under-count.
- Modern txs must clear gas attach at persist (store.rs:489-492).

**Expect:** gas_usd / net closer to receipts; type-0 under-count gone.

### 2.2 Flash-loan netting + fee (§19/§21)
- Add `decode_flash_loan(log)` in decode.rs (glue `LogData` →
  `chain::events` / `chain::flashloans` decoders or equivalent), covering
  **Aave V2/V3 and Balancer V2**.
- **Uni V3 `Flash`:** different semantics (callback repay in-tx, not a classic
  loan+fee provider event). Either implement a dedicated netting path with its
  own tests, or leave it explicitly out of this phase and list under Deferred.
  Do not pretend Aave-style netting applies unchanged.
- In `classify_block`, when a searcher tx contains a flash-loan event: call the
  currently-dead `net_flash_loan` (profit.rs:176) on the ledger **before**
  `select_profit_token`, and record `flashloan_fee` (event fee, else
  provider-bps default) on the event.
- New `MevEvent.flashloan_fee_wei`; store:
  `ALTER TABLE mev_ops ADD COLUMN flashloan_fee_usd REAL` (runtime migration
  via `pragma table_info` for existing DBs),
  `net_profit_usd = profit_usd − gas_usd − flashloan_fee_usd` (store.rs:489-492).

**Expect:** flash-funded arbs no longer gross-inflated; fee column roundtrips.

> **Status: done.** `decode_flash_loan` covers Aave V2/V3 with native ABI
> layouts and Balancer V2 via the scanner decoder; `FlashLoanFact` carries the
> emitting `provider`. `classify_block` nets the borrow only when the repay leg
> is absent from the Transfer stream (repay to provider/recipient detected ⇒ no
> double-subtraction), records the largest non-zero fee into
> `MevEvent.flashloan_fee_wei`/`flashloan_fee_token`, and store persists
> `flashloan_fee_usd` (runtime `ensure_column` migration) with
> `net_profit_usd = profit_usd − gas_usd − flashloan_fee_usd`. Uni V3 `Flash`
> stays deferred. Tests: `aave_v2_flash_loan_decodes`,
> `aave_v3_flash_loan_decodes`, `flash_loan_principal_is_netted_when_repay_missing`,
> `flash_loan_repay_present_keeps_profit`.

### 2.3 Multi-token net USD
- `DeltaLedger::positive_tokens` (profit.rs:105) already gives all positive
  residuals. Emit them per-event; at persist sum USD across all (store.rs:479),
  net = Σtokens − gas − flashloan_fee; keep `profit_token` as display-primary.
  Depends on 2.2 so flash-netting has already cleaned the ledger.
- JIT events have `profit_token=None ⇒ profit_usd=None ⇒ 0 in USD aggregates`
  (classify.rs:470); leave unit-reported but flag in docs.

**Expect:** multi-residual txs no longer under-report USD vs single-token pick.

> **Status: done.** `MevEvent.profit_tokens: Vec<(Address, U256)>` carries every
> strictly-positive post-netting residual (deterministic address order; zero-net
> excluded). `DeltaLedger::positive_tokens` was tightened to net>0. The arb pass
> populates it from the ledger; persist (store.rs) now USD-sums across all
> priced residuals (`profit_usd = Σ token_usd`), keeps
> `profit_token`/`profit_amount` as the display-primary, nets
> `net_profit_usd = Σ − gas − flashloan_fee`, and surfaces the residual list in
> `details_json.profit_tokens` for forensic display. JIT (fee-capture,
> `profit_token=None`) and sandwich/liquidation single-token paths are unchanged.
> Tests: `multi_residual_arb_captures_every_positive_token`,
> `multi_residual_profit_usd_sums_all_priced_tokens`,
> `positive_tokens_lists_net_positives_only`, `insert_and_query_roundtrip`.

### 2.4 Pricing
> **Status: done.** Multicall3 decimals fallback in `ingest`;
> `amount_usd_realized` prefers hourly price then on-chain realized rate;
> `fot_tokens` / rebase registry mark `usd_approximate` at persist.
- Decimals fallback via `erc20.decimals()` batch multicall
  (core/src/rpc/multicall.rs) for long-tail tokens.
- Prefer the on-chain realized rate (stable leg of the route) as
  `realized_usd` anchor.
- Apply `core/data/fot_tokens.json` so FOT/rebase deltas are marked approximate.

**Expect:** fewer null `profit_usd` on long-tail; FOT marked approximate.

---

## Phase 3 — New strategy kinds (spec conformance)

Requires **Phase 5a-1** (kinds + DB CHECK + canonical + `kind_order`) before
persistable events.

### 3.1 Backrun — new `Backrun` kind (§12)
- New pass `classify_backruns(input)` after sandwiches (and after frontrun
  exclusion of consumed txs). Logs-only causality, never ordering alone (§24):
  market-moving tx (large notional or big Δtick/Δreserve on a pool) followed by
  an adjacent tx where a different sender profitably closes a cycle at a
  _better execution price than the pool offered pre-move_.
  Requires pool overlap + direction-of-benefit + closed cycle; else rejected.
- `Confidence::Inferred` + reasons `STATE_DELTA_MATCH`, `DIRECT_TOKEN_CYCLE`.
- **Known approximation:** the detection-engine spec prefers
  `profit(B|state_before_A)` vs `profit(B|state_after_A)` (REVM). Logs-only
  execution-price comparison is a weaker proxy; do **not** treat
  `STATE_DELTA_MATCH` as REVM-verified. Document under Known biases.

### 3.2 Frontrun — new `Frontrun` kind (§13)
- New pass `classify_frontruns(input)`. Requires all of: searcher tx changes a
  pool's execution price (measurable via swap amounts), victim tx immediately
  after gets degraded execution vs pre-searcher price, and the searcher
  economically benefits (subsequent profitable opposite leg).
  `Confidence::Inferred`; reasons `SAME_POOL`, `VICTIM_EXECUTION_DEGRADED`,
  `PROFIT_VERIFIED`. Explicit negative test: tx before victim with no
  measurable degradation ⇒ not frontrun.
- Sandwich-consumed fronts/backs are excluded (kind priority).

### 3.3 Evidence / confidence model (§23)
- Serialize reason codes into `MevEvent.details`; `Confidence::Exact` only for
  structurally verified cases (closed arb cycle, sandwich structure,
  liquidation event match, JIT pairing); `Inferred` elsewhere with reasons.

**Ship gate:** labeled golden set from Phase 0.5 — precision on backrun/
frontrun positives/negatives acceptable before enabling in live feed defaults.

> **Status: done** (synthetic ship gate). Embedded labeled set in
> `explorer::golden` (2 positives + 4 negatives incl. sandwich exclusion);
> `score_embedded_causal_set` + unit test; CLI
> `explorer validate --golden-causal`; live-feed/API defaults exclude
> `frontrun`/`backrun` (`--kinds all` to opt in). Chain-curated blocks still
> pending RPC.
>
> `classify_backruns`/`classify_frontruns` added in
> `classify.rs` (after sandwiches, before final sort), plus helpers
> `CAUSAL_WINDOW_TXS=8`, `exec_price`, `price_better_by`, `price_worse_by`,
> `pool_legs`, `tx_cycle_profit`.
>
> **Backrun (3.1):** per-pool legs (resolved swaps only, sorted by tx_index);
> for a searcher leg B emitting a `Backrun`-Inferred event it must: not be a
> sandwich/frontrun-consumed tx; close a profitable ≥2-edge closed cycle
> (`tx_cycle_profit`, reuse of the arb ledger machinery incl. profit-token
> selection); have a prior opposite-direction leg A from a **different** sender
> within 8 txs with `amount_in ≥ B.amount_in` (large notional move); and a
> pre-move reference leg (same direction as B, different sender from A) such
> that B's execution price beats it by >0.5%. Claims are keyed **per pool** —
> a tx swapping on several pools keeps a backrun per pool (a plain tx_index key
> made detection nondeterministic on `HashMap` iteration order; fixed and
> stress-tested). Emits `victim_hashes=[A.tx]`, ledger + evidence, reasons
> `STATE_DELTA_MATCH`, `DIRECT_TOKEN_CYCLE`.
>
> **Frontrun (3.2):** for a searcher leg F, victim V is the next third-party
> same-direction leg within the window on the same pool whose execution
> degraded >0.5% vs F's price (explicit negative path: no measurable
> degradation ⇒ rejected); profit is verified via a subsequent opposite-
> direction leg from F's sender after V — the close may be on **any** pool
> (cross-pool close avoids the same-pool case that the sandwich pass owns).
> Reasons `SAME_POOL`, `VICTIM_EXECUTION_DEGRADED`, `PROFIT_VERIFIED`.
>
> **Wiring:** `events.retain` drops `ArbAtomic`/`Unknown` events on txs claimed
> by a Frontrun/Backrun (superseder map; frontruns win on same-tx); consumed
> set is built from sandwiches **before** appending (append order was moving
> sandwich facts out and silently emptying the consumed set). Liquidation
> events at superseded txs are kept.
>
> Tests: `causal_backrun_detected_after_market_move`,
> `backrun_rejected_without_pre_move_reference`,
> `backrun_rejected_when_move_and_backrun_same_sender`,
> `causal_frontrun_detected_with_cross_pool_close`,
> `frontrun_rejected_without_measurable_degradation`,
> `sandwich_consumed_legs_not_reclaimed_by_phase3` (30 classify tests, 205 core
> lib tests, cli + api suites green, clippy `-D warnings` clean). Both passes
> are logs-only proxies per §24; `STATE_DELTA_MATCH`/`PROFIT_VERIFIED` are **not**
> REVM-verified — documented under Known biases; Phase 3 requires a wipe + reindex
> of the affected window.

---

## Phase 4 — Live-mode & operational parity

- Reorg checks only run every 32 blocks in `run_live` (ingest.rs:484); keep,
  but also re-verify on each live poll only blocks ≥ stored `indexed_to` (cheap
  single-call hash compare). Backfill is replay-safe already.
- Confirmations (default 6) drive live lag (~12s on Polygon); expose per-chain
  confirmation override for fast-consensus chains.
- `unknown` gating from 1.2 reduces live-feed noise.
- JIT open-position table must survive process restart (1.5 / 5a-0).

> **Status: code done** for per-poll re-verify + confirmation override.
> `verify_indexed_tip` (ingest.rs, after `check_reorg`) re-verifies the indexed
> tip header on **every** live poll where the liveness gap applies
> (`indexed_to < next_block`), falling back to header-hash compare + `unwind_from`
> on mismatch; the heavier 32-block sweep is kept. Confirmations are per-chain
> overridable (`mev-scout.toml`, default kept at 6). Live-mode E2E still requires
> an RPC environment to exercise.

---

## Phase 5 — Cross-cutting plumbing & schema

Split so kinds can land before strategy passes.

### 5a — Schema scaffold (split)

### 5a-0 — JIT open-positions table (before 1.5)
- `jit_open_positions(pool, owner, tick_lower, tick_upper, opened_block,
  liquidity)` with a prune key on `opened_block` — feeder table for 1.5.

> **Status: done.** Table + `jit_open_positions_prune(opened_block)` created in
> `store.rs` `initialize` (idempotent `CREATE TABLE IF NOT EXISTS`), used by 1.5.

### 5a-1 — Kinds scaffold (before Phase 3)
- `types.rs`: add `Backrun`, `Frontrun` to `MevKind` (+ `as_str` / `parse`).
- `classify.rs` `kind_order`: insert new kinds per priority list above.
- `store.rs:163` kind `CHECK` must gain the new kinds — needs a `mev_ops` table
  rebuild migration for existing DBs (or relax the CHECK).
- `canonical.rs`: canonical ID branches for `Backrun`/`Frontrun` (pool +
  tx-index anchor, sandwich-style).
- `api/src/routes/explorer.rs` `kind_map` (+ any kind filter / OpenAPI surface)
  must accept the new kinds; update `api/tests/explorer.rs` when responses
  change.

> **Status: done.** `Backrun`/`Frontrun` added to `MevKind` (`as_str` /
> `parse` → `"frontrun"`/`"backrun"`). `kind_order` now follows the precedence
> list: Sandwich=0, Frontrun=1, Backrun=2, ArbAtomic=3, JitArb=4, Jit=5,
> Liquidation=6, Unknown=7. `mev_ops.kind` CHECK extended with the two kinds
> (fresh `CREATE TABLE IF NOT EXISTS`; existing DBs rebuilt via the
> wipe+reindex recipe). `canonical.rs` emits sandwich-style IDs
> (`Frontrun|pool|victim_tx:N`, `Backrun|pool|source_tx:N`). `kind_map` in
> `api/src/routes/explorer.rs` includes the new kinds. Verification: core +
> `api/tests/explorer.rs` (14) pass, clippy clean.

### 5b — Remaining plumbing (with / after 2.2 and Phase 3)
- `flashloan_fee_usd REAL` column per 2.2.
- `validate.rs`: `strategy_to_kind` notes — no opportunity strategies map to
  backrun/frontrun → scanner-side `precision_signal` only; wire golden-set
  path; add profit-error aggregation per Phase 0.2; document in report.
- `mod.rs` doc update: new passes + flash-loan netting + kind priority.

> **Status: code done.** `flashloan_fee_usd` column lives on `mev_ops` (net =
> profit_usd − gas_usd − flashloan_fee_usd; flash-loan fee USD via borrowed
> token price, per 2.2). `core/src/explorer/mod.rs` now documents the pass
> stack (ArbAtomic/Sandwich/Frontrun/Backrun/Jit), kind priority, gas +
> flashloan netting, Phase 1.4 sandwich profitability gate, and the 1.2
> flow-ownership exact/estimated rule. `validate.rs` profit-error aggregation
> landed with Phase 0.2; `strategy_to_kind` documents no opportunity map for
> backrun/frontrun; golden-set path wired via `--golden-causal`.

---

## Phase 6 — Regression fixtures & verification

- **Golden-block end-to-end fixture**: hand-built receipt for one arb + one
  sandwich + one JIT; assert exact event kind, profit token/amount, gas, and the
  persisted `mev_ops` row.

> **Status: golden fixture done.** `core/tests/explorer_golden.rs` builds one
> block (tx0 atomic-arb cycle USDC→TOKA→USDC across two pools; tx1–tx3
> three-EOA sandwich on one pool with swaps-only legs; tx4–tx5 JIT Mint(with
> in-range swap)+exact Burn), runs `classify_block` → asserts exactly 3 events
> of kind ArbAtomic/Sandwich/Jit (all `Exact`, searcher, profit token/amount,
> summed front+back gas, victim hash), persists via
> `ExplorerStore::insert_block_facts(BlockFactsInput{…})` with USDC @ $1 (6-dec)
> and native @ $0.75, then asserts exactly 3 exact-confidence `mev_ops` rows
> incl. canonical-ID prefixes (`ArbAtomic|`/`Sandwich|`/`Jit|`) and a
> positive-net sandwich surviving the Phase 1.4 profitability gate.
- Full unit coverage: interleaved 3-hop cycle (non-chain order) + no-cycle
  transfers ⇒ `unknown` not arb (§39); liquidation event/transfer mismatch +
  Absorb zero-collateral path + multi-asset pricing path; sandwich victim-impact
  evidence + negative-net sandwich; JIT tick-overlap required + no-overlap
  negative + long-holding (> window) mint/burn negative + restart-safe open
  positions; backrun/frontrun positive + negative
  pairs (adjacent-but-causal vs adjacent-only) **and** sandwich-exclusion
  (sandwich legs must not also emit backrun/frontrun); flash-loan gross
  deflation + fee recorded + column roundtrip + migration on a v0 schema;
  aggregator fill+swap dedup.
- Run Phase 0.5 labeled set as an integration check for backrun/frontrun.
- Update API tests when fields/kinds change (multi-token net may add columns —
  schema migration note).
- Document methodology + known biases in `ARCHITECTURE.md` §4.11 and update §6
  artifacts:
  - historical arb catch-all (`arb_likely`)
  - logs-only backrun ≠ REVM `profit(B|before)` vs `profit(B|after)`
  - gas approximation (pre-2.1)
  - single-token profit (pre-2.3)
  - hourly pricing / FOT
  - Uni V3 Flash omitted or approximate (per 2.2 decision)

> **Status: biases documented** in `ARCHITECTURE.md` §4.11 (Known biases table).
> Golden arb/sandwich/JIT fixture + causal labeled set + unit coverage as above.
> Phase 0.4 RPC baseline numbers and live-mode E2E remain environment-gated.

- Verify per phase:
  ```bash
  cargo build
  cargo test -p mev-scout-core
  cargo test -p mev-scout-cli
  cargo clippy --all-targets -- -D warnings
  ```
  plus the Phase-0 baseline recipe (and wipe+reindex) before/after each change.

---

## Execution order

```
0 (incl. golden-set curation)
  → 1.1
  → 1.2 (parity flag default=true; gate before default=false)
  → 2.1
  → 2.2
  → 5a-0 (JIT open-positions table)
  → 1.5
  → 1.3 / 1.4
  → 1.6
  → 5a-1 (kinds, CHECK, canonical, kind_order, API kind_map)
  → 3 (backrun / frontrun / evidence; after 5a-1)
  → 2.3 / 2.4
  → 5b
  → 4
  → 6
```

Rationale: Phase 0 gives the measurement baseline; 1.1 (registry direction)
unblocks 1.2 and Phase 3; 2.1 (real gas) precedes 2.2 flash-loan / P&L netting
so costs are correct before netting; **5a-0 before 1.5** so the open-position
table exists when the JIT pass persists; **5a-1 before 3** so new kinds are
persistable; 2.3 after 2.2 so multi-token USD sums a flash-cleaned ledger;
additive strategies land after identification correctness. Each step is
independently revertible; classifier changes require wipe+reindex of the
measurement window.

---

## Deferred (explicitly out of scope)

- REVM verification of realized ops (including true backrun
  `profit(B|before_A)` vs `profit(B|after_A)`).
- Slippage / protocol-fee / builder-payment columns in explorer
  (opportunity-side).
- Opportunity-side backrun/frontrun detectors.
- Redis/Postgres migration (stays SQLite).
- Uni V3 `Flash` netting — unless pulled into 2.2 with a dedicated path; until
  then, out of scope rather than mis-modeled as Aave.
