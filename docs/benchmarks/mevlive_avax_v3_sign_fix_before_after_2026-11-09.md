# Before/after V3 sign fix — same window

Window `99833048..=99836394`. mevlive arbs frozen: 47.

| metric | before (wrong V3 sign) | after run1 |
|---|---|---|
| matched ∩ mevlive | 5 | **18** |
| missed mevlive | 42 | **29** |
| our `arb_atomic` | 21 | **35** |
| our total ops | 79 | 94 |

Newly recovered vs mevlive: **15** (lost prior: 2).
Of the 23 previously labeled `BUG_swap_direction`: recovered **12**, still missed 11.

## Newly matched hashes

- `0x0303062037c786ff2bb168c47341196b54b52040f772049d687607ff261cbd11` was-BUG-direction
- `0x03a995d539e080f50b8b4696786c1933eaa866d7fdccdce15d5d6590b3d8a2b7` was-BUG-direction
- `0x0832d91743f8a76eb0a9efbe3dd4aaf023155022aec31c4e242e624de7d10c26` was-BUG-direction
- `0x3b7b1aa00d00ff71e784c04a967fa3dddb2cd06bd773722c79cfdc5b47b8487b` was-BUG-direction
- `0x48ee736d741c8c0fcfc654e7df4a51197de5ec32fec660cbdb87368149dc4281` was-BUG-direction
- `0x62434bf32400f965097e4a15363003696ab55fede62a84efa490f3c73cccea84` was-net-zero
- `0x88163f3a901a424e6296f086e319a13b51cd4691f68e010e92b8f6c2aebe7c9b` was-BUG-direction
- `0x8d367577840c01a340235a096f81ef6774826398383905f7180c776a62bd32c6` was-BUG-direction
- `0xa47306026e21718807f3aaa26abe78debabdf5a5873815c7c8d9da0823ac4b31` was-net-zero
- `0xd01a4a8d16cfdb604fcc7bdf158998c66d1e0d49a96b5f7593630f8128be5934` was-DEF-GAP
- `0xd0b6a1fd4d987cc8d3ae83e278f8a393579dded0098b686493b1c141277ec644` was-BUG-direction
- `0xdd292a3e2f7c8a72a03f8a23943169e2b9cccbd59c74ca84fc96f56ffc0003ea` was-BUG-direction
- `0xe2a0a5de87c3b15028b59f3a7086b56a7eba0b9b94b0f9890525073f2a2a7f59` was-BUG-direction
- `0xe842b691ee44458e63a5a24e85debafc1512ba1c9a2dd96cbd73c69233825563` was-BUG-direction
- `0xf4d5ec379834438135903556383ad1990654d7cfa0178228181d79425db4028f` was-BUG-direction

## Run1 vs Run2 (stability)

| metric | run1 | run2 |
|---|---|---|
| matched | 18 | 6 |
| missed | 29 | 41 |
| arb_atomic | 35 | 12 |

Identical match set + kind counts: **False**
