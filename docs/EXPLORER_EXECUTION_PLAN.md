# MEV Explorer — Execution Plan (Polygon-first, self-built)

> Status: DRAFT — Owner: mev-scout team — Last updated: 2026-09-05
>
> Goal: build our own MEV explorer for Polygon (then other EVM chains), comparable in
> surface to https://explorer.mev.zone (Avalanche), **without using any third-party MEV
> API** (no EigenPhi, no mev.zone API). All data is reconstructed from raw chain data
> via RPC. Primary deliverable: a set of `mev-scout explorer` CLI commands that
> (a) index MEV operations from blocks, (b) render a live feed + historical stats,
> and (c) cross-validate against our existing `run` / `live` opportunity scanner.

---

## 0. Goals & Non-Goals

**Goals**
1. Self-developed MEV detection/explorer pipeline, end-to-end, from RPC data.
2. `explorer live`: mev.zone-style live feed of extracted MEV ops (time, token,
   route, profit, sender) — comparable to `/mevlive`.
3. Historical sections: last 1d / 7d / 30d aggregates, daily breakdown, leaderboards
   (senders, tokens, pools), per-tx operation detail.
4. Cross-validation harness: compare "what was actually extracted on-chain" (explorer
   ground truth) vs "what `run`/`live` would have detected" (our opportunity scanner).
5. Full transparency of methodology (detection rules, confidence levels, pricing).

**Non-Goals (v1)**
- No mempool/pre-trade detection (already covered separately by `detectors/mempool.rs`).
- No web frontend in v1 (CLI only; web UI is Phase 6, optional).
- No CEX-DEX (non-atomic) profit attribution in v1 — methodologically opaque; listed
  as `unknown`/excluded. Revisit in v2.
- No multi-chain in v1 — Polygon (chain 137) only; architecture must stay chain-generic
  (the `ChainName` enum already covers 7 chains).

---

## 1. Background: how explorer.mev.zone works, and why ours is different

- mev.zone is the explorer of the **Avalanche MEV Zone auction**: searchers submit
  bundles with bids → builders assemble candidate blocks → validators pick the best
  reward+burn block. Because the MEV Zone operator **sees the auction**, its explorer
  knows exact routes, profits, and senders without inference.
- Polygon PoS has **no bundle/relay layer**. Ordering is Bor block producers +
  priority-fee auctions + private RPCs. There is no bid/bundle stream to tap.
- Therefore our explorer is a **forensic reconstruction**: replay blocks, decode swaps
  and transfers, classify patterns, and attribute profit from token balance deltas.
  Every number is "attributed MEV" with an explicit confidence level. This is the same
  model used by EigenPhi, libMEV, and (historically) Flashbots' `mev-inspect-py`.

**Taxonomy we will support (v1)**

| Kind | Definition | Confidence |
|---|---|---|
| `arb_atomic` | Single tx whose token-flow graph closes a cycle (≥1 or ≥2 pools) ending in the start token; residual = gross profit | `exact` |
| `sandwich` | Same sender performs two opposite-direction swaps on the same pool within one block, with a third-party swap between them | `exact` |
| `liquidation` | Known liquidation events (Aave `LiquidationCall`, Morpho, …); profit = seized − repaid (+ subsequent swap in same tx) | `exact` |
| `jit` | Mint + burn of a concentrated position in the same block around a large swap (fee capture) | `exact` |
| `jit_arb` | JIT + follow-up arb in same tx/block | `exact` |
| `unknown` | Profitable pattern not matching any rule (incl. probable CEX-DEX bots) | `inferred` |

---

## 2. Codebase assets we reuse (map)

| Explorer need | Existing module |
|---|---|
| RPC fan-out, RPS limiting, retries | `core/src/rpc/` (client, middleware, multicall, consts), `cli/src/rpc_setup.rs` |
| Config (chain, providers, output fmt) | `core/src/config/` + `mev-scout.toml` |
| Chain metadata + factories | `core/src/types/chain.rs` (Polygon: QuickSwap/Sushi/ApeSwap/DFYN/Meshswap V2, Uniswap V3 + QuickSwap V3, chain id 137) |
| Event/topic decoding | `core/src/sigs/` (resolver, downloader), `core/src/pool/decoders.rs` |
| Transfers / trades extraction | `core/src/chain/{transfers,trades,events}.rs` |
| Liquidation sources | `core/src/chain/liquidations.rs`, `core/src/chain/flashloans.rs` |
| MEV detectors | `core/src/mev/detectors/{sandwich,two_hop,multi_hop,liquidation,jit,jit_arb}.rs` |
| Block/state replay | `core/src/replay/` (replayer, db), `core/src/cache/` (redb stores) |
| Gas model | `core/src/mev/gas.rs`, `core/src/pipeline/gas.rs` (`historical_exact`) |
| USD pricing | `core/src/coingecko.rs` |
| Output rendering | `cli/src/display.rs` (table), `output = table|csv|json` config key |
| CLI plumbing | `cli/src/cli.rs`, `cli/src/commands/*` (clap) |

New code lives in: `core/src/explorer/` (indexer, store, attribution, queries) and
`cli/src/commands/explorer.rs` (subcommands).

---

## 3. Architecture

```
RPC providers (mev-scout.toml, rate-limited)
        │  headers + receipts (+ optional traces, Phase 0 decision)
        ▼
explorer::ingest   ── block/tx/receipt fetch, reorg-aware, confirmation lag
        ▼
explorer::decode   ── swap/transfer/liquidation/jit facts  (reuse pool decoders + sigs)
        ▼
explorer::detect   ── arb cycles, sandwiches, JIT …         (reuse + new detectors)
        ▼
explorer::account  ── per-tx token deltas → gross profit → gas → USD → net
        ▼
explorer::store    ── SQLite (WAL) v1 → Postgres/ClickHouse v2 (same schema)
        ▼
explorer::query    ── live / stats / top / show / validate  → display.rs
```

Key design rules:
- **Decode-and-discard**: never persist raw traces/logs; only extracted facts.
- **Idempotent writes**: `(block_number, tx_index, log_index)` primary keys; re-running
  a range must be a no-op.
- **Reorg safety**: index block N only when head ≥ N + `confirmations` (default 6 ≈ 12s);
  on reorg detect (hash mismatch), delete ≥ fork block and re-index.

---

## 4. Data source decision (Phase 0 spike)

**v1 = logs-only** (receipts), which covers ~all v1 taxonomy:
- Swaps: V2/V3/V4 `Swap`, Curve `TokenExchange`, Balancer, Solidly/LB — all emit logs.
- Transfers: ERC20 `Transfer` (+ tx `value` for native POL accounting).
- Liquidations: Aave/Morpho/… events.
- Sandwich/arb structure: derived from swaps + transfers per tx.
- Gas/tips: from receipt `gasUsed`, `effectiveGasPrice`; priority fee = effective − base.

Traces (`debug_traceBlockByNumber` w/ callTracer) are only needed for **internal native
POL transfers** (unwrapped profit) — defer to v2. Verify per-provider support with
`explorer doctor`; Alchemy supports `debug_*` on Polygon (config already has 3 Alchemy
keys); drpc/GetBlock support to be probed at runtime, not assumed.

**Volume math (Polygon, 2s blocks ⇒ 43,200 blocks/day)**
- Receipts: 1 call per block via `alchemy_getTransactionReceipts` (or per-tx otherwise).
- At ~90 rps aggregate budget (9 providers), full-day live ≈ 45–50k calls/day → fine.
- 7-day backfill ≈ 320k blocks ⇒ ~400–600k calls incl. headers/retries ⇒ few hours.
- SQLite size estimate: ~5–20 GB/month for facts (swaps dominate); prune `transfers`
  older than N days (ops + swaps retained).

---

## 5. Storage (SQLite v1, WAL; Postgres-portable schema)

```sql
CREATE TABLE blocks(
  block_number INTEGER PRIMARY KEY, block_hash TEXT NOT NULL, ts INTEGER NOT NULL,
  producer TEXT, base_fee_gwei REAL, tx_count INTEGER, indexed_at INTEGER NOT NULL);

CREATE TABLE txs(
  hash TEXT PRIMARY KEY, block_number INTEGER NOT NULL, tx_index INTEGER NOT NULL,
  "from" TEXT NOT NULL, "to" TEXT, success INTEGER NOT NULL,
  gas_used INTEGER, effective_gas_price_gwei REAL, priority_fee_gwei REAL,
  value_native TEXT, FOREIGN KEY(block_number) REFERENCES blocks(block_number));
CREATE INDEX txs_block ON txs(block_number);

CREATE TABLE transfers(
  block_number INTEGER, tx_index INTEGER, log_index INTEGER,
  token TEXT, "from" TEXT, "to" TEXT, amount TEXT, is_native INTEGER DEFAULT 0,
  PRIMARY KEY(block_number, tx_index, log_index));
CREATE INDEX transfers_token ON transfers(token, block_number);

CREATE TABLE swaps(
  block_number INTEGER, tx_index INTEGER, log_index INTEGER,
  pool TEXT NOT NULL, dex TEXT, amm TEXT,           -- amm: v2|v3|v4|curve|balancer|solidly|lb
  token_in TEXT NOT NULL, token_out TEXT NOT NULL,
  amount_in TEXT NOT NULL, amount_out TEXT NOT NULL, sender TEXT,
  PRIMARY KEY(block_number, tx_index, log_index));
CREATE INDEX swaps_pool ON swaps(pool, block_number);

CREATE TABLE mev_ops(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  block_number INTEGER NOT NULL, tx_hash TEXT NOT NULL, ts INTEGER NOT NULL,
  kind TEXT NOT NULL CHECK(kind IN ('arb_atomic','sandwich','liquidation','jit','jit_arb','unknown')),
  eoa TEXT NOT NULL, contract TEXT,                 -- searcher clustering target
  confidence TEXT NOT NULL CHECK(confidence IN ('exact','inferred')),
  profit_token TEXT, profit_amount TEXT, profit_usd REAL,
  gas_cost_usd REAL, net_profit_usd REAL,
  route_json TEXT,                                  -- [{pool,dex,token_in,token_out,amount_in,amount_out}]
  victim_hashes TEXT,                               -- JSON array (sandwiches)
  details_json TEXT,                                -- kind-specific (liq bonus, JIT fees, …)
  detector TEXT NOT NULL, created_at INTEGER NOT NULL);
CREATE INDEX mev_ops_block ON mev_ops(block_number);
CREATE INDEX mev_ops_sender ON mev_ops(eoa, block_number);
CREATE INDEX mev_ops_kind_ts ON mev_ops(kind, ts);

CREATE TABLE labels(
  address TEXT PRIMARY KEY, kind TEXT, name TEXT, entity TEXT,
  evidence TEXT, first_seen_block INTEGER);

CREATE TABLE prices(
  hour INTEGER NOT NULL, token TEXT NOT NULL, usd REAL NOT NULL, source TEXT,
  PRIMARY KEY(hour, token));

CREATE TABLE sync_state(
  chain_id INTEGER PRIMARY KEY, head INTEGER, indexed_to INTEGER, last_indexed_at INTEGER);
```

Token metadata (decimals/symbol) reuses `cache/token_cache.rs`. Aggregate views
(daily counts, per-kind profit, sender leaderboards, most profitable token) are SQL
views in the store, queried by the reporting commands.

---

## 6. CLI surface (new `explorer` subcommand group)

All commands respect existing `output = table|csv|json` and `mev-scout.toml` provider
config. New optional toml section `[explorer]` (db path, confirmations, backfill batch).

```
mev-scout explorer doctor
    Probe every configured provider: latest block, archive (eth_getProof @ old block),
    traces (debug_traceBlockByNumber on recent), multicall support, observed rps.
    → prints capability matrix; blocks Phase 0.

mev-scout explorer index [--from N] [--to M] [--live] [--confirmations 6]
    Backfill and/or stream-index blocks into the store.
    Live mode: follow head − confirmations; decode → detect → account → insert.
    Idempotent; resumable via sync_state.

mev-scout explorer live [--kinds arb_atomic,sandwich,liquidation] [--min-profit-usd 1]
    mev.zone "MEV Live"-style streaming table (tail of the store or channel from a
    running `index --live`): time | kind | token | route (A→B→C via DEX) | profit USD |
    sender. Cross-validation baseline vs `mev-scout live`.

mev-scout explorer stats [--since 1d|7d|30d|all]
    Overview: op count per kind, total/avg/net profit, gas+tips burned, operated tokens,
    most profitable token, highest single profit, daily breakdown table.

mev-scout explorer top [--by sender|token|pool] [--metric profit|ops] [--since …]
    Leaderboards (mev.zone "Leaderboard"/"Senders" sections).

mev-scout explorer show <TX_HASH>
    Operation detail: full route with per-hop amounts, gross vs gas vs net, confidence,
    related victim txs (sandwich), links into existing replay tooling.

mev-scout explorer validate [--since 7d] [--strategy all]
    Cross-validation: join on-chain extracted ops (ground truth) vs `run`/`live`
    opportunity detections cached in results/ → recall, missed-value USD,
    false-positive rate, time-to-extraction by competitors. Output feeds the weekly
    report (`report` command) as a new section.

mev-scout explorer export --format json|csv --since … [--kinds …] [--out FILE]
    Bulk export for external analysis (Dune-style notebooks).
```

---

## 7. Detection & profit-attribution methodology (self-contained)

**Per-tx accounting (the core primitive)**
1. Collect ERC20 `Transfer` logs + native value for the tx.
2. Build per-address per-token delta map.
3. EOA sender (or its deployed contract) = candidate searcher.
4. Gross profit = positive delta of the searcher in the tx's "profit token" (token that
   appears in a closed cycle, or the residual token after netting flash-loan
   borrow/repay — flashloans already detected by `chain/flashloans.rs`).
5. Net = gross − gas (USD via `historical_exact` gas model) − flash-loan fees.

**Arb (cycle) detection**: decode swaps of the tx; the swap sequence forms a directed
edge list over tokens/pools; a closed walk starting/ending in the same token with the
searcher's net positive delta ⇒ `arb_atomic`. Two-hop vs multi-hop inherits from
existing detectors, but the explorer path uses **accounted balances**, not pool-state
simulation — no archive state needed, hence logs-only feasibility.

**Sandwich**: group txs in a block by pool; find pattern (swap A: tokenX→Y by EOA e) →
(victim swap X→Y) → (swap Y→X by same e). Profit = backrun output − frontrun input
netted in USD; victim hashes recorded; user-harm USD = victim's slippage vs execution
price without the sandwich (computable from pool reserves reconstructed by our pipeline).

**Liquidation**: event-driven; profit = collateral seized − debt repaid (+ same-tx swap
of collateral); reuse `chain/liquidations.rs` seeds (Aave V3 Polygon pool
`0x794a61358D6845594F94dc1DB02A252b5b4814aD` first).

**Pricing**
- Live: CoinGecko (`coingecko.rs`), cached hourly in `prices`.
- Backfill: DefiLlama hourly close (batch-friendly) — avoid free-tier throttling.
- Long-tail fallback: on-chain stable-route pricing using our pool math (WMATIC/WPOL,
  WETH, USDC, USDT, DAI legs) — flagged `source=onchain`.

**Labels / sender leaderboard**
Seed list (routers, DEX factories, Aave/Morpho, known MEV bot contracts), then cluster:
same deployer bytecode, same funder, same nonce stream, reuse `chain/labels.rs`.
Compounding asset — must start collecting from day one of live indexing.

---

## 8. Phased roadmap (with acceptance criteria)

### Phase 0 — Capability probe + spike (est. 1–2 days)
- `explorer doctor` implemented.
- Spike: fetch receipts for 1k recent Polygon blocks; measure latency/cost per provider;
  confirm logs-only assumption on a sample containing V2/V3/Curve/Balancer swaps.
- ✅ Done when: capability matrix printed; storage/backfill cost estimate validated.

### Phase 1 — Indexer core (est. ~1–1.5 weeks)
- `explorer::ingest` + `explorer::store` (schema above), reorg handling, resume.
- Decode: swaps + transfers + liquidation events for Polygon factories.
- ✅ Done when: `index --from -50000 --to -1000` completes idempotently on mainnet
  Polygon; `swaps` counts match a manual `eth_getLogs` sample within 0%.

### Phase 2 — Detectors → mev_ops (est. ~1 week)
- Explorer-mode detectors (accounting-based) writing `mev_ops` with confidence levels.
- ✅ Done when: on a known sandwich-heavy range, ≥95% of manually labeled sandwiches are
  detected with <2% false positives (manual audit of 100 ops).

### Phase 3 — Accounting: profit, gas, USD (est. ~1 week)
- Balance-delta attribution, flash-loan netting, pricing backfill, tips/gas USD.
- ✅ Done when: 100 sampled arb ops reconcile manually (±1% USD) against EigenPhi or a
  hand-computed sheet; `net_profit_usd` populated for ≥99% of `exact` ops.

### Phase 4 — Reporting commands (est. 3–5 days)
- `live`, `stats`, `top`, `show`, `export` + aggregate views, display.rs integration.
- ✅ Done when: `stats --since 7d` reproduces the mev.zone home-page metric set on our
  Polygon data; `live` streams new ops within ~2 confirmations of block inclusion.

### Phase 5 — Cross-validation harness (est. 3–5 days)
- `explorer validate`: join ground-truth ops vs `run`/`live` detections; weekly metrics.
- ✅ Done when: weekly report includes recall/missed-value/latency per strategy, and
  discrepancies are explainable (each >1% miss has a categorized cause).

### Phase 6 — (Optional, later) Web UI
- axum REST + WS over the same store; Next.js dashboard (overview, live feed, op detail,
  sender profile). Migrate store to Postgres/ClickHouse if/when needed.
- ✅ Done when: parity with CLI sections; no business logic duplicated outside
  `core/src/explorer`.

---

## 9. Polygon constants & seeds

| Item | Value |
|---|---|
| Chain id | 137 |
| Block time | ~2s (43,200 blocks/day) |
| WMATIC/WPOL | `0x0d500B1d8E8eF31E21C99d1Db9A6444d3ADf1270` |
| POL | `0x455e53CBB86018Ac2B8092FdCd39d8444aFFC3F6` |
| WETH | `0x7ceB23fD6bC0adD59E62ac25578270cFf1b9f619` |
| USDC (native) | `0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359` |
| USDC.e | `0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174` |
| USDT | `0xc2132D05D31c914a87C6611C10748AEb04B58e8F` |
| DAI | `0x8f3Cf7ad23Cd3CaDbD9735AFf958023239c6A063` |
| Profit-token priority list | USDC → USDT → DAI → WMATIC → WETH → (long-tail via route pricing) |
| V2 Swap topic0 | `0xd78ad95f…9d822` (standard; forks reuse) |
| V3 Swap topic0 | `0xc42079f9…ca67` |
| ERC20 Transfer topic0 | `0xddf252ad…b3ef` |

(Full 32-byte topics live in `core/src/sigs/` — the resolver already computes them.)

---

## 10. Risks & mitigations

| Risk | Mitigation |
|---|---|
| RPC quota/cost for backfill + live | Provider fan-out with existing RPS middleware; `alchemy_getTransactionReceipts` batching; logs-only v1; option of dedicated node later |
| Long-tail token pricing noise | Stable-route on-chain fallback; mark `source`; exclude tokens with no stable leg from USD stats (report in token units) |
| Misattribution (proxy/factory bots) | Confidence field + labeler evidence; never present `inferred` as exact in UI |
| CEX-DEX opacity | Excluded in v1; `unknown` bucket; explicit methodology note |
| Reorgs on Bor | Confirmation lag (default 6) + hash-mismatch re-index |
| Secrets | **`mev-scout.toml` currently contains live API keys in-repo — move to env/file outside git before any sharing** |
| SQLite write contention during backfill | Single-writer task + WAL + batched transactions; readers are WAL-safe |

## 11. Open questions
1. GetBlock/drpc `debug_*` support on Polygon — confirm via doctor (affects Phase 6
   native-transfer coverage only).
2. Should `explorer live` also emit ops into the same `results/` artifacts used by
   `report`, or stay read-only over the store? (Suggest: separate, join at validate.)
3. Retention policy for `transfers` (propose: 30 days hot, aggregate-only older).
4. Naming: `explorer` vs `inspect` for the subcommand group.

## 12. References (methodology, not APIs)
- Flashbots `mev-inspect-py` (archived) — schema/taxonomy reference (swaps, arbitrages,
  liquidations, miner payments).
- EigenPhi methodology pages — classification definitions & known limitations.
- libMEV — open multi-chain explorer (prior art for labels/leaderboards).
- mev.zone docs (gitbook) — auction model reference (Avalanche-specific data advantage).