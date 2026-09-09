# DEX Coverage Gap Plan

Plan for closing the gap between **what mev-scout can discover/decode per chain** and
**where trading volume actually is** (snapshot: September 2026, DefiLlama / CoinGecko /
GeckoTerminal).

Motivation: the built-in factory lists were assembled Polygon-first and skew toward
2024-era leaders. Volume leadership has since shifted (Pharaoh overtook LFJ on Avalanche,
Velodrome V3/Superchain CL launched, Fluid/Metric/Infinity grew into top-5 venues on
several chains). Missing a DEX is silent: discovery simply yields fewer pools and
detectors report zero opportunities for that venue.

---

## 1. Engine capability today

`core/src/dex_type.rs` supports 10 DEX engine types:

| DexType | Discovery | Decoder | Math |
|---|---|---|---|
| UniswapV2 | ✅ `discovery/v2.rs` | ✅ | ✅ `math/core.rs` |
| UniswapV3 (incl. **Algebra forks** via `Pool(address,address,address)` topic) | ✅ `discovery/v3.rs` | ✅ | ✅ `math/v3.rs` |
| UniswapV4 | ✅ `discovery/v4.rs` | ✅ | ✅ |
| PancakeSwap Infinity (CL) | ✅ `discovery/infinity.rs` | ✅ | ✅ (reuses CL/V3 math) |
| Curve | ✅ `discovery/curve.rs` (registry: Ethereum only) | ✅ | ✅ `math/curve.rs` + `stable_swap.rs` |
| Balancer | ✅ `discovery/balancer.rs` | ✅ | ✅ `math/balancer.rs` |
| Solidly (Aerodrome/Velodrome V2) | ✅ `discovery/solidly.rs` | ✅ | ✅ |
| Camelot | ✅ `discovery/camelot.rs` | ✅ | ✅ (slot 8) |
| TraderJoeLB | ✅ `discovery/trader_joe.rs` | ✅ | ✅ `math/lb.rs` |
| Pendle | ✅ `discovery/pendle.rs` | ✅ | ✅ `math/pendle.rs` |

Factory sources: `core/data/chains.toml` (effective defaults, merged at load) and
`ChainName::default_*_factories()` in `core/src/types/chain.rs` (fallback when the
config omits factories).

Remote discovery: GeckoTerminal (`discovery/remote/geckoterminal.rs`) +
DexScreener (`discovery/remote/dexscreener.rs`), all 7 chains mapped.
Classification via `infer_dex_type()` — **unknown labels fall back to `UniswapV2`**,
which silently misclassifies every DEX not in its match list (see §3.1).

---

## 2. Volume leaders vs. coverage (Sept 2026 snapshot)

Volumes are 24h/30d approximations; ranks are stable, absolute numbers fluctuate.
Support column: ✅ covered · 🟡 engine type exists, factory address missing → **config-only**
· ❌ no decoder → **new code** · ⚠️ misclassified by remote discovery today.

> **Volume refresh:** figures below are a Sept 2026 snapshot. Re-read live data before
> committing to priorities via the DeFiLlama API:
> `curl "https://api.llama.fi/overview/dexs/<chain>?excludeTotalDataChart=true&excludeTotalDataChartBreakdown=true"`
> filtering on `total24h` descending. Ranks are more stable than absolute numbers.
> **Verified via API pull 2026-09-08:** the four discrepancy claims below all
> reproduced (Aqua $308M, Slipstream $488M, Infinity $342M, Pharaoh DLMM $153M/24h).
>
> **Discrepancies vs. the first draft (Sept 2026 DeFiLlama pull):**
> 1. **1inch Aqua** (~$312M/24h) has become the #1 venue on Ethereum — above even
>    Uniswap V4. It is an intent-based AMM; see §3.6 for scoping.
> 2. **Aerodrome Slipstream** on Base (~$484M/24h) leads the chain — not Uniswap V3.
>    Its factory must be added to `[base] uniswap_v3_factories` (Phase 1.1b).
> 3. **PancakeSwap Infinity** on BSC (~$341M/24h) is now the chain's #2 venue — the
>    volume share listed below as "growing" understates it.
> 4. **Pharaoh DLMM** (~$147M/24h) is larger than the plan's early estimate.

### Ethereum (~$29–33B 30d)
| # | DEX | ~Vol 24h | Support | Gap action |
|---|-----|----------|---------|------------|
| 1 | 1inch Aqua (intent-based AMM) | ~$312M | ❌ | Phase 3.6 (scoping) |
| 2 | Uniswap V4 | ~$308M | ✅ | — |
| 3 | Uniswap V3 | ~$227M | ✅ | — |
| 4 | Fluid | ~$76M | ❌ | Phase 3.2 |
| 5 | Lista DEX | ~$45M | ❌ | Phase 3.7 |
| 6 | Metric | ~$35M | ❌ | Phase 3.4 |
| 7 | Curve | ~$31M | ✅ | — |
| 8 | Ekubo | ~$27M | ❌ | Phase 4 (low prio) |
| 9 | Hashflow | ~$19M | ❌ | descope — RFQ order flow, no pool state (see guardrails) |
| 10 | Native Swap | ~$16M | ❌ | descope (see guardrails class) |
| 11 | Maverick V2 | ~$13.6M | ❌ | Phase 4 (low prio) — new engine type, AMM proper |
| 12 | Pendle V2 | ~$11.1M | ✅ | already covered — `pendle_factory` set in `[ethereum]` |
| 13 | PancakeSwap AMM V3 | ~$8.1M | 🟡 | **Phase 1.2b** — config-only, same deterministic factory as Base |
| 14 | Uniswap V2 | ~$4.7M | ✅ | — (earlier ~$20M figure was stale) |
| 15 | Balancer | ~$1.7M | ✅ | — |

### Base (~$26B 30d)
| # | DEX | ~Vol 24h | Support | Gap action |
|---|-----|----------|---------|------------|
| 1 | Aerodrome Slipstream (CL) | ~$484M | ⚠️ | Phase 1.1b — verify factory + add to `[base]` |
| 2 | Uniswap V3 | ~$106M | ✅ | — |
| 3 | PancakeSwap V3 | ~$86M | 🟡 | Phase 1.2 |
| 4 | Metric V2 | ~$75M | ❌ | Phase 3.4 |
| 5 | Uniswap V4 | ~$49M | ✅ | — |
| 6 | Tessera V | ~$27M | ❌ | Phase 4 (low prio) — verified live ($27.3M/24h) |
| 7 | Aerodrome V1 (classic) | ~$22M | ✅ | — |
| 8 | Hanji Protocol / ElfomoFi | ~$10–14M each | ❌ | new venues — Phase 4 (low prio) |
| 9 | PancakeSwap Infinity | <$10M **on Base** — the ~$341M/24h figure is **BSC-only** | ✅ (BSC) | Phase 3.3 landed (2026-09-09) — BSC wired; Base manager address unknown → not wired there |

### BSC (~$43B 30d)
| # | DEX | Share | Support | Gap action |
|---|-----|-------|---------|------------|
| 1 | PancakeSwap V3 | ~$528M/24h ≈ 40% of chain — the "~85%" figure is stale | ✅ | — |
| 2 | PancakeSwap Infinity | ~$342M/24h — chain #2 venue, not "growing" | ✅ | Phase 3.3 landed (2026-09-09) — see row below |
| 3 | GMGN | ~$141M | ❌ | **descope** — trading bot / aggregator-adjacent, no trackable pool state (guardrails class; applies without a Q3-style spike) |
| 4 | Uniswap V4 | ~$115M/24h | ✅ | — |
| 5 | PancakeSwap V2 | ~$114M/24h — still material, not just "declining" | ✅ | — |
| 6 | Uniswap V3 | ~$60M/24h | ✅ | — |
| 7 | Metric V2 | ~$56M/24h | ❌ | Phase 3.4 |
| 8 | Lista DEX | ~$38M/24h | ❌ | Phase 3.7 |
| 9 | Topaz CL | ~$30M/24h | ❌ | new venue — Phase 4 (low prio) |
| 10 | Flap sh | ~$35M/24h | ❌ | **descope** — launchpad, not a DEX (DeFiLlama category "Launchpad"; guardrails class, see Phase 3 descope guardrails) |
| 11 | Native Swap | ~$16M/24h | ❌ | Phase 4 (low prio) |

### Arbitrum (~$4.9B 30d)
| # | DEX | ~Vol 24h | Support | Gap action |
|---|-----|----------|---------|------------|
| 1 | Uniswap V3 | ~$71M (~70%) | ✅ | — |
| 2 | Uniswap V4 | ~$19M | ✅ | — |
| 3 | Metric V2 | ~$10M | ❌ | Phase 3.4 |
| 4 | PancakeSwap V3 | ~$7M | ✅ | — |
| 5 | Fluid | ~$6.1M | ❌ | Phase 3.2 |
| 6 | Camelot (V3 Nitro + V2) | ~$5.0M | ✅ | — |
| 7 | WOOFi | ~$2.4M | ⚠️ | Phase 3.1 classification |
| 8 | GMX V2 AMM (spot leg) | ~$1.5M | ❌ | perp-adjacent — out of scope |
| 9 | Pendle V2 | ~$1.0M | ✅ | already covered — `pendle_factory` set in `[arbitrum]` |
| 10 | Maverick V2 | ~$0.5M | ❌ | Phase 4 (low prio) |
| 11 | LFJ V2.2 (Arbitrum) | ~$0.3M | 🟡 | covered by Phase 1.5 list field |
| 12 | Ramses V3 (CL) | minor | ✅ | — |

### Polygon (~$4.5B 30d)
| # | DEX | ~Vol 24h | Support | Gap action |
|---|-----|----------|---------|------------|
| 1 | Polymarket International | ~$55M | ❌ | **descope** — prediction-market CLOB, no trackable AMM pool state (guardrails class) |
| 2 | Uniswap V4 | ~$26M | ✅ | — |
| 3 | Metric V2 | ~$18M | ❌ | Phase 3.4 |
| 4 | Uniswap V3 | ~$15M | ✅ | — |
| 5 | RamsesX (CL V2) | ~$13.7M | 🟡 | Phase 1.3 |
| 6 | QuickSwap (Dex + V3, + V4 leg) | ~$14.8M | ✅ | — |
| 7 | DODO | ~$6.1M | ❌ | Phase 4 (low prio) |
| 8 | Balancer V2 | ~$2.9M | ✅ | — |
| 9 | Uniswap V2 / Sushi V3 | ≤$0.2M each | ✅ | effectively dead on Polygon |
| 10 | Curve | absent from top-25 | 🟡 | Phase 0 note |

> Polygon's top-25 is dominated by prediction markets and card products
> (Polymarket, Courtyard, Betmoar, MetaMask/Gate Predictions, Azuro, Nexo/Kolo
> cards) — all guardrails-class descopes, none decodeable as AMMs.

### Optimism (~$650M 30d)
| # | DEX | ~Vol 24h | Support | Gap action |
|---|-----|----------|---------|------------|
| 1 | Velodrome V3 (CL, OP + Ink + Soneium) | ~$12M | 🟡⚠️ | Phase 1.1 + Phase 3.1 |
| 2 | EtherFi Cash Liquid | ~$4.1M | ❌ | **descope** — card/liquid product, not a pool-state AMM (guardrails class) |
| 3 | WOOFi | ~$1.4M | ⚠️ | Phase 3.1 classification |
| 4 | Uniswap V3 | ~$1.3M | ✅ | — |
| 5 | Uniswap V4 | ~$0.8M | ✅ | — |
| 6 | Velodrome V2 | ~$0.6M | ✅ | — |
| 7 | Curve / Solidly V3 / DODO | minor | 🟡/❌ | **Phase 0 note** — Curve: verify factory address on Optimism (see Phase 0 Curve gap); Solidly V3: likely misclassified by `infer_dex_type` (Phase 2.1 fix); DODO: out of scope (Phase 4 low prio) |

**Merger watch:** Aerodrome + Velodrome merged into **Aero** (announced Nov 2025,
AERO token launch ~Jul 2026, 94.5/5.5 split). Contract migration may re-point
factories on Optimism + Base. → Phase 4.

### Avalanche (~$7B 30d)
| # | DEX | ~Vol 24h | ~Vol 30d | Support | Gap action |
|---|-----|----------|----------|---------|------------|
| 1 | **Pharaoh DLMM** (bins, x(3,3)) | ~$147M | ~$390M+ | ❌⚠️ | Phase 3.1 + Phase 3.5 |
| 2 | **Pharaoh Exchange V3** (CL, UniV3 fee tiers 0.01/0.05/0.30/1%) | ~$67M | ~$896M | 🟡⚠️ | Phase 1.4 + Phase 3.1 |
| 3 | Metric V2 | ~$12M | — | ❌ | Phase 3.4 |
| 4 | Uniswap V3 | ~$5M | — | ✅ | — |
| 5 | Blackhole (CLMM + AMM) | ~$5.0M | ❌ | **Phase 1.8 candidate** — if the CLMM is Algebra-family it is config-only; otherwise low prio |
| 6 | LFJ V2.2 (Liquidity Book) | ~$0.9M | — | 🟡 | Phase 1.5 |
| 7 | LFJ V2.1 / Pangolin V2 / Sushi | minor (V2-era) | — | 🟡 | Phase 1.5 (optional) |

> Historical note: the old "LFJ leads Avalanche ~35%" figure is stale — Pharaoh
> (DLMM + V3 combined ~$1.3B+/30d, 100% on Avalanche) now dwarfs LFJ and Uniswap.
> Pharaoh's TVL is small (~$38M) but turnover is extreme — high rotation is
> MEV-relevant. **Note:** Pharaoh DLMM's daily volume is now ~3× the earlier
> estimate above — Avalanche is unusable as a target until both Pharaoh decoders land.

---

## 3. Work plan

### Phase 0 — Audit existing `chains.toml` addresses (config hygiene, ~0.5 day)

Silent-failure risk: wrong addresses never error; they just yield nothing.

| Field | Chain | Issue | Verify against |
|---|---|---|---|
| `aave_v3_pool` | BSC | `0x87870Bca...` = Ethereum mainnet Aave V3 Pool — BSC deployment has a different address | docs.aave.com deployments |
| `balancer_vault` | BSC | Balancer is **not deployed** on BSC; entry is inert or wrong | Balancer deployments |
| `trader_joe_factory` | Ethereum | Trader Joe is not on mainnet — remove | LFJ docs |
| `aave_v3_pool` | Arbitrum, Optimism, Avalanche | Same address as Polygon (`0x794a...1ad`) — plausible (Aave reuses one proxy address across several chains) but confirm per-chain | docs.aave.com |
| `trader_joe_factory` | Avalanche | Currently points at V2.1 (`0xb43120...`); V2.2 is the live venue. Field is a single `Option<String>` — promote to `Vec<String>` so V2.1 + V2.2 can coexist (see Phase 1.5; consumers in `pool/discovery/trader_joe.rs`, `config/validation.rs`) | LFJ docs |
| `pool_discovery_start_block` | all non-Polygon | Defaults were never exercised — full validation procedure lives in **Phase 2.5** (kept there to avoid duplicating the guard design) | Phase 2.5 |

**Phase 0 additional — Curve coverage gap:** Curve discovery (`discovery/curve.rs`) only
uses a **registry** on Ethereum. Other chains (BSC, Arbitrum, Optimism) need direct
**factory** addresses instead — Curve supports both styles. Per-chain Curve factories to
confirm and add to `chains.toml` (e.g. Curve StableSwap factory on BSC
`0x0959158b6040D32d04c301A72CBFD6b39E21c9AE` — verify via `getcode`); either extend the
Curve discovery to read factories per chain or add a config field.

**`aave_v3_pool` BSC:** the configured address is Ethereum mainnet's Aave V3 Pool.
On BSC that address holds no such contract, so wiring it as the flash-loan provider
makes every flash-loan probe fail and the runner silently reports
SKIPPED_NO_FLASHLOAN — the same failure mode as a wrong factory. The fix is the
Phase 0 row above (replace with BSC's real Pool proxy); no protocol-mixup is
involved, just a wrong-address copy.

Validation: `mev-scout config` per chain + a single-block `scan --kind flashloans`
probe against each configured protocol contract.

### Phase 1 — Config-only quick wins (no code changes, ~1 day total)

The V3 discovery path scans **both** canonical (`PoolCreated(address,address,uint24,int24,address)`)
and Algebra (`Pool(address,address,address)`) topics over every factory in
`uniswap_v3_factories` (`core/src/pool/discovery/v3.rs:47`), so **any** V3-family
factory (canonical, Algebra, Pancake V3, Ramses, Velodrome Slipstream, Pharaoh V3)
is a pure config addition — fee/tick metadata is repaired later via `eth_call`.

| # | Item | File | Effort |
|---|---|---|---|
| 1.1 | Velodrome Slipstream/V3 CL factory → `[optimism] uniswap_v3_factories` (Algebra family — verify exact factory addr on-chain before committing) | `core/data/chains.toml` | trivial |
| 1.1b | **Aerodrome Slipstream factory → `[base] uniswap_v3_factories`** — the chain's #1 venue (~$484M/24h). **Verify first whether it emits the Algebra `Pool(address,address,address)` topic** or a bespoke event; Slipstream is the same ALM/CL family as Velodrome V3 so config addition is likely sufficient, but the topic check gates it | `core/data/chains.toml` | trivial + verification |
| 1.2 | PancakeSwap V3 factory on Base → `[base] uniswap_v3_factories` (deterministic deploy `0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865` — same as Arbitrum entry; confirm via `getcode`). **Note:** PancakeSwap V3 routes swaps through a `PancakePair` with a **masterchef-style pool address** — verify the `PoolCreated` topic signature matches what `discovery/v3.rs:47` scans | `core/data/chains.toml` | trivial + verification |
| 1.2b | **PancakeSwap V3 factory on Ethereum** → `[ethereum] uniswap_v3_factories` — same deterministic address `0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865` ($8.1M/24h currently invisible) | `core/data/chains.toml` | trivial |
| 1.3 | RamsesX factory → `[polygon] uniswap_v3_factories` (Algebra fork; ~$406M/30d) | `core/data/chains.toml` | trivial |
| 1.4 | Pharaoh V3 factory → `[avalanche] uniswap_v3_factories` — **first verify implementation family** (canonical UniV3 vs Algebra vs Pancake-v3-style); fee tiers match UniV3. If the pool address in the `PoolCreated` topic is a **proxy**, the topic-scanned address differs from the implementation — decode must handle this | `core/data/chains.toml` | trivial + verification |
| 1.5 | LFJ: promote `trader_joe_factory: Option<String>` → `trader_joe_factories: Vec<String>` in `ChainConfig`, then add V2.2 Liquidity Book factory alongside V2.1 (`0xb43120...`) on **Avalanche, and also on Arbitrum** (LFJ V2.2 there ~$0.3M/24h — §2 Arbitrum row). V2.1 and V2.2 are **both live** — removing V2.1 loses historical backtest coverage; keeping both requires the list field. Update all consumers (`pool/discovery/trader_joe.rs`, `config/validation.rs`) | `core/data/chains.toml`, `core/src/config/defaults.rs` + consumers | small |
| 1.6 | *(optional)* Pangolin **V3** factory → `[avalanche] uniswap_v3_factories` — live data shows Pangolin V3 active ($1.3M/24h); the Pangolin **V2** factory referenced in the `chain.rs` fallback list is effectively dead | `core/data/chains.toml` | trivial |
| 1.7 | *(optional)* Curve **direct factories** per non-Ethereum chain (see Phase 0) → new config field `curve_factories` + discovery support | `chains.toml`, `discovery/curve.rs` | small |
| 1.8 | *(optional)* **Blackhole CLMM** (Avalanche, chain #4 at ~$5.0M/24h — bigger than Uniswap V3 there) — verify whether its CL pools are Algebra-family; **if Algebra-family**: factory → `[avalanche] uniswap_v3_factories` is config-only (Phase 1). **If not Algebra-family**: demote to Phase 4 (low prio) — requires new decoder. Its Solidly-style "AMM" leg is negligible ($21K) | `core/data/chains.toml` | trivial if Algebra; Phase 4 otherwise |
| 1.9 | *(optional, needs schema change)* **QuickSwap V4 on Base** ($2.4M/24h) — V4-family, but `v4_pool_manager` is a single `Option<String>` per chain; supporting both Uniswap V4 and QuickSwap V4 on one chain requires promoting it to a list (same pattern as Phase 1.5). **Check whether QuickSwap V4 runs its own PoolManager on Polygon too** — if yes, the list promotion is needed there as well (Polygon's current `v4_pool_manager` is Uniswap V4's, so a second V4 manager is not representable today) | `core/data/chains.toml`, `core/src/config/defaults.rs` | small |

**Acceptance:** `mev-scout -f <cfg> discover --source onchain` per chain lists the new
factories; `validate-pools` recall for those DEXes goes from 0 to ≥ target.

> **Status (2026-09-09)** — implemented in this working tree (uncommitted):
> - **1.1 / 1.1b / 1.2 / 1.2b / 1.3 / 1.4** ✅ — Slipstream topic scan arm + factories on
>   Base/Optimism, Pancake V3 (Base/Ethereum/Arbitrum), RamsesX (Polygon), Pharaoh V3
>   (Avalanche) all live. Pin tests `phase_d_factories_present`,
>   `coverage_plan_v3_factories_present` guard the lists.
> - **1.5** ✅ `trader_joe_factories: Vec<String>` landed; LFJ V2.1+V2.2 on Avalanche/Arbitrum.
> - **1.6 / 1.8** ✅ — Pangolin V3 `0x1128F23D...` and **Blackhole CLMM
>   `0x512eb74954...`** (confirmed **Algebra Integral** from the official `BlackholeV3`
>   docs.rs deployment table → `Pool(address,address,address)` topic) added to
>   `[avalanche] uniswap_v3_factories` (chains.toml + `chain.rs` mirror + pin test).
> - **1.7** ✅ — new `ChainConfig.curve_factories` + `DiscoveryConfig.curve_factories`;
>   `scan_curve_batch` now scans all configured curve authorities over **both**
>   `PoolAdded(address,uint256)` and `PoolDeployed(address)` topics (CurveStableswapFactoryNG
>   emits the latter). Seeded from the official Curve deployments page: Polygon
>   `0x1764ee18...`, BSC `0xd7E72f36...`, Arbitrum `0x9AF14D26...`, Ethereum
>   `0x6A8cbed7...`, Optimism `0x5eeE3091...`. Mainnet registry still scanned alongside.
> - **1.9** — **resolved by research, not code**: QuickSwap V4 is **Algebra Integral V4, not
>   UniswapV4-family** (AlgebraPoolDeployer ABI on BaseScan, QuickSwap "Algebra Integral =
>   their V4" docs, "Polygon POS V4 Algebra" section). A second canonical V4 PoolManager
>   cannot exist on either chain, so the `v4_pool_manager` list promotion is **not needed**;
>   when QuickSwap's V4 **Algebra factory** addresses are confirmed per chain they go into
>   `uniswap_v3_factories` as config-only (same as QuickSwap V3). No schema change.
> - **2.1** ✅ — `infer_dex_type()`/`is_unsupported_dex()` no longer poison unknown labels
>   as `UniswapV2`.
> - **3.5** 🟡 — Pharaoh DLMM is an **Liquidity-Book-family** deployment (docs.phar.gg
>   `DLMMFactory 0xEb480050b016f6c6d45203D2346B68bDDDa23D4D`, Dune reuses the LB v2.1 macro).
>   Factory wired into `[avalanche] trader_joe_factories` (reuses the `TraderJoeLB` arm).
>   **Q6 in the open-questions table remains open**: the DLMM `LBPairCreated` indexed-layout
>   matches `LB_PAIR_CREATED_TOPIC` on assumption only — on-chain log verification is deferred
>   (no reliable RPC); if the signature differs, promote 3.5 to a bespoke decoder phase.
> - **2.2** ✅ — curated per-chain GeckoTerminal slug ladder landed in
>   `core/src/pool/discovery/remote/mod.rs` (`curated_dex_slugs()`), queried ahead of the
>   network enumeration in `supplement_via_per_dex()` (no slug queried twice). Ordered by
>   §2 volume ranks, e.g. base `aerodrome-slipstream → uniswap-v3-base → …`, avalanche
>   `pharaoh-dlmm → pharaoh-exchange-v3 → …`. Pin test guards the leaders per chain.
> - **2.5** ✅ — `SqliteStore::earliest_creation_block_by_factory()` (first-observed-block
>   cache over `pool_info.factory` + `MIN(creation_block)`, no schema change) + a
>   `discover.rs` guard that warns when `pool_discovery_start_block` is later than the
>   earliest observed pool-creation block for a factory. Both cases unit-tested (incl.
>   remote rows with `creation_block == 0` being ignored).
> - **3.7** 🟡 — feasibility done: Lista is a **true AMM** with three engines (SmartSwap
>   stableswap, ListaV3 = UniV3 fork, ListaV2 = UniV2 fork). BSC ListaV3/V2 factory
>   addresses added **config-only** to `[bsc] uniswap_v3_factories` / `uniswap_v2_factories`
>   (chains.toml + `chain.rs` mirror). Stableswap engine has no verified pool-creation
>   event → not wired; topic compatibility still deferred (Q9).
> - **3.2 / 3.4 / 3.6** — feasibility studies **complete**; all three are
>   **new-decoder** phases (verdicts + contract addresses in the Phase 3 table). Not yet
>   implemented.
> - **3.3 (Pancake Infinity)** ✅ — implemented this working tree (uncommitted): discovery +
>   decoder + CL-math reuse + toml-only `infinity_cl_pool_manager`; see the Phase 3.3 row.
> - **Remaining:** 3.2 (Fluid), 3.4 (Metric), 3.6 (Aqua) decoders,
>   Phase 4 (Aero watch), Phase 5 (recall harness + weekly volume check).

> **Config-only vs new-decoder paths.** Everything in Phase 1 reuses an existing
> `DexType` (the engine already scans that factory's event family), so the Phase 3
> checklist does **not** apply. The only code touchpoints are the `chains.toml` entry and
> — where a field is promoted to a list (1.5, 1.9) — `config/defaults.rs`
> `pick_factories()`, `config/validation.rs`, and the consuming discovery module.

### Phase 2 — Remote-discovery classification fixes (small code, ~0.5 day)

**2.1 Fix `infer_dex_type` fallback poisoning** (`core/src/pool/discovery/remote/geckoterminal.rs:425`
and the label heuristic in `remote/dexscreener.rs:315`).

Today every unknown GeckoTerminal dex id → `DexType::UniswapV2`. Concretely wrong today:

| GeckoTerminal label | Currently classified as | Correct |
|---|---|---|
| `velodrome`, `velodrome-v2` | UniswapV2 ❌ | Solidly |
| `aerodrome`, `aerodrome-v1` | UniswapV2 ❌ | Solidly |
| `velodrome-v3` / `velodrome-slipstream` | UniswapV2 ❌ | UniswapV3 (Algebra) |
| `pharaoh-v3` | UniswapV2 ❌ | UniswapV3 |
| `pharaoh-dlmm` | UniswapV2 ❌ | new type (Phase 3.5) |
| `pancakeswap-infinity` | (fixed) → `DexType::PancakeInfinity` ✅ | live — mapped in both remote sources, removed from UNSUPPORTED |
| `fluid` / `metric` / `dodo` / `woofi` | UniswapV2 ❌ | skip + warn until decoded |

Actions: extend the match list above the generic fallback; for not-yet-decoded
protocols return a `DexType` that **skips** the pool (or a `RemotePool.supported=false`
flag) instead of silently importing it as UniswapV2 — misclassified pools poison
pool state application (`pool/state/apply.rs`) and produce wrong quotes.
Update unit tests (`infer_dex_type_specific_labels_win_over_v2_fallback`) accordingly.

**2.2 Add DEX slugs to the per-DEX fallback ladder** so `discover --source remote`
pulls top pools for the new venues per chain (`fetch_network_dexes` already enumerates;
add a curated priority list per chain mirroring §2 volume ranks).

> **Status (2026-09-09) ✅** — implemented: `curated_dex_slugs()` in
> `core/src/pool/discovery/remote/mod.rs`, consumed as the head of the per-DEX ladder in
> `supplement_via_per_dex()` (network enumeration becomes the best-effort tail, de-duped).
> Slug inventory verified against `api.geckoterminal.com/api/v2/networks/{n}/dexes` the
> same day; stale slugs 404 and skip (best-effort, degrades gracefully).

### Phase 2.5 — Pool-discovery start-block validation (~0.5 day)

Silent-failure risk (same class as Phase 0): `pool_discovery_start_block` for every
non-Polygon chain was carried over without exercise. If a start block is **higher than
the first factory deployment** of a chain's main DEXes, those pools are never indexed.

- For each chain, confirm `pool_discovery_start_block` ≤ earliest deployment of each
  configured factory (`chains.toml` §1 rows).
- Export the deployment block per factory via `eth_getCode` + explorer, or run
  `discover --source onchain` with a sliding start-block probe and diff pool counts.
- Add a config guard: warn if `pool_discovery_start_block` is later than the factory's
  known deployment block (needs a small hardcoded map or first-observed-block cache).

> **Status (2026-09-09) ✅** — implemented via the **first-observed-block cache** (no schema
> change): `SqliteStore::earliest_creation_block_by_factory()` derives
> `MIN(creation_block) GROUP BY factory` from `pool_info`; `cli/src/commands/discover.rs`
> warns when the configured start block is later than a factory's earliest observed pool
> creation. A per-factory "blocks behind" backfill (compile-time nudge to re-run discovery)
> is left as follow-up; the guard itself is live.

### Phase 3 — New decoders (ordered by volume impact)

Compiled touchpoint checklist for a new `DexType` (anchors verified against the codebase
on 2026-09-08; line numbers are valid as of that date):

1. `core/src/dex_type.rs` — new enum variant + serde/strum rename attributes.
   `Display`/`FromStr` come from the strum derives; there are **no** manual
   `as_u8`/`from_u8`/`all()`/`chain_families()` helpers (the file is just the enum).
2. `core/src/pool/state/pool_types.rs` — new `PoolState` variant + state struct (`PoolState` at line 350).
3. `core/src/pool/state/factory.rs` — pool-init + metadata-repair arm (per-`DexType` matches at lines ~230-256 and ~498-507).
4. `core/src/pool/state/apply.rs` — reserve-update arm (swap/sync/burn/mint/flash; `update_from_logs()` dispatch at line 134).
5. `core/src/pool/discovery/<name>.rs` + `pool/discovery/mod.rs` — new module, and wire into the
   `discover_pools()` dispatch (`mod.rs:729`). Factory-creation topic constants also live at
   the top of `discovery/mod.rs` (lines 33-60).
6. `core/src/chain/events.rs` — swap-topic constant + decoder function.
7. `core/src/chain/trades.rs` — `trade_topics()` (line 20) + decode dispatch in `scan_trades()`;
   a missing entry means the trades scanner never sees that dex's swaps.
8. `core/src/pipeline/scanner.rs` — `topics::all_topics()` (line 87), the **activity-scanner**
   topic registry; a missing entry means blocks containing that dex's swaps are never flagged
   during replay. **Gap vs. the first checklist draft**: it referenced a non-existent
   `discovery/mod.rs` `swap_topics()` — the real registries are this item and #7.
9. `core/src/pool/math/<name>.rs` + **`pool/math/core.rs` `quote_exact_in()` (lines 53-123)**
   — **gap vs. the old plan**: the "follow the Camelot precedent" template is misleading,
   because Camelot is **not** in `quote_exact_in()` (nor Solidly). A new dex *must* get a
   match arm there or quoting silently returns `None` for those pools.
10. `core/src/types/gas.rs` — `DEX_SLOTS = 16` (line 14) is the headroom for the new
    discriminant, consumed by arithmetic bucketing in `bucket_index()` (line 18) — there is
    **no** per-`DexType` match to update; keep the discriminant < 16 or bump `DEX_SLOTS`.
11. Detectors that dispatch on `DexType` (per-hop gas calibration): only
    `mev/detectors/two_hop.rs` (~line 906) and `multi_hop.rs` (`dominant_dex_type()`,
    line 991). `sandwich.rs`/`jit.rs`/`jit_arb.rs`/`liquidation.rs` have no `DexType`
    dispatch today — no change needed there.
12. `core/src/explorer/decode.rs` — `decode_swap()` (line 64) new topic arm (consumed via
    `decode_tx_logs()` in `explorer/classify.rs:484`). The first draft named a
    non-existent `decode_receipt_facts()`.
13. `core/src/explorer/types.rs` — **`Amm` enum (line 140)** — **gap vs. the old plan**:
    decide whether the new dex reuses an existing `Amm` family (e.g. Pharaoh DLMM →
    `Amm::Lb`, like Camelot → `Amm::Solidly`) or needs a new variant for classification.
14. `core/src/pool/discovery/remote/geckoterminal.rs` + `dexscreener.rs` — `infer_dex_type()` mappings (see Phase 2.1).
15. Config: new `ChainConfig` field in `config/defaults.rs` if the dex reads a bespoke
    factory; `pick_factories()` itself lives in `pool/discovery/mod.rs:356` (not in config/)
    **and the `ChainName::default_*_factories()` fallbacks in `core/src/types/chain.rs`
    (lines 151-219) — gap vs. the old plan**: zero-config fallback lists duplicate
    `chains.toml` and must be kept in sync, or a config that omits factories silently
    loses the new dex.

Add `DexType` variant → discovery module → decoder → math → state application →
swap-topic registration (#7 trades scanner + #8 activity scanner) → GeckoTerminal
mapping. Use the Camelot decoder as a **contracts/events** template (it is the smallest
custom decoder) — but note Camelot has **no math entry in `quote_exact_in()`** (its math
is only a gas-calibration slot), so the math wiring in step 9 is still required for new
dexes.

| # | Item | Chains / Volume case | Notes | Effort |
|---|---|---|---|---|
| 3.5 | **Pharaoh DLMM** | Avalanche #1 (~$390M+/30d, now ~$147M/24h) | Bin-based (Liquidity Book family). ✅ **Status (2026-09-09):** confirmed LB v2.1-compatible (docs.phar.gg `DLMMFactory 0xEb480050b016f6c6d45203D2346B68bDDDa23D4D`; Dune reuses the LB macro). Factory wired **config-only** into `[avalanche] trader_joe_factories`. Q6 (indexed `LBPairCreated` layout) still deferred on-chain | 1-2 wks → done config-only |
| 3.3 | **PancakeSwap Infinity** | BSC (chain #2 venue ~$341M/24h), Base | **Implemented (2026-09-09, uncommitted):** custom `DexType::PancakeInfinity` (slot 10) + `discovery/infinity.rs` — Initialize-topic scan over the BSC `CLPoolManager 0xa0FfB9c1CE1Fe56963B0321B32E7A0302114058b` (same bytes32-PoolId / singleton-manager pattern as V4; synthetic pool key = `poolId[12..32]`) + `decode_infinity_cl_swap` (bespoke `Swap(bytes32,address,address,int128,int128,uint160,int24,uint16)`; first two data words are the signed net deltas, rest ignored) + **math is reuse, not bespoke** — `PancakeInfinityPoolState = UniswapV3PoolState`, `quote_v3_exact_in` (corrects the earlier "no math reuse" verdict) + init/refetch state via `eth_call getSlot0(bytes32)/getLiquidity(bytes32)` on the manager (no contract at the pseudo-address). `infinity_cl_pool_manager` config field wired toml-only (mirrors `v4_pool_manager`; no `chain.rs` mirror, pin test in `defaults.rs`). **Remaining:** topic digests are still assumption-only — on-chain verification deferred (no reliable RPC, same class as Q6/Q10); Base Infinity manager address unknown → BSC-only wiring | done (BSC) |
| 3.2 | **Fluid** | Ethereum #4, Arbitrum, Base (~$2.5B/mo on ETH alone) | **Verdict (2026-09-09):** true AMM (on-chain reserves) but reserves live in the unified **Liquidity layer**, not per-pool slots — **not log-only trackable**; needs `eth_call` (resolver + `centerPrice`). Hard-swap enforced at ±5% around `centerPrice`. ⇒ new decoder + state-read support; do **not** start without RPC budget for the resolver reads. Note: also on BSC via Lista DAO | 2-4 wks |
| 3.4 | **Metric** | ETH/ARB/POL/BSC top-5 | **Verdict (2026-09-09):** **true AMM, not an aggregator** (DefiLlama sub-category AMM). Single V2 factory `0xe22F9fc0f04486dE25ed6CF1800a4a47aFD82e0C` on all 10 chains (live ~Feb 2026, ~$275M/24h). **Custom events** (not canonical): `PoolCreated(address indexed token0, address indexed token1, address indexed priceProvider, address pool, bytes32 poolId)` and tick/bin-based `Swap(address sender, address recipient, bool exactInput, int128 amount0Delta, int128 amount1Delta, int16 newTick, uint104 newPositionInBin)`. **Oracle anchor (not log-only):** each pool reads a mid-price from an `IPriceProvider` oracle (`MetricOmmPool`) into storesQ64.64 bins — bin bounds derive from the oracle mid-price, so price state needs per-pool `eth_call` reads, not just swap logs. ⇒ **new decoder** (custom `DexType`; tick/bin math family); single factory per chain keeps config/distribution trivial | 1-2 wks |
| 3.6 | **1inch Aqua** | Ethereum #1 (~$312M/24h), possibly others | **Verdict (2026-09-09):** intent-family but **no pooled custody** — makers' own wallets with ERC-20 allowances, virtual-balance accounting in the Aqua Router registry `0x1111113ccf1426a8e30e2bff5e005d929bf6a90a` (AquaSwapVMRouter v1.0.2 `0x111111338c5091e8440b67b168bae16a668ac0de` executes SwapVM strategies: XYC, Decay, PeggedSwap). Pricing is **deterministic** (no off-chain solver) ⇒ MEV-relevant, but tracking requires an entirely different state model (registry virtual balances, not pools). New decoder + state model | spike first → 1-2 wks |
| 3.7 | **Lista DEX** | Ethereum (~$45M/24h), BSC | **Verdict (2026-09-09):** true AMM, three engines: SmartSwap (Curve-like stableswap), ListaV3 = UniV3 fork (`0xcb010ed373523942706F730b89792aA1C1597b20` BSC), ListaV2 = UniV2 fork (`0x28F5E6C71C7541b1C6523351AE331CcAfC443626` BSC). Ethereum StableSwap factory `0xF6c9ffA64bD0aE8a068dd7b7d954c654A3E7F8a6` (pools: ETH/wstETH `0x23072d031d5Af614395C8E58B7f7e91F003b331a`, USDT/USDC `0x35c9a4DaE1ff05788f24B5B32721D89340CBB636`). 🟡 BSC V3/V2 factories added **config-only**; stableswap pool-creation event unverified → not wired. Topic compat deferred (Q9) | spike first → config-only done |

Deliberately descoped: 1inch **aggregator routing** (distinct from 1inch Aqua, which
holds reserves — Aqua stays candidate pending Q4), GMX/perp venues, FermiSwap/Ekubo/
Native/DODO/Hashflow (small, idiosyncratic, or RFQ; revisit if volume share grows).

> **Descope guardrails**
> - **Metric** — verified a **true AMM** (not an aggregator); custom tick/bin events ⇒
>   new decoder. Terminal check flipped from "aggregator?" to "decode-first".
> - **1inch (aggregator)** ≠ 1inch Aqua (holds reserves). Only Aqua is a candidate.
> - **Pharaoh DLMM** before Pharaoh V3: volume ratio ~2:1 in favor of DLMM.
> - **Fluid** requires a feasibility study on log-only state reconstruction before
>   any decoder work (Phase 3.2 is contingent on that study's outcome).
> - **Non-AMM volume classes** — DefiLlama counts these under DEX volume, but they
>   have no trackable AMM pool state and must always be descoped, never decoded:
>   prediction markets (Polymarket, Predict.fun, OPINION, Overtime, Azuro,
>   MetaMask/Gate Predictions), card/spend products (Nexo/Kolo/Karta/Exa, EtherFi
>   Cash), memepads/launchpads (four.meme, Virtuals-style bonding curves), trading
>   bots (GMGN), and RFQ order flow (Hashflow). Check this class before ranking any
>   chain's "top DEX" — it inflates several chains in this plan (Polymarket is the
>   #1 Polygon entry purely through this lens).

### Phase 4 — Merger watch: Aerodrome + Velodrome → Aero

- Track the Aero migration (Optimism + Base; VELO/AERO → single AERO).
- When Aero contracts deploy: update `[base]`/`[optimism]` factories; keep old
  Velodrome V2 solidly factory for historical backtests (log replay needs it).
- SuperchainSwap (OP-stack interop) may create cross-rollup swap venues — watch,
  out of scope until measurable.
- **Factory re-pointing:** Aero may change factory addresses or pool code. Validate
  with the Phase 1.4 proxy check (pool address in topic may differ from implementation).
- **Impact on Phase 1.1/1.1b:** if Aero re-points Slipstream/V3 factory addresses on
  Optimism or Base, the config entries from Phase 1.1/1.1b must be updated to match.
  Validate factory addresses after Aero deployment before treating Phase 1.1/1.1b as
  permanently solved.

> **Status (2026-09-09):** watch-list only — no code. No Aero deployment change observed
> yet; re-check the factory set when the merger is live, and keep old Velodrome V2 and
> Aerodrome V1 solidly factories in config for historical log replay.

### Phase 5 — Regression & validation harness

- Extend `core/tests/` + `cli/tests/` config tests asserting the new factories parse
  (pattern exists: `all_default_factories_parse_as_addresses`,
  `phase_d_factories_present` in `core/src/types/chain.rs`).
- Add `validate-pools` runs per chain to CI-adjacent tooling: recall per DEX vs
  GeckoTerminal, with per-DEX target ≥80% for top-5 venues.
- Volume sanity check: `scan --kind trades` per-chain weekly totals within 2× of
  DefiLlama chain volume for covered DEXes (catches silent decoder breakage).
- CI command: `cargo test --test config_validation` (or equivalent) should pass after
  every Phase 1/1.5/1.9 config change; add to CI pipeline if not already present.
- **Add new-factory pin tests** for every Phase 1.6–3.7 config addition (pattern:
  `coverage_plan_v3_factories_present`, `coverage_plan_lb_and_curve_factories_present`,
  `all_default_factories_parse_as_addresses` in `core/src/types/chain.rs`).

> **Status (2026-09-09):** item 1 partially landed — the pin-test pattern now covers the
> Phase 1.6/1.7/1.8/3.5/3.7 config additions, and `all_default_factories_parse_as_addresses`
> walks every default list (incl. `curve_factories` plus the new Lista BSC entries).
> Remaining: per-DEX recall harness vs GeckoTerminal (≥80% top-5), weekly
> trades-to-DefiLlama volume sanity check, and a CI entry for the config tests.

---

## 5. Open questions (blockers on phases 1–3)

Resolved before committing the associated phase:

| # | Question | Gates | How to verify |
|---|---|---|---|
| Q1 | Does **Aerodrome Slipstream** (Base) emit the Algebra `Pool(address,address,address)` topic, or a bespoke event? If bespoke, Phase 1.1b needs a decoder shim, not just config | Phase 1.1b | `getLogs` on factory for existing pools; compare topic sig against §1 V3 list |
| Q2 | Does **PancakeSwap V3** (Base) use the canonical `PoolCreated(address,address,uint24,int24,address)` topic and emit the real pool contract address? (Pancake V3 pools are standard V3-style contracts — masterchef is farming, unrelated to pool creation; the check is topic + factory only) | Phase 1.2 | `getLogs` on the factory; compare topic sig with §1; sanity-check one pool's `slot0` via `eth_call` |
| Q3 | Is **Metric V2** a true AMM (trackable pool state) or an **aggregator/router**? Aggregator → descope Phase 3.4 | Phase 3.4 | **Resolved: true AMM** (DefiLlama AMM sub-category; V2 factory `0xe22F9fc0...` live on all 10 chains). Custom tick/bin events (`Swap(... int128 amount0Delta, int128 amount1Delta, int16 newTick, uint104 newPositionInBin)`) → **new decoder**, not descope |
| Q4 | Is **1inch Aqua** MEV-relevant despite intent-based pricing? Solver-set pricing may not be arbitrageable like AMM quotes | Phase 3.6 | **Resolved (feasibility):** pricing is **deterministic** — strategies are SwapVM opcodes (XYC/Decay/PeggedSwap) with no off-chain solver, so quotes are arbitrageable in principle. But Aqua has **no pooled custody** (maker-wallet allowances + virtual-balance registry) ⇒ new state model needed, not a pool decoder |
| Q5 | Can **Fluid** per-swap state be reconstructed from logs alone, or does it need subgraph data? | Phase 3.2 | **Resolved: no.** Fluid is a true AMM but reserves live in the unified **Liquidity layer**, not per-pool storage slots ⇒ state needs `eth_call` (resolver + `centerPrice`); hard-swap is enforced at ±5% around `centerPrice`. Log-only descope; keep as new decoder + state reads (not zero effort) |
| Q6 | Is **Pharaoh DLMM**'s bin event signature compatible with `math/lb.rs`, or does it need its own decoder (different event fields)? | Phase 3.5 | **Deferred — no reliable RPC.** Config-only wiring landed (factory → `trader_joe_factories`); verify `LBPairCreated` indexed layout on-chain before treating as permanent (see Status note, Phase 1/3.5) |
| Q7 | What is the **Aero** factory address set at launch, and do old Velodrome V2/Aerodrome V1 factories keep emitting swaps? | Phase 4 | follow Aero docs while migration is live |
| Q8 | Are per-chain **`aave_v3_pool`** addresses correct (esp. BSC) or does the same address string hide per-chain proxy differences? | Phase 0 | `eth_getCode` per chain + Aave deployment table |
| Q9 | Is **Lista DEX** a true AMM (trackable pool state) or a wrapper/aggregator? New entrant with fast growth ($45M/24h ETH, $38M/24h BSC) — verify before committing decoder effort | Phase 3.7 | **Resolved: true AMM** — three engines: SmartSwap (Curve-like stableswap), ListaV3 = UniV3 fork (`0xcb010ed3...` BSC), ListaV2 = UniV2 fork (`0x28F5E6C7...` BSC). BSC V3/V2 wired config-only; stableswap pool-creation event unverified → not wired |
| Q10 | Do the **ListaV3/V2 fork factories** emit the canonical `PoolCreated`/`PairCreated` topics (so config-only scanning works), or bespoke ones that need a shim? | Phase 3.7 | **Deferred** — no reliable RPC for `getLogs`. Config-only entries assume canonical-fork events (degrades silently, harmless if wrong); verify with `getLogs` on the BSC factories when an RPC is available (same constraint as Q6) |
| Q11 | Are the **Pancake Infinity CL** `Initialize`/`Swap` topic digests and the `Swap` data-word layout decoded from the `pancakeswap/infinity-core` source correct on-chain? (`PoolId = keccak256(poolKey, 0xc0)`; `getSlot0/getLiquidity` on the manager return the pool's state) | Phase 3.3 | **Deferred** — no reliable BSC RPC for `getLogs` on the live `CLPoolManager 0xa0FfB9c1...` / `Vault 0x238a3588...`. Discovery/decoder are unit-tested against hand-built logs; flip the assumption-only digest tests to pinned digests when one verified log is available (same deferral class as Q6/Q10) |

---

## 6. Priority order (volume-driven)

1. **Phase 0** — wrong addresses cause silent zeros on strategies that already exist.
2. **Phase 1.1–1.4 + 1.1b + 1.2b** — config lines unlock Velodrome V3, Aerodrome
   Slipstream, Pancake V3 Base **and Ethereum**, RamsesX, Pharaoh V3: combined
   multi-$B/mo of currently invisible volume. (Q1/Q2 gate 1.1b/1.2.)
3. **Phase 2.1** — stops active misclassification poisoning remote discovery.
4. **Phase 2.5** — fixes never-exercised start blocks per non-Polygon chain.
5. **Phase 1.5 + 3.5** — Avalanche and Arbitrum LFJ + Pharaoh DLMM (top venues by
   volume); bundle **1.8 (Blackhole) and 1.9 (QuickSwap V4 Base)** into the same
   traversal since they share the same chains.
6. **Phase 1.6 + 1.7** — Pangolin V3 (Avalanche) and Curve direct factories
   (low effort, mostly config).
7. **Phase 3.3 → 3.4 → 3.2 → 3.7** — Infinity ✅ **done** (uncommitted, BSC; topic
   verification still deferred), Metric (true AMM + oracle-anchored bins — new decoder
   with per-pool `IPriceProvider` reads), Fluid* (new decoder + `eth_call` state reads),
   Lista DEX (V3/V2 config-only landed). *Fluid only proceeds with an RPC budget for
   resolver reads (Q5 verdict: not log-only).
8. **Phase 3.6** — 1inch Aqua, contingent on Q4 pilot results; verdict keeps it viable
   (deterministic SwapVM pricing) but it needs a registry-based state model, not pools.
9. **Phase 4/5** — ongoing.
