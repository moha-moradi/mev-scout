# mev-scout — Architecture & CLI Command Guide

An MEV opportunity scanner & backtester for EVM chains (primary target: Polygon).
Three host crates on one engine, plus a SPA frontend:

- **`core/`** — `mev-scout-core`: engine + stores + shared job orchestration.
- **`cli/`** — `mev-scout-cli`: thin binary (`mev-scout`) with 11 subcommands. Parses args, loads config, presentation, dispatches to core.
- **`api/`** — `mev-scout-api`: local-only HTTP API (`127.0.0.1`); jobs call core in-process (no CLI subprocess); serves `web/dist`.
- **`web/`** — Vite + React SPA: Dashboard, Run, Live, Explorer, Pools, Jobs, Results, Config.

---

## 1. Top-down module view

```mermaid
flowchart TB
    subgraph CLI["mev-scout-cli (binary: mev-scout)"]
        MAIN["main.rs<br/>parse args · load config · logging"]
        CLIDEF["cli.rs<br/>clap: 11 subcommands + BlockRange args"]
        DISPATCH["commands/mod.rs<br/>CliCommand trait → dispatch"]
        UI["display.rs · overrides.rs<br/>tables · config merge"]
    end

    subgraph WEB["web/ (Vite + React SPA)"]
        PAGES["pages: Dashboard · Run · Live · Explorer<br/>Pools · Jobs · Results · Config"]
    end

    subgraph API["mev-scout-api (binary: mev-scout-api)"]
        APIMAIN["main.rs<br/>bind 127.0.0.1 · serve web/dist"]
        ROUTES["routes<br/>health · chains · config · jobs<br/>results · runs · opportunities<br/>pools · explorer · sync"]
        JOBS["jobs.rs · exec.rs<br/>JobManager → core::jobs"]
        READ["read.rs · state<br/>readonly SQLite for UI queries"]
    end

    subgraph CORE["mev-scout-core (library)"]
        direction TB

        subgraph ORCH["Orchestration"]
            COREJOBS["jobs<br/>run · live · discover · scan · tokens · report · index"]
            PIPE["pipeline<br/>BacktestRunner · run_block / run_range(_hybrid)<br/>aggregate → metrics · gas model"]
        end

        subgraph DETECT["Detection"]
            MEV["mev::detectors<br/>two-hop · multi-hop · sandwich<br/>JIT · JIT-arb · liquidation · mempool"]
            POOL["pool<br/>state: PoolManager (reserves/ticks)<br/>discovery: V2/V3/V4/Solidly/Curve/<br/>Balancer/Fluid/Infinity/…<br/>math: AMM curves per DEX"]
        end

        subgraph EXEC["Execution"]
            REPLAY["replay<br/>BlockReplayer (revm)<br/>filtered EVM replay + Polygon precompiles"]
            RESOLVER["resolver<br/>RangeResolver → ResolvedRange<br/>(days / blocks / range)"]
        end

        subgraph IO["Data I/O"]
            FETCH["fetch<br/>Fetcher — parallel block+receipt download<br/>sharded across providers, gap refill"]
            RPC["rpc<br/>RpcClient — multi-provider<br/>weighted · rate-limited · Multicall3"]
            CACHE["cache<br/>SqliteStore — blocks, receipts, state,<br/>discovered pools, tokens, run manifests"]
        end

        subgraph SCAN["Event scanning"]
            CHAIN["chain<br/>trades · transfers · flashloans<br/>liquidations · labels"]
        end

        subgraph EXPLORE["Realized-MEV explorer"]
            EXPL["explorer<br/>ingest · decode · classify · profit<br/>store (own SQLite) · validate · reject"]
        end

        subgraph SUPPORT["Support"]
            CFG["config<br/>TOML settings + validation"]
            TYPES["types<br/>MevOpportunity · Strategy · GasConfig · ResultsFile"]
            SIGS["sigs — 4byte signature resolver"]
            MISC["data · error · dex_type · utils · progress"]
        end
    end

    MAIN --> CLIDEF --> DISPATCH
    DISPATCH --> UI
    DISPATCH --> COREJOBS
    PAGES --> ROUTES
    APIMAIN --> ROUTES
    ROUTES --> JOBS
    ROUTES --> READ
    JOBS --> COREJOBS
    READ --> CACHE
    READ --> EXPL

    COREJOBS --> RESOLVER
    COREJOBS --> FETCH
    COREJOBS --> CHAIN
    COREJOBS --> POOL
    COREJOBS --> PIPE
    COREJOBS --> EXPL
    COREJOBS --> CFG

    RESOLVER --> RPC
    FETCH --> RPC
    FETCH --> CACHE
    PIPE --> REPLAY
    REPLAY --> CACHE
    REPLAY --> RPC
    PIPE --> POOL
    PIPE --> MEV
    MEV --> POOL
    POOL --> RPC
    POOL --> CACHE
    EXPL --> RPC
    COREJOBS -. "opportunities always;<br/>rejections if --record-rejections" .-> EXPL
    CHAIN --> RPC
    CFG --> TYPES
    PIPE --> TYPES
    FETCH -.-> SIGS
```

**Key relationships**

- Both `CLI` and `API` are first-class hosts: `CLI → core` and `web → API → core`. Shared long-running flows live in `core::jobs`.
- `pipeline` is the hub: `BacktestRunner` owns `BlockReplayer` + `PoolManager` and drives every detector per transaction.
- `cache` (SQLite) is the local-first backbone — fetch stores blocks there; replay and the runner read from it; pool discovery persists pools/tokens into it.
- `rpc` fronts the chain for everything: fetching, `eth_call` pool state, log scans, and replay's on-demand state misses (via `CachedRpcDb`).
- `explorer` answers "what was made" (realized MEV forensics) vs the detectors' "what could be made" — it ingests via RPC into its own SQLite store (`explorer_{chain}.sqlite`) so backfill writes never contend with replay-path cache reads. Scanner `run`/`live` always persist detected opportunities there (via in-memory `ResultsFile` DTO); `--record-rejections` also stores rejected candidates for miss attribution.
- The API never shells out to the CLI binary: `JobManager` + `exec` parse argv-style flags and call the same `core::jobs::*` entry points the CLI uses.

---

## 2. API & web UI

Local-only server (`mev-scout-api`, default `http://127.0.0.1:7600`). Serves the built SPA from `web/dist` with SPA fallback for client routes; Vite dev (`:5173`) can hit the API via CORS.

### Job surface (write / long-running)

`POST /api/jobs` allowlist (CLI-only stay off this list: `fetch`, `replay`, `config`, `validate-pools`, most `explorer` query subcommands):

| Command | Notes |
|---|---|
| `run` | Full backtest |
| `live` | One-shot or looping detection |
| `discover` | Pool universe build |
| `tokens` | Token metadata cache |
| `scan` | Raw event scans |
| `report` | Re-render a recorded run |
| `explorer index` | Realized-MEV backfill / live index |

Job lifecycle: create → poll `/api/jobs/:id` · `/progress` · `/log` → optional `/stop`. Execution is in-process on a worker thread (`exec::run_job` → `core::jobs`).

### Read surface (SQLite → JSON)

| Route group | Source | Purpose |
|---|---|---|
| `/api/health`, `/api/chains` | process + config | liveness, configured chains |
| `/api/config` | TOML (GET/PUT) | view / edit resolved config |
| `/api/runs`, `/api/pools` | cache DB | run manifests, discovered pools |
| `/api/results`, `/api/opportunities` | explorer DB | run detail, PnL, validation, opp lists |
| `/api/explorer/*` | explorer DB | feed, stats, overview, top, ops, op detail |
| `/api/sync` | explorer sync_state | indexer checkpoint / lag |

### SPA pages (`web/src/pages`)

Dashboard · RunBacktest · Live · Explorer · Pools · Jobs · Results · Config — all talk to the routes above (poll jobs; read SQLite-backed JSON).

---

## 3. CLI command map

| Command | Purpose | Chain access | Writes |
|---|---|---|---|
| `run` | Full backtest → opportunities | yes (fetch + eth_call) | SQLite cache, explorer SQLite |
| `live` | Stream new blocks, detect as they arrive | yes | SQLite cache, explorer SQLite |
| `fetch` | Pre-cache blocks only (no detection) | yes | SQLite cache |
| `discover` | Find pools (on-chain factories / aggregators) | yes (RPC and/or REST) | SQLite cache |
| `validate-pools` | Measure discovery accuracy vs GeckoTerminal | yes (RPC + REST) | SQLite cache |
| `tokens` | Populate / view token metadata cache | optional REST (`--enrich`) | SQLite `token_symbols` |
| `scan` | Raw event scans (trades/whales/flashloans/liqs) | yes (getLogs only) | — |
| `replay` | Debug a single block through revm | yes (fallback calls) | — |
| `report` | Re-render a recorded run from SQLite | no | — |
| `config` | Print fully-resolved TOML | no | — |
| `explorer` | Realized-MEV forensics (index/feed/stats/top/show/explain/validate/export) | yes (logs; optional traces) | Explorer SQLite store |

---

## 4. Command workflows (per command)

### 4.1 `run` — the full backtest

The main pipeline. Everything is cached first, then replayed and detected.

```
mev-scout run --days 7 [--batch-rpc] [--record-rejections]
```

```mermaid
flowchart TB
    A["validate config<br/>(validation::validate_and_resolve)"] --> B["init_rpc<br/>multi-provider client"]
    B --> C["SqliteStore::open<br/>(per-chain cache.db)"]
    C --> D["RangeResolver.resolve<br/>--days / --blocks / --block / --from--to<br/>→ ResolvedRange"]
    D --> E["RunManifest → SQLite<br/>(run_id = run_{epoch})"]
    E --> F{"discovered pools<br/>in cache?"}
    F -- "yes" --> G["Fetcher.fetch_relevant<br/>log-first: only blocks with<br/>pool activity"]
    F -- "no" --> H["Fetcher.fetch_range<br/>all blocks"]
    G --> I{"gaps after fetch?"}
    H --> I
    I -- yes --> J["auto_refetch_gaps"]
    I -- no --> K
    J --> K["PoolManager::init_pools<br/>load pools from discovery cache<br/>skip pools created after start<br/>fetch reserves at start_block−1"]
    K --> L["BlockReplayer::new<br/>(revm, chain_id)"]
    L --> M["BacktestRunner::new<br/>+ proximity window · min profit<br/>+ capture pending · persistence scoring"]
    M --> N{"aave_v3_pool<br/>configured?"}
    N -- yes --> O["prefetch_aave_reserves<br/>(for LiquidationDetector)"]
    N -- no --> P
    O --> P["runner.run_range<br/>per block: filtered revm replay<br/>→ detectors → MevOpportunities"]
    P --> Q["ResultsFile DTO → explorer SQLite<br/>(opportunities always)<br/>render results + block summary tables<br/>(--record-rejections: rejected<br/>candidates → explorer store)"]
```

Inside `run_range` — per block:

```mermaid
flowchart LR
    B0["load block data<br/>+ txs from SQLite"] --> B1{"tx touches tracked<br/>pool/token?"}
    B1 -- yes --> B2["full revm execution<br/>(BlockMode::FullReplay)"]
    B1 -- no --> B3["synthesized ExecutedTx<br/>from cached receipts<br/>(fast path)"]
    B2 --> B4["apply Swap/Sync logs<br/>→ PoolManager state"]
    B3 --> B4
    B4 --> B5["detectors per tx:<br/>1 two-hop arb<br/>2 multi-hop arb (BFS ≤4)<br/>3 JIT liquidity<br/>4 sandwich<br/>5 JIT+arb hybrid<br/>(+ liquidation, mempool)"]
    B5 --> B6["filter: min_profit_wei<br/>max_candidates_per_tx<br/>persistence confidence decay"]
```

### 4.2 `live` — real-time streaming detection

Same engine, one-shot or continuous polling (`--loop [--duration 1h] [--poll-interval-ms 2000]`); `--record-rejections` persists rejected candidates to the explorer store.

```mermaid
flowchart TB
    A["validate_live + init_rpc<br/>+ open cache"] --> B["read pool addresses<br/>from discovery cache"]
    B --> C{"--loop?"}
    C -- "no (one-shot)" --> D["tip = get_block_number"]
    C -- "yes" --> E["init: tip, PoolManager,<br/>runner, Aave prefetch"]
    D --> F["init pools at tip−1<br/>+ Aave prefetch"]
    F --> G["fetch tip block"] --> H["detect_state_horizon<br/>→ run_range_hybrid"] --> I["persist → explorer SQLite<br/>print table"]
    E --> J["poll loop"]
    J --> K["sleep(poll_interval)"]
    K --> L["get_block_number"]
    L --> M{"tip > last_block?"}
    M -- no --> K
    M -- yes --> N["fetch blocks last+1..tip<br/>(fetch_relevant if pools known)"]
    N --> O["run_range_hybrid<br/>FullReplay within state horizon,<br/>LogOnly beyond"]
    O --> P{"error?"}
    P -- "yes (<5 consecutive)" --> K
    P -- no --> Q["persist → explorer SQLite<br/>print per-range summary"]
    Q --> R{"deadline reached?"}
    R -- no --> K
    R -- yes --> S["session summary<br/>(blocks, txs, opportunities)"]
    P -- "≥5 consecutive" --> T["bail out"]
```

### 4.3 `fetch` — pre-cache blocks only

Warms the SQLite cache so later `run`/`replay` are fast/offline-friendly.

```
mev-scout fetch --blocks 1000 [--batch-rpc] [--no-sig-resolve]
```

```mermaid
flowchart TB
    A["resolve_chain + init_rpc"] --> B["open SQLite cache"]
    B --> C["resolve block range"]
    C --> D["RunManifest → SQLite"]
    D --> E{"--no-sig-resolve?"}
    E -- no --> F["ensure_signature_db<br/>4byte.directory → local sig DB"]
    E -- yes --> G["skip sig resolution"]
    F --> H["Fetcher.fetch_range<br/>sharded across providers by weight<br/>parallel block+receipt download"]
    G --> H
    H --> I["store blocks+receipts+txs<br/>(sig labels resolved per tx)"]
    I --> J{"missing blocks?"}
    J -- yes --> K["auto_refetch_gaps"]
    J -- no --> L["print fetch summary<br/>(fetched / cached / phase timings)"]
    K --> L
```

### 4.4 `discover` — build the pool universe

Finds pools from factory events and/or free aggregators; result feeds all other commands (they read the discovery cache).

```
mev-scout discover --days 30 [--source onchain|remote|hybrid] [--enrich] [--incremental]
```

```mermaid
flowchart TB
    A["resolve_chain + init_rpc<br/>+ open cache + warm TokenCache"] --> B{"--source"}
    B -- "remote" --> R["skip block range & on-chain scan"]
    B -- "onchain / hybrid" --> C["resolve range<br/>(default: last pool_discovery_lookback_blocks → tip)"]
    B -- "hybrid / remote" --> R2["remote leg (below)"]
    C --> D{"--incremental?"}
    D -- yes --> E["from = max cached creation_block + 1<br/>(skip if cache is current)"]
    D -- no --> F
    E --> F["Phase 1: discover_and_cache<br/>factory event scan (chunked getLogs):<br/>V2 · V3 · V4 · Solidly · Camelot<br/>Curve registry · Balancer vault<br/>TraderJoe LB · Pendle · Fluid<br/>Pancake Infinity CL<br/>+ pool metadata via Multicall3"]
    F --> G
    R --> G["merge sources"]
    R2 --> H["Phase 2: remote aggregators<br/>GeckoTerminal + DexScreener<br/>(not DefiLlama yields)<br/>(--max-pools, --min-tvl)"]
    H --> G
    G --> I{"--source semantics"}
    I -- onchain --> J["on-chain pools only"]
    I -- remote --> K["remote pools only"]
    I -- hybrid --> L["union, dedup by address"]
    J --> M
    K --> M
    L --> M
    M{"--resolve-remote-metadata?"}
    M -- yes --> N["Multicall3: fill fee/tickSpacing/<br/>tokens for remote CL pools"]
    M -- no --> O
    N --> O{"--health-check? (default on)"}
    O -- yes --> P["drop drained/paused pools<br/>(on-chain state probe)"]
    O -- no --> Q
    P --> Q["Phase 5.3: persist universe → SQLite<br/>(cache-first merge, never clobber richer rows)"]
    Q --> T["output: table or --json"]
```

### 4.5 `validate-pools` — discovery accuracy audit

Compares our on-chain discovery (set A) against GeckoTerminal references (set B).

```
mev-scout validate-pools --days 7 [--source all|gecko] [--json] [--markdown-out report.md]
```

```mermaid
flowchart TB
    A["resolve_chain"] --> B["Set B first (no RPC needed,<br/>fail fast if all dead):<br/>discover_via_geckoterminal"]
    B --> C["init_rpc + resolve --days window"]
    C --> D["open cache"]
    D --> E["Set A: discover_pools over window<br/>(same factories as discover,<br/>NO token cache)"]
    E --> F["health check set A"]
    F --> G["compare A vs B per source:<br/>• recall = |A∩B| / |B| overall + per DEX<br/>• A∖B extras (dust retention)<br/>• fee / token-side mismatches on overlap<br/>• TVL mean-absolute-delta<br/>• top missing pools by TVL"]
    G --> H["UTF-8 table · --json · --markdown-out"]
```

### 4.6 `tokens` — token metadata cache

Offline by default (bundled known-token list + SQLite). Optional `--enrich`
pulls missing **symbol/decimals** from DefiLlama coins and **name/icon URL**
from CoinGecko's contract endpoint. DefiLlama yields is **not** used for
pool discovery (UUID-only; no pool contract addresses).

```
mev-scout tokens [--symbol USDC] [--decimals 6] [--limit 100] [--cache-only] [--enrich]
```

```mermaid
flowchart LR
    A["resolve chain → chain_id"] --> B["open SQLite cache"]
    B --> C["TokenCache::warm(chain_id)<br/>bundled known tokens"]
    C --> D["merge persisted tokens<br/>from SQLite"]
    D --> E{"--enrich?"}
    E -- yes --> F["seed addresses from pool_info"]
    F --> G["DefiLlama coins<br/>symbol / decimals"]
    G --> H["CoinGecko contract<br/>name / icon_url (capped)"]
    H --> I["persist_all → SQLite"]
    I --> J
    E -- no --> J["filter by --symbol / --decimals"]
    J --> K["sort by address, truncate --limit"]
    K --> L{"--cache-only?"}
    L -- yes --> M["print count only"]
    L -- no --> N["table / json / csv"]
```

### 4.7 `scan` — raw on-chain event scans

Topic-level `eth_getLogs` scans; replaces the old Dune-query workflows. No cache writes.

```
mev-scout scan --kind trades|transfers|flashloans|liquidations|labels [--address 0x..] [--limit 500]
```

```mermaid
flowchart TB
    A["resolve_chain + init_rpc"] --> B["resolve block range"]
    B --> C{"--kind"}
    C -- trades --> D["chain::trades::scan_trades<br/>V2/V3/Algebra/Solidly/Curve swap topics"]
    C -- transfers --> E{"--min-value set?"}
    E -- yes --> F["scan_whale_transfers"]
    E -- no --> G["scan_transfers"]
    C -- flashloans --> H["chain::flashloans::scan_flash_loans<br/>Aave V2/V3 · Balancer V2 · Uni V3"]
    C -- liquidations --> I["chain::liquidations::scan_liquidations<br/>Aave V3 LiquidationCall · Compound V3 Absorb"]
    C -- labels --> J["LabelDb::load<br/>(bundled JSON + DefiLlama cache)<br/>print --address lookups"]
    D --> K["chunked getLogs<br/>(--batch-size, --address filter)"]
    F --> K
    G --> K
    H --> K
    I --> K
    K --> L["print table / --json / --csv<br/>limited to --limit"]
```

### 4.8 `replay` — single-block EVM debugger

Re-runs one cached block through revm and verifies results against cached receipts.

```
mev-scout replay --block 65000000 [--tx-index 42] [--analyze]
```

```mermaid
flowchart TB
    A["validate_replay + init_rpc<br/>+ open cache"] --> B{"block cached?"}
    B -- no --> X["bail: 'run mev-scout fetch<br/>--block N first'"]
    B -- yes --> C{"--analyze?"}
    C -- yes --> D["load discovered pools<br/>into address→PoolInfo map"]
    C -- no --> E
    D --> E["BlockReplayer.replay_to<br/>sequential revm execution 0..tx_index<br/>(single EVM context, state carried forward)"]
    E --> F["per tx: idx · hash · status · gas_used<br/>receipt match ✓/✗"]
    F --> G{"--analyze?"}
    G -- yes --> H["decode DEX log interactions:<br/>Swap / Sync / Mint / Burn per pool"]
    G -- no --> I
    H --> I["receipt verification summary:<br/>matched/total (%), warn if < 99%"]
```

### 4.9 `report` — re-render saved results

Offline. Reads run history from the SQLite stores — the `run_manifests` table (cache DB) for run metadata and the `opportunities` table (explorer DB) — and re-renders them. No chain access; no JSON result files on disk.

```
mev-scout report [--run-id run_1717...]
```

```mermaid
flowchart LR
    A["cache DB<br/>(run_manifests)"] --> B{"--run-id given?"}
    B -- no --> C["pick latest run<br/>(by resolved_at)"]
    B -- yes --> D["row for run_id"]
    C --> E["manifest metadata"]
    D --> E
    E --> F{"output format"}
    F -- table --> G["run header +<br/>render_results_table"]
    F -- csv --> H["CSV rows:<br/>block, tx_index, strategy,<br/>input, profit, gas, confidence"]
    F -- json --> I["pretty-print full run"]
```

### 4.10 `config` — print resolved config

Offline, two lines of logic: merges TOML file (if any) with CLI overrides in `main.rs`, then serializes.

```
mev-scout config [-f custom.toml] [--verbose]
```

```mermaid
flowchart LR
    A["main.rs: load -f file,<br/>mev-scout.toml, or defaults"] --> B["merge CLI overrides<br/>(overrides.rs)"] --> C["to_toml_string → stdout"]
```

### 4.11 `explorer` — realized-MEV forensics

Forensic reconstruction of MEV that was actually extracted on-chain — the counterpart to the scanner's simulated opportunities. Pipeline: `ingest` (block+receipt fetch, reorg-aware, resumable) → `decode` (swaps, transfers, liquidation facts) → `classify` (per-block pattern passes) → `profit` (token balance-delta accounting, gas, USD) → `store` (dedicated SQLite, `explorer_{chain}.sqlite`) → query surface below. Scanner `run`/`live` persist opportunities into the same store (via `ResultsFile` as an in-memory DTO); `--record-rejections` adds rejected candidates for cross-validation and miss-cause attribution.

```
mev-scout explorer doctor
mev-scout explorer index [--from N --to N | --days N] [--live [--duration DURATION]]
mev-scout explorer live-feed [--kinds KINDS] [--min-profit-usd USD] [--duration DURATION]
mev-scout explorer stats [--since 1d|7d|30d|all] [--kind KIND]
mev-scout explorer top --by sender|token|pool --metric profit|ops [--since WINDOW]
mev-scout explorer show <TX_HASH> [--trace]
mev-scout explorer explain <TX_HASH>
mev-scout explorer validate [--since WINDOW] [--match-window N] [--threshold-sweep]
mev-scout explorer export --format json|csv [--out FILE]
```

```mermaid
flowchart TB
    A["resolve_chain + init_rpc"] --> B["ExplorerStore::open<br/>(explorer_{chain}.sqlite, WAL)"]
    B --> C{"subcommand"}
    C -- doctor --> D["probe every provider:<br/>latest · archive · bulk-receipts · traces"]
    C -- index --> E["ingest: backfill range or --live<br/>(head − confirmations)<br/>idempotent · reorg-aware · classify-in-stream"]
    E --> F["decode → classify → profit<br/>(balance-delta accounting + gas + USD)"]
    C -- "live-feed" --> G["tail of mev_ops<br/>(--kinds, --min-profit-usd, poll)"]
    C -- "stats / top" --> H["pure SQL aggregates:<br/>op counts · profit · daily · leaderboards"]
    C -- "show / explain" --> I["op detail per tx hash<br/>(--trace: prestateTracer diffMode recompute)<br/>explain: rejected candidates + miss cause"]
    C -- validate --> J["realized ground truth vs run/live<br/>tiered recall · miss taxonomy · sweep"]
    C -- export --> K["bulk json|csv"]
    F --> L["mev_ops + blocks/txs/transfers/swaps<br/>+ opportunities + rejected_candidates<br/>+ sync_state checkpoints"]
```

#### Classifier-change replay recipe (wipe + reindex)

Any change to `decode` / `classify` / P&L invalidates existing `mev_ops` rows for
the affected window, so before/after Phase numbers are only comparable after a
replay. `index` is replay-safe (INSERT OR REPLACE, reorg-aware, idempotent), so
the wipe is required only for classifier *semantics* changes, not plain re-indexes:

```bash
# 1) Wipe the affected window on the forensic DB (example: Polygon, from N).
#    Remove classified ops + classified-block markers and rewind the checkpoint.
sqlite3 explorer_polygon.sqlite \
  "DELETE FROM mev_ops       WHERE block_number >= N;
   DELETE FROM blocks_classified WHERE block >= N;
   UPDATE sync_state SET indexed_to = N-1 WHERE chain_id = 137;"

# 2) Reclassify from stored facts.
mev-scout explorer index --from N --to M   # or --days D after the wipe above

# 3) Re-run baseline validation + missing-pool report.
mev-scout explorer validate --threshold-sweep --emit-missing-pools
```

#### Phase-0 baseline recipe and ship gates

Fixed-window recipe (e.g. Polygon, last 7 days):

```bash
mev-scout explorer index --days 7
mev-scout explorer validate --threshold-sweep --emit-missing-pools
```

Record the report on every phase merge (`cargo test` + clippy alone are not the
gate). Numbers are data-dependent — tests assert report *shape* only:

| Window | recall | USD-recall | miss-taxonomy | profit-error MAD | `arb_likely` parity |
|---|---|---|---|---|---|
| *(pending — run recipe above with RPC)* | — | — | — | — | `true` (default) |

No working RPC credentials were available to fill real numbers: the avalanche
endpoint committed in `mev-scout.toml` is revoked (connection reset; `explorer
doctor` reports `latest=✗ archive=n/a traces=n/a`, Gate FAIL). Re-run the
recipe locally with a valid key (plain URL or `${ENV_VAR}` placeholder) and
paste validate output into the table before flipping `arb_likely_parity` or
enabling causal kinds in live-feed defaults.

Gates that depend on this table:
- **Phase 1.2**: flip `explorer.arb_likely_parity=false` only when USD-recall drop
  ≤ the agreed budget recorded here (or an explicit "precision over recall"
  decision is written next to the numbers). No blind cliffs.
- **Phase 3 ship gate**: labeled golden set (Phase 0.5) must show acceptable
  backrun/frontrun precision before those kinds are enabled in live-feed defaults.
  Synthetic CI set: `mev-scout explorer validate --golden-causal` (or
  `cargo test -p mev-scout-core golden`). Live-feed / API feed default **excludes**
  `frontrun`/`backrun`; pass `--kinds all` (or include them explicitly) to opt in.
  Chain-curated labels remain a follow-up once an RPC baseline window is available.

#### Known biases / methodology notes

| Bias | Status | Notes |
|---|---|---|
| Historical `arb_likely` catch-all | Active (`arb_likely_parity=true` default) | Single-hop / non-cycle profitable residuals still label `arb_atomic` until the Phase-0 window gate flips the default. |
| Logs-only Backrun / Frontrun | By design | Execution-price proxies ≠ REVM `profit(B\|before_A)` vs `profit(B\|after_A)`. `STATE_DELTA_MATCH` / `PROFIT_VERIFIED` are **not** REVM-verified. |
| Gas | Mitigated (Phase 2.1) | Prefer receipt `effectiveGasPrice`, then legacy `gasPrice`; only fall back to `base_fee + priority`. |
| Multi-token profit | Mitigated (Phase 2.3) | Persist sums USD across all positive residuals; `profit_token` remains display-primary. JIT fee-capture stays unit-reported (`profit_token=None`). |
| Hourly pricing / FOT | Partial | Hourly token prices at persist; FOT/rebase tokens flagged `usd_approximate` in details. Long-tail decimals via Multicall3; realized-rate fallback for unpriced residuals. |
| Uni V3 `Flash` | Deferred | Not netted as Aave-style flash loan (different callback repay semantics). |
| Sandwich contract mediation | Logs-only | `tx.to` on front/back tags `contract_mediated`; store-backed labeled-searcher registry not required. |
| JIT cross-block reorg | Known limitation | Exact-key + `>=` liquidity close; cannot restore a position whose Burn is unwound by a reorg. |

---

## 5. The engine core: how detection works

`BacktestRunner.run_block` (pipeline/runner.rs) is the heart of `run` and `live`:

1. **Load** block + txs from SQLite.
2. **Filter** — only txs whose `to` or log emitter matches a tracked pool/token are replayed through revm; all others are synthesized from cached receipts (the main performance optimization for large backtests).
3. **Apply** decoded Swap/Sync/Mint/Burn events to `PoolManager`, so all detectors see post-tx reserves.
4. **Detect** per tx, in order: two-hop arb → multi-hop arb (BFS ≤ depth 4) → JIT liquidity → sandwich → JIT+arb hybrid; liquidation and mempool strategies where context allows.
5. **Post-process** — dust filter (`min_profit_wei`), per-tx candidate cap, cross-block persistence scoring (decaying confidence, `PERSISTENCE_DECAY = 0.75`, grace 5 blocks), gas calibration from observed `gasUsed`.

The hybrid path (`run_range_hybrid`, used by `live`) picks `FullReplay` vs `LogOnly` per block based on `rpc.detect_state_horizon` — blocks deeper than available archive state skip EVM execution and lose only the EVM-context strategies.

## 6. Persistent artifacts

| Artifact | Produced by | Consumed by |
|---|---|---|
| SQLite `cache.db` (blocks, receipts, state, discovered pools, tokens, run manifests) | `run`, `live`, `fetch`, `discover`, `validate-pools` | `run`, `live`, `replay`, `discover` (incremental), `tokens`, `report` (manifests), API read routes |
| Explorer store `explorer_{chain}.sqlite` — `opportunities` (+ optional `rejected_candidates`) | `run`, `live` (always opportunities; rejections with `--record-rejections`) | `report`, API `/api/results` · `/api/opportunities`, `explorer explain` / `validate` |
| Explorer store `explorer_{chain}.sqlite` — forensic layer (blocks, txs, transfers, swaps, `mev_ops`, sync_state, …) | `explorer index` | `explorer` CLI + API `/api/explorer/*` |
| Signature DB (4byte directory snapshot) | `fetch` (unless `--no-sig-resolve`) | tx decoding |
| `api_data/` job logs | API `JobManager` | `/api/jobs/:id/log` |

`ResultsFile` is an in-memory / presentation DTO (CLI tables, API job outcomes) — not a durable on-disk JSON artifact.
