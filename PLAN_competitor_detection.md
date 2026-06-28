# Competitor Detection & Analysis — Implementation Plan

## Overview

**Goal**: Identify competitor MEV searchers during backtesting and live operation — who they are, what they spent to win, how quickly they acted, and what strategies they used — then use this data to calibrate the existing PGA model and inform live strategy decisions.

**Current state**: The project has a probabilistic PGA simulation (`pga.rs`) with hardcoded `mean_competitors=3.0` and `intensity=0.5`. No real competitor data is extracted from on-chain activity.

---

## Architecture

```
core/src/mev/competition/
├── mod.rs               # Public API — CompetitionAnalyzer struct
├── extraction.rs        # CompetitorExtraction, ExtractionType, extraction identification
├── profiler.rs          # CompetitorProfile, profile aggregation, persistence
├── calibrator.rs        # PGA parameter calibration from observed data
└── report.rs            # CompetitionReport, serialization, CLI display
```

---

## Phase 1 — Retrospective Competitor Extraction (MVP)

### Core Types (`extraction.rs`)

```rust
/// Identified MEV extraction type from on-chain tx analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExtractionType {
    TwoHopArb,
    MultiHopArb,
    Jit,
    JitArb,
    Sandwich,
    Liquidation,
    /// Has DEX swaps but doesn't match known patterns
    UnknownMev,
}

/// A single on-chain MEV extraction event attributed to a searcher
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompetitorExtraction {
    pub searcher: Address,
    pub extraction_type: ExtractionType,
    pub block_number: u64,
    pub tx_index: usize,
    pub gas_used: u64,
    pub gas_effective_wei: u128,
    pub priority_fee_wei: u128,        // effective - base_fee
    pub gas_cost_wei: u128,
    pub gross_profit_wei: u128,        // from swap amounts
    pub net_profit_wei: i128,          // gross - gas
    pub pools_involved: Vec<Address>,
    pub tokens_involved: Vec<Address>,
    pub builder: Address,               // block_data.coinbase
    pub matched_opportunity_id: Option<String>,  // links to MevOpportunity.canonical_id
    pub confidence: f64,                // 0.0-1.0 classification confidence
}

/// Per-block competition snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockCompetition {
    pub block_number: u64,
    pub total_tx_count: usize,
    pub extractions: Vec<CompetitorExtraction>,
    pub unique_searchers: usize,
}
```

### Extraction Identification Algorithm

**Input**: For each tx in the block — `sender`, `gas_used`, `gas_effective`, event logs, pool state.

**Step 1 — Parse logs**: For each event log, check topic[0] against known signatures:

| Event | Topic (first 8 bytes) | From |
|-------|----------------------|------|
| V2 Swap | `d78ad95f...` | `apply.rs:9` |
| V2 Sync | `1c411e9a...` | `apply.rs:11` |
| V3 Swap | `c42079f9...` | `decoders.rs:9` |
| V3 Mint | `keccak256("Mint(...)")` | `decoders.rs:12-13` |
| V3 Burn | `0c396cd9...` | `decoders.rs:16` |
| Curve Exchange | `keccak256("TokenExchange(...)")` | `decoders.rs:18-19` |
| Balancer Swap | `keccak256("Swap(bytes32,...)")` | `decoders.rs:24-25` |
| Aave LiquidationCall | `keccak256("LiquidationCall(...)")` | `liquidation.rs:10-11` |

All decoders already exist in `core/src/pool/decoders.rs` and `liquidation.rs` — no new decoding needed.

**Step 2 — Per-sender tx classification**:

| Pattern | Detection Rule |
|---------|---------------|
| **Arbitrage** | Tx has `Swap`/`TokenExchange` events on ≥2 different pool addresses; no `Mint`/`Burn` from `tx.from` |
| **TwoHopArb** | Exactly 2 swap events on different pools |
| **MultiHopArb** | 3+ swap events on different pools |
| **JIT** | `Mint(from)` → (swap from *different* address) → `Burn(from)` in same tx or adjacent txs |
| **JitArb** | JIT pattern + same sender also performs a swap crossing pools |
| **Sandwich** | Cross-reference with `SandwichDetector` results: `tx.from` matches `frontrun_tx` or `backrun_tx` sender |
| **Liquidation** | Event log contains `LiquidationCall` topic |
| **UnknownMev** | Has pool swap events but no pattern match |

**Step 3 — Profit estimation**:

For **arbitrage** (most common): decode swap amounts from event logs and compute:

```
For each swap event:
  - V2 Swap: (amount0In, amount0Out, amount1In, amount1Out)
    → tokens sent TO pool = (amount0In, amount1In)
    → tokens received FROM pool = (amount0Out, amount1Out)
  
  - V3 Swap: (amount0, amount1) — signed
    → positive = received by pool (searcher paid)
    → negative = sent from pool (searcher received)

Net token flows for the searcher:
  For each token T:
    flow[T] = sum(received[T]) - sum(paid[T])
  
Gross profit = max(flow[token_out_native], flow_converted_to_native)
```

For other types, use existing detector profit estimates cross-referenced with the tx.

**Step 4 — Cross-reference with detected opportunities**:

After all extractions are identified, match them against the `MevOpportunity` list:
- Match on `(block, pool_a, pool_b, token_in, token_out)` for arb types
- Match on `(block, victim_tx_index)` for sandwich types
- Store `matched_opportunity_id = opportunity.canonical_id` on the extraction

### Integration: Modified `run_block()` in `runner.rs`

Add a `CompetitionAnalyzer` field to `BacktestRunner`:

```rust
pub struct BacktestRunner {
    // ... existing fields ...
    pub competition_analyzer: Option<CompetitionAnalyzer>,
}
```

Inside the `replay_each_filtered` `on_tx` closure (runner.rs:326), add:

```rust
// Collect extraction data (after pool state update)
if let Some(ref mut analyzer) = self.competition_analyzer {
    analyzer.process_tx(
        i,
        sender,
        tx.gas_used,
        tx.gas_effective,
        &tx.logs,
        &pm,
    );
}
```

After the replay loop completes, after the existing cross-block snapshot (runner.rs:451-457):

```rust
// Competition analysis
if let Some(ref mut analyzer) = self.competition_analyzer {
    analyzer.finalize_block(
        block_num,
        &block_data,
        &all_opportunities,
    );
}
```

Return the block's `BlockCompetition` alongside the opportunities:

```rust
// Extend return type or add a new output
Ok((all_opportunities, block_competition, stats, gas_prices))
```

### New Output: Competition Report

Added to `ResultsFile` in `opportunity.rs`:

```rust
pub struct ResultsFile {
    // ... existing fields ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub competition: Option<CompetitionReport>,
}
```

`CompetitionReport` contains:

```rust
pub struct CompetitionReport {
    pub total_searchers_found: usize,
    pub total_extractions: usize,
    pub by_strategy: HashMap<Strategy, usize>,
    pub per_block: Vec<BlockCompetition>,
    pub top_searchers: Vec<CompetitorProfile>,
    pub pga_calibration: PgaCalibration,
}
```

### CLI Output

New sections in `display.rs`:

```
┌──────────────────────────────────────────────────────────────┐
│  Competitor Activity                                         │
├──────────────┬───────┬──────────┬────────┬────────┬──────────┤
│ Block        │ Txs  │ Searchers│ Arbs   │ S.Wich │ Liq      │
├──────────────┼───────┼──────────┼────────┼────────┼──────────┤
│ 58432100     │ 142   │ 8        │ 12     │ 3      │ 1        │
│ 58432101     │ 158   │ 11       │ 15     │ 2      │ 0        │
└──────────────┴───────┴──────────┴────────┴────────┴──────────┘

┌──────────────────────────────────────────────────────────────┐
│  Top Searchers                                               │
├──────────────┬──────────┬──────────┬──────────┬──────────────┤
│ Searcher     │ Total    │ Gas Paid │ Gross $  │ Strategies   │
├──────────────┼──────────┼──────────┼──────────┼──────────────┤
│ 0xabcd...    │ 142      │ 4.2 ETH  │ 12.5 ETH │ arb, liq     │
│ 0xef01...    │ 98       │ 2.8 ETH  │ 8.1 ETH  │ arb, sandwich│
└──────────────┴──────────┴──────────┴──────────┴──────────────┘
```

---

## Phase 2 — PGA Model Calibration

### Types (`calibrator.rs`)

```rust
/// PGA parameters derived from observed competition data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PgaCalibration {
    /// Per-strategy, per-chain mean competitors
    pub mean_competitors: HashMap<Strategy, f64>,
    /// Per-strategy, per-chain bid-to-value ratio (intensity proxy)
    pub bid_to_value_ratio: HashMap<Strategy, f64>,
    /// Total blocks analyzed
    pub blocks_analyzed: u64,
}
```

### Algorithm

After a backtest run with competitor extraction enabled:

```
for each block:
    competitors_in_block = unique searchers with extractions in this block
    
    for each strategy S:
        mean_competitors[S] = avg(competitors_in_block for blocks where S was active)
        
    for each extraction E:
        bid_ratio = E.priority_fee_wei / (E.gross_profit_wei / E.gas_used)
        bid_to_value_ratio[S] = median(bid_ratio for all extractions of strategy S)
```

The calibrated `mean_competitors` and `bid_to_value_ratio` feed directly into `PgaConfig`:

```rust
let pga_config = PgaConfig {
    mean_competitors: calibration.mean_competitors.get(&Strategy::TwoHopArb).copied().unwrap_or(3.0),
    intensity: calibration.bid_to_value_ratio.get(&Strategy::TwoHopArb).copied().unwrap_or(0.5),
};
```

### CLI Flag

```
--calibrate-pga         # After backtest, print calibrated PGA params
```

Also adds `--pga-calibration-file <path>` to save/load calibration between runs.

---

## Phase 3 — Live Competitor Tracking

### Integration in `live.rs`

**3a — Settled block extraction**:

After `self.backtest_runner.run_block(block_num)` returns (live.rs:261), add:

```rust
match self.backtest_runner.run_block(block_num) {
    Ok((opportunities, competition, stats, _)) => {
        // Existing processing...
        self.process_settled_opportunities(opportunities);
        
        // New: track competitor activity from settled block
        if let Some(comp) = competition {
            self.competition_state.record_block(comp);
        }
    }
}
```

**3b — Competitor state**:

```rust
pub struct LiveCompetitionState {
    /// Known searcher profiles (persisted across runs)
    pub known_searchers: HashMap<Address, CompetitorProfile>,
    /// Recent competitor activity (sliding window of blocks)
    pub recent_activity: VecDeque<BlockCompetition>,
    /// Active searchers in current mempool
    pub active_in_mempool: HashSet<Address>,
}
```

Loaded from previous backtest results:

```rust
pub fn load_from_backtest(path: &str) -> Self {
    // Deserialize CompetitionReport, extract profiles
}
```

**3c — Mempool competitor detection**:

In `run_mempool_detection()` (live.rs:409), after computing opportunities:

```rust
// Check if known competitors are in the pending txs
for tx in &pending.txs {
    if self.competition_state.known_searchers.contains_key(&tx.from) {
        self.competition_state.active_in_mempool.insert(tx.from);
    }
}

// Log alert if competitors are active on the same opportunity
if !self.competition_state.active_in_mempool.is_empty() {
    tracing::info!(
        "Known competitors active in mempool: {}",
        self.competition_state.active_in_mempool.len()
    );
}
```

**3d — Dashboard extension**:

In `print_dashboard()` (live.rs), add a row:

```
│ Active competitors: 3 │ Known: 47 │ Today: 142 extractions │
```

### Persistence

`CompetitorProfile` database stored in SQLite cache:

| Table | Columns |
|-------|---------|
| `competitor_profiles` | `address`, `first_seen`, `last_seen`, `total_extractions`, `total_gas_wei`, `total_profit_wei`, `json_blob` |
| `extractions` | `id`, `block`, `tx_index`, `searcher`, `type`, `gas_wei`, `profit_wei`, `opportunity_id` |

---

## File Changes Summary

### New Files

| File | Lines | Purpose |
|------|-------|---------|
| `core/src/mev/competition/mod.rs` | ~30 | `CompetitionAnalyzer` pub struct + builder |
| `core/src/mev/competition/extraction.rs` | ~350 | Extraction types, classification logic, profit estimation |
| `core/src/mev/competition/profiler.rs` | ~200 | `CompetitorProfile`, aggregation, DB persistence |
| `core/src/mev/competition/calibrator.rs` | ~150 | PGA parameter derivation from observed data |
| `core/src/mev/competition/report.rs` | ~100 | `CompetitionReport`, serde, display helpers |

### Modified Files

| File | Changes |
|------|---------|
| `core/src/mev/mod.rs` | Re-export `pub mod competition` |
| `core/src/pipeline/runner.rs` | Add `CompetitionAnalyzer` field to `BacktestRunner`, call in `run_block()` |
| `core/src/pipeline/aggregate.rs` | Add competitor section to aggregation output |
| `core/src/types/opportunity.rs` | Add `competition` field to `ResultsFile` |
| `core/src/mev/execution/live.rs` | Add `LiveCompetitionState`, integrate with settled + mempool pipeline |
| `cli/src/commands/run.rs` | Wire `--competition` flag, optional calibration |
| `cli/src/commands/live.rs` | Wire competition state loading |
| `cli/src/display.rs` | Competition table rendering |
| `cli/src/cli.rs` | Add `--competition`, `--calibrate-pga`, `--competition-db` flags |
| `core/src/cache/store.rs` | Add `put_competitor_profile`, `get_competitor_profiles` |

---

## Detection Accuracy & Limitations

### False Positive Mitigation

| Scenario | Mitigation |
|----------|------------|
| Normal user swap (not MEV) | Net profit will be negative or zero — filter `net_profit_wei <= 0` |
| Flashloan + repay in same tx | Strongly correlated with MEV — classify as `UnknownMev` minimum, arb if swap count ≥2 |
| Aggregator contract (1inch, ParaSwap) | Tx goes to aggregator, not directly to pools. Fallback: check `to` against known aggregators, or mark as `UnknownMev` |
| CEX-DEX arb (can't see CEX side) | Appears as a normal arb across pools — correctly classified but profit may be underestimated if the fill was on CEX |

### Ambiguous Cases

- **Multiple searchers in one block**: If searcher A identifies opportunity but searcher B wins it via higher gas, we see both in extractions. Both may match the same detected opportunity — only the winning tx (higher gas, later index? actually first in block) actually profited.
- **Bundle inclusion**: Multiple txs from same searcher in sequence → merge into one extraction event
- **Searcher using proxy contract**: `tx.from` is the proxy, not the EOA. Track both `tx.from` and `tx.to` for proxy detection.

---

## Implementation Order

```
Phase 1 ─── Day 1:  Types + extraction classification (extraction.rs, mod.rs)
            Day 2:  Integration into BacktestRunner (runner.rs changes)
            Day 3:  Profiler + report output (profiler.rs, report.rs, display.rs)
            Day 4:  CLI flags, testing on historical data
                    ─── MVP complete: "who spent what to win" ───

Phase 2 ─── Day 5:  Calibrator implementation (calibrator.rs)
            Day 6:  Integration with PgaConfig, testing
                    ─── PGA now data-driven ───

Phase 3 ─── Day 7:  LiveCompetitionState, settled block tracking
            Day 8:  Mempool competitor matching, dashboard
            Day 9:  Persistence layer, cross-session profiling
            Day 10: End-to-end testing, edge cases
                    ─── Full live competitor intelligence ───
```
