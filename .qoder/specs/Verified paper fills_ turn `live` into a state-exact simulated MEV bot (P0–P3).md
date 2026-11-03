# Verified paper fills: turn `live` into a state-exact simulated MEV bot (P0–P3)

**خلاصه (فارسی):** `live` امروز یک bot تحقیقاتی است: فرصت را پیدا می‌کند و عددِ `expected_profit - gas` را مستقیم در کیف پول مجازی می‌نشاند، بدون آنکه اعتبار آن عدد را بسنجد. این پلن (مراحل P0–P3) یک لایه‌ی شبیه‌سازی اضافه می‌کند که پیش از ثبت هر fill، نقدینگی را واقعاً مصرف می‌کند، کاندیددهای بعدی را روی state تغییر‌یافته دوباره quote می‌کند، و شرط بازپرداخت وام آنی (`out ≥ in + fee`) را اعمال می‌کند. فعلاً opt-in با `--sim state` است و رفتارسازیِ پیش‌فرض تغییر نمی‌کند.

**Scope for state-sim (locked):** under `--sim state`, only `TwoHopArb`, `MultiHopArb`, and `Backrun` are simulated. Backrun is in-scope because it is a relabeled two/multi-hop after a net-flip (`backrun.rs`) — same flash-loan path math, capital-free. `Jit` is skipped with an explicit reason (do not mix assumed claim P&L into a state-sim session). More capital-free strategies can be added later.

## Context

Reviewing [core/src/jobs/live.rs](file:///D:/gitlab.dte.repo/mev-scout/core/src/jobs/live.rs), [cli/src/commands/live.rs](file:///D:/gitlab.dte.repo/mev-scout/cli/src/commands/live.rs) and `core/src/explorer/` produced this verdict:

- The virtual wallet exists and works: `LedgerPolicy{starting_gas_wei, reserve_wei, max_fills_per_block}` ([paper/ledger.rs:16](file:///D:/gitlab.dte.repo/mev-scout/core/src/paper/ledger.rs#L16), default 10 native units at :35), `--initial-balance` / `--reserve`, sessions in `paper_sessions`.
- A flash loan is already *priced in*: `FlashLoanProvider::fee_rate_bps` ([types/strategy.rs:35](file:///D:/gitlab.dte.repo/mev-scout/core/src/types/strategy.rs#L35)) is subtracted from gross profit in [mev/detectors/two_hop.rs:123](file:///D:/gitlab.dte.repo/mev-scout/core/src/mev/detectors/two_hop.rs#L123) and `multi_hop.rs:~362`, and the wallet is gas-only, so a zero-capital searcher is the implicit model.
- But a "fill" is a claim, not an outcome: `net_wei = expected_profit − gas_cost_wei` ([ledger.rs:56](file:///D:/gitlab.dte.repo/mev-scout/core/src/paper/ledger.rs#L56)) and acceptance is five gates (:136–204) that never re-derive anything from liquidity state. `PoolConflict` (:183) is set-membership over pool addresses, so **two candidates exploiting the same imbalance both win**.
- The revm-grade machinery exists but is unreachable: `execute_whatif` ([replay/whatif.rs:99](file:///D:/gitlab.dte.repo/mev-scout/core/src/replay/whatif.rs#L99)) and `paper_vs_executed` ([paper/recon.rs:201](file:///D:/gitlab.dte.repo/mev-scout/core/src/paper/recon.rs#L201)) have zero callers outside `core/tests/paper_corpus.rs`.

**Goal of this plan:** close the *self-consistency* half of that gap — fills must be re-derived from the same AMM state the detector used, with flash-loan repayment enforced — using only code paths that already work offline and on pruned nodes.

**Scope decisions (confirmed with user):** P0–P3 only; `assumed` stays the default and simulation is opt-in via `--sim state`; wallet stays gas-only with borrow/repay modeled arithmetically; state-sim strategies = two-hop + multi-hop + backrun only; everything folds into `live`, no new subcommand.

## Architecture

One new pure module between detection and the ledger: `core/src/paper/sim.rs`. Detection keeps producing `MevOpportunity` unchanged; the pipeline additionally captures the **pool state each candidate was quoted against** (`SimSeeds`), and `LedgerPolicy` gains a sibling `apply_simulated` that re-derives each fill's output by walking the candidate's own legs over a block-level scratch overlay. `apply` is left byte-for-byte intact so the default path cannot regress.

```
detect (runner)  ──opps──►  LiveContext.run_blocks
                   └─seeds─►  SimSeeds (canonical_id -> pool states)
                                  │
                         apply_simulated(opps, seeds, SimPolicy)   [paper/sim.rs]
                                  │  consider: net-desc (same as apply)
                                  │  commit overlays: tx_index order among accepted
                                  │  leg1 → … → legN → fee once → native normalize → repay
                                  ▼
                              LedgerResult (net = simulated net, + sim fields per fill)
```

### Locked policy decisions (review amendments)

These were underspecified in the first draft; implement against this text, not against improvisation.

#### 1. Selection order vs execution order

Do **not** replace net-descending consideration with pure `tx_index` greedy. That would starve high-net late txs under `max_fills` and make `assumed` vs `state` incomparable beyond a vague “≤”.

- **Consider** candidates in the same order as `LedgerPolicy::apply` today: descending net, then mempool last (`mempool_rank`), then `tx_index`, then `canonical_id` ([ledger.rs:114–122](file:///D:/gitlab.dte.repo/mev-scout/core/src/paper/ledger.rs#L114)).
- Gates 1–4 unchanged (native-eligible / positive net / max fills / gas+reserve). Gate 5 (`PoolConflict` set-membership) is replaced by the re-quote against the overlay.
- When a candidate is **accepted**, its post-swap pool states are written into the **block overlay**. Co-impact for later *considered* candidates uses that overlay (liquidity already consumed), regardless of consideration order.
- Mempool-only candidates sort after all tx-anchored ones (same as today).

#### 2. Block overlay merge algorithm

Per block, maintain `overlay: HashMap<Address, PoolState>` (empty at block start). For each considered candidate that reaches the sim gate:

```
for each pool P in candidate.path (or pool_a/pool_b):
  if overlay contains P:
    use overlay[P]
    if candidate.snapshot.tx_index != the tx_index that last wrote P into overlay:
      state_drift = true
  else if candidate.snapshot contains P:
    seed overlay[P] = snapshot[P].clone()
  else:
    skip SimPathUnavailable / SimNotQuotable

walk legs with quote_swap_post on overlay states; carry intermediate amounts
on accept: write each leg's PoolPostState back into overlay[P]
on reject (SimPriceConsumed / SimRepayShortfall / …): do not mutate overlay
```

Do **not** re-seed a pool from a later candidate's snapshot once it is already in the overlay — that would undo earlier fills.

#### 3. Gross / flash fee / native units (no double-count)

Detectors already do `profit_after_fl = raw_profit − flash_fee` then `normalize_profit` to native ([two_hop.rs:122–133](file:///D:/gitlab.dte.repo/mev-scout/core/src/mev/detectors/two_hop.rs#L122)). Sim must mirror that pipeline exactly once:

1. Re-quote raw cycle output in **token units**: `out` from walking legs; `borrowed = input_amount`.
2. Repay check: `out ≥ borrowed + flash_loan_fee(borrowed)` else `SimRepayShortfall` (`repay_ok=false`).
3. Token gross = `out − borrowed − fee` (fee subtracted **once**).
4. Native gross = same `normalize_profit` / `normalize_profit_native` path the detectors use (promote those helpers to `pub(crate)` if needed).
5. `sim_net_wei = native_gross − gas`. Persist `sim_gross_wei` as the **native** gross so it is comparable to `expected_profit`.

Invariant (b) (`sim_gross_wei ≤ expected_profit`) is only meaningful after step 4. Comparing token-raw to native `expected_profit` is a false failure.

#### 4. Strategy gate under `--sim state`

| Strategy | `--sim assumed` | `--sim state` |
|---|---|---|
| `TwoHopArb` | fill via claimed net | simulate |
| `MultiHopArb` | fill via claimed net | simulate |
| `Backrun` | fill via claimed net | simulate (same path math) |
| `Jit` | fill via claimed net | **skip** (`SimNotSimulatable` or dedicated reason) — no inventory model |

Add `FillSkipReason` arm for JIT-under-state (name TBD: `SimStrategyUnsupported` is fine). Do not credit JIT assumed net into a state-sim session wallet.

#### 5. Caps are different dials

- `sim_max_candidates_per_block` = **seed budget** in the runner (how many snapshots to retain per block). Default 32.
- `max_fills_per_block` / `HARD_MAX_FILLS_PER_BLOCK` = **fill slots** in the ledger. Default 32.

They happen to share the number 32; report them separately (`seeds capped` vs `fills capped`) so debugging the wrong dial is obvious.

#### 6. Non-V2/V3 legs

`quote_kind` marks Curve/Balancer as `Generic` ([quoting.rs:42–53](file:///D:/gitlab.dte.repo/mev-scout/core/src/pool/state/quoting.rs#L42)) with no reserve-shaped post-state. `quote_swap_post` returns `None` for those (and TraderJoe/Pendle/… Unsupported) → `SimPathUnavailable`. Do **not** force them into a V2/V3 `PoolPostState` variant. V3-family (V3/V4/PancakeInfinity) share the concentrated post-state shape; V2 is reserves-only.

## Phase P0 — vocabulary + persistence (no behavior change)

**Files:** [core/src/paper/types.rs](file:///D:/gitlab.dte.repo/mev-scout/core/src/paper/types.rs), [core/src/paper/store.rs](file:///D:/gitlab.dte.repo/mev-scout/core/src/paper/store.rs), [core/src/explorer/store/schema.rs](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/schema.rs), [core/src/config/settings.rs](file:///D:/gitlab.dte.repo/mev-scout/core/src/config/settings.rs), [cli/src/cli.rs](file:///D:/gitlab.dte.repo/mev-scout/cli/src/cli.rs), [core/src/jobs/live.rs](file:///D:/gitlab.dte.repo/mev-scout/core/src/jobs/live.rs)

- `types.rs`: new `pub enum SimMode { Assumed, StateSim }` (mirrors `PaperMode`'s `as_str`/`Display` pattern at :46–60). **Naming:** `PaperMode::Sim` already exists and is used only at [paper/store.rs:287](file:///D:/gitlab.dte.repo/mev-scout/core/src/paper/store.rs#L287) for a different concept (paper run kind). Keep both enums; add a one-line doc on each so nobody “simplifies” them together.
- Extend `FillSkipReason` (:7) with `SimNotQuotable` (state snapshot missing), `SimPriceConsumed` (re-quoted net ≤ 0 after earlier fills), `SimRepayShortfall` (`out < in + flash fee`), `SimPathUnavailable` (leg pool absent / non-simulatable pool type), `SimStrategyUnsupported` (e.g. JIT under state-sim). Add matching `as_str` arms (:21) — exhaustive, compiler-forced.
- Extend `PaperFill` (:64) with `sim_mode: SimMode`, `sim_gross_wei: u128`, `sim_net_wei: i128`, `borrowed_wei: u128`, `repay_ok: bool`, `state_drift: bool`. For `assumed` fills these are identity values (`sim_net_wei == net_wei`), so no downstream reader has to special-case.
- Migration: append `migrate_v11_paper_fill_verification` to `MIGRATIONS` ([schema.rs:27–38](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/schema.rs#L27); tip today is `migrate_v10_pricing_flags`) using `ensure_column` ([schema.rs:665](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/schema.rs#L665)). `paper_fills`: `sim_mode TEXT NOT NULL DEFAULT 'assumed'`, `sim_gross_wei TEXT`, `sim_net_wei TEXT`, `borrowed_wei TEXT`, `repay_ok INTEGER`, `state_drift INTEGER NOT NULL DEFAULT 0`. `paper_sessions`: `sim_mode TEXT`, and optionally `replay_blocks` / `log_only_blocks` INTEGER for the P2 mode counters. Then update the duplicated DDL in `ensure_paper_tables` ([paper/store.rs:11–50](file:///D:/gitlab.dte.repo/mev-scout/core/src/paper/store.rs#L11)) and **both** INSERT lists (`:69` sessions, `:104–109` fills) plus the readers (`paper_session`, `paper_fills`) — six sites, one checklist.
- Config `[paper]`: `sim: String = "assumed"`, `sim_flash_loan: bool = true`, `sim_max_candidates_per_block: usize = 32`, and `PaperConfig::sim_policy() -> SimPolicy`.
- CLI: `LiveArgs.sim: Option<String>` (`--sim assumed|state`), `LiveOpts.sim` + `sim_max_candidates_per_block`, resolved next to the existing `resolve_ledger_policy` fold point in `job_live`.
- `report` and `render_ledger_summary` gain: `sim: state-sim N fill / M skipped | assumed K`, plus separate seed-cap vs fill-cap lines when relevant, and `replay_blocks` / `log_only_blocks` when recorded.

**Done when:** `cargo test -p mev-scout-core` green with the new columns round-tripping through `paper/store.rs` tests, and a default run still emits `sim_mode='assumed'` rows identical to today's.

## Phase P1 — post-state quote primitives

`quote_dir` ([pool/state/quoting.rs:32](file:///D:/gitlab.dte.repo/mev-scout/core/src/pool/state/quoting.rs#L32)) returns only the output amount; it discards the post-swap reserves, so it cannot consume liquidity. Add the missing half without duplicating math:

- **New** `core/src/pool/state/sim.rs`: `pub enum PoolPostState` (V2: `reserve0/reserve1`; V3-family: `sqrt_price_x96/tick/liquidity` — covers V3/V4/PancakeInfinity). No Curve/Balancer/Unsupported variants — those return `None` from `quote_swap_post`. `pub fn quote_swap_post(&PoolState, token_in, amount_in, token_out) -> Option<(u128, PoolPostState)>`, `pub fn apply_post_state(&mut PoolState, &PoolPostState)`. V2 mutation mirrors `apply_v2_swap` ([pool/state/apply.rs:16](file:///D:/gitlab.dte.repo/mev-scout/core/src/pool/state/apply.rs#L16)); V3 mutation mirrors `apply_v3_swap` (:55).
- Extract `quote_v3_exact_in_post(...)` out of the existing tick-crossing loop in `core/src/pool/math/v3.rs` (the loop already tracks `sqrt_price`, `current_tick`, `liquidity` and throws them away) and make `quote_v3_exact_in` a thin wrapper, so the V2/V3 fee-and-impact math stays single-sourced.
- `PoolManager::subset(&self, addrs: &[Address]) -> PoolManager` (absent today; `PoolState` and `PoolManager` are both `Clone`) so a scratch overlay carries only the pools a candidate touches and every existing quote helper keeps working on it.
- Promote to `pub(crate)` and reuse: `two_hop_profit_at` ([two_hop.rs:255](file:///D:/gitlab.dte.repo/mev-scout/core/src/mev/detectors/two_hop.rs#L255)), `MultiHopArbDetector::walk_quote` ([multi_hop.rs:467](file:///D:/gitlab.dte.repo/mev-scout/core/src/mev/detectors/multi_hop.rs#L467)) — for **mutating** walks prefer a `walk_quote_post` built on `quote_swap_post`, not a silent non-mutating re-quote. Bounds via `max_input`/`sell_reserve`/`reserve_pair` (quoting.rs:60/98/133). Also promote `normalize_profit` / `normalize_profit_native` from `arb_common` for the fee→native pipeline in §3.

**Tests:** `quote_swap_post` + `apply_post_state` round-trip equals `quote_dir` for the same input; a second identical swap on a V2 pair returns strictly less output; `subset` leaves the source manager untouched; Curve/Balancer → `None`. Harness: inline `v2_pool()` ([two_hop.rs:459](file:///D:/gitlab.dte.repo/mev-scout/core/src/mev/detectors/two_hop.rs#L459)) and `pm_with()` (multi_hop.rs:~906).

## Phase P2 — capture the seeds

The state a candidate was quoted against exists only inside the per-tx loop and is currently discarded: `on_tx` closure `|i, tx, _db|` ([pipeline/runner.rs:789](file:///D:/gitlab.dte.repo/mev-scout/core/src/pipeline/runner.rs#L789)). Follow the `take_rejections()` drain precedent ([runner.rs:334](file:///D:/gitlab.dte.repo/mev-scout/core/src/pipeline/runner.rs#L334)):

- `type SimSeeds = HashMap<String, PoolSnapshot>` with `PoolSnapshot { block_number, tx_index, pools: Vec<(Address, PoolState)> }`, keyed by `canonical_id`.
- `BacktestRunner`: `sim_seeds: RefCell<SimSeeds>` + `pub fn take_sim_seeds(&mut self) -> SimSeeds`, populated at the three emit points — `finalize_block_opportunities` ([runner.rs:447](file:///D:/gitlab.dte.repo/mev-scout/core/src/pipeline/runner.rs#L447)), `sync_block_from_logs` (:852, the log-only path), and [mempool.rs:58](file:///D:/gitlab.dte.repo/mev-scout/core/src/mev/detectors/mempool.rs#L58) `detect_pending_opportunities` — and only when `sim != Assumed`, capped by `sim_max_candidates_per_block` (**seed** budget, not fill cap). Prefer seeding the three in-scope strategies; JIT need not be snapshotted under state-sim.
- `run_range_hybrid` (:1190) keeps its signature; `LiveContext::run_blocks` stops discarding `Vec<BlockMode>` and accumulates `replay_blocks` / `log_only_blocks` on the session so report output is not anecdotal.

**Tests:** with `sim=assumed`, `take_sim_seeds()` is empty (zero memory cost on the default path); with `sim=state`, every emitted in-scope opportunity has a snapshot containing its `pool_a`/`pool_b`/`path`.

## Phase P3 — the simulated ledger

- `SimPolicy { flash_loan: bool, fee_bps: u128, max_candidates_per_block: usize }` (fee/gas-overhead reused from `FlashLoanProvider::fee_rate_bps` / `gas_overhead`, types/strategy.rs:35/48; gas via `GasConfig::compute_gas_cost_with_limit`, types/strategy.rs:276).
- `pub fn simulate_candidate(...)` / block driver implementing §1–§4 above: consider net-desc; overlay merge per §2; fee+normalize per §3; strategy gate per §4. `borrowed = input_amount`; repay check before wallet credit; `state_drift` as defined in §2.
- `LedgerPolicy::apply_simulated(&self, opps, seeds, sim) -> LedgerResult`: gates 1–4 as today; gate 5 = overlay re-quote; `net_wei(opp)` replaced by `SimOutcome.net_wei` for accepted fills. `apply` stays as-is for `Assumed`.
- **Session-state detail that matters:** `update_ledger` in [jobs/live.rs](file:///D:/gitlab.dte.repo/mev-scout/core/src/jobs/live.rs) accumulates `session_opps` across every pass and re-applies from scratch. Seeds must accumulate identically (`LiveContext.session_seeds: SimSeeds`, merged per pass, keyed by `canonical_id`), otherwise a loop session silently degrades earlier passes back to `SimNotQuotable`. This preserves the invariant the existing test `recompute_over_full_list_is_stable` guards.

## Deferred, deliberately

- **P4** EVM probe on realized anchors (`what_if_real_txs` + `paper_vs_executed`): cheap credibility, but verifies the *victim* tx, not our bundle. Next candidate if this lands.
- **P5** real candidate execution in revm: needs hand-rolled calldata (no `sol!`/`dyn-abi` in the alloy feature set), synthetic `TxEnv`s, and ERC-20 balance-slot injection; a literal flash loan additionally needs a `receiveFlashLoan` executor whose bytecode the repo does not have. Also unreachable for pending/tip blocks — `get_pending_block` (rpc/client.rs:1125) yields no state.
- Optional later: surface `sim_net` beside assumed as an estimate column without changing default wallet math (insight-layer style). Not required while `--sim state` *is* the P&L path.

## Verification

1. `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`.
2. Invariant tests added in P3 (these are the ones that prove the feature): (a) a second candidate on a pool already consumed by an accepted fill can never realize its detected net — `SimPriceConsumed` or a strictly lower net; (b) `sim_gross_wei ≤ expected_profit` monotonically **after native normalize**, never an *improvement* over the detector; (c) `repay_ok=false` ⇒ skipped, never filled; (d) `sim=assumed` reproduces today's `LedgerResult` byte-for-byte on the existing fixtures; (e) snapshot count bounded by `sim_max_candidates_per_block`; (f) JIT under `state` is always skipped, never filled; (g) fee is applied once (token gross after fee + normalize equals detector pipeline on an untouched overlay).
3. End-to-end, no archive needed (the point of P0–P3): `cargo run -p mev-scout-cli -- live --blocks 64` (default, `assumed`) then the same with `--sim state`; compare the two ledger summaries — fills and net must be equal-or-lower under `state`, and the skip-reason breakdown must name the new variants. Then `cargo run -p mev-scout-cli -- report <run_id>` to confirm `paper_fills.sim_*` persisted, and repeat with `--loop --duration 5m` to prove seed accumulation across passes.
4. Loop path check: a session whose newest block is log-only must still report simulated fills (not `SimNotQuotable`) — that is the difference between this tier and the deferred EVM tier.

## Out of scope

- Liquidation revival (explicitly retired from execution, [types/strategy.rs:82–88](file:///D:/gitlab.dte.repo/mev-scout/core/src/types/strategy.rs#L82)).
- JIT inventory / LP capital modeling (JIT is skipped under state-sim; under `assumed` it remains claim-fillable as today).
- Own-capital modeling for strategies that are not flash-loan capital-free.
- Signing / broadcast; competition with other searchers; latency; mempool timing races.
- Explorer→live candidate bridging (realized `mev_ops` are not bot inputs).
- Sandwich / frontrun / skim / utility as executable strategies (later, if capital-free and good effort/profit/risk).

## Honest residual gap

After P0–P3 the bot simulates execution on real price state it owns, and enforces the repayment invariant arithmetically for two-hop / multi-hop / backrun. It still does **not** demonstrate that a flash-loan callback would have succeeded on-chain: no revert semantics, no real reserve/hook behavior for non-V2/V3-family pool types, no fee-on-transfer tax inside quotes (`is_fot_pair` is a filter, not a model), and consideration is net-desc while overlay commits reflect accept order — co-impact is consistent with the overlay algorithm in §2, not a full global block scheduler. Those need P4/P5 and, for true flash loans, a per-chain executor-contract artifact that the repo does not have today.
