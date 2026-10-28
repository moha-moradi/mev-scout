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

## External coverage oracle — baseline

Frozen 2026-10-26 before any Phase-A change (Task M0 of the explorer improvement plan). The explorer's arb output is measured against an independent external oracle — the BigLabs arbitrage feed — so every later checkpoint re-indexes the *same* block range and compares against the *same* snapshot. Freeze once: the snapshot is the only durable copy of that hour; later checkpoints must never re-fetch or re-range.

- Vendor window `2026-10-26T04:06:49Z` .. `2026-10-26T05:06:49Z` (3600 s), newest vendor block time `2026-10-26T05:06:49Z`; 46 records / 46 unique hashes, 1 page (`per_page=50`, `sortBy=-created_at`), all `type=arbitrage`, 12 distinct senders, 37 of 46 at `usdProfit < $0.01`.
- Frozen block range `98572463..=98575973` (3511 blocks), derived from the snapshot's min/max `blockNumber`.
- Our side: code commit `351a19c` (pre-plan), DB `cache/oracle-avalanche.sqlite` (git-ignored), archive RPC = the user-provided `mev-scout.avalanche-run.toml` dwellir endpoint (user override of the brief's `hist.toml`; generated config `cache/oracle-avalanche.toml`, both git-ignored). 3511/3511 blocks classified, 117 `mev_ops` rows / 117 unique hashes (67 `arb_atomic`, 30 `unknown`, 20 `jit`), 20 NULL profit (all JIT), 0 `rejected_candidates` (`record_rejections` off by default).
- Snapshot `docs/oracle/biglabs-avalanche-98572463-98575973.json`, SHA256 `e9b06cfc518e4accb43e8098d20ff73e80b5aa40f8a3707040b232a4d19896cb`. Frozen L\V adjudications live in `docs/oracle/adjudications-baseline.json` and are reused by every later checkpoint (per row only `adjudication` and `note` are read; the file's `snapshot_sha256` and `block_range` are validated against the run, so stale labels fail loudly).
- Reproduce (writes the git-ignored machine report `cache/oracle-report-baseline.json`):

```
python scripts/compare_biglabs.py \
  --db cache/oracle-avalanche.sqlite \
  --snapshot docs/oracle/biglabs-avalanche-98572463-98575973.json \
  --chain avalanche --lo 98572463 --hi 98575973 --label baseline \
  --adjudications docs/oracle/adjudications-baseline.json --local-commit 351a19c
```

### Baseline metrics (verbatim script output)

<!-- compare_biglabs.py v1.1.0 label=baseline -->

Snapshot: `docs/oracle/biglabs-avalanche-98572463-98575973.json`  
SHA256: `e9b06cfc518e4accb43e8098d20ff73e80b5aa40f8a3707040b232a4d19896cb`  
Vendor fetch: fetched_at `2026-10-26T05:54:48Z`, newest vendor block time `2026-10-26T05:06:49Z`, pages 1  
Local index: `cache/oracle-avalanche.sqlite` (code commit 351a19c)  
Adjudications: frozen map `docs/oracle/adjudications-baseline.json`  
Block range: 98572463..=98575973 (3511 blocks)
Index facts: blocks classified 3511/3511, rejected_candidates 0

| metric | value |
|--------|-------|
| vendor txs (unique hashes) | 46 |
| local mev_ops rows (unique hashes) | 117 (117) |
| intersection V n L | 46 |
| recall_vs_vendor | 1.0000 (46/46) |
| vendor_agreement (lower bound) | 0.3932 (46/117) |
| L\V | 71 |
| V\L | 0 |
| matched rows: kind we assigned | arb_atomic=46 |
| delta on V n L: median / p90 / worst / best | -0.531% / -0.055% / -99.399% / +0.013% (n=46, skipped vendor null/zero=0, skipped local null=0) |

L\V composition: unknown=30, arb_atomic=21, jit=20

Adjudication counts: `tp_vendor_miss`=20, `fp_suspect`=1, `unknown`=50

| block | tx | kind | conf | local profit | net | adjudication | note |
|-------|----|------|------|--------------|-----|--------------|------|
| 98572511 | `0xfbf67ff0...7cc6e1` | arb_atomic | exact | $0.0129 | $0.005024 | `tp_vendor_miss` | realized triangular 3-hop cross-DEX cycle, profit $0.012948 net $0.005024 (exact); vendor lists txs down to $0.000016, so its default sender/pool watch set, not a size floor, explains the miss |
| 98572517 | `0x86238f16...1e7f47` | unknown | exact | $0.0119 | $-0.001970 | `unknown` | ERC-4337 bundler flow (protocol=erc4337); utility family, invisible to the arb-only vendor by design |
| 98572610 | `0xdbea7759...fd792e` | arb_atomic | inferred | $4.9822 | $4.9656 | `tp_vendor_miss` | realized triangular 3-hop cross-DEX cycle, profit $4.982246 net $4.965578 (inferred, forwarded_profit); vendor lists txs down to $0.000016, so its default sender/pool watch set, not a size floor, explains the miss |
| 98572640 | `0x250340e1...c78a8f` | unknown | exact | $0.003784 | $0.000104 | `unknown` | ERC-4337 bundler flow (protocol=erc4337); utility family, invisible to the arb-only vendor by design |
| 98572713 | `0xb8ea931d...e40744` | unknown | exact | $0.002840 | $-0.001843 | `unknown` | ERC-4337 bundler flow (protocol=erc4337); utility family, invisible to the arb-only vendor by design |
| 98572721 | `0xd9275078...43fd86` | unknown | exact | $0.002761 | $0.000006 | `unknown` | ERC-4337 bundler flow (protocol=erc4337); utility family, invisible to the arb-only vendor by design |
| 98572740 | `0x7b1b0dd5...c02d02` | jit | exact | NULL | NULL | `unknown` | JIT liquidity in pool 0x8ac5707f...; vendor lists only arbitrage, so JIT sits outside its observation set by design (profit NULL in baseline) |
| 98572740 | `0x94eff59d...a50228` | jit | exact | NULL | NULL | `unknown` | JIT liquidity in pool 0x8ac5707f...; vendor lists only arbitrage, so JIT sits outside its observation set by design (profit NULL in baseline) |
| 98572740 | `0x988f0ef2...b14a39` | jit | exact | NULL | NULL | `unknown` | JIT liquidity in pool 0x8ac5707f...; vendor lists only arbitrage, so JIT sits outside its observation set by design (profit NULL in baseline) |
| 98572740 | `0xeddab874...ae2d33` | jit | exact | NULL | NULL | `unknown` | JIT liquidity in pool 0x8ac5707f...; vendor lists only arbitrage, so JIT sits outside its observation set by design (profit NULL in baseline) |
| 98572744 | `0x63dd3445...1dfd5e` | jit | exact | NULL | NULL | `unknown` | JIT liquidity in pool 0x8ac5707f...; vendor lists only arbitrage, so JIT sits outside its observation set by design (profit NULL in baseline) |
| 98572744 | `0x65a8a055...5e1863` | jit | exact | NULL | NULL | `unknown` | JIT liquidity in pool 0x8ac5707f...; vendor lists only arbitrage, so JIT sits outside its observation set by design (profit NULL in baseline) |
| 98572754 | `0x24ad482c...46fba3` | arb_atomic | exact | $0.0151 | $0.005688 | `tp_vendor_miss` | realized triangular 3-hop cross-DEX cycle, profit $0.015071 net $0.005688 (exact); vendor lists txs down to $0.000016, so its default sender/pool watch set, not a size floor, explains the miss |
| 98572768 | `0x5495a368...923178` | arb_atomic | exact | $0.0117 | $0.002389 | `tp_vendor_miss` | realized triangular 3-hop cross-DEX cycle, profit $0.011696 net $0.002389 (exact); vendor lists txs down to $0.000016, so its default sender/pool watch set, not a size floor, explains the miss |
| 98572847 | `0x1296d36a...4a24ed` | arb_atomic | exact | $0.006664 | $0.001385 | `tp_vendor_miss` | realized triangular cycle, profit $0.006664 net $0.001385 (exact); vendor lists comparable dust and has no size floor, so the miss is a vendor coverage gap, not a false positive |
| 98572947 | `0xc3a6575a...d5209e` | arb_atomic | exact | $0.0197 | $0.000302 | `tp_vendor_miss` | realized triangular 6-hop cross-DEX cycle, profit $0.019726 net $0.000302 (exact); vendor lists txs down to $0.000016, so its default sender/pool watch set, not a size floor, explains the miss |
| 98572994 | `0x65423fa0...3b468a` | jit | exact | NULL | NULL | `unknown` | JIT liquidity in pool 0x8ac5707f...; vendor lists only arbitrage, so JIT sits outside its observation set by design (profit NULL in baseline) |
| 98573056 | `0x4cae09e5...cc5845` | unknown | exact | $0.006969 | $-0.003228 | `unknown` | ERC-4337 bundler flow (protocol=erc4337); utility family, invisible to the arb-only vendor by design |
| 98573218 | `0x07f16e14...eee5f1` | unknown | exact | $0.002605 | $-0.000435 | `unknown` | ERC-4337 bundler flow (protocol=erc4337); utility family, invisible to the arb-only vendor by design |
| 98573296 | `0x350938b3...27df61` | unknown | exact | $0.007029 | $-0.000743 | `unknown` | ERC-4337 bundler flow (protocol=erc4337); utility family, invisible to the arb-only vendor by design |
| 98573341 | `0x1c5c59b6...ffd1e9` | arb_atomic | exact | $0.0281 | $0.0143 | `tp_vendor_miss` | realized two_pool 2-hop cross-DEX cycle, profit $0.028061 net $0.014315 (exact); vendor lists txs down to $0.000016, so its default sender/pool watch set, not a size floor, explains the miss |
| 98573342 | `0xdd3a0026...917894` | arb_atomic | exact | $0.0102 | $0.003702 | `tp_vendor_miss` | realized triangular 3-hop cross-DEX cycle, profit $0.010239 net $0.003702 (exact); vendor lists txs down to $0.000016, so its default sender/pool watch set, not a size floor, explains the miss |
| 98573377 | `0x908ae93e...df321c` | unknown | exact | $0.005010 | $0.000282 | `unknown` | ERC-4337 bundler flow (protocol=erc4337); utility family, invisible to the arb-only vendor by design |
| 98573622 | `0x374bbd6d...b5a0de` | arb_atomic | exact | $0.0187 | $0.005900 | `tp_vendor_miss` | realized two_pool 4-hop cross-DEX cycle, profit $0.018651 net $0.005900 (exact); vendor lists txs down to $0.000016, so its default sender/pool watch set, not a size floor, explains the miss |
| 98573708 | `0x09e2adf8...6c1e78` | unknown | exact | $0.002854 | $-0.001830 | `unknown` | ERC-4337 bundler flow (protocol=erc4337); utility family, invisible to the arb-only vendor by design |
| 98573714 | `0x1766761a...2b366c` | unknown | exact | $0.002768 | $-0.000141 | `unknown` | ERC-4337 bundler flow (protocol=erc4337); utility family, invisible to the arb-only vendor by design |
| 98573840 | `0xf4226a77...1366ba` | jit | exact | NULL | NULL | `unknown` | JIT liquidity in pool 0x8ac5707f...; vendor lists only arbitrage, so JIT sits outside its observation set by design (profit NULL in baseline) |
| 98573856 | `0x14533e36...8aac8f` | jit | exact | NULL | NULL | `unknown` | JIT liquidity in pool 0x8ac5707f...; vendor lists only arbitrage, so JIT sits outside its observation set by design (profit NULL in baseline) |
| 98573974 | `0x44a33965...edbfa9` | unknown | exact | $0.002609 | $-0.000436 | `unknown` | ERC-4337 bundler flow (protocol=erc4337); utility family, invisible to the arb-only vendor by design |
| 98574019 | `0x98b277a8...93f98d` | unknown | exact | $0.008259 | $-0.001046 | `unknown` | ERC-4337 bundler flow (protocol=erc4337); utility family, invisible to the arb-only vendor by design |
| 98574212 | `0xfcca76f2...6e98d4` | unknown | exact | $0.004948 | $0.000221 | `unknown` | ERC-4337 bundler flow (protocol=erc4337); utility family, invisible to the arb-only vendor by design |
| 98574250 | `0xedd773e1...da5ddf` | jit | exact | NULL | NULL | `unknown` | JIT liquidity in pool 0x41100c6d...; vendor lists only arbitrage, so JIT sits outside its observation set by design (profit NULL in baseline) |
| 98574267 | `0x3658fc3b...e150f6` | unknown | exact | $0.007199 | $-0.000525 | `unknown` | ERC-4337 bundler flow (protocol=erc4337); utility family, invisible to the arb-only vendor by design |
| 98574510 | `0x33a3e2a6...7b4d41` | unknown | exact | $0.003067 | $-0.004007 | `unknown` | ERC-4337 bundler flow (protocol=erc4337); utility family, invisible to the arb-only vendor by design |
| 98574670 | `0x54ecf25f...326bfd` | unknown | exact | $0.003235 | $-0.001652 | `unknown` | ERC-4337 bundler flow (protocol=erc4337); utility family, invisible to the arb-only vendor by design |
| 98574677 | `0x197a23f5...431ba9` | unknown | exact | $0.002873 | $-0.000255 | `unknown` | ERC-4337 bundler flow (protocol=erc4337); utility family, invisible to the arb-only vendor by design |
| 98574735 | `0xf0b54c5c...fd2176` | arb_atomic | exact | $0.008134 | $0.000625 | `tp_vendor_miss` | realized triangular cycle, profit $0.008134 net $0.000625 (exact); vendor lists comparable dust and has no size floor, so the miss is a vendor coverage gap, not a false positive |
| 98574833 | `0x8d9466c9...fc377c` | arb_atomic | exact | $0.0821 | $0.0383 | `tp_vendor_miss` | realized two_pool 4-hop cross-DEX cycle, profit $0.082116 net $0.038326 (exact); vendor lists txs down to $0.000016, so its default sender/pool watch set, not a size floor, explains the miss |
| 98574850 | `0xf299e7d6...1333a3` | arb_atomic | exact | $0.0110 | $0.004378 | `tp_vendor_miss` | realized triangular 3-hop cross-DEX cycle, profit $0.010969 net $0.004378 (exact); vendor lists txs down to $0.000016, so its default sender/pool watch set, not a size floor, explains the miss |
| 98574910 | `0x3dd02ebb...74924d` | unknown | exact | $0.005010 | $0.000282 | `unknown` | ERC-4337 bundler flow (protocol=erc4337); utility family, invisible to the arb-only vendor by design |
| 98574947 | `0xbb986a40...0fefb9` | unknown | exact | $0.006560 | $-0.003059 | `unknown` | ERC-4337 bundler flow (protocol=erc4337); utility family, invisible to the arb-only vendor by design |
| 98575039 | `0x5d8f68e5...32b624` | jit | exact | NULL | NULL | `unknown` | JIT liquidity in pool 0x8ac5707f...; vendor lists only arbitrage, so JIT sits outside its observation set by design (profit NULL in baseline) |
| 98575039 | `0x98e898cc...ce22d3` | jit | exact | NULL | NULL | `unknown` | JIT liquidity in pool 0x8ac5707f...; vendor lists only arbitrage, so JIT sits outside its observation set by design (profit NULL in baseline) |
| 98575039 | `0xc307afd8...036253` | jit | exact | NULL | NULL | `unknown` | JIT liquidity in pool 0x8ac5707f...; vendor lists only arbitrage, so JIT sits outside its observation set by design (profit NULL in baseline) |
| 98575040 | `0x3c03fb78...900c09` | jit | exact | NULL | NULL | `unknown` | JIT liquidity in pool 0x8ac5707f...; vendor lists only arbitrage, so JIT sits outside its observation set by design (profit NULL in baseline) |
| 98575040 | `0x63d11092...1277a5` | jit | exact | NULL | NULL | `unknown` | JIT liquidity in pool 0x8ac5707f...; vendor lists only arbitrage, so JIT sits outside its observation set by design (profit NULL in baseline) |
| 98575075 | `0x3c5178ac...0569c2` | arb_atomic | exact | $0.008731 | $0.001862 | `tp_vendor_miss` | realized triangular cycle, profit $0.008731 net $0.001862 (exact); vendor lists comparable dust and has no size floor, so the miss is a vendor coverage gap, not a false positive |
| 98575075 | `0x46ee0c4e...8c6091` | jit | exact | NULL | NULL | `unknown` | JIT liquidity in pool 0x8ac5707f...; vendor lists only arbitrage, so JIT sits outside its observation set by design (profit NULL in baseline) |
| 98575085 | `0xedeab064...9887b4` | arb_atomic | exact | $0.004983 | $0.000927 | `tp_vendor_miss` | realized two_pool cycle, profit $0.004983 net $0.000927 (exact); vendor lists comparable dust and has no size floor, so the miss is a vendor coverage gap, not a false positive |
| 98575118 | `0xb8fa4197...077340` | unknown | exact | $0.003290 | $-0.001408 | `unknown` | ERC-4337 bundler flow (protocol=erc4337); utility family, invisible to the arb-only vendor by design |
| 98575245 | `0x25571cb6...d581ee` | unknown | exact | $0.0157 | $-0.005154 | `unknown` | ERC-4337 bundler flow (protocol=erc4337); utility family, invisible to the arb-only vendor by design |
| 98575444 | `0xb9d5c7a8...1bfb8b` | unknown | exact | $0.0145 | $-0.004308 | `unknown` | ERC-4337 bundler flow (protocol=erc4337); utility family, invisible to the arb-only vendor by design |
| 98575481 | `0x47c164ef...5998c3` | unknown | exact | $0.0157 | $-0.004651 | `unknown` | ERC-4337 bundler flow (protocol=erc4337); utility family, invisible to the arb-only vendor by design |
| 98575495 | `0xb18ede98...d22a6b` | jit | exact | NULL | NULL | `unknown` | JIT liquidity in pool 0x8ac5707f...; vendor lists only arbitrage, so JIT sits outside its observation set by design (profit NULL in baseline) |
| 98575502 | `0x026a5e4e...052a29` | jit | exact | NULL | NULL | `unknown` | JIT liquidity in pool 0x8ac5707f...; vendor lists only arbitrage, so JIT sits outside its observation set by design (profit NULL in baseline) |
| 98575502 | `0xebcbf01b...a8e140` | jit | exact | NULL | NULL | `unknown` | JIT liquidity in pool 0x8ac5707f...; vendor lists only arbitrage, so JIT sits outside its observation set by design (profit NULL in baseline) |
| 98575559 | `0x84eaba2b...065bc7` | unknown | exact | $0.002948 | $-0.001845 | `unknown` | ERC-4337 bundler flow (protocol=erc4337); utility family, invisible to the arb-only vendor by design |
| 98575566 | `0x8cd21cce...aa79af` | unknown | exact | $0.003717 | $-0.000602 | `unknown` | ERC-4337 bundler flow (protocol=erc4337); utility family, invisible to the arb-only vendor by design |
| 98575569 | `0xb7a5ea75...e0b40b` | unknown | exact | $0.004627 | $-0.002654 | `unknown` | ERC-4337 bundler flow (protocol=erc4337); utility family, invisible to the arb-only vendor by design |
| 98575571 | `0x1fe46000...fc4520` | unknown | exact | $0.003961 | $0.000158 | `unknown` | ERC-4337 bundler flow (protocol=erc4337); utility family, invisible to the arb-only vendor by design |
| 98575571 | `0xb77466f8...ae72f8` | jit | exact | NULL | NULL | `unknown` | JIT liquidity in pool 0x8ac5707f...; vendor lists only arbitrage, so JIT sits outside its observation set by design (profit NULL in baseline) |
| 98575604 | `0x2c388e3d...2c30cb` | arb_atomic | exact | $0.000001 | $-0.008134 | `fp_suspect` | profit_amount is a single raw USDC unit ($0.000001) and net is negative (-$0.008134): rounding residue of an unrelated multi-hop swap. Observed fp_suspect, not a proven A14 target; A14 may or may not remove it, and fp_suspect is an observation label, never a proven false positive |
| 98575637 | `0x3b1c7aad...080faa` | arb_atomic | inferred | $0.0439 | $0.002937 | `tp_vendor_miss` | realized triangular 3-hop cross-DEX cycle, profit $0.043903 net $0.002937 (inferred, forwarded_profit); vendor lists txs down to $0.000016, so its default sender/pool watch set, not a size floor, explains the miss |
| 98575643 | `0x43f31df0...e26930` | arb_atomic | inferred | $0.0136 | $0.004505 | `tp_vendor_miss` | realized triangular 4-hop cross-DEX cycle, profit $0.013647 net $0.004505 (inferred, forwarded_profit); vendor lists txs down to $0.000016, so its default sender/pool watch set, not a size floor, explains the miss |
| 98575677 | `0xe8e2134c...9bfd36` | unknown | exact | $0.006339 | $-0.001674 | `unknown` | ERC-4337 bundler flow (protocol=erc4337); utility family, invisible to the arb-only vendor by design |
| 98575764 | `0x5e7ae25c...f0188c` | unknown | exact | $0.003284 | $-0.001386 | `unknown` | ERC-4337 bundler flow (protocol=erc4337); utility family, invisible to the arb-only vendor by design |
| 98575799 | `0xf1a09a33...626de8` | arb_atomic | exact | $0.006341 | $0.001238 | `tp_vendor_miss` | realized triangular cycle, profit $0.006341 net $0.001238 (exact); vendor lists comparable dust and has no size floor, so the miss is a vendor coverage gap, not a false positive |
| 98575834 | `0xe57cdf46...ca6164` | arb_atomic | exact | $0.0227 | $0.0136 | `tp_vendor_miss` | realized two_pool 2-hop cross-DEX cycle, profit $0.022677 net $0.013614 (exact); vendor lists txs down to $0.000016, so its default sender/pool watch set, not a size floor, explains the miss |
| 98575951 | `0x7c4e94f6...5ff2a9` | unknown | exact | $0.006026 | $-0.001642 | `unknown` | ERC-4337 bundler flow (protocol=erc4337); utility family, invisible to the arb-only vendor by design |
| 98575963 | `0x65e857c1...7a2ff5` | arb_atomic | exact | $0.0113 | $0.003964 | `tp_vendor_miss` | realized two_pool 2-hop cross-DEX cycle, profit $0.011318 net $0.003964 (exact); vendor lists txs down to $0.000016, so its default sender/pool watch set, not a size floor, explains the miss |
| 98575968 | `0x0aaaa158...9fe4fd` | arb_atomic | exact | $0.0165 | $0.006461 | `tp_vendor_miss` | realized two_pool 2-hop cross-DEX cycle, profit $0.016494 net $0.006461 (exact); vendor lists txs down to $0.000016, so its default sender/pool watch set, not a size floor, explains the miss |

### Station caveats

- **Coverage oracle, not ground truth.** The vendor lists what it saw; it is not a census of Avalanche arbitrage, so `recall_vs_vendor` is a lower bound on true recall and `V \ L = 0` does not prove completeness.
- **`vendor_agreement` is a lower bound**, never "precision": it is `|V n L| / |L|`, and the denominator includes kinds the vendor cannot possibly list.
- **The endpoint is arb-only** (`type=arbitrage`, one class): structurally blind to JIT, sandwich, frontrun/backrun, liquidation, and to our kind-precedence choices, so kind disagreement on a shared hash is a finding, not a miss.
- **The vendor has no size floor** — it lists txs down to `$0.000016` — so its misses are not explained by a profit threshold: the five dust local arbs above are recorded as `tp_vendor_miss`, not as false positives. A14 is not a dollar floor; its checkpoint criterion is that genuinely fake-$0/NULL rows leave L (watch `fp_suspect` rows whose local profit is ~0/NULL).
- **Profit deltas are valuation / route-scope differences, not detection errors.** The worst delta (-99.399%) is block 98572491 `0x8c0f891e...30b5d6`: the vendor values a 7-hop `AVAX/BLACK/XAUT0` route at `$0.947217` while we value the residual in long-tail token `0xcd94...43f6` at `$0.005694`. The row matches; the numbers do not.
- 20 of 117 rows carry a NULL `profit_usd` (all `jit`, by design), so aggregate profit sums over `mev_ops` are not comparable to the vendor's `usdProfit` sums.

### Adjudicating L\V (71 rows)

Counts: `tp_vendor_miss`=20, `fp_suspect`=1, `unknown`=50 (20 JIT + 30 `unknown`-kind ERC-4337 bundler).

A14's checkpoint criterion is that genuinely fake-$0/NULL rows leave L (watch `fp_suspect` rows whose local profit is ~0/NULL); the five dust vendor-misses are expected to remain. `fp_suspect` is an observation label, never a proven false positive.

- `unknown` — JIT rows (20) and the utility family (30 rows of `kind=unknown`, all `protocol=erc4337` bundler flows) are invisible to an arb-only oracle by design. They are not evidence of false positives, and later checkpoints must not "fix" them away by dropping detection.
- `tp_vendor_miss` — 20 `arb_atomic` rows: 15 material cycles plus the five dust cycles (net `$0.0006`–`$0.0019`). All are real cross-DEX cycles (2–6 hops, `pnl_basis=R`); the vendor lists comparable dust and has no size floor, so each miss is a vendor coverage gap, not our error. The largest is block 98572610 `0xdbea7759...fd792e` at `$4.982246` (`inferred`, `forwarded_profit`) — the vendor missed the biggest arb we found.
- `fp_suspect` — 1 `arb_atomic` row at block 98575604 `0x2c388e3d...2c30cb`: `profit_usd` is a single raw USDC unit (`$0.000001`) and net is negative (`-$0.008134`), a rounding residue of an unrelated multi-hop swap. A14 may or may not remove it; it is observed, not a proven A14 target. `fp_suspect` is an observation label, never a proven false positive.

A row that disappears from L\V at a later checkpoint must map to a predicted task effect, or it is a regression flag. The only edit path for the frozen labels is a reviewed edit of `docs/oracle/adjudications-baseline.json`.

### Checkpoint `phase-a` (code `6f4e8d9`, run 2026-10-28)

Re-indexed the same frozen window `98572463..=98575973` from scratch (DB recreated as `cache/oracle-avalanche.sqlite`; the baseline DB was kept as `cache/oracle-avalanche.pre-a.sqlite`): 3511/3511 blocks, 117 ops in 466.7s. All Phase-A tasks landed except A20 (blocked on B7).

```
python scripts/compare_biglabs.py --db cache/oracle-avalanche.sqlite \
  --snapshot docs/oracle/biglabs-avalanche-98572463-98575973.json \
  --chain avalanche --lo 98572463 --hi 98575973 --label phase-a \
  --adjudications docs/oracle/adjudications-baseline.json --local-commit 6f4e8d9
```

| metric | baseline `351a19c` | phase-a `6f4e8d9` |
|---|---|---|
| recall_vs_vendor | 1.0000 (46/46) | **1.0000 (46/46) — floor held** |
| vendor_agreement (lower bound) | 0.3932 (46/117) | 0.3932 (46/117) |
| L\V | 71 | 71 (composition unchanged: unknown=30, arb_atomic=21, jit=20) |
| V\L | 0 | 0 |
| matched kinds | arb_atomic 46 (of the vendor set) | arb_atomic 46 |
| delta on V n L: median / p90 / worst | −0.531% / −0.055% / −99.399% | −0.940% / −0.610% / −99.399% (n=46, skipped local null=0) |
| adjudication counts | tp=20, fp=1, unknown=50 | tp=20, fp=1, unknown=50 |

**L\V diff vs baseline: none appeared, none disappeared** (same 71 hashes) — no `REGRESSION?` flag. The A19 relabeling is visible in the table only as confidence changes (`arb_atomic` rows stay `exact` where basis R; bundler `unknown` rows and JIT rows now read `inferred`); the oracle is blind to kind/confidence by design.

Per-hash movement and attribution (M1 Step 2):

- **Net changed on all 46 matched rows.** Backfill prices gas at the *run-time* native price (the explicitly deferred historical-native-pricing gap), so `net_profit_usd` is not comparable across checkpoints. The script's delta gate is on gross `profit_usd`; that is the comparable figure.
- **Gross `profit_usd` changed on 19 WAVAX-denominated rows by a uniform −1.18%.** The new run's `prices` table has no block-hour row for hour `498053` (the old run's fetch-time hour coincided with it), so the lookup falls back to hour `498052`: 8.799 vs 8.904 USD/AVAX = ×0.98820. This is price-source variance between runs, not a task effect — the hour-convention mismatch A2 documents (token prices keyed by block hour, native by fetch hour) and the deferred historical-native-pricing work are the cause.
- **A14 criterion:** no matched row became NULL (`skipped local null=0`); the single `fp_suspect` residue (one raw USDC unit) remains, exactly as the M0 ruling predicted ("A14 was never going to remove all six").
- **A13/A16/A19** (fee counted once, native residuals priced, basis-labeled confidence) moved net/confidence as predicted; none touches the vendor-matched gross figures beyond the price variance above.

## Kind collision census

Task A15 (2026-10-26, run 2026-10-26). Measured which `MevKind` pairs actually co-occur on the same `(block_number, tx_index)` in every DB on disk, before Task B8 turns `kind_precedence` into the tx_index claims registry. Test: `core/tests/explorer_kind_census.rs` — the two census queries and the post-B8 guard are `#[ignore]`d (gates `MEV_SCOUT_CENSUS=1` / `MEV_SCOUT_E2E=1`) and `bail!` when their gate is set but no DB resolves (A0 convention); an always-on static test prepares + executes the census SQL against the real schema. `mev_ops` has no chain column, so the chain label is the DB file stem; DB resolution is `MEV_SCOUT_CENSUS_DB` (path list) or the configured explorer DB. The plan's `gross_profit_usd` maps to the schema's `profit_usd` column (the pre-gas gross); a NULL gross (unpriced op) contributes 0 and the unpriced count is printed next to the sums.

Reproduce (repo root; both ignored tests run together):

```
MEV_SCOUT_CENSUS=1 MEV_SCOUT_E2E=1 \
MEV_SCOUT_CENSUS_DB='cache/oracle-avalanche.sqlite;cache/avalanche-mev-scout.sqlite' \
  cargo test -p mev-scout-core --test explorer_kind_census -- --ignored --nocapture
```

### Result (real test output, 2026-10-26)

Verbatim census header (guard line follows the same run):

```
== DB D:\gitlab.dte.repo\mev-scout\cache/oracle-avalanche.sqlite (chain label from file stem: oracle-avalanche)
   ops=117 distinct_kinds=3 window=Some(98572463)..=Some(98575973) blocks_with_ops=99 null_tx_index=0 unpriced_profit_usd=20
   kind-pair collisions: NONE — no (block_number, tx_index) carries two kinds (census empty)
guard DB cache/avalanche-mev-scout.sqlite: not an explorer DB (no mev_ops table) — skipped
guard ok: 0 multi-kind rows across 1 explorer DB(s) — the B8 invariant holds on this data
census DB cache/avalanche-mev-scout.sqlite: not an explorer DB (no mev_ops table) — skipped
```

| DB (git-ignored) | chain label | window | mev_ops rows | kinds | NULL tx_index | unpriced `profit_usd` | kind-pair collisions | USD at stake |
|---|---|---|---|---|---|---|---|---|
| `cache/oracle-avalanche.sqlite` (frozen M0 oracle baseline) | `oracle-avalanche` | 98572463..=98575973 (99 blocks with ops) | 117 | `arb_atomic` 67, `unknown` 30, `jit` 20 | 0 | 20 (all `jit`) | **none — census empty** | $0.00 |
| `cache/avalanche-mev-scout.sqlite` | `avalanche-mev-scout` | — | — | — | — | — | not an explorer DB (scanner cache, no `mev_ops` table) — skipped, labeled by filename | — |

**The census is empty — and that is the valuable result.** All 117 `mev_ops` rows in the only explorer DB present sit on unique `(block_number, tx_index)`s (117 unique tx hashes; no tx hosts two rows at all), so no unordered kind pair co-occurs anywhere in the corpus and no precedence choice has measured USD at stake. The co-labeling the A15 brief verified as *unrestrained* in the classifier (the superseder `retain` only drops `ArbAtomic`/`Unknown`, so `Liquidation`/`Jit`/`Skim` co-labeling with a closed-cycle liquidator tx would emit both kinds) simply does not occur in this window. Consequence: **B8 is a no-op on today's corpus**, and the precedence question is answered on principle below. The census is cheap to re-run (env gate + `--ignored`), and the permanent guard `no_tx_index_carries_two_pnl_kinds` passes today; it is EXPECTED to go red on a pre-B8 DB the moment a real co-label lands — that red is B8's fitness function, not breakage (the test's doc comment says so).

### Decided order per contested pair (on-principle — census empty; numbers unchanged until B8)

No `kind_precedence` number changed in A15: an empty census leaves no measured USD to justify a re-baseline outside B8. The decisions B8 must encode (also cited in the `kind_precedence` doc comment):

| pair (loser first) | decision | rationale |
|---|---|---|
| Liquidation vs ArbAtomic | **Liquidation wins** | anchored on an explicit `LiquidateBorrow` event; an inferred cycle must not delete the liquidation label. Today's numbers order the opposite (`ArbAtomic` 3 < `Liquidation` 6) — the flip + golden re-baseline is B8's job. Every `P0.*`/`polygon.*`/`bsc.*` row in this checklist is a Liquidation case. |
| Jit vs ArbAtomic | **Jit wins** | anchored on explicit Mint/Burn facts; genuine combined arb+JIT cases already upgrade to `JitArb`, so a bare Jit-vs-Arb collision would be a mislabel. |
| Skim vs ArbAtomic | **Skim wins** | anchored on explicit pair outflow `Transfer`s (skim is defined by the absence of Swap/Mint/Burn/Sync on the pair). |
| Liquidation vs Jit vs Skim | **Liquidation > Jit > Skim** | explicit anchors, economic purpose of the tx first (liquidation P&L), fee/outflow annotations second. |
| Sandwich/Frontrun/Backrun vs ArbAtomic/Unknown | not contested | already enforced structurally by the superseder `retain` (A1); the precedence list agrees, no decision needed. |
