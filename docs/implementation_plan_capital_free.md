# Implementation Plan: Capital-Free MEV Detectors (multi-chain)

> Source: `docs/mev_strategies.md` — strategy reference, capital-free inventory (§11),
> Dune validation (§17), and codebase status (§16).
> Scope: **all capital-free, non-CEX strategies**, sequenced, chain-generic.
> Protocol-specific addresses are gated through `ChainConfig` / `chains.toml` (same
> pattern as `aave_v3_pool` today).

Two delivery tracks (can proceed in parallel):

1. **Detectors** (Phases 1–6) — live / full-replay opportunities for execution or paper P&L.
2. **Historical market reports** — past-month opportunity count + estimated revenue from
   on-chain logs/storage, without requiring a live bot. Replaces the Dune §17 workflow
   with our own RPC log decoding (closes gaps where Dune tables were missing).

---

## Report modes vs simulation

Not every strategy needs a detector or backtest to size the market. Three modes:

| Mode | Question | Infra |
|---|---|---|
| **A. Settled capture** | How often did *someone* already execute this, and roughly how much moved? | Log scan + decode (+ optional USD pricing) |
| **B. Latent / reconstructable** | How often did the *condition* exist (drift, unsafe vault, auction window), even if nobody acted? | Event replay + storage/balance reconstruction (`balance_drift`, OSM slots, etc.) |
| **C. Simulation-required** | Was *our* path profitable after gas/fees/ordering? | `revm` / `BlockReplayer` (+ mempool ordering for sandwiches) |

| Question | Tool |
|---|---|
| How big is the *market* that already cleared? | On-chain **report** (A/B) |
| What would *our* bot have captured after gas, conflicts, wallet? | **Paper** ledger over detections |
| Was a specific path profitable in a given block state? | **Simulation** (C) |

Dune §17 was mostly **A with noisy proxies**. Our on-chain path can do **A** cleanly and
**B** for strategies Dune lacked tables for (skim/sync on Polygon).

### Per-strategy reportability

| Strategy | Plan phase | Report? | Mode | Notes |
|---|---|---|---|---|
| skim() | 1 | **Yes** | A + B | **A:** successful `skim` + outbound `Transfer` → realized skim $. **B:** reconstruct `balance − reserve` from `Transfer`/`Sync` → latent inventory. Dune missing on Polygon (§17.4); raw logs work. |
| sync() race | 1 | Weak | A / B→C | `sync` calls are visible; $ usually lives in the follow-on arb or skim denial → needs sim. |
| Flash-loan atomic liq | 2 | **Yes (best A)** | A | Same fingerprint as `VALIDATE_FLASH_LIQ_PROFIT`. ~323 txs/mo, ~$493/tx avg on Polygon (§17.5). Closest to “Dune but ours.” |
| Interest accrual liq | 2 | Partial | A + B | Liquidations easy to count; *interest-driven* attribution needs HF-over-time reconstruction (rates + collateral/debt). |
| Backrunning | 3 | Noisy A; $ → C | A proxy / C | Same-block opposing flow is a proxy (Dune inflated then corrected). True profit needs post-swap quote sim — treat like arb. |
| V4 hook MEV | 4 | Activity A; $ → C | A / C | Hook-touched pools countable from logs/addresses; extractable value needs hook-path sim. |
| Maker OSM kick() | 5 | Frequency A | A / B–C | Easy event count (often 0 in calm months, §17.2). Profit at poke needs next-price / vault safety reconstruction. |
| Maker Clip take() | 5 | Frequency A | A / C | `Take` events exist ($9.99M all-time, sparse lately). Optimal take block = Dutch-curve sim. |
| GMX V2 ADL | 5 | A after indexing | A / C | Dune tables empty (§17.3). Index own ADL/liq logs for frequency; front-run edge still C. |
| Cascading liq | 6 | Weak A | A / C | Liquidation storms visible; constructed cascade is sim + dependency graph. |
| Long-tail arb | 6 | Noisy A; $ → C | A proxy / C | Multi-pool txs ≠ arb (§17.5: $0.26 avg, inflated). Needs cycle/quote sim. |
| ERC-4337 bundler | 6 | Partial A | A / C | UserOp volume reportable; bundler MEV needs alt-mempool + sim. Deferred. |
| Multi-block MEV | 6 | No | C | Needs sim + proposer knowledge. Deferred. |
| Sandwich (excluded from capital-free build, but same rule) | — | No for $ | C | Pattern matching ≠ profit; needs ordering + re-quote sim. |

### Recommended report track (ship before / alongside detectors)

High-signal historical scanners that do **not** need full Phase 1–6 detectors, mempool, or
`scheduler`:

1. **Skim** — realized skims + optional latent drift inventory (A+B); first scanner that
   closes a Dune gap and validates `balance_drift`.
2. **Flash-loan liquidations** — count, volume, bonus estimate (A).
3. **Maker Clip `take` / OSM `kick`** — event frequency + notional (A; expect calm-month zeros).
4. **GMX ADL/liq** — once indexed (A).

Keep behind detectors + simulation / paper (mode C):

- Sandwich, backrun *profit*, multi-hop / long-tail arb
- Interest-liq *attribution*, cascading construction
- V4 hook extractable value, optimal Clip `take` timing

---

## Shared infrastructure (unblocks nearly everything)

1. **`core/src/mev/detectors/balance_drift.rs`** — event-driven token-balance
   accounting per pool. Detects `balanceOf > reserve` drift from `Transfer` logs
   (`ddf252ad...`) vs the stored `UniswapV2PoolState.reserve0/reserve1`. Live-mode
   cross-check via `RpcClient::call` balanceOf (selector `70a08231` already in
   `sigs/fallback_data.rs`). Powers skim, sync race, rebase arb, and the skim
   historical report (modes A+B).
2. **`core/src/mev/scheduler.rs`** — min-heap priority queue of
   `(epoch_block, Action)` + `process_due(current_block)`. Nothing like this exists
   today. Needed by interest-accrual liq, OSM poke timing, and Clip `take()` block
   calc. **Not** required for the report track.
3. **`core/src/types/strategy.rs`** (+ strum names) and **`runner.rs` registration** —
   add variants (`Skim`, `SyncRace`, `InterestLiq`, `Backrun`, `V4HookMev`,
   `MakerOsmKick`, `MakerClipTake`, `GmxAdl`, `CascadingLiq`). A detector plugs in at
   exactly three spots:
   - construction (`runner.rs` ~L430-440)
   - full-replay invocation (`runner.rs` ~L492-583)
   - `sync_block_from_logs` if event-only (log-only path)
   Note: the `strategies` config list currently does **not** gate execution — that stays
   as-is (all detectors run; config is for reporting only).

---

## Build sequence

### Phase 0 — Historical market reports (~2–4 days, parallelizable)

Thin log/storage scanners (explorer-style job or `mev-scout report …`) producing
past-month **N opps / $ revenue** without live execution. Prefer these before investing
in full detectors for the same strategy.

| Scanner | Strategy | Mode | Notes |
|---|---|---|---|
| skim report | 1.1 skim() | A + B | Realized `skim` + `Transfer` out; optional latent `balance − reserve` via `balance_drift`. Closes Dune §17.4 gap. |
| flash-liq report | 4.4 flash-loan atomic liq | A | Flash borrow + liquidation (+ repay) same tx; USD via existing pricing path. |
| maker-keeper report | 4.2 / 4.10 kick + take | A | Event frequency + notional; stress-window aware (calm months → 0). |
| gmx-adl report | 7.8 GMX V2 ADL | A | Own event indexing (Dune empty); frequency only until detector + sim exist. |

### Phase 1 — Trivial (~2–4 days)

| Detector | Strategy | Report mode | Notes |
|---|---|---|---|
| `skim.rs` | 1.1 skim() capture (`Skim`) | A + B (report first) | Capital-free, first-caller wins. Scan V2/V4 pairs via `PoolManager`, use balance-drift tracker; excess tokens → opportunity (`pool_a`=pair, `token_in`=excess token). Runs before sync in each block. |
| `sync_race.rs` | 1.2 sync() race (`SyncRace`) | C for $ | Defensive burn + post-rebase-down `balance < reserve` correction that re-opens arb paths; shares the drift tracker. Value is follow-on — not a standalone revenue report. |

### Phase 2 — Capital-free liquidation (~1 week, extends `liquidation.rs`)

| Detector | Strategy | Report mode | Notes |
|---|---|---|---|
| `interest_liq.rs` | 4.13 interest accrual liq (`InterestLiq`) | A count / B attribution | Proactive forward HF model. Extend `AaveReserveData` to carry `variableBorrowRate` (already fetched by `AaveReserveCache::fetch_reserve`, `liquidation.rs:63-98`); project HF crossing block from debt compounding; hand to the scheduler. Competition 2/10 — near-zero, continuous income. |
| `liquidation.rs` extension | 4.4 flash-loan atomic liq (extends `Liquidation`) | A (strong) | Fee/gas plumbing already exists (`FlashLoanProvider` + `GasConfig::flash_loan_fee`, 0 bps Balancer path). Add flash-borrow → liquidate → swap → repay modeling. Validated market on Polygon: ~323 txs/mo, $493/tx avg (§17.5). Report scanner can ship before this extension. |

### Phase 3 — Backrunning (3–5 days)

| Detector | Strategy | Report mode | Notes |
|---|---|---|---|
| `backrun.rs` | 2.1 backrunning (`Backrun`) | C ($); A only as noisy proxy | Mempool-driven; feeds off `capture_pending_block` + revm post-state simulation (`BlockReplayer`) + `arb_common` quoting; MEV-Share-compatible by design. Highest *validated* demand in the doc (~10K opps/mo, ~$387K est on Polygon). Low capital (flash works). Do **not** treat same-block opposing-swap counts as revenue. |

### Phase 4 — Uniswap V4 hook MEV (~1 week)

| Detector | Strategy | Report mode | Notes |
|---|---|---|---|
| `v4_hook_mev.rs` | 7.11 V4 hook MEV (`V4HookMev`) | A activity / C $ | `UniswapV4PoolState` + `hook_address` already modeled; flags derivable from address byte 17 (`0x08` = beforeSwap). Hook registry → TWAMM-run remaining flow, dynamic-fee jumps, limit-order triggers; flash accounting = zero capital. Competition 2/10, capital-efficiency score 24.5. |

### Phase 5 — Keeper / event-gated (chain-gated via `ChainConfig`, mostly ETH L1)

| Detector | Strategy | Report mode | Notes |
|---|---|---|---|
| `makerdao_osm.rs` | 4.2 OSM preview + kick() (`MakerOsmKick`) | A freq / B–C $ | Capital-free keeper, profitability 9/10. `RpcClient::get_storage_at` exists — read OSM slot 4 next-price, pre-compute unsafe vaults, fire `kick()` via scheduler on poke. Hourly cadence; pays during market stress. |
| `makerdao_clip.rs` | 4.10 Clip Dutch auction take() (`MakerClipTake`) | A freq / C optimal $ | Optimal `take()` block via auction decay simulation; flash via join-adapter callback. Rare in calm markets. |
| `gmx_adl.rs` | 7.8 GMX V2 ADL front-run (`GmxAdl`) | A after index / C edge | Keeper reward; needs GMX DataStore/Reader addresses added to `ChainConfig`; Dune tables empty → on-chain indexing required (report track indexes first). |

### Phase 6 — Advanced capital-free (assemble as infra matures)

| Detector | Strategy | Report mode | Notes |
|---|---|---|---|
| `cascading_liq.rs` | 4.1 cascading liq engineering (`CascadingLiq`, cpx 10) | Weak A / C | Cross-protocol dependency graph + flash-loan routing; highest ceiling ($10K–$100K/event) but hardest; build last. |
| `multi_hop.rs` extension | 2.2 long-tail token arb | C ($) | Extend existing Bellman-Ford (depth 4) to the wider discovered token universe; thin per-opp profit, huge frequency. Multi-pool tx proxies are not revenue. |
| `erc4337_bundler.rs` | 7.4 ERC-4337 AA bundler MEV (`Erc4337Bundler`) | A / C | Needs alt-mempool infra. Deferred/optional. |
| `multi_block.rs` | 8.5 multi-block MEV | C | Needs validator relationships (business problem, not code). Deferred/optional. |

---

## Testing & conventions

- Unit tests via the existing patterns:
  - synthetic pools (`core/tests/arbitrage.rs`)
  - synthetic `ExecutedLog`s (`core/tests/liquidation.rs`)
  - detector-driven (`core/tests/sandwich.rs`)
- Report scanners: synthetic log fixtures for skim / flash-liq fingerprints; optional
  E2E month-window smoke behind `MEV_SCOUT_E2E=1`.
- E2E gated behind `MEV_SCOUT_E2E=1` + `RPC_URL` (`core/tests/common/setup.rs`).
- Detectors are chain-generic; all chain-specific inputs flow through
  `ChainConfig` (`config/defaults.rs`) + `chains.toml`.

## Explicitly excluded

- CEX–DEX arbitrage (incl. top-tier infra-moat pairs).
- Anything requiring real capital: JIT, stat arb, depeg arb, NFT/Convex/Pendle epoch
  plays, V3 range orders, etc.
- Permanently deprioritized (§15): TWAP manipulation, NFT floor arb, governance MEV.
- Treating sandwich / backrun / long-tail **proxy event counts** as revenue without
  simulation (mode C) — use paper/backtest for “our” P&L instead.