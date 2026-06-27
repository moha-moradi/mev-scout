# Live Mode — Plan

## Overview

Add a new `live` subcommand that turns MEV Scout into a **virtual live MEV bot**. Instead of replaying historical blocks, it connects to the live chain and runs two parallel workloads each cycle:

1. **Settled block processing** — Each new finalized block is fetched and replayed through the full `BacktestRunner::run_block()` pipeline, exercising **all detection strategies** (TwoHopArb, MultiHopArb, Sandwich, JIT, JitArb, Liquidation, CrossBlock). Pool state is updated authoritatively from the block's execution logs.
2. **Mempool scanning** — Pending transactions are fetched and their pool impact is estimated (via revm simulation or calldata parsing). Arb-only detection runs on the projected post-mempool state.

A **virtual wallet** tracks per-token balances and P&L across all simulated trades. No real transactions are broadcast — everything is simulated via the existing revm engine, so no private keys or wallet setup is needed.

---

## 1. New Data Structures

### File: `core/src/live.rs`

```rust
pub struct LiveConfig {
    pub initial_balance_wei: U256,        // Starting virtual balance in wei
    pub min_profit_threshold_wei: U256,   // Min profit to execute a trade
    pub poll_interval_ms: u64,            // How often to poll mempool (default 1000)
    pub max_executions: Option<u64>,      // Optional cap on virtual trades
    pub strategies: Vec<Strategy>,        // Which strategies to detect
    pub gas_config: GasConfig,            // Gas price model, limit, priority fee
}

pub struct ExecutionRecord {
    pub opportunity: MevOpportunity,
    pub simulated_profit_wei: U256,
    pub simulated_gas_cost_wei: u128,
    pub token_in: Address,           // Token spent
    pub token_out: Address,          // Token received
    pub input_amount: U256,
    pub output_amount: U256,
    pub block_number: u64,
    pub timestamp: u64,
    pub success: bool,
}

pub struct LiveRunnerState {
    /// Native token balance (e.g. ETH, MATIC)
    pub native_balance_wei: U256,
    /// Per-token balances for non-native assets (token_addr -> balance)
    pub token_balances: HashMap<Address, U256>,
    pub initial_native_balance_wei: U256,
    pub total_executions: u64,
    pub successful_executions: u64,
    pub failed_executions: u64,
    pub total_profit_wei: U256,
    pub total_gas_spent_wei: u128,
    /// Execution history (capped at last 10k records to bound memory)
    pub execution_history: Vec<ExecutionRecord>,
}
```

> **Virtual wallet assumptions**: The wallet assumes unlimited/infinite token approval on all known DEX routers (no real approval tx is needed). WETH ↔ ETH wrapping/unwrapping is supported via helper methods `wrap_native()` / `unwrap_native()` on `LiveRunner` — opportunities routing through WETH use these to keep `native_balance` and `token_balances` consistent.

---

## 2. New Module: `core/src/live.rs` — LiveRunner

### Design principle
A real MEV bot must process **settled blocks** (to stay synchronized with on-chain state and detect all MEV types) **and** scan the mempool (for pre-execution arb visibility). The `LiveRunner` does both each cycle, with settled blocks as the authoritative state source.

### Internal architecture

`LiveRunner` embeds a `BacktestRunner` to reuse its full block replay + detection pipeline:

```rust
pub struct LiveRunner {
    config: LiveConfig,
    rpc: RpcClient,
    pool_manager: PoolManager,       // authoritative state (synced from settled blocks)
    backtest_runner: BacktestRunner,  // wraps BlockReplayer for settled block replay
    wallet: LiveRunnerState,
    last_processed_block: u64,
    last_resync_block: u64,
}
```

### Initialization flow
1. Load pool definitions from cache (or discover from current chain state)
2. Fetch latest finalized block number via RPC
3. Initialize pool manager at that block (same as `BacktestRunner::init_pools`)
4. Construct `BlockReplayer` with a read-through cache (fetches uncached blocks from RPC on demand)
5. Construct `BacktestRunner` wrapping the replayer + pool manager + gas config
6. Optionally fetch Aave V3 reserves and CoinGecko prices
7. Set `last_processed_block` = latest finalized block
8. Return ready LiveRunner instance

### Main loop (`run()`)
`LiveRunner::run()` is an **`async fn`** — the loop runs on the tokio runtime. Block replay + detection is synchronous (reuses existing `BacktestRunner::run_block()`); only RPC and cache I/O are async.

```
loop {
    // ─── Phase A: Settled block processing ────────────────────────────
    //
    // This is the authoritative state sync. Every new finalized block is
    // fetched (from cache or RPC), replayed through revm, and fed through
    // ALL detectors via BacktestRunner::run_block(). Pool state is updated
    // from real execution logs, not estimation.
    //
    // Detectors exercised every settled block:
    //   - TwoHopArbDetector
    //   - MultiHopArbDetector
    //   - SandwichDetector
    //   - JitDetector
    //   - JitArbDetector
    //   - LiquidationDetector
    //   - CrossBlockDetector (+ TimeBandit)

    // 1. Get latest settled block number
    let latest_block = rpc.get_block_number().await;

    // 2. Reorg detection: if latest_block < last_processed_block, chain reorged.
    //    Re-initialize pool state from latest_block. Wallet is preserved.
    //    Reset last_processed_block = latest_block and skip to mempool phase.
    if latest_block < last_processed_block {
        handle_reorg(latest_block);
        continue;
    }

    // 3. Process each new settled block since last check.
    //    Uses Fetcher (existing) to fetch block data if not cached, then
    //    calls backtest_runner.run_block(block_num) which replays through
    //    revm and runs all detectors. Pool state is updated from real logs.
    while last_processed_block < latest_block {
        let block_num = last_processed_block + 1;
        fetch_if_uncached(block_num);                       // async RPC
        let (opportunities, stats, _) = backtest_runner
            .run_block(block_num)?;                         // sync (revm + detection)
        process_settled_opportunities(opportunities);        // virtual wallet execution
        last_processed_block = block_num;
    }

    // Optional: cross-block detection on the accumulated window
    let cross_opps = backtest_runner.detect_cross_block();
    process_settled_opportunities(cross_opps);

    // ─── Phase B: Mempool scanning ────────────────────────────────────
    //
    // Pool manager is now up-to-date from the latest settled block.
    // We fork the state and apply pending txs to look for arb opportunities
    // that would exist *if* those txs land in the next block.

    // 4. Capture pending block from mempool
    let pending = capture_pending_block(&rpc).await;
    if pending.txs.is_empty() { goto phase_c; }

    // 5. Fork pool manager for speculative state
    let mut speculative_state = pool_manager.clone();

    // 6. Apply pending tx effects to the fork.
    //    Two approaches (try revm first, fall back to calldata parsing):
    //
    //    a) [Preferred] Run pending txs through revm against an RPC-forked
    //       state. Capture storage writes to known pool contracts and decode
    //       into reserve/price updates.
    //
    //    b) [Fallback when pending tx count > threshold (e.g. 50)] Parse
    //       known DEX router calldata patterns:
    //       - V2: swapExactTokensForTokens / swapTokensForExactTokens
    //       - V3: exactInput / exactOutput path decoding
    //       - If parsing fails or pool is unknown, skip that tx.

    // 7. Fetch live gas prices for this cycle:
    //    - Use eth_gasPrice or base_fee from pending block
    //    - Use eth_maxPriorityFeePerGas for priority fee
    //    - Build GasConfig with GasModel::Live
    //    - flash_loan_provider is ignored (no flash loans in live mode)

    // 8. Run arb-only detection on the speculative state:
    //    - TwoHopArbDetector
    //    - MultiHopArbDetector
    //    - Skip log-based detectors (need real logs from settled blocks)

    // 9. For each mempool opportunity (sorted by profit descending):
    //    a. Check wallet: token_balances[token_in] >= input_amount AND
    //       native_balance_wei >= gas_cost_wei
    //    b. Simulate trade via revm against speculative state
    //    c. If simulated profit >= min_profit_threshold:
    //       - Update wallet balances (deduct token_in, add token_out, deduct gas)
    //       - Record ExecutionRecord
    //       - Apply swap effects to speculative state (for subsequent opps)
    //       - Increment execution counters
    //
    // 9b. Bankruptcy check: if native_balance_wei < min_gas_cost AND no
    //     token holdings can be swapped to native, log warning and auto-stop.

    // ─── Phase C: Output & sleep ──────────────────────────────────────

    // 10. Print dashboard (ANSI-clear + rewrite).
    //     In verbose mode: redirect tracing to live_{run_id}.log to avoid
    //     interleaving with the dashboard.

    // 11. Sleep for poll_interval_ms
}
```

### Pool state estimation for pending txs

Since pending txs have no execution receipts/logs, we use a **revm-first** approach:

1. **Primary: revm simulation** — Execute each pending tx against an RPC-forked EVM state (`eth_call` at the pending block). After execution, inspect the resulting state for storage writes to known pool contract addresses. Decode those writes into reserve/price updates and apply them to the cloned `PoolManager`. This catches all DEX interactions regardless of router complexity (aggregators, multi-hop, flash loans). The existing `BlockReplayer`'s revm pipeline can be adapted for this.

2. **Fallback: calldata parsing** — When revm simulation is too expensive (many pending txs, rate-limited RPC), fall back to parsing known DEX router calldata patterns:
   - **V2 swaps**: Parse `swapExactTokensForTokens` / `swapTokensForExactTokens` calldata for amountIn/amountOut, find the pool via token pair + factory, apply constant-product formula to estimate reserve changes.
   - **V3 swaps**: Parse `exactInput` / `exactOutput` calldata for path (pool + fee), use sqrt_price from current pool state to estimate price impact.
   - If parsing fails or pool is unknown, skip that tx's effect.

> **Caveat — Concurrent tx ordering**: Pending txs returned by the RPC are applied sequentially in arbitrary order. Multiple txs touching the same pool may produce a different state than the actual block. The next settled block replay (Phase A) corrects this drift authoritatively.

> **Caveat — Competition**: Real bots submit txs concurrently, affecting execution price. Since this is pure simulation, detected opportunities may not reflect the price impact of competing trades. This is inherent to simulation-based analysis and is acceptable.

---

## 3. CLI Changes

### File: `core/src/cli.rs`
- Add `Command::Live(LiveArgs)` to enum
- Define `LiveArgs` struct

### LiveArgs fields:
| Field | Flag | Default | Description |
|-------|------|---------|-------------|
| chain_args | (flatten) | — | Standard chain/RPC args |
| initial_balance | `--initial-balance` | 10.0 ETH | Starting virtual balance (native token) |
| min_profit | `--min-profit` | 0.001 ETH | Minimum profit to execute |
| poll_interval | `--poll-interval` | 1000 ms | Mempool poll interval |
| max_executions | `--max-executions` | None | Max trades before auto-stop |
| strategies | `--strategies` | two_hop_arb,multi_hop_arb | Detection strategies (comma-separated, reuses existing `--strategies` format) |
| gas_limit | `--gas-limit` | 200,000 | Gas limit per trade |
| priority_fee | `--priority-fee` | 1.0 gwei | Priority fee |
| gas_model | `--gas-model` | live | Gas price model (`live` fetches from chain, `fixed` uses --priority-fee only) |
| resync_interval | `--resync-interval` | 60 | Number of poll cycles between full pool state resyncs |
| price_oracle | `--price-oracle` | coingecko | USD price source (optional — only used for dashboard, not execution) |
| token_prices | `--token-price` | None | Manual token prices (optional, only used for dashboard) |
| export_path | `--export-path` | ./results | Output directory |
| replay_file | `--replay-file` | None | Path to a recorded pending-tx JSON file for offline replay/iteration (disables live RPC polling) |

### File: `core/src/config.rs`
- Add to `Config`: `initial_balance`, `min_profit_threshold`, `poll_interval_ms`, `max_executions`
- Add to `CliOverrides`: same fields
- Update `merge_cli()` for the new fields

### File: `cli/src/main.rs`
- Add `build_overrides` arm for `Command::Live`
- Add dispatch arm:
   1. Validate config
   2. Connect RPC, init pool manager
   3. Create LiveRunner, call `run()`
   4. On exit (Ctrl+C, SIGTERM, or max_executions/bankruptcy reached), print final P&L summary
   5. Save execution history JSON to export_path
   6. Register signal handlers for SIGINT and SIGTERM in the dispatch arm so that shutdown is graceful even when running in Docker/containerized environments

---

## 4. Mempool Detection Enhancements

### File: `core/src/mev/mempool.rs` (moderate changes)
- Keep existing `capture_pending_block()` and `detect_pending_opportunities()` as-is
- Add two new helpers for estimating pending tx pool impact:

### New in `core/src/mev/mempool.rs`:
```rust
/// Run a pending tx through revm against an RPC-forked state and extract
/// pool state changes (reserve updates, price changes) from the resulting
/// EVM state diff. Returns pool effects for all known pools touched.
pub fn simulate_pending_tx_pool_impact(
    tx: &TxData,
    pool_manager: &PoolManager,
    rpc: &RpcClient,
    chain_id: u64,
) -> Vec<PendingPoolEffect>

/// Estimate the effect of a single pending tx on pool reserves
/// by parsing known DEX router calldata patterns (fallback when revm
/// simulation is too expensive).
pub fn estimate_pending_tx_pool_impact(
    tx: &TxData,
    pool_manager: &PoolManager,
) -> Vec<PendingPoolEffect>
```

---

## 5. Output & Monitoring

### Live Dashboard (printed every poll cycle)
```
=== MEV Scout - Live Mode ===
Chain: polygon | Block: 59284710 | Pending: 47 txs
Settled blks:  37 (12 new) | Mempool scans: 412
----------------------------------------
Native Balance: 9.8472 MATIC
Tokens Held:    3 (USDC, WETH, WBTC)
Executions:     12 (11 ok, 1 fail)
  Settled:      8  | two_hop:3 multi:2 sandwich:1 jit:1 liq:1
  Mempool:      4  | two_hop:3 multi:1
Total Profit:   0.1528 MATIC
Total Gas:      0.0040 MATIC
Net P&L:       +0.1488 MATIC (+1.49%)
```

> **Note**: When `--verbose` is active, `tracing::info!` output will interleave with the ANSI dashboard, corrupting the display. In live mode, verbose logging is redirected to a file (`live_{run_id}.log`) automatically, or the dashboard is suppressed. Use `--quiet` for clean dashboard-only output.

### P&L Summary (on exit)
```
=== MEV Scout - Live Mode (shutdown) ===
Runtime:          2h 34m 12s
Settled blocks:   37 (12 with opps)
Mempool scans:    412
Total txs seen:   1,842
----------------------------------------
Initial Native Balance:  10.0000 MATIC
Final Native Balance:    10.1000 MATIC
Token Holdings:          0.0400 USDC, 0.0010 WETH
Net P&L (native only):  +0.1000 MATIC (+1.00%)
Net P&L (incl. tokens): +0.1488 MATIC (+1.49%) *
Total Executions:       12 (11 ok, 1 fail)
  Settled:  8  (two_hop:3, multi:2, sandwich:1, jit:1, liq:1)
  Mempool:  4  (two_hop:3, multi:1)
Best Trade:             +0.0420 MATIC (two_hop_arb @ 59284705)
Avg Profit/Trade:       +0.0124 MATIC
Total Gas Spent:        0.0040 MATIC
  * Token holdings valued at last known prices
```

### Execution Log (saved to JSON)
Periodically append `ExecutionRecord`s to a JSON file. Each record includes `token_in`, `token_out`, `input_amount`, and `output_amount` so the full P&L can be reconstructed per-token.

---

## 6. Files to Create/Modify

> Note: as `core/src/live.rs` grows, it can be refactored into a `live/` directory module with submodules (`live/config.rs`, `live/runner.rs`, `live/state.rs`, etc.).

| File | Action |
|------|--------|
| `core/src/live.rs` | **Create** — LiveRunner, LiveConfig, ExecutionRecord, LiveRunnerState |
| `core/src/lib.rs` | Add `pub mod live;` |
| `core/src/cli.rs` | Add `Command::Live(LiveArgs)`, define `LiveArgs` |
| `core/src/config.rs` | Add live-mode config fields + overrides + merge |
| `core/src/mev/mempool.rs` | Add `simulate_pending_tx_pool_impact()` and `estimate_pending_tx_pool_impact()` helpers |
| `core/src/types.rs` | Add `GasModel::Live` variant |
| `cli/src/main.rs` | Add dispatch for `Command::Live`, dashboard + summary output |

### Key dependency: `Fetcher` and `BlockReplayer` for settled blocks

Live mode reuses the existing `Fetcher` (RPC block fetching) and `BlockReplayer` (revm replay with filtered execution). Unlike backtest mode where blocks are pre-cached, live mode uses a **read-through cache** pattern: if a block isn't cached, `Fetcher` fetches it on demand via RPC. The `BlockReplayer`/`BacktestRunner::run_block()` pipeline then replays and detects against it synchronously, just like in backtest mode. This avoids duplicating the replay + detection logic.

---

## 7. Out of Scope (Future Iterations)

- Real transaction submission (private key mgmt, signing, nonce)
- Flash loan integration in live mode (mempool phase)
- JIT / Sandwich / Liquidation detection on **mempool** (needs log data from settled blocks — these detectors DO run on settled blocks in Phase A)
- Dynamic pool discovery while running (coverage degrades over uptime as new pools deploy)
- Telegram / Discord alerts
- Persistent database for execution history across restarts
- Multi-instance / multi-chain concurrent operation
