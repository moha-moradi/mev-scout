# Plan: Pool Discovery Accuracy Audit + Layered Off-Chain Sourcing (Polygon-first)

> Status: approved direction. Scope: **Polygon first**, generalized to the other 6 chains afterward.
> Deliverable: **both** a validation harness (quantified accuracy vs DEX explorers) **and** layered
> off-chain sourcing — The Graph subgraphs primary, free aggregators (GeckoTerminal / DefiLlama) as
> fallback and cross-check. No Dune reintroduction.

---

## 1. Context

### 1.1 Current state of discovery

Discovery is **100% on-chain RPC** (`eth_getLogs` + `eth_call`). Unified entry point
`discover_pools()` at `core/src/pool/discovery/mod.rs:581` combines:

1. **DEX activity scan** — topic-only `eth_getLogs` (V2 Swap/Sync, V3 Swap/Mint/Burn, Curve
   `TokenExchange`, Balancer `Swap`) → finds pools *active* in the scanned window.
2. **Factory creation scan** — address-filtered `PairCreated` / `PoolCreated` / `PoolRegistered` /
   `PoolAdded` / `NewMarket` / `LBPairCreated` / V4 `Initialize` → finds *new* pools with metadata.

Then Phase 2 metadata `eth_call`s (`token0/fee/tickSpacing`, mod.rs:924), symbol resolution via
`TokenCache` (mod.rs:1119), dedup/merge (`merge_from`, mod.rs:188), SQLite persistence
(`discover_and_cache`, mod.rs:1252), and liveness filtering (`health_check_pools`, mod.rs:1306).

The Dune path was fully removed (`docs/plan-remove-dune-onchain.md`, executed 2026-08-19). Dune had
proven unreliable anyway: stale `Mint`/`Burn` decode tables (stopped ~2022-09), `dex.trades`
mislabeling QuickSwap, missing `dex.liquidity` (see `mev_strategies.md:~2919-2950`). A stale,
silently-ignored `dune_api_key` line remains in `mev-scout-arbitrum.toml`.

### 1.2 Key framing: explorers ARE subgraph frontends

Uniswap's own docs state the Explore/analytics pages are powered by Uniswap subgraphs on The Graph
(`developers.uniswap.org/docs/ecosystem/subgraphs/overview`). QuickSwap, Balancer, Curve publish
equivalents. **Therefore querying the right subgraph is effectively querying the same source the
explorer UI renders** — it is the correct ground truth for an accuracy comparison, and the natural
Dune replacement.

Caveat: the free hosted service (`api.thegraph.com`) is deprecated; most subgraphs now live on the
decentralized network behind `gateway.thegraph.com/api/<KEY>/subgraphs/id/<ID>` (API key required).
Some legacy hosted names still respond. Step 0 resolves this per endpoint; aggregators cover the
no-key case.

### 1.3 Known accuracy gaps vs explorer UIs (from code review)

| # | Gap | Evidence | Effect |
|---|-----|----------|--------|
| 1 | **No TVL/volume/USD ranking** | Discovery captures addresses+metadata only; USD pricing lives in the separate CoinGecko oracle | Cannot rank/filter like explorer's TVL / 1D / 30D views |
| 2 | **Windowed coverage, not universe** | Polygon `discovery_start_block = 49_100_000` (~mid-2023); no genesis back-scan | Older dormant-but-funded pools invisible unless they traded in-window |
| 3 | **QuickSwap V3 (Algebra) under-discovered** | Polygon's second "V3" factory `0x08958a…` is Algebra-based. Algebra events differ from canonical V3 (factory emits `Pool(token0,token1,pool)`, not `PoolCreated`; swap/mint/burn topic hashes also differ). We scan it with the canonical V3 topic | Few/no decodes from that factory; activity scan also misses Algebra-only liquidity events. Biggest single Polygon recall bug |
| 4 | **Curve blind spot outside Ethereum** | `chains.toml`: `curve_registry` set only for Ethereum mainnet | On Polygon, Curve pools surface only via `TokenExchange` activity; low-turnover pools missed entirely |
| 5 | **Dust-pool over-reporting** | `health_check_pools` checks non-zero reserves/sqrtPrice only — no USD TVL floor | Explorer suppresses dust; we report it. Intentional for MEV but counts won't match explorer pagination — must be documented, optionally gated by `--min-tvl` |
| 6 | **Fee assumptions** | V2-family defaults to 30 bps (Pancake corrected to 25 via config); Solidly stable/volatile nuance collapsed to one value | Field-level mismatches vs subgraph `feeTier` |
| 7 | Minor | Trader Joe `bin_step` unset at discovery; Balancer Linear/Managed types intentionally skipped (balancer.rs:36); long-tail token symbols resolve lazily | Display parity only |

### 1.4 Off-chain source inventory (to be finalized in Step 0 spike)

| Source | Auth | Notes |
|---|---|---|
| Uniswap V3 Polygon subgraph — decentralized-network ID `BvYimJ6vCLkk63oWZy7WB5cVDTVVMugUAF35RAUZpQXE` via gateway; legacy `api.thegraph.com/subgraphs/name/ianlapham/uniswap-v3-polygon` may still work | key (gateway) | Schema: `pools { id token0 token1 feeTier tickSpacing liquidity totalValueLockedUSD volumeUSD txCount createdAtBlockNumber }` + `PoolDayData` for 1D/30D windows; paginate `first:1000, skip:N` |
| QuickSwap official subgraphs — v2 (`QuickSwap-subgraph` repo, `pairs` entity) and V3/Algebra (`QuickSwap/v3-subgraphs` repo; legacy name `sameepsi/quickswap-v3`) | varies | Critical for gap #3 |
| Balancer V2 Polygon, Curve Polygon subgraphs | key (gateway) | Fixes gaps #4; Balancer entity carries `tokens[]`, Curve carries `coins[]` |
| Uniswap interface gateway `interface.gateway.uniswap.org/v2/uniswap.explore.pools` (undocumented) | none known | Optional direct-explorer reference for the harness; best-effort |
| GeckoTerminal API `GET /api/v2/networks/polygon_pos/pools?sort=h_tvl|volume_usd` | none (attribution requested) | Top-N across ALL DEXs with dex label, TVL, volume — mirrors explorer rankings; ~30 req/min free |
| DefiLlama `GET api.llama.fi/pools` (filter chain=Polygon), `/overview/dexs/polygon` | none | Pool TVL/APY; whitelisted pools only, no fee/tickSpacing → cross-check tier only |
| Goldsky mirrors (`api.goldsky.com/api/public/project_…/subgraphs/…`) | none/varies | Alternate host failover list per DEX |

---

## 2. Design

### 2.1 Module layout (merges both prior proposals; schema-kind adapters, not one client per DEX)

```
core/src/pool/discovery/
├── mod.rs                  # existing RPC path — unchanged behavior when --source onchain
├── remote/
│   ├── mod.rs              # trait PoolSource, RemotePool, orchestrator discover_via_remote(),
│   │                       #   merge into DiscoveredPool, reachability probe + graceful skip
│   ├── graphql.rs          # GraphClient: POST {"query":…} over existing reqwest (json feature),
│   │                       #   first/skip pagination, retry+backoff on 429/"throttled"/502 reusing
│   │                       #   RateLimiter pattern from core/src/rpc/middleware.rs, 15s timeout,
│   │                       #   ${GRAPH_API_KEY} template expansion, multi-URL failover in order
│   ├── schemas.rs          # enum SubgraphSchema { UniswapV2, UniswapV3, Algebra, BalancerV2, Curve }
│   │                       #   with query() + parse() per kind (V4 reuses V3 shape);
│   │                       #   From<RemotePool> for DiscoveredPool (symbols from subgraph)
│   ├── geckoterminal.rs    # REST, paginated top pools, dex-name normalization
│   └── defillama.rs        # REST, chain-filtered pool list
core/data/chains.toml       # [chains.polygon.subgraphs] entries: { dex_name, dex_type, schema,
                            #   urls = [gateway, hosted, goldsky…] } — tried in order
```

Registry precedence: `chains.toml` overrides → `ChainName::default_subgraphs()` hardcoded fallbacks
in `core/src/types/chain.rs` (mirrors existing `default_*_factories()` pattern). New config fields
`#[serde(default)]` — backward compatible.

Auth: `GRAPH_API_KEY` env var overrides TOML value; never logged or committed. No key ⇒ gateway URLs
skipped, hosted/aggregator URLs still tried; if everything fails ⇒ warn + fall back to pure RPC
(discover never hard-fails because of remote sources).

### 2.2 CLI changes

`DiscoverArgs` (cli/src/cli.rs:270) additions:

- `--source onchain|remote|hybrid` — default `onchain` (zero behavior change). `hybrid` = union of
  RPC + remote, deduped by address via `merge_from`.
- `--enrich` — attach `tvl_usd`, `volume_usd_24h`, `volume_usd_30d` to discovered pools from best
  available source (subgraph preferred, aggregator fallback).
- `--min-tvl <USD>` (default 0 = full parity), `--max-pools` (pagination cap).

New verb `validate-pools --chain polygon [--days N] [--source all|subgraph|gecko|llama] [--json]`
(cli/src/commands/validate_pools.rs):

- Runs on-chain discovery over the window → set **A**; fetches reference sets → set **B** per source.
- Reports: **recall** `|A∩B|/|B|` overall + per-DEX (vs explorer-top-N semantics), **false positives**
  (A∖B failing health check), **field mismatches** (`fee`, `token0`, `token1` A-vs-B diffs),
  **metric parity** (TVL/volume deltas on overlap).
- Output: comfy-table console report + JSON + optional markdown file. This is the regression gate
  re-run after integration.

### 2.3 Data-model & storage

- Add nullable `tvl_usd`, `volume_usd_24h`, `volume_usd_30d` to `DiscoveredPool` +
  SQLite migration **v10** (nullable columns; backfilled by `--enrich`). Resolves the
  display-vs-persist question in favor of persistence since ranking is a first-class goal.
- Subgraph-sourced pools get `creation_block = 0` when the subgraph omits it ⇒ **incremental mode
  stays RPC-only** (it relies on `max_creation_block`); optionally estimate blocks from
  `createdAtTimestamp` via `timing.rs::estimate_block_for_timestamp` later.
- Remote-seeded pools always pass through `health_check_pools` (subgraph TVL can be stale; drained
  pools get pruned) and later through normal on-chain state init — RPC remains ground truth for
  simulation; remote data is candidate generation + ranking only.

### 2.4 Bug fix bundled in: Polygon Algebra decoding (gap #3)

Add Algebra-specific handling so QuickSwap V3 decodes correctly:
factory creation event `Pool(token0, token1, pool)` mapping, plus Algebra swap/mint/burn topics for
the activity scan. Exact hashes verified against the live contract in Step 0. Expected to materially
raise Polygon recall on its own.

---

## 3. Implementation phases

### Phase 0 — Live endpoint spike + baseline audit (read-only)

- [ ] Probe each candidate endpoint (gateway w/ key, legacy hosted, QuickSwap official, Goldsky,
      GeckoTerminal, DefiLlama, Uniswap interface gateway). Record working URL + auth needs directly
      into `chains.toml` comments. Verify Algebra event hashes from the deployed factory.
- [ ] Baseline run: `discover --chain polygon` over a fixed recent window (default last ~2M blocks;
      full-range audit opt-in — full 49M→tip is hours on public RPC) → snapshot A.
- [ ] Snapshot reference sets B (subgraphs top-1000 by TVL; GeckoTerminal top pools; optional UI
      capture of app.uniswap.org/explore/pools/polygon + dapp.quickswap.exchange for 1:1 eyeballing).
- [ ] Deliverable: `docs/pool-discovery-accuracy.md` — quantified recall/precision/metric-delta
      tables answering "does discovery match the explorers", including window sensitivity
      (7d vs 30d vs 2M-blocks) and explicit notes on intentional divergences (#5, Balancer skips).

### Phase 1 — Remote layer skeleton

- [ ] `remote/graphql.rs` GraphClient (pagination, retries, rate-limit backoff, key templating,
      multi-URL failover) + `remote/schemas.rs` adapters for `UniswapV3` and `Algebra` first.
- [ ] Unit tests with recorded GraphQL JSON fixtures via `wiremock` dev-dependency (offline CI).
- [ ] `chains.toml` `[chains.polygon.subgraphs]` + `ChainConfig.subgraphs` +
      `ChainName::default_subgraphs()`; env-var expansion.
- [ ] `discover --source remote` path printing pools end-to-end (manual smoke vs Phase 0 numbers).

### Phase 2 — Full coverage + aggregators

- [ ] Remaining schema adapters: `UniswapV2`, `BalancerV2` (tokens[]→underlying_tokens),
      `Curve` (coins[]) — fixes Polygon Curve blind spot without a registry.
- [ ] `geckoterminal.rs` + `defillama.rs` clients (fallback tier + cross-check inputs for
      validate-pools).
- [ ] `DiscoveredPool` enrichment fields + SQLite migration v10; `--enrich` wiring.
- [ ] Algebra creation/activity fix (§2.4) landed and covered by fixture tests.

### Phase 3 — Hybrid integration + validation harness

- [ ] `--source hybrid` union flow through `merge_from`; health check applied to all sources;
      graceful degradation ladder (subgraph → aggregator → RPC-only warn).
- [ ] `validate-pools` verb implemented; run before/after comparison; update accuracy doc with
      post-fix numbers (expect large recall gain from Algebra fix + seeding; near-parity vs
      subgraph reference in hybrid mode).
- [ ] Progress bars for pagination (indicatif, already a dep).

### Phase 4 — Tests, docs, cleanup

- [ ] Integration test (network-gated): combined coverage > onchain-only coverage on a small
      Polygon window.
- [ ] E2E case `e2e_subgraph_discovery` tagged `#[ignore]` (requires `GRAPH_API_KEY`): fetch ≥10
      pools, assert token0 ≠ ZERO.
- [ ] Performance note in docs: measure `eth_getLogs` call reduction (expected 10–50× fewer RPC
      calls when sourcing remotely).
- [ ] Remove stale `dune_api_key` from `mev-scout-arbitrum.toml`. Update README/config examples.
- [ ] Generalization checklist for remaining 6 chains (appendix of the doc).

---

## 4. Verification

1. `cargo build --workspace` · `cargo clippy --workspace` · `cargo test --workspace`
   (fixture-based unit tests offline; wiremock for GraphQL parsing/pagination/failover).
2. Regression guard (no key needed):
   `cargo run -p mev-scout-cli -- discover --chain polygon --days 2 --health-check --json` —
   identical behavior to today under default `--source onchain`.
3. Remote smoke: `GRAPH_API_KEY=… discover --chain polygon --source remote --max-pools 100 --json`
   → 100 pools sorted by TVL, symbols populated without extra `eth_call`.
4. Harness: `validate-pools --chain polygon` before/after — recall improvement documented in
   `docs/pool-discovery-accuracy.md`.
5. Existing e2e (`core/tests/e2e.rs:315`) stays green; `rg -i dune` remains clean.

## 5. Risks & mitigations

| Risk | Mitigation |
|---|---|
| Hosted-service endpoints dead / gateway requires key | Step 0 decides per endpoint; config-driven URL lists (gateway → hosted → Goldsky) tried in order; aggregators cover no-key mode; RPC-only fallback never fails |
| Subgraph lag (5–15 min) vs head | Documented; `live` mode keeps using RPC logs; hybrid union means freshest pools still arrive via RPC |
| Stale/incorrect remote metadata | Health check + on-chain state init remain ground truth; remote data only seeds/ranks candidates |
| Niche forks absent from subgraphs (DFYN, Meshswap, ApeSwap…) | RPC activity scan still catches them; gap noted per-DEX in accuracy doc |
| Rate limits (GeckoTerminal ~30/min, gateway free tier) | Reuse RateLimiter middleware pattern; bounded concurrency; pagination caps |
| `creation_block` missing for remote pools | Set 0; incremental mode documented as RPC-only; optional timestamp→block estimation later |

## 6. Decisions (resolved open questions)

1. **Key hosting:** `GRAPH_API_KEY` env overrides TOML; env never committed.
2. **TVL filter default:** `0` (full parity); `--min-tvl` opt-in to mimic explorer suppression.
3. **Metric persistence:** persist nullable tvl/volume columns (migration v10), not display-only.
4. **Fallback hosts:** config allows multiple URLs per DEX, tried in order (Graph gateway → hosted →
   Goldsky → aggregators → RPC).
5. **Audit scope:** default last ~2M blocks for speed; full-range audit opt-in flag.
