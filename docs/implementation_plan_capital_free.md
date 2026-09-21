# Implementation Plan: Capital-Free MEV Detectors (multi-chain)

> Source: `docs/mev_strategies.md` — strategy reference, capital-free inventory (§11),
> Dune validation (§17), and codebase status (§16).
> Scope: **all capital-free, non-CEX strategies**, sequenced, chain-generic.
> Protocol-specific addresses are gated through `ChainConfig` / `chains.toml` (same
> pattern as `aave_v3_pool` today).

---

## Shared infrastructure (unblocks nearly everything)

1. **`core/src/mev/detectors/balance_drift.rs`** — event-driven token-balance
   accounting per pool. Detects `balanceOf > reserve` drift from `Transfer` logs
   (`ddf252ad...`) vs the stored `UniswapV2PoolState.reserve0/reserve1`. Live-mode
   cross-check via `RpcClient::call` balanceOf (selector `70a08231` already in
   `sigs/fallback_data.rs`). Powers skim, sync race, and rebase arb later.
2. **`core/src/mev/scheduler.rs`** — min-heap priority queue of
   `(epoch_block, Action)` + `process_due(current_block)`. Nothing like this exists
   today. Needed by interest-accrual liq, OSM poke timing, and Clip `take()` block
   calc.
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

### Phase 1 — Trivial (~2–4 days)

| Detector | Strategy | Notes |
|---|---|---|
| `skim.rs` | 1.1 skim() capture (`Skim`) | Capital-free, first-caller wins. Scan V2/V4 pairs via `PoolManager`, use balance-drift tracker; excess tokens → opportunity (`pool_a`=pair, `token_in`=excess token). Runs before sync in each block. |
| `sync_race.rs` | 1.2 sync() race (`SyncRace`) | Defensive burn + post-rebase-down `balance < reserve` correction that re-opens arb paths; shares the drift tracker. |

### Phase 2 — Capital-free liquidation (~1 week, extends `liquidation.rs`)

| Detector | Strategy | Notes |
|---|---|---|
| `interest_liq.rs` | 4.13 interest accrual liq (`InterestLiq`) | Proactive forward HF model. Extend `AaveReserveData` to carry `variableBorrowRate` (already fetched by `AaveReserveCache::fetch_reserve`, `liquidation.rs:63-98`); project HF crossing block from debt compounding; hand to the scheduler. Competition 2/10 — near-zero, continuous income. |
| `liquidation.rs` extension | 4.4 flash-loan atomic liq (extends `Liquidation`) | Fee/gas plumbing already exists (`FlashLoanProvider` + `GasConfig::flash_loan_fee`, 0 bps Balancer path). Add flash-borrow → liquidate → swap → repay modeling. Validated market on Polygon: ~323 txs/mo, $493/tx avg (§17.5). |

### Phase 3 — Backrunning (3–5 days)

| Detector | Strategy | Notes |
|---|---|---|
| `backrun.rs` | 2.1 backrunning (`Backrun`) | Mempool-driven; feeds off `capture_pending_block` + revm post-state simulation (`BlockReplayer`) + `arb_common` quoting; MEV-Share-compatible by design. Highest *validated* demand in the doc (~10K opps/mo, ~$387K est on Polygon). Low capital (flash works). |

### Phase 4 — Uniswap V4 hook MEV (~1 week)

| Detector | Strategy | Notes |
|---|---|---|
| `v4_hook_mev.rs` | 7.11 V4 hook MEV (`V4HookMev`) | `UniswapV4PoolState` + `hook_address` already modeled; flags derivable from address byte 17 (`0x08` = beforeSwap). Hook registry → TWAMM-run remaining flow, dynamic-fee jumps, limit-order triggers; flash accounting = zero capital. Competition 2/10, capital-efficiency score 24.5. |

### Phase 5 — Keeper / event-gated (chain-gated via `ChainConfig`, mostly ETH L1)

| Detector | Strategy | Notes |
|---|---|---|
| `makerdao_osm.rs` | 4.2 OSM preview + kick() (`MakerOsmKick`) | Capital-free keeper, profitability 9/10. `RpcClient::get_storage_at` exists — read OSM slot 4 next-price, pre-compute unsafe vaults, fire `kick()` via scheduler on poke. Hourly cadence; pays during market stress. |
| `makerdao_clip.rs` | 4.10 Clip Dutch auction take() (`MakerClipTake`) | Optimal `take()` block via auction decay simulation; flash via join-adapter callback. Rare in calm markets. |
| `gmx_adl.rs` | 7.8 GMX V2 ADL front-run (`GmxAdl`) | Keeper reward; needs GMX DataStore/Reader addresses added to `ChainConfig`; Dune tables empty → on-chain indexing required. |

### Phase 6 — Advanced capital-free (assemble as infra matures)

| Detector | Strategy | Notes |
|---|---|---|
| `cascading_liq.rs` | 4.1 cascading liq engineering (`CascadingLiq`, cpx 10) | Cross-protocol dependency graph + flash-loan routing; highest ceiling ($10K–$100K/event) but hardest; build last. |
| `multi_hop.rs` extension | 2.2 long-tail token arb | Extend existing Bellman-Ford (depth 4) to the wider discovered token universe; thin per-opp profit, huge frequency. |
| `erc4337_bundler.rs` | 7.4 ERC-4337 AA bundler MEV (`Erc4337Bundler`) | Needs alt-mempool infra. Deferred/optional. |
| `multi_block.rs` | 8.5 multi-block MEV | Needs validator relationships (business problem, not code). Deferred/optional. |

---

## Testing & conventions

- Unit tests via the existing patterns:
  - synthetic pools (`core/tests/arbitrage.rs`)
  - synthetic `ExecutedLog`s (`core/tests/liquidation.rs`)
  - detector-driven (`core/tests/sandwich.rs`)
- E2E gated behind `MEV_SCOUT_E2E=1` + `RPC_URL` (`core/tests/common/setup.rs`).
- Detectors are chain-generic; all chain-specific inputs flow through
  `ChainConfig` (`config/defaults.rs`) + `chains.toml`.

## Explicitly excluded

- CEX–DEX arbitrage (incl. top-tier infra-moat pairs).
- Anything requiring real capital: JIT, stat arb, depeg arb, NFT/Convex/Pendle epoch
  plays, V3 range orders, etc.
- Permanently deprioritized (§15): TWAP manipulation, NFT floor arb, governance MEV.