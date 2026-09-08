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

`core/src/dex_type.rs` supports 9 DEX engine types:

| DexType | Discovery | Decoder | Math |
|---|---|---|---|
| UniswapV2 | ✅ `discovery/v2.rs` | ✅ | ✅ `math/core.rs` |
| UniswapV3 (incl. **Algebra forks** via `Pool(address,address,address)` topic) | ✅ `discovery/v3.rs` | ✅ | ✅ `math/v3.rs` |
| UniswapV4 | ✅ `discovery/v4.rs` | ✅ | ✅ |
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
| 9 | PancakeSwap Infinity | <$10M **on Base** — the ~$341M/24h figure is **BSC-only** | ❌ | Phase 3.3 (driven by BSC, not Base) |

### BSC (~$43B 30d)
| # | DEX | Share | Support | Gap action |
|---|-----|-------|---------|------------|
| 1 | PancakeSwap V3 | ~$528M/24h ≈ 40% of chain — the "~85%" figure is stale | ✅ | — |
| 2 | PancakeSwap Infinity | ~$342M/24h — chain #2 venue, not "growing" | ❌ | Phase 3.3 |
| 3 | GMGN | ~$141M | ❌ | bot/aggregator-adjacent — apply the same Q3 classify-before-decode check as Metric |
| 4 | Uniswap V4 | ~$115M/24h | ✅ | — |
| 5 | PancakeSwap V2 | ~$114M/24h — still material, not just "declining" | ✅ | — |
| 6 | Uniswap V3 | ~$60M/24h | ✅ | — |
| 7 | Metric V2 | ~$56M/24h | ❌ | Phase 3.4 |
| 8 | Lista DEX | ~$38M/24h | ❌ | Phase 3.7 |
| 9 | Flap sh / Topaz CL | ~$31–35M each | ❌ | new venues — Phase 4 (low prio) |
| 10 | Native Swap | ~$16M/24h | ❌ | Phase 4 (low prio) |

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
| 7 | Curve / Solidly V3 / DODO | minor | 🟡/❌ | Phase 0 note |

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
| 1.5 | LFJ: promote `trader_joe_factory: Option<String>` → `trader_joe_factories: Vec<String>` in `ChainConfig`, then add V2.2 Liquidity Book factory to Avalanche alongside V2.1 (`0xb43120...`). V2.1 and V2.2 are **both live** — removing V2.1 loses historical backtest coverage; keeping both requires the list field. Update all consumers (`pool/discovery/trader_joe.rs`, `config/validation.rs`) | `core/data/chains.toml`, `core/src/config/defaults.rs` + consumers | small |
| 1.6 | *(optional)* Pangolin **V3** factory → `[avalanche] uniswap_v3_factories` — live data shows Pangolin V3 active ($1.3M/24h); the Pangolin **V2** factory referenced in the `chain.rs` fallback list is effectively dead | `core/data/chains.toml` | trivial |
| 1.7 | *(optional)* Curve **direct factories** per non-Ethereum chain (see Phase 0) → new config field `curve_factories` + discovery support | `chains.toml`, `discovery/curve.rs` | small |
| 1.8 | *(optional)* **Blackhole CLMM** (Avalanche, chain #4 at ~$5.0M/24h — bigger than Uniswap V3 there) — verify whether its CL pools are Algebra-family; if yes, factory → `[avalanche] uniswap_v3_factories` is config-only. Its Solidly-style "AMM" leg is negligible ($21K) | `core/data/chains.toml` | trivial + verification |
| 1.9 | *(optional, needs schema change)* **QuickSwap V4 on Base** ($2.4M/24h) — V4-family, but `v4_pool_manager` is a single `Option<String>` per chain; supporting both Uniswap V4 and QuickSwap V4 on one chain requires promoting it to a list (same pattern as Phase 1.5) | `core/data/chains.toml`, `core/src/config/defaults.rs` | small |

**Acceptance:** `mev-scout -f <cfg> discover --source onchain` per chain lists the new
factories; `validate-pools` recall for those DEXes goes from 0 to ≥ target.

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
| `pancakeswap-infinity` | UniswapV2 ❌ | new type (Phase 3.3) |
| `fluid` / `metric` / `dodo` / `woofi` | UniswapV2 ❌ | skip + warn until decoded |

Actions: extend the match list above the generic fallback; for not-yet-decoded
protocols return a `DexType` that **skips** the pool (or a `RemotePool.supported=false`
flag) instead of silently importing it as UniswapV2 — misclassified pools poison
pool state application (`pool/state/apply.rs`) and produce wrong quotes.
Update unit tests (`infer_dex_type_specific_labels_win_over_v2_fallback`) accordingly.

**2.2 Add DEX slugs to the per-DEX fallback ladder** so `discover --source remote`
pulls top pools for the new venues per chain (`fetch_network_dexes` already enumerates;
add a curated priority list per chain mirroring §2 volume ranks).

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

### Phase 3 — New decoders (ordered by volume impact)

Each item: add `DexType` variant → discovery module → swap decoder → math →
state application → `chain/trades.rs` scan topic → GeckoTerminal mapping. Follow the
Camelot precedent (smallest existing custom decoder) as the template.

| # | Item | Chains / Volume case | Notes | Effort |
|---|---|---|---|---|
| 3.5 | **Pharaoh DLMM** | Avalanche #1 (~$390M+/30d, now ~$147M/24h) | Bin-based (Liquidity Book family). `math/lb.rs` bin math reusable, but contracts/events differ from TraderJoeLB → new `DexType::PharaohDLMM`, own decoder. **Do this before Pharaoh V3 (Phase 1.4) is finalized** — DLMM carries ~2× the volume. Must land **before** Avalanche is treated as a first-class target | 1–2 wks |
| 3.3 | **PancakeSwap Infinity** | BSC (chain #2 venue ~$341M/24h), Base | Hook-based AMM, V4-adjacent. Investigate first whether swap events are V4-compatible — if yes, extend `discovery/v4.rs` rather than new decoder. Verify the Infinity factory/pool addresses on Base too | 1–2 wks |
| 3.2 | **Fluid** | Ethereum #4, Arbitrum, Base (~$2.5B/mo on ETH alone) | Lending-integrated, differential-liquidity architecture. **Feasibility study first**: can per-swap state be tracked from logs alone? If it needs off-chain subgraph data, descope. Note Fluid is also present on BSC via Lista DAO | 2–4 wks |
| 3.4 | **Metric** | ETH/ARB/POL/BSC top-5 | AMM internals undocumented in this repo — spike first. **Scoping check:** Metric V2 may be a **DEX aggregator**, not a plain AMM — if it routes through other venues, tracking its pool state is low-yield for MEV and it should be **descoped** rather than decoded blind | 1–2 wks |
| 3.6 | **1inch Aqua** | Ethereum #1 (~$312M/24h), possibly others | Intent-based AMM. **Not the same as the 1inch aggregator** — Aqua holds real pool reserves. Evaluate MEV-relevance (solver-style, quotes may be non-uniform vs AMM pricing) before committing decoder effort | spike first |
| 3.7 | **Lista DEX** | Ethereum (~$45M/24h), BSC | New entrant, growing top-5. Spike before committing | spike first |

Deliberately descoped: 1inch **aggregator routing** (distinct from 1inch Aqua, which
holds reserves — Aqua stays candidate pending Q4), GMX/perp venues, FermiSwap/Ekubo/
Native/DODO/Hashflow (small, idiosyncratic, or RFQ; revisit if volume share grows).

> **Descope guardrails**
> - **Metric** is likely an aggregator — verify before building; do not decode aggregates.
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

### Phase 5 — Regression & validation harness

- Extend `core/tests/` + `cli/tests/` config tests asserting the new factories parse
  (pattern exists: `all_default_factories_parse_as_addresses`,
  `phase_d_factories_present` in `core/src/types/chain.rs`).
- Add `validate-pools` runs per chain to CI-adjacent tooling: recall per DEX vs
  GeckoTerminal, with per-DEX target ≥80% for top-5 venues.
- Volume sanity check: `scan --kind trades` per-chain weekly totals within 2× of
  DefiLlama chain volume for covered DEXes (catches silent decoder breakage).

---

## 5. Open questions (blockers on phases 1–3)

Resolved before committing the associated phase:

| # | Question | Gates | How to verify |
|---|---|---|---|
| Q1 | Does **Aerodrome Slipstream** (Base) emit the Algebra `Pool(address,address,address)` topic, or a bespoke event? If bespoke, Phase 1.1b needs a decoder shim, not just config | Phase 1.1b | `getLogs` on factory for existing pools; compare topic sig against §1 V3 list |
| Q2 | Does **PancakeSwap V3** (Base) use the canonical `PoolCreated(address,address,uint24,int24,address)` topic and emit the real pool contract address? (Pancake V3 pools are standard V3-style contracts — masterchef is farming, unrelated to pool creation; the check is topic + factory only) | Phase 1.2 | `getLogs` on the factory; compare topic sig with §1; sanity-check one pool's `slot0` via `eth_call` |
| Q3 | Is **Metric V2** a true AMM (trackable pool state) or an **aggregator/router**? Aggregator → descope Phase 3.4 | Phase 3.4 | inspect Metric contracts + subgraph; check whether pool reserves are on-chain |
| Q4 | Is **1inch Aqua** MEV-relevant despite intent-based pricing? Solver-set pricing may not be arbitrageable like AMM quotes | Phase 3.6 | run a pilot `scan --kind arbitrage` on Ethereum Aqua pools |
| Q5 | Can **Fluid** per-swap state be reconstructed from logs alone, or does it need subgraph data? | Phase 3.2 | feasibility study (contracts + log replay over a sample block range) |
| Q6 | Is **Pharaoh DLMM**'s bin event signature compatible with `math/lb.rs`, or does it need its own decoder (different event fields)? | Phase 3.5 | compare TraderJoeLB vs PharaohSwap event ABI |
| Q7 | What is the **Aero** factory address set at launch, and do old Velodrome V2/Aerodrome V1 factories keep emitting swaps? | Phase 4 | follow Aero docs while migration is live |
| Q8 | Are per-chain **`aave_v3_pool`** addresses correct (esp. BSC) or does the same address string hide per-chain proxy differences? | Phase 0 | `eth_getCode` per chain + Aave deployment table |

---

## 6. Priority order (volume-driven)

1. **Phase 0** — wrong addresses cause silent zeros on strategies that already exist.
2. **Phase 1.1–1.4 + 1.1b** — config lines unlock Velodrome V3, Aerodrome Slipstream,
   Pancake V3 Base, RamsesX, Pharaoh V3: combined multi-$B/mo of currently invisible
   volume. (Q1/Q2 gate 1.1b/1.2.)
3. **Phase 2.1** — stops active misclassification poisoning remote discovery.
4. **Phase 2.5** — fixes never-exercised start blocks per non-Polygon chain.
5. **Phase 1.5 + 3.5** — Avalanche is unusable as a target until Pharaoh DLMM +
   LFJ V2.2 are covered (they are the top venues by volume).
6. **Phase 3.3 → 3.2 → 3.4** — Infinity, Fluid*, Metric (descope Metric if Q3 = aggregator).
   *Fluid only proceeds if Q5 feasibility study passes.
7. **Phase 3.6** — 1inch Aqua, contingent on Q4 pilot results.
8. **Phase 4/5** — ongoing.
