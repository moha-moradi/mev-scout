# Pool Discovery Redesign — Audit & Implementation Plan

**Status:** Analysis/planning only. No source files were modified and nothing was committed. Implementation of Phase 2 and later is **pending approval**.
**Repository state audited:** `a18f3e5` (`remove docs`).
**Verification date:** 2026-11-27. Important claims are cited to source files/tests or to recorded live probes. Unverified external sources are explicitly labelled.

---

## خلاصه (فارسی)

این سند نتیجهٔ بررسی کامل ماژول `core/src/pool/discovery/`، مسیر `discover` → SQLite → `PoolManager`/Live و منابع خارجی است. هدف، ساخت مجموعهٔ بزرگی از «موجودی استخرهای فعال با TVL» برای تمام زنجیره‌های پشتیبانی‌شده است که همهٔ منابع را بدون وابستگی به یک منبع، با هویت canonical، سابقهٔ منبع (provenance) و جداسازی کامل «موجودی master» از «ثبت تجاری tradable» ترکیب کند. یافتهٔ اصلی: مقدار پیشفرض `min_tvl = 25,000` و `max_pools = 1000` استخرهای با TVL ناشناخته را حذف میکند؛ هویت Uniswap V4 (poolId) ذخیره نمیشود؛ منابع رسمی DEX اصلاً متصل نیستند؛ و Live از تمام ردیفهای SQLite قدیمی بارگذاری میکند نه از نتیجهٔ رتبهبندی شده. طرح شامل فاز ۰ تا ۶، فایلهای دقیق، تستها، معیارهای پذیرش و هزینهٔ RPC است. **لطفاً قبل از اجرای فاز ۲ یا بعدتر، تأیید دهید.**

---

## 0. Scope and verification method

### 0.1 What was inspected

- `core/src/pool/discovery/` — all 21 `.rs` files, including `remote/geckoterminal.rs` and `remote/dexscreener.rs`.
- `core/src/jobs/discover.rs`, `core/src/jobs/index.rs`, `core/src/jobs/live.rs`, `core/src/jobs/run.rs`.
- `core/src/pool/state/` (`manager.rs`, `factory.rs`, `pool_types.rs`, `quoting.rs`), `core/src/pipeline/runner.rs`.
- `core/src/cache/store/{pools,mod,blocks}.rs`, `core/src/fetch/fetcher.rs`.
- `cli/src/{cli,main,overrides}.rs`, `cli/src/commands/{discover,live}.rs`, `cli/src/commands/explorer/backfill.rs`.
- `core/examples/pool_discovery_bench.rs`, `core/data/chains.toml`, `core/src/types/chain.rs`, `core/src/config/settings.rs`, all root TOML profiles.
- `docs/POOL_DISCOVERY_RECEIPTS.md`, `docs/CLI.md`, `docs/CONFIG_PROFILES.md`, checked-in `bench/*.json`.
- Live HTTP probes against GeckoTerminal, DexScreener, Curve, DefiLlama and several official API/doc hosts.

### 0.2 What was **not** done

- No `cargo check`, `cargo test`, or benchmark run (they write build artifacts).
- No endpoint is claimed as an official source unless a live probe or documentation fetch succeeded. Where access was blocked, the source is marked **unverified** and deferred to Phase 1.

### 0.3 Supported chains

`core/src/types/chain.rs:45-73`:

| Chain | ID | Public RPC note |
|---|---:|---|
| Ethereum | 1 | `chain.rs:79-142` |
| BSC | 56 | |
| Polygon | 137 | |
| Avalanche | 43114 | only `publicnode` listed, 1.0 RPS (`:106-111`) |
| Arbitrum | 42161 | |
| Base | 8453 | |
| Optimism | 10 | |

---

## 1. Executive findings

| # | Finding | Evidence |
|---|---|---|
| 1 | Seven chains, **12 on-chain decoder families**, only **2 remote aggregators** (GeckoTerminal, DexScreener). No official DEX API or subgraph source is wired. | `discovery/{v2,v3,v4,balancer,curve,solidly,camelot,trader_joe,pendle,infinity,fluid,metric}.rs`; `remote/{geckoterminal,dexscreener}.rs` |
| 2 | Default `min_tvl = $25,000` and `max_pools = 1000` are applied as a hard drop of unknown-TVL pools plus a global truncation when any remote/enrich path is active. | `jobs/discover.rs:51-52,144-162,583-596`; `config/settings.rs:409-414`; test `jobs/discover.rs:904-954` |
| 3 | GeckoTerminal is capped at 50 pages × 20 pools ≈ 1,000 chain-wide and 25 × 20 = 500 per DEX; pagination stops on a *post-filtered* empty page. | `remote/geckoterminal.rs:65,80-98,132,143-164` |
| 4 | DexScreener has no chain-wide/paginated endpoint; 3 hub symbols per chain. Live probe: 30 results per hub on Avalanche, 90 unique total. | `remote/dexscreener.rs:18-30,48-99`; live probe |
| 5 | No provenance model: one `tvl_usd`, first-wins `merge_from`, no observation log, no source timestamps. | `discovery/mod.rs:136-198,295-361` |
| 6 | Identity is pool address only. Uniswap V4 stores a synthetic address (last 20 bytes of poolId) and **drops `pool_id`**; health then probes a non-contract address. `pool_info` is keyed by `address BLOB PRIMARY KEY`, so singleton identities cannot be represented. | `v4.rs:17-23,60-73`; `health.rs:36-41`; `cache/store/mod.rs:149-160` |
| 7 | Unknown remote DEX labels fall back to `UniswapV2` (`fee=0`), undermining the fail-closed `DexType::Unknown` guard in the execution engine. | `remote/mod.rs:127-133`; `pipeline/runner.rs:1894-1901` |
| 8 | A GeckoTerminal per-DEX fallback error discards already-successful chain-wide results (`Vec::new()`). | `remote/mod.rs:305-311` |
| 9 | There are **two different universes**: on-chain/receipt pools are persisted before ranking, so SQLite can contain pools that the current ranked outcome dropped, health removed, or an older profile left behind. Live loads all cached rows. | `discovery/mod.rs:1719-1763`; `jobs/discover.rs:285-302,583-668`; `jobs/live.rs:576-581`; `pipeline/runner.rs:952-1013` |
| 10 | `getLogs` scans mark the discovery watermark unconditionally even though `scan_onchain()` swallows non-cancellation errors and returns empty results. | `jobs/discover.rs:247-253,549-557` |
| 11 | No canonical-hash reconciliation for scanner cache or Live; explorer reorg unwind is separate. | `cache/store/blocks.rs:7-23`; `fetch/fetcher.rs:153-175`; `jobs/live.rs:1870-1872,1920-1922`; `explorer/ingest.rs:310-327,588-597` |
| 12 | Production multi-hop detection does **not** use `max_pairs_per_token`; it uses Bellman-Ford negative cycles over all pools with separate caps. | `mev/detectors/multi_hop.rs:25-45,70-74,99-161,738-772` |

---

## 2. Current architecture and actual data flow

### 2.1 End-to-end path

```
cli discover                       cli/src/commands/discover.rs:10-48
  └─ DiscoverOpts::from_config     core/src/jobs/discover.rs:69-96
  └─ job_discover                  core/src/jobs/discover.rs:305-684
       ├─ mode/window validation             :316-459,686-778
       ├─ resolve factories                  :496-513 → discovery/factories.rs:32-106
       ├─ on-chain getLogs scan              :549-558 → discovery/mod.rs:945-1067
       │   or receipts scan                  :517-548 → discovery/mod.rs:1835-1987
       ├─ remote fetch (hybrid/remote/enrich):563-571 → remote/mod.rs:495-514
       ├─ dedup + merge                      :573-581
       ├─ TVL ranking + global cap           :583-596   ← destructive to outcome
       ├─ Multicall metadata repair          :599-643
       ├─ health check                       :645-662 → discovery/health.rs:24-147
       └─ persist_universe                   :664-668 → cache/store/pools.rs:22-49

live / explorer
  ├─ bootstrap only if cache empty           cli/src/commands/live.rs:80-105
  ├─ load_pool_registry / list_discovered_pools jobs/index.rs:127-184
  ├─ BacktestRunner::init_pools              pipeline/runner.rs:952-1013
  ├─ add_pool_to_manager (Unknown fail-closed) pipeline/runner.rs:1791-1902
  └─ PoolManager::init_from_rpc              pool/state/factory.rs:229-311
       ├─ repair_pool_metadata               :242-247
       ├─ fetch state per pool               :249-296
       └─ remove_unhealthy_pools + reindex   :298-302,748-769
```

### 2.2 Stage details

| Stage | What actually happens |
|---|---|
| Source modes | `onchain \| remote \| hybrid \| receipts` (`jobs/discover.rs:316-320`). CLI exposes only block-range flags and `--incremental` (`cli/src/cli.rs:199-210`); all budgets and `receipts` source are TOML-only. |
| Activity topics | 22 topics assembled once (`discovery/mod.rs:1565-1587`): V2, V3, four Curve variants, Balancer, two LFJ LB, five Pendle, V4, Infinity, Fluid, Metric. |
| Factory scanners | 12 scanners via `join_all` (`mod.rs:1640-1654`), first-wins merge (`ScanBatchResult::merge:714-728`). |
| Receipts | Shares `decode_factory_creation_log` (`mod.rs:791-854`); failed windows returned and watermark not advanced (`jobs/discover.rs:536-548`). |
| Multi-provider shards | Only final `Vec<DiscoveredPool>` is merged; `PoolHit`/factory metadata from a shard is reconstructed from `pool_id`/`creation_block` and otherwise lost (`mod.rs:1037-1051`). |
| Persistence | `INSERT OR REPLACE INTO pool_info` keyed by address (`cache/store/pools.rs:22-49`); schema version 13 (`cache/store/mod.rs:72-73`). |
| Tradable graph | `max_pairs_per_token` default 50 (`pool/state/manager.rs:163`); per-token overrides `:244-254`; pairs sorted by liquidity estimate then truncated (`manager.rs:403-444`). `Metric`/`Fluid` liquidity estimate is hardcoded `0` (`manager.rs:398`), so they are pruned first. |
| Multi-hop | Bellman-Ford over all pools (`multi_hop.rs:738-772`), caps `MAX_NEGATIVE_CYCLES=16`, `MAX_CYCLE_ATTEMPTS=64`, depth 4 (`multi_hop.rs:25-40`). BFS is compatibility-only (`:163-167`). |
| Quote support | Exact-in math: V2, V3/V4/Infinity, Curve, Balancer, LB, Pendle; Metric/Fluid return `None` (`pool/math/core.rs:62-118`). Two-hop optimizer: LB/Pendle/Metric/Fluid are `Unsupported` (`pool/state/quoting.rs:41-53`). |

### 2.3 Confirmed code defects

| Defect | Location | Effect |
|---|---|---|
| `factory_pools.drain()` empties the map, then `underlying_tokens` is read from it | `mod.rs:1367` vs `1409-1411` | Metadata-derived Curve/Balancer pools always lose underlying token list. |
| GT pagination evaluates “empty page” after `min_tvl` filter | `geckoterminal.rs:76,78,84-93,268-277` | A page of sub-threshold pools terminates discovery early. |
| GT per-DEX fallback error returns empty vector | `remote/mod.rs:305-311` | One source error destroys successful chain-wide results. |
| V4 `Initialize` decoder never sets `pool_id` | `v4.rs:60-73` | Singleton pool identity unrecoverable; health probes synthetic address. |
| Metric `bytes32 poolId` discarded; Fluid `dexId` ignored; LFJ `binStep` ignored | `metric.rs:18-30`, `fluid.rs:20-40`, `trader_joe.rs:22-49` | Protocol identity/parameters lost. |
| No topic-length guards in several `classify_activity` helpers | e.g. `v2.rs:10`, `v3.rs:11`, `curve.rs:15` | Malformed logs can panic on `topics()[0]`. |
| Solidly/Camelot have no activity classifier | `v2.rs:9-15`, `mod.rs:687-698` | Activity-only pools mislabelled `UniswapV2`, fee 30. |
| `getLogs` watermark advanced on failed scans | `jobs/discover.rs:247-253,549-557` | Failed ranges can be permanently skipped. |
| `pool_states` table exists but no writer found | `cache/store/mod.rs:162-167` | Ambiguous half-schema. |

### 2.4 Two universes — important nuance

Ranking and health filtering change `DiscoverOutcome.pools` and the final persist pass, but **do not delete earlier cached rows**. Therefore SQLite holds the union of everything ever persisted:

- pools found by earlier on-chain/receipt scans that current ranking dropped for low/unknown TVL;
- pools removed by health checks;
- stale rows from other config profiles;
- remote rows with `creation_block = 0`.

Live does not re-apply ranking; it loads all of them (`jobs/live.rs:576-581`, `pipeline/runner.rs:952-1013`), then shrinks the in-memory manager via future-`creation_block` skip, metadata repair drops (`factory.rs:428-479`), unhealthy removal (`factory.rs:748-759`) and `Unknown` fail-closed skip (`runner.rs:1894-1901`).

---

## 3. Existing source and DEX coverage

### 3.1 On-chain decoder families

| Family | Chains configured | Factory/event source |
|---|---|---|
| UniswapV2-style | ETH, BSC, Polygon, Avalanche, Base, Optimism | `chain.rs:150-185`; `chains.toml:14-20,48-53,169-174,387-391,424` |
| V3 / Algebra / Slipstream | all 7 | `chain.rs:187-227`; `chains.toml:9-13,42-47,164-168,316-320,350-355,383-386,419-423` |
| Balancer V2 | all 7 (vault) | `chains.toml:3,38,162,312,347,377,416` |
| Curve | all 7 (+ Ethereum legacy registry) | `chain.rs:265-281`; `chains.toml:32,66,190,337,367,401-403,435` |
| Solidly | Base, Optimism | `chain.rs:229-241` |
| Camelot | Arbitrum only | `chain.rs:283-291` |
| LFJ / Trader Joe LB | AVAX, Arbitrum, BSC, ETH | `chain.rs:243-263`; `chains.toml:69-78,189,333-336,406` |
| Uniswap V4 | all 7 | `chains.toml:31,64,185,331,365,404,436` |
| Pancake Infinity CL | **BSC only** | `chains.toml:187` |
| Pendle | BSC, Arbitrum, Base, ETH, Optimism (no AVAX/Polygon) | `chains.toml:191,338,368,407,437` |
| Fluid | ETH, Arbitrum, Base | `chains.toml:341,371,410` |
| Metric V2 | all 7 | `chains.toml:34,80,194,343,373,412,440` |

### 3.2 Remote sources

| Source | Endpoints used | Chains | Limits / notes |
|---|---|---|---|
| GeckoTerminal | `/networks/{n}/pools`, `/dexes`, `/dexes/{d}/pools` | all 7 via `network_slug` (`geckoterminal.rs:23-33`) | 20 rows/page; 50-page chain cap; 25-page per-DEX cap; 200 ms inter-page sleep; 15 s timeout; 3 retries with 600/1200 ms backoff (`remote/mod.rs:41-101`). |
| DexScreener | `/latest/dex/search?q={hub}` only | all 7, 3 hub symbols each | No pagination; 250 ms sleep; unknown TVL dropped (`dexscreener.rs:172-180`). |
| DefiLlama yields | **not wired** | — | Explicitly rejected (`remote/mod.rs:15-21`); live probe confirms 0/250 Avalanche rows have a `0x...` pool id. |
| Official DEX APIs / subgraphs | **none implemented** | — | `--source subgraph` flag removed (`cli/tests/cli_data_foundation.rs:79-91`). |
| Unsupported remote labels | `dodo`, `woofi`, `hashflow`, `maverick`, `ekubo` skipped; all others → `UniswapV2` | — | `remote/mod.rs:127-133,232-238` |

### 3.3 Per-chain / per-DEX source coverage matrix

✓ configured on-chain · ~ activity-topic only · — not configured · GT = GeckoTerminal · DS = DexScreener hub search · **Native** = official DEX-native reference (none wired; status in §4).

| Chain | DEX / family | Factory | Activity | GT chain | GT per-DEX (curated) | DS hubs | Native reference |
|---|---|:---:|:---:|:---:|---|---|---|
| Ethereum | Uniswap V2 / Sushi / ShibaSwap | ✓ | ✓ | ✓ | `uniswap_v2` | WETH/USDC/USDT | unverified |
| Ethereum | Uniswap V3 | ✓ | ✓ | ✓ | `uniswap_v3` | ✓ | unverified |
| Ethereum | Uniswap V4 | ✓ | ✓ | ✓ | `uniswap-v4-ethereum` | ✓ | unverified |
| Ethereum | Curve (registry + factories) | ✓ | ✓ | ✓ | `curve` | ✓ | **verified API** |
| Ethereum | Balancer V2 | ✓ | ✓ | ✓ | `balancer_ethereum` | ✓ | unverified |
| Ethereum | Pendle / Fluid / Metric / LFJ / Pancake V3 | ✓ | ✓ | ✓ | partial | ✓ | unverified |
| BSC | PancakeSwap V2 / V3 | ✓ | ✓ | ✓ | `pancakeswap_v2`, `pancakeswap-v3-bsc` | WBNB/USDC/USDT | unverified |
| BSC | Pancake Infinity CL | ✓ | ✓ | ✓ | — (not curated) | ✓ | unverified |
| BSC | Uni V3 / ListaV2/V3 / BiSwap / Sushi | ✓ | ✓ | ✓ | partial | ✓ | unverified |
| BSC | Curve / Pendle / LFJ V2.1 / Metric | ✓ | ✓ | ✓ | partial | ✓ | Curve API no rows |
| Polygon | QuickSwap V2 / V3 | ✓ | ✓ | ✓ | `quickswap`, `quickswap_v3` | WMATIC/USDC/USDT | unverified |
| Polygon | RamsesX / Uni V3 / Uni V4 | ✓ | ✓ | ✓ | ✓ | ✓ | unverified |
| Polygon | ApeSwap / DFYN / Meshswap / Sushi | ✓ | ✓ | ✓ | tail via DEX enumeration | ✓ | unverified |
| Polygon | Curve / Balancer / Metric | ✓ | ✓ | ✓ | `curve_polygon_pos` | ✓ | **verified API** |
| Avalanche | Pangolin V2/V3, LFJ V1, Blackhole V2/CLMM | ✓ | ✓ | ✓ | ✓ curated | WAVAX/USDC/USDT | unverified |
| Avalanche | Pharaoh V3 / DLMM | ✓ | ✓ | ✓ | ✓ curated | ✓ | unverified |
| Avalanche | Uni V3 / V4, LFJ V2.1/V2.2, Curve, Balancer, Metric | ✓ | ✓ | ✓ | ✓ | ✓ | Curve API 5/2 |
| Avalanche | Pendle | — | ~ | ✓ | — | ✓ | gap |
| Arbitrum | Uni V3 / V4, Pancake V3, Ramses | ✓ | ✓ | ✓ | ✓ curated | WETH/USDC/USDT | unverified |
| Arbitrum | Camelot (V2 + V3) | ✓ (only chain) | ~ V2 mislabelled | ✓ | `camelot-v3` | ✓ | unverified |
| Arbitrum | LFJ V2.1/V2.2, Curve, Balancer, Pendle, Fluid, Metric | ✓ | ✓ | ✓ | ✓ | ✓ | Curve API 5/2 |
| Base | Uni V3 / V2 / V4, Pancake V3 | ✓ | ✓ | ✓ | ✓ curated | WETH/USDC/USDT | unverified |
| Base | Aerodrome V2 + Slipstream CL | ✓ | ~ V2 mislabelled | ✓ | `aerodrome-base`, `aerodrome-slipstream` | ✓ | unverified |
| Base | Curve, Balancer, Pendle, Fluid, Metric | ✓ | ✓ | ✓ | ✓ | ✓ | Curve API no rows |
| Base | Pancake Infinity CL | **—** | ~ | ✓ | `pancakeswap-infinity-clmm-base` | ✓ | on-chain config gap |
| Optimism | Uni V3 / V4, Velodrome V2 + Slipstream | ✓ | ~ V2 mislabelled | ✓ | ✓ curated | WETH/USDC/USDT | unverified |
| Optimism | Curve, Balancer, Pendle, Metric, Sushi V2 | ✓ | ✓ | ✓ | partial | ✓ | Curve API 4/− |

---

## 4. DEX-native and external reference sources

### 4.1 Verified live probes (2026-11-27)

| Source / URL pattern | Observed result | Implication |
|---|---|---|
| `GET api.geckoterminal.com/api/v2/networks/{slug}/pools?page=N` | 20 rows/page; top-level JSON contained only `data` (no `meta`/`links`); `eth`, `avax`, `base` valid, `avalanche` → 404 (code correctly maps to `avax`); **HTTP 429 after ~7 rapid requests** | Complete pagination must be driven by “empty page”; no server-side total page count; shared rate limiter required. |
| `GET .../networks/avax/dexes?page=1` → 56 dexes; `page=2` → 1 | Code enumerates only pages 1–3 and breaks when a page has <20 (`geckoterminal.rs:189,204-209`) | DEX enumeration incomplete; curated slug list is de facto universe. |
| `GET .../networks/base/dexes/aerodrome-base/pools?page=1` | 20 rows, includes `pool_created_at` | Per-DEX endpoint works and supplies creation time; usable for incremental reference. |
| `GET api.dexscreener.com/latest/dex/search?q={WAVAX\|USDC\|USDT}` | 30 results per query, 90 unique pairs on Avalanche | ≤90 pools/chain from this adapter in practice; long tail structurally missed. |
| `GET api.dexscreener.com/token-pairs/v1/avalanche/0xB31f…66c7` | 30 pairs; docs do not document pagination for this endpoint | Useful for per-token validation/enrichment, not exhaustive enumeration. DexScreener API terms apply (`docs.dexscreener.com/api/api-terms-and-conditions.md`). |
| `GET api.curve.finance/api/getPools/{chain}/{registry}` | ETH main 49 / crypto 8; AVAX 5/2; Polygon 2/4; Arbitrum 5/2; Optimism 4/−; BSC, Base: no rows | Official Curve reference usable but small; label per registry, not “all Curve pools”. |
| `GET yields.llama.fi/pools` | 16,037 rows; Avalanche 250; **0 of 250** with `0x…` pool id | Not a valid identity source; possible TVL/volume enrichment only via explicit `(chain, project, symbol)` key. |

### 4.2 Candidates requiring Phase 1 verification (not claimed as working)

| Protocol category | Source class to verify | Status from this environment |
|---|---|---|
| Uniswap V2/V3/V4 | Official Uniswap API / official subgraphs | `api.uniswap.org/v2/...` → 409 `ACCESS_DENIED`; `docs.uniswap.org` → 403. **Unverified.** |
| Balancer | Official Balancer GraphQL/API + subgraphs | `api-v3.balancer.fi/graphql` → 403/empty; docs 404. **Unverified.** |
| PancakeSwap V2/V3/Infinity | Official pool-list API / subgraph / website data source | `docs.pancakeswap.finance` → 403/redirect; `api.pancakeswap.info` → 530. **Unverified.** |
| SushiSwap, QuickSwap, Camelot, Aerodrome, Velodrome, LFJ, Pharaoh, Blackhole, Pangolin, Ramses | Official subgraph / protocol indexer / website API | The Graph hosted-service paths returned 301; gateway needs API key. **Unverified.** |
| DefiLlama | `yields.llama.fi` (verified) as TVL/volume enrichment only | Verified; identity unusable. |

**Phase 1 rule:** an endpoint enters the reference matrix only after recording chain coverage, response shape, page size, hard limits, freshness field and canonical identity availability. Anything less is labelled *incomplete reference*.

---

## 5. Missing pools, protocol families and likely gaps

| Gap | Consequence |
|---|---|
| No official DEX-native source; only aggregator rankings | Ranked/volume-sorted views can never be treated as complete inventories. |
| GT chain-wide cap 1,000 and per-DEX cap 200/500 | Tail venues underrepresented; global cap favours large DEXes. |
| GT DEX enumeration pages 1–3 + `<20` break | Tail venues may never be queried; curated slug list is the real driver. |
| DexScreener 3 hub tokens × 30 observed results | Pools without WETH/WAVAX/USDC/USDT invisible to that adapter. |
| Uniswap V4 `pool_id` dropped, synthetic address used as identity | Cannot dedupe/verify V4; health probes non-contract; multi-shard hit reconstruction breaks. |
| Metric/Fluid/Infinity identity fields discarded | Protocol-specific pools cannot be canonically represented. |
| Unknown remote DEX → `UniswapV2, fee=0` | Wrong quotes; blocks fail-closed `Unknown` path in `add_pool_to_manager`. |
| Infinity on Base visible in GT curated slugs but no on-chain manager in `chains.toml` | Remote-only rows with no executable venue support, indistinguishable from supported pools. |
| Pendle configured on 5 chains, none on AVAX/Polygon; Camelot only Arbitrum | Venue coverage depends on hand-maintained TOML edits. |
| Ranking applied after merge, but SQLite retains pre-rank rows | Ranked outcome, SQLite registry and Live universe diverge. |
| Master inventory and tradable registry share one `pool_info` table | Unsupported/unverified pools have nowhere to live; silently dropped or mislabelled. |
| getLogs watermark advanced on failed scans | Permanent coverage gaps. |
| No reorg reconciliation | Discovery observations can outlive the blocks that produced them. |

---

## 6. Filtering, ranking, pagination and deduplication problems

| Problem | Code | Required correction |
|---|---|---|
| Unknown TVL ⇒ dropped when `min_tvl` set | `jobs/discover.rs:150-152` | Tiered ranking; unknown-TVL retained in Master, excluded from top-rank Tradable only by explicit policy. |
| Global `max_pools` truncation after TVL sort | `jobs/discover.rs:158-160` | Remove default cap; per-source/per-DEX budgets plus explicit resource caps. |
| `$25k` default has no documented rationale in code | `settings.rs:409-411`, `jobs/discover.rs:51` | Make policy configurable and report its effect in output/benchmark. |
| GT stops on post-filter empty page | `geckoterminal.rs:76-93` | Paginate on raw rows; filter after full page collection; record raw vs retained counts. |
| GT per-DEX error discards chain-wide success | `remote/mod.rs:305-311` | Per-adapter result objects; failures recorded as observations, never zero out other results. |
| Dedup key is address only | `jobs/discover.rs:129-142`, `remote/mod.rs:518-538` | Canonical identity `(chain, family, identity)`. |
| `merge_from` first-wins with no provenance | `mod.rs:295-361` | Multiple observations with source, timestamp, confidence and deterministic conflict rules. |
| Remote-only unknown DEX fallback to V2 | `remote/mod.rs:127-133` | Keep `Unknown` + raw label; never assign quoting family without evidence. |
| Health treats zero liquidity as `Dead` and removes | `mod.rs:2074-2097`, `factory.rs:748-759` | Separate inventory health from tradability; zero reserves ≠ delete from Master. |
| `creation_block = 0` for all remote pools | `remote/mod.rs:260-261` | Mark unknown rather than genesis; fill by incremental verification. |
| `getLogs` watermark advanced on swallowed error | `jobs/discover.rs:247-253,549-557` | Advance only on verified full-window success (mirror receipts guard). |
| `INSERT OR REPLACE` + first-wins merge | `cache/store/pools.rs:22-49` | Observation-based upsert instead of row replacement. |

---

## 7. Target architecture

```
Discovery Sources
  ├─ OnChainFactoryAdapter   (existing scanners)
  ├─ ReceiptAdapter          (existing receipts path)
  ├─ GeckoTerminalAdapter    (chain-wide + per-DEX + dex enumeration)
  ├─ DexScreenerAdapter      (hub + per-token + pair lookup)
  ├─ OfficialApiAdapter      (per-protocol REST/GraphQL)
  ├─ SubgraphAdapter         (per-protocol GraphQL, key/budget config)
  └─ RegistryAdapter         (factory enumeration / protocol registries)
        ↓ each returns SourceResult { observations[], completeness, errors[], cost }
  Normalization → CanonicalIdentity → Dedup + provenance
        ↓
  Master Pool Inventory → verification/enrichment → Tradable Pool Registry
        ↓
  PoolManager / Live / detectors
```

### 7.1 Two datasets

| Dataset | Contents | Deletion policy |
|---|---|---|
| **Master Pool Inventory** | Every unique discovered pool with canonical identity, raw labels, partial metadata, provenance, last-seen, TVL observations | Never deleted for unknown TVL, unsupported family or zero reserves; marked `inactive`/`unverified` with evidence. |
| **Tradable Pool Registry** | Subset with verified identity, complete tokens/parameters, non-zero liquidity, quoteable state, supported execution family | Removed/refreshed on failed state init, quote failure or confirmed drained state; re-entry automatic when verification succeeds. |

### 7.2 Schema extension (minimum)

| Field group | Fields |
|---|---|
| Identity | `chain_id`, `canonical_id` (family-prefixed), `address` (nullable for singletons), `pool_id` bytes32, `factory/manager`, `family`, `raw_dex_label` |
| Tokens | `token0`, `token1`, canonical order, full `underlying_tokens`, symbols/decimals, `hook_address`, `bin_step`, `maturity` |
| Provenance | `discovery_sources[]`, `first_seen`, `last_seen`, `observation_block/timestamp`, `source_confidence` |
| Liquidity | `liquidity_verified_at_block`, `reserves_nonzero`, `tvl_usd`, `tvl_source`, `tvl_observed_at`, `tvl_confidence` |
| Activity | `volume_24h/7d/30d`, `volume_source`, `last_swap_block`, `last_swap_timestamp` |
| Status | `metadata_complete`, `identity_verified`, `liquidity_active`, `recently_traded`, `state_init_supported`, `quote_supported`, `two_hop_supported`, `health_probe_supported`, `execution_supported`, `health_state`, `exclusion_reason` |

Storage: keep `pool_info` as legacy/tradable view; add `pool_identity`, `pool_observation`, `pool_status`, `source_run`, with schema migration (v13 → v14). Resolve the unused `pool_states` table explicitly (write it or drop it).

### 7.3 Canonical identity rules

| Family | Canonical key |
|---|---|
| V2 / V3 / Algebra / Solidly / Camelot / Curve plain / LFJ LB | `chain_id : pool_address` |
| Uniswap V4 | `chain_id : "v4" : poolId(bytes32)` — store full pool key (hook/fee/tickSpacing); synthetic address is derived only |
| Pancake Infinity | `chain_id : "infinity" : poolId(bytes32)` |
| Balancer | `chain_id : "balancer" : poolId(bytes32)` (+ vault address) |
| Multi-token/singleton families | `chain_id : family : protocol_id` |

Never merge different pools just because they share a token pair; never split one pool because two sources found it.

### 7.4 Capability flags (not one boolean)

Because support differs by detector, the Tradable Registry must track:

| Capability | Current state |
|---|---|
| State init | V2/V3/V4/Infinity/Curve/Balancer/Solidly/Camelot/LB/Pendle yes; Metric/Fluid `None` (`factory.rs:936-975`) |
| Exact-in quote | V2/V3/V4/Infinity/Curve/Balancer/LB/Pendle; Metric/Fluid `None` (`math/core.rs:62-118`) |
| Two-hop quote | V2/V3/V4/Infinity/Curve/Balancer; LB/Pendle/Metric/Fluid `Unsupported` (`quoting.rs:41-53`) |
| Health probe | V2/Solidly/Camelot/V3/V4/Infinity/LB/Pendle/Curve/Balancer; Metric/Fluid/Unknown none (`health.rs:24-97`) |

A pool can be in Master with `two_hop_supported=false` and still be used by multi-hop if state init succeeds.

---

## 8. Recommended source-adapter architecture

| Rule | Implementation detail |
|---|---|
| Result isolation | `SourceResult` returned even on partial failure; orchestrator merges successes and records failures. No `Vec::new()` on a sibling error. |
| Completeness flags | `complete`, `top_n_ranked`, `pagination_limited`, `unknown`, `hard_limit`; a top-N source must never be presented as full coverage. |
| Budgets | Per-adapter request/page/time caps configurable; page loops driven by raw response length, not post-filter length. |
| Provenance | Adapter name + endpoint + fetch time + block/timestamp + raw fields stored per observation. |
| Identity resolution | Adapter emits canonical identity + raw identifiers; ambiguous label ⇒ `Unknown` in Master, never V2. |
| Incremental / reorg-aware | Store source cursor and canonical block hash; re-fetch overlapping windows; invalidate observations whose block is no longer canonical. |
| Failure taxonomy | `rate_limited`, `timeout`, `bad_schema`, `auth`, `unsupported_chain`; 429 must degrade gracefully (observed live). |
| Testability | Every adapter has wiremock tests for page 1, empty page, 429, malformed row, duplicate identity, unknown TVL. |

---

## 9. Benchmark methodology

### 9.1 Benchmark A — pool-universe coverage

For each `(chain, DEX)` build a reference inventory from its own listing interface or verifiable authoritative source with explicit snapshot block/time and eligibility rules.

| Chain | DEX | Reference pools | Our pools | Missing | Additional | TVL coverage | Reference complete? |
|---|---|---:|---:|---:|---:|---:|---|

Metrics:

- **Pool coverage** = matched eligible reference pools / all eligible reference pools.
- **TVL-weighted coverage** = TVL(matched reference pools) / TVL(all eligible reference pools).
- **Unique incremental yield** = new unique verified pools contributed by source *i* after sources 1..i−1.
- **Source overlap** = pairwise Jaccard of canonical identities, per chain and family.
- Label incomplete references explicitly (ranked subset, capped pages, missing fields).

### 9.2 Benchmark B — realized opportunity recall

Same chain and block/time range; compare Current discovery vs Expanded discovery vs native references on:

- active pools, quoteable pools, initialized pools, pair count;
- two-hop / triangular / multi-hop candidate counts;
- opportunities per strategy, overlap, missed opportunities, false positives;
- RPC cost and runtime.

### 9.3 Existing baseline

`docs/POOL_DISCOVERY_RECEIPTS.md:106-133` and checked-in `bench/pool-discovery-M{0..5}-W1.json`:

| Method | Pools (3 runs) | Median time | RPC calls/run |
|---|---:|---:|---:|
| M0 hybrid | 27/27/27 | 85.0 s | 71 |
| M1 receipts | 100/100/101 | 14.0 s | 488–566 |
| M2 receipts (Dwellir) | 100/68/97 | 515.0 s | 478–553 |
| M3 mixed | 110/108/51 | 171.5 s | 452–627 |
| M4 factory_state | skipped | — | — |
| M5 remote | 27/27/27 | 5.6 s | 1 |

This measures *method cost*, not reference coverage or opportunity recall; it cannot be used as a coverage claim. Bench settings intentionally neutralize filters: `min_tvl = 0`, `max_pools = 100_000`, `health_check = false`, `resolve_remote_metadata = false`, `factory_batch_size = 10000` (`core/examples/pool_discovery_bench.rs:207-296`).

---

## 10. Phase-by-phase implementation plan

### Phase 0 — Repository and discovery audit

| Item | Detail |
|---|---|
| Files affected | None (this document only). |
| Design | Read-only verification of source, docs, tests, live endpoints. |
| Tasks | Map data flow; enumerate sources/factories; probe official APIs; record defects and stale docs. |
| Tests | None. |
| Expected output | This document with cited findings and coverage matrix. |
| Acceptance criteria | Every important claim cited; unverified sources labelled; no invented endpoints or counts. |
| Performance / RPC cost | Live HTTP probes only; no chain RPC. |
| Status | **Complete.** |

---

### Phase 1 — DEX/source coverage matrix and reference benchmark

| Item | Detail |
|---|---|
| Files affected | New `docs/POOL_SOURCE_MATRIX.md`; new `core/examples/pool_reference_bench.rs`; new `bench/reference/*.json`; `core/src/pool/discovery/remote/mod.rs` (source metadata struct). |
| Design | `SourceDescriptor { provider, chain, endpoint, pagination, completeness, limits, freshness_field, identity_field }`. Reference inventories are versioned by snapshot block/time and eligibility rules. |
| Tasks | 1) Verify official API/subgraph/pool-list endpoints per chain/DEX from protocol docs or live probes. 2) Record rate limits, page sizes, hard caps, missing-field frequency, freshness. 3) Build reference inventories for every obtainable `(chain, DEX)`. 4) Implement coverage metrics from §9.1. 5) Label incomplete references. |
| Tests | Wiremock fixtures per reference source; pagination-limit tests; fixture-based coverage calculation tests. |
| Expected output | Per-chain/DEX table: reference pools, our pools, missing, additional, TVL coverage, completeness label; incremental-yield and overlap tables. |
| Acceptance criteria | At least one verified native reference per major DEX where obtainable; every reference labelled complete/incomplete; no endpoint or count asserted without probe evidence. |
| Performance / RPC cost | HTTP only; budget 1–3 req/s per provider; GeckoTerminal observed 429 at ~7 rapid requests. |

---

### Phase 2 — Inventory and ranking architecture corrections

| Item | Detail |
|---|---|
| Files affected | `core/src/pool/discovery/mod.rs` (schema, `merge_from`, identity), `core/src/jobs/discover.rs` (ranking/persistence), `core/src/config/settings.rs` (defaults/policy), `core/src/cache/store/mod.rs` (schema v14 migration), `core/src/cache/store/pools.rs` (observation upserts). |
| Design | Split Master Inventory from Tradable Registry; identity-keyed tables; observation log; tiered ranking with no default global cap; provenance-aware conflict resolution. |
| Tasks | 1) Add identity/canonical-id fields and migration. 2) Add `pool_observation` and `pool_status` tables. 3) Replace destructive `apply_explorer_ranking` with tiered ranking that retains unknown-TVL pools in Master. 4) Remove default `max_pools` cap; expose explicit budgets. 5) Fix `factory_pools.drain()` bug. 6) Retain V4 `pool_id`, Metric `poolId`, Fluid `dexId`, LFJ `bin_step`. 7) Decide `pool_states` fate. 8) Prevent `Unknown`→V2 assignment. 9) Make cache merges observation-based instead of `INSERT OR REPLACE`. |
| Tests | Update `jobs/discover.rs:904-954` to assert unknown-TVL **retained**; new tests for identity keys, observation merging, migration round-trip, V4 poolId persistence, conflict rules. |
| Expected output | Default run loses zero pools purely for missing TVL; singleton identities survive persistence; Master and Tradable are distinct datasets. |
| Acceptance criteria | No pool deleted from Master for unknown TVL or unsupported family; all required identity fields round-trip; legacy cache migrates without data loss. |
| Performance / RPC cost | No extra RPC; SQLite write volume increases with observations — batch inserts and index `(chain, canonical_id, source)`. |

---

### Phase 3 — New source adapters and pagination

| Item | Detail |
|---|---|
| Files affected | `core/src/pool/discovery/remote/mod.rs` (trait/registry), `remote/geckoterminal.rs`, `remote/dexscreener.rs`, new `remote/official_api.rs`, `remote/subgraph.rs`, `remote/registry.rs`; `core/src/jobs/discover.rs` (watermark fix). |
| Design | Per-adapter `SourceResult` with completeness and cost; shared rate limiter; raw-page pagination; per-source cursors; no cross-source result loss. |
| Tasks | 1) Introduce adapter trait and registry. 2) Fix GT raw-page pagination and remove post-filter early stop. 3) Fix per-DEX fallback error discarding successes. 4) Expand DEX enumeration beyond 3 pages with budget. 5) Add DexScreener per-token/pair lookup as enrichment (not exhaustive enumeration). 6) Add verified official API/subgraph adapters from Phase 1. 7) Add DefiLlama yields as TVL/volume enrichment keyed by `(chain, project, symbol)` — never identity. 8) Fix `getLogs` watermark advancement on failed scans. 9) Add reorg-aware observation invalidation. |
| Tests | Wiremock: page continuation past low-TVL page; 429 retry/backoff; partial failure isolation; duplicate identity across sources; malformed row; cursor resume; watermark not advanced on failure. |
| Expected output | Union ≥ any single source; GT reaches raw exhaustion or reports hard limit; one adapter failure cannot zero others. |
| Acceptance criteria | `discover_via_remote` never discards a successful source's results; every adapter reports completeness/limits; failed scan ranges remain uncovered. |
| Performance / RPC cost | GT: shared limiter (observed 429 at ~7 rapid requests); per-DEX requests scale with enumerated DEX count (Avalanche observed 56+1 dexes); per-DEX cap 200; document all budgets in report. |

---

### Phase 4 — Active / liquidity / TVL validation

| Item | Detail |
|---|---|
| Files affected | `core/src/pool/discovery/health.rs`, new `core/src/pool/discovery/validation.rs`, `core/src/pool/discovery/metadata.rs`. |
| Design | Separate `identity_verified`, `metadata_complete`, `liquidity_active`, `tvl_verified`, `recently_traded`, `quoteable` with timestamps and sources. |
| Tasks | 1) Protocol-correct health probes: V4 via PoolManager + poolId; Balancer via vault + poolId; add Metric/Fluid fallback probe or mark unprobeable. 2) Add on-chain liquidity verification independent of aggregator TVL. 3) Add TVL enrichment with source + observation time. 4) Add recent-activity detection from swap events/volume fields. 5) Stop deleting from Master on zero reserves; write status instead. |
| Tests | Unit tests per family probe; tests that zero-liquidity pool stays in Master but leaves Tradable; unknown-TVL pool retained; TVL source/timestamp present. |
| Expected output | Every reported TVL has source and timestamp; every “active” classification backed by chain state. |
| Acceptance criteria | No Master deletion for unknown TVL, unsupported family or zero reserves; no reported TVL without provenance. |
| Performance / RPC cost | One `eth_call` per candidate probe, batched via `Multicall3` where possible; bounded concurrency; reuse discovery cache to avoid duplicate probes at different blocks. |

---

### Phase 5 — Live and PoolManager integration

| Item | Detail |
|---|---|
| Files affected | `core/src/pipeline/runner.rs`, `core/src/pool/state/manager.rs`, `core/src/pool/state/factory.rs`, `core/src/jobs/index.rs`, `core/src/jobs/live.rs`, `cli/src/commands/live.rs`, `core/src/mev/detectors/{two_hop,multi_hop}.rs`. |
| Design | Tradable Registry selected by capability flags; no auto-V2 fallback; fail-closed `Unknown` everywhere; separate budgets for two-hop and multi-hop; reorg invalidation. |
| Tasks | 1) Load tradable subset by identity rather than raw `address` key. 2) Preserve fail-closed `Unknown` from discovery through quoting. 3) Add capability flags per §7.4; allow multi-hop-only pools. 4) Separate `max_pairs_per_token` (two-hop) from multi-hop negative-cycle caps; report candidate counts and prune reasons. 5) Let Live bootstrap expand a non-empty but incomplete cache. 6) Avoid dropping pools merely because metadata repair fails — move them to Master with exclusion reason. 7) Add canonical-hash checks for cached blocks used by Live. |
| Tests | Pair generation with large universes; `Unknown`/unsupported excluded from quotes but present in Master; Live bootstrap with partial cache; reorg invalidation; per-detector budget reporting. |
| Expected output | Newly verified pools enter Live without manual factory-list edits; unsupported families never quoted; pair explosion controlled and reported. |
| Acceptance criteria | Inventory coverage and trading support reported separately; no unsupported pool is quoted; two-hop and multi-hop candidate counts explained. |
| Performance / RPC cost | State init scales with tradable pool count; use existing concurrency cap and `creation_block` filter; do not initialize Metric/Fluid as quotes unless implemented. |

---

### Phase 6 — Controlled benchmark and regression testing

| Item | Detail |
|---|---|
| Files affected | `core/examples/pool_discovery_bench.rs`, new `core/examples/opportunity_recall_bench.rs`, new `docs/benchmarks/pool-coverage-*.md`, new `docs/benchmarks/opportunity-recall-*.md`. |
| Design | Benchmark A (pool universe) and Benchmark B (realized opportunity recall) as defined in §9, with identical blocks/time range and explicit snapshots. |
| Tasks | 1) Extend harness to ingest reference inventories and diff by canonical identity. 2) Compute coverage, TVL-weighted coverage, incremental yield, overlap. 3) Add opportunity-recall harness comparing current vs expanded vs reference. 4) Record RPC bytes/calls/runtime. 5) Add missing regression tests: CLI→SQLite→Live coverage; scanner/Live reorg; failed-getLogs watermark; singleton identity end-to-end; Master retention vs Tradable removal. |
| Tests | Full discovery/job/state test suite; new integration tests above; before/after JSON artifacts. |
| Expected output | Before/after tables for both benchmarks, with provider-limit caveats. |
| Acceptance criteria | Coverage ≥ each obtainable native reference (or gap explained by provider limits); realized opportunity recall non-decreasing; no unexplained runtime/RPC regression. |
| Performance / RPC cost | Document exact budgets. Do not run 30-day receipt scans without explicit approval — docs estimate 4–6 h at concurrency 8 (`POOL_DISCOVERY_RECEIPTS.md:129-133`). |

---

## 11. Documentation and test corrections to fold into the plan

### 11.1 Documentation mismatches

| Claim | Actual |
|---|---|
| `docs/CLI.md:305` and `settings.rs:101` list only `hybrid \| remote \| onchain` | Job accepts `receipts` too (`jobs/discover.rs:316-320`). |
| `docs/CLI.md:92-95,322`: no-range resumes from newest cached `creation_block` | With `lookback_days`, watermark gap logic is used; remote/hybrid can skip on-chain scan (`jobs/discover.rs:390-427`). |
| `README.md:135-137`, `docs/CLI.md:287-291` describe hybrid ranking generally | Ranking applies only when `should_fetch_remote` (`discover.rs:333,583-596`); pure onchain/receipts are unranked/uncapped. |
| `docs/CLI.md:329`: “never clobber richer rows” | `merge_from` fills only `None`, but current non-`None` values win and `INSERT OR REPLACE` is used (`discover.rs:197-202`; `pools.rs:22-49`); the test covers only sparse case (`discover.rs:876-901`). |
| `POOL_DISCOVERY_RECEIPTS.md:3` and `core/examples/pool_discovery_bench.rs:1-2` reference `docs/pool-discovery-benchmark-plan.md` | File was deleted in `a18f3e5` along with `docs/ARCHITECTURE.md`. |
| Bench header says “combined summary” (`bench.rs:5-6`) | Only per-invocation JSON written (`bench.rs:479-490`). |
| `pipeline/runner.rs:940-951`: `init_pools` verifies via `eth_getCode` | `init_pools` does not call `filter_existing_pools` (`runner.rs:952-1013`; `factory.rs:772-779`). |
| `pipeline/runner.rs:1039`: multi-hop BFS depth 4 | Production uses Bellman-Ford negative cycles; BFS compatibility-only (`multi_hop.rs:163-167`). |
| `pool/state/pool_types.rs:76-79`: `PoolInfo` from `eth_getLogs` only | Also remote, receipts, hybrid merge, cache-first merge. |
| `pool/state/pool_types.rs:455`: “DODO pools” above `TraderJoeLB` | Copy/paste error; variant is Trader Joe LB. |

### 11.2 Existing tests that pin current behaviour

- `jobs/discover.rs:781-955`: watermark gaps, cache-first persistence, **ranking drops unknown TVL** (must be updated in Phase 2).
- `discovery/mod.rs:2145-2492`: V4/Infinity decoding, LFJ selectors, Pendle override, `merge_from` conflicts, receipt decoder coverage.
- `remote/mod.rs:545-676`: merge dedup, curated slugs, DefiLlama rejection, strict inference.
- `remote/geckoterminal.rs:440-763`: parse, min-TVL, network slug, token order, sort param, TVL sort, unknown-TVL retention, dex classification.
- `remote/dexscreener.rs:250-342`: hub profile, filter/canonicalize, label precedence, Infinity mapping, wiremock fetch.
- `core/tests/receipts_batch.rs:65-167`: batched receipts, provider pinning, `failed_windows`.

### 11.3 Test gaps to close in Phase 6

- No end-to-end CLI `discover` → SQLite → Live `PoolManager` coverage test.
- No scanner/Live reorg integration test.
- No test that a failed `getLogs` window leaves watermark unchanged.
- No identity-keyed pool (V4 poolId / Balancer poolId) survival test through dedup, persistence and Live init.
- No test distinguishing Master retention from Tradable removal.

---

## 12. Definition of success

The final system provides the broadest practically obtainable inventory of unique, verified, active, TVL-bearing pools across all supported chains and DEXes:

- matches or exceeds the eligible pools exposed by DEX-native reference sources (or explicitly documents provider-imposed limits);
- expands beyond them through independent sources;
- preserves canonical identity, multiple source observations, TVL source/timestamp and explicit provenance;
- never deletes a discovered pool from the Master Inventory because TVL is unknown or execution is unsupported;
- keeps an accurately scoped Tradable Registry aligned with actual state-init and quoting capabilities;
- demonstrably increases genuine arbitrage opportunity recall in Benchmark B, not just theoretical pair counts.

---

## 13. Approval log

| Date | Event |
|---|---|
| 2026-11-27 | Audit and plan delivered in chat; no files changed. |
| 2026-11-27 | Addendum delivered after jobs/integration audit; no files changed. |
| 2026-11-27 | This document created at user request. **Implementation of Phase 2+ still awaits approval.** |

**Next action:** approve Phase 2, or specify a different starting phase.
