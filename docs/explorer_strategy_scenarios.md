# Explorer strategy scenarios

Per-item test surface for [`explorer_strategy_tracking_plan.md`](./explorer_strategy_tracking_plan.md).

Two layers:

| Layer | Where | RPC? | Purpose |
|-------|-------|:----:|---------|
| **Synthetic** | `core/src/explorer/scenarios.rs` | no | Deterministic fingerprint + `pnl` / `pnl_basis` (CI) |
| **Real (Avalanche)** | `core/tests/explorer_corpus.rs` + hunt table below | yes (`MEV_SCOUT_E2E=1`) | Prove the decoder/classifier on settled 43114 activity |

Machine-readable hunt recipes live in
`core/src/explorer/scenario_targets.rs` (`STRATEGY_SCENARIO_TARGETS`).

---

## How to seed a real case

```text
set MEV_SCOUT_E2E=1
set RPC_URL=<avalanche archive>
set MEV_SCOUT_RECORD=1
set MEV_SCOUT_RECORD_CHAIN=avalanche
set MEV_SCOUT_RECORD_FROM=<lo>
set MEV_SCOUT_RECORD_TO=<hi>
cargo test -p mev-scout-core --test explorer_corpus -- --nocapture
```

Then:

1. Confirm the expected `kind` / `details.tags` appear in the RECORD report.
2. Add a `CorpusCase` (and optionally pin `expected_tag` in `scenario_targets`).
3. Prefer a single dense block or a short window; keep `min_ops` at ~60–70% of observed count.

Topic0 / address filters below are the **search recipe** when no seed is pinned yet
(SnowScan / `eth_getLogs` / Dune raw logs).

---

## Catalogue

Status legend: **syn** = synthetic scenario ships · **real** = corpus seed pinned ·
**hunt** = recipe only (no pinned window yet).

### §1 Baseline

| Item | Tag / kind | Syn test | Real status | Hunt / seed |
|------|------------|----------|-------------|-------------|
| Atomic arb | `arb_atomic` | `baseline_atomic_arb` | **real** | Avalanche `95681722..=95682322` (`av-arb-*` corpus) |
| Sandwich | `sandwich` | `baseline_sandwich` | hunt | Dense mempool blocks; ETH seed `26055651..=26055745` exists (not Avalanche) |
| Frontrun | `frontrun` | `baseline_frontrun` | hunt | Same-pool victim after searcher; RECORD dense arb windows |
| Backrun | `backrun` | `baseline_backrun` | hunt | Same-pool victim before searcher |
| Liquidation | `liquidation` | `baseline_liquidation` | **real** | Avalanche block `95682033` liquidator `0xd2a8…9dd` |
| JIT | `jit` | `baseline_jit` | hunt | V3 Mint+Burn same pool same block; rare — widen RECORD window |
| Skim | `skim` | `baseline_skim` | hunt | V2-like outbound Transfer without Swap/Sync/Mint/Burn |

### Priority 0

| Item | Tag | Syn test | Real status | Hunt / seed |
|------|-----|----------|-------------|-------------|
| P0.1 Flash-loan liq | `flash_loan_liq` | `p0_1_flash_loan_liq` | hunt | Same tx: Aave V3 `FlashLoan` topic **and** `LiquidationCall` on Aave pool `0x69FA…B9b0`. Candidate pattern (verify logs): Silo-style flash+liq e.g. [0xafbcbcd2…](https://snowscan.xyz/tx/0xafbcbcd25e4a5e9ca40d95dd1efe2405398fcae50401084181f64f7ddc1c3586) (block `88784031`) — only counts if our flash decoder covers the provider |
| P0.2 Benqi relabel | `protocol=benqi` | `p0_2_benqi_alias` | hunt | `LiquidateBorrow` topic0 from qiToken in `chains.toml` aliases (e.g. qiAVAX `0x5c04…ef1c`) |
| P0.4 Flash arb | `flash_arb` | `p0_4_flash_arb` | hunt | `arb_atomic` tx with Aave/Balancer/UniV3 `Flash*` + closed cycle. Flash-only examples (need arb legs): [0xbfa3270f…](https://snowscan.xyz/tx/0xbfa3270f0975f87a5769c45d64ed05826f1f090d437bacc43d0e7a9b6f83216c), [0xdd5c0fd2…](https://snowscan.xyz/tx/0xdd5c0fd2e35662b1faf00a0c70433e06fb8119362c543c20d64a7c8505afe552) |

### Priority 1

| Item | Tag / field | Syn test | Real status | Hunt / seed |
|------|-------------|----------|-------------|-------------|
| P1.1 Interest accrual | `interest_accrued` | `p1_1_interest_accrual` | hunt | `LiquidationCall` + prior-window `ReserveDataUpdated` on debt, **no** relevant Chainlink `AnswerUpdated` |
| P1.3 Long-tail | `long_tail` | `p1_3_long_tail` | hunt | Multi-hop `arb_atomic` whose path includes a non-blue-chip token (not USDC/USDT/DAI/WAVAX) — filter RECORD on av-arb window |
| P1.4 Oracle co-block | `oracle_poke_block` | `p1_4_oracle_poke_block` | hunt | Same block: `LiquidationCall` + Chainlink feed from `avalanche.chainlink_feeds` |
| P1.5 Keeper | `keeper` | `p1_5_keeper_execution` | hunt | Gelato `ExecSuccess` or Chainlink `UpkeepPerformed` / `LogTriggered` |

### Priority 2

| Item | Tag / field | Syn test | Real status | Hunt / seed |
|------|-------------|----------|-------------|-------------|
| P2.3 Flash routing | `flash_providers` | `p2_3_flash_routing_stats` | hunt | Flash-liq tx with **≥2** distinct flash providers (rare); else assert single-provider path omits list |

### Priority 3

| Item | Tag | Syn test | Real status | Hunt / seed |
|------|-----|----------|-------------|-------------|
| P3.1 LB bin-JIT | `lb_bin_jit` | `p3_1_lb_bin_jit` | hunt | LFJ/Pharaoh DLMM `DepositedToBins` + `WithdrawnFromBins` around same-pool swap |
| P3.2 Balancer staleness | `rate_provider_staleness` | `p3_2_balancer_staleness` | hunt | Closed arb with Balancer + other AMM, **no** `TokenRateCacheUpdated` in block |
| P3.5 Curve imbalance | `curve_imbalance` | `p3_5_curve_imbalance` | hunt | Closed arb with Curve + non-Curve leg (Avalanche Curve volume thin → `sparse` likely) |
| P3.7 GMX ADL | `gmx_adl_arb` | `p3_7_gmx_adl` | hunt | Co-block GMX EventEmitter `0xDb17…9389` ADL/liq name-hash + `arb_atomic`. Liq-adjacent candidate: [0x254407d4…](https://snowtrace.io/tx/0x254407d42d9b62ecff92302ffcc939080a7c13431ad6a8d29936c53a6b01b46e) (block `81075048`) — confirm EventEmitter topics + same-block arb |
| P3.11 ERC-4337 | `bundler` | `p3_11_erc4337_bundler` | hunt | EntryPoint `0x5FF1…2789` `UserOperationEvent` success |
| P3.12 FoT / rebase | `fot_arb` / `rebase_arb` | `p3_12_fot_rebase_arb` | hunt | Closed arb touching registry FoT/rebase token |
| P3.13 Claim-and-sell | `claim_and_sell` | `p3_13_claim_and_sell` | hunt | Transfer from `0x0` + same-tx swap selling that token |
| P3.14 Bad-debt liq | `bad_debt_liq` | `p3_14_bad_debt_liq` | hunt | Morpho `Liquidate` with `badDebtAssets > 0` — **not on Avalanche** (§6.1); keep synthetic only until Morpho ships on 43114 |
| P3.15 Epoch transition | `epoch_transition` | `p3_15_epoch_transition` | hunt | `NotifyReward` + Pharaoh/Blackhole pool swap, or Thursday 00:00 UTC ±2h window on venue pools |
| P3.16 sAVAX rate arb | `savax_rate_arb` | `p3_16_savax_rate_arb` | hunt | Closed arb sAVAX↔WAVAX with pool vs `getPooledAvaxByShares` divergence. Surface example (not necessarily arb): sAVAX/WAVAX swap legs in [0x9978d2e0…](https://snowtrace.io/tx/0x9978d2e030121082571af89beb6fc8d0b1a81d8450e7d4f40d9d797eb170f7f3) |

---

## Coverage summary

| Bucket | Count |
|--------|------:|
| Synthetic scenarios shipping | 25 (7 baseline + 18 plan items) |
| Real corpus seeds (Avalanche) | 2 kinds (`arb_atomic`, `liquidation`) |
| Hunt-only (need RECORD pin) | remainder of P0–P3 tags |
| Intentionally synthetic-only on 43114 | P3.14 (Morpho) |

Phase-gate rule from the plan still applies: after pinning a real window, if a 30-day backfill yields 0 hits mark the tracker `sparse` in the PR description.
