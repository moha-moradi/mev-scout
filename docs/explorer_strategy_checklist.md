# Explorer strategy verify checklist

Working status for the hunt -> explorer-index -> pin loop. 2026-10-24 re-hunt (current classifier): recent Avalanche window 98415954..=98416254 (301 blocks) and dense window 95681722..=95682322 = 0 frontrun/backrun/skim ops. 2026-10-25 ETH re-hunt (drpc, 26264000..=26264962, 963 blocks): 2 backrun seeds (26264539, 26264858), 0 frontrun, 0 skim — Phase 3 causal seed **pinned**. 2026-10-25 AVAX re-hunt (dwellir archive RPC, 98489000..=98494000, 5001 blocks): 0 frontrun/backrun/skim — confirms causal is ETH-side; skim limited to the 1 V2-like pool in the (near-empty) AVAX scanner cache. (ETH full window 26264000..=26267500 only partially indexed; AVAX full window fully indexed via dwellir.) Classifier not loosened.

Status values: `pending` | `candidate_block` | `verified` | `sparse_cross_chain` | `blocked`

| id | label | priority | status | chain | seed_from | seed_to | notes |
|----|-------|----------|--------|-------|-----------|---------|-------|
| baseline.arb | arb_atomic | smoke | verified | avalanche | 95681722 | 95682322 | corpus Seeded |
| baseline.liquidation | liquidation | smoke | verified | avalanche | 95682033 | 95682033 | corpus Seeded |
| baseline.sandwich | sandwich | B | sparse_cross_chain | ethereum | 26089508 | 26089508 | AVAX empty; ETH DB verified |
| baseline.frontrun | frontrun | A | pending | - | | | not observed; 2026-10-25 ETH re-hunt (963 blk) 0; AVAX 2026-10-25 (5001 blk, 98489000..=98494000) 0 |
| baseline.backrun | backrun | A | verified | ethereum | 26264539 | 26264858 | ETH re-hunt 2026-10-25 drpc (963 blk): 2 seeds (searcher 0xe8509…, 0xb52dae…); corpus `eth-backrun-*` |
| baseline.jit | jit | A | verified | avalanche | 97783274 | 97783274 | Seeded in scenario_targets |
| baseline.skim | skim | A | pending | - | | | not observed; AVAX 2026-10-25 (5001 blk, 98489000..=98494000, dwellir) 0; ETH re-hunt (963 blk) 0 |
| P0.1 | flash_loan_liq | A | verified | avalanche | 97782322 | 97782322 | Aave V2 flash+liq; `aave_v2_pool` remap → details.protocol=aave_v2 |
| P0.2 | benqi | A | verified | avalanche | 97850836 | 97850836 | non-indexed LiquidateBorrow; c_token_underlyings=22 (core+isolated) |
| polygon.liquidation | liquidation | smoke | verified | polygon | 94430973 | 94431566 | corpus poly-liquidation-*; protocol=aave_v3 |
| polygon.aave_v2 | liquidation | A | verified | polygon | 95893023 | 95893023 | corpus `poly-aave-v2-95893023`; `aave_v2_pool` pin; searcher `0xd9ff…76c3` |
| bsc.aave | liquidation | A | verified | bsc | 127706714 | 127706714 | corpus `bsc-aave-127706714`; protocol=aave_v3 (no V2 on BSC); searcher `0xec40d8aa…1118` |
| bsc.venus | liquidation | A | verified | bsc | 102639252 | 102639252 | corpus `bsc-venus-102639252`; protocol=venus (vUSDT emitter); searcher `0x6898c2c1…e052` |
| P0.3 | buycollateral | C | pending | ethereum | | | no BuyCollateral in tip 40k |
| P0.4 | flash_arb | A | pending | avalanche | | | flashes rare; no arb co-hit |
| P1.1 | interest_accrued | D | sparse_cross_chain | ethereum | 26214788 | 26214788 | Morpho liq flag |
| P1.3 | long_tail | A | verified | avalanche | 97783552 | 97783552 | Seeded |
| P1.4 | oracle_poke_block | D | pending | - | | | no co-block poke+liq |
| P1.5 | keeper | A | pending | - | | | Gelato addrs: 0 hits / 120k |
| P2.3 | flash_providers | D | pending | - | | | needs multi-provider flash liq |
| P3.1 | lb_bin_jit | B | pending | avalanche | | | needs address-bound LB hunt |
| P3.2 | rate_provider_staleness | B | pending | - | | | |
| P3.5 | curve_imbalance | B | pending | - | | | |
| P3.7 | gmx_adl_arb | B | pending | avalanche | | | emitter quiet / candidate pruned |
| P3.11 | bundler | A | verified | avalanche | 97505001 | 97505001 | Seeded |
| P3.12 | fot_arb|rebase_arb | B | pending | - | | | |
| P3.13 | claim_and_sell | B | verified | avalanche | 97781738 | 97781738 | Seeded |
| P3.14 | bad_debt_liq | C | sparse_cross_chain | ethereum | 26214788 | 26214788 | AVAX SyntheticOnly |
| P3.15 | epoch_transition | B | pending | - | | | |
| P3.16 | savax_rate_arb | B | pending | avalanche | | | transfer-only blocks had 0 arb |

## Tooling

- `scripts/hunt_strategy.py` / `scripts/hunt_strategy.mjs` -- eth_getLogs hunt
- `scripts/verify_explorer_block.mjs` -- index + tag/kind assert
- `scripts/tag_coverage.py` / `scripts/_coverage_report.mjs` -- coverage + profit sums
