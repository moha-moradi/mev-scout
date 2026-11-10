# Missed mevlive arbs — filter vs bug (final)

Window `99833048..=99836394`. mevlive 47 arbs, we matched 5, **missed 42**.
Reindexed with `record_rejections=true`: every miss has an `explorer_classify` reject.

> **Historical audit (2026-11-09).** All 42 are now matched — final result
> **47/47** (2026-11-10, `after_attribution_fix`). The 23 direction-bug and 15
> "definition gap" cases closed once the committed V3 fix set both
> `token_in`/`token_out` sentinels; run1's binary predated that half of the fix.
> The multi-recipient attribution fix also corrected the under-reported
> forwarded-profit case (`0x29aba…`). Keep this as the root-cause record.

## Bottom line

- **23/42 BUG**: decoded `token_in`/`token_out` opposite of ERC-20 flow to/from the pool on ≥1 hop → directed cycle missing → `unresolved_cycle`. Re-inferring direction from transfers closes the cycle.
- **3/42 INTENTIONAL**: closed cycle found, rejected as `net_zero_searcher` (no attributable anchor profit / tip). Tiny mevlive USD (~$0.007–$0.016).
- **15/42 DEFINITION GAP** (likely intentional vs Exact): still no ≥2-pool directed cycle after transfer-based direction repair. mevlive labels arb anyway.
- **1/42 residual parallel-pair**: same token pair on 2+ pools; directions still not anti-parallel after transfer infer (worth a second look / same bug family).

## Arb reject reasons (primary)

| reason | count |
|---|---|
| `unresolved_cycle` | 39 |
| `net_zero_searcher` | 3 |

## Direction-flip AMM histogram (hops flipped vs transfers)

| amm | flipped hops |
|---|---|
| `v3` | 33 |

## Root cause (direction bug)

`apply_signed_deltas` in `core/src/explorer/decode/swap.rs` treats **negative** V3 `amount0` as token-in. Uniswap V3 pool deltas are the opposite: **positive** `amount0` means the pool received token0 (user sold token0).

Flipping every V3 edge still leaves a directed cycle, so **pure-V3** routes often still classify. **Mixed V2/LB + V3** routes break: V2 directions stay correct, V3 hops are inverted → DAG instead of cycle → `unresolved_cycle`.

All 33 transfer-vs-decode flips in this sample were on `amm=v3`.

## Example (direction bug)

Tx `0x1c6d5faa…d918` — decode edges form a DAG; transfer-inferred flips the WETH.e/WAVAX hop and closes `WETH.e → PNG → WAVAX → WETH.e`.

## Per miss

| block | usd | bucket | reject | hash |
|---|---|---|---|---|
| 99833048 | 0.0079 | `INTENTIONAL_net_zero_searcher` | `net_zero_searcher` | `0x8d67e6ce69bdffea9abf87044f5f8c95ee3a88f33877f35e9e48345bfa0fdd1c` |
| 99833054 | 0.0137 | `BUG_swap_direction_vs_transfers` | `unresolved_cycle` | `0x1c6d5faa063b3857050d2cdfe6609573ee89f0409c685a650e5fdf894f18d918` |
| 99833989 | 0.0092 | `DEFINITION_GAP_no_cycle_even_with_transfer_dirs` | `unresolved_cycle` | `0x9bff44bdcb297af0f6d35c0c0354bdb5fbea1cb0b3eb3d08486aab4ef114f79c` |
| 99833991 | 0.0095 | `DEFINITION_GAP_no_cycle_even_with_transfer_dirs` | `unresolved_cycle` | `0x636502744e95b9b943b46da852a5172b2bf36cc3ad4145691ec0f47fae0b9575` |
| 99834108 | 0.0124 | `DEFINITION_GAP_no_cycle_even_with_transfer_dirs` | `unresolved_cycle` | `0x1d5c42a1cf32d9bf64559a0db51671af6c1b3206265b979f9b210253b271ec36` |
| 99834157 | 0.0079 | `BUG_swap_direction_vs_transfers` | `unresolved_cycle` | `0xeeba59660c3d3168d3476d5e8547a15c85a749f1aadb56739ab7f85c866bc423` |
| 99834175 | 0.0144 | `DEFINITION_GAP_no_cycle_even_with_transfer_dirs` | `unresolved_cycle` | `0xf8e371ae3e74be09200cbd976c4ab0181edf991533aabe08cd7d034fdc9e1433` |
| 99834189 | 0.0062 | `DEFINITION_GAP_no_cycle_even_with_transfer_dirs` | `unresolved_cycle` | `0xb21da21e76b3d0ccc613963ea54af38c13e239d428ab939c7f1c74e24d275444` |
| 99834195 | 0.0153 | `DEFINITION_GAP_no_cycle_even_with_transfer_dirs` | `unresolved_cycle` | `0x41cdb9c9e5e566d5c4832e2dd5f23f8c4e31fcdcbf890086f763f35a01054cf8` |
| 99834217 | 0.0106 | `BUG_swap_direction_vs_transfers` | `unresolved_cycle` | `0x88163f3a901a424e6296f086e319a13b51cd4691f68e010e92b8f6c2aebe7c9b` |
| 99834303 | 0.0087 | `BUG_swap_direction_vs_transfers` | `unresolved_cycle` | `0x0832d91743f8a76eb0a9efbe3dd4aaf023155022aec31c4e242e624de7d10c26` |
| 99834303 | 0.0104 | `BUG_swap_direction_vs_transfers` | `unresolved_cycle` | `0x8d367577840c01a340235a096f81ef6774826398383905f7180c776a62bd32c6` |
| 99834304 | 0.0072 | `BUG_swap_direction_vs_transfers` | `unresolved_cycle` | `0x9d38017cb48b770f4e610b7ef9211e8d99101590286da0891a3f2f204294fb88` |
| 99834305 | 0.0099 | `BUG_swap_direction_vs_transfers` | `unresolved_cycle` | `0xef49a3a3a07d58c2b7d2652b5375f3b05402edc2c25e6caf73767c7469c896b9` |
| 99834367 | 0.0117 | `BUG_swap_direction_vs_transfers` | `unresolved_cycle` | `0xdd292a3e2f7c8a72a03f8a23943169e2b9cccbd59c74ca84fc96f56ffc0003ea` |
| 99834440 | 0.0051 | `BUG_swap_direction_vs_transfers` | `unresolved_cycle` | `0xa61e47b1163e5630addd855ceff6e6ca14c3a3e440db2d538ce8dca55264dad0` |
| 99834506 | 0.0064 | `BUG_swap_direction_vs_transfers` | `unresolved_cycle` | `0x749d23aee8e4168330f36c5234ca7c824218181aada3f8db389460161260cdb0` |
| 99834506 | 0.0108 | `DEFINITION_GAP_no_cycle_even_with_transfer_dirs` | `unresolved_cycle` | `0x89c565bf9b223009ab5963c84973f557101595167fd196c3ba784b6e6b20c8cc` |
| 99834514 | 0.0101 | `DEFINITION_GAP_no_cycle_even_with_transfer_dirs` | `unresolved_cycle` | `0x3776e8d48bd03804b47b477068582c1a3b4767063d738b7a63b92acbd3b3e3de` |
| 99834514 | 0.0164 | `INTENTIONAL_net_zero_searcher` | `net_zero_searcher` | `0x62434bf32400f965097e4a15363003696ab55fede62a84efa490f3c73cccea84` |
| 99834516 | 0.0068 | `DEFINITION_GAP_no_cycle_even_with_transfer_dirs` | `unresolved_cycle` | `0x05b34236ae3f67225884c10a2165184d265f9aa0c7a3b75ccf54f791e4140055` |
| 99834522 | 0.0072 | `BUG_swap_direction_vs_transfers` | `unresolved_cycle` | `0x3791c8ea9317bebd96f94331d1227ffbf9c8ae17a2d6bb6f8f6ac7b33f43c14d` |
| 99834522 | 0.0069 | `INTENTIONAL_net_zero_searcher` | `net_zero_searcher` | `0xa47306026e21718807f3aaa26abe78debabdf5a5873815c7c8d9da0823ac4b31` |
| 99834524 | 0.0575 | `BUG_swap_direction_vs_transfers` | `unresolved_cycle` | `0xe842b691ee44458e63a5a24e85debafc1512ba1c9a2dd96cbd73c69233825563` |
| 99834525 | 0.0048 | `BUG_swap_direction_vs_transfers` | `unresolved_cycle` | `0x5173eb945bb9ae4477614b9647bce4626ad1ecdf2ab24af96bcbe4309f6cdbf7` |
| 99834525 | 0.0132 | `BUG_swap_direction_vs_transfers` | `unresolved_cycle` | `0xf4d5ec379834438135903556383ad1990654d7cfa0178228181d79425db4028f` |
| 99834527 | 0.0087 | `BUG_swap_direction_vs_transfers` | `unresolved_cycle` | `0x21897bf48a1b41d10f87ea0e68ec574dc23177c4c0dcd62fbbc6b65beb2a09bc` |
| 99834527 | 0.0085 | `BUG_swap_direction_vs_transfers` | `unresolved_cycle` | `0xe2a0a5de87c3b15028b59f3a7086b56a7eba0b9b94b0f9890525073f2a2a7f59` |
| 99834528 | 0.0102 | `BUG_swap_direction_vs_transfers` | `unresolved_cycle` | `0x03a995d539e080f50b8b4696786c1933eaa866d7fdccdce15d5d6590b3d8a2b7` |
| 99834580 | 0.0123 | `DEFINITION_GAP_no_cycle_even_with_transfer_dirs` | `unresolved_cycle` | `0x95b03a2e89e017230450591e35a97dc3a6d13cac0416181a092cb944a01b37eb` |
| 99834604 | 0.0077 | `BUG_swap_direction_vs_transfers` | `unresolved_cycle` | `0x8da1a036265a626109a2263e943ffd35bb2ddde00cb98a711b36424506cdc340` |
| 99834609 | 0.0083 | `DEFINITION_GAP_no_cycle_even_with_transfer_dirs` | `unresolved_cycle` | `0xd01a4a8d16cfdb604fcc7bdf158998c66d1e0d49a96b5f7593630f8128be5934` |
| 99834612 | 0.0202 | `BUG_swap_direction_vs_transfers` | `unresolved_cycle` | `0xd0b6a1fd4d987cc8d3ae83e278f8a393579dded0098b686493b1c141277ec644` |
| 99834664 | 0.0095 | `DEFINITION_GAP_no_cycle_even_with_transfer_dirs` | `unresolved_cycle` | `0x59c38a7c898e74d943755a4f7248091ee24eec7045c35ec73eff58a8ed5b955b` |
| 99835474 | 0.0213 | `DEFINITION_GAP_no_cycle_even_with_transfer_dirs` | `unresolved_cycle` | `0x29aba6979c579799e8e8d871d77a221b9a9c4f906b567da70b08d560c638462b` |
| 99835965 | 0.0197 | `BUG_swap_direction_vs_transfers` | `unresolved_cycle` | `0xf31019f1b52edef7e133bcb3ab6270514350cf53e06d5ab351ccd4a971e31415` |
| 99836214 | 0.0055 | `DEFINITION_GAP_no_cycle_even_with_transfer_dirs` | `unresolved_cycle` | `0x8c59f6519ac7f6f739150076552eb8ae55e6dca8c0ed56fbde12086a8fd78f74` |
| 99836261 | 0.0119 | `BUG_swap_direction_vs_transfers` | `unresolved_cycle` | `0x48ee736d741c8c0fcfc654e7df4a51197de5ec32fec660cbdb87368149dc4281` |
| 99836268 | 0.0100 | `BUG_swap_direction_vs_transfers` | `unresolved_cycle` | `0x0303062037c786ff2bb168c47341196b54b52040f772049d687607ff261cbd11` |
| 99836276 | 0.0078 | `BUG_swap_direction_vs_transfers` | `unresolved_cycle` | `0x3b7b1aa00d00ff71e784c04a967fa3dddb2cd06bd773722c79cfdc5b47b8487b` |
| 99836354 | 0.0058 | `LIKELY_BUG_or_mevlive_parallel_arb` | `unresolved_cycle` | `0x8f5105061196a0f9e9dccfb975f7656e129604c1237a9be429c0e9cf069260a9` |
| 99836394 | 0.0135 | `DEFINITION_GAP_no_cycle_even_with_transfer_dirs` | `unresolved_cycle` | `0x675caa7d06850765efcfa02f5f2f1db4ff129b879407371981c2a1570e87e598` |

## Sources

- `results/missed_arb_audit_final.json`
- `results/missed_arb_audit_deep.json`
- classifier: `core/src/explorer/classify/arb.rs` (`unresolved_cycle` / `net_zero_searcher`)
- direction attach: `core/src/explorer/decode/swap.rs` + `attach.rs`
