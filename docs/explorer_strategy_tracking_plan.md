# Explorer Strategy Tracking Plan

> Which catalogue strategies (`mev_strategies.md` Parts I–IV) the realized-MEV
> explorer can track, prioritized by: **capital-free / flash-loan-viable and
> cheap-to-add first**, then competition, profitability, and effort.
>
> Tracking = post-hoc classification of settled on-chain activity (the
> explorer's `MevKind` + `details` pipeline). It does **not** imply live
> detection or execution — that is `ROADMAP_LIVE_BOT.md` territory.
> Fingerprints are the §17.8 on-chain algorithms (mode A = realized event
> count, mode B = latent/storage reconstructable, mode C = needs simulation).
>
> **Scope: Avalanche C-Chain (43114).** Strategies whose protocol is not
> deployed on Avalanche were removed from §2–§5 and archived in §6.1
> (`not_on_avalanche`). Protocol availability is a precondition for both
> admission gates — no protocol, nothing to fingerprint, nothing to value.
>
> **Admission bar (two gates — an item enters this plan only if it passes BOTH):**
>
> 1. **Trackable**: deterministic on-chain fingerprint (mode A, or A+B where
>    the B part is a storage read). No off-chain legs, no simulation-only
>    value, no probabilistic label that cannot be validated against fixtures.
> 2. **Profit-measurable**: realized P&L of every classified instance is
>    computable to an accurate, reproducible number using a declared
>    valuation basis (§0.1). "Opportunity counts without $", "future savings",
>    and "unrealized position value" do **not** qualify.

---

## 0. Feasibility answer

| Class | Count | Verdict |
|-------|:-----:|---------|
| Passes both gates **and deployable on Avalanche** → in this plan | ~32 of 60 | ✅ scheduled in §2–§5 |
| Protocol not deployed on Avalanche | ~12 | ❌ moved to §6.1 (`not_on_avalanche`) |
| Fingerprint OK, P&L not exact | ~8 | ❌ dropped to §6 (`pnl_not_exact`) |
| Not / partially trackable | ~9 | ❌ dropped to §6 (`partial`) / §7 excluded |

**Already not fully trackable (gate 1 fails):** CEX–DEX arb (§2.3 — CEX leg
off-chain), cross-chain arb (§6.4 — multi-chain ingest), multi-block MEV
(§8.5 — validator intel), solver/intent surplus (§8.1 — mode C), PBS/block
building (§6.3), TWAP manipulation (§5.2 — also hostile, §15).

### 0.1 Profit valuation basis (applies to every item)

Every tracked instance stores `pnl` **plus** `pnl_basis` so numbers are never
mixed across bases:

| Code | Basis | When allowed | Accuracy |
|:----:|-------|--------------|----------|
| `R` | **Realized in-tx**: net token deltas of the acting address across the tx (flash principal nets to zero by construction); gas = `gasUsed × effectiveGasPrice` subtracted in native | end-token arb, sandwich attacker legs, JIT tip paid in tx, skim amount, claim-and-sell | exact |
| `O` | **Oracle-valued**: amounts from event args valued at the protocol's own oracle answer for that block (Aave/Compound/Chainlink/MakeDAO OSM) | liquidations, auction takes, discount captures | accurate estimate (deterministic, reproducible from archive state) |
| `F` | **Fee/reward events**: explicit fee or reward fields in logs (Gelato fee, flash-loan premium, JIT tip, keeper reward) | automation/keeper, flash routing | exact |
| `S` | **Block-close spot fallback** | only as a secondary field next to `R`/`O`, never as primary P&L | approximate — informational only |

**Family formulas** (gas always subtracted):

- **Arb / sandwich / JIT-arb / skim / long-tail** → `R`: initiator net deltas
  in received tokens; USD conversion of the final token at block-close spot is
  a *display* detail, the token amount itself is exact.
- **Liquidation (any protocol)** → `O`: `collateral_seized × oracle_price −
  debt_repaid − gas`. Repaid/seized amounts and rates are event args
  (`LiquidationCall`, `Absorb`, `Clipper.Take`, `Liquidate`).
- **Flash-loan liquidation** → `O` minus flash premium (event arg → part
  `F`).
- **Discount capture (BuyCollateral, auction)** → `O` for collateral value,
  `R` for coins paid.
- **Keeper / automation** → `F` (fee args) plus any residual `R`.
- **Notional (secondary only)**: volume shifted, debt refinanced, tab
  previewed — reported *alongside* a real `pnl`, never instead of one.

Prohibited: inventing USD from opportunity counts (the §17.5 Dune inflation
trap), mode-C re-quote as the primary number, mark-to-market of positions
sold later, and "expected future savings" (§20) as profit.

---

## 1. Baseline — already tracked (no work, P&L basis confirmed)

`MevKind` today (`core/src/explorer/types.rs:12`):

| Kind | Covers catalogue strategy | P&L basis |
|------|---------------------------|-----------|
| `ArbAtomic` | §1.3 flash-swap arb, §2.2 long-tail (partially), multi-hop | `R` |
| `Sandwich` | §3.1 — attacker legs' net delta | `R` |
| `Frontrun` / `Backrun` | §2.1 | `R` |
| `Liquidation` | §4.4 (partially), §4.6 — Aave v2/v3 + Compound v2/v3 `Absorb` (`core/src/explorer/decode.rs:587`) | `O` |
| `Jit` / `JitArb` | §3.2, §3.3 (classification side; engine detector pruned) | `F` / `R` |
| `Skim` | §1.1 | `R` |
| `Unknown` | catch-all | none |

---

## 2. Priority 0 — Capital-free, trivial effort (week 1–2)

All reuse already-decoded facts (`FlashLoanFact`, `LiquidationFact`,
`SwapFact`, transfers). No new `MevKind` required — sub-label via
`details.protocol` / `details.tags` to avoid DB migration. All pass both
admission gates.

### P0.1 — Flash-loan atomic liquidation tag (§4.4)

- **Fingerprint** (§17.8.4, mode A): same `tx_hash` has a flash-borrow
  (`FlashLoanFact` present) **and** `LiquidationFact`.
- **P&L** (§0.1): `O` (seized × oracle − repaid) minus flash premium (`F`).
- **Why first**: both inputs already decoded; Dune-validated ~323/mo on
  Polygon (§17.1); capital-free; highest validated $/tx ($493 avg).
- **Work**: in `classify.rs` liquidation pass, if `tx.flashloans` non-empty →
  `details.tags += "flash_loan_liq"` + record flash provider/fee. Test: one
  Aave `flashLoan` + `LiquidationCall` fixture, one negative (flash without
  liquidation).
- **Files**: `core/src/explorer/classify.rs`, fixtures in same file's tests.
- **Effort**: ~0.5 day.

### P0.2 — Avalanche lending label fix: Benqi (§24)

- **Fingerprint** (§17.8.4, mode A): Benqi comptroller emits Compound-V2
  `LiquidateBorrow`; the decoder already matches topic0
  (`core/src/explorer/decode.rs:639`) but mislabels the protocol
  `"compound_v2"`.
- **P&L** (§0.1): `O` — standard Compound-family formula
  (`seizeTokens × oracle − repayAmount − gas`), identical to any
  liquidation; the relabel only fixes `details.protocol`.
- **Why**: Benqi is a core Avalanche lending protocol — deployment is not
  in question, fingerprint is already decoding (mislabeled), P&L basis
  already defined. Capital-free tracking; execution flash-viable via the
  Avalanche provider set **Aave V3 → Balancer → Uni V4**.
- **Work**: address→protocol relabel registry for Avalanche (pattern:
  `remap_aave_family`, `core/src/explorer/decode.rs:728`) mapping the Benqi
  comptroller to `details.protocol = "benqi"`. **Silo V2 / Euler V2 stay out
  of the plan** until a 30-day backfill proves ≥1 realized
  `LiquidationCall` (phase gate §8.6); only then do they enter via the same
  topic0 registry.
- **Effort**: ~0.5 day.

### P0.4 — Flash-swap / flash-loan flag on arbs (§1.3)

- **Fingerprint** (§17.8.1, mode A): `uniswapV2Call` / V3 `flash` callback
  traces, or flash netting already in `classify.rs:155–169`.
- **P&L**: unchanged `R` from the arb; the tag proves capital-free execution
  (principal borrowed).
- **Work**: `details.tags += "flash_arb"` on `ArbAtomic` when principal was
  borrowed. ~0.5 day.

**P0 exit criteria**: `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace` green; each tag has ≥1 positive + ≥1 negative
fixture **and** a P&L fixture asserting an exact `pnl` + `pnl_basis`;
backfill one Avalanche range and confirm flash-liq P&L ≈ the $493/tx order
of magnitude (§17.1, Polygon-derived baseline).

---

## 3. Priority 1 — Capital-free, moderate effort (week 3–5)

### P1.1 — Interest accrual liquidation attribution (§4.13)

- **Fingerprint** (§17.8.4, mode B/A): realized `LiquidationCall` events
  flagged as **interest-driven** when the oracle price was flat over the
  prior window while debt grew (`ReserveDataUpdated` borrow-rate series).
  Cause label is inferred; the liquidation itself is deterministic.
- **P&L**: `O` — identical to any liquidation (exact formula, §0.1); the
  attribution flag only partitions an already-computed number.
- **Why**: competition 2/10, continuous, "best low-barrier zero-capital
  entry" (§11, §14 score 12.5).
- **Work**: new module `core/src/explorer/interest_attr.rs` — borrow-rate
  time series, price-flat window check, flag
  `details.interest_accrued = true`. Expected count 100–500/mo (§17.8).
- **Effort**: ~3–5 days (archive reads for validation).

### P1.3 — Long-tail token arb label (§2.2)

- **Fingerprint** (§17.8.2, mode A): multi-hop `ArbAtomic` whose path
  excludes blue-chip pairs → `details.tags += "long_tail"`.
- **P&L**: `R` — realized initiator deltas of the completed arb are exact.
  The §17.5 caveat (14x inflation, $0.26/opp) applies to **Dune opportunity
  counting**, which we deliberately do not report; only realized `pnl` is
  emitted.
- **Work**: pool-registry token allowlist check in `classify.rs` arb pass.
  ~1–2 days.

### P1.4 — Oracle-latency liquidation co-block (§4.3)

- **Fingerprint** (§17.8.4, mode A): `LiquidationCall` in a block that also
  contains a Chainlink `Updated` event for the relevant feed; mode B adds
  pre-poke divergence (archive read of pre-update state).
- **P&L**: `O` on the liquidation itself (exact); mode-B divergence is a
  secondary diagnostic field, not a $ claim. Label
  `details.oracle_poke_block = true` is co-occurrence — declared as such in
  fixtures (strong, repeatable, but correlational).
- **Profit 9/10**, capital-free via flash (§4.3).
- **Work**: Chainlink feed registry per chain in `config`; co-block check in
  `classify_block`. ~2–3 days.

### P1.5 — Automation / keeper-network execution (§21)

- **Fingerprint** (§17.8.9, mode A): Gelato `TaskExecuted`, Chainlink
  `LogTriggered`/`UpkeepPerformed` — both live on Avalanche. Keep3r
  `KeeperWork` and DFS `Trigger` are Ethereum-only → §6.1.
- **P&L**: `F` — explicit fee fields in those events + any residual token
  deltas (`R`). Every keeper execution has a machine-readable payout.
- Capital-free; generalizes to protocol-native keepers wherever deployed
  (topic0 registry — on Avalanche: Gelato/Chainlink only, see §6.1).
- **Work**: event registry entries + `details.protocol` label. ~2 days.

---

## 4. Priority 2 — Flash-viable extensions (week 5–6)

All pass both gates; P&L basis stated per item.

| # | Strategy | § | Fingerprint (mode) | P&L | Capital | Effort | Notes |
|---|----------|---|--------------------|-----|---------|--------|-------|
| P2.3 | Flash-loan liq **routing stats** | 4.4 | extend P0.1: provider (Aave V3/Balancer/Uni V4 — the Avalanche set) + premium | `F` (premium) + `O` (liq) | — | 1 d | builds on P0.1; Morpho unavailable on 43114 → §6.1 |

**Dropped from P2** (see §6 / §6.1): owner-side salvage
(`pnl_not_exact` — HF-restoring repay value needs future-state simulation),
V3 range-order snipe (`pnl_not_exact` — position value requires mode-C
quoting at exit), Fluid vault liquidations and crvUSD LLAMMA band arb
(not on Avalanche — §6.1).

---

## 5. Priority 3 — Protocol niches (week 8+)

Ordered by (competition ↑, capital-free ↑, effort ↓). Every row passed both
admission gates.

| # | Strategy | § | Mode | P&L basis | Capital | Effort | Note |
|---|----------|---|:---:|-----------|---------|--------|------|
| P3.1 | Trader Joe V2 LB bin-JIT | 7.13 | A | `F`/`R` (JIT tip, completed swaps) | low | 3 d | LB math exists in `pool/math`; Avalanche config |
| P3.2 | Balancer rate-provider staleness arb | 7.7 | A | `R` | flash | 3 d | staleness cause = inferred label |
| P3.5 | Curve pool imbalance arb | 7.1 | A | `R` | flash | 3 d | raw logs work (§17.4) |
| P3.7 | GMX V2 ADL-adjacent arb | 7.8 | A | `R` | none | 3 d | raw decode via `EventEmitter` topics (§17.3); **GMX V2 live on 43114** (docs.gmx.io contract addresses: `LiquidationHandler` 0x1eAa0E…, `EventEmitter` 0xDb17B2…) — volume phase-gated (§8.6) |
| P3.11 | ERC-4337 bundler executions | 7.4 | A | `R` (bundler/builder deltas incl. tips) | none | 3 d | EntryPoint event decode; volume phase-gated (§8.6) |
| P3.12 | Rebase + FoT token arb | 5.3/5.4 | A | `R` | low | 2 d | drift flag secondary; realized swaps only |
| P3.13 | Airdrop claim-and-sell | 7.3 | A | `R` — only txs that sell in the same tx | none | 2 d | held claims are **not** counted (no MTM) |
| P3.14 | Bad-debt / near-insolvent liq attribution | 4.15 | A | `O` (standard liq formula) + post-liq HF flag (B) | flash | 3 d | attribution only; P&L is the liq itself |
| P3.15 | Pharaoh epoch-transition arb | 7.5 (note at 2214) | A | `R` (epoch-boundary realized swaps) | medium | 3 d | §7.5's strategy maps to Pharaoh Exchange, the ve(3,3) fork on Avalanche (`mev_strategies.md:2214`); Pharaoh V3 + DLMM factories configured (`chains.toml`); ~4 opps/mo expected → `sparse` likely |
| P3.16 | sAVAX rate arb | non-catalogue (Avalanche audit) | A | `R` | flash | 2 d | `exchangeRate()` on Benqi StakedAvax vs sAVAX/AVAX pool price (Joe/Curve); Avalanche-native so gate 1 is certain — volume unproven → phase-gated (§8.6) |

> **Numbering is frozen.** Rows removed by the Avalanche scoping revision
> (P3.3 Pendle, P3.4 Lido, P3.6 Velodrome/Aerodrome, P3.8 Synthetix,
> P3.9 Liquity, P3.10 Morpho Blue) are archived in §6.1 — code comments and
> prior PRs reference the original numbers. P3.7 (GMX V2) was removed in the
> first scoping pass on a wrong "Arbitrum-only" claim and is **restored** —
> GMX V2 is deployed on 43114.

**Dropped from P3** (see §6 / §6.1): V4 hook counting
(`pnl_not_exact` — value attribution is mode C), init-price snipe
(`pnl_not_exact` — realized only on later sale), token launch snipe (same),
NFT collateral liquidation (`pnl_not_exact` — floor oracle unreliable),
solver/intent fills (`pnl_not_exact` — surplus is mode C), bridge MEV
(`partial` — destination leg outside single-chain ingest).

---

## 6. Dropped in this revision (NOT scheduled — reason recorded)

Added by the two-gate review so the plan never silently includes items that
cannot deliver a trustworthy number.

| Item | Was | Gate failed | Reason |
|------|-----|:-----------:|--------|
| Cross-market refinancing (§20) | P0.4 (rev-0) | 2 | Profit = *future* interest savings — not realized on-chain; only notional shifted is observable. Revisit if a same-tx realized delta appears (e.g., debt-mgmt with immediate withdrawal surplus). |
| `sync()` race (§1.2) | P1.6 | 1+2 | Sync itself transfers ~$0; follow-on arb attribution is fuzzy correlation. |
| Owner-side salvage (§22) | P2.4 | 1 | Weak fingerprint (near-1.0 HF repay is indistinguishable from normal deleveraging without simulation). |
| V3 range order snipe (§3.4) | P2.6 | 2 | Acquired position value requires mode-C quoting at exit. |
| Uniswap V4 hook MEV counting (§7.11) | P3.7 (rev-0) | 2 | Value attribution to hooks is mode C; counts alone are not P&L. |
| Init price snipe (§1.4) | P3.13 (rev-0) | 2 | Profit realized only at a later, unknown sale price (no MTM). |
| Token launch snipe (§8.4) | P3.19 | 2 | Same unrealized-position problem. |
| NFT collateral liquidation (§4.14) | P3.16 | 2 | Floor-oracle reliability caveat → no trustworthy valuation. |
| Solver/intent + batch auctions (§8.1/8.2) | P3.18 | 2 | Fill surplus is mode C; observable fee alone does not represent strategy P&L. |
| Bridge MEV (§6.1) | P3.20 | 1 | Destination-chain impact outside single-chain ingest. |
| CEX–DEX arb (§2.3), stat arb (§2.4), cross-chain (§6.4), multi-block (§8.5), TWAP (§5.2), PBS (§6.3) | §0 | 1 | Off-chain / multi-chain / validator legs. |

> **`Was` values marked (rev-0)** come from the pre-two-gate numbering and
> do **not** correspond to current §2–§5 items (e.g. current P0.4 = flash-arb
> flag, current P3.13 = airdrop claim-and-sell). Unmarked values match the
> live numbering.

### 6.1 Removed — protocol not deployed on Avalanche

Added by the Avalanche scoping revision (header) and the coverage audit
(catalogue vs plan diff — rows marked `— (coverage gap)`). The first group
passed both adoption gates *as strategies* but have no contract to
fingerprint on 43114, so gate 1 collapses to zero instances; the coverage
rows failed gate 1 or gate 2 outright. `Was` preserves the original
item number — code comments (`events.rs`, `decode.rs`, `classify.rs`)
reference them.

| Item | Was | Reason |
|------|-----|--------|
| Compound V3 `Absorb` → `BuyCollateral` (§26) | P0.3 | Decoder shipped (`decode.rs:744`, `classify.rs:175`) but no Comet deployment on 43114 → zero instances. Re-enable if Compound ships on Avalanche. |
| MakerDAO `kick()`/`take()` (§4.2/§4.10) | P1.2 | MakerDAO is Ethereum-only. |
| Fluid vault liquidations (§23) | P2.1 | Fluid not deployed on Avalanche (`chain.rs:311-318`). |
| crvUSD LLAMMA band arb (§25) | P2.2 | crvUSD/LLAMMA is Ethereum-only. |
| Pendle PT/YT spread arb (§7.6) | P3.3 | Pendle v2 deployments exclude 43114 (docs.pendle.finance); legacy v1 markets expired 2023. |
| Lido oracle report co-block swaps (§7.9) | P3.4 | Lido is ETH L1 only. |
| Velodrome/Aerodrome epoch arb (§7.5) | P3.6 | Velodrome/Aerodrome are Optimism/Base; no Solidly factories configured on Avalanche — but the strategy maps to Pharaoh Exchange on 43114 → new **P3.15**. |
| Synthetix flag + delayed liq (§4.7) | P3.8 | Synthetix is Ethereum/Optimism. |
| Liquity recovery + stability pool (§4.8/4.9) | P3.9 | Liquity is Ethereum-only. |
| Morpho Blue liquidations (§7.10/§24) | P3.10 | No 43114 deployment (`chains.toml` sets `morpho_blue` only for arbitrum/base/ethereum). |
| Keep3r / DFS keeper execution (§21, part of P1.5) | P1.5 | Ethereum-only networks; Gelato + Chainlink Automation remain in P1.5. |
| Silo V2 / Euler V2 liquidations (§24, part of P0.2) | P0.2 | Deployed per ecosystem reports but unproven realized volume — admitted only after a 30-day backfill shows ≥1 `LiquidationCall` (phase gate §8.6). |
| GMX v1 keeper race (§4.11) | — (coverage gap) | Catalogue lists Arbitrum + Avalanche, but current docs.gmx.io addresses cover V2 (`gmx-synthetics`) only — V1 contracts absent. Admission gated: verify legacy V1 deployment on 43114 + ≥1 `Vault.Liquidation` in a 30-day backfill (§8.6). Dune has no V1 table (§17.3) → raw decode. |
| Perp protocol keeper (§4.12) | — (coverage gap) | Catalogue chains = Arbitrum/Optimism/Polygon/Cosmos (GMX/Gains style); no perp-DEX of this class fingerprinted on 43114. |
| Convex/Curve gauge vote epoch (§7.12) | — (coverage gap) | Catalogue chains = Ethereum/Arbitrum (line 2072). Also gate-2 failure: LP-migration position value is not exactly computable. |

---

## 7. Excluded by strategy catalogue (§15 / structural — do not schedule)

| Strategy | § | Reason |
|----------|---|--------|
| Top-tier CEX–DEX arb | 2.3, §15 | CEX leg off-chain; Jump/Wintermute moat |
| Sandwich on ETH L1 | 3.1, §15 | already tracked as `Sandwich`; execution market compressed |
| TWAP oracle manipulation | 5.2, §15 | adversarial, hostile, legal exposure |
| NFT floor arbitrage | 8.3, §15 | wash-trading distortion |
| Governance MEV | 7.2, §15 | low frequency, forum monitoring |
| Multi-block MEV | 8.5 | needs validator relationships, not observable from RPC |
| Cascading liquidation engineering | 4.1 | mode C only for realized $; rare; future sim feature |
| LST/stablecoin depeg | 4.5/5.1 | rare event-driven — revisit in volatile windows, not scheduled. Avalanche surface: native USDC vs USDC.e pools (add addresses when revisited). |
| PBS / block building | 6.3 | separate block-builder product, out of explorer scope |
| L2 sequencer MEV | 6.2 | not a distinct fingerprint — reuses §17.8.6 primitives per ordering model |

---

## 8. Engineering conventions (apply to every item)

1. **Prefer `details` tags over new `MevKind` variants.** `MevKind` is a
   stored string (`core/src/explorer/types.rs:49`); new variants are a schema
   event. Promote to a variant only when queries group by it constantly.
2. **Every fingerprint ships with** (a) ≥1 positive fixture, (b) ≥1 negative
   fixture (near-miss), (c) `Confidence::Exact` vs `Inferred` declared per
   §49, (d) mode A/B/C documented in the decode function doc-comment citing
   the §17.8.x row it implements, (e) **a P&L fixture asserting the exact
   `pnl` value and `pnl_basis` code from §0.1**.
3. **`pnl` + `pnl_basis` are mandatory on every classified instance.**
   Basis must be `R`, `O`, or `F` per §0.1; `S` may exist only as a secondary
   display field. No basis → the instance is not reported as profitable
   (count-only, explicitly marked). Never invent USD from counts (§17.5
   trap) and never MTM an unrealized position.
4. **Cause labels ≠ P&L.** Where the fingerprint is co-occurrence or
   inferred (oracle co-block, staleness, interest-accrual), the label is
   stored separately from `pnl`, which always comes from the deterministic
   family formula. Fixtures must assert both.
5. **New protocols are config, not code**: addresses go in `ChainConfig`
   (`core/src/config/defaults.rs`); decoders key off topic0 where possible so
   a new address needs no code change (§24 pattern; P0.2 = Benqi address
   alias).
6. **Phase gates**: backfill a fixed block range per added tracker; record
   counts **and** P&L sums per basis in the PR description. If a tracker
   yields 0 events on a 30-day range, mark it `sparse` (§17.2 lesson); if
   P&L basis drifts from §0.1, the PR is blocked.
7. **CI per PR**: `cargo fmt --all -- --check` &&
   `cargo clippy --workspace --all-targets -- -D warnings` &&
   `cargo test --workspace`.

---

## 9. Sequencing summary

```
Week 1–2   P0.1 flash-liq tag → P0.2 Benqi relabel → P0.4 flash-arb flag
           [all capital-free, ~1.5–2 days]
Week 3–5   P1.1 interest accrual → P1.3 long-tail → P1.4 oracle-latency
           → P1.5 keeper exec (Gelato/Chainlink)
Week 5–6   P2.3 flash-liq routing stats
Week 8+    P3.1 / P3.2 / P3.5 / P3.7 / P3.11–P3.16 by comp/capital/effort
           order (P3.7 first if GMX V2 43114 volume proves out)
Never      §6 dropped (until a new exact-P&L method exists)
           + §6.1 not-on-Avalanche (until the protocol ships on 43114)
           + §7 excluded
```

Priority logic recap: everything in **P0–P1 is capital-free or flash-loan
viable** (§11 inventory) and reuses the existing decode/classify pipeline —
highest strategy-value per hour of explorer work. Protocol-niche and
event-driven items queue behind them regardless of headline profitability,
because calm-market signal volume is the binding constraint (§17.2). Every
scheduled item can answer both questions per instance: *what happened* (mode
A/B fingerprint) and *how much it earned* (§0.1 basis-tagged `pnl`) — and
every scheduled item exists on the target chain (Avalanche, header scope).
