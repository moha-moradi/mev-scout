# Explorer strategy verify checklist

Working status for the hunt -> explorer-index -> pin loop (2026-10-18 run).

Status values: `pending` | `candidate_block` | `verified` | `sparse_cross_chain` | `blocked`

| id | label | priority | status | chain | seed_from | seed_to | notes |
|----|-------|----------|--------|-------|-----------|---------|-------|
| baseline.arb | arb_atomic | smoke | verified | avalanche | 95681722 | 95682322 | corpus Seeded |
| baseline.liquidation | liquidation | smoke | verified | avalanche | 95682033 | 95682033 | corpus Seeded |
| baseline.sandwich | sandwich | B | sparse_cross_chain | ethereum | 26089508 | 26089508 | AVAX empty; ETH DB verified |
| baseline.frontrun | frontrun | A | pending | - | | | not in indexed windows |
| baseline.backrun | backrun | A | pending | - | | | not in indexed windows |
| baseline.jit | jit | A | verified | avalanche | 97783274 | 97783274 | Seeded in scenario_targets |
| baseline.skim | skim | A | pending | - | | | not observed |
| P0.1 | flash_loan_liq | A | verified | avalanche | 97782322 | 97782322 | Aave V2 flash+liq; `aave_v2_pool` remap → details.protocol=aave_v2 |
| P0.2 | benqi | A | verified | avalanche | 97850836 | 97850836 | non-indexed LiquidateBorrow; c_token_underlyings=22 (core+isolated) |
| polygon.liquidation | liquidation | smoke | verified | polygon | 94430973 | 94431566 | corpus poly-liquidation-*; protocol=aave_v3 |
| bsc.aave | liquidation | A | pending | bsc | | | no V2 on BSC **verified** (0x6c81… LendingPool has no code, V3 0x6807… live; `aave_v2_pool` correctly unpinned → BSC Aave labels `aave_v3`). Explorer BSC ingest path confirmed (block+receipts, no getLogs). Left: corpus seed `bsc-aave-<block>` (assert scaffolding added) — needs a real BSC liquidation block; public RPC blocks getLogs, needs archive token (e.g. allnodes/dwellir) to hunt. |
| bsc.venus | liquidation | A | candidate_block | bsc | | | 55 vToken aliases + 54 underlyings pinned; BSC Venus labels `venus` (verified). Explorer BSC ingest path confirmed. Left: corpus seed `bsc-venus-<block>` (assert scaffolding added) — needs a real BSC Venus liquidation block; public RPC blocks getLogs, needs archive token to hunt. |
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
