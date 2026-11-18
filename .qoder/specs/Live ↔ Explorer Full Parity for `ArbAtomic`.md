# Live ↔ Explorer Full Parity for `ArbAtomic`

## Context

The explorer classifies realized on-chain MEV and attaches ten sub-tags to every
`MevKind::ArbAtomic`: `backrun`, `flash_arb`, `long_tail`, `curve_imbalance`,
`rate_provider_staleness`, `fot_arb`, `rebase_arb`, `savax_rate_arb`,
`epoch_transition`, `gmx_adl_arb`. The live module detects only two-hop,
multi-hop, V3 JIT, and backrun; it has no tag field, no state-sim support for
Curve/Balancer/LB/Pendle, and an executor that only runs V2/V3 legs inside a
Balancer flash loan.

Goal: every ArbAtomic tag the explorer can emit must be detectable, tagged,
state-simulated, executed (local revm/anvil only), ledgered, persisted, and
reported in live.

Locked decisions:
- Execution means local revm/anvil simulation. No real tx submission.
- Executor extension uses one generic call leg, not per-venue kinds.
- FoT/rebase policy is chain-gated (see Workstream E).
- One consolidated change, internally ordered by workstream.
- Out of scope: liquidation, skim, JIT, `forwarded_profit` (realized-only).

## Current gaps (verified)

1. `MevOpportunity` has no `tags`/`details`; `opportunities` and `paper_fills`
   have no tag columns (`core/src/types/opportunity.rs:59`,
   `core/src/explorer/store/schema.rs:621`).
2. `DetectCtx` exposes no receipt logs or chain-config tag context
   (`core/src/mev/detectors/ctx.rs:9`); block facts are discarded.
3. State-sim supports only V2 + V3-family; Curve/Balancer/LB/Pendle return
   `None` → `SimPathUnavailable` (`core/src/pool/state/sim.rs:98`).
4. Exec planner accepts only V2-family/UniswapV3 legs and a Balancer flash loan
   (`core/src/exec/plan.rs:56,117`); the Solidity contract has only kind0/kind1
   swap legs (`contracts/src/FlashArbExecutor.sol:186`).
5. Balancer `rate_providers` are stored but never used; no
   `TokenRateCacheUpdated` handling in `pool/` (`core/src/pool/state/pool_types.rs:374`).
6. FoT pairs are excluded; rebase is unhandled; sAVAX, epoch, and GMX signals
   exist only in explorer.
7. Multi-token Curve/Balancer routes are ambiguous: `next_token` picks the
   lexicographically smallest token, not the detector's actual hop
   (`core/src/paper/sim.rs:290`).

## Data model

Keep `Strategy` as the execution-shape enum (no new variants). Add attribution
fields to `MevOpportunity` (`core/src/types/opportunity.rs`), all serde-default:

```rust
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub tags: Vec<String>,

#[serde(default, skip_serializing_if = "Option::is_none")]
pub route_tokens: Option<Vec<Address>>, // len = path.len() + 1

#[serde(default, skip_serializing_if = "Option::is_none")]
pub savax_route: Option<SavaxRoute>,
```

`SavaxRoute { pool, direction: Redeem|Mint, rate_wad, shares, min_out_wad }`.
`savax_rate_arb` rows remain `Strategy::MultiHopArb`; the virtual Benqi
mint/redeem leg is described by `savax_route`, not a new Strategy.

Add `add_tag` / `has_tag`; `core/src/types/tags.rs` holds the ten constants.
Canonical id is unchanged and excludes tags. Add
`compute_savax_canonical_id(pool, direction)` so mint/redeem in one block do not
collapse.

### Persistence and report

- Migration `migrate_v17_live_tags` (append to `MIGRATIONS`,
  `core/src/explorer/store/schema.rs:27`; current head is 16):
  `opportunities.tags_json`, `opportunities.details_json`,
  `paper_fills.tags_json` via `ensure_column`.
- `OpportunityInput` + `insert_opportunities` + read path
  (`core/src/explorer/store/mod.rs:181`, `query.rs:100,316`) carry and parse
  tags/details.
- `core/src/explorer/results.rs:28` serializes `tags`, `route_tokens`,
  `savax_route`, and tag evidence into `details_json`.
- `PaperFill` (`core/src/paper/types.rs:181`) gains `tags: Vec<String>`;
  `paper/store.rs` writes `tags_json`.
- `AggregationResult` (`core/src/pipeline/aggregate.rs:60`) gains
  `by_tag: HashMap<String, StrategyMetrics>`; a row with N tags contributes to
  each bucket, so tag totals overlap by design. Render in live
  (`core/src/jobs/live.rs:761`) and `cli/src/commands/report.rs:106`; add a
  truncated tag column in `cli/src/display.rs`.

## Per-tag design

| Tag | Live implementation | Detect | Tag | Sim | Exec | Report |
|---|---|---|---|---|---|---|
| `backrun` | Keep `BackrunDetector` net-flip semantics; stamp on every `Strategy::Backrun` row at finalize. No direction check in v1. | exists | add | exists | V2/V3 exists | add |
| `flash_arb` | Computed route attribute: stamp when `ArbTagPolicy.flash_financed` (all current exec paths use the Balancer loan; paper `flash_loan=false` disables it). Not a detector. | attribute | add | exists | exists | add |
| `long_tail` | Reuse explorer `is_long_tail_arb` (`classify/arb.rs:617`, make `pub(crate)`); blue chips = `ChainConfig.profit_stable_tokens` + wrapped native. Warn if the list is empty. | exists | add | exists | venue-gated | add |
| `curve_imbalance` | Mirror explorer exactly: Curve leg + ≥2 distinct AMM labels. Label only; no new threshold. Evidence (Curve balances/A) goes into `details_json`. | exists | add | Workstream C | Workstream D | add |
| `rate_provider_staleness` | Reuse explorer condition: Balancer leg + cross-venue + no same-block `TokenRateCacheUpdated` for that pool. Add per-token rate-cache state for evidence; do not invent post-refresh prediction. | exists | add | C | D | add |
| `fot_arb` | Chain-gated dynamic tax: detectors stop excluding learned-FoT pairs; apply learned bps in route quotes. Static ETH registry is N/A on Avalanche. Tag when a route token has measured/registered bps. | E | E | E | natural | E |
| `rebase_arb` | Keep current behavior (rebase reserves are applied by Sync before detection); tag from the static registry only. No ex-ante rebase prediction. | exists | add | exists | venue-gated | add |
| `savax_rate_arb` | New `mev/detectors/savax.rs`, shared hourly rate cache, both mint and redeem, ≥5 bps divergence, notional cap. Emits `MultiHopArb` + `savax_route`. | new | add | new branch | D | add |
| `epoch_transition` | Block-wide `NotifyReward` facts; ≥2 legs on ≥2 epoch-venue pools; reward token touched. Tag at finalize (same as explorer) plus a post-NotifyReward targeted rescan. | exists + rescan | add | C | venue-gated | add |
| `gmx_adl_arb` | GMX EventEmitter decode; route touches `gmx_market_tokens`; `signal_tx <= opp.tx_index`. Tag at finalize plus a post-ADL rescan. | exists + rescan | add | C | venue-gated | add |

Tag conditions are not re-implemented: build a synthesized `Vec<SwapFact>` from
the candidate route and call `explorer::strategy_tags::arb_strategy_tags`
(`core/src/explorer/strategy_tags.rs:18`) so live/explorer cannot drift.

## Workstreams (single change, ordered commits)

### A. Foundation: types, facts, persistence, report

Files:
- `core/src/types/opportunity.rs`, `core/src/types/tags.rs` (new)
- `core/src/explorer/results.rs`, `store/mod.rs`, `store/query.rs`, `store/schema.rs`
- `core/src/paper/types.rs`, `paper/store.rs`, `paper/ledger.rs`
- `core/src/pipeline/aggregate.rs`, `jobs/live.rs`
- `cli/src/display.rs`, `cli/src/commands/report.rs`

Update every struct-literal constructor (`arb_common.rs:163`, `jit.rs:329`,
`store/query.rs:382`, test constructors in `jobs/live.rs`, `paper/ledger.rs`,
`pipeline/aggregate.rs`, `pipeline/runner.rs`).

### B. Detection and tagging

- `core/src/data/types.rs`: `impl From<&ExecutedLog> for LogData` (fields match).
- `core/src/mev/detectors/tagging.rs` (new):
  - `ArbTagPolicy { blue_chips, wrapped_native, savax, epoch_venue_pools,
    gmx_market_tokens, gmx_event_emitters, flash_financed }`.
  - `BlockTagFacts { epoch_reward_tokens, rate_cache_updates, gmx_signal_tx,
    savax_rate_wad }`.
  - `synth_route_swaps`, `stamp_route_tags`.
- `pipeline/runner.rs`:
  - `BlockDetectors` collects facts from `tx.logs` after `update_from_logs`
    using reusable decoders (`explorer/decode/signals.rs:22,49,148,166`).
  - `finalize_block_opportunities` stamps route tags and backrun; both replay
    and log-only call sites pass facts.
  - Targeted rescan after `NotifyReward` (epoch venue pools) and GMX ADL
    (`ScanScope::Dirty` over pools containing GMX tokens), reusing existing
    per-block dedup.
  - Mempool path stamps only `flash_arb` + `long_tail` (no logs).
- Detectors set `route_tokens`: two-hop `[token_in, shared, token_out]`;
  multi-hop the exact `walk_token` sequence.
- `jobs/live.rs::LiveContext::new` builds `ArbTagPolicy` from
  `validation.chain_config` and `jobs::load_pool_registry` (`PoolRegistry.epoch_venue`).
- Make `is_long_tail_arb` and `savax_pool_diverges` `pub(crate)`; no logic change.

### C. Venue state and state-sim parity

- `core/src/pool/state/sim.rs`: add `PoolPostState::Balances(Vec<u128>)` for
  Curve/Balancer and `Reserves2`/`PendleBalances` for LB/Pendle; implement
  `quote_swap_post` via `PoolState::quote_dir` + reserve/balance update; extend
  `apply_post_state`.
- `core/src/paper/sim.rs`: `walk_quote_post` takes `route_tokens` and uses it
  instead of `next_token` when present; sAVAX virtual-leg branch applies the
  rate to shares before the next leg.
- Balancer rate cache: add `rate_cache: Vec<Option<RateCacheEntry>>` to
  `BalancerPoolState`; handle `TokenRateCacheUpdated` in `apply.rs`; include in
  manager undo snapshots. Extend `RateCacheFact` with `token_index`.
- Unsupported Metric/Fluid or missing token index keeps returning `None` →
  `SimPathUnavailable`; never convert into a fill.

### D. Exec parity: generic call leg

Solidity `contracts/src/FlashArbExecutor.sol`, ABI v3:

```solidity
struct Leg {
  address target; address tokenIn; address tokenOut; address spender;
  uint8 kind; uint24 fee; uint256 minOut; bytes data;
}
// kinds: 0 V2 exact-in, 1 V3 exact-in, 2 call,
//        3 wrapAll, 4 unwrap, 5 callWithValue
```

- `_leg` dispatches; kind2 snapshots tokenOut/native balance, optional
  `forceApprove`, `target.call(data)`, asserts delta ≥ `minOut`; kinds 3/4
  wrap/unwrap WETH; kind5 sends native value and checks native delta.
- Security: operator-only, no delegatecall, reentrancy guard, closed kind set,
  `legs.length <= 8`, `data.length <= 512`, optional `allowedTargets`
  (unrestricted in revm/anvil, recommended for deployment).
- `core/src/exec/calldata.rs`: encode the new dynamic tuple array. Prefer
  `alloy::sol!` types plus golden-vector tests over hand-rolled offsets.
- `core/src/exec/venues.rs` (new): Curve `exchange`/`exchange_underlying`,
  Balancer Vault `swap(SingleSwap,…)`, LB router, Pendle router, Solidly-stable
  precomputed `swap(amount0Out, amount1Out, to, "")`, sAVAX mint/redeem.
- `core/src/exec/plan.rs`: walk `route_tokens`; build legs via `venues.rs`;
  keep V2/V3 fast paths. Add `FillSkipReason::ExecVenueUnsupported` for
  Metric/Fluid/unconfigured routers.
- `exec/artifacts.rs`: rebuild both hex artifacts with the documented forge
  recipe; bump `ABI_VERSION` and Solidity `version()` to 3; assert at deploy.

### E. FoT/rebase policy (chain-gated)

- Change `PoolManager.dynamic_fot: HashSet` to
  `HashMap<Address, TaxEstimate { bps, samples }>`; keep the static registry as
  a fallback. `learn_taxes_from_tx` computes conservative bps.
- Remove the blanket `is_fot_pair` rejection; apply tax per leg in route
  quoting: pool input uses `amount_in·(10000−bps)/10000`; received output is
  haircut by the output token's bps. Paper state-sim applies the same haircut
  and updates reserves with pool-side amounts.
- Tag `fot_arb` when a route token has a measured or static bps.
- Rebase: tag-only from the static registry; Avalanche has no entries, so the
  tag is structurally absent there (documented, not silently dropped).
- Gate with `[live] fot_quote` (default true; no-op when the tax map is empty).

### F. sAVAX rate arb

- Move `SavaxRateCache` + `fetch_savax_exchange_rate` from
  `explorer/ingest.rs:184,1237` into `core/src/chain/savax.rs`; re-export for
  explorer. Hourly cache, one `eth_call` per hour.
- New detector `mev/detectors/savax.rs`: redeem direction (buy sAVAX in pool,
  redeem at `rate_wad`) and mint direction (mint, sell into pool); 5 bps
  threshold, notional capped by pool reserve and config; emits `MultiHopArb`
  with `savax_route` and tag; no rate ⇒ no emission, logged explicitly.
- Sim branch: pool leg then apply `rate_wad` to shares (redeem) or mint shares
  (mint).
- Exec: redeem = `[pool WAVAX→sAVAX, redeem(shares), wrapAll]`; mint =
  `[unwrap, callWithValue submit, pool sAVAX→WAVAX]`.
- `LiveContext` refreshes the rate once per pass and passes it to the runner.

### G. Verification

1. `cargo test --workspace -j 2` and `cargo clippy --workspace -j 2 -- -D warnings`.
2. `forge test` in `contracts/` (existing 12 + new kind2/3/4/5, allowlist,
   minOut, native delta, version).
3. Tag parity unit: feed identical synthetic swaps to
   `explorer::arb_strategy_tags` and `tagging::stamp_route_tags`; assert equal
   tag sets. Cover backrun, flash, long-tail, sAVAX probe, FoT, epoch, GMX.
4. State-sim: V2/V3 regressions unchanged; Curve/Balancer/LB/Pendle post-state
   tests; multi-token route uses `route_tokens`; sAVAX both directions; no
   silent `SimPathUnavailable` → fill.
5. Exec: calldata golden vectors; plan-builder tests per venue; revm mock
   targets; anvil fork test for one Curve `exchange` route if an endpoint is
   available.
6. Corpus: extend `core/tests/mev_corpus.rs` (`Strategy→kind` map) and
   `paper_corpus.rs`; add `live_tag_parity.rs` that replays the frozen AVAX
   window `99833048..=99836394` and joins live rows to explorer rows on
   `tx_hash` + route-pool intersection, per tag. Exclude `forwarded_profit` and
   chain-unreachable static FoT/rebase.
7. Migration: open a pre-v17 DB, verify old rows read with empty tags, insert
   tagged rows and read back; `paper_fills.tags_json` round-trip.
8. Manual: `live` one pass with `--sim state --exec revm`, confirm ledger
   `by strategy` plus new `by tag` sections and persisted `tags_json`.

## Acceptance criteria

- Every explorer ArbAtomic tag has a live path: detector/trigger or computed
  attribute, tag stamped, persisted, reported; FoT static/AVAX and
  `forwarded_profit` carry explicit N/A documentation.
- Every live route containing Curve/Balancer/LB/Pendle/sAVAX is state-simulatable
  or produces an explicit counted skip; no silent drops.
- Exec plans cover every supported venue; revm runs green; Uniswap V4/Pancake
  state gaps and Metric/Fluid remain explicit unsupported with counted skips.
- Frozen AVAX window keeps 47/47 arb recall and 108 arb_atomic; live tag parity
  shows no missing explorer tag for the supported set.

## Risks

- Tag drift: mitigated by calling explorer helpers; parity test guards.
- ABI v3 dynamic `bytes[]` encoding: use `sol!` + golden vectors + on-chain
  `version()` assertion.
- All-in-one change: order commits A→B→C→D→E→F→G; feature flags `fot_quote`
  and `venue_sim` allow disabling the riskiest paths without reverting schema.
- Multi-token ambiguity: `route_tokens` is mandatory for Curve/Balancer/LB
  sim/exec; fallback paths must count a skip, not guess.
- RPC budget: sAVAX hourly cache; rescans bounded by registry sets; no
  per-block provider `eth_call`.
- Windows: build/test with `-j 2`; one writer per SQLite DB; stop the live exe
  before rebuilds that lock artifacts.

## Critical files

- `core/src/types/opportunity.rs`, `core/src/types/tags.rs` (new)
- `core/src/mev/detectors/tagging.rs` (new), `savax.rs` (new)
- `core/src/pipeline/runner.rs`
- `core/src/pool/state/sim.rs`, `apply.rs`, `pool_types.rs`, `manager.rs`
- `core/src/paper/sim.rs`, `paper/types.rs`, `paper/store.rs`, `paper/ledger.rs`
- `core/src/exec/plan.rs`, `calldata.rs`, `venues.rs` (new), `artifacts.rs`
- `contracts/src/FlashArbExecutor.sol`, `contracts/test/FlashArbExecutor.t.sol`
- `core/src/explorer/store/schema.rs`, `query.rs`, `mod.rs`, `results.rs`
- `core/src/pipeline/aggregate.rs`, `core/src/jobs/live.rs`
- `cli/src/display.rs`, `cli/src/commands/report.rs`