# Explorer strategy Phase-2 frequency / volume snapshot

Last refresh: **2026-10-18T09:09:53.886Z** (interim while AVAX `--days 7` backfill runs).

Source JSON: `cache/phase2_coverage.json`.

## Avalanche (`cache/explorer-avalanche.sqlite`) ? IN PROGRESS

- Historical window target: ~97562463..=97864863 (~302401 blocks, archive = dwellir + avalanche.drpc.org)
- ops so far: **831** across blocks **97505001..97857389**
- classified_blocks (DB total): **36449**
- kinds: unknown 401 (~28 USD), arb_atomic 364 (~6062 USD), jit 54, liquidation 12 (~-191869785 USD)
- tags: bundler 391, erc4337 391, long_tail 320, epoch_transition 24, rate_provider_staleness 11, claim_and_sell 10, airdrop 10, curve_imbalance 6, flash_loan_liq 4, savax_rate_arb 2
- flags: {"interest_accrued":7}

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
