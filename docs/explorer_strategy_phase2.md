Superseded for live progress by [`docs/explorer_strategy_phase2_progress.md`](explorer_strategy_phase2_progress.md) (2026-10-19 stop snapshot).

# Explorer strategy Phase-2 frequency / volume snapshot

Last refresh: **2026-10-18T09:40:23.486Z** (paused; resume with `node scripts/run_backfill.mjs`).

Source JSON: `cache/phase2_coverage.json`.

## Avalanche (`cache/explorer-avalanche.sqlite`) - PAUSED (~21% of 7d window classified)

- Historical window target: ~97562463..=97864863 (~302401 blocks, archive = dwellir + avalanche.drpc.org)
- ops so far: **1391** across blocks **97505001..97857389**
- classified_blocks (DB total): **63583** (~62k inside 7d window)
- kinds: unknown 827 (~64 USD), arb_atomic 450 (~8082 USD), jit 96, liquidation 18 (~-191869785 USD)
- tags: bundler 800, erc4337 800, long_tail 393, claim_and_sell 28, airdrop 28, epoch_transition 24, rate_provider_staleness 14, flash_loan_liq 9, curve_imbalance 9, savax_rate_arb 4
- flags: {"interest_accrued":8}

## Polygon

- ops: **3**; kinds: unknown 2 (~0 USD), arb_atomic 1; tags: bundler 2, erc4337 2

## Ethereum

- ops: **508** across **26059586..26214788**
- kinds: arb_atomic 473 (~11641 USD), owner_salvage 12, sandwich 8 (~1 USD), unknown 7 (~0 USD), jit 6, liquidation 1, refinance 1
- tags: bundler 7, erc4337 7, long_tail 1, bad_debt_liq 1; flags: {"interest_accrued":1,"bad_debt":1}

## Measure command

```powershell
mev-scout -f mev-scout.phase2-avax.toml explorer index --days 7
node scripts/coverage_report_sqljs.js
```
