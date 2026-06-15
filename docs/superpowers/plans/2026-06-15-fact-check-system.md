# Fact-Check System — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Add fact-checking capabilities to `run` and `replay` commands so users can verify the correctness of backtest results.

**Architecture:** Pure additive changes — no refactoring. New `fact_check` module in core. Enhanced CLI output rendering. Existing data structures are unchanged; new types are added in `fact_check.rs`. The `fact-check` subcommand reads saved JSON results and re-verifies opportunities.

**Tech Stack:** Rust, existing `comfy_table`, serde, existing PoolManager API

---

### Overview of changes

| Area | What changes |
|------|-------------|
| `core/src/lib.rs` | Add `pub mod fact_check;` |
| `core/src/fact_check.rs` | **New file** — `BlockSummary`, `OpportunityFactCheck` structs + computation logic |
| `core/src/run.rs` | Return richer per-block data from `run_block` / `run_range` |
| `cli/src/main.rs` | Enhanced `run` output; new `Command::FactCheck` variant |
| `core/src/cli.rs` | Add `FactCheck` subcommand + `FactCheckArgs` |

---

### Task 1: Create `fact_check` module scaffold

**Files:**
- Modify: `core/src/lib.rs`
- Create: `core/src/fact_check.rs`

- [ ] **Step 1: Add module declaration to lib.rs**

Insert after `pub mod fetch;`:
```rust
pub mod fact_check;
```

- [ ] **Step 2: Create `fact_check.rs` with data types**

```rust
use crate::mev::opportunity::MevOpportunity;
use alloy::primitives::Address;
use serde::{Deserialize, Serialize};

/// Per-block summary from a backtest run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockSummary {
    pub block_number: u64,
    pub total_tx: usize,
    pub dex_tx: usize,           // txs that passed the filter (touched a tracked pool/token)
    pub opportunities: usize,    // total opps found in this block
    pub by_strategy: std::collections::HashMap<String, usize>,
}

/// Fact-check result for a single opportunity — re-verifies profit and shows pool state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpportunityFactCheck {
    pub block_number: u64,
    pub tx_index: usize,
    pub strategy: String,
    pub pool_a: Address,
    pub pool_b: Address,
    pub pool_a_name: Option<String>,
    pub pool_b_name: Option<String>,
    pub token_in: Address,
    pub token_out: Address,
    pub input_amount: String,       // formatted for readability
    pub expected_profit: String,    // formatted
    pub gas_cost_wei: u128,
    pub profit_gt_gas: bool,        // simple sanity: profit > gas cost
    /// Re-computed profit using the strategy's formula (when possible).
    /// None if re-verification is not applicable.
    pub recomputed_profit: Option<String>,
    pub recomputation_match: Option<bool>,
    /// Sandwich specific
    pub victim_tx_index: Option<usize>,
    pub backrun_tx_index: Option<usize>,
    /// JIT specific
    pub tick_lower: Option<i32>,
    pub tick_upper: Option<i32>,
    pub liquidity_amount: Option<u128>,
    /// Multi-hop specific
    pub path: Option<Vec<Address>>,
}

/// Full fact-check report for a run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactCheckReport {
    pub run_id: String,
    pub chain: String,
    pub block_count: usize,
    pub total_opportunities: usize,
    pub passed: usize,        // opportunities where all checks pass
    pub failed: usize,        // opportunities where at least one check fails
    pub block_summaries: Vec<BlockSummary>,
    pub opportunity_checks: Vec<OpportunityFactCheck>,
}

/// Compute per-block summaries from a list of opportunities and per-block tx/dex counts.
pub fn compute_block_summaries(
    opportunities: &[MevOpportunity],
    per_block_tx_counts: &[(u64, usize, usize)], // (block_num, total_tx, dex_tx)
) -> Vec<BlockSummary> {
    // Group opportunities by block
    // Map strategy names to counts
    // Return BlockSummary per block
    todo!()
}

/// Build opportunity fact checks from saved results.
/// For now this performs format/sanity checks.
/// Future: re-load block data from cache and re-compute profits.
pub fn verify_opportunities(
    opportunities: &[MevOpportunity],
) -> Vec<OpportunityFactCheck> {
    // For each opportunity:
    //   1. Format check: profit > gas_cost?
    //   2. Missing field check: does sandwich have victim_tx_index?
    //   3. Return OpportunityFactCheck
    todo!()
}
```

- [ ] **Step 3: Write unit tests for the module**

Tests should cover:
- Empty input
- Single opportunity of each strategy type
- Verify that `profit_gt_gas` flag is correct
- Block summary grouping

---

### Task 2: Collect per-block tx stats in `run_range`

**Files:**
- Modify: `core/src/run.rs`
- Modify: `core/src/fact_check.rs` (update `compute_block_summaries`)

The `BacktestRunner::run_range` method currently returns `Vec<MevOpportunity>`. We need it to also return per-block metadata: how many total transactions and how many DEX transactions per block.

- [ ] **Step 1: Add `BlockReplayStats` struct to `run.rs` or `fact_check.rs`**

```rust
/// Stats collected during a single block replay.
pub struct BlockReplayStats {
    pub block_number: u64,
    pub total_tx_count: usize,
    pub dex_tx_count: usize,   // txs that passed the filter
}
```

- [ ] **Step 2: Collect stats inside `run_block`**

Inside `run_block`, the `replay_each_filtered` closure's `filter` and `on_tx` already process each tx. We need to count:
- Total txs in the block (available from `load_block_data`)
- Filter-passed txs (count inside the filter closure)

Introduce a `RefCell<BlockReplayStats>` that gets updated during replay.

- [ ] **Step 3: Return `(Vec<MevOpportunity>, BlockReplayStats)` from `run_block`**

Change signature from:
```rust
pub fn run_block(&mut self, block_num: u64) -> anyhow::Result<Vec<MevOpportunity>>
```
to:
```rust
pub fn run_block(&mut self, block_num: u64) -> anyhow::Result<(Vec<MevOpportunity>, BlockReplayStats)>
```

- [ ] **Step 4: Update `run_range` to collect all stats**

Change signature:
```rust
pub fn run_range(&mut self, resolved: &ResolvedRange) -> anyhow::Result<(Vec<MevOpportunity>, Vec<BlockReplayStats>)>
```

---

### Task 3: Enhanced `run` output

**Files:**
- Modify: `cli/src/main.rs`

- [ ] **Step 1: Print block summary table after opportunities**

After the opportunities table, if blocks > 1, print a table:

```
Block Summary
  Block      Txs  DEX txs  Opportunities  two_hop  multi_hop  jit  sandwich  jit_arb
  ───────   ────  ───────  ─────────────  ───────  ─────────  ───  ────────  ───────
  51234567   142       38              3        1          1    0         1        0
  51234568    98       22              0        0          0    0         0        0
  ───────   ────  ───────  ─────────────  ───────  ─────────  ───  ────────  ───────
  Total     240       60              3        1          1    0         1        0
```

- [ ] **Step 2: Add DEX names to opportunity table**

Enrich `render_results_table` with pool names or DEX labels from PoolManager metadata (e.g., "QuickSwap V3", "SushiSwap V2").

If pool name metadata is available via `PoolInfo::name`, show it in a "Pool A / Pool B" column.

- [ ] **Step 3: Add `--fact-check` flag to `RunArgs`**

```rust
/// Print detailed fact-check report after the run
#[arg(long, help_heading = "Output")]
pub fact_check: bool,
```

When `--fact-check` is set, after saving results, call `fact_check::verify_opportunities()` and print the detailed report.

---

### Task 4: New `fact-check` subcommand

**Files:**
- Modify: `core/src/cli.rs`
- Modify: `cli/src/main.rs`

- [ ] **Step 1: Add `FactCheck` variant to `Command` enum**

In `core/src/cli.rs`:
```rust
/// Verify a previous run's results
FactCheck(FactCheckArgs),
```

- [ ] **Step 2: Add `FactCheckArgs` struct**

```rust
#[derive(Args, Debug, Clone)]
pub struct FactCheckArgs {
    /// Run ID to fact-check (e.g. "run_1712345678")
    #[arg(required = true, value_name = "RUN_ID")]
    pub run_id: String,
}
```

- [ ] **Step 3: Implement handler in `main.rs`**

```rust
Command::FactCheck(args) => {
    // 1. Load results JSON from export_path / {run_id}.json
    // 2. Run fact_check::verify_opportunities() on loaded opportunities
    // 3. Print the report to terminal
    // 4. Optionally save report as {run_id}_factcheck.json
}
```

The handler:
- Reads `ResultsFile` from `{export_path}/{run_id}.json`
- Calls `verify_opportunities(&results_file.opportunities)`
- Prints `FactCheckReport` using a formatted table
- Saves the report as `{export_path}/{run_id}_factcheck.json`

- [ ] **Step 4: Add block-override flag to `FactCheckArgs`**

```rust
/// Re-load block data from cache and re-verify pool state (requires cached blocks)
#[arg(long)]
pub re_verify: bool,
```

When `--re-verify` is set:
- Re-connect to cache and RPC like `replay` does
- For each opportunity, load the block, re-run the relevant tx, and compare the opportunity's expected profit with the actual pool state changes

---

### Task 5: Enhanced `replay` output with DEX interaction analysis

**Files:**
- Modify: `cli/src/main.rs`

- [ ] **Step 1: Add `--analyze` flag to `ReplayArgs`**

```rust
/// Show DEX interaction analysis per transaction
#[arg(long)]
pub analyze: bool,
```

- [ ] **Step 2: Implement DEX interaction detection**

When `--analyze` is set, after `replay_to` returns `results`:
1. For each `ExecutedTx`, inspect the logs
2. Match log addresses against known pool addresses (from PoolManager or from the cache's pool registry)
3. For each matched pool, decode the event topic to determine the interaction type (Swap, Mint, Burn, Sync)
4. Print a DEX interaction summary for each tx

Example output:
```
Replaying block 51234567 on polygon (142 txs, replaying 0..141)

  idx  tx_hash                                                           status  gas_used  receipt
  ────  ────────────────────────────────────────────────────────────────  ──────  ────────  ────────
  0    0xabcd...                                                          ok      142000    ✓
        DEX interactions:
          ├ 0x... QuickSwap V3 USDC/WETH  — Swap [USDC→WETH]
          └ 0x... SushiSwap V2 WMATIC/USDC — Sync
  1    0xef01...                                                          ok      21000     ✓
        (no DEX interactions)
  ...

  Receipt verification: 2/2 match (100.0%) — 0.15s
```

---

### Task 6: Tests

- [ ] **Step 1: Unit tests for `fact_check.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_block_summaries_empty() {
        let result = compute_block_summaries(&[], &[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_compute_block_summaries_single_block() {
        // Create opportunities in block 1
        // Provide tx counts: (1, 100, 25)
        // Verify summary has correct counts
    }

    #[test]
    fn test_verify_opportunities_sandwich() {
        // Create sandwich opportunity with victim_tx_index
        // Verify check passes
    }

    #[test]
    fn test_verify_opportunities_missing_sandwich_fields() {
        // Create sandwich without victim_tx_index
        // Verify check reports issue
    }

    #[test]
    fn test_verify_opportunities_profit_vs_gas() {
        // Create profitable and unprofitable opps
        // Verify profit_gt_gas flag
    }
}
```

- [ ] **Step 2: Update existing tests in `run.rs`**

Update tests that call `run_block` or `run_range` to handle the new return type `(Vec<MevOpportunity>, BlockReplayStats)` or `(Vec<MevOpportunity>, Vec<BlockReplayStats>)`.

---

### Files modified (summary)

| File | Change |
|------|--------|
| `core/src/lib.rs` | +1 line: `pub mod fact_check;` |
| `core/src/fact_check.rs` | **New** — ~200 lines: types, computation, tests |
| `core/src/run.rs` | ~30 lines changed: richer return types, collect stats |
| `core/src/cli.rs` | ~15 lines: `FactCheck` variant + `FactCheckArgs` |
| `cli/src/main.rs` | ~80 lines: enhanced output, `FactCheck` handler, `--analyze` |
