# MEV Scout — Codebase Architecture

## 1. Crate Dependency Graph

```mermaid
flowchart TB
    subgraph workspace["Cargo Workspace"]
        cli["mev-scout-cli\n(binary crate)\ncli/src/main.rs"]
        core["mev-scout-core\n(library crate)\ncore/src/*.rs"]
    end

    cli --> core
    cli --> clap
    cli --> tracing
    cli --> indicatif
    cli --> comfy_table["comfy-table"]
    core --> revm
    core --> alloy
    core --> rusqlite
    core --> tokio
    core --> arrow_parquet["arrow / parquet"]
    core --> serde
    core --> reqwest
    core --> clap_core["clap (arg defs)"]
    core --> bincode
    core --> futures
    core --> thiserror
    core --> anyhow
```

## 2. End-to-End Data Pipeline

```mermaid
flowchart LR
    Config["config.toml\n+ CLI args"] --> Validation["validation.rs\nresolve & validate"]
    Validation --> Fetcher["fetch.rs\nFetcher"]

    subgraph Fetch["Fetch Phase"]
        Fetcher --> Scanner["scan.rs\nActivityScanner\n(log-first: DEX events)"]
        Scanner --> SQLite["cache.rs\nSqliteStore\n(SQLite + bincode blobs)"]
        Fetcher --> Parquet["parquet_writer.rs\nParquetWriter\n(ZSTD compressed)"]
        Fetcher --> RPC["rpc.rs\nRpcClient\n(retry + URL rotation)"]
        RPC --> Scanner
        RPC --> SQLite
    end

    SQLite --> Replayer["replay.rs\nBlockReplayer\n(revm EVM)"]
    Replayer --> CachedRpcDb["replay.rs\nCachedRpcDb\n(revm Database trait)"]
    CachedRpcDb --> SQLite
    CachedRpcDb --> RPC

    Replayer --> PoolMgr["pool/state.rs\nPoolManager"]

    subgraph Discover["Pool Discovery"]
        Disc["pool/discovery.rs\non-chain discovery"] --> PoolMgr
        Subgraph["pool/subgraph_discovery.rs\nsubgraph endpoints"] --> Disc
    end

    subgraph Detect["Detection Phase (per block)"]
        PoolMgr --> TwoHop["mev/two_hop.rs\nTwoHopArbDetector"]
        PoolMgr --> MultiHop["mev/multi_hop.rs\nMultiHopArbDetector"]
        PoolMgr --> Sandwich["mev/sandwich.rs\nSandwichDetector"]
        PoolMgr --> JIT["mev/jit.rs\nJitDetector"]
        PoolMgr --> JitArb["mev/jit_arb.rs\nJitArbDetector"]
        PoolMgr --> Liq["mev/liquidation.rs\nLiquidationDetector"]
        PoolMgr --> CrossBlock["mev/cross_block.rs\nCrossBlockDetector"]
        Replayer --> PGA["mev/pga.rs\nPGA Simulator"]
        Replayer --> TwoHop
        Replayer --> MultiHop
        Replayer --> Sandwich
        Replayer --> JIT
        Replayer --> JitArb
        Replayer --> Liq
        Replayer --> CrossBlock
    end

    subgraph Pending["Mempool / Pending Block"]
        Mempool["mev/mempool.rs\nPendingBlockCapture"] --> RPC
        Mempool --> PoolMgr
        Mempool --> MultiHop
        Mempool --> TwoHop
    end

    TwoHop --> Opportunities["mev/opportunity.rs\nMevOpportunity[]"]
    MultiHop --> Opportunities
    Sandwich --> Opportunities
    JIT --> Opportunities
    JitArb --> Opportunities
    Liq --> Opportunities
    PGA --> Opportunities
    CrossBlock --> Opportunities
    Mempool --> Opportunities

    Opportunities --> FactCheck["fact_check.rs\nverify_opportunities()"]
    FactCheck --> RPC

    Opportunities --> Aggregator["aggregate.rs\naggregate_with_prices()"]
    Aggregator --> PriceOracle["coingecko.rs\nPriceCache\n(on-chain / CoinGecko)"]
    Aggregator --> GasDist["gas_distribution.rs\nGasPriceDistribution\n(H10 percentiles)"]

    Aggregator --> Output["Results\n(JSON / CSV / Table)"]

    subgraph Live["Live Mode"]
        LiveRunner["live.rs\nLiveRunner"] --> Run["run.rs\nBacktestRunner"]
        LiveRunner --> Mempool
        LiveRunner --> Replayer
    end
```

## 3. Core Module Hierarchy

```mermaid
flowchart TB
    subgraph core["core/"]
        types["types.rs\nChainName, Strategy,\nGasConfig, OutputFormat"]
        config["config.rs\nConfig, ChainConfig,\nCliOverrides"]
        cli_def["cli.rs\nCli, Command"]
        validation["validation.rs\nValidationResult"]
        data["data.rs\nBlockData, TxData,\nReceiptData, LogData"]
        utils["utils.rs\nu128_from_be_bytes"]
        gas_dist["gas_distribution.rs\nGasPriceDistribution"]
        live["live.rs\nLiveRunner, LiveConfig"]

        subgraph infra["Infrastructure"]
            rpc["rpc.rs\nRpcClient"]
            cache["cache.rs\nSqliteStore"]
            parquet["parquet_writer.rs\nParquetWriter"]
            scan["scan.rs\nActivityScanner"]
            resolver["resolver.rs\nRangeResolver"]
        end

        subgraph pool["Pool Management"]
            state["state.rs\nPoolManager, PoolState\nV2/V3/Curve/Balancer"]
            math["math.rs\nAMM formulas,\noptimal arb amounts"]
            v3_quote["v3_quote.rs\nV3 exact-in/out\nquoting"]
            decoders["decoders.rs\nEvent log decoders"]
            discovery["discovery.rs\nPool discovery"]
            subgraph_disc["subgraph_discovery.rs\nSubgraphEndpoint config"]
            curve_math["curve_math.rs\nCurve AMM formulas"]
            balancer_math["balancer_math.rs\nBalancer AMM formulas"]
            dex_type["dex_type.rs\nDexType enum"]
        end

        subgraph mev["MEV Detection"]
            two_hop["two_hop.rs\nTwo-hop arbitrage"]
            multi_hop["multi_hop.rs\nMulti-hop arbitrage"]
            sandwich["sandwich.rs\nSandwich detection"]
            jit["jit.rs\nJIT liquidity"]
            jit_arb["jit_arb.rs\nJIT arbitrage"]
            liquidation["liquidation.rs\nAave V3 liquidation"]
            pga["pga.rs\nPGA simulation"]
            block_builder["block_builder.rs\nBundle packing"]
            opportunity["opportunity.rs\nMevOpportunity"]
            cross_block["cross_block.rs\nCross-block arb\n+ time-bandit"]
            mempool["mempool.rs\nPending block capture\n+ mempool arb detection"]
        end

        replay["replay.rs\nBlockReplayer"]
        fetch["fetch.rs\nFetcher"]
        run["run.rs\nBacktestRunner"]
        aggregate["aggregate.rs\nAggregation"]
        fact_check["fact_check.rs\nVerification"]
        coingecko["coingecko.rs\nPrice oracle"]
    end

    subgraph cli_app["cli/"]
        main["main.rs\nEntry point,\n8 subcommands"]
    end

    main --> run
    main --> fetch
    main --> fact_check
    main --> live
    run --> state
    run --> replay
    run --> two_hop
    run --> multi_hop
    run --> sandwich
    run --> jit
    run --> jit_arb
    run --> liquidation
    run --> pga
    run --> cross_block
    run --> mempool
    run --> discovery
    run --> resolver
    run --> rpc
    run --> cache
    run --> gas_dist
    run --> opportunity

    fetch --> cache
    fetch --> scan
    fetch --> rpc
    fetch --> resolver
    fetch --> parquet

    replay --> cache
    replay --> rpc
    replay --> data

    live --> run
    live --> mempool
    live --> rpc
    live --> cache
    live --> replay

    state --> rpc
    state --> decoders
    state --> dex_type
    state --> utils
    state --> data

    math --> state
    math --> v3_quote
    math --> curve_math
    math --> balancer_math

    discovery --> cache
    discovery --> dex_type
    discovery --> rpc
    discovery --> scan

    subgraph_disc --> dex_type
    subgraph_disc --> discovery

    curve_math --> state
    balancer_math --> state

    two_hop --> state
    two_hop --> math
    two_hop --> v3_quote
    two_hop --> opportunity

    multi_hop --> state
    multi_hop --> math
    multi_hop --> v3_quote
    multi_hop --> opportunity

    sandwich --> state
    sandwich --> math
    sandwich --> v3_quote
    sandwich --> opportunity
    sandwich --> data
    sandwich --> decoders
    sandwich --> utils

    jit --> state
    jit --> v3_quote
    jit --> opportunity
    jit --> data
    jit --> decoders

    jit_arb --> state
    jit_arb --> math
    jit_arb --> v3_quote
    jit_arb --> opportunity
    jit_arb --> data
    jit_arb --> decoders

    liquidation --> state
    liquidation --> opportunity
    liquidation --> rpc
    liquidation --> data

    cross_block --> opportunity
    cross_block --> state
    cross_block --> types

    mempool --> state
    mempool --> math
    mempool --> rpc
    mempool --> two_hop
    mempool --> multi_hop
    mempool --> opportunity
    mempool --> data
    mempool --> utils

    aggregate --> opportunity
    aggregate --> coingecko

    fact_check --> rpc
    fact_check --> state
    fact_check --> v3_quote
    fact_check --> math
    fact_check --> opportunity
```

## 4. MEV Detection Strategy Flow

```mermaid
flowchart TB
    subgraph block["Single Block Processing"]
        replayed["ExecutedBlock\n(replayed via revm)"] --> logs["ExecutedLog[]"]
        logs --> decode["pool/decoders.rs\nevent decoding"]

        decode --> v2_swaps["V2 Sync/Swap\nV3 Swap/Mint/Burn\nCurve TokenExchange\nBalancer Swap"]

        v2_swaps --> pool_update["pool/state.rs\nupdate_from_logs()"]
        pool_update --> pool_state["PoolState (mutated)"]
    end

    subgraph detection["Per-Detector Invocation"]
        pool_state --> two_hop_detect["TwoHopArbDetector\ndetect()"]
        pool_state --> multi_hop_detect["MultiHopArbDetector\ndetect()"]
        pool_state --> sand_detect["SandwichDetector\ndetect()"]
        pool_state --> jit_detect["JitDetector\ndetect()"]
        pool_state --> jitarb_detect["JitArbDetector\ndetect()"]
        pool_state --> liq_detect["LiquidationDetector\ndetect()"]
        pool_state --> xblock_detect["CrossBlockDetector\ndetect()"]
    end

    subgraph mempool_flow["Mempool Detection"]
        pending["PendingBlockCapture\ncapture_pending_block()"] --> pending_decode["Decode pending\ntransactions"]
        pending_decode --> pending_pool["PoolState\n(speculative)"]
        pending_pool --> mempool_2hop["Mempool two-hop\ndetection"]
        pending_pool --> mempool_mhop["Mempool multi-hop\ndetection"]
    end

    subgraph output["Opportunity Collection"]
        two_hop_detect --> opps["Vec<MevOpportunity>"]
        multi_hop_detect --> opps
        sand_detect --> opps
        jit_detect --> opps
        jitarb_detect --> opps
        liq_detect --> opps
        xblock_detect --> opps
        mempool_2hop --> opps
        mempool_mhop --> opps

        opps --> pga_sim["mev/pga.rs\nsimulate_pga()"]
        opps --> reuse["reused for\nnext block"]
    end

    opps --> post_block["End of block\n→ Aggregate\n→ Fact Check\n→ Output"]
```

## 5. Key Data Types

```mermaid
classDiagram
    class ChainName {
        Polygon
        Avalanche
        Bsc
        Arbitrum
        Base
        Ethereum
        Optimism
    }

    class Strategy {
        TwoHopArb
        MultiHopArb
        Jit
        JitArb
        Sandwich
        Liquidation
        CrossBlockArb
        TimeBandit
    }

    class DexType {
        UniswapV2
        UniswapV3
        Curve
        Balancer
    }

    class PoolState {
        <<enum>>
        UniswapV2(UniswapV2PoolState)
        UniswapV3(UniswapV3PoolState)
        Curve(CurvePoolState)
        Balancer(BalancerPoolState)
    }

    class MevOpportunity {
        strategy: Strategy
        block_number: u64
        tx_index: u64
        profit: U256
        gas_used: u64
        path: Vec~Address~
        +with_canonical_id()
    }

    class PoolManager {
        pools: HashMap~Address, PoolState~
        +init_from_rpc()
        +update_from_logs()
        +arbitrage_pairs()
    }

    class BlockReplayer {
        db: CachedRpcDb
        executor: revm::Evm
        +replay_block() -> ExecutedBlock
    }

    class CrossBlockDetector {
        window_size: u64
        snapshots: Vec~PoolSnapshot~
        +detect() -> Vec~MevOpportunity~
    }

    class LiveRunner {
        config: LiveConfig
        state: LiveRunnerState
        +run_live() -> ExecutionRecord[]
    }

    PoolManager --> PoolState
    PoolManager --> DexType
    MevOpportunity --> Strategy
    MevOpportunity --> DexType
```

## 6. Configuration & CLI Structure

```mermaid
flowchart LR
    subgraph Cli["CLI (clap)"]
        Run["Run\n(backtest)"]
        Fetch["Fetch\n(blocks only)"]
        Report["Report\n(results)"]
        Config["Config\n(validate)"]
        ReplayCmd["Replay\n(single block)"]
        Discover["Discover\n(pools)"]
        FactCheck["FactCheck\n(verify)"]
        LiveCmd["Live\n(live MEV bot)"]
    end

    subgraph ConfigFile["config.toml"]
        chain["chain\n+ chain-specific\naddresses"]
        strategies["strategies\n(which to run)"]
        gas["gas model\nhistorical/P90/fixed"]
        flashloan["flash loan\nprovider + fee"]
        price["price oracle\nmode"]
        range["block range\nmode"]
        live_cfg["live mode\nwallet, RPC, mempool"]
    end

    ConfigFile --> validation["validation.rs\nvalidate_and_resolve()"]
    Cli --> validation
    validation --> run_plan["Resolved Run Plan"]
```

## 7. Pool Management Detail

```mermaid
flowchart TB
    subgraph init["Pool Initialization"]
        subgraph_disc["subgraph_discovery.rs\nSubgraphEndpoints\n(per-chain config)"]
        discover["discovery.rs\ndiscover_pools_in_range()"]
        subgraph_disc --> discover
        discover --> pool_addrs["DiscoveredPool[]"]
        pool_addrs --> state_init["state.rs\ninit_from_rpc()"]
        state_init --> eth_call["eth_call\n(live state)"]
        state_init --> storage_fallback["eth_getStorageAt\n(fallback)"]
    end

    subgraph update["Live Update (per block)"]
        logs["ExecutedLog[]"] --> decoders["decoders.rs\nevent decode"]
        decoders --> v3_swap["V3 Swap →\nsqrt_price_x96, tick, liquidity"]
        decoders --> v3_mint["V3 Mint/Burn →\nliquidity delta"]
        decoders --> v2_sync["V2 Sync →\nreserve0, reserve1"]
        decoders --> curve_swap["Curve Swap →\nbalances update"]
        decoders --> bal_swap["Balancer Swap →\nbalances update"]

        v3_swap --> pool_state["PoolState\n(updated in-place)"]
        v3_mint --> pool_state
        v2_sync --> pool_state
        curve_swap --> pool_state
        bal_swap --> pool_state
    end

    subgraph quoting["Quoting"]
        pool_state --> v3_quote["v3_quote.rs\nquote_v3_exact_in()\nquote_v3_exact_out()"]
        pool_state --> curve_math["curve_math.rs\ncurve_output_amount()"]
        pool_state --> balancer_math["balancer_math.rs\nbalancer_quote_exact_in()"]
        pool_state --> math["math.rs\nquote_exact_in()\nconstant_product_*()\noptimal_two_hop_arb()"]
        math --> v3_quote
        math --> curve_math
        math --> balancer_math
    end
```

## File Size Overview

| File | Lines | Module | Purpose |
|---|---|---|---|
| `pool/state.rs` | ~2,255 | Core | Pool manager + all pool state structs |
| `integration.rs` | ~1,324 | Tests | Integration tests |
| `fact_check.rs` | ~1,225 | Core | On-chain opportunity verification |
| `replay.rs` | ~1,176 | Core | EVM block replayer + CachedRpcDb |
| `cache.rs` | ~1,151 | Core | SQLite-backed block/state cache |
| `pool/v3_quote.rs` | ~1,031 | Pool | Uniswap V3 quoting engine |
| `config.rs` | ~990 | Core | Config struct, chain configs, CLI overrides |
| `rpc.rs` | ~982 | Infra | Multi-provider RPC client + rate limiter |
| `liquidation.rs` | ~930 | MEV | Aave V3 liquidation detection |
| `types.rs` | ~915 | Core | ChainName, Strategy, GasConfig, etc. |
| `two_hop.rs` | ~806 | MEV | Two-hop arbitrage detection |
| `sandwich.rs` | ~791 | MEV | Sandwich attack detection |
| `run.rs` | ~667 | Core | BacktestRunner orchestration |
| `pool/discovery.rs` | ~659 | Pool | Pool discovery |
| `live.rs` | ~646 | Core | Live MEV bot runner |
| `jit_arb.rs` | ~617 | MEV | JIT arbitrage detection |
| `jit.rs` | ~605 | MEV | JIT liquidity detection |
| `aggregate.rs` | ~580 | Core | USD aggregation + metrics |
| `pool/subgraph_discovery.rs` | ~572 | Pool | Subgraph endpoint configuration |
| `pool/math.rs` | ~545 | Pool | AMM math + unified quote_exact_in |
| `mev/mempool.rs` | ~538 | MEV | Pending block capture + mempool arb |
| `parquet_writer.rs` | ~510 | Infra | Parquet (ZSTD) block data writer |
| `validation.rs` | ~506 | Core | Config validation + resolution |
| `multi_hop.rs` | ~487 | MEV | Multi-hop arbitrage detection |
| `fetch.rs` | ~423 | Core | Fetcher — block data fetching |
| `mev/opportunity.rs` | ~400 | MEV | MevOpportunity struct + ResultsFile |
| `pool/decoders.rs` | ~395 | Pool | Event log decoders |
| `pool/curve_math.rs` | ~355 | Pool | Curve AMM math (StableSwap + CryptoSwap) |
| `coingecko.rs` | ~319 | Core | CoinGecko USD pricing with cache |
| `data.rs` | ~287 | Core | Wire-format data types |
| `cli.rs` | ~282 | Core | CLI argument parsing (clap) |
| `mev/cross_block.rs` | ~252 | MEV | Cross-block MEV detection |
| `pool/balancer_math.rs` | ~249 | Pool | Balancer AMM math |
| `mev/block_builder.rs` | ~217 | MEV | Bundle packing into blocks |
| `gas_distribution.rs` | ~183 | Core | Gas price distribution |
| `resolver.rs` | ~180 | Infra | Block range resolution |
| `scan.rs` | ~145 | Infra | DEX activity scanner |
| `mev/pga.rs` | ~141 | MEV | PGA simulation |
| `utils.rs` | ~46 | Core | u128_from_be_bytes utility |
| `pool/dex_type.rs` | ~31 | Pool | DexType enum |
| `main.rs` | ~1,520 | CLI | CLI entry point |
| `e2e.rs` | ~492 | Tests | End-to-end tests |
