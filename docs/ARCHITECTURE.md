# mev-scout — Architecture & CLI Command Guide

An MEV opportunity scanner & backtester for EVM chains (primary target: Polygon).
Two crates:

- **`core/`** — `mev-scout-core`: all engine logic (fetching, replay, detection, caching).
- **`cli/`** — `mev-scout-cli`: thin binary (`mev-scout`) with 10 subcommands. Parses args, loads config, dispatches to core.

---

## 1. Top-down module view

```mermaid
flowchart TB
    subgraph CLI["mev-scout-cli (binary: mev-scout)"]
        MAIN["main.rs<br/>parse args · load config · logging"]
        CLIDEF["cli.rs<br/>clap: 10 subcommands + BlockRange args"]
        DISPATCH["commands/mod.rs<br/>CliCommand trait → dispatch"]
        UI["display.rs · overrides.rs · rpc_setup.rs<br/>tables · config merge · RPC init"]
    end

    subgraph CORE["mev-scout-core (library)"]
        direction TB

        subgraph ORCH["Orchestration"]
            PIPE["pipeline<br/>BacktestRunner · run_block / run_range(_hybrid)<br/>aggregate → metrics · gas model"]
        end

        subgraph DETECT["Detection"]
            MEV["mev::detectors<br/>two-hop · multi-hop · sandwich<br/>JIT · JIT-arb · liquidation · mempool"]
            POOL["pool<br/>state: PoolManager (reserves/ticks)<br/>discovery: V2/V3/V4/Curve/Solidly/…<br/>math: AMM curves per DEX"]
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

        subgraph SUPPORT["Support"]
            CFG["config<br/>TOML settings + validation"]
            TYPES["types<br/>MevOpportunity · Strategy · GasConfig · ResultsFile"]
            SIGS["sigs — 4byte signature resolver"]
            MISC["coingecko · data · error · dex_type · utils"]
        end
    end

    MAIN --> CLIDEF --> DISPATCH
    DISPATCH --> UI
    DISPATCH --> CFG

    DISPATCH --> RESOLVER
    DISPATCH --> FETCH
    DISPATCH --> CHAIN
    DISPATCH --> POOL
    DISPATCH --> PIPE

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
    CHAIN --> RPC
    CFG --> TYPES
    PIPE --> TYPES
    FETCH -.-> SIGS
```

**Key relationships**

- `pipeline` is the hub: `BacktestRunner` owns `BlockReplayer` + `PoolManager` and drives every detector per transaction.
- `cache` (SQLite) is the local-first backbone — fetch stores blocks there; replay and the runner read from it; pool discovery persists pools/tokens into it.
- `rpc` fronts the chain for everything: fetching, `eth_call` pool state, log scans, and replay's on-demand state misses (via `CachedRpcDb`).

---

## 2. CLI command map

| Command | Purpose | Chain access | Writes |
|---|---|---|---|
| `run` | Full backtest → opportunities | yes (fetch + eth_call) | SQLite cache, results JSON |
| `live` | Stream new blocks, detect as they arrive | yes | SQLite cache, results JSON |
| `fetch` | Pre-cache blocks only (no detection) | yes | SQLite cache |
| `discover` | Find pools (on-chain factories / aggregators) | yes (RPC and/or REST) | SQLite cache |
| `validate-pools` | Measure discovery accuracy vs GeckoTerminal | yes (RPC + REST) | SQLite cache |
| `tokens` | Populate / view token metadata cache | no (cache-only) | — (reads cache) |
| `scan` | Raw event scans (trades/whales/flashloans/liqs) | yes (getLogs only) | — |
| `replay` | Debug a single block through revm | yes (fallback calls) | — |
| `report` | Re-render a saved run's JSON | no | — |
| `config` | Print fully-resolved TOML | no | — |

---

## 3. Command workflows (per command)

### 3.1 `run` — the full backtest

The main pipeline. Everything is cached first, then replayed and detected.

```
mev-scout run --days 7 [--batch-rpc]
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
    P --> Q["ResultsFile → export JSON<br/>render results table<br/>render block summary table"]
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

### 3.2 `live` — real-time streaming detection

Same engine, one-shot or continuous polling (`--loop [--duration 1h] [--poll-interval-ms 2000]`).

```mermaid
flowchart TB
    A["validate_live + init_rpc<br/>+ open cache"] --> B["read pool addresses<br/>from discovery cache"]
    B --> C{"--loop?"}
    C -- "no (one-shot)" --> D["tip = get_block_number"]
    C -- "yes" --> E["init: tip, PoolManager,<br/>runner, Aave prefetch"]
    D --> F["init pools at tip−1<br/>+ Aave prefetch"]
    F --> G["fetch tip block"] --> H["detect_state_horizon<br/>→ run_range_hybrid"] --> I["save JSON<br/>print table"]
    E --> J["poll loop"]
    J --> K["sleep(poll_interval)"]
    K --> L["get_block_number"]
    L --> M{"tip > last_block?"}
    M -- no --> K
    M -- yes --> N["fetch blocks last+1..tip<br/>(fetch_relevant if pools known)"]
    N --> O["run_range_hybrid<br/>FullReplay within state horizon,<br/>LogOnly beyond"]
    O --> P{"error?"}
    P -- "yes (<5 consecutive)" --> K
    P -- no --> Q["save live_{epoch}.json<br/>print per-range summary"]
    Q --> R{"deadline reached?"}
    R -- no --> K
    R -- yes --> S["session summary<br/>(blocks, txs, opportunities)"]
    P -- "≥5 consecutive" --> T["bail out"]
```

### 3.3 `fetch` — pre-cache blocks only

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

### 3.4 `discover` — build the pool universe

Finds pools from factory events and/or free aggregators; result feeds all other commands (they read the discovery cache).

```
mev-scout discover --days 30 [--source onchain|remote|hybrid] [--enrich] [--incremental]
```

```mermaid
flowchart TB
    A["resolve_chain + init_rpc<br/>+ open cache + warm TokenCache"] --> B{"--source"}
    B -- "remote" --> R["skip block range & on-chain scan"]
    B -- "onchain / hybrid" --> C["resolve range<br/>(default: pool_discovery_start_block → tip)"]
    B -- "hybrid / remote" --> R2["remote leg (below)"]
    C --> D{"--incremental?"}
    D -- yes --> E["from = max cached creation_block + 1<br/>(skip if cache is current)"]
    D -- no --> F
    E --> F["Phase 1: discover_and_cache<br/>factory event scan (chunked getLogs):<br/>V2 · V3 · V4 · Solidly · Camelot<br/>Curve registry · Balancer vault<br/>TraderJoe LB · Pendle<br/>+ pool metadata via Multicall3"]
    F --> G
    R --> G["merge sources"]
    R2 --> H["Phase 2: remote aggregators<br/>GeckoTerminal + DexScreener<br/>(--max-pools, --min-tvl)"]
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

### 3.5 `validate-pools` — discovery accuracy audit

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

### 3.6 `tokens` — token metadata cache

Cache-only command; resolves nothing new on-chain at runtime (the bundled known-token list + previously resolved symbols).

```
mev-scout tokens [--symbol USDC] [--decimals 6] [--limit 100] [--cache-only]
```

```mermaid
flowchart LR
    A["resolve chain → chain_id"] --> B["open SQLite cache"]
    B --> C["TokenCache::warm(chain_id)<br/>bundled known tokens"]
    C --> D["merge persisted tokens<br/>from SQLite"]
    D --> E["filter by --symbol substring<br/>and --decimals"]
    E --> F["sort by address, truncate --limit"]
    F --> G{"--cache-only?"}
    G -- yes --> H["print count only"]
    G -- no --> I["table / json / csv"]
```

### 3.7 `scan` — raw on-chain event scans

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

### 3.8 `replay` — single-block EVM debugger

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

### 3.9 `report` — re-render saved results

Offline. Reads a `ResultsFile` JSON written by `run`/`live` and re-renders it — no chain access.

```
mev-scout report [--run-id run_1717...]
```

```mermaid
flowchart LR
    A["export dir from config<br/>(output.export_path)"] --> B{"--run-id given?"}
    B -- no --> C["pick newest *.json<br/>(by file creation time)"]
    B -- yes --> D["{run_id}.json"]
    C --> E["read + parse ResultsFile"]
    D --> E
    E --> F{"output format"}
    F -- table --> G["run header +<br/>render_results_table"]
    F -- csv --> H["CSV rows:<br/>block, tx_index, strategy,<br/>input, profit, gas, confidence"]
    F -- json --> I["pretty-print full file"]
```

### 3.10 `config` — print resolved config

Offline, two lines of logic: merges TOML file (if any) with CLI overrides in `main.rs`, then serializes.

```
mev-scout config [-f custom.toml] [--verbose]
```

```mermaid
flowchart LR
    A["main.rs: load -f file,<br/>mev-scout.toml, or defaults"] --> B["merge CLI overrides<br/>(overrides.rs)"] --> C["to_toml_string → stdout"]
```

---

## 4. The engine core: how detection works

`BacktestRunner.run_block` (pipeline/runner.rs) is the heart of `run` and `live`:

1. **Load** block + txs from SQLite.
2. **Filter** — only txs whose `to` or log emitter matches a tracked pool/token are replayed through revm; all others are synthesized from cached receipts (the main performance optimization for large backtests).
3. **Apply** decoded Swap/Sync/Mint/Burn events to `PoolManager`, so all detectors see post-tx reserves.
4. **Detect** per tx, in order: two-hop arb → multi-hop arb (BFS ≤ depth 4) → JIT liquidity → sandwich → JIT+arb hybrid; liquidation and mempool strategies where context allows.
5. **Post-process** — dust filter (`min_profit_wei`), per-tx candidate cap, cross-block persistence scoring (decaying confidence, `PERSISTENCE_DECAY = 0.75`, grace 5 blocks), gas calibration from observed `gasUsed`.

The hybrid path (`run_range_hybrid`, used by `live`) picks `FullReplay` vs `LogOnly` per block based on `rpc.detect_state_horizon` — blocks deeper than available archive state skip EVM execution and lose only the EVM-context strategies.

## 5. Persistent artifacts

| Artifact | Produced by | Consumed by |
|---|---|---|
| SQLite `cache.db` (blocks, receipts, state, discovered pools, tokens, manifests) | `run`, `live`, `fetch`, `discover`, `validate-pools` | `run`, `live`, `replay`, `discover` (incremental), `tokens` |
| `results/*.json` (`run_{epoch}` / `live_{epoch}`) | `run`, `live` | `report` |
| Signature DB (4byte directory snapshot) | `fetch` (unless `--no-sig-resolve`) | tx decoding |
