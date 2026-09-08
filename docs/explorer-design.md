# Explorer Design — Realized-MEV Classification Methodology

> Deliverable of Phase 5 (`docs/EXPLORER_UNIFIED_PLAN.md` §14). This document
> writes out the detection rules the classifier actually implements, the
> confidence model, and the known blind spots. Companion to
> `EXPLORER_UNIFIED_PLAN.md` (architecture + roadmap).

## 1. Scope and stance

The explorer answers **"what was made"**, not "what could be made":
`mev::` detectors simulate opportunities; `explorer::` classifiers reconstruct
realized extractions from settled-block receipts. Everything is **logs-first**
(no traces in the backfill path); traces are an on-demand audit tool
(`explorer show <TX> --trace`, prestateTracer diffMode).

Confidence semantics:

| Level | Meaning | Source |
|---|---|---|
| `exact` | Deterministic event/flow match (liquidation events, closed-cycle arbs, sandwich structure, JIT Mint+Burn pairs) | Pattern structure |
| `inferred` | Heuristic attribution (profitable residual without a matching structure) | Balance deltas only |

`inferred` ops are never presented as ground truth in cross-validation
headlines (they populate the `unknown` bucket and feed the M8 audit).

## 2. Data model

Input per block: header + ordered successful txs with receipts. Decoded facts
(`core/src/explorer/decode.rs`):

- **Transfers** — raw ERC-20 `Transfer(topic 0xddf252…)`; the accounting
  primitive. Log position within the tx is preserved.
- **Swaps** — V2/V3/V4/Curve/Balancer/Solidly/LB/Pendle events. Direction is
  resolved by *transfer pairing*: for a swap log on pool `P`, `token_in` is the
  nearest prior Transfer with `to == P`, `token_out` the nearest later
  Transfer with `from == P`. Registry-free and chain-generic; V3 signed
  amounts carry a sentinel resolved by the same pairing.
- **Liquidations** — per-protocol event registry: Aave V3 `LiquidationCall`
  (collateral/debt/user in topics; amounts in data), Compound V3 `Absorb`.
  `LiquidationCall` carries no liquidator address — the tx sender is the
  attribution target.
- **JIT facts** — V3 `Mint`/`Burn` with owner + tick range.

## 3. Classifier (per settled block, ordered passes)

`core/src/explorer/classify.rs`. Pass order is **first-match-wins and
disjoint** so counts sum.

1. **Liquidation pass** — exact event match. Profit = collateral seized
   (same-tx swap of collateral is the arb pass's job). `exact`.
2. **Swap attribution** — per-tx `DeltaLedger`: signed per-address per-token
   deltas from the Transfer stream (+ tx `value` as a native marker).
   Noise filters:
   - *wrap noise*: Transfers from/to the zero address on the wrapped-native
     token (deposit/withdraw pairs) are excluded.
   - *fee accrual*: pool fees stay inside the pool contract and net out of
     searcher deltas; no special handling needed.
   - *flash-loan netting*: borrow/repay loops are stripped
     (`net_flash_loan`) before the residual is read.
3. **Atomic arb pass** — the tx's swap sequence forms a directed token edge
   list; a connected closed walk (≥2 pools) starting/ending on the same token
   plus a positive single-token residual for the searcher candidate confirms
   `arb_atomic` (`exact`). Searcher candidates: tx sender, tx `to` contract.
   Profit-token priority: chain stables (USDC → USDT → DAI) → wrapped native →
   largest-delta fallback (§8.2.4 of the plan).
4. **Sandwich pass** — cross-tx, same-block state machine keyed
   `(pool, attacker EOA)`: front-run swap → ≥1 third-party (victim) swap →
   opposite-direction back-run by the same EOA. Profit = back-run output −
   front-run input (netted in the pool's input token); victim hash + size are
   recorded. `exact` structure; profit is an approximation because the two
   legs may be denominated in different tokens (see §6).
5. **JIT pass** — block-wide pairing of V3 `Mint` → `Burn` with identical
   (pool, owner, tick range) and burn.liquidity ≥ mint.liquidity;
   `jit_arb` when the same tx also closed an arb cycle.
6. **Unknown pass** — profitable residual, no matching structure
   (`inferred`). Includes probable CEX–DEX bots (non-atomic legs are
   invisible; only the on-chain half shows up).

## 4. Accounting (§8.2)

Gross profit = searcher's positive net delta of the profit token.
Gas = `gasUsed × (base + priority)` from the receipt (the scanner's receipt
conversion does not carry `effectiveGasPrice`, so effective is approximated
as base + max-priority — documented limitation). Net = gross − gas(USD) −
flash-loan fees. USD via hourly price cache: CoinGecko (native, live) +
DefiLlama (tokens, backfill); tokens with no stable leg are reported in
token units and excluded from USD aggregates.

## 5. Canonical form and cross-validation

`explorer_canonical_id` (§11.1.1 item 0) mirrors `compute_canonical_id`:

- arb: `ArbAtomic|<sorted pool set>` — route order differs between searchers
  and simulation, so the set is canonical.
- sandwich: `Sandwich|<pool>|victim:N|backrun:M` matching the opportunity side.
- liquidation: borrower+liquidator pair. JIT: pool+tick range.

T1 exact matching is *aspirational* (simulation IDs vs flow IDs rarely
coincide); `validate` reports T1∪T2 (headline) and T3 (ceiling) separately
and never merges them. Recall is **USD-weighted first** (count-weighted
flatters trivial ops), threshold-swept, and bucketed over time.

## 6. Miss taxonomy (M1–M8) — implemented semantics

Ordered attribution, first match wins (`explorer/validate.rs`):

1. **M1 pool-gap** — a realized route pool absent from the scanner's
   opportunity pool set → config/discovery fix.
2. **M7 scanner coverage** — scanner never ran the block → pipeline fix.
3. **M4 gas-model** — a matching-pool rejection with reason `gas_dominates`.
4. **M5 quote-math** — rejection `quote_nonpositive` on a matching strategy.
5. **M3 threshold** — rejection `below_min_profit` on a matching strategy.
6. **M6 competition** — scanner covered the block but realized extraction is
   unmatched (detection success, execution loss; reported, out of scope).
7. **M8 false-ground-truth** — `unknown`-kind leftovers are audit suspects.
8. **unknown-coverage** — no rejections recorded for the window; reported
   separately, never folded into M3/M5 (this is why `--record-rejections`
   must be ON for windows fed to `validate`).

## 7. Known blind spots (accepted)

1. **Multi-hop attribution is heuristic** — chained arbs across contracts can
   split/mask deltas; confidence + `--trace` audit cover case-by-case.
2. **Effective gas approximation** — see §4; exact numbers via `--trace`.
3. **Non-atomic / CEX–DEX MEV invisible** — logs-only sees one side.
4. **Victim loss is approximate** — attacker profit + victim size only;
   counterfactual slippage deferred (needs simulation).
5. **Survivorship bias** — the explorer sees only landed, profitable
   extractions; never compare opportunity counts to op counts directly.
6. **Sandwich profit units** — front-run input and back-run output are the
   same token only when the attack is the common buy→sell shape; exotic
   multi-token sandwiches fall back to the raw difference (flagged in
   details).
7. **Atomic chain txs (Avalanche)** — non-EVM C-Chain txs have no DEX-shaped
   logs and classify to nothing; they never error the classifier.

## 8. Per-chain adaptation (config, not code)

The classifier is chain-generic. Per chain, configuration supplies: wrapped
native, stable addresses (profit priority), lending-pool registry
(liquidations), factory registries (scanner side), reorg depth
(confirmations), and price feed asset ids. Polygon seeds and the chain matrix
are in the unified plan §13.
