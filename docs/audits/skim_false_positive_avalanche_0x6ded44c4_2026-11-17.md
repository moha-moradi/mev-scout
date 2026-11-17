# Diagnostic Audit — False-Positive `skim` on Avalanche tx `0x6ded44c4…ad4a9`

- **Date:** 2026-11-17
- **Chain:** Avalanche
- **Block:** 100538126
- **Tx:** `0x6ded44c4edeb5f6f2e9e077001965e8dfb28910052b948c5d376a6b1885ad4a9`
- **Repository HEAD:** `18f49aa` ("fix(explorer): exclude log-emitting pools and unverified factories from skim")
- **Scope:** diagnostic only. No Rust changes, no DB/config/migration/fixture changes, no formatting or dependency changes.

**Evidence base**

| Artifact | Role |
|---|---|
| `cache/explorer-avalanche.sqlite.pre-skimfix.pre.evidence.txt` | Authoritative tx export (the `mev-tx-452-*.json` named in the brief **does not exist** in the repo) |
| `cache/explorer-avalanche.sqlite.pre-skimfix` | Pre-fix state: `mev_ops` row id 452, `kind=skim`, `confidence=exact`, `profit_usd=3104.1924906888244` |
| `cache/explorer-avalanche.sqlite` | Post-fix state: block 100538126 present in `blocks_classified`, **zero** `mev_ops`, **zero** `tx_evidence` |
| `cache/avalanche-mev-scout.sqlite` | Pool registry ground truth: `0x8ac570…` → `dex_type=0`, `factory=NULL`, `dex_name='pharaoh'` |

---

## Part 1 — Executive Summary

The transaction was **not MEV**. It is a routine Pharaoh (TraderJoe-Liquidity-Book-family) DLMM position exit that (a) burned LB bin liquidity, (b) withdrew **431.66675815240695808 WAVAX** from the pool, and (c) claimed **18.463567… PHAR** emissions from the Pharaoh rewarder. Zero of those three actions is a value transfer from a third party to the searcher.

It was reported as a `skim` worth **$3,104.19** because of three independent defects that happened to align:

1. **Registry misclassification.** `infer_dex_type("pharaoh")` had no branch for the bare name `pharaoh`, so it fell through to the `DexType` default `UniswapV2 = 0`. The DLMM pool was therefore loaded into `v2_like_pools` (`jobs/index.rs:is_v2_like`, 21–26) despite `factory = NULL`.
2. **Detector blind spot.** `classify_skims` excluded pools that appeared in `swaps` or `v2_pair_ops`, but **not** pools that emitted logs. A DLMM pool emits `WithdrawnFromBins` and `BinRewardAccumulatorsUpdated`, so the pool stayed a candidate. With one outbound `Transfer` and no inbound `Transfer` to any registered pool, the "one-way outflow" heuristic fired (`PnlBasis::R`, `skim.rs:125–151`).
3. **P&L basis fraud by construction.** `profit_amount` was set to the **gross outbound amount**, i.e. the LP principal plus accrued fees being returned to its owner, with no deposit cost basis and no balance-delta check. Priced at $7.191177991039187/AVAX this is exactly `3104.1924906888244`, confirming the reported figure is `431.66675815240695808 × 7.191177991039187` — arithmetic on a capital return, not on profit.

`confidence` was `exact` only because the op bypassed every downgrade rule in `normalize_confidence` (the op was created with the default `Confidence::Exact` and nothing in the pipeline contested it).

**Fix status:** commit `18f49aa` already on `main` suppresses this exact false positive through two independent gates — `TxInput.log_emitters` seeding `excluded` in `skim.rs:42`, and `is_skim_eligible` requiring `factory.is_some()` (`jobs/index.rs:28–34`). The live DB confirms the op is gone. **However**, the fix is suppression, not comprehension: the pool registry row is still `dex_type=0, factory=NULL` (the code fix at `remote/mod.rs:152–154` is not yet reflected in stored rows), and there is still no way to *name* what this transaction actually did. The pipeline now says "not skim"; it cannot say "liquidity removal + reward claim".

---

## Part 2 — Actual Execution Path

### 2.1 End-to-end chain (historical, pre-fix)

| # | Stage | File / function / lines | What it decided here |
|---|---|---|---|
| 1 | Block + receipt ingest | `core/src/explorer/ingest.rs` (`build_tx_evidence` 924–1049) | 6 receipt logs captured; trace sections forced to `null` with `status: "unavailable"` (1035–1041) |
| 2 | Registry load | `core/src/jobs/index.rs` `load_pool_registry` 39–67 | Row for `0x8ac570…` read with `dex_type=0`, `factory=NULL` |
| 3 | v2-like gate | `core/src/jobs/index.rs` `is_v2_like` 21–26 | `dex_type=0` ⇒ treated as UniswapV2 ⇒ pool enters `v2_like_pools` (no factory check pre-fix) |
| 4 | Registry inference (root input) | `core/src/pool/discovery/remote/mod.rs` `infer_dex_type` 126–190 | `"pharaoh"` unmatched ⇒ falls through to `DexType::UniswapV2` default (`core/src/dex_type.rs`) |
| 5 | Log decode dispatch | `core/src/explorer/classify/mod.rs` `decode_tx_logs_with_aliases` 396–511; `core/src/explorer/decode/mod.rs` `topic_families` 56–167 | `WithdrawnFromBins` → `LogFamily::LbLiquidity`; `ClaimRewards` (`0x9aa05b3d…`) → **no family**; `TransferBatch` (`0x4a39dc06…`) → not ERC-20 `Transfer`, ignored (`decode/transfer.rs:10–25`) |
| 6 | LB liquidity decode | `core/src/explorer/decode/jit.rs` `decode_lb_bins_liquidity` 113–157 | Produces a `JitFact` only; **no operation kind is emitted** |
| 7 | JIT classifier | `core/src/explorer/classify/jit.rs` `classify_jit` 45–130 | Requires a Mint+Burn pair; a standalone Burn is dropped ⇒ no `Jit` op |
| 8 | Block classification | `core/src/explorer/classify/mod.rs` `classify_block` 194–310 | Invokes skim detector with `TxInput` built at 58–92 |
| 9 | **Skim detector** | `core/src/explorer/classify/skim.rs` | `excluded` (pre-fix) = swaps + `v2_pair_ops` only; pool not excluded ⇒ candidate |
| 10 | Skim economics | `skim.rs` 87–105, 125–151 | `profit_tokens` = {(WAVAX, 431.666758…)}; `profit_amount` = gross; `pnl_basis = R`; `MevEvent` built with default `Confidence::Exact` |
| 11 | Claim arbitration | `core/src/explorer/classify/claims.rs` `TxClaims` 1–37; `mod.rs` `kind_precedence` 345–353 | One kind per tx. No other claim existed (reward claim undecodable, LB burn not a kind) ⇒ `skim` wins by walkover |
| 12 | Confidence normalisation | `core/src/explorer/classify/mod.rs` `normalize_confidence` 312–338 | No downgrade rule matched (single claim, resolvable token, available hourly price) ⇒ `exact` persisted |
| 13 | Canonicalisation | `core/src/explorer/canonical.rs` 86–88 | id = `Skim|pool|searcher` |
| 14 | Persist | `core/src/explorer/store/facts.rs`, `core/src/explorer/store/pnl.rs` `annotate_event_with_volume` 70–326 | `profit_usd` = `profit_tokens × price` (102–151); gas/net 213–269 |
| 15 | Types | `core/src/explorer/types.rs` `MevKind` 11–24, `Confidence` 114–134, `PnlBasis` 422–447 | No `liquidity_remove`, no `reward_claim` ⇒ the only representable label was `skim` |

**Identity of the six logs (keccak-verified):**

| Log pos | Event | Emitter | Family / outcome |
|---|---|---|---|
| 0 | `BinRewardAccumulatorsUpdated(uint24,uint256[])` `0x8eb73e2d…` | pool `0x8ac570…` | unmapped |
| 1 | `TransferBatch(address,address,address,uint256[],uint256[])` `0x4a39dc06…` | LB token | LB burn; not ERC-20 `Transfer` ⇒ ignored |
| 2 | `WithdrawnFromBins(address,address,uint256[],bytes32[])` `0xa32e1468…` | pool `0x8ac570…` | `LbLiquidity` → `JitFact` only |
| 3 | `Transfer(pool, executor, 431.66675815240695808e18)` | WAVAX `0xb31f…` | feeds skim detector |
| 4 | `Transfer(rewarder, executor, 18.463567…e18)` | PHAR `0x13a466…` | not from a registered pool ⇒ excluded from `profit_tokens` |
| 5 | `ClaimRewards(address,address,uint256)` `0x9aa05b3d…` | rewarder `0x138ce0…` | unmapped ⇒ invisible |

**Contract identity (Routescan-verified):** `0x138ce0…` is a Pharaoh **DLMM rewarder BeaconProxy** (impl `0x9b9d60557b639Eda795BCc28822fa6214C29CEb2`); its constructor arguments name `0x8ac570…` as the DLMM pool, corroborated by `pool_info.dex_name='pharaoh'`.

### 2.2 Task A — diagnostic answers (A–J)

Answered in the order asked; each header restates the question for traceability.

**A. Which code path produced the op?** `classify_block` → `classify_skims` only. Every other classifier is silent: `classify_jit` needs Mint+Burn (`jit.rs:45–130`); LB liquidity decodes to facts, not kinds (`jit.rs:113–157`); `ClaimRewards` has no `topic_families` entry (`decode/mod.rs:56–167`); `TransferBatch` is not ERC-20 and is dropped (`decode/transfer.rs:10–25`).

**B. Which inputs drove the decision?** Four: (i) registry row `dex_type=0, factory=NULL`; (ii) one `Transfer` with `from` ∈ `v2_like_pools`; (iii) absence of any inbound `Transfer` to a registered pool; (iv) absence of a decoded `Swap`/`Sync`/`Mint`/`Burn` in `tx.swaps` / `tx.v2_pair_ops`.

**C. Why `confidence: exact`?** `MevEvent` was constructed with the default `Confidence::Exact` (`types.rs:114–134`), and `normalize_confidence` (`mod.rs:312–338`) had no reason to downgrade: one claim, a token with a valid hourly price, no ambiguity marker. If any downgrade rule had matched, the row would not read `exact` — so the rule set is falsified by the persisted outcome.

**D. Why `$3,104.19`?** Because `profit_amount` was the **gross** pool outflow and `pnl_basis` was `R` (`skim.rs:125–151`). `annotate_event_with_volume` multiplied by the hourly AVAX quote: `431.66675815240695808 × 7.191177991039187 = 3104.1924906888244` — exact to the last digit of the stored value. Gas `0.002075194485 AVAX × 7.191177991039187 = 0.014923092907657901` USD is subtracted for the net (`≈3104.1775675959167`), but nothing else is.

**E. Why not any other kind?** The `MevKind` enum (`types.rs:11–24`) has no representation for "liquidity removal" or "reward claim", and `TxClaims` (`claims.rs:1–37`) permits one P&L-bearing kind per tx. The transaction's true actions were unrepresentable, so the only candidate claim present won unopposed.

**F. What was actually decoded?** Only `WithdrawnFromBins` reached a typed structure, and as a `JitFact`. The two economically decisive events — the PHAR `ClaimRewards` and the LB `TransferBatch` burn — produced no semantic output at all.

**G. What did the pool registry say?** `dex_type=0` (UniswapV2 by default), `factory=NULL`, `dex_name='pharaoh'`. `infer_dex_type` matched no branch for the bare string `pharaoh`; the `DexType` default is `UniswapV2 = 0`. The registry was therefore self-inconsistent and no invariant caught it.

**H. What role did traces play?** None. `build_tx_evidence` hardcodes `internal_calls`/`internal_transactions` to `null` and `trace.status="unavailable"` with reason "Ingest stores block and receipt data but does not call a trace RPC" (`ingest.rs:1035–1041`). The export confirms `trace.status: "unavailable"`. No inference in this report depends on internal calls — the classification defect is fully explained by receipt logs plus registry state.

**I. Does the committed fix resolve it?** For **this tx**, yes: the pool emits logs, so it is in `log_emitters`; `skim.rs:42` seeds `excluded` with log emitters ⇒ the WAVAX transfer at log 3 is skipped at line 67. Independently, `is_skim_eligible` (`jobs/index.rs:28–34`) requires `factory.is_some()`, and this pool's factory is `NULL`. Two independent gates, either sufficient. Confirmed empirically: the live DB has no `mev_ops` row for the block.

**J. What remains unproven?** The registry row was **not** re-derived — it still carries `dex_type=0, factory=NULL`, so the `infer_dex_type` fix exists in code but not in data. Suppression works; labelling does not. The tx is now invisible rather than correctly attributed.

---

## Part 3 — Root Cause Analysis

### 3.1 Defect table

| ID | Defect | Location | Class | Effect on this tx | Status |
|---|---|---|---|---|---|
| R1 | `infer_dex_type` has no branch for bare `"pharaoh"`; falls through to `DexType::UniswapV2 = 0` | `pool/discovery/remote/mod.rs:126–190`, default in `core/src/dex_type.rs` | **Confirmed bug** | DLMM pool admitted to `v2_like_pools` | Fixed in code (`152–154`), **not** in stored data |
| R2 | `is_v2_like` trusts `dex_type` without requiring a verified factory | `jobs/index.rs:21–26` | **Confirmed bug** | Unverified pool eligible for skim | Fixed by new `is_skim_eligible` (`28–34`) |
| R3 | Skim exclusion set omits pools that emit logs | `skim.rs` (pre-fix `excluded` init), now `:42` | **Confirmed bug** | Pool not excluded despite emitting `WithdrawnFromBins` | Fixed |
| R4 | Skim P&L = gross outbound amount, no cost basis, no balance-delta check | `skim.rs:87–105, 125–151` | **Confirmed bug (design-level)** | Principal return booked as $3,104 profit | **Not fixed** — still `PnlBasis::R` |
| R5 | `ClaimRewards` / LB `TransferBatch` undecodable | `decode/mod.rs:56–167`, `decode/transfer.rs:10–25` | **Confirmed gap** | Reward claim invisible; burn invisible | Not fixed |
| R6 | No `liquidity_remove` / `reward_claim` kind; one claim per tx | `types.rs:11–24`, `claims.rs:1–37`, `kind_precedence` `mod.rs:345–353` | **Confirmed gap** | Only expressible label was `skim` | Not fixed |
| R7 | `Confidence::Exact` is the default and is never contested for skim | `types.rs:114–134`, `normalize_confidence` `mod.rs:312–338` | **Confirmed design weakness** | False positive carried maximum trust | Not fixed |
| R8 | Registry rows are not re-derived when inference code changes | `load_pool_registry` `jobs/index.rs:39–67` | **Hypothesis → confirmed as stale data** | Fix present in code, absent in DB | Open |
| R9 | `blocks_classified` retains a row with `event_count=1` after all ops and evidence were removed | live DB (`cache/explorer-avalanche.sqlite`) | **Unclassified severity** — possibly intended reclassification bookkeeping | A block appears processed while yielding zero auditable artifacts | Needs owner decision |

### 3.2 Why the false positive was *inevitable* rather than unlucky

Each gate that could have stopped it was individually satisfiable:

- **Is it a real pool?** Yes — it exists and is registered; no invariant required a factory.
- **Is it a swap?** No decoded `Swap` event → "not a swap".
- **Did the pool receive anything?** No inbound transfer → "not a swap-like pattern".
- **Is the outflow profit?** Nothing compared outflow to reserve delta or to a prior deposit.
- **Is there a competing claim?** Nothing else was representable.

So the detector's exclusion logic was a union of *known-benign patterns* (decoded swaps, JIT pairs) rather than an **allow-list of positive evidence for skim**. Absence of evidence was treated as evidence.

### 3.3 Task C — classification / conflict resolution (6 questions)

1. **Was `skim` ever a defensible label?** No. A skim requires third-party value captured from a pool's reserves. Here the receiver is the position owner reclaiming its own principal plus fees, and the pool's reserve decreased by construction of the burn. UNVERIFIED only in the trivial sense that reserve state is unavailable offline; the burn event alone falsifies skim.
2. **Could it have been `jit` instead?** No. `classify_jit` requires Mint+Burn in the same tx; only a Burn is present, and standalone burns are deliberately dropped (`jit.rs:45–130`).
3. **Could it have been a reward claim?** Semantically yes — log 5 is a genuine `ClaimRewards` — but the kind does not exist and the topic is unmapped, so the pipeline could not produce it.
4. **Which claim should win if several apply?** None should be discarded. `kind_precedence` (`mod.rs:345–353`) resolving to a single kind is the wrong model for a tx that withdraws liquidity, claims rewards, and may also swap. Precedence should order *display*, never *persistence*, and it must never let a weaker-evidence claim (skim) outrank a stronger one (explicit protocol event).
5. **Does the committed fix change the classification or only the suppression?** Only suppression. Post-fix the tx yields **zero** ops. That is a correct *negative* but a false *silence*: no `liquidity_remove`, no `reward_claim`, no economic record.
6. **Was `confidence: exact` correct at any layer?** No layer justified it. The detector had no balance evidence, no cost basis, and a pool whose family was known-wrong. `exact` should have been impossible for any skim derived purely from transfer heuristics.

---

## Part 4 — Recommended Architecture

Principle: **classify from positive, decoded protocol evidence; treat heuristics as downgraded candidates that must pass an economic gate before they may carry USD.**

```
                    ┌──────────────────────────────┐
   block+receipts → │ 1. Evidence capture           │  logs, transfers, decoded
                    │    (trace optional, tagged)   │  events, pool snapshots
                    └───────────────┬───────────────┘
                                    ↓
                    ┌──────────────────────────────┐
                    │ 2. Event taxonomy (typed)     │  per-protocol decoders:
                    │    no "unmapped = ignore"     │  V2, V3, LB, DLMM, rewarders
                    └───────────────┬───────────────┘  unknown topic ⇒ UnknownEvent
                                    ↓                    (recorded, never dropped)
                    ┌──────────────────────────────┐
                    │ 3. Pool identity resolution   │  factory-verified family,
                    │    (fail-closed)              │  explicit "unverified" state,
                    └───────────────┬───────────────┘  never a silent default
                                    ↓
                    ┌──────────────────────────────┐
                    │ 4. Action extraction         │  liquidity_add / liquidity_remove
                    │    (many per tx)             │  swap / reward_claim / transfer
                    └───────────────┬───────────────┘
                                    ↓
                    ┌──────────────────────────────┐
                    │ 5. Claim arbitration         │  all claims kept;
                    │    (multi-claim, ordered)     │  display precedence only
                    └───────────────┬───────────────┘
                                    ↓
                    ┌──────────────────────────────┐
                    │ 6. Economic gate             │  skim requires:
                    │    (fail-closed to UNKNOWN)   │   balance delta > outflow
                    └───────────────┬───────────────┘   AND no burn/mint/withdraw
                                    ↓                    AND verified pool
                    ┌──────────────────────────────┐
                    │ 7. P&L attribution           │  cost basis, fees, rewards,
                    │    (basis-tagged)             │  gas; explicit basis per number
                    └───────────────┬───────────────┘
                                    ↓
                    ┌──────────────────────────────┐
                    │ 8. Confidence + persistence    │  exact only with positive
                    │    + evidence linkage         │  evidence; every op links to
                    └──────────────────────────────┘  the decoded events that justify it
```

Four hard rules the current design lacks:

1. **Fail-closed identity.** A pool with no verified factory is `Unverified`, never a default family. `dex_type` must not have a meaningful default.
2. **Allow-list, not deny-list.** Skim requires positive proof (balance delta exceeds outflow, no burn/withdraw/mint by the same actor). Exclusions are a secondary filter, never the primary gate.
3. **Many actions, one transaction.** Arbitration orders presentation; it must not delete claims.
4. **Basis travels with the number.** Every USD figure carries `pnl_basis` and the evidence that justifies it; `exact` requires evidence, not merely the absence of a contradiction.

---

## Part 5 — Proposed Data Model Changes (conceptual only)

No schema is written here; this is the shape of the change, not a migration.

| Concept | Current | Proposed |
|---|---|---|
| `MevKind` | 8 variants, no liquidity/reward semantics (`types.rs:11–24`) | Add `LiquidityAdd`, `LiquidityRemove`, `RewardClaim`, `FeeAccrual`; retain `Skim` but require the economic gate |
| Claim cardinality | One P&L-bearing kind per tx (`claims.rs:1–37`) | N claims per tx; each row carries `role` (`primary`/`secondary`) and `precedence_reason` |
| Pool identity | `dex_type` with `UniswapV2 = 0` default; `factory` nullable | `dex_family` nullable with **no default**, `factory` required for any `swap`/`skim` eligibility, `identity_status ∈ {verified, unverified, conflict}` |
| Pool derivation | Stored rows never re-derived (`load_pool_registry`) | Rows carry `derivation_version`; a code-version bump marks rows stale and re-derives them |
| Event taxonomy | `topic_families` returns `Option`; unmapped = silently ignored (`decode/mod.rs:56–167`) | Every log → `DecodedEvent` or `UnknownEvent(emitter, topic0)`; nothing is silent |
| LB / DLMM | `WithdrawnFromBins` → `JitFact` only (`decode/jit.rs:113–157`); `ClaimRewards` unmapped | First-class `LiquidityRemove` (bins, amounts) and `RewardClaim` (rewarder, token, amount) |
| Skim evidence | Gross outbound only (`skim.rs:87–105`) | Requires `pool_balance_before/after` (or reserve slot read), `outflow`, `inflow`, `delta`, and `counter_events` (burn/withdraw/mint) |
| P&L basis | `R` asserted, never substantiated (`types.rs:422–447`) | Basis enum extended with `ResidualVerified`; a basis requires its evidence fields to be present, else `UNKNOWN` |
| Confidence | Default `Exact` (`types.rs:114–134`) | `Exact` requires a positive-evidence predicate; defaults to `Heuristic` for any transfer-only inference |
| Evidence linkage | `tx_evidence` blob, no op↔event join (`ingest.rs:924–1049`) | Op references the specific decoded event ids that justify it; deleting evidence invalidates the op explicitly |
| Block bookkeeping | `blocks_classified` can claim a block with 0 ops and 0 evidence | Distinguish `classified_with_ops`, `classified_clean`, `reclassified` |

---

## Part 6 — Regression Test Plan

| # | Scenario | Fixture | Assertions |
|---|---|---|---|
| 1 | **This tx, replayed verbatim** — Pharaoh DLMM burn + withdraw + PHAR claim | `0x6ded44c4…ad4a9` (block 100538126) | No `Skim`; either `LiquidityRemove` + `RewardClaim`, or an explicit `unclassified` record with reason. Never `confidence=exact` |
| 2 | **Positive control: canonical V2 skim** — verified factory, one-way ERC-20 outflow, no burn/withdraw/mint, reserve delta > outflow | synthetic UniswapV2 fixture | `Skim`, `exact`, USD matches outflow × price |
| 3 | **Swap with undecoded `Swap` event** — pool receives and sends | V2 fixture with stripped `Swap`/`Sync` | Not `skim`; `SkimLooksLikeSwap` or `Swap` |
| 4 | **Unverified factory / `factory IS NULL`** | registry row mutated to `NULL` | Pool never enters `v2_like_pools`; zero skim candidates; `identity_status=unverified` |
| 5 | **LB JIT: Mint + Burn in one tx** | LB fixture | `Jit`, not `Skim`, not `LiquidityRemove` |
| 6 | **Standalone LB burn with withdraw, no claim** | this tx minus log 5 and log 4 | `LiquidityRemove`; no `skim`; USD attributed to principal, not profit |
| 7 | **Reward claim only** (`ClaimRewards`, no liquidity movement) | rewarder fixture | `RewardClaim`; profit = PHAR amount × price − gas; not `skim` |
| 8 | **Multi-claim conflict: withdraw + claim + swap in one tx** | composite fixture | All three claims persisted; `kind_precedence` affects display only; conflict recorded; no claim silently dropped |
| 9 | **Economic gate + pricing integrity** — skim candidate whose outflow equals the burned/withdrawn amount; and a profit token with no price | two fixtures | Skim rejected on reserve/burn evidence; missing price ⇒ `UNKNOWN`, no fabricated USD; gas always netted |
| guard | **Evidence deletion hygiene** | run 1, then delete `tx_evidence` | Op is invalidated, not silently orphaned; `blocks_classified` state is not `classified_with_ops` |

---

## Part 7 — Economic Attribution Findings

### 7.1 What the $3,104.19 actually is

| Component | Amount | Priced? | Is it profit? |
|---|---|---|---|
| WAVAX withdrawn from DLMM bins (log 2/3) | 431.66675815240695808 | Yes, $7.191177991039187 | **No** — LP principal + accrued swap fees returned to the depositor |
| PHAR claimed from rewarder (log 4/5) | 18.463567… | No — omitted from `profit_tokens` | Arguably yes (emissions), but never valued and never booked |
| Gas | 0.002075194485 AVAX → $0.014923092907657901 | Yes | Cost (correctly netted) |
| Deposit cost basis | UNKNOWN / not in pipeline | — | Required to compute profit at all |
| Impermanent loss vs. hold | UNKNOWN / not in pipeline | — | Required to compute true LP P&L |

**Verified arithmetic**

```
reported   = 431.66675815240695808 WAVAX × 7.191177991039187 USD = 3104.1924906888244   (exact match)
gas_usd    =   0.002075194485   AVAX × 7.191177991039187 USD =    0.014923092907657901  (exact match)
net_usd    = 3104.1924906888244 − 0.014923092907657901       = 3104.1775675959167
```

The reported figure is a **gross capital return**, i.e. the number a withdrawer sees on their own position. Booking it as MEV profit overstates economic gain by the full principal plus the entire position lifetime's cost basis.

### 7.2 Attribution defects

1. **Unmeasured inflow, unmeasured outflow.** Skim reads only *pool-originated* transfers. The searcher's own deposit (historical blocks) and any same-tx inbound leg never enter the calculation. `PnlBasis::R` asserts a residual that is never computed.
2. **No balance-delta reconciliation.** A genuine skim moves the pool's actual token balance below its recorded reserve. Nothing in the pipeline reads reserve state, so "outflow" and "profit" are the same variable.
3. **Reward leg invisible.** Because log 4's `from` is the rewarder and not a registered pool, PHAR never entered `profit_tokens`. The one genuinely income-like component was the only one excluded.
4. **Price provenance is incidental.** The only reason the figure has cents is that an hourly llama quote existed for WAVAX. Had the profit token lacked a quote, the same false positive would have carried a different (or absent) USD value with no change in classification.
5. **Counter-evidence discarded.** The tx contains three independent signals that this is a position exit — `WithdrawnFromBins`, an LB `TransferBatch` burn, and a `ClaimRewards`. All three were decoded-or-visible and all three were non-blocking. The economic gate must treat any of them as disqualifying for `skim`.

---

## Part 8 — Implementation Roadmap

**Phase 0 — Guardrails (no behaviour change beyond invitations to fail loudly)**
- Add `derivation_version` to registry rows; log a startup warning when stored inference disagrees with current code (this tx's row is the canonical example: `dex_type=0` vs. the now-correct LB mapping).
- Add `UnknownEvent` capture for unmapped `topic0` instead of silent drops.
- *Acceptance:* running against current caches reports the `0x8ac570…` inconsistency without altering data.

**Phase 1 — Identity correctness (fail-closed)**
- Remove the meaningful `UniswapV2 = 0` default; introduce explicit `Unverified`.
- Require a verified factory for any swap/skim eligibility (extend `is_skim_eligible` into a general `is_tradeable_pool`).
- Re-derive stale registry rows; do not rely on `load_pool_registry` reading history.
- *Acceptance:* no pool can reach the skim detector without `identity_status=verified`.

**Phase 2 — Event taxonomy and action extraction**
- Map `ClaimRewards`, LB `TransferBatch`, DLMM bin events to typed events across protocols.
- Emit `LiquidityRemove` / `LiquidityAdd` / `RewardClaim` as first-class actions (Phase 2a: LB+DLMM only, to contain scope).
- *Acceptance:* this tx produces two actions (withdraw, claim) and zero unknowns.

**Phase 3 — Skim economic gate**
- Replace the deny-list with a positive-evidence predicate: verified pool, no burn/withdraw/mint by the same actor, and balance/reserve delta exceeding outflow.
- Default any transfer-only inference to `Heuristic`; forbid `Exact`.
- *Acceptance:* scenarios 1, 3, 4, 9 in Part 6 pass; scenario 2 still yields `exact`.

**Phase 4 — Multi-claim model + P&L attribution**
- N claims per tx; precedence becomes display-only with a recorded reason.
- Carry cost basis and fees where derivable; `ResidualVerified` basis requires its evidence fields.
- Backfill nothing; forward-only.
- *Acceptance:* scenario 8 passes; no claim deletion in any historical replay.

**Phase 5 — Validation and operations**
- Replay a fixed window of Avalanche blocks pre/post change; diff op counts by kind.
- Resolve the `blocks_classified`-with-zero-artifacts question (whether the current behavior is bookkeeping or a bug) before shipping Phase 4's reclassification semantics.
- *Acceptance:* zero net-new `skim` ops on known-benign protocols; every removed op has a documented reason code.

**Sequencing constraint:** Phases 1–2 must land before Phase 3. A positive-evidence skim gate built on a taxonomy that cannot name `ClaimRewards` will simply relocate the blind spot.

---

## Part 9 — Unresolved Questions

1. **Task A wording.** The A–J items were reconstructed as the ten diagnostic questions above (path, inputs, confidence, USD, kind choice, decoding, registry, traces, fix efficacy, residual unknowns). If the intended list differs, the answers can be re-mapped without new investigation — every claim in Part 2 is independently cited.
2. **USDC.e leg.** The receipt contains exactly **one** pool→executor ERC-20 transfer (WAVAX). Pool token1 is USDC.e (`0xb97ef9…`). Whether the position was single-sided, or a USDC.e leg exists outside the captured logs, is **UNKNOWN**.
3. **Pool `0x8ac570…` source.** The contract is unverified on Routescan. Its identity rests on the rewarder's constructor arguments naming it as the DLMM pool plus `pool_info.dex_name='pharaoh'`. Treated as strong but indirect.
4. **Stale registry rows.** The `infer_dex_type` fix is in code, but `cache/avalanche-mev-scout.sqlite` still stores `dex_type=0, factory=NULL`. Whether registry re-derivation is expected on upgrade or requires an explicit re-sync is **undecided**.
5. **Blast radius of the default.** How many other registry rows were inferred from unmatched names and silently became `UniswapV2` is **not quantified** in this audit.
6. **`blocks_classified` orphan.** Block 100538126 remains marked classified with `event_count=1` while having zero `mev_ops` and zero `tx_evidence`. Whether that is intentional post-reclassification bookkeeping or a separate defect is **UNVERIFIED** and needs an owner decision.
7. **Historical position economics.** Deposit block, cost basis, bin step, fee tier, and time-in-position are **unavailable** offline. The true realized P&L (fees + PHAR − gas − IL) cannot be computed from this evidence.
8. **Fix completeness.** `18f49aa` demonstrably suppresses this tx, but whether it correctly handles *other* LB-family transactions (e.g. genuine LB JIT, LB swaps, reward claims on verified LB pools) is **not covered** by this audit's evidence. Phase 2 of the roadmap exists specifically to close that gap.

**Status:** diagnostic complete. No repository or database state was altered beyond adding this document. Awaiting authorization before any implementation work.

---

## Appendix A — Implementation log

### Phase 0 — Guardrails (implemented 2026-11-17)

Scope: make stale registry metadata and undecoded receipt logs visible. No
classification behaviour changes, no schema migration, no data rewrite.

**1. Registry derivation guardrail** (`core/src/jobs/index.rs`, `core/src/jobs/mod.rs`)

- `POOL_DERIVATION_VERSION` (currently `1`) marks the current inference
  semantics.
- `RegistryHealth { stale_derivations, unverified_v2_like }` is populated by
  `load_pool_registry` and exposed on `PoolRegistry::health`.
- `rederived_dex_type(pool)` re-runs `infer_dex_type` against the row's own
  `dex_name`. When the stored `dex_type` disagrees *and* the disagreement
  changes skim eligibility (`is_v2_like` before vs. after), one aggregated
  `warn!` is emitted with up to three example rows. Family-only drift is
  counted but not warned.
- The historical row (`0x8ac570…`, `dex_name='pharaoh'`, `dex_type=0`,
  `factory=NULL`) is exactly the shape this warns about; stored rows are still
  never rewritten, so Phase 1 must decide the re-derivation path.

**2. Unmapped log capture** (`core/src/explorer/types.rs`,
`core/src/explorer/decode/mod.rs`, `core/src/explorer/classify/mod.rs`,
`core/src/explorer/ingest.rs`)

- New `UnmappedLog { tx_index, log_index, emitter, topic0: Option<B256> }`.
- `decode_tx_logs_with_aliases` records every log no decoder claims — including
  anonymous logs (no `topic0`) — instead of dropping it at `None => {}`. Logs
  claimed by the GMX name-hash path are not counted twice.
- `decode::is_mapped_log` mirrors the dispatch for callers that only have raw
  logs.
- `tx_evidence` gains an `unmapped_logs` array (`log_index`, `address`,
  `topic0`, `topic_count`, `data_bytes`) for every transaction that produces an
  op, so "nothing happened" is distinguishable from "we could not see it".
- Ingest emits one `debug!` summary per block (count, distinct topics, top-3
  sample) — deliberately not `warn!`, since `Approval`-class noise is constant.

**Tests added**

- `classify::tests::unmapped_logs_are_recorded_not_dropped` — Transfer still
  decodes; unknown topic and anonymous log are both recorded.
- `jobs::index::tests::stale_derivations_are_detected_not_trusted` — the
  Pharaoh-shaped row re-derives to `TraderJoeLB`, a correct row does not, and a
  missing `dex_name` yields `None`.
- `ingest` evidence test extended with `unmapped_logs` assertions.

**Verification**

- `cargo check -p mev-scout-core --all-targets` — clean.
- `cargo clippy -p mev-scout-core --all-targets` — clean (workspace denies
  warnings; `unwrap_used`/`expect_used` denied).
- `cargo test -p mev-scout-core` — 554 lib tests + all integration binaries
  pass, 0 failures.

**Deliberately deferred**

- Rewriting stale registry rows (needs `derivation_version` persistence →
  Phase 1/5).
- Persisting unknown events outside `tx_evidence` (needs the typed event table
  → Phase 2).
- Any change to `skim`, `MevKind`, `Confidence`, or P&L maths.

### Phase 1 — Identity fail-closed + registry self-heal (implemented 2026-11-17)

Scope chosen by the user: full Phase 1 with schema migration. No
classification rule, `MevKind`, confidence, or P&L change.

**1. Fail-closed family identity** (`core/src/dex_type.rs`)

- Added `DexType::Unknown = 13` (`serde: "unknown"`, strum `"Unknown"`) as the
  `#[default]`; `UniswapV2` is no longer the default. `PoolInfo::default()` now
  builds `Unknown`.
- `Unknown` is never V2-like (hence never skim-eligible), gets no fee
  interpretation, no liveness probe, no metadata fetch, and is skipped with a
  warning by `pipeline::runner::add_pool_to_manager` instead of being guessed
  as V2.
- Added `POOL_DERIVATION_VERSION: u32 = 1`, shared by discovery writes (via
  `From<DiscoveredPool> for PoolInfo`) and the registry loader.

**2. Persistence** (`core/src/cache/store/{mod,pools}.rs`,
`core/src/pool/state/pool_types.rs`)

- Schema v13: `ALTER TABLE pool_info ADD COLUMN derivation_version INTEGER`
  (last entry in the migration vec, matching `SCHEMA_VERSION = 13`).
- `PoolInfo.derivation_version: Option<u32>` (`#[serde(default)]`, old JSON
  fixtures still load); read from column 21 by `row_to_pool_info`; written by
  `put_discovered_pool`; both SELECTs extended.
- `dex_type_from_i64` maps `13 => Unknown`.

**3. Strict re-derivation and controlled repair** (`core/src/pool/discovery/remote/mod.rs`, `core/src/jobs/index.rs`)

- `infer_dex_type` is unchanged externally: it delegates to the new
  `infer_dex_type_strict(...) -> Option<DexType>` and keeps the lenient
  `UniswapV2` fallback for discovery. Strict inference returns `None` when no
  family is positively identified (e.g. `"Trader Joe LB"`, `"unknown-amm"`,
  empty name), so repair cannot downgrade a correct CL/LB row to V2.
- `load_pool_registry` re-derives rows whose `derivation_version` is missing or
  older. It only *persists* a repair when the change removes skim exposure
  (`V2-like → non-V2-like`), stamping the current version. Family-only drift is
  ignored, and would-be `non-V2 → V2` downgrades are counted in
  `RegistryHealth.downgrades_refused` and never written.
- `RegistryHealth` now has `stale_derivations`, `repaired`,
  `downgrades_refused`, `unverified_v2_like`; repairs produce one aggregated
  `warn!` with up to three sample rows, and stay noisy until they are fixed.

**4. Real-cache finding (read-only check)**

`cache/avalanche-mev-scout.sqlite` contains **two** `dex_name='pharaoh'` rows:

- `0x8ac5707f…` — `dex_type=0` (V2), `factory=NULL` → repaired to
  `TraderJoeLB (8)` and stamped `derivation_version=1` on the next registry
  load. The checked-in DB file itself was **not** modified; the self-heal is
  code-driven and idempotent.
- `0xf01449c0…` — `dex_type=1` (V3), `factory=NULL` → deliberately **not**
  changed. Both families are skim-ineligible, and the bare label is less
  specific than the stored V3 classification. This is why repairs are limited
  to the skim-relevant direction.

**Tests added / extended**

- `remote::tests::strict_inference_refuses_ambiguous_names` — positive names
  still resolve; ambiguous ones return `None`; lenient contract unchanged.
- `jobs::index::tests::unknown_family_is_never_skim_eligible`.
- `jobs::index::tests::stale_row_is_repaired_and_stamped` — temp-DB round trip:
  V2/Pharaoh row repaired + stamped + removed from `v2_like`, V3/Pharaoh row
  untouched, generic-label V3 downgrade refused, second load is a no-op for
  the repaired row.
- `cache::store::pools::tests::derivation_version_column_exists_and_roundtrips`.
- Exhaustive `DexType::Unknown` arms in fee/discovery/state/pipeline/CLI/test
  helpers.

**Verification**

- `cargo check --workspace --all-targets` — clean.
- `cargo clippy --workspace --all-targets` — clean.
- `cargo test --workspace` — 558 lib tests + all integration binaries pass,
  0 failures.

**Deliberately deferred**

- Phase 2 event taxonomy (`ClaimRewards`, LB `TransferBatch`,
  `LiquidityRemove`/`RewardClaim` kinds).
- Positive-evidence gate in the skim classifier (Phase 3) and multi-claim /
  P&L-basis work (Phase 4).
- The fixture's `schema_version` table is empty (pre-existing); migrations are
  idempotent via duplicate-column handling, so v13 is applied on open without
  changing that table. Fixing version bookkeeping is out of scope here.

### Phase 2 — LB/DLMM first-class actions (implemented 2026-11-17)

Scope approved by the user: LB/DLMM first, with `LiquidityAdd` /
`LiquidityRemove` / `RewardClaim` emitted as first-class actions that bypass the
one-P&L-kind-per-tx arbitration, so a withdraw+claim transaction keeps both
rows. No checked-in DB or config fixture was modified.

**1. Event taxonomy and strict decoders** (`core/src/chain/events.rs`,
`core/src/explorer/decode/{mod,actions,tests}.rs`)

- Pinned three new topics with both a literal pin (the on-chain value) and a
  keccak signature pin: `ClaimRewards(address,address,uint256)`,
  `TransferBatch(address,address,address,uint256[],uint256[])`,
  `BinRewardAccumulatorsUpdated(uint24,uint256[])`.
- Three new `LogFamily` variants (`RewardClaim`, `LbTokenTransfer`,
  `LbRewardAccumulator`) and three strict decoders. Strictness is deliberate:
  a family match whose decoder returns `None` is **not** recorded as an
  unmapped log, so a permissive shape check would hide evidence. Address-topic
  padding, dynamic-array offsets and array lengths are all validated.
- Dispatch-table test updated to 50 unique topics; manifest pin test extended.

**2. Typed facts and new kinds** (`core/src/explorer/types.rs`)

- Added `RewardClaimFact`, `LbTransferBatchFact`, `LbRewardAccumulatorFact`
  and threaded them through `DecodedTxFacts` → `TxInput` in
  `classify/mod.rs` and `ingest::decode_successful_txs`.
- Added `MevKind::{LiquidityAdd, LiquidityRemove, RewardClaim}` with DB labels
  `liquidity_add` / `liquidity_remove` / `reward_claim`; `MevKind::ALL`,
  `parse`, census and corpus lists updated.
- Liquidity actions carry **no** profit token/amount — principal return is not
  P&L. `RewardClaim` carries the reward token/amount with `PnlBasis::F`.
- Every action is `Confidence::Inferred` (never `Exact`), with
  `details.confidence_reason = "heuristic_typed_event"`.

**3. Action classifier and arbitration bypass** (`core/src/explorer/classify/{actions,mod}.rs`)

- New `classify_actions` pass emits `LiquidityRemove` / `LiquidityAdd` from
  decoded `WithdrawnFromBins` / `DepositedToBins` facts (skipping txs already
  represented by a paired JIT position) and `RewardClaim` from `ClaimRewards`.
  ERC-20 transfer legs are bound by exact amount to fill `token0` / `token1`
  without guessing.
- A decoded LB bin withdrawal suppresses a `Skim` for that tx: the same
  outbound Transfer is principal, not skim capture.
- Actions are appended **after** `TxClaims` arbitration, so withdraw+claim in
  one tx yields two rows. Precedence is extended
  (`Liquidation=1, Jit=2, Skim=3, LiquidityRemove=4, LiquidityAdd=5,
  RewardClaim=6, ArbAtomic=7, Utility=8`) for same-tx row ordering.
- Canonical IDs for the new kinds include `details.log_index` so multiple
  same-tx actions stay distinct rows (`explorer_canonical_id`).

**4. Schema migration** (`core/src/explorer/store/schema.rs`)

- `migrate_v16_action_kinds` rebuilds `mev_ops` with a wider `kind` CHECK
  including the three new labels (plus the retired legacy labels). Existing
  rows and ids are copied verbatim; the `'unknown'` label is intentionally
  omitted for census parity with V8. Explorer DB `user_version` moves 15 → 16.

**5. Raw-log regression test** (`core/src/explorer/classify/actions.rs`)

- `audited_withdraw_and_claim_replay_to_two_actions_and_zero_unknowns`
  replays all six exact receipt-log payloads of tx `0x6ded44c4…ad4a9` (read
  read-only from `tx_evidence` in `cache/explorer-avalanche.sqlite.pre-skimfix`)
  and asserts: zero `unmapped_logs`, one JIT/bin-withdrawal fact, one reward
  claim, one LB batch, one bin-reward update, two ERC-20 transfers; output is
  exactly `[LiquidityRemove, RewardClaim]`, the pool is forced V2-like in the
  registry context yet no `Skim` is emitted, the withdrawal has no profit
  amount, the claim carries the PHAR amount, and neither row is `Confidence::Exact`.

**Verification**

- `cargo test -p mev-scout-core --lib audited_withdraw_and_claim -- --nocapture`
  — passes.
- `cargo check --workspace --all-targets` — clean.
- `cargo clippy --workspace --all-targets` — clean.
- `cargo test --workspace` — 559 lib tests + all integration binaries pass,
  0 failures.

**Acceptance status**

- Replay of Avalanche tx `0x6ded44c4…ad4a9` yields `LiquidityRemove` +
  `RewardClaim`: **met**.
- Zero unmapped logs for that tx: **met** (all six logs claimed by strict
  decoders).
- No `Skim` for that tx despite a V2-like registry context: **met**.
- No `confidence=exact` on the emitted actions: **met** (both `Inferred`).

**Deliberately deferred**

- Phase 3 positive-evidence gate in the generic skim classifier (this phase
  suppresses only decoded LB withdrawals).
- Persisting unknown events outside `tx_evidence` (needs the typed event table).
- Widening action taxonomy beyond LB/DLMM (Aave/Compound claims, generic LP
  position Add/Remove, etc.).

### Phase 3 — Skim economic gate (implemented 2026-11-17)

Scope chosen by the user: the phase's two statements conflict — "forbid
`Exact`" versus "scenario 2 still yields `exact`" — and the block+receipt
ingest path has no reserve/balance reads. The user chose **all skim ops are
`Inferred`**: a transfer-only inference can never carry the `Exact` claim, and
scenario 2's `exact` expectation is superseded by that decision (the event,
amount and `R` basis are unchanged). No schema migration, no data rewrite, no
classification rule outside skim.

**1. Positive-evidence gate** (`core/src/explorer/classify/skim.rs`)

The deny-list (`excluded` = swaps + pair-ops + every log emitter) is replaced
by an explicit allow-list. A pool outflow is a skim only when **all** of these
hold; otherwise the candidate is rejected with a named reason:

| Gate | Counter-evidence | Reject anchor |
|---|---|---|
| No LB bin action in the tx | `tx_bin_liquidity_action` | tx |
| Pool emitted no log | `pool_emitted_logs` | aggregate |
| No decoded `Swap` on the pool | `decoded_swap_on_pool` | aggregate |
| No decoded `Sync`/`Mint`/`Burn` on the pool | `pair_op_on_pool` | aggregate |
| Pool receives no tokens (one-way) | `skim_looks_like_swap` (existing) | tx |
| No liquidity fact on the pool | `decoded_liquidity_on_pool` | tx |
| No liquidity fact by the same actor (V3 CL or LB bins) | `same_actor_liquidity_event` | tx |
| Pair token0/token1 known | `pair_tokens_unresolved` | tx |
| Outflow token ∈ {token0, token1} | `outflow_token_not_in_pair` | tx |

Routine venue activity (pool logs, decoded swaps/pair-ops) is aggregated per
block so a normal DEX swap does not create one reject row per tx; ambiguous
counter-evidence keeps a tx anchor. This also retires the separate
`liquidity_action_txs` post-filter from Phase 2 — the audited withdraw+claim
tx now fails the gate itself and records `tx_bin_liquidity_action`. Outflows
to the zero address are skipped before candidacy: a burn has no recipient and
is not a capture.

**2. Pair-token identity** (`core/src/explorer/classify/mod.rs`,
`core/src/explorer/ingest.rs`)

`BlockContext.pool_tokens` carries the registry's pool → (token0, token1)
view (same source as `PoolViews.tokens`). A genuine UniV2 `skim()` can only
move the pair's own two tokens, so a foreign outflow token or missing pair
metadata fails the candidate closed.

**3. Confidence** (`skim.rs`, `normalize_confidence`)

- Skim events are emitted as `Confidence::Inferred` with
  `details.confidence_reason = "transfer_only_skim_inference"` and
  `details.detection = "v2_pair_outflow_without_counter_evidence"`.
- `details.evidence` records the gate inputs (`verified_pool`, `pair_tokens`,
  `one_way_outflow`, `no_pool_logs`, `no_pair_events`,
  `no_liquidity_action`) and `reserve_delta: "unavailable"` — the missing
  economic proof is explicit, not implied.
- `pnl_basis` stays `R`: the amount is the searcher's ledger residual; only
  the confidence claim is capped.
- `normalize_confidence` demotes any `Skim` to `Inferred` even if a future
  caller (or fixture) sets `Exact`, so the "forbid Exact" rule has one
  enforcement point.

**4. Reject reason** (`reject.rs`, `dashboard/reduce.rs`,
`docs/explorer_strategy_checklist.md`)

New `skim_counter_evidence` reason, wired into the dashboard reject ladder and
the A20 reason table.

**Tests added / extended**

- `skim::tests::one_way_pair_outflow_is_skim_but_never_exact` — positive
  control: `Skim`, `Inferred`, `R` basis, evidence block pinned.
- `pool_emitted_log_is_counter_evidence`, `decoded_swap_on_pool_is_counter_evidence`,
  `pair_op_on_pool_is_counter_evidence`, `decoded_burn_equal_to_outflow_is_counter_evidence`
  (scenario 9: outflow equals the decoded burn amount).
- `same_actor_liquidity_event_elsewhere_is_counter_evidence`,
  `bin_liquidity_in_tx_suppresses_skim`, `outflow_token_outside_the_pair_is_counter_evidence`,
  `unresolved_pair_tokens_fail_closed`, `pool_also_receiving_stays_looks_like_swap`,
  `executor_recipient_does_not_confuse_actor_gate`.
- `confidence_basis_tests::skim_can_never_claim_exact` — the normalize guard.
- A20: `skim_reports_pool_set_rejections` extended with the aggregated
  `pool_emitted_logs` reason; structural guard still green
  (`no_instrumented_pass_drops_a_candidate_without_a_reason`).
- `scenarios::baseline_skim` now asserts `Confidence::Inferred`.

**Verification**

- `cargo check --workspace --all-targets` — clean.
- `cargo clippy --workspace --all-targets` — clean.
- `cargo test --workspace -j 2` — 571 lib tests + all integration binaries
  pass, 0 failures (parallel `-j` default hits the Windows paging-file limit
  during test-binary linking; unrelated to the change).
- Audited replay
  `audited_withdraw_and_claim_replay_to_two_actions_and_zero_unknowns` still
  yields `[LiquidityRemove, RewardClaim]`, zero unknowns, no `Skim`, no
  `Exact`.
- Phase 5-style A/B replay (2026-11-17): Avalanche `100537626..100538625`
  indexed twice — Phase 2 (`984744a`, separate worktree) vs Phase 3 — with
  isolated scanner-registry copies, 1000/1000 blocks each and zero skips.
  Identical 110 ops, identical 374-row reject ladder, `skim` 0 → 0, no
  `skim_counter_evidence` (the window's registry has no skim-eligible pool).
  The anchor block re-indexes end-to-end to `LiquidityRemove` + `RewardClaim`
  with the PHAR claim priced at $0.751274 and `tx_evidence = 1`. Report:
  `docs/audits/phase3_validation_avalanche_100537626-100538625_2026-11-17.md`.

**Acceptance status (Part 6)**

- Scenario 1 (audited tx replayed): **met** — two actions, no skim, no exact.
- Scenario 2 (positive control): **met with the approved deviation** — the
  event is emitted with the correct amount and `R` basis, but as `Inferred`.
  `Exact` would require reserve/balance evidence, which this pipeline cannot
  produce (Part 4 step 6); the audit's own Q6 forbids `exact` for
  transfer-only inference, so this supersedes the earlier `exact` expectation.
- Scenario 3 (undecoded swap): **met** — inbound or emitted-swap evidence
  rejects with `skim_looks_like_swap` / `pool_emitted_logs`.
- Scenario 4 (unverified factory): **met** (Phase 1) — pool never enters
  `v2_like`; no candidate reaches the gate.
- Scenario 9 (economic gate + pricing integrity): **met** — burn/withdraw
  evidence equal to the outflow rejects; missing price still persists
  `profit_usd = NULL` with `MULTI_ASSET_PRICING` (existing `store::pnl`
  tests), gas remains netted.

**Deliberately deferred**

- Reserve/balance evidence channel (the only path that could legitimately
  re-enable `Exact` for skim). It needs a state-source producer; ingest stays
  block+receipt-only.
- Backfilling historical `confidence='exact'` skim rows: forward-only, as
  Phase 4 specifies.
- Multi-claim arbitration and basis-tagged P&L (Phase 4).
- Wider Phase 5 operational validation: multi-window replay, the
  `blocks_classified` zero-artifact decision, and registry coverage on
  Avalanche. The single Phase 3 A/B window above is complete.

