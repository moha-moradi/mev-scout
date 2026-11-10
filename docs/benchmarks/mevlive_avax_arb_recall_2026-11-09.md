# Benchmark: mevlive vs mev-scout explorer (Avalanche arb recall)

Frozen window for classifier recall work. **Final result: 47/47 mevlive arbs matched (2026-11-10).** Revisit when arb detection improves.

## Scope

| Field | Value |
|---|---|
| Chain | Avalanche C-Chain (`43114`) |
| Block range (inclusive) | `99833048` .. `99836394` |
| Block count | 3347 |
| Wall-clock (mevlive `time`) | `2026-11-09T08:00:50Z` .. `2026-11-09T08:54:05Z` |
| Snapshot UTC | cutoff `2026-11-09T07:56:18.404865+00:00` / fetched `2026-11-09T08:56:18.404865+00:00` |
| mevlive source | `GET /api/avalanche/arbitrages?per_page=100&sortBy=-created_at` (last-1h filter) |
| Liquidations in window | 0 |
| Our index command | `explorer index --from-block 99833048 --to-block 99836394` |
| Artifacts | `results/mevlive_arbitrages_1h.json`, `results/compare_after_attribution_fix.json`, `cache/explorer-avalanche.sqlite` |

## Final result (2026-11-10, `after_attribution_fix`)

Full V3 decoder fix (`887067c`, both token sentinels) + the three classifier fixes
(`8361872`) + the multi-recipient attribution fix.

| Metric | Value |
|---|---|
| Blocks indexed | **3347 / 3347** (0 failed chunks, 386.2s) |
| mevlive arbitrages | **47** |
| our `arb_atomic` | **108** |
| our total ops | **167** (`arb_atomic` 108, `utility` 44, `jit` 15) |
| hash overlap (matched) | **47 / 47 (100%)** |
| mevlive missed by us | **0** |
| vs run1 | **+29 matched, 0 lost** |
| vs `after_three_fixes` | +1 our-only arb, 0 lost |

### Profit accuracy (47 matched)

- Profit token matches mevlive: **47 / 47**.
- 46 priced; median relative error **0.40%**, p90 **1.04%**.
- Totals: ours **$0.5963** vs mevlive **$0.5432**. Excluding two bundled-route
  outliers (`0xdd292a…`, `0xe2a0a5…`), **$0.5221 vs $0.5231 (0.19% off)**.
- One unpriced match: `0x62434b…` (USDT.e price missing; classification is Exact).
- `0x29aba…` (the multi-recipient attribution case) now attributes the
  `0xe0556223…` payee: 0.0026869 WAVAX, **$0.02128 vs mevlive $0.02133 (0.23%)**.
- Our-only: 61 arbs, total **$1.0409**, max **$0.1952**, concentrated in known
  arb bots (`0xfa606c5b…` 20, `0x476ee0…` 19, `0xe0556223…` 11,
  `0xafd6d126…` 6). No large phantom profits; the old `$841/$530/$394`
  one-way-flow rows remain rejected.

#### Outlier and our-only PnL breakdown

Gross is compared with mevlive's `usdProfit`; gas and net are our values only.
The two bundled-route outliers account for essentially the entire aggregate gap.

| Case | Structure | Our gross | mevlive gross | Delta (ours - mevlive) | Our gas | Our net | Status |
|---|---|---:|---:|---:|---:|---:|---|
| `0xdd292a…` | 10 swaps; bundled subcycles | **$0.04675455** | $0.011666 | **+$0.03508855** | $0.04142484 | **$0.00532971** | Largest outlier; bundled-cycle attribution needs audit |
| `0xe2a0a5…` | 4 swaps; bundled subcycles | **$0.02750530** | $0.008493 | **+$0.01901230** | $0.02575453 | **$0.00175077** | Second outlier; same attribution-boundary ambiguity |
| **Both outliers** | 2 txs | **$0.07425985** | ≈$0.020159 | **≈+$0.05410085** | **$0.06717937** | **$0.00708048** | The remaining 44 priced matches offset about $0.0010, leaving the +$0.0531 aggregate gap |
| **Our-only** | **61 arbs** | **$1.04092592** | not in feed | n/a | **$0.69543560** | **$0.34549032** | Not independently adjudicated; neither confirmed nor false positives |

Maximum our-only gross is **$0.19524165**. Of the 61 our-only arbs, **56 (91.8%)**
come from four known searchers: `0xfa606c5b…` 20, `0x476ee0…` 19,
`0xe0556223…` 11, `0xafd6d126…` 6; the remaining 5 are outside those groups.
Both outliers are gross-positive but spend most of that profit on gas, so their
large gross deltas are not large net deltas.

### Caveat

This is the frozen window used to derive the fixes; 47/47 is a recall floor, not
independent validation. Re-run on a fresh holdout window before generalizing.

> The baseline and miss audit below are historical (pre-fix). All 42 misses are
> now matched; keep them as the root-cause record.

## Baseline headline scores (pre-fix)

| Metric | Count |
|---|---|
| mevlive arbitrages | **47** |
| our `arb_atomic` in window | **21** |
| our total ops in window | **79** (`utility` 44, `arb_atomic` 21, `jit` 14) |
| hash overlap (matched) | **5 / 47** (~10.6% recall on mevlive set) |
| mevlive missed by us | **42** (all their blocks were classified — classifier gap, not RPC gap) |

### Did we lose? (historical)

On **count / recall against mevlive's arb feed: yes** — we only recovered 5 of their 47 hashes. That was the benchmark failure mode to fix; the final result is 47/47.

Caveats (not excuses):
- Definitions differ: mevlive labels many tiny cycle arbs (~$0.005–$0.05). We also emit large `arb_atomic` rows mevlive did not list in this feed, plus `jit` / `utility`.
- This is **not** an index failure: every missed mevlive tx sits in a `blocks_classified` block.

## Matched (5) — baseline (historical)

| block | mevlive usdProfit | our kind | tx_hash |
|---|---|---|---|
| 99833225 | 0.009121 | arb_atomic | `0xa18053bd76e5798f1fd27d33d7e38ad19f0c48d79fc637f4adeb6be2cd37d0b2` |
| 99833940 | 0.005274 | arb_atomic | `0x961ef10ccdc06b744b39c7098e01a038c248bfe4b30cad01fffa1ad81c31c425` |
| 99834454 | 0.007896 | arb_atomic | `0x9c48177913059bb789151a85912ac826f3e8e259dbf6047ad3ff2f57726f465a` |
| 99834524 | 0.051483 | arb_atomic | `0x478d138667a4d89a581a981fabbac8510d34e77b5dbea0f294d0ca6878d4e4ea` |
| 99835800 | 0.005016 | arb_atomic | `0x436f467fa1339968398d71742d442f1633068713daa80e49aa9af235843591d5` |

## Missed mevlive arbs (42) — baseline (historical; all 42 now matched)

| block | time | usdProfit | sender | contract | tx_hash |
|---|---|---|---|---|---|
| 99833048 | 2026-11-09T08:00:50Z | 0.007919 | `0x925a84ae8b22Eb9A051804fDcF4f2290E03d74Bd` | `0x2D9a3bA2C1b6a7F29C1D533a14F714110D114B07` | `0x8d67e6ce69bdffea9abf87044f5f8c95ee3a88f33877f35e9e48345bfa0fdd1c` |
| 99833054 | 2026-11-09T08:00:58Z | 0.013658 | `0xAc47F32AB465E207f8f8aAa77d4Fc22FFbec8ABB` | `0xAFd6d126bfe4d985a1DC0a221749C76C96e2779e` | `0x1c6d5faa063b3857050d2cdfe6609573ee89f0409c685a650e5fdf894f18d918` |
| 99833989 | 2026-11-09T08:15:11Z | 0.009159 | `0xC8a13E5e7a1C64de9fc0a8e270F4F923f12a24Ec` | `0x2D9a3bA2C1b6a7F29C1D533a14F714110D114B07` | `0x9bff44bdcb297af0f6d35c0c0354bdb5fbea1cb0b3eb3d08486aab4ef114f79c` |
| 99833991 | 2026-11-09T08:15:12Z | 0.009475 | `0x7Ede4a302ec27a0D8Bd3356002Fb77Fb940a1A75` | `0xFA606C5B6a68C897BcbcbDC3D0F14471536F3bA4` | `0x636502744e95b9b943b46da852a5172b2bf36cc3ad4145691ec0f47fae0b9575` |
| 99834108 | 2026-11-09T08:17:07Z | 0.012418 | `0xe0A87B6132d04eE0BBC1adC88E9A3F52CB232d87` | `0xFA606C5B6a68C897BcbcbDC3D0F14471536F3bA4` | `0x1d5c42a1cf32d9bf64559a0db51671af6c1b3206265b979f9b210253b271ec36` |
| 99834157 | 2026-11-09T08:17:44Z | 0.007858 | `0x60E15152E40c2e50cA6a6dB091E3d2B64073d8d8` | `0x2D9a3bA2C1b6a7F29C1D533a14F714110D114B07` | `0xeeba59660c3d3168d3476d5e8547a15c85a749f1aadb56739ab7f85c866bc423` |
| 99834175 | 2026-11-09T08:18:03Z | 0.014350 | `0xF9d8DA146dBD8e003717828d916E4222C95F7e4C` | `0xAFd6d126bfe4d985a1DC0a221749C76C96e2779e` | `0xf8e371ae3e74be09200cbd976c4ab0181edf991533aabe08cd7d034fdc9e1433` |
| 99834189 | 2026-11-09T08:18:13Z | 0.006229 | `0x6474bBcaAC3d5b0CBC68cdb0fe3B6FA9A1784501` | `0x2D9a3bA2C1b6a7F29C1D533a14F714110D114B07` | `0xb21da21e76b3d0ccc613963ea54af38c13e239d428ab939c7f1c74e24d275444` |
| 99834195 | 2026-11-09T08:18:18Z | 0.015330 | `0xD78e78E752406ab10fd2504Da760d1982F123292` | `0x3dBDdADF5d0c933e6827Af9A58836dD3338b7205` | `0x41cdb9c9e5e566d5c4832e2dd5f23f8c4e31fcdcbf890086f763f35a01054cf8` |
| 99834217 | 2026-11-09T08:18:46Z | 0.010612 | `0x14F4B01401C1e7ECfdd61e0a71C519B9bBeBD4Bb` | `0xAFd6d126bfe4d985a1DC0a221749C76C96e2779e` | `0x88163f3a901a424e6296f086e319a13b51cd4691f68e010e92b8f6c2aebe7c9b` |
| 99834303 | 2026-11-09T08:20:14Z | 0.010358 | `0x363691704f68878959bD8Be5BB7bBf504ee4216d` | `0x3dBDdADF5d0c933e6827Af9A58836dD3338b7205` | `0x8d367577840c01a340235a096f81ef6774826398383905f7180c776a62bd32c6` |
| 99834303 | 2026-11-09T08:20:14Z | 0.008702 | `0x447d9C96e65c6A467623cffc1496B11d6e14157E` | `0x2D9a3bA2C1b6a7F29C1D533a14F714110D114B07` | `0x0832d91743f8a76eb0a9efbe3dd4aaf023155022aec31c4e242e624de7d10c26` |
| 99834304 | 2026-11-09T08:20:14Z | 0.007189 | `0x82cCD7b83B579A7a96018B430e6c6CF7eeE44d84` | `0x2D9a3bA2C1b6a7F29C1D533a14F714110D114B07` | `0x9d38017cb48b770f4e610b7ef9211e8d99101590286da0891a3f2f204294fb88` |
| 99834305 | 2026-11-09T08:20:15Z | 0.009942 | `0x7Ede4a302ec27a0D8Bd3356002Fb77Fb940a1A75` | `0xFA606C5B6a68C897BcbcbDC3D0F14471536F3bA4` | `0xef49a3a3a07d58c2b7d2652b5375f3b05402edc2c25e6caf73767c7469c896b9` |
| 99834367 | 2026-11-09T08:21:12Z | 0.011666 | `0xD78e78E752406ab10fd2504Da760d1982F123292` | `0x3dBDdADF5d0c933e6827Af9A58836dD3338b7205` | `0xdd292a3e2f7c8a72a03f8a23943169e2b9cccbd59c74ca84fc96f56ffc0003ea` |
| 99834440 | 2026-11-09T08:22:10Z | 0.005088 | `0x4aca4F9a5832C8Bb1FA640ff7e3A766df6164fBD` | `0x2D9a3bA2C1b6a7F29C1D533a14F714110D114B07` | `0xa61e47b1163e5630addd855ceff6e6ca14c3a3e440db2d538ce8dca55264dad0` |
| 99834506 | 2026-11-09T08:23:16Z | 0.010768 | `0xf01446Ec52e630a26D81D1596F13445B775fbB4A` | `0xAFd6d126bfe4d985a1DC0a221749C76C96e2779e` | `0x89c565bf9b223009ab5963c84973f557101595167fd196c3ba784b6e6b20c8cc` |
| 99834506 | 2026-11-09T08:23:16Z | 0.006381 | `0xc9ffbe5ec78a88119aB2B3e55236eC5c0c363596` | `0x2D9a3bA2C1b6a7F29C1D533a14F714110D114B07` | `0x749d23aee8e4168330f36c5234ca7c824218181aada3f8db389460161260cdb0` |
| 99834514 | 2026-11-09T08:23:21Z | 0.016416 | `0xfc22AB8Ec5CE66743cACC1C9597648BB38B67eD8` | `0xFA606C5B6a68C897BcbcbDC3D0F14471536F3bA4` | `0x62434bf32400f965097e4a15363003696ab55fede62a84efa490f3c73cccea84` |
| 99834514 | 2026-11-09T08:23:21Z | 0.010072 | `0xe75834EeEB8f0e47Fd2f04F374890EC618A8Cc48` | `0xFA606C5B6a68C897BcbcbDC3D0F14471536F3bA4` | `0x3776e8d48bd03804b47b477068582c1a3b4767063d738b7a63b92acbd3b3e3de` |
| 99834516 | 2026-11-09T08:23:23Z | 0.006757 | `0x450F6C730fFB4cfCB126d29EE30B99366D449730` | `0x2D9a3bA2C1b6a7F29C1D533a14F714110D114B07` | `0x05b34236ae3f67225884c10a2165184d265f9aa0c7a3b75ccf54f791e4140055` |
| 99834522 | 2026-11-09T08:23:31Z | 0.006934 | `0x00B9BCC38b160d4d3EDAbd8e32143B76eD6B5011` | `0x0BDF522D0345B0F34253E204BC5C26FAf0984134` | `0xa47306026e21718807f3aaa26abe78debabdf5a5873815c7c8d9da0823ac4b31` |
| 99834522 | 2026-11-09T08:23:31Z | 0.007222 | `0x8D010E4De4AC8148334450B49EB65c713b56d191` | `0x2D9a3bA2C1b6a7F29C1D533a14F714110D114B07` | `0x3791c8ea9317bebd96f94331d1227ffbf9c8ae17a2d6bb6f8f6ac7b33f43c14d` |
| 99834524 | 2026-11-09T08:23:32Z | 0.057473 | `0x6474bBcaAC3d5b0CBC68cdb0fe3B6FA9A1784501` | `0x2D9a3bA2C1b6a7F29C1D533a14F714110D114B07` | `0xe842b691ee44458e63a5a24e85debafc1512ba1c9a2dd96cbd73c69233825563` |
| 99834525 | 2026-11-09T08:23:33Z | 0.013237 | `0x7Ede4a302ec27a0D8Bd3356002Fb77Fb940a1A75` | `0xFA606C5B6a68C897BcbcbDC3D0F14471536F3bA4` | `0xf4d5ec379834438135903556383ad1990654d7cfa0178228181d79425db4028f` |
| 99834525 | 2026-11-09T08:23:33Z | 0.004808 | `0x099B80e6cc77bde62284ce9Eb49811f9FAe9Fb43` | `0x2D9a3bA2C1b6a7F29C1D533a14F714110D114B07` | `0x5173eb945bb9ae4477614b9647bce4626ad1ecdf2ab24af96bcbe4309f6cdbf7` |
| 99834527 | 2026-11-09T08:23:34Z | 0.008493 | `0xD78e78E752406ab10fd2504Da760d1982F123292` | `0x3dBDdADF5d0c933e6827Af9A58836dD3338b7205` | `0xe2a0a5de87c3b15028b59f3a7086b56a7eba0b9b94b0f9890525073f2a2a7f59` |
| 99834527 | 2026-11-09T08:23:34Z | 0.008672 | `0xe0A87B6132d04eE0BBC1adC88E9A3F52CB232d87` | `0xFA606C5B6a68C897BcbcbDC3D0F14471536F3bA4` | `0x21897bf48a1b41d10f87ea0e68ec574dc23177c4c0dcd62fbbc6b65beb2a09bc` |
| 99834528 | 2026-11-09T08:23:35Z | 0.010194 | `0x7Ede4a302ec27a0D8Bd3356002Fb77Fb940a1A75` | `0xFA606C5B6a68C897BcbcbDC3D0F14471536F3bA4` | `0x03a995d539e080f50b8b4696786c1933eaa866d7fdccdce15d5d6590b3d8a2b7` |
| 99834580 | 2026-11-09T08:24:26Z | 0.012277 | `0xe75834EeEB8f0e47Fd2f04F374890EC618A8Cc48` | `0xFA606C5B6a68C897BcbcbDC3D0F14471536F3bA4` | `0x95b03a2e89e017230450591e35a97dc3a6d13cac0416181a092cb944a01b37eb` |
| 99834604 | 2026-11-09T08:24:43Z | 0.007671 | `0xF9d8DA146dBD8e003717828d916E4222C95F7e4C` | `0xAFd6d126bfe4d985a1DC0a221749C76C96e2779e` | `0x8da1a036265a626109a2263e943ffd35bb2ddde00cb98a711b36424506cdc340` |
| 99834609 | 2026-11-09T08:24:47Z | 0.008272 | `0xe0A87B6132d04eE0BBC1adC88E9A3F52CB232d87` | `0xFA606C5B6a68C897BcbcbDC3D0F14471536F3bA4` | `0xd01a4a8d16cfdb604fcc7bdf158998c66d1e0d49a96b5f7593630f8128be5934` |
| 99834612 | 2026-11-09T08:24:52Z | 0.020178 | `0xAeE4b1937b2F7574f9CA43DB3a4D7C6773c4d11e` | `0xd1f6cA075b345c3532EE3957f0a32A0a0C881449` | `0xd0b6a1fd4d987cc8d3ae83e278f8a393579dded0098b686493b1c141277ec644` |
| 99834664 | 2026-11-09T08:25:38Z | 0.009539 | `0x35b97f8EA6297635772B0Da866d43206F93ba960` | `0xAFd6d126bfe4d985a1DC0a221749C76C96e2779e` | `0x59c38a7c898e74d943755a4f7248091ee24eec7045c35ec73eff58a8ed5b955b` |
| 99835474 | 2026-11-09T08:38:57Z | 0.021329 | `0xD78e78E752406ab10fd2504Da760d1982F123292` | `0x3dBDdADF5d0c933e6827Af9A58836dD3338b7205` | `0x29aba6979c579799e8e8d871d77a221b9a9c4f906b567da70b08d560c638462b` |
| 99835965 | 2026-11-09T08:46:44Z | 0.019662 | `0x0CD4d40dd1B19082f58e1Be16183b02eF84aAD32` | `0x2D9a3bA2C1b6a7F29C1D533a14F714110D114B07` | `0xf31019f1b52edef7e133bcb3ab6270514350cf53e06d5ab351ccd4a971e31415` |
| 99836214 | 2026-11-09T08:51:01Z | 0.005451 | `0x1FE86eb2D73f8B02597186dBFbd5B18064b68395` | `0x2D9a3bA2C1b6a7F29C1D533a14F714110D114B07` | `0x8c59f6519ac7f6f739150076552eb8ae55e6dca8c0ed56fbde12086a8fd78f74` |
| 99836261 | 2026-11-09T08:51:45Z | 0.011869 | `0x7Ede4a302ec27a0D8Bd3356002Fb77Fb940a1A75` | `0xFA606C5B6a68C897BcbcbDC3D0F14471536F3bA4` | `0x48ee736d741c8c0fcfc654e7df4a51197de5ec32fec660cbdb87368149dc4281` |
| 99836268 | 2026-11-09T08:51:54Z | 0.010020 | `0x0CD4d40dd1B19082f58e1Be16183b02eF84aAD32` | `0x2D9a3bA2C1b6a7F29C1D533a14F714110D114B07` | `0x0303062037c786ff2bb168c47341196b54b52040f772049d687607ff261cbd11` |
| 99836276 | 2026-11-09T08:51:59Z | 0.007816 | `0x06B5bf3E619C377d5a15ac6b9dd224B80E8Ba93D` | `0x0BDF522D0345B0F34253E204BC5C26FAf0984134` | `0x3b7b1aa00d00ff71e784c04a967fa3dddb2cd06bd773722c79cfdc5b47b8487b` |
| 99836354 | 2026-11-09T08:53:22Z | 0.005839 | `0x60E15152E40c2e50cA6a6dB091E3d2B64073d8d8` | `0x2D9a3bA2C1b6a7F29C1D533a14F714110D114B07` | `0x8f5105061196a0f9e9dccfb975f7656e129604c1237a9be429c0e9cf069260a9` |
| 99836394 | 2026-11-09T08:54:05Z | 0.013544 | `0x7Ede4a302ec27a0D8Bd3356002Fb77Fb940a1A75` | `0xFA606C5B6a68C897BcbcbDC3D0F14471536F3bA4` | `0x675caa7d06850765efcfa02f5f2f1db4ff129b879407371981c2a1570e87e598` |

## Our ops in the same block window (79)

| block | idx | kind | subkind | conf | profit_usd | net_profit_usd | eoa | contract | tx_hash | vs mevlive |
|---|---|---|---|---|---|---|---|---|---|---|
| 99833089 | 1 | utility | bundler | inferred | 0.003741 | 0.003741 | `0x357dd52130f5420445db9c681447c755cef83eb3` | `0x0000000071727de22e5e9d8baf0edac6f37da032` | `0xb205be4b17b5dde464830b19ac6ed54119e80aa783e45e8765e94a29a7b94872` | ours-other |
| 99833089 | 2 | utility | bundler | inferred | 0.002921 | 0.002921 | `0x3cfdc212769c890907bce93d3d8c2c53de6a7a89` | `0x433709009b8330fda32311df1c2afa402ed8d009` | `0xf0f27b69be82ffa198d06ba4b2686a5e162401e7b21d5d87073afdde593ac758` | ours-other |
| 99833106 | 1 | utility | bundler | inferred | 0.009504 | 0.009504 | `0x357dd52130f5420445db9c681447c755cef83eb3` | `0x0000000071727de22e5e9d8baf0edac6f37da032` | `0x2f2838542a00e446d5342ca0c816fbf1ab0d4eb95031ebdb2a8eeeeadcb19bad` | ours-other |
| 99833159 | 6 | arb_atomic |  | exact | 0.008604 | 0.002539 | `0xfa606c5b6a68c897bcbcbdc3d0f14471536f3ba4` | `0xfa606c5b6a68c897bcbcbdc3d0f14471536f3ba4` | `0x3da483e91bcc3530387cdcb96a087a632a8365e86f1d72eb1908d4db9858e47e` | OUR-ONLY |
| 99833185 | 0 | utility | bundler | inferred | 0.000000 | 0.000000 | `0x57c5115fe9f1cffdf0607a9b9beefa56a9125870` | `0x5ff137d4b0fdcd49dca30c7cf57e578a026d2789` | `0x9f4e8e7ed5a06331f6e203b0502bc974e116a6c69cb8924f64cde6612492800b` | ours-other |
| 99833190 | 0 | utility | bundler | inferred | 0.014427 | 0.014427 | `0x433700890211c1c776c391d414cffd38efdd1811` | `0x5ff137d4b0fdcd49dca30c7cf57e578a026d2789` | `0xc55b4e5d3dd986923830833d5ead4cec0a655f589460f482523ca0f25b19289d` | ours-other |
| 99833216 | 1 | utility | bundler | inferred | 0.006414 | 0.006414 | `0xba9cfe6a44979adddbf9f4342c65c4da9c5b207b` | `0x5ff137d4b0fdcd49dca30c7cf57e578a026d2789` | `0x39571d361637a8391dd539f572379b83976811a14c8b4a8fa33b72ebaa2aa334` | ours-other |
| 99833225 | 4 | arb_atomic |  | exact | 0.009212 | 0.003205 | `0xc5ed564ee1ee633656605ca98d5f1b6aad368439` | `0xc5ed564ee1ee633656605ca98d5f1b6aad368439` | `0xa18053bd76e5798f1fd27d33d7e38ad19f0c48d79fc637f4adeb6be2cd37d0b2` | MATCH |
| 99833243 | 0 | utility | bundler | inferred | 0.018136 | 0.018136 | `0x4337009be43c7ecc9bfbde0f1780553aa065187e` | `0x5ff137d4b0fdcd49dca30c7cf57e578a026d2789` | `0x9196125e97a9037e04584a429c449bc30bda1bf7a536b1f491b6c8c1cc470318` | ours-other |
| 99833392 | 0 | utility | bundler | inferred | 0.003739 | 0.003739 | `0xcd2fda710b56b0cdbcb41f2b1d0210ed6f972b75` | `0x0000000071727de22e5e9d8baf0edac6f37da032` | `0xa3b00740205642d0998242fcea88d3ae06079fdb963307040855debb2721f7e2` | ours-other |
| 99833409 | 3 | utility | bundler | inferred | 0.009502 | 0.009502 | `0xcd2fda710b56b0cdbcb41f2b1d0210ed6f972b75` | `0x0000000071727de22e5e9d8baf0edac6f37da032` | `0x1fce12ee04b8444857d5cf5448da8488027665615e3f105c704df281d4c02637` | ours-other |
| 99833431 | 0 | utility | bundler | inferred | 0.007385 | 0.007385 | `0x43370108f30ee5ed54a9565f37af3be8502903f5` | `0x5ff137d4b0fdcd49dca30c7cf57e578a026d2789` | `0x46031dff51ce3f9345c7c3e705106f124e94969c6cecf024c9ec308ccbe02a5c` | ours-other |
| 99833485 | 11 | arb_atomic |  | exact | 841.028697 | 840.893926 | `0x111116058b9828cdcdd810c502b925c14f34ca11` | `0x292fc50e4eb66c3f6514b9e402dbc25961824d62` | `0xee6bd20c07efd9716febc64a4f466a5bd9700f313556e2498e367e2e3b03edda` | OUR-ONLY |
| 99833510 | 1 | arb_atomic |  | exact | 11.106989 | 11.046037 | `0x09ad820aac5779683b481c4674208a4e1b024afa` | `0x20f6ee51340adeed01a59b0e65cb3703f3dc860c` | `0x29b766e06dc85d2b9d5bd05d95c0e952e2fab7b3a0b7ae59fe9687a28e5061cc` | OUR-ONLY |
| 99833611 | 1 | utility | bundler | inferred | 0.000000 | 0.000000 | `0x591dc14b5cd66c104bdeb17d5e2bc844ca3179ca` | `0x5ff137d4b0fdcd49dca30c7cf57e578a026d2789` | `0x4eeaef379f5e40575b259645ddd94507f74b78a4cdd7d7978769fa418041293d` | ours-other |
| 99833706 | 5 | utility | bundler | inferred | 0.000000 | 0.000000 | `0xe7538bfa89e46ce0a00b49d6724821e9806c5bf8` | `0x5ff137d4b0fdcd49dca30c7cf57e578a026d2789` | `0xa6563315c8ad24aa73b492596e3edf8235cf3343ee27b3e3cd8629af40d5669d` | ours-other |
| 99833786 | 2 | utility | bundler | inferred | 0.002307 | 0.002307 | `0x3cfdc212769c890907bce93d3d8c2c53de6a7a89` | `0x0000000071727de22e5e9d8baf0edac6f37da032` | `0xfcfdb9569b33fad39aa8967701beb279c547761cc93aa6af801a05fea2829ea9` | ours-other |
| 99833839 | 0 | utility | bundler | inferred | 0.004188 | 0.004188 | `0x357dd52130f5420445db9c681447c755cef83eb3` | `0x5ff137d4b0fdcd49dca30c7cf57e578a026d2789` | `0x4d75a616c73c344ca45776381768b94af7a1430120e87f999dd1c162b00274d6` | ours-other |
| 99833869 | 0 | utility | bundler | inferred | 0.004078 | 0.004078 | `0x357dd52130f5420445db9c681447c755cef83eb3` | `0x0000000071727de22e5e9d8baf0edac6f37da032` | `0xcea9c7dcba746e01be64f7e11625342ecf5cc053a54d14887052bed2ff99de20` | ours-other |
| 99833902 | 0 | utility | bundler | inferred | 0.004071 | 0.004071 | `0x357dd52130f5420445db9c681447c755cef83eb3` | `0x4337084d9e255ff0702461cf8895ce9e3b5ff108` | `0x35892a3ebdf15089e9daf5022b645b15fa9fcae7fedbb25603758799998ebf0a` | ours-other |
| 99833930 | 23 | jit |  | inferred |  |  | `0x3fed017ec0f5517cdf2e8a9a4156c64d74252146` | `` | `0xbdbbd288d9a665e8bd60d365bc5454b0bd93db1292cbfb3e4979d5940e05798e` | ours-other |
| 99833940 | 12 | arb_atomic |  | inferred | 0.005317 | 0.001511 | `0x476ee0e83303e3d9c02a84aed098dd5f75890fae` | `0x2d9a3ba2c1b6a7f29c1d533a14f714110d114b07` | `0x961ef10ccdc06b744b39c7098e01a038c248bfe4b30cad01fffa1ad81c31c425` | MATCH |
| 99833941 | 34 | jit |  | inferred |  |  | `0xf04c572e6cf90ee47e4cee2379a25d25ec508fb5` | `` | `0xbc5f6d87411679ed5e5ff493d91b1f44011cd03044891ff1d9f82ba9d9b4e38a` | ours-other |
| 99833990 | 13 | utility | bundler | inferred | 0.002805 | 0.002805 | `0x4337001fff419768e088ce247456c1b892888084` | `0x5ff137d4b0fdcd49dca30c7cf57e578a026d2789` | `0x296f7745a70695e8d659236eabc49d1ca9306952304d16cfab86bcb72d7339e9` | ours-other |
| 99833997 | 4 | utility | bundler | inferred | 0.002796 | 0.002796 | `0x4337eb57c1ab67f7ef38f6962547b1952dba00f7` | `0x0000000071727de22e5e9d8baf0edac6f37da032` | `0x6d79cf2cf94d3f6199850bde06277adef42020fa36db005ac811901504f87a90` | ours-other |
| 99833999 | 3 | utility | bundler | inferred | 0.000000 | 0.000000 | `0x6c9d96942e4dc17d558e13ce679d197ea9b579f1` | `0x5ff137d4b0fdcd49dca30c7cf57e578a026d2789` | `0x45c231b462d8bd00f38b66feda6f2559c6ec66a3305243dc8e243b6a42c3903c` | ours-other |
| 99834156 | 16 | jit |  | inferred |  |  | `0xc014d4701b50db2c40ffd3c1c1924ee2bba5f21f` | `` | `0x04a600647d02af5d9ea7a846b83ced08c0b4c64447f05e3483ead0bb1a9d950d` | ours-other |
| 99834162 | 0 | utility | bundler | inferred | 0.000000 | 0.000000 | `0xdb034a1485da0300175b07282f5a84af69c2493e` | `0x5ff137d4b0fdcd49dca30c7cf57e578a026d2789` | `0x08de438531292c72e7656f4eb8f8aaba3241a2c1773490b523788392b89c86f9` | ours-other |
| 99834269 | 0 | arb_atomic |  | exact | 394.535927 | 394.520118 | `0xf835925589ac11df66690b3aa343de649f38cd83` | `` | `0xbadff826380598f176b36d4f9b5a7d118696133c4f9cfe299194aba3951be99c` | OUR-ONLY |
| 99834290 | 1 | arb_atomic |  | exact | 0.223235 | 0.205066 | `0xa35df874df4b412f845c1236c8cca0ffcdb08c8b` | `0x663dc15d3c1ac63ff12e45ab68fea3f0a883c251` | `0x6c98181df0e0a3acaf6b063ece42236634ba7546acc9594f9a525e05fbacbc93` | OUR-ONLY |
| 99834301 | 7 | jit |  | inferred |  |  | `0xc014d4701b50db2c40ffd3c1c1924ee2bba5f21f` | `` | `0x7c85c5b9fae6741c0bdaa74753d17866d881f1c2f71d8b8f57bf2d3113918ddf` | ours-other |
| 99834362 | 6 | arb_atomic |  | inferred | 0.009572 | 0.005027 | `0x476ee0e83303e3d9c02a84aed098dd5f75890fae` | `0x2d9a3ba2c1b6a7f29c1d533a14f714110d114b07` | `0x1dd806a8df253f978868daeebbb8f1756b37911f49185caeeba3038faab5c673` | OUR-ONLY |
| 99834363 | 9 | arb_atomic |  | exact | 0.195242 | 0.074626 | `0xe055622353dbee59943b877b7ffa2dd34410dc48` | `0x3dbddadf5d0c933e6827af9a58836dd3338b7205` | `0x970f4a47386f65c3ed5dda0c804fca4f02b61f0fc521950f13968c83b16d8013` | OUR-ONLY |
| 99834389 | 2 | utility | bundler | inferred | 0.000000 | 0.000000 | `0xac7ca4b036b55399cc8a4826347750bb29b3e9fe` | `0x5ff137d4b0fdcd49dca30c7cf57e578a026d2789` | `0x49692cbebe1bdd31f303de8180a07af0b245412eae5836a11d4b5942d174030b` | ours-other |
| 99834408 | 2 | utility | bundler | inferred | 0.004187 | 0.004187 | `0xca1ef33a9522ef11265858592f0209b45473c846` | `0x5ff137d4b0fdcd49dca30c7cf57e578a026d2789` | `0x3748cabf96d71cc7a0863b20509a124e985e23640435d840c4f957d9ce82e2a8` | ours-other |
| 99834454 | 0 | arb_atomic |  | exact | 0.007877 | -0.000285 | `0xe055622353dbee59943b877b7ffa2dd34410dc48` | `0x3dbddadf5d0c933e6827af9a58836dd3338b7205` | `0x9c48177913059bb789151a85912ac826f3e8e259dbf6047ad3ff2f57726f465a` | MATCH |
| 99834522 | 28 | utility | bundler | inferred | 0.004076 | 0.004076 | `0xca1ef33a9522ef11265858592f0209b45473c846` | `0x0000000071727de22e5e9d8baf0edac6f37da032` | `0x49d4c62d9e26ffd5aede939ef62262aeed1e5cd68fb4112dfbd917922b421f03` | ours-other |
| 99834524 | 10 | arb_atomic |  | inferred | 0.051347 | 0.046840 | `0x476ee0e83303e3d9c02a84aed098dd5f75890fae` | `0x2d9a3ba2c1b6a7f29c1d533a14f714110d114b07` | `0x478d138667a4d89a581a981fabbac8510d34e77b5dbea0f294d0ca6878d4e4ea` | MATCH |
| 99834540 | 27 | arb_atomic |  | exact | 241.957246 | 241.905247 | `0xc014d4701b50db2c40ffd3c1c1924ee2bba5f21f` | `` | `0xd8bf9dae93785c233c284f4a0cc0bd83bdea81aa1fb3efdd2410aa6e1f58d901` | OUR-ONLY |
| 99834542 | 1 | utility | bundler | inferred | 0.002383 | 0.002383 | `0x3cfdc212769c890907bce93d3d8c2c53de6a7a89` | `0x433709009b8330fda32311df1c2afa402ed8d009` | `0x27d220adba9256b7c33ee728b672a7ec216ef969078ab831ba7aab03c5862c0b` | ours-other |
| 99834575 | 0 | utility | bundler | inferred | 0.004069 | 0.004069 | `0xca1ef33a9522ef11265858592f0209b45473c846` | `0x4337084d9e255ff0702461cf8895ce9e3b5ff108` | `0xf42d3ed1425ffcf727f8397c4e94f0adf824e61bb00dc911b7c16eda14bef218` | ours-other |
| 99834603 | 3 | jit |  | inferred |  |  | `0x2f3a099de760277de46420696c89f62a9c512aff` | `` | `0x98ded72ffbd22875b8aaee8e899d978ab4383ab956b51f10520f8df0e103a5b7` | ours-other |
| 99834611 | 4 | arb_atomic |  | exact | 0.933102 | 0.814929 | `0x06380c0e0912312b5150364b9dc4542ba0dbbc85` | `0x20f6ee51340adeed01a59b0e65cb3703f3dc860c` | `0x665ea1538d125b96b80fd695d85d9d71b858be580f8f1b7109b933f68a2bdd6d` | OUR-ONLY |
| 99834647 | 2 | utility | bundler | inferred | 0.000000 | 0.000000 | `0x9c4bd019cf2f4ce5945afa82b807e412b820acfd` | `0x5ff137d4b0fdcd49dca30c7cf57e578a026d2789` | `0x022238cb2d5d34f5be127f51fcb357d297f1facc99aaa9876c6b6a58b62abba2` | ours-other |
| 99834662 | 6 | utility | bundler | inferred | 0.000000 | 0.000000 | `0xb2f52cb5a2094a00101d91eeb65b85cb1d86431e` | `0x5ff137d4b0fdcd49dca30c7cf57e578a026d2789` | `0x6e87634a5252ea596d6d6fb08dca4c9122137e4bcef60580e97762f1c737fdb3` | ours-other |
| 99834727 | 6 | utility | bundler | inferred | 0.000000 | 0.000000 | `0x9d4c1c9e1f850f22e5940b8385aa5a580798e5de` | `0x5ff137d4b0fdcd49dca30c7cf57e578a026d2789` | `0x9a21f7fc4d488358bbcfed7453fb3d7ffd3a72eed9f896d108370cbfdfe8976b` | ours-other |
| 99834762 | 22 | jit |  | inferred |  |  | `0xc014d4701b50db2c40ffd3c1c1924ee2bba5f21f` | `` | `0x91476508e233f6631c55c785636438fb8867c69bf328a49b3c044df5a01dfbf5` | ours-other |
| 99834937 | 0 | utility | bundler | inferred | 0.003958 | 0.003958 | `0x4337002c5702ce424cb62a56ca038e31e1d4a93d` | `0x5ff137d4b0fdcd49dca30c7cf57e578a026d2789` | `0x8e55fa46a9ea695d40cfa10f12a3fe1f0571e077a19c73d30d916fea05121ef1` | ours-other |
| 99834943 | 6 | utility | bundler | inferred | 0.003851 | 0.003851 | `0x43372f7d6208a2b4f5a26863d2a5a84b2b0aa2b2` | `0x0000000071727de22e5e9d8baf0edac6f37da032` | `0x54d1e9fffeb666c194c8141e9f5b168e4b78c5775d67501ede9b28fdd2c02b7d` | ours-other |
| 99834989 | 4 | utility | bundler | inferred | 0.000000 | 0.000000 | `0xb1bcca2ac714a6eed7742866a2ec772394e9ea1a` | `0x5ff137d4b0fdcd49dca30c7cf57e578a026d2789` | `0x3781d2be0add8fda2fa626a0e6b4a873ccf4db76ad65ccd0c11d1955fb50ec69` | ours-other |
| 99835049 | 6 | jit |  | inferred |  |  | `0x744b88a45ba9d1ce01f9a8a0c5d42dff786711b4` | `` | `0x8bbdbabb3d00ee486552fbca7854ae7a7d7197907186398a5e5207f7ded9033d` | ours-other |
| 99835096 | 39 | jit |  | inferred |  |  | `0xf04c572e6cf90ee47e4cee2379a25d25ec508fb5` | `` | `0x717674ab6e5b26a0a9291fcc80cabf75781fd2b968a27b2aea97664a2038f2c9` | ours-other |
| 99835111 | 0 | utility | bundler | inferred | 0.002844 | 0.002844 | `0x3cfdc212769c890907bce93d3d8c2c53de6a7a89` | `0x0000000071727de22e5e9d8baf0edac6f37da032` | `0x3e992f3fe7e17a2354090dda4f86267f8536f8710377c49ee9546a415fa3eff8` | ours-other |
| 99835213 | 0 | arb_atomic |  | exact | 530.600068 | 530.569950 | `0x3007445ee56247487a55cceaedc0ca1661c40021` | `` | `0xb678547c55b69e47930010c3ec6e1d7b2cbebcde22889585bebf59c438a173c8` | OUR-ONLY |
| 99835289 | 14 | utility | bundler | inferred | 0.000000 | 0.000000 | `0x57c5115fe9f1cffdf0607a9b9beefa56a9125870` | `0x5ff137d4b0fdcd49dca30c7cf57e578a026d2789` | `0xd1ceb1c29b5d5950e104c044da652914753de657fe9050f7a912f8f9c67f9a55` | ours-other |
| 99835348 | 1 | jit |  | inferred |  |  | `0x3fed017ec0f5517cdf2e8a9a4156c64d74252146` | `` | `0x6cb05abe3025a7673a5322cb3dd2ff34f45b0ed2b0ea3c857ec0f07620986e79` | ours-other |
| 99835361 | 24 | utility | bundler | inferred | 0.007057 | 0.007057 | `0xba9cfe6a44979adddbf9f4342c65c4da9c5b207b` | `0x5ff137d4b0fdcd49dca30c7cf57e578a026d2789` | `0xbd9c17999f5519c4a1fad46fbb3351360fef51bdec49150c184228796a386e5b` | ours-other |
| 99835376 | 1 | utility | bundler | inferred | 0.006414 | 0.006414 | `0xba9cfe6a44979adddbf9f4342c65c4da9c5b207b` | `0x5ff137d4b0fdcd49dca30c7cf57e578a026d2789` | `0x3e2a9056b97a20e632c13c768df617f3fec8483a3a394b634c7f250c2db46f93` | ours-other |
| 99835391 | 0 | utility | bundler | inferred | 0.007057 | 0.007057 | `0xba9cfe6a44979adddbf9f4342c65c4da9c5b207b` | `0x5ff137d4b0fdcd49dca30c7cf57e578a026d2789` | `0x88ede57770d8092c2a605327d6ca59e6f44a1743ae886d22c277e8899b46d5c7` | ours-other |
| 99835442 | 28 | jit |  | inferred |  |  | `0x3fed017ec0f5517cdf2e8a9a4156c64d74252146` | `` | `0x7552108ffc167c22e27ec0ec0a7f39e7c90790dcf501fa6a6d164eca24c716b8` | ours-other |
| 99835464 | 0 | utility | bundler | inferred | 0.033081 | 0.033081 | `0x433713df632ac453efd07f7ae750a5db9d5aabe6` | `0x0000000071727de22e5e9d8baf0edac6f37da032` | `0x3d29d27654b1b45925e88123f83cbf312432dfc0d6e43379b5b1db9a2bcdf6c2` | ours-other |
| 99835472 | 42 | jit |  | inferred |  |  | `0xf04c572e6cf90ee47e4cee2379a25d25ec508fb5` | `` | `0x042566746301920d4ac8395b0c147311ce871a9b1cb897bba2fe4816ddc7e06e` | ours-other |
| 99835473 | 13 | arb_atomic |  | inferred | 0.009943 | 0.006082 | `0x476ee0e83303e3d9c02a84aed098dd5f75890fae` | `0x2d9a3ba2c1b6a7f29c1d533a14f714110d114b07` | `0x0de135700937db0b404f9bac137431dffc92e32770c839f714bb4b5fa7d8fb94` | OUR-ONLY |
| 99835530 | 2 | utility | bundler | inferred | 0.000000 | 0.000000 | `0x591dc14b5cd66c104bdeb17d5e2bc844ca3179ca` | `0x5ff137d4b0fdcd49dca30c7cf57e578a026d2789` | `0x26390f993ba07736274e6d5ab6c8b43a729ef03f4a13a63e3ae33b1120abd2d6` | ours-other |
| 99835543 | 3 | utility | bundler | inferred | 0.007057 | 0.007057 | `0xba9cfe6a44979adddbf9f4342c65c4da9c5b207b` | `0x5ff137d4b0fdcd49dca30c7cf57e578a026d2789` | `0x620416f5c9daf53158da5c063c59b4ca2723c640b056a8a133b7a23bd2e4b432` | ours-other |
| 99835633 | 1 | utility | bundler | inferred | 0.000000 | 0.000000 | `0xe7538bfa89e46ce0a00b49d6724821e9806c5bf8` | `0x5ff137d4b0fdcd49dca30c7cf57e578a026d2789` | `0x02ba74d88193b64f61528e98bc467b85196efd45e295e273ee7ea727d7f8bafd` | ours-other |
| 99835655 | 0 | arb_atomic |  | exact | 0.011723 | 0.005490 | `0xfa606c5b6a68c897bcbcbdc3d0f14471536f3ba4` | `0xfa606c5b6a68c897bcbcbdc3d0f14471536f3ba4` | `0x3a5ac6b9208a159f6d75860f524acc0e274da24a6402f546e639de0d85c42492` | OUR-ONLY |
| 99835655 | 1 | arb_atomic |  | exact | 0.008265 | 0.002335 | `0xfa606c5b6a68c897bcbcbdc3d0f14471536f3ba4` | `0xfa606c5b6a68c897bcbcbdc3d0f14471536f3ba4` | `0xf00d8258a52b77a69146d547b0f69647c5316f0f1e9e246280843ad556e48be4` | OUR-ONLY |
| 99835753 | 2 | jit |  | inferred |  |  | `0x3fed017ec0f5517cdf2e8a9a4156c64d74252146` | `` | `0x9ac0168f7faaea4344f1348e424d5e0f1532b0d8b4d5fad4c471211e17110d7f` | ours-other |
| 99835800 | 13 | arb_atomic |  | inferred | 0.004995 | 0.001218 | `0x476ee0e83303e3d9c02a84aed098dd5f75890fae` | `0x2d9a3ba2c1b6a7f29c1d533a14f714110d114b07` | `0x436f467fa1339968398d71742d442f1633068713daa80e49aa9af235843591d5` | MATCH |
| 99835832 | 0 | utility | bundler | inferred | 0.002995 | 0.002995 | `0x3cfdc212769c890907bce93d3d8c2c53de6a7a89` | `0x433709009b8330fda32311df1c2afa402ed8d009` | `0x6723bbaf10cbe2bb39c9d9f3e475b20bf08b122c1826822812fcb38c81e1dc9c` | ours-other |
| 99835854 | 3 | arb_atomic |  | exact | 0.014303 | 0.008859 | `0xe055622353dbee59943b877b7ffa2dd34410dc48` | `0x3dbddadf5d0c933e6827af9a58836dd3338b7205` | `0xb2cfe81dce98cb814d8c12286e62b65341a99f099527e1cbd6a5c82e3df7e769` | OUR-ONLY |
| 99835859 | 13 | utility | bundler | inferred | 0.003334 | 0.003334 | `0x4337003fcd2f56de3977ccb806383e9161628d0e` | `0x5ff137d4b0fdcd49dca30c7cf57e578a026d2789` | `0x26f661073c3d5625b0ce31dccbd591093260a0f94c4d5d1593a3a7432d95b2f5` | ours-other |
| 99835865 | 0 | utility | bundler | inferred | 0.003241 | 0.003241 | `0x4337ccb347bcda77d667276415f220adf2718cbc` | `0x0000000071727de22e5e9d8baf0edac6f37da032` | `0x41be5b7bbdb43e59d58742175a024436c5d4634f084c2a39bff21708e42b5ced` | ours-other |
| 99835911 | 1 | utility | bundler | inferred | 0.000000 | 0.000000 | `0x6c9d96942e4dc17d558e13ce679d197ea9b579f1` | `0x5ff137d4b0fdcd49dca30c7cf57e578a026d2789` | `0x7856faa80abadd1619779e2cc3de5778cd244153ec01a6a75e6ab13f2cec5176` | ours-other |
| 99836051 | 16 | arb_atomic |  | exact | 0.012076 | 0.005813 | `0xafd6d126bfe4d985a1dc0a221749c76c96e2779e` | `0xafd6d126bfe4d985a1dc0a221749c76c96e2779e` | `0x0b684053476bdf8b7ae1664eedba6578211dbb2bfbd84b30b32c3b21871ed012` | OUR-ONLY |
| 99836155 | 11 | arb_atomic |  | exact | 169.660863 | 169.580624 | `0xc014d4701b50db2c40ffd3c1c1924ee2bba5f21f` | `` | `0xef0eb4792d86beaf515318ef1441f07f6e2b0b7bac69a8ba81e27995655f3b95` | OUR-ONLY |
| 99836195 | 22 | jit |  | inferred |  |  | `0x2f3a099de760277de46420696c89f62a9c512aff` | `` | `0xb231f118eb581f2de4908add3c55c6e0e3fdf8036183832b6f23419ff88d5cda` | ours-other |
| 99836195 | 23 | jit |  | inferred |  |  | `0x840b11e1f41339eff5b602405d74b60b2605646e` | `` | `0x3b4046d5a5e30954584df6b3bb2d7d2f753938cb64b2bfea7a54d775ed6fd05f` | ours-other |

## Replay commands

```powershell
mev-scout -f mev-scout.toml explorer index --from-block 99833048 --to-block 99836394
mev-scout -f mev-scout.toml explorer --windows all --kind arb_atomic --top 50

# full harness: clears the window, indexes, compares, writes results/compare_<label>.json
python results/_reindex_compare.py run after_attribution_fix
```

Final artifacts: `results/compare_after_attribution_fix.json`,
`results/index_after_attribution_fix.log`.

## Miss audit (historical)

See [`mevlive_avax_missed_arb_audit_2026-11-09.md`](./mevlive_avax_missed_arb_audit_2026-11-09.md).

| bucket | n | call |
|---|---|---|
| V3 signed-delta direction bug (esp. mixed V2/LB+V3) | 23 | **bug** — fixed `apply_signed_deltas` |
| `net_zero_searcher` | 3 | intentional filter / forwarded profit |
| no closed cycle even after transfer-dir repair | 15 | definition gap vs mevlive |
| parallel-pair residual | 1 | likely same bug family |

**All 42 are now matched.** Run1 (18/47) used a binary built before commit
`887067c` added the `token_out` sentinel half of the V3 fix, so it still missed
the mixed routes; the complete decoder plus the classifier/attribution fixes
closed them. The old note that matched 5/5 were pure V2 or pure V3 and missed
`unresolved_cycle` were 36/39 mixed describes the pre-fix baseline only.
