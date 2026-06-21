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
    core --> revm
    core --> alloy
    core --> rusqlite
    core --> tokio
    core --> arrow_parquet["arrow / parquet"]
    core --> serde
    core --> reqwest
    core --> clap_core["clap (arg defs)"]
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

    subgraph Detect["Detection Phase (per block)"]
        PoolMgr --> TwoHop["mev/two_hop.rs\nTwoHopArbDetector"]
        PoolMgr --> MultiHop["mev/multi_hop.rs\nMultiHopArbDetector"]
        PoolMgr --> Sandwich["mev/sandwich.rs\nSandwichDetector"]
        PoolMgr --> JIT["mev/jit.rs\nJitDetector"]
        PoolMgr --> JitArb["mev/jit_arb.rs\nJitArbDetector"]
        PoolMgr --> Liq["mev/liquidation.rs\nLiquidationDetector"]
        Replayer --> PGA["mev/pga.rs\nPGA Simulator"]
        Replayer --> TwoHop
        Replayer --> MultiHop
        Replayer --> Sandwich
        Replayer --> JIT
        Replayer --> JitArb
        Replayer --> Liq
    end

    TwoHop --> Opportunities["mev/opportunity.rs\nMevOpportunity[]"]
    MultiHop --> Opportunities
    Sandwich --> Opportunities
    JIT --> Opportunities
    JitArb --> Opportunities
    Liq --> Opportunities
    PGA --> Opportunities

    Opportunities --> FactCheck["fact_check.rs\nverify_opportunities()"]
    FactCheck --> RPC

    Opportunities --> Aggregator["aggregate.rs\naggregate_with_prices()"]
    Aggregator --> PriceOracle["coingecko.rs\nPriceCache\n(on-chain / CoinGecko)"]
    Aggregator --> GasDist["gas_distribution.rs\nGasPriceDistribution\n(H10 percentiles)"]

    Aggregator --> Output["Results\n(JSON / CSV / Table)"]
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
        end

        replay["replay.rs\nBlockReplayer"]
        fetch["fetch.rs\nFetcher"]
        run["run.rs\nBacktestRunner"]
        aggregate["aggregate.rs\nAggregation"]
        fact_check["fact_check.rs\nVerification"]
        coingecko["coingecko.rs\nPrice oracle"]
    end

    subgraph cli_app["cli/"]
        main["main.rs\nEntry point,\n7 subcommands"]
    end

    main --> run
    main --> fetch
    main --> fact_check
    run --> state
    run --> replay
    run --> two_hop
    run --> multi_hop
    run --> sandwich
    run --> jit
    run --> jit_arb
    run --> liquidation
    run --> pga
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

    state --> rpc
    state --> decoders
    state --> dex_type
    state --> utils

    two_hop --> state
    two_hop --> math
    two_hop --> v3_quote
    two_hop --> opportunity

    multi_hop --> state
    multi_hop --> math
    multi_hop --> v3_quote
    multi_hop --> opportunity

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
    end

    subgraph output["Opportunity Collection"]
        two_hop_detect --> opps["Vec<MevOpportunity>"]
        multi_hop_detect --> opps
        sand_detect --> opps
        jit_detect --> opps
        jitarb_detect --> opps
        liq_detect --> opps

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
    end

    subgraph ConfigFile["config.toml"]
        chain["chain\n+ chain-specific\naddresses"]
        strategies["strategies\n(which to run)"]
        gas["gas model\nhistorical/P90/fixed"]
        flashloan["flash loan\nprovider + fee"]
        price["price oracle\nmode"]
        range["block range\nmode"]
    end

    ConfigFile --> validation["validation.rs\nvalidate_and_resolve()"]
    Cli --> validation
    validation --> run_plan["Resolved Run Plan"]
```

## 7. Pool Management Detail

```mermaid
flowchart TB
    subgraph init["Pool Initialization"]
        discover["discovery.rs\ndiscover_pools_in_range()"] --> pool_addrs["DiscoveredPool[]"]
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
        pool_state --> math["math.rs\nconstant_product_*()\noptimal_two_hop_arb()"]
    end
```

## File Size Overview

| File | Lines | Module | Purpose |
|---|---|---|---|
| `pool/state.rs` | ~2,215 | Core | Pool manager + all pool state structs |
| `replay.rs` | ~1,344 | Core | EVM block replayer + CachedRpcDb |
| `fact_check.rs` | ~1,327 | Core | On-chain opportunity verification |
| `v3_quote.rs` | ~1,088 | Core | Uniswap V3 quoting engine |
| `integration.rs` | ~1,041 | Tests | Integration tests |
| `liquidation.rs` | ~903 | MEV | Aave V3 liquidation detection |
| `two_hop.rs` | ~951 | MEV | Two-hop arbitrage detection |
| `jit_arb.rs` | ~627 | MEV | JIT arbitrage detection |
| `jit.rs` | ~598 | MEV | JIT liquidity detection |
| `sandwich.rs` | ~680 | MEV | Sandwich attack detection |
| `aggregate.rs` | ~571 | Core | USD aggregation + metrics |
| `discovery.rs` | ~569 | Pool | Pool discovery |
| `multi_hop.rs` | ~489 | MEV | Multi-hop arbitrage |
| `math.rs` | ~472 | Pool | AMM math formulas |
| `decoders.rs` | ~395 | Pool | Event log decoders |
| `main.rs` | ~1,800 | CLI | CLI entry point |
