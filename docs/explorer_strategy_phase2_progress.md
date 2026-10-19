# Explorer strategy Phase-2 progress (resume snapshot)

Generated: **2026-10-19T09:30:05Z**. Backfill job **stopped** (user request).
DB remains authoritative for resume: `cache/explorer-avalanche.sqlite`.

Companion / older interim notes: `docs/explorer_strategy_phase2.md`.
Raw JSON: `cache/phase2_coverage.json`.

## How to read these numbers

- **DB totals** only grow. Gap-resume skips already-classified blocks. Rely on the DB.
- **`--days 7` window %** is a *sliding tip window*. When tip moves, old classified blocks can fall *outside* the new window even though they stay in the DB. Do not treat a drop in window % as lost work.
- Until `Backfill done`, frequency/volume figures are **interim**, not Opps/Mo.

## Avalanche progress (authoritative)

| metric | value |
|--------|-------|
| status | **STOPPED** (resumable) |
| classified blocks (DB total) | **98332** |
| mev_ops (DB total) | **1975** |
| ops block range (min..max) | **97505001..97857389** |
| classified max in current window | **97857398** (still gap-filling below tip) |

### Current `--days 7` tip window (this resume session)

- Window: `97639634..=97942034` (~302401 blocks)
- Classified **inside this window**: **38216** (~**12.6%**), remain ~264185
- Ops **inside this window**: **661**

### Prior pause snapshot (2026-10-18, for comparison)

- Window then: `97562463..=97864863`
- Classified inside that window: ~62k (~21%); DB total then: 63583; ops: 1391
- Overlap with today's window is still in the DB; blocks below `97639634` aged out of the rolling 7d % only.

## Avalanche kinds / tags (DB-wide interim)

Kinds: unknown 1266, arb_atomic 540 (~8465 USD), jit 150, liquidation 19

Tags (top): bundler/erc4337 1228, long_tail 479, claim_and_sell/airdrop 39, epoch_transition 24, rate_provider_staleness 17, flash_loan_liq 10, curve_imbalance 10, savax_rate_arb 4

Flags: see `cache/phase2_coverage.json` (`interest_accrued` present).

**Caveat:** liquidation `profit_usd` aggregates are untrusted (pricing bug; huge negative in JSON). Ignore for ranking.

## Other chains (unchanged forensic slices)

- Polygon: 3 ops (bundler x2, arb_atomic x1)
- Ethereum: 508 ops @ 26059586..26214788 (arb-heavy; sandwich/bad_debt/interest flags as before)

## Resume

Prefer a **fixed** range so % stays comparable:

```powershell
mev-scout -f mev-scout.phase2-avax.toml explorer index --from-block 97639634 --to-block 97942034
```

Or rolling tip again:

```powershell
node scripts/run_backfill.mjs
# = explorer index --days 7
```

Then refresh:

```powershell
node scripts/coverage_report_sqljs.js
```

## RPC note (why slow)

At stop time: dwellir#1 flaky/cooldowns, dwellir#2 often 429, publicnode OK, drpc unreliable for `eth_getBlockReceipts`. Local AvalancheGo was down. Local path is `mev-scout.local.toml` when the node is up.
