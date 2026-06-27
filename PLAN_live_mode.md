# Live Mode — Plan

## Overview

Add a new `live` subcommand that turns MEV Scout into a **virtual mempool bot**. Instead of replaying historical blocks, it connects to the live chain, continuously polls the mempool for pending transactions, applies them to pool state, detects MEV opportunities in real time, and maintains a **virtual wallet** that tracks per-token balances, executions, and P&L.

No real transactions are broadcast — everything is simulated via the existing revm engine, so no private keys or wallet setup is needed.

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
    pub execution_history: Vec<ExecutionRecord>,
}
```

---

## 2. New Module: `core/src/live.rs` — LiveRunner

### Initialization flow
1. Load pool definitions from cache (or discover from current chain state)
2. Fetch latest finalized block number via RPC
3. Initialize pool manager at that block (same as `BacktestRunner::init_pools`)
4. Optionally fetch Aave V3 reserves and CoinGecko prices
5. Return ready LiveRunner instance

### Main loop (`run()`)
`LiveRunner::run()` is an **`async fn`** — the entire loop runs on the tokio runtime. Detection (TwoHopArbDetector, MultiHopArbDetector) is synchronous; only RPC calls are async.

```
loop {
    // 0. RPC safety: wrap capture in retry logic with exponential backoff.
    //    If RPC is down for N retries, log warning and continue sleeping,
    //    don't crash the loop.

    // 1. Capture pending block from mempool via eth_getBlockByNumber("pending")
    let pending = capture_pending_block(&rpc).await;

    // 2. Get latest settled block number
    let latest_block = rpc.get_block_number().await;

    // 2b. Periodic state resync: every time latest_block advances past the
    //     last resync block, re-initialize pool state from the new finalized
    //     block. This corrects drift caused by imperfect pending-tx estimation
    //     and ensures pool state reflects on-chain reality.
    //     Uses BacktestRunner::init_pools(&pool_manager, &rpc, latest_block, ...).

    // 3. Clone pool manager to create a speculative state fork.
    //    Apply pending tx effects to the clone.
    //    Two approaches (try revm first, fall back to calldata parsing):
    //
    //    a) [Preferred] Run pending txs through revm against an RPC-forked
    //       state (eth_call with block: "pending" — or simulate against a
    //       local state fork). Capture state diffs (storage writes to known
    //       pool contracts) and apply them to the cloned PoolManager.
    //       Reuses existing revm infrastructure (no receipts needed).
    //
    //    b) [Fallback] Parse known DEX router calldata patterns when revm
    //       simulation is not available or too expensive:
    //       - V2: swapExactTokensForTokens / swapTokensForExactTokens
    //       - V3: exactInput / exactOutput path decoding
    //       - If parsing fails or pool is unknown, skip that tx.

    // 4. Fetch live gas prices for this cycle:
    //    - Use eth_gasPrice or base_fee from pending block
    //    - Use eth_maxPriorityFeePerGas for priority fee
    //    - Build GasConfig with GasModel::Live (new variant) that uses
    //      these live values instead of historical block data
    //    - flash_loan_provider is ignored in live mode (no flash loans)

    // 5. Run detection on the speculative state:
    //    - two_hop_arb (existing TwoHopArbDetector)
    //    - multi_hop_arb (existing MultiHopArbDetector)
    //    - skip log-based detectors (JIT, Sandwich, etc.)

    // 6. For each detected opportunity (sorted by profit descending):
    //    a. Check if virtual wallet holds sufficient token_in balance
    //       (token_balances[token_in] >= input_amount) AND native
    //       balance covers gas (native_balance_wei >= gas_cost_wei)
    //    b. Simulate the trade via revm (block_on on RPC-backed state)
    //    c. If simulated profit >= min_profit_threshold:
    //       - Deduct input_amount from token_balances[token_in]
    //       - Add output_amount to token_balances[token_out]
    //       - Deduct gas_cost_wei from native_balance_wei
    //       - Record ExecutionRecord with token_in/token_out/input/output
    //       - Apply swap effects to speculative pool state
    //       - Increment execution counters

    // 7. Print dashboard to terminal (ANSI-clear + rewrite).
    //    Note: when --verbose is on, tracing output will interfere with
    //    the ANSI dashboard. In live mode, route tracing to a file or
    //    suppress the dashboard when verbose is set.

    // 8. Sleep for poll_interval_ms
}
```

### Pool state estimation for pending txs

Since pending txs have no execution receipts/logs, we use a **revm-first** approach:

1. **Primary: revm simulation** — Execute each pending tx against an RPC-forked EVM state (`eth_call` at the pending block). After execution, inspect the resulting state for storage writes to known pool contract addresses. Decode those writes into reserve/price updates and apply them to the cloned `PoolManager`. This catches all DEX interactions regardless of router complexity (aggregators, multi-hop, flash loans). The existing `BlockReplayer`'s revm pipeline can be adapted for this.

2. **Fallback: calldata parsing** — When revm simulation is too expensive (many pending txs, rate-limited RPC), fall back to parsing known DEX router calldata patterns:
   - **V2 swaps**: Parse `swapExactTokensForTokens` / `swapTokensForExactTokens` calldata for amountIn/amountOut, find the pool via token pair + factory, apply constant-product formula to estimate reserve changes.
   - **V3 swaps**: Parse `exactInput` / `exactOutput` calldata for path (pool + fee), use sqrt_price from current pool state to estimate price impact.
   - If parsing fails or pool is unknown, skip that tx's effect.

A heuristic determines which approach to use per cycle: if pending tx count > threshold (e.g. 50), use calldata parsing; otherwise use revm simulation.

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
  4. On exit (Ctrl+C or max_executions reached), print final P&L summary
  5. Save execution history JSON to export_path

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
Chain: polygon | Block: 59284710 | Pending txs: 47
----------------------------------------
Native Balance: 9.8472 MATIC
Tokens Held:    3 (USDC, WETH, WBTC)
Executions:     12 (11 ok, 1 fail)
Total Profit:   0.1528 MATIC
Total Gas:      0.0040 MATIC
Net P&L:       +0.1488 MATIC (+1.49%)
```

> **Note**: When `--verbose` is active, `tracing::info!` output will interleave with the ANSI dashboard, corrupting the display. In live mode, verbose logging is redirected to a file (`live_{run_id}.log`) automatically, or the dashboard is suppressed. Use `--quiet` for clean dashboard-only output.

### P&L Summary (on exit)
```
=== MEV Scout - Live Mode (shutdown) ===
Runtime:        2h 34m 12s
Blocks seen:    37
Total txs seen: 1,842
----------------------------------------
Initial Native Balance:  10.0000 MATIC
Final Native Balance:    10.1000 MATIC
Token Holdings:          0.0400 USDC, 0.0010 WETH
Net P&L (native only):  +0.1000 MATIC (+1.00%)
Net P&L (incl. tokens): +0.1488 MATIC (+1.49%) *
Total Executions:       12 (11 ok, 1 fail)
Best Trade:             +0.0420 MATIC (two_hop_arb @ 59284705)
Avg Profit/Trade:       +0.0124 MATIC
Total Gas Spent:        0.0040 MATIC
  * Token holdings valued at last known prices
```

### Execution Log (saved to JSON)
Periodically append `ExecutionRecord`s to a JSON file. Each record includes `token_in`, `token_out`, `input_amount`, and `output_amount` so the full P&L can be reconstructed per-token.

---

## 6. Files to Create/Modify

| File | Action |
|------|--------|
| `core/src/live.rs` | **Create** — LiveRunner, LiveConfig, ExecutionRecord, LiveRunnerState |
| `core/src/lib.rs` | Add `pub mod live;` |
| `core/src/cli.rs` | Add `Command::Live(LiveArgs)`, define `LiveArgs` |
| `core/src/config.rs` | Add live-mode config fields + overrides + merge |
| `core/src/mev/mempool.rs` | Add pending-tx pool impact estimation helper |
| `cli/src/main.rs` | Add dispatch for `Command::Live`, dashboard + summary output |

---

## 7. Out of Scope (Future Iterations)

- Real transaction submission (private key mgmt, signing, nonce)
- Flash loan integration in live mode
- JIT / Sandwich / Liquidation detection on mempool (needs log data from settled blocks)
- Dynamic pool discovery while running (coverage degrades over uptime as new pools deploy)
- Telegram / Discord alerts
- Persistent database for execution history across restarts
- Multi-instance / multi-chain concurrent operation
