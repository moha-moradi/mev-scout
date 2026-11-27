# Pool Discovery Redesign — Audit & Implementation Plan

**Status:** Analysis/planning only. The Final SQL and Architecture Consistency Review has been incorporated. No application source, schema, dependency, or production database was modified, and nothing was committed. Implementation of Phase 2 and later is **pending approval**.
**Repository source baseline:** `a18f3e5`; current documentation baseline `df5a676`. These commits differ only by this plan document, not by `core/`, `cli/`, `Cargo.toml`, or `Cargo.lock`.
**Verification date:** 2026-11-27. Important claims are cited to source files/tests or to recorded live probes. Unverified external sources are explicitly labelled.

---

## خلاصه (فارسی)

این سند نتیجهٔ بررسی کامل ماژول `core/src/pool/discovery/`، مسیر `discover` → SQLite → `PoolManager`/Live و منابع خارجی است. هدف، ساخت مجموعهٔ بزرگی از «موجودی استخرهای فعال با TVL» برای تمام زنجیره‌های پشتیبانی‌شده است که همهٔ منابع را بدون وابستگی به یک منبع، با هویت canonical، سابقهٔ منبع (provenance) و جداسازی کامل «موجودی master» از سه سطح آمادگی Quoteable، Simulation-ready و Execution-ready ترکیب کند. DDL نهایی و تستهای آن با SQLite 3.45.0 بundleشدهٔ مخزن اعتبارسنجی شده است. یافتهٔ اصلی: مقدار پیشفرض `min_tvl = 25,000` و `max_pools = 1000` استخرهای با TVL ناشناخته را حذف میکند؛ هویت Uniswap V4 (poolId) ذخیره نمیشود؛ منابع رسمی DEX اصلاً متصل نیستند؛ و Live از تمام ردیفهای SQLite قدیمی بارگذاری میکند نه از نتیجهٔ رتبهبندی شده. طرح شامل فاز ۰ تا ۶، فایلهای دقیق، تستها، معیارهای پذیرش و هزینهٔ RPC است. **لطفاً قبل از اجرای فاز ۲ یا بعدتر، تأیید دهید.**

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

### 0.4 Final SQL and architecture validation

The revised schema was executed as one complete SQLite fixture, including tables, indexes, composite foreign keys, compatibility/readiness views, triggers, and representative records. Validation used the repository's `rusqlite 0.31.0` bundled SQLite **3.45.0**, not the system Node/Python SQLite stack.

Validated outcomes:

- full schema creation and `PRAGMA integrity_check=ok`;
- empty `PRAGMA foreign_key_check`;
- full singleton PoolIds with the required `0x` prefix;
- unresolved creation/remote/market observations with `pool_pk=NULL`;
- composite evidence-to-pool and source/chain/run/window consistency;
- scoped Live admission and scoped rank uniqueness;
- separate Quoteable, Simulation-ready, and Execution-ready views;
- route/amount/block-specific quote-probe invalidation;
- read-only compatibility views using `INSTEAD OF` triggers;
- cursor/page/snapshot scan windows without invented timestamps;
- intentional rejection of malformed identity, cross-pool evidence, capability dependency violations, duplicate admission/rank, invalid quote probes, and archive writes.

This was temporary design validation only. It did not migrate or modify any repository database.

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
| Current arbitrage graph | `max_pairs_per_token` default 50 (`pool/state/manager.rs:163`); per-token overrides `:244-254`; pairs sorted by liquidity estimate then truncated (`manager.rs:403-444`). `Metric`/`Fluid` liquidity estimate is hardcoded `0` (`manager.rs:398`), so they are pruned first. |
| Multi-hop | Bellman-Ford over all pools (`multi_hop.rs:738-772`), caps `MAX_NEGATIVE_CYCLES=16`, `MAX_CYCLE_ATTEMPTS=64`, depth 4 (`multi_hop.rs:25-40`). BFS is compatibility-only (`:163-167`). |
| Quote support | Exact-in math: V2, V3/V4/Infinity, Curve, Balancer, LB, Pendle; Metric/Fluid return `None` (`pool/math/core.rs:62-118`). Two-hop optimizer: LB/Pendle/Metric/Fluid are `Unsupported` (`pool/state/quoting.rs:41-53`). |

### 2.3 Confirmed code defects

| Defect | Location | Effect |
|---|---|---|
| `factory_pools.drain()` empties the map, then `underlying_tokens` is read from it | `mod.rs:1367` vs `1409-1411` | The post-drain lookup is always empty. Factory rows already inserted into `resolved_addrs` keep their existing underlying tokens; the defect primarily breaks later association for metadata-only results. |
| GT pagination evaluates “empty page” after `min_tvl` filter | `geckoterminal.rs:76,78,84-93,268-277` | A page of sub-threshold pools terminates discovery early. |
| GT per-DEX fallback error returns empty vector | `remote/mod.rs:305-311` | One source error destroys successful chain-wide results. |
| V4 `Initialize` decoder never sets `pool_id` | `v4.rs:60-73` | Singleton pool identity unrecoverable; health probes synthetic address. |
| Metric `bytes32 poolId` discarded; Fluid `dexId` ignored; LFJ `binStep` ignored | `metric.rs:18-30`, `fluid.rs:20-40`, `trader_joe.rs:22-49` | Protocol identity/parameters lost. |
| No topic-length guards in several `classify_activity` helpers | e.g. `v2.rs:10`, `v3.rs:11`, `curve.rs:15` | Malformed logs can panic on `topics()[0]`. |
| Solidly/Camelot have no activity classifier | `v2.rs:9-15`, `mod.rs:687-698` | Activity-only pools mislabelled `UniswapV2`, fee 30. |
| `getLogs` watermark advanced on failed scans | `jobs/discover.rs:247-253,549-557` | Failed ranges can be permanently skipped. |
| `pool_states` table exists but no writer found | `cache/store/mod.rs:162-167` | Ambiguous half-schema. |
| Migration SQL strings are enumerated as versions and `schema_version` is never seeded | `cache/store/mod.rs:223-229,238-303` | A fresh database may not reach the intended v13 columns; existing databases can have different structural shapes despite the same nominal version. |
| Balancer `pool_id` is omitted during metadata/runtime state construction | `pool/state/factory.rs:113-118`; `pipeline/runner.rs:1827-1840` | Balancer quote/execution code exists but is not reachable end-to-end. |
| V4 state initialization reads the synthetic address; singleton-emitted swaps are filtered by emitter address | `pool/state/factory.rs:845-860`; `pool/state/apply.rs:137-208` | V4/Infinity/Balancer manager or Vault events do not reliably update address-keyed pool state. |

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
| Master inventory and the current runtime pool set share one `pool_info` table | Unsupported/unverified pools have nowhere to live; silently dropped or mislabelled. |
| getLogs watermark advanced on failed scans | Permanent coverage gaps. |
| No reorg reconciliation | Discovery observations can outlive the blocks that produced them. |

---

## 6. Filtering, ranking, pagination and deduplication problems

| Problem | Code | Required correction |
|---|---|---|
| Unknown TVL ⇒ dropped when `min_tvl` set | `jobs/discover.rs:150-152` | Tiered ranking; unknown-TVL retained in Master and excluded from readiness/admission only by explicit configurable policy. |
| Global `max_pools` truncation after TVL sort | `jobs/discover.rs:158-160` | Remove default cap; per-source/per-DEX budgets plus explicit resource caps. |
| `$25k` default has no documented rationale in code | `settings.rs:409-411`, `jobs/discover.rs:51` | Make policy configurable and report its effect in output/benchmark. |
| GT stops on post-filter empty page | `geckoterminal.rs:76-93` | Paginate on raw rows; filter after full page collection; record raw vs retained counts. |
| GT per-DEX error discards chain-wide success | `remote/mod.rs:305-311` | Per-adapter result objects; failures recorded as observations, never zero out other results. |
| Dedup key is address only | `jobs/discover.rs:129-142`, `remote/mod.rs:518-538` | Canonical identity `(chain_id, identity_namespace, canonical_identifier)`; family/version remain mutable classification and are never part of the unique identity. |
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
  Master Pool Inventory → verification/enrichment
        ├─ Quoteable scopes → candidate discovery / dashboards
        ├─ Simulation-ready scopes → replay / backtest / paper
        └─ Execution-ready scopes → Live planner / executor
```

### 7.1 Inventory and readiness datasets

| Dataset | Contents | Deletion/suspension policy |
|---|---|---|
| **Master Pool Inventory** | Every unique discovered pool, including unresolved/provisional identities, partial metadata, unsupported families, zero liquidity, and unknown TVL | Never deleted for unknown TVL, unsupported family, or zero reserves. Orphaned evidence is invalidated, not the entity. |
| **Quoteable Registry** | Verified pools with at least one valid route/amount/slippage/state-block quote probe and verified exact-quote capability | A route leaves only when its probe becomes stale/orphaned/failed or its capability is withdrawn. |
| **Simulation-ready Registry** | Quoteable scopes plus verified state initialization and swap replay | Used for replay/backtest/paper simulation; does not imply execution support. |
| **Execution-ready Registry** | Simulation-ready scopes plus verified execution capability and a current scoped Live admission | Used only by the Live execution planner. Suspension never removes the Master entity. |

These are read-only projections over one authoritative inventory, not independently mutable registries.

### 7.2 Schema v14 contract and repository integration

#### Authoritative tables

| Group | Tables |
|---|---|
| Identity/metadata | `pool_identity`, `pool_legacy`, `pool_address_role`, `pool_token`, `pool_parameter`, `pool_classification` |
| Evidence/status | `pool_observation`, `observation_metric`, `pool_quote_probe`, `pool_status` |
| Capability/admission | `pool_capability`, `live_admission`, `live_admission_reason` |
| Source/scan/reorg | `discovery_source`, `source_run`, `scan_window`, `scan_attempt`, `scan_segment`, `scan_cursor`, `reorg_event` |
| Revisioning | `registry_revision` |

Required constraints:

- Canonical identity is `(chain_id, identity_namespace, canonical_identifier)`; family/version are not identity.
- Singleton canonical identifiers include `0x` before both deployment address and full 32-byte PoolId.
- Evidence references use composite foreign keys so an observation cannot be attached to another pool or chain.
- Source, run, scan-window, and scan-attempt chain/source relationships are composite-FK enforced.
- Unresolved observations may have `pool_pk=NULL`; `identity_match_state='exact'` may not.
- `live_admission` uses `(pool_pk, policy_version, scope_key)`; rank uniqueness is `(policy_version, scope_key, rank)`.
- Capability dependencies are trigger-enforced: exact quote requires quote math; execution requires same-scope exact quote plus state init and replay.
- `scan_window.request_kind` supports `block_range`, `time_range`, `cursor`, and `snapshot`; cursor/snapshot requests do not require invented timestamps.

#### Compatibility objects

- `pool_info` becomes a **read-only, verified-only, nullable compatibility view**.
- `pool_info_legacy_complete` exposes the old 22-column shape only when address, tokens, fee, DEX type, and creation block are all known.
- Unknown tokens, DEX family, fee, creation block, TVL, volume, and factory remain `NULL`; manager/vault addresses are never substituted as factory.
- View writes are rejected with `INSTEAD OF` triggers.
- `pool_info_v13`, `pool_states_v13_archive`, and `discovery_scan_watermark_v13_archive` are immutable migration archives.
- `pool_states` has no current reader or writer and is retired rather than revived.

#### Existing Rust and SQL integration

| Current integration point | Required v14 behavior |
|---|---|
| `cache/store/pools.rs::put_discovered_pool` | Retain the public signature as a compatibility adapter, but write identity/observation/projection transactions instead of `INSERT OR REPLACE`. Partial/zero-token input is retained as unresolved/provisional, not silently discarded. |
| `get_discovered_pool` / `list_discovered_pools` | Continue returning legacy `PoolInfo` only from `pool_info_legacy_complete`. Add `get_pool_record` / `list_master_inventory` for full canonical records. |
| `row_to_pool_info` | Keep positional mapping only for the complete compatibility view; do not read nullable `pool_info` directly. |
| `persist_discovered_pools` | Use the new repository API and retain unresolved identities. Zero-token rows remain in Master with missing-metadata status. |
| `jobs/discover.rs::persist_universe` | Replace fill-only cache merge plus REPLACE with observation-based upsert and deterministic conflict rules. |
| `jobs/index.rs::load_pool_registry` | Read Master/classification records. Keep its existing address-based `PoolRegistry` projection for address pools; singleton support is added in Phase 5. |
| `jobs/index.rs` derivation repair | Update classification/evidence through the repository API; never write the compatibility view. |
| `jobs/live.rs` and `jobs/run.rs` | Stop loading raw `list_discovered_pools`. Live consumes readiness views; run/backtest consumes Simulation-ready records. |
| `pipeline/runner.rs::init_pools` | Accept canonical records/scopes and initialize by `PoolKey`, not synthetic address. |
| `pool_count` / `cli/src/commands/discover.rs::cache_is_populated` | Count Master entities for reporting only. Incremental/bootstrap decisions use per-source coverage and readiness, never a non-zero row count. |
| `earliest_creation_block_by_factory` / `max_creation_block` | Derive from canonical creation observations and verified factory roles. |
| `discovery_watermark` / `note_discovery_scan` | Replace with per-source/range APIs. Legacy read may return a span only when every required source/type proves contiguous coverage; legacy write fails closed. |
| `list_pool_dex_names` / `pool_dex` | Keep the optional dashboard enrichment table and reader for address pools. It is not authoritative identity; singleton labels live in classification/observations. |
| `cli/examples/cacheq.rs` | Read `pool_info_legacy_complete` for old counts or Master views for full inventory; never write `pool_info`. |
| Test seed helpers/direct SQL fixtures using `put_discovered_pool` or `INSERT INTO pool_info` | Retain compatibility writes through the adapter, but move direct SQL fixtures to the new repository API or archive setup. Assert unresolved records through the Master API; seed verified tokens/status explicitly where hydration is required. |

#### Migration safety

Phase 2 must repair migration versioning before adding v14:

1. Inspect actual columns with `PRAGMA table_info`; do not trust `MAX(schema_version.version)`.
2. Add missing intended v13 columns without broadly swallowing duplicate-column errors.
3. Seed `schema_version` with `INSERT OR IGNORE`, not `UPDATE` against an empty table.
4. Run v14 DDL, archive renames, data import, view creation, and version stamping in one `BEGIN IMMEDIATE` transaction.
5. Keep every legacy row linked through `pool_legacy`; ambiguous records remain unresolved/provisional.
6. Never convert the old chain watermark into completed per-source scan windows.

Migration/rollback tests must use sanitized temporary copies of real v13 databases and cover:

- full 22-column v13, sparse/older structural variants, and empty `schema_version`;
- V4/Balancer rows with full PoolIds and rows with only synthetic addresses;
- zero-token, remote `creation_block=0`, unknown-DEX, and conflicting legacy records;
- existing `pool_states` and watermark rows preserved in archives;
- row-count/hash preservation, `foreign_key_check`, `integrity_check`, and old-reader smoke tests;
- crash before commit, crash after commit, reopen/idempotency, and backup-restore rollback;
- proof that no legacy row is auto-admitted to Live without fresh verification.

Use the existing `VACUUM INTO` snapshot pattern in `core/tests/live_tag_parity.rs:134-149`; never test against a production database.

### 7.3 Canonical identity rules

| Architecture | Canonical identity | Status |
|---|---|---|
| V2 / V3 / Algebra / Slipstream / Solidly / Camelot / Curve / LFJ LB / Pendle | `chain_id:evm_pool_address:0x<pool>` | **Confirmed.** Actual per-pool contract address is authoritative. |
| Uniswap V4 | `chain_id:evm_singleton_pool:0x<manager>:0x<full_pool_id>` | **Confirmed.** No synthetic pool address is stored as identity. |
| Balancer V2 | `chain_id:evm_singleton_pool:0x<vault>:0x<full_pool_id>` | **Confirmed.** Actual pool contract address may be stored separately when supplied by `PoolRegistered`. |
| Pancake Infinity | `chain_id:evm_singleton_typed_pool:0x<deployment>:<pool_type>:0x<full_pool_id>` | **Confirmed structure.** Exact pool-type tags and manager/vault role per type require Phase 1 protocol verification. |
| Metric | Default `chain_id:evm_pool_address:0x<pool>` provisionally; retain full `poolId` as a protocol parameter | **Unresolved whether singleton.** Do not change namespace until protocol evidence proves deployment+PoolId uniqueness. |
| Fluid | Default `chain_id:evm_pool_address:0x<pool>` provisionally; retain full `dexId` as a protocol parameter | **Unresolved whether singleton.** Same rule as Metric. |
| Balancer V3 | No v14 identity rule enabled | **Out of current scope.** V2 Vault+PoolId semantics must not be silently generalized. Add only with explicit V3 support and tests. |
| Other singleton architectures | `chain_id:evm_singleton_pool:0x<deployment>:0x<native_id>` or typed form | Use only after the protocol's deployment and native identifier are verified. |

Factory, manager, vault, registry, hook, router, and execution target are separate address roles. Family/version live in `pool_classification`; correcting family updates classification, parameters, capabilities, and status without changing canonical identity.

Never merge different pools merely because they share a token pair; never split one pool because two sources found it.

### 7.4 Capabilities and readiness scopes

Capabilities are stored per pool, capability, and scope. A math arm alone is not executable support.

| Family | Effective current limitation | Readiness policy |
|---|---|---|
| V2 / V3 / Curve / Solidly / Camelot | Existing state/quote paths | Eligible after state, replay, quote, and execution tests pass. |
| Trader Joe LB | Two-hop optimizer unsupported; versioned router required | May be Simulation-ready and multi-hop capable; Execution-ready only for a verified router scope. |
| Pendle | Two-hop unsupported; underlying wrap/unwrap route not modeled | PT/SY scope only unless the route is explicitly implemented and tested. |
| Balancer V2 | `pool_id` is lost in metadata/runtime state; Vault-emitted replay is not routed | Inventory/quote-math only until Phase 4/5 fixes prove state, replay, quote, and execution. |
| Uniswap V4 | State read uses synthetic address; singleton replay/execution unsupported | Inventory/quote-math only until manager+PoolId hydration, replay, and execution are verified. |
| Pancake Infinity | Manager state exists, but tick map/replay/execution are incomplete | May become Quoteable after a valid probe; not Simulation-ready or Execution-ready until replay/execution pass. |
| Metric / Fluid | Event-only; no state init, exact quote, health probe, or execution | Master only. |
| Unknown | Fail closed | Master only. |

Readiness views:

- `quoteable_pool_scope`: one row per successful valid probe, including pool, scope, token direction, amount, slippage, block number, and block hash.
- `quoteable_pool_registry`: distinct pools with at least one quoteable scope.
- `simulation_ready_pool_scope`: quoteable scopes with verified `state_init` and `swap_replay`.
- `simulation_ready_pool_registry`: distinct simulation-ready pools.
- `execution_ready_pool_scope`: simulation-ready scopes with same-scope execution capability and a current admitted `live_admission` row referencing that exact probe.
- `execution_ready_pool_registry`: distinct execution-ready pools.

Consumers:

| Consumer | Dataset |
|---|---|
| Offline candidate discovery, two-hop/multi-hop path enumeration, dashboards | Quoteable |
| Backtest, replay, paper trading, opportunity validation | Simulation-ready |
| Live transaction builder and executor | Execution-ready |

A pool can be Quoteable without being Simulation-ready, or Simulation-ready without being Execution-ready.

### 7.5 Status revision, quote freshness, and reorg invalidation

- `pool_status` holds one current status row per pool and a monotonically increasing `status_revision`.
- `live_admission` stores the exact `status_revision` and `quote_probe_id` used for the decision.
- Any identity, token, parameter, liquidity, health, capability, or canonical-head change increments `status_revision` and marks prior admissions non-current/suspended in the same transaction.
- A quote probe is valid only when its observation is canonical/valid, its capability remains verified, and its age is within configurable block/time freshness limits.
- Live normally requires the probe at the current canonical head or within `quote_max_lag_blocks`; backtest requires the exact benchmark block.
- A new direction, amount, slippage, scope, or block creates a new probe. It never overwrites the old evidence.
- `active`, `liquidity available`, `recently traded`, `TVL verified`, `Quoteable`, `Simulation-ready`, and `Execution-ready` remain independent predicates.

Reorg handling:

1. Detect the hash mismatch and discover the common ancestor using network calls **outside** any SQLite write transaction.
2. In one short local transaction, re-check stored hashes, insert `reorg_event`, orphan affected observations/segments, rewind affected cursors, recompute status/admission, and bump `registry_revision`.
3. Commit, then rescan affected ranges in separate transactions.
4. Readiness views automatically exclude orphaned quote probes. Pool entities and historical evidence remain retained.

### 7.6 Inventory writer and Live reader runtime

| Role | Responsibilities |
|---|---|
| **Inventory writer** | Discovery, verification, classification, status recomputation, admission evaluation, archive/retention jobs, and dashboard enrichment that writes SQLite. Owns all authoritative writes. |
| **Live reader** | Read-only SQLite connection. Polls `registry_revision`, loads changed records/readiness scopes, stages RPC initialization outside the manager lock, and applies atomic manager generations. Does not write inventory tables. |

Live refresh:

- Poll every `live_poll_ms` (existing default 2 seconds) and immediately after a new canonical head.
- Read `registry_revision > last_applied_revision` in a consistent snapshot.
- Deduplicate by canonical `PoolKey`, not synthetic address.
- Reinitialize only when identity, tokens, parameters, capability scope, or state block changed.
- Stage state fetches outside the manager lock; apply adds/removals/reinitializations as one generation.
- Rebuild token indexes and pair graph once per generation; invalidate detector caches from older generations.
- On DB failure, keep the last good manager marked stale. On cursor gap/restart, reconcile from the current admission snapshot.
- A non-empty cache never suppresses discovery or refresh. Discovery cadence is independently configurable.

The existing per-connection `Arc<Mutex<Connection>>` remains; WAL and short transactions handle cross-process reads. Concurrent dashboard/discovery writers must serialize through the same repository/busy-timeout policy rather than maintaining a second registry.

### 7.7 Configurable policy values

Tunable values must not block implementation once their semantics are fixed.

| Setting | Proposed home | Default policy direction |
|---|---|---|
| `discover.min_tvl` | `[discover]` | Ranking/display only; never a Master deletion rule. Default `0` for persistence. |
| `discover.max_pools` | `[discover]` | `0` means unlimited Master inventory. Per-source page/time budgets remain configurable. |
| Source page/request/time budgets | `[discover]` | Conservative existing limits, reported whenever reached. |
| `live.registry_refresh_ms` | `[live]` | Reuse `live_poll_ms` default 2000. |
| Discovery cadence | `[live]` or scheduler config | Configurable; initial conservative value may be tuned after measurement. |
| `quote_max_age_ms`, `quote_max_lag_blocks` | `[live]` / `[backtest]` | Separate Live and exact-block backtest policies. |
| `max_live_pools`, `max_graph_edges`, `max_pools_per_token` | `[live]` | Explicit budgets; `0` may mean unlimited only where safe. |
| `max_rpc_calls_per_refresh`, `max_refresh_ms`, `max_pending_inits` | `[live]` | Observable backpressure limits. |
| `max_manager_memory_mb` | `[live]` | Measured guard with explicit deferral reasons. |
| Admission weights and coverage reserves | `[live]` | Versioned policy; start simple and tune from benchmark data. |
| Observation/archive retention | `[discover]` or cache policy | Configurable; archival precedes purge. |
| Reorg confirmation depth | `[live]` / chain policy | Reuse existing confirmation policy where practical. |

Numeric thresholds, weights, retention periods, and cadence are **configurable defaults**, not architectural blockers.

---

## 8. Recommended source-adapter architecture

| Rule | Implementation detail |
|---|---|
| Result isolation | `SourceResult` returned even on partial failure; orchestrator merges successes and records failures. No `Vec::new()` on a sibling error. |
| Completion key | `chain + adapter/factory/manager/vault/registry + scan_type + requested range`. No chain-level watermark is authoritative. |
| Window states | `pending`, `running`, `complete`, `partial`, `failed`; `running` is transient. A window is complete only when every requested subrange/page succeeds without detected truncation. |
| Coverage class | `exhaustive`, `bounded`, `provider_limited`, or `unknown`. A documented top-N request may be complete but still not prove chain-wide completeness. |
| Remote request shape | `request_kind` is `block_range`, `time_range`, `cursor`, or `snapshot`. Cursor/page and snapshot requests do not require fabricated start/end timestamps. |
| Budgets | Per-adapter request/page/time caps configurable; page loops driven by raw response length, not post-filter length. |
| Provenance | Adapter name + endpoint + fetch time + block/timestamp + raw fields stored per observation. |
| Identity resolution | Adapter emits canonical identity + raw identifiers; ambiguous label ⇒ `Unknown` in Master, never V2. |
| Incremental / reorg-aware | Store per-source cursor and canonical block hash; re-fetch overlapping windows; invalidate observations whose block is no longer canonical. |
| Failure taxonomy | `rate_limit`, `timeout`, `range_limit`, `auth`, `bad_schema`, `provider_error`, `cancelled`; 429 must degrade gracefully (observed live). |
| Truncation | Explicit provider flags/errors are recorded. A silent successful `getLogs` truncation is `unknown` unless split-window or independent factory/receipt evidence corroborates completeness. |
| Reorg boundary | Common-ancestor discovery uses network calls outside SQLite transactions. Local invalidation/cursor/status changes commit atomically; rescanning happens separately. |
| Testability | Every adapter has wiremock tests for page 1, empty page, 429, malformed row, duplicate identity, unknown TVL, cursor continuation, snapshot requests, and partial failure isolation. |

---

## 9. Benchmark methodology

### 9.1 Ground Truth tiers and practical benchmark matrix

Ground Truth must be independent of the detector being measured. Use the strongest obtainable source and label its completeness explicitly.

| Tier | Reference construction | Permitted claim |
|---|---|---|
| A | Canonical factory/registry/Vault/manager enumeration for the configured scope and frozen block range | Complete for the configured factories/registries only |
| B | Verified official API/subgraph with recorded pagination, caps, freshness, and identity fields | Conditional on the provider's documented subset |
| C | GeckoTerminal/DexScreener or other ranked aggregator | Candidate overlap only; never “true recall” |
| D | Landed historical transactions/receipts | Realism/precision validation; not exhaustive missed-opportunity recall |

Practical chain/DEX matrix:

| Chain | Primary Ground Truth | Secondary cross-check | Expected completeness |
|---|---|---|---|
| Ethereum | Configured V2/V3/V4 factories, Curve registry/factories, Balancer V2 Vault | Verified native API/subgraph where available | A for configured on-chain scope; B/C elsewhere |
| BSC | Pancake V2/V3 factories, Infinity CL manager, Metric factory | GeckoTerminal per-DEX | A for configured scope; Infinity type semantics verified in Phase 1 |
| Polygon | QuickSwap V2/V3, Uniswap V3/V4, Balancer V2 Vault | Curve API where available | A for configured scope |
| Avalanche | Pangolin/LFJ/Blackhole factories, Uniswap V3/V4, Balancer V2 Vault, Metric | Frozen existing benchmark range plus per-DEX aggregators | A for configured scope |
| Arbitrum | Camelot/LFJ factories, Uniswap V3/V4, Curve, Balancer V2, Pendle, Fluid, Metric | GeckoTerminal per-DEX | A for configured scope |
| Base | Aerodrome/Slipstream, Uniswap V2/V3/V4, Pancake V3 | GeckoTerminal per-DEX | A for configured scope; Infinity Base remains C until a manager is configured |
| Optimism | Velodrome V2/Slipstream, Uniswap V3/V4, Curve, Balancer V2, Metric | GeckoTerminal per-DEX | A for configured scope |

Benchmark windows:

- default `W1`: 1,000 canonical blocks per chain;
- Avalanche frozen regression: `99833048..99836394` with the existing 47/47 recall invariant where applicable;
- optional `W2`: 7 days, only when the reference source can prove its coverage;
- no 30-day run without explicit approval.

Each reference artifact records chain, DEX/family, source, snapshot block/time, pagination/caps, eligibility rules, canonical identity fields, and completeness tier.

### 9.2 Benchmark A — pool-universe coverage

For each matrix row, compare canonical identities rather than `(chain, project, symbol)`.

| Chain | DEX/family | Reference tier | Reference pools | Matched | Missing | Additional | TVL-weighted coverage | Complete for scope? |
|---|---|---|---:|---:|---:|---:|---:|---|

Metrics:

- **Pool recall** = matched eligible reference pools / all eligible reference pools.
- **TVL-weighted coverage** = TVL(matched reference pools) / TVL(all eligible reference pools).
- **Quoteable / Simulation-ready / Execution-ready coverage** are reported separately.
- **Unique incremental yield** = new unique verified pools contributed by source *i* after sources 1..i−1.
- **Source overlap** = pairwise Jaccard of canonical identities, per chain and family.
- **Completeness** is never inferred from raw candidate count or aggregator rank.

### 9.3 Benchmark B — opportunity recall and quality

Freeze identical block ranges, state snapshots, capital assumptions, token prices, gas costs, protocol fees, slippage, detector versions, and RPC budgets for every compared version.

Construct the reference opportunity set with an independent path enumerator and simulator over the Tier A/B inventory. Do not define Ground Truth as the union of candidates emitted by the two detectors under test.

Report separately:

- pool recall;
- candidate/path recall;
- quote success rate;
- simulation success rate;
- profitable opportunity rate;
- simulated net PnL and realized landed PnL;
- false-positive rate;
- RPC calls, bytes, retries, runtime, and peak memory;
- Quoteable, Simulation-ready, and Execution-ready scope counts;
- per-chain/family misses and provider-limit caveats.

If only Tier C/D evidence exists, label the result “candidate overlap” or “landed-opportunity precision,” not true recall.

### 9.4 Existing baseline

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
| Tasks | Map data flow; enumerate sources/factories; probe official APIs; record defects and stale docs; execute the complete proposed SQLite schema and fixture/rejection tests against bundled SQLite 3.45.0. |
| Tests | Temporary complete-schema fixture, compatibility/readiness views, composite-FK checks, scoped admission, quote invalidation, and intentional constraint rejections. |
| Expected output | This document with cited findings, coverage matrix, validated schema contract, and corrected implementation phases. |
| Acceptance criteria | Every important claim cited; unverified sources labelled; complete DDL executes with `integrity_check=ok` and empty `foreign_key_check`; no repository database is migrated. |
| Performance / RPC cost | Live HTTP probes and local in-memory SQLite only; no production chain RPC or database writes. |
| Status | **Complete.** |

---

### Phase 1 — DEX/source coverage matrix and reference benchmark

| Item | Detail |
|---|---|
| Files affected | New `docs/POOL_SOURCE_MATRIX.md`; new `core/examples/pool_reference_bench.rs`; new `bench/reference/*.json`; `core/src/pool/discovery/remote/mod.rs` (source metadata struct). |
| Design | `SourceDescriptor { provider, chain, endpoint, pagination, completeness, limits, freshness_field, identity_field }`. Reference inventories are versioned by snapshot block/time and eligibility rules. |
| Tasks | 1) Verify official API/subgraph/pool-list endpoints per chain/DEX. 2) Record rate limits, page sizes, hard caps, missing-field frequency, freshness. 3) Verify Metric/Fluid native-ID semantics, Infinity pool-type tags/manager-vault roles, and explicitly decide whether Balancer V3 remains out of scope. 4) Build the §9.1 chain/DEX Ground Truth matrix. 5) Implement coverage metrics from §9.2. 6) Label every reference with Tier A/B/C/D completeness. |
| Tests | Wiremock fixtures per reference source; pagination-limit tests; fixture-based coverage calculation tests; identity-resolution fixtures for Metric/Fluid/Infinity/Balancer. |
| Expected output | Per-chain/DEX table: reference tier, reference pools, matched/missing/additional, TVL coverage, readiness coverage, completeness label; incremental-yield and overlap tables. |
| Acceptance criteria | Every obtainable major DEX has a labelled reference; no endpoint or count is asserted without evidence; unresolved identity semantics remain provisional rather than guessed. |
| Performance / RPC cost | HTTP only; budget 1–3 req/s per provider; GeckoTerminal observed 429 at ~7 rapid requests. |

---

### Phase 2 — Inventory and ranking architecture corrections

| Item | Detail |
|---|---|
| Files affected | `core/src/cache/store/{mod,pools}.rs`; `core/src/pool/discovery/mod.rs`; `core/src/jobs/{discover,index,run}.rs`; `core/src/pipeline/runner.rs`; `core/src/config/settings.rs`; `cli/examples/cacheq.rs`; discovery/cache/e2e tests. |
| Design | One authoritative Master Inventory with canonical identity, append-only evidence, current projections, scoped capabilities/admission, and read-only compatibility/readiness views. No default global Master cap. |
| Tasks | 1) Repair migration versioning and structural preflight. 2) Add the §7.2 v14 tables, composite FKs, indexes, triggers, views, and archives. 3) Implement canonical `PoolKey`/`PoolRecord` and family-independent identity resolution. 4) Replace `INSERT OR REPLACE` and first-wins cache merge with observation-based transactions. 5) Retain unresolved/zero-token records instead of dropping them. 6) Implement `put_discovered_pool` as a compatibility adapter and add full Master APIs. 7) Migrate discovery/index/run readers and writers; keep `pool_dex` as optional enrichment. 8) Make `pool_info`/`pool_info_legacy_complete` read-only with `INSTEAD OF` triggers. 9) Remove default Master `max_pools` truncation; make `min_tvl` ranking-only. 10) Fix decoder identity/guard defects and the corrected `factory_pools.drain()` association bug. 11) Add configurable policy fields with conservative defaults. |
| Tests | Complete-schema fixture/rejection suite; real v13 copy upgrades and rollback; unknown-TVL retention; singleton/full-PoolId round-trip; unresolved observation storage; cross-pool evidence rejection; compatibility view write rejection; old reader/writer smoke tests; `foreign_key_check`/`integrity_check`; crash/reopen/idempotency. |
| Expected output | Legacy databases upgrade without data loss; every discovered entity is retained; singleton identities survive; existing address-based callers continue through compatibility APIs; Master and readiness datasets are distinct. |
| Acceptance criteria | No Master deletion for unknown TVL, unsupported family, or zero reserves; no direct writes to compatibility views; every legacy row is archived and linked; all required identity fields round-trip; migration/rollback tests pass on sanitized v13 copies. |
| Performance / RPC cost | No extra chain RPC. Batch observation writes; index `(chain_id, identity_namespace, canonical_identifier)`, `(source_id, dedup_key)`, evidence composite keys, and `registry_revision`. |

---

### Phase 3 — New source adapters and pagination

| Item | Detail |
|---|---|
| Files affected | `core/src/pool/discovery/remote/mod.rs` (trait/registry), `remote/geckoterminal.rs`, `remote/dexscreener.rs`, new `remote/official_api.rs`, `remote/subgraph.rs`, `remote/registry.rs`; `core/src/jobs/discover.rs` (watermark fix). |
| Design | Per-adapter `SourceResult` with request kind, completeness, errors, and cost; shared rate limiter; raw-page pagination; per-source/type/range cursors; no cross-source result loss. |
| Tasks | 1) Introduce adapter trait/registry and `discovery_source`/`source_run` records. 2) Implement `scan_window`/`scan_attempt`/`scan_segment`/`scan_cursor` for block, time-range, cursor/page, and snapshot requests. 3) Fix GT raw-page pagination and per-DEX fallback result loss. 4) Expand DEX enumeration with explicit budgets. 5) Add DexScreener per-token/pair enrichment. 6) Add verified official API/subgraph adapters. 7) Add DefiLlama only as unverified/exact-match market enrichment, never identity. 8) Replace global watermark advancement with per-source contiguous complete segments. 9) Implement reorg detection with network ancestor discovery outside SQLite and short local invalidation transactions. |
| Tests | Wiremock: low-TVL page continuation, 429/backoff, partial failure isolation, duplicate identity, malformed row, cursor resume, snapshot request, silent-truncation `unknown`, watermark not advanced on failure, and reorg transaction boundaries. |
| Expected output | Union ≥ any single source; every requested window has a status and coverage class; one adapter failure cannot zero others; failed ranges remain retryable. |
| Acceptance criteria | No failed/partial window is marked complete; no chain-level completion is inferred from another factory; every cursor advancement is backed by contiguous complete segments. |
| Performance / RPC cost | Shared provider limiter; per-adapter budgets are configurable and reported. Avoid 30-day scans without explicit approval. |

---

### Phase 4 — Active / liquidity / TVL validation

| Item | Detail |
|---|---|
| Files affected | `core/src/pool/discovery/{health,validation,metadata}.rs`; `core/src/pool/state/{factory,pool_types}.rs`; `core/src/cache/store/{mod,pools}.rs`; protocol math/selector modules as needed. |
| Design | Independent, evidence-backed status predicates; protocol-aware liquidity/TVL; route-specific quote probes; scoped capabilities; current `pool_status.status_revision`. |
| Tasks | 1) Implement protocol-correct state/liquidity probes for V2, V3/V4, Curve, Balancer, LB, Infinity, Pendle; Metric/Fluid remain unsupported until verified. 2) Fix Balancer `pool_id` hydration and V4 manager+PoolId state reads. 3) Separate third-party reported TVL from on-chain calculated TVL with token price provenance/freshness. 4) Add recent-activity evidence. 5) Implement `pool_quote_probe` and quote freshness rules. 6) Compute Quoteable and Simulation-ready views; do not require execution support for either. 7) Increment status revision and suspend stale admissions atomically. 8) Keep zero-liquidity/unknown-TVL entities in Master. |
| Tests | Per-family state/liquidity fixtures; zero-liquidity retention; TVL source/timestamp/price freshness; Balancer/V4 singleton hydration; multiple quote directions/amounts/blocks; stale/orphaned probe invalidation; status-revision admission invalidation; capability dependency tests. |
| Expected output | Every status/TVL/quote claim has anchored evidence; Quoteable and Simulation-ready scopes are measurable independently of execution. |
| Acceptance criteria | No generic non-zero-reserve test; no ambiguous external TVL match; no reported TVL without provenance; no global quoteability claim across unsupported routes. |
| Performance / RPC cost | Batch `eth_call` through Multicall3 where possible; bound concurrency; reuse canonical state observations; quote cadence/freshness are configurable. |

---

### Phase 5 — Live and PoolManager integration

| Item | Detail |
|---|---|
| Files affected | `core/src/pipeline/runner.rs`; `core/src/pool/state/{manager,factory,pool_types,apply}.rs`; `core/src/jobs/{index,live,run}.rs`; `cli/src/commands/live.rs`; `core/src/mev/detectors/{two_hop,multi_hop}.rs`; `core/src/types/opportunity.rs`. |
| Design | One inventory writer and a read-only Live reader. Live refreshes from `registry_revision`, initializes canonical `PoolKey` records incrementally, and consumes Execution-ready scopes only for order building. |
| Tasks | 1) Replace address-only manager/pair/opportunity keys with canonical pool references while preserving legacy address serialization. 2) Route V4/Balancer/Infinity singleton-emitted logs by full PoolId. 3) Implement scoped capability and admission evaluation with explicit deferral reasons. 4) Add Live registry polling on `live_poll_ms` and canonical-head triggers. 5) Stage RPC initialization outside the manager lock; apply atomic manager generations and graph rebuilds. 6) Deduplicate initialized pools by canonical key/state revision. 7) Suspend drained/invalid/orphaned scopes and automatically re-admit after fresh verification. 8) Remove the non-empty-cache bootstrap guard; discovery cadence is configurable. 9) Separate two-hop pool limits from multi-hop graph/cycle budgets and report all pruning. 10) Enforce configurable RPC, memory, graph, and refresh budgets without silent truncation. |
| Tests | Live refresh with a non-empty stale cache; incremental add/update/remove; restart/cursor-gap recovery; singleton replay; scoped admission changes; quote/status/reorg invalidation; manager generation/cache invalidation; budget/deferral reporting; large-universe pair graph tests. |
| Expected output | Newly verified pools enter a running Live process without restart; unsupported pools remain in Master but never execute; every deferred scope has a reason. |
| Acceptance criteria | Quoteable/Simulation-ready/Execution-ready counts are reported separately; no unsupported pool reaches the executor; Live performs no authoritative SQLite writes; refresh survives partial failures. |
| Performance / RPC cost | State initialization is bounded by admitted scopes and configurable concurrency. Reuse canonical state observations and avoid reinitializing unchanged pools. |

---

### Phase 6 — Controlled benchmark and regression testing

| Item | Detail |
|---|---|
| Files affected | `core/examples/pool_discovery_bench.rs`, new `core/examples/opportunity_recall_bench.rs`, new `docs/benchmarks/pool-coverage-*.md`, new `docs/benchmarks/opportunity-recall-*.md`. |
| Design | Benchmark A (pool universe/readiness coverage) and Benchmark B (opportunity recall/quality) as defined in §9, using identical frozen snapshots and independent Ground Truth. |
| Tasks | 1) Ingest Tier A/B/C/D reference inventories and diff by canonical identity. 2) Compute pool, TVL-weighted, Quoteable, Simulation-ready, and Execution-ready coverage. 3) Compute incremental yield and source overlap. 4) Add independent reference path enumeration/simulation for opportunity recall. 5) Record quote/simulation success, profitable rate, net PnL, false positives, RPC calls/bytes/retries, runtime, and memory. 6) Add regressions for CLI→SQLite→Live, migration/rollback, scanner/Live reorg, failed-getLogs coverage, singleton identity, compatibility APIs, and Master retention versus readiness removal. |
| Tests | Full discovery/job/state test suite; frozen AVAX regression; before/after JSON artifacts; reference completeness and metric-denominator tests. |
| Expected output | Before/after tables for both benchmarks, with reference tier and provider-limit caveats. |
| Acceptance criteria | Coverage meets each obtainable reference scope or explains the gap; opportunity recall is non-decreasing within the declared Ground Truth; false positives and resource regressions are explained. |
| Performance / RPC cost | Document exact budgets. Default W1 is 1,000 blocks; no 30-day receipt scan without explicit approval. |

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
| `cache/store/mod.rs` migration comments imply 13 logical versions | SQL strings are enumerated individually and `schema_version` is never seeded; structural preflight is required. |
| `cli/examples/cacheq.rs` reads `pool_info` directly | v14 must read `pool_info_legacy_complete` or Master views; `pool_info` may contain NULLs. |
| `pool/state/factory.rs:936-975` implies Balancer/V4 state init is generally available | Balancer loses `pool_id`; V4 reads a synthetic address. Effective capability remains false until fixed. |

### 11.2 Existing tests that pin current behaviour

- `jobs/discover.rs:781-955`: watermark gaps, cache-first persistence, **ranking drops unknown TVL** (must be updated in Phase 2).
- `discovery/mod.rs:2145-2492`: V4/Infinity decoding, LFJ selectors, Pendle override, `merge_from` conflicts, receipt decoder coverage.
- `remote/mod.rs:545-676`: merge dedup, curated slugs, DefiLlama rejection, strict inference.
- `remote/geckoterminal.rs:440-763`: parse, min-TVL, network slug, token order, sort param, TVL sort, unknown-TVL retention, dex classification.
- `remote/dexscreener.rs:250-342`: hub profile, filter/canonicalize, label precedence, Infinity mapping, wiremock fetch.
- `core/tests/receipts_batch.rs:65-167`: batched receipts, provider pinning, `failed_windows`.
- `core/src/cache/store/pools.rs:274-332`: positional pool round-trip and sparse merge expectations.
- `core/tests/e2e.rs`: `put_discovered_pool`/`list_discovered_pools` persistence and per-chain DB isolation.
- `core/tests/live_tag_parity.rs:134-197`: `VACUUM INTO` snapshot and registry seeding through compatibility APIs.
- `cli/examples/cacheq.rs`: direct SQL counts over `pool_info`.

### 11.3 Test gaps to close in Phases 2–6

- No v13-copy migration/rollback suite covering structural variants, data preservation, crash recovery, and archive integrity.
- No compatibility-view/API suite proving old address-based readers still work without writing `pool_info`.
- No end-to-end CLI `discover` → SQLite → readiness views → Live `PoolManager` test.
- No scanner/Live reorg integration test with network/local transaction separation.
- No test that a failed `getLogs` window leaves per-source coverage unchanged.
- No identity-keyed V4/Balancer/Infinity survival test through dedup, persistence, replay, and Live init.
- No test distinguishing Master retention from Quoteable, Simulation-ready, and Execution-ready removal.
- No scoped admission/rank/status-revision/quote-probe invalidation suite.
- No resource-budget/deferral observability test.

---

## 12. Definition of success

The final system provides the broadest practically obtainable inventory of unique, verified, active, TVL-bearing pools across all supported chains and DEXes:

- matches or exceeds the eligible pools exposed by DEX-native reference sources (or explicitly documents provider-imposed limits);
- expands beyond them through independent sources;
- preserves canonical identity, multiple source observations, TVL source/timestamp and explicit provenance;
- never deletes a discovered pool from the Master Inventory because TVL is unknown or execution is unsupported;
- migrates existing v13 databases without data loss and keeps legacy readers working through explicit compatibility APIs;
- reports Quoteable, Simulation-ready, and Execution-ready scopes separately and aligns each with actual evidence/capabilities;
- refreshes a running Live process incrementally without treating a non-empty cache as complete;
- makes admission, retention, cadence, freshness, and resource budgets configurable and observable;
- demonstrably increases genuine arbitrage opportunity recall in Benchmark B, not just theoretical pair counts.

### 12.1 Approval gates versus configurable defaults

**Structural decisions requiring explicit approval:**

1. Approve read-only `pool_info` plus `pool_info_legacy_complete` as the compatibility contract, with all writers migrated in the same change.
2. Approve one authoritative inventory writer and a read-only Live process; concurrent dashboard enrichment must serialize through the same writer policy.
3. Confirm Balancer V3 remains out of scope until separately designed and tested.
4. Approve the default Ground Truth matrix and any provider/API-key access needed for Tier B references.

**Safe defaults that do not block implementation:**

- Metric/Fluid remain provisional address identities with native IDs retained until Phase 1 proves singleton semantics.
- Admission weights/thresholds, discovery cadence, quote freshness, retention, page budgets, RPC limits, graph limits, and memory budgets are configurable and tunable after measurement.
- Infinity uses the typed singleton structure now; exact pool-type tags are filled after Phase 1 verification.
- Historical backfill starts from the configured lookback; the legacy watermark is never treated as proof of completeness.

**Recommendation:** the plan is ready for Phase 2 implementation once the four structural choices above are approved. Tunable policy values should not delay the schema/identity/migration work.

---

## 13. Approval log

| Date | Event |
|---|---|
| 2026-11-27 | Audit and plan delivered in chat; no files changed. |
| 2026-11-27 | Addendum delivered after jobs/integration audit; no files changed. |
| 2026-11-27 | This document created at user request. **Implementation of Phase 2+ still awaits approval.** |
| 2026-11-27 | Final SQL/architecture consistency review incorporated: complete DDL validated on bundled SQLite 3.45.0; compatibility, migration, scoped readiness, runtime, Ground Truth, and configuration corrections folded into the existing phases. Documentation only; no application/schema/database changes or commits. |

**Next action:** approve the four structural choices in §12.1 and authorize Phase 2, or specify a different starting phase.
