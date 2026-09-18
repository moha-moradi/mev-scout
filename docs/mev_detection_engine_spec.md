# MEV Detection & Opportunity Engine

## Engineering Specification for Rust + Alloy + REVM

**Target users:** Coding Agents (OpenCode / Cursor / Claude Code / similar)

**Primary goal:** Build a production-oriented, research-grade MEV detection and backtesting engine that can reproduce the *observable* functionality of MEV explorers/inspectors, while going beyond post-hoc classification into deterministic opportunity detection and REVM-based profitability simulation.

**Initial target chains:** Avalanche C-Chain, Polygon, BSC, and compatible EVM chains. The first implementation can be Polygon-only if scope needs to be reduced.

**Initial strategy scope:**
- Arbitrage
- Liquidations
- Sandwich detection
- Backrun detection
- Front-run detection
- Uniswap V3 JIT liquidity
- JIT + arbitrage

**Explicitly deferred:** liquidation execution, generalized solver/intent MEV, cross-domain MEV, fully realistic searcher competition modeling, and production execution/broadcasting unless separately requested.

---

## 1. Executive Summary

The system should be designed as two related but distinct engines:

1. **Historical MEV Classification Engine**
   - Answers: **"What actually happened on-chain?"**
   - Inputs: blocks, transactions, receipts, logs, traces, token transfers, pool/protocol state.
   - Detects realized arbitrage, liquidations, sandwiches, backruns, front-runs, JIT, and composite strategies.

2. **Opportunity Engine**
   - Answers: **"What could a professional MEV searcher have done from this state?"**
   - Inputs: historical state and optionally pending/mempool transactions.
   - Produces candidates, then validates them with deterministic REVM simulation.
   - Computes realistic gross and net P&L including gas, protocol fees, flash-loan fees, slippage, and optional builder/validator payment assumptions.

These engines share normalized primitives but must not be conflated.

### Core principle

```text
Cheap detection / graph analysis
        -> candidate opportunity
        -> deterministic REVM simulation
        -> exact token flows
        -> optimization
        -> cost accounting
        -> net P&L
```

Do **not** REVM-simulate every transaction blindly. Use a cheap detection stage to reduce the candidate set first.

---

# 2. Important Conceptual Distinction

## 2.1 Historical MEV vs Live Opportunity

A historical explorer may observe:

```text
DEX A -> DEX B -> DEX C -> original token
```

and report a realized arbitrage profit.

An opportunity engine must instead detect a state such as:

```text
Pool A price != Pool B price
```

then determine whether any input size can exploit the discrepancy *after actual fees, price impact, gas, flash-loan fees, and other costs*.

Therefore:

```text
Historical classifier != Opportunity scanner
```

The same distinction applies to liquidations, sandwiches, backruns, front-runs, and JIT.

---

# 3. Reference Architecture

```text
                         ┌────────────────────────┐
                         │      EVM Chain         │
                         │ Avalanche / Polygon    │
                         │ BSC / Base / etc.      │
                         └────────────┬───────────┘
                                      │
                    ┌─────────────────┴─────────────────┐
                    │                                   │
                 Historical                         Live / Pending
                    │                                   │
             Blocks / Tx / Traces                    Mempool
                    │                                   │
                    └─────────────────┬─────────────────┘
                                      ▼
                           ┌───────────────────┐
                           │   Ingestion       │
                           │ Alloy / RPC / WS  │
                           └─────────┬─────────┘
                                     ▼
                           ┌───────────────────┐
                           │ Decoder /         │
                           │ Normalizer        │
                           └─────────┬─────────┘
                                     ▼
             ┌───────────────────────┼────────────────────────┐
             │                       │                        │
             ▼                       ▼                        ▼
         Call Traces             Transfers                 Swaps
             │                       │                        │
             └───────────────────────┼────────────────────────┘
                                     ▼
                           ┌───────────────────┐
                           │ MEV Detectors     │
                           ├───────────────────┤
                           │ Arbitrage         │
                           │ Liquidation       │
                           │ Sandwich          │
                           │ Backrun           │
                           │ Front-run         │
                           │ JIT               │
                           │ JIT + Arb         │
                           └─────────┬─────────┘
                                     ▼
                           ┌───────────────────┐
                           │ Candidate Engine  │
                           └─────────┬─────────┘
                                     ▼
                           ┌───────────────────┐
                           │ REVM Simulator    │
                           └─────────┬─────────┘
                                     ▼
                           ┌───────────────────┐
                           │ Optimizer         │
                           │ amount / route    │
                           └─────────┬─────────┘
                                     ▼
                           ┌───────────────────┐
                           │ Accounting / P&L  │
                           ├───────────────────┤
                           │ Gross profit      │
                           │ Gas               │
                           │ Flash-loan fee    │
                           │ Slippage          │
                           │ Protocol fees     │
                           │ Optional bribe    │
                           │ Net profit        │
                           └─────────┬─────────┘
                                     ▼
                           ┌───────────────────┐
                           │ Local Storage     │
                           │ RocksDB/redb/etc. │
                           └───────────────────┘
```

---

# 4. System Responsibilities

## 4.1 Ingestion Layer

Responsibilities:
- Fetch blocks.
- Fetch transactions.
- Fetch receipts.
- Fetch logs.
- Fetch traces where the node/provider supports them.
- Optionally subscribe to pending transactions for live mode.
- Never let strategy code directly call RPC.
- Normalize RPC differences between chains/providers.

Design rule:

```text
RPC -> ingestion -> persistent normalized data -> everything else
```

Strategies and detectors must not depend directly on RPC APIs.

---

# 5. Required Normalized Data Model

The system should use chain-agnostic normalized primitives.

## 5.1 Block

Suggested fields:

```rust
struct BlockContext {
    chain_id: u64,
    block_number: u64,
    block_hash: B256,
    parent_hash: B256,
    timestamp: u64,
    base_fee: Option<U256>,
    gas_limit: u64,
    gas_used: u64,
}
```

Include any chain-specific fields required by the target network.

## 5.2 Transaction

```rust
struct NormalizedTransaction {
    chain_id: u64,
    hash: B256,
    block_number: Option<u64>,
    transaction_index: Option<u64>,
    from: Address,
    to: Option<Address>,
    nonce: u64,
    value: U256,
    input: Bytes,
    gas_limit: u64,
    gas_price: Option<U256>,
    max_fee_per_gas: Option<U256>,
    max_priority_fee_per_gas: Option<U256>,
}
```

## 5.3 Trace

```rust
struct TraceNode {
    tx_hash: B256,
    trace_address: Vec<u32>,
    depth: u32,
    call_type: CallType,
    from: Address,
    to: Option<Address>,
    value: U256,
    input: Bytes,
    output: Option<Bytes>,
    gas: Option<u64>,
    gas_used: Option<u64>,
    error: Option<String>,
    children: Vec<TraceNodeId>,
}
```

Trace ordering must be deterministic using at minimum:

```text
(transaction_index, trace_address)
```

## 5.4 Token Transfer

```rust
struct TokenTransfer {
    tx_hash: B256,
    trace_address: Vec<u32>,
    token: Address,
    from: Address,
    to: Address,
    amount: U256,
}
```

## 5.5 Normalized Swap

```rust
struct Swap {
    chain_id: u64,
    block_number: u64,
    tx_hash: B256,
    transaction_position: u64,
    trace_address: Vec<u32>,

    trader: Address,
    pool: Address,
    router: Option<Address>,
    dex: DexId,
    pool_type: PoolType,

    token_in: Address,
    token_out: Address,
    amount_in: U256,
    amount_out: U256,

    gas_used: Option<u64>,
}
```

For concentrated-liquidity pools, optionally add:

```text
sqrt_price_before
sqrt_price_after
liquidity_before
liquidity_after
tick_before
tick_after
```

when state reconstruction allows it.

---

# 6. Protocol Registry

Create a protocol registry rather than hard-coding logic throughout detectors.

Each protocol definition should contain:

```text
ProtocolId
ChainId
ContractAddress
ABI / Interface
Function Selectors
Event Signatures
Classifier Type
Pool Discovery Rules
State Reader
```

Examples:
- Uniswap V2
- Uniswap V3
- Uniswap V4 (future)
- SushiSwap / forks
- QuickSwap
- Trader Joe / Liquidity Book as applicable
- Pangolin
- Curve
- Balancer
- Aave
- Compound
- 0x
- 1inch
- ParaSwap
- Odos

The initial implementation should not attempt every protocol simultaneously.

---

# 7. Trace Classification Architecture

A useful reference architecture is the historical Flashbots `mev-inspect` style:

```text
raw trace
   -> identify target contract
   -> choose ABI
   -> decode calldata
   -> map function signature to classification
   -> produce normalized ClassifiedTrace
```

The classifier should produce categories such as:

```text
swap
liquidate
borrow
repay
deposit
withdraw
mint
burn
flash_loan
unknown
```

Do not make detector implementations parse raw calldata separately when a shared protocol decoder can provide a normalized representation.

---

# 8. Arbitrage Detection

## 8.1 Historical Arbitrage

Detect closed token cycles where the net token amount returned to the starting asset exceeds the initial amount.

Example:

```text
USDC -> WETH   DEX A
WETH -> AVAX   DEX B
AVAX -> USDC   DEX C
```

Algorithm:

1. Extract all normalized swaps in the block.
2. Group swaps by transaction.
3. Build a directed token graph.
4. Find cycles within each transaction.
5. Track token quantities through the actual swap route.
6. Require the cycle to return to a token controlled by the original trader/searcher.
7. Produce an `ArbitrageCandidate`.
8. Re-simulate the route with REVM when exact historical state is available.
9. Calculate gross and net P&L.

### Candidate fields

```rust
struct ArbitrageCandidate {
    tx_hash: Option<B256>,
    trader: Address,
    route: Vec<SwapRef>,
    start_token: Address,
    start_amount: U256,
    expected_end_amount: U256,
    detection_confidence: f64,
}
```

## 8.2 Live / Hypothetical Arbitrage

From a single state snapshot:

```text
pool prices
    -> generate candidate routes
    -> optimize input amount
    -> simulate route with REVM
    -> calculate net profit
```

Do not assume that a visible price difference is profitable; account for:
- pool fees
- price impact
- protocol fees
- gas
- flash-loan fee
- transfer taxes where applicable
- execution constraints

---

# 9. Arbitrage Route Search

Use a graph representation:

```text
Node = token
Edge = pool swap
```

Support both:
- direct 2-pool arbitrage
- triangular routes
- longer cyclic paths

Do not search an unbounded graph.

Recommended controls:

```text
max_hops
max_pools_per_token
max_candidates_per_block
liquidity threshold
minimum expected edge
```

Use pruning based on:
- known viable token pairs
- pool liquidity
- estimated price edge
- gas budget
- token whitelist/blacklist

---

# 10. Liquidation Detection

## 10.1 Historical Liquidation

Detect protocol-specific liquidation functions/events.

Pipeline:

```text
transaction
    -> decode liquidation call
    -> identify parent liquidation trace
    -> collect child traces
    -> collect token transfers
    -> parse debt repaid
    -> parse collateral received
    -> classify liquidation
```

The historical Flashbots implementation follows this parent/child trace structure and then delegates to a protocol-specific liquidation classifier.

## 10.2 Liquidation Candidate

```rust
struct LiquidationCandidate {
    protocol: ProtocolId,
    liquidator: Address,
    borrower: Address,
    debt_token: Address,
    collateral_token: Address,
    debt_repaid: U256,
    collateral_received: U256,
    detection_confidence: f64,
}
```

## 10.3 Live Liquidation Opportunity

Monitor positions and protocol state.

Inputs:
- collateral balance
- debt balance
- oracle price
- liquidation threshold
- health factor or equivalent metric
- liquidation bonus
- protocol-specific close factor / max repay

Pipeline:

```text
position
    -> current risk metric
    -> determine liquidatable status
    -> compute max repay
    -> estimate collateral received
    -> simulate liquidation
    -> calculate net P&L
```

Do not mark a position as profitable solely because it is liquidatable.

---

# 11. Sandwich Detection

## 11.1 Historical Sandwich Structure

Typical pattern:

```text
TX A: searcher front leg
TX B: victim swap
TX C: searcher back leg
```

Heuristics:

1. Sort swaps by `(transaction_position, trace_address)`.
2. Consider the first swap as a front candidate.
3. Exclude known router contracts as the searcher identity when necessary.
4. Search later swaps in the same block.
5. Require same pool/contract where appropriate.
6. Require same token direction between front and victim.
7. Require reverse direction for the searcher's back leg.
8. Require victim sender differs from the searcher.
9. Require the searcher identity is consistent.
10. Validate that the victim received a worse execution.
11. Validate that the complete sequence is profitable.

The published Flashbots sandwich detector uses transaction position + trace ordering and searches for a same-direction victim swap followed by a reverse-direction swap by the same searcher on the same pool.

## 11.2 Important limitation

Ordering alone is not proof of malicious or intentional front-running.

Use a confidence score and explicit evidence fields.

```rust
struct SandwichEvidence {
    same_pool: bool,
    same_pair: bool,
    direction_match: bool,
    reverse_backrun: bool,
    same_searcher: bool,
    victim_price_impact: Option<f64>,
    realized_profit: Option<U256>,
}
```

## 11.3 Live Sandwich Opportunity

Inputs:
- pending victim transaction
- current pool state
- candidate front-run transaction
- candidate back-run transaction

Pipeline:

```text
pending victim
    -> decode
    -> identify victim pool(s)
    -> simulate victim alone
    -> create front-run candidate
    -> simulate front + victim
    -> create back-run candidate
    -> simulate front + victim + back
    -> calculate net profit
```

Never treat a mempool transaction as a guaranteed inclusion or ordering.

---

# 12. Backrun Detection

## 12.1 Historical Backrun

The conceptual pattern is:

```text
TX A = state-changing / market-moving transaction
TX B = transaction that monetizes the resulting state
```

A stronger detector should compare the opportunity before and after TX A:

```text
profit(B | state_before_A)
        vs
profit(B | state_after_A)
```

If the profitability materially increases due to A, classify B as a probable backrun candidate.

Do not classify based solely on adjacent transaction indexes.

## 12.2 Live Backrun

```text
pending market-moving tx
        -> simulate TX A
        -> obtain post-A state
        -> scan arbitrage / rebalance routes
        -> simulate candidate B
        -> calculate net profit
```

This is one of the most important uses of deterministic REVM in the live architecture.

---

# 13. Front-run Detection

## 13.1 Historical Front-run

Candidate pattern:

```text
Searcher transaction
    -> changes price/state
Victim transaction
    -> receives materially worse execution
Searcher later benefits
```

Minimum evidence should include:
- ordering
- same/correlated asset/pool
- measurable state change
- victim execution degradation
- economic benefit to the searcher

A transaction merely occurring before another transaction is not enough.

## 13.2 Live Front-run Candidate

```text
pending victim
    -> simulate victim
    -> generate candidate state-changing transaction
    -> simulate candidate + victim
    -> measure victim degradation
    -> calculate searcher profit
```

Any final classification should be based on explicit evidence rather than labels alone.

---

# 14. Uniswap V3 JIT Liquidity

Historical pattern:

```text
Mint liquidity
      ↓
Large swap through the range
      ↓
Burn liquidity
```

Detect when:
- the same LP/provider address performs mint and burn
- the position exists for a very short time
- the relevant swap occurs while liquidity is active
- the mint/burn tick range overlaps the swap's executed range

Pipeline:

```text
Mint
  -> position created
Swap
  -> fees generated / liquidity utilized
Burn
  -> position removed
```

Calculate:

```text
JIT P&L
= collected fees
- mint/burn gas
- other execution costs
```

Add position metadata:

```text
pool
owner
position_id
lower_tick
upper_tick
liquidity
mint_block
burn_block
active_duration
fees_collected
```

---

# 15. JIT + Arbitrage

Composite pattern:

```text
Mint liquidity
      ↓
Victim / large swap
      ↓
Arbitrage opportunity
      ↓
Arbitrage execution
      ↓
Burn liquidity
```

The detector should permit composite classification:

```text
strategy = JIT
components = [JIT, Arbitrage]
```

Compute both:
- LP fee income
- arbitrage profit

then combine all costs.

---

# 16. REVM Simulation Layer

REVM should be used as the deterministic execution validator.

## 16.1 Simulation Responsibilities

- Reconstruct or load exact state.
- Apply transaction sequence.
- Execute candidate calldata.
- Track token balances.
- Track gas.
- Capture revert reasons.
- Capture logs and state changes.
- Return exact post-state.

## 16.2 Simulation Inputs

```rust
struct SimulationRequest {
    chain_id: u64,
    block_context: BlockContext,
    state_snapshot: StateSnapshot,
    transactions: Vec<SimulatedTransaction>,
}
```

## 16.3 Candidate Simulation Examples

### Arbitrage

```text
flashloan (optional)
  -> swap A
  -> swap B
  -> swap C
  -> repay
```

### Sandwich

```text
front-run
  -> victim
  -> back-run
```

### Backrun

```text
market-moving tx
  -> candidate trade
```

### Liquidation

```text
liquidation call
  -> collateral handling
  -> optional swap
  -> repay flashloan
```

---

# 17. Exact P&L Accounting

Every result must expose a full breakdown.

```rust
struct ProfitBreakdown {
    gross_profit: SignedAmount,
    gas_cost: SignedAmount,
    flashloan_fee: SignedAmount,
    protocol_fees: SignedAmount,
    slippage_cost: SignedAmount,
    builder_payment: SignedAmount,
    other_costs: SignedAmount,
    net_profit: SignedAmount,
}
```

Formula:

```text
Net Profit
= Gross Profit
- Gas Cost
- Flash-loan Fee
- Protocol Fees
- Slippage Cost
- Builder / Validator Payment
- Other Costs
```

Use exact token accounting first. Convert to USD only after a reference pricing step.

Never calculate P&L by simply subtracting nominal USD values if token quantities have not been reconciled.

---

# 18. Flash Loan Modeling

Initial backtest should support:
- Balancer flash loans
- Aave flash loans

Optional future support:
- Uniswap V2 flash swaps
- other chain-native lending/flash mechanisms

Flash-loan simulation must model:

```text
borrowed amount
+ fee
= exact repayment requirement
```

A candidate is only profitable if repayment succeeds in the same atomic simulation.

---

# 19. Gas Accounting

For historical transactions:

```text
gas_cost_native = gas_used * effective_gas_price
```

For hypothetical simulations:

```text
estimated_gas_cost_native
    = simulated_gas_used * assumed_effective_gas_price
```

Keep chain-native amount and USD valuation separately.

Do not hard-code one global USD gas price.

---

# 20. State Reconstruction

Historical simulation requires the state corresponding to the relevant execution point.

Preferred hierarchy:

1. Local archive/full node state.
2. Deterministic state database populated by ingestion.
3. Provider historical state calls when necessary.

The goal is that the simulation layer does not depend on live RPC for normal historical backtests.

Recommended architecture:

```text
RPC / node
    -> ingestion
    -> local state DB
    -> REVM database adapter
```

---

# 21. Storage

The initial architecture should keep dependencies lightweight.

Recommended candidates:
- RocksDB
- redb
- in-memory structures for tests / hot cache

Avoid Redis/Postgres unless a concrete scaling requirement justifies them.

Store at minimum:

```text
blocks
transactions
receipts
traces
transfers
swaps
pool metadata
pool state snapshots
protocol events
MEV candidates
simulation results
P&L results
```

Historical raw RPC data should be cacheable/offline so repeated research runs do not re-query the chain.

---

# 22. Candidate vs Opportunity vs Realized MEV

Use explicit types.

```text
Candidate
   = pattern detected, not yet simulated

Opportunity
   = candidate confirmed profitable by simulation

RealizedMEV
   = observed on-chain strategy execution
```

Do not mix them.

Example:

```text
Candidate Arbitrage
        ↓ REVM
Opportunity Arbitrage
        ↓ compare with actual chain
Realized Arbitrage
```

This distinction is essential for backtesting.

---

# 23. Confidence Model

Every heuristic detector should expose a confidence/evidence model rather than pretending classification is always certain.

Example:

```rust
struct DetectionEvidence {
    detector: DetectorId,
    confidence: f64,
    reasons: Vec<ReasonCode>,
    supporting_transactions: Vec<B256>,
    supporting_traces: Vec<TraceRef>,
}
```

Example reason codes:

```text
SAME_POOL
SAME_PAIR
REVERSE_DIRECTION
SAME_SEARCHER
DIRECT_TOKEN_CYCLE
LIQUIDATION_SELECTOR_MATCH
LIQUIDATION_EVENT_MATCH
STATE_DELTA_MATCH
VICTIM_EXECUTION_DEGRADED
PROFIT_VERIFIED
SHORT_LP_LIFETIME
OVERLAPPING_TICK_RANGE
```

Do not use one opaque score as the only explanation.

---

# 24. False Positive Control

False-positive prevention is a first-class requirement.

## Arbitrage

Do not classify a round trip as profitable until exact token balances reconcile.

## Liquidation

Do not classify a generic transfer as liquidation without protocol-specific semantics.

## Sandwich

Do not classify simply because two same-direction swaps surround a victim.

## Backrun

Do not classify a transaction as backrun merely because it follows a whale transaction.

## Front-run

Do not classify merely based on transaction index.

## JIT

Do not classify any short-lived LP position as JIT unless its active range overlaps relevant swap execution and fee generation.

---

# 25. Live vs Historical Capabilities Matrix

| Strategy | Historical Block Data | Live / Mempool Data | REVM Needed | Confidence Type |
|---|---:|---:|---:|---|
| Arbitrage | Yes | Yes | Strongly recommended | High after simulation |
| Liquidation | Yes | Yes | Yes | High with protocol decoder |
| Sandwich | Yes | Yes | Yes for opportunity | Heuristic + simulation |
| Backrun | Yes | Yes | Yes | State-causality heuristic |
| Front-run | Yes | Yes | Yes | Heuristic + victim impact |
| JIT | Yes | Yes | Yes | High with V3 state |
| JIT + Arb | Yes | Yes | Yes | Composite |

---

# 26. Initial MVP Scope

The first production-quality MVP should be deliberately smaller.

## MVP-1

Chain:
- Polygon only

Protocols:
- Uniswap V2-compatible DEXs
- Uniswap V3-compatible DEXs

Strategies:
- Historical arbitrage
- Historical liquidation
- Historical sandwich
- Historical JIT

Infrastructure:
- Rust
- Alloy
- REVM
- RocksDB or redb

Requirements:
- deterministic backtest
- cached blocks
- exact accounting
- CLI output

## MVP-2

Add:
- live mempool ingestion
- live arbitrage
- live backrun
- live sandwich candidate generation
- Balancer flash loan

## MVP-3

Add:
- Avalanche
- BSC
- Base
- additional DEXs
- Aave
- Curve
- Balancer
- aggregators

## MVP-4

Add:
- competition modeling
- builder payment modeling
- bundle simulation
- execution strategy
- latency metrics

---

# 27. Suggested Rust Workspace

```text
mev-backtest/
├── Cargo.toml
├── crates/
│   ├── mev-primitives/
│   ├── mev-ingest/
│   ├── mev-storage/
│   ├── mev-decoder/
│   ├── mev-protocols/
│   ├── mev-pools/
│   ├── mev-detectors/
│   │   ├── arb/
│   │   ├── liquidation/
│   │   ├── sandwich/
│   │   ├── backrun/
│   │   ├── frontrun/
│   │   ├── jit/
│   │   └── composite/
│   ├── mev-sim/
│   │   └── revm/
│   ├── mev-optimize/
│   ├── mev-accounting/
│   ├── mev-pipeline/
│   └── mev-cli/
└── tests/
```

Alternative if keeping the existing requested workspace names:

```text
mev-backtest-core/
mev-backtest-cli/
```

with the internal modules described above.

---

# 28. Detector Interface

A common interface is strongly recommended.

```rust
pub trait MevDetector {
    fn id(&self) -> DetectorId;

    fn detect(
        &self,
        ctx: &DetectionContext,
    ) -> anyhow::Result<Vec<MevCandidate>>;
}
```

The `DetectionContext` should contain references to normalized, already-ingested data and must not contain an RPC client.

For live mode:

```rust
pub trait LiveMevDetector {
    fn on_pending_transaction(
        &self,
        tx: &PendingTransaction,
        ctx: &LiveDetectionContext,
    ) -> anyhow::Result<Vec<MevCandidate>>;
}
```

---

# 29. Simulation Interface

```rust
pub trait OpportunitySimulator {
    fn simulate(
        &self,
        candidate: &MevCandidate,
        state: &StateSnapshot,
    ) -> anyhow::Result<SimulationResult>;
}
```

`SimulationResult` should include:

```text
success
revert_reason
gas_used
native_gas_cost
token_deltas
trace_summary
post_state_hash
profit_breakdown
```

---

# 30. Optimization Layer

The simulator should not assume that the first profitable amount is optimal.

For each candidate route:

```text
candidate amount range
    -> coarse search
    -> local refinement
    -> exact REVM validation
```

Example:

```text
$1k
$2k
$5k
$10k
$20k
$50k
$100k
...
```

then refine around the best region.

For concentrated liquidity pools, include tick-boundary effects.

---

# 31. USD Pricing

Separate execution accounting from valuation.

Execution accounting:

```text
Token A amount
Token B amount
Native gas amount
```

Valuation:

```text
Token amount -> USD
```

Use explicit price source and timestamp/block.

For stablecoins, allow configured deviations instead of blindly assuming all stablecoins are exactly $1.

---

# 32. Reporting

CLI should expose both strategy-level and aggregate metrics.

Example:

```text
Block Range: 60,000,000 - 60,010,000
Chain: Polygon

Strategy              Candidates  Simulated  Profitable  Net Profit
--------------------------------------------------------------------
Arbitrage                  842         321         118     $8,421.19
Liquidation                 39          22          11     $3,912.42
Sandwich                    73          31          14     $1,842.77
JIT                         18          15           7       $931.20
JIT+Arb                      9           7           4       $714.03

TOTAL                                                       $15,821.61
```

The CLI must make it clear whether the result is:
- realized historical MEV
- hypothetical opportunity
- simulation result
- post-cost net profit

---

# 33. Example Opportunity Record

```json
{
  "chain": "polygon",
  "block": 60000000,
  "strategy": "arbitrage",
  "status": "profitable",
  "route": [
    {"dex": "DEX_A", "token_in": "USDC", "token_out": "WETH"},
    {"dex": "DEX_B", "token_in": "WETH", "token_out": "USDC"}
  ],
  "input_amount": "100000000000",
  "output_amount": "101250000000",
  "gross_profit": "1250000000",
  "gas_cost": "31000000",
  "flashloan_fee": "8000000",
  "protocol_fees": "0",
  "builder_payment": "0",
  "net_profit": "1211000000",
  "confidence": 0.99
}
```

---

# 34. Testing Strategy

Testing must be deterministic and fixture-driven.

## Unit tests

- ABI decode
- event decode
- trace ordering
- transfer extraction
- swap normalization
- route graph
- liquidation parser
- sandwich detector
- JIT detector
- P&L formulas

## Integration tests

Use real historical blocks as fixtures.

For each fixture include:

```text
block
transaction hashes
expected normalized swaps
expected detector output
expected simulation result
expected net P&L
```

## Property tests

Examples:
- token cycle conservation
- no negative balances unless protocol semantics permit it
- flash loan must be repaid
- gas cost cannot be negative
- `net_profit <= gross_profit + allowed valuation effects`

## Regression tests

Every false positive discovered in research becomes a fixture.

---

# 35. Performance Requirements

The system should be optimized for large historical ranges.

Avoid:
- repeated ABI parsing
- repeated RPC calls
- repeated state loading
- REVM execution for obviously unprofitable candidates

Use:
- cached ABI decoders
- compact normalized records
- parallel block processing
- per-block or per-shard workers
- immutable shared protocol metadata
- hot caches for pools/tokens

The pipeline should support:

```text
ingest in parallel
normalize in parallel
candidate detection in parallel
REVM simulation in bounded worker pool
```

Do not spawn unbounded REVM jobs.

---

# 36. Concurrency Model

Recommended architecture:

```text
Producer:
    blocks -> normalized records

Consumers:
    detector workers

Simulation queue:
    candidate -> bounded REVM worker pool

Storage writer:
    batch writes
```

Use channels / lock-free or low-contention data structures where justified.

Avoid adding Redis solely for queueing in the first version.

---

# 37. Error Handling

Never silently convert a missing trace or missing state into an invalid zero value.

Errors should distinguish:

```text
MissingData
DecodeError
UnsupportedProtocol
SimulationRevert
StateUnavailable
RPCError
StorageError
OptimizationFailure
```

A candidate can be `unverified` when data is insufficient, but must not be marked profitable.

---

# 38. Security / Correctness Rules

- Never trust token symbols or decimals from calldata.
- Resolve token metadata from verified contract state/configuration.
- Treat proxy contracts carefully.
- Account for fee-on-transfer tokens.
- Treat rebasing tokens separately.
- Do not assume ERC-20 compliance is perfect.
- Validate chain ID.
- Detect transaction reverts.
- Do not include reverted transactions as successful MEV.
- Preserve exact integer arithmetic.
- Avoid floating-point math in core accounting.
- Use decimal/fixed-point representations for valuation and reporting.

---

# 39. Relationship to MEV Zone / MEV Inspect

The target is **behavioral and architectural inspiration**, not a claim that this specification reproduces proprietary internals.

Publicly observable concepts and open-source reference implementations show a useful pipeline:

```text
raw blockchain data
   -> traces
   -> ABI / protocol classification
   -> swaps / transfers
   -> MEV detectors
   -> profit extraction
```

The public Flashbots `mev-inspect-py` code provides concrete examples for:
- trace classification
- liquidation parsing
- sandwich detection
- normalized swap-based analysis

MEV Zone itself should be treated as an external explorer/reference product. Do not rely on undocumented private APIs.

---

# 40. Recommended Development Order

## Phase 1 - Primitives and ingestion

Implement:
- chain abstraction
- block ingestion
- transaction ingestion
- receipts/logs
- trace ingestion
- normalized models
- local storage

Acceptance criterion:

```text
Given block range X-Y,
all required normalized data is available locally.
```

## Phase 2 - Decoder and protocol registry

Implement:
- ABI cache
- protocol registry
- function/event decoding
- transfer extraction
- swap normalization

Acceptance:

```text
Known DEX transactions become normalized Swap objects.
```

## Phase 3 - Historical detectors

Implement in order:

1. Arbitrage
2. Liquidation
3. Sandwich
4. JIT
5. Backrun
6. Front-run
7. Composite JIT+Arb

Acceptance:

```text
Known historical fixtures are classified correctly.
```

## Phase 4 - REVM

Implement:
- state adapter
- deterministic block context
- candidate simulation
- exact token delta extraction
- gas accounting

Acceptance:

```text
Simulation reproduces known historical outcomes within expected deterministic limits.
```

## Phase 5 - Opportunity engine

Implement:
- pool snapshot
- arbitrage route generation
- amount optimization
- liquidation candidate generation
- JIT candidate generation

Acceptance:

```text
Historical opportunities can be reconstructed as hypothetical candidates.
```

## Phase 6 - Flash loans

Implement:
- Balancer flash loan model
- Aave flash loan model
- atomic repayment checks

## Phase 7 - Live mempool

Implement:
- pending tx ingestion
- victim classification
- post-state simulation
- backrun/sandwich opportunity generation

## Phase 8 - Competition model

Add only after deterministic opportunity research works:
- searcher competition
- builder payment
- priority ordering
- latency
- bundle inclusion assumptions

---

# 41. CLI Design

Example commands:

```bash
mev-backtest ingest \
  --chain polygon \
  --from-block 60000000 \
  --to-block 60001000
```

```bash
mev-backtest detect \
  --chain polygon \
  --from-block 60000000 \
  --to-block 60001000 \
  --strategies arb,liquidation,sandwich,jit
```

```bash
mev-backtest simulate \
  --chain polygon \
  --from-block 60000000 \
  --to-block 60001000 \
  --strategy arb \
  --flashloan balancer
```

```bash
mev-backtest report \
  --chain polygon \
  --from-block 60000000 \
  --to-block 60001000 \
  --format table
```

Optional future command:

```bash
mev-live --chain polygon --strategies arb,sandwich,backrun
```

---

# 42. Configuration

Use a typed config file.

Example:

```toml
[chain]
name = "polygon"
chain_id = 137
rpc_url = "..."
ws_url = "..."

[storage]
backend = "rocksdb"
path = "./data/polygon"

[simulation]
enable_revm = true
max_concurrent = 16

[flashloan.balancer]
enabled = true

[flashloan.aave]
enabled = false

[detectors]
arbitrage = true
liquidation = true
sandwich = true
backrun = true
frontrun = true
jit = true
jit_arb = true

[limits]
max_hops = 4
max_candidates_per_block = 5000
minimum_profit_usd = 1.0
```

---

# 43. No-Competition Backtest Mode

The initial backtester explicitly assumes:

```text
No searcher competition
No mempool simulation
No latency model
No bundle auction
No failed inclusion model
```

It answers:

> "If a hypothetical professional searcher had access to the historical state and could execute the detected strategy without competing with other searchers, what deterministic net profit was available?"

This is intentionally not a realistic live execution probability model.

Later, add competition as a separate simulation layer rather than contaminating the baseline.

---

# 44. Output Metrics

Per strategy:

```text
opportunities_detected
candidates_simulated
profitable_candidates
unprofitable_candidates
reverted_candidates
minimum_profit
maximum_profit
mean_profit
median_profit
p95_profit
sum_gross_profit
sum_gas_cost
sum_flashloan_cost
sum_protocol_fees
sum_builder_payments
sum_net_profit
```

Also:

```text
profit_by_block
profit_by_pool
profit_by_dex
profit_by_token
profit_by_searcher
profit_by_strategy
```

---

# 45. Searcher Identification

Use multiple identifiers:

```text
EOA sender
contract recipient/router
flash-loan initiator
transaction sequence
funding relationships (optional future)
```

Do not blindly assume `tx.from` is always the economic beneficiary.

Track:

```text
initiator
executor
beneficiary
```

when detectable.

---

# 46. Historical Reconstruction of a Professional Bot

The backtester should eventually answer questions like:

```text
For the last N blocks on Polygon:

How many arbitrage opportunities existed?
How many were theoretically profitable?
What was the optimal input size?
How much gas would have been spent?
How much would a Balancer flash loan have cost?
What was the net profit?
How much came from JIT?
How much from JIT + Arb?
```

This is different from merely counting transactions labeled as MEV.

---

# 47. What Not to Build Yet

Do not initially build:
- generic AI/ML opportunity prediction
- neural routing models
- full cross-chain routing
- generalized intent solver
- full production execution contract
- full validator/builder network simulator
- complex distributed services
- Redis/Postgres dependency stack

First make deterministic on-chain analysis and REVM simulation correct.

---

# 48. Quality Gates for Every Detector

A detector is not complete until it has:

```text
1. normalized inputs
2. explicit algorithm
3. evidence / explainability
4. candidate schema
5. false-positive tests
6. historical fixtures
7. REVM validation where appropriate
8. cost accounting
9. CLI visibility
10. deterministic regression test
```

---

# 49. Coding Agent Instructions

When implementing this specification:

1. Keep modules small and composable.
2. Prefer strongly typed Rust structures over loosely typed maps.
3. Use Alloy for Ethereum/EVM types, ABI handling, RPC, and primitives.
4. Use REVM for deterministic simulation.
5. Avoid ethers-rs.
6. Do not let strategy code make arbitrary RPC calls.
7. Do not add Redis/Postgres unless explicitly required.
8. Do not prematurely optimize by making the architecture incomprehensible.
9. Preserve exact integer arithmetic in accounting.
10. Add tests before extending detector scope.
11. Every detector must explain why it classified something.
12. Every profitable opportunity must be REVM-verifiable before it is reported as `profitable`.
13. A detector may produce a `candidate` without simulation, but must not call it a verified opportunity.
14. Keep protocol-specific code behind registries/adapters.
15. Do not couple the historical engine to the live mempool engine.

---

# 50. Definition of Done for the First Useful Release

The first genuinely useful release should be able to do this end-to-end:

```text
Polygon block range
      ↓
local ingestion
      ↓
trace + log + transaction normalization
      ↓
V2/V3 swap decoding
      ↓
Historical arbitrage detection
      ↓
Hypothetical opportunity generation
      ↓
REVM simulation
      ↓
Balancer flashloan accounting
      ↓
gas accounting
      ↓
net profit calculation
      ↓
CLI report
      ↓
local cached dataset for re-runs
```

Then add:

```text
liquidation
sandwich
JIT
backrun
front-run
JIT + Arb
```

in that order only after the baseline pipeline is reliable.

---

# 51. Reference Sources

These are public references for behavior and architectural ideas. They are not a specification of undocumented MEV Zone internals.

- MEV Zone live explorer: https://explorer.mev.zone/mevlive
- MEV Zone documentation: https://mevzone.gitbook.io/mevzone/
- Flashbots `mev-inspect-py`: https://github.com/flashbots/mev-inspect-py
- Flashbots `mev-inspect` sandwich detector example: https://github.com/flashbots/mev-inspect-py/blob/main/mev_inspect/sandwiches.py
- Flashbots `mev-inspect` liquidation detector example: https://github.com/flashbots/mev-inspect-py/blob/main/mev_inspect/liquidations.py
- Flashbots `mev-inspect` trace classifier example: https://github.com/flashbots/mev-inspect-py/blob/main/mev_inspect/classifiers/trace.py
- Alloy documentation: https://alloy.rs/
- REVM: https://github.com/bluealloy/revm

Important: `mev-inspect-rs` is an archived historical reference implementation. Use it for ideas and detector semantics, not as the current foundation of the new system.

---

# 52. Final Architectural Principle

The central design should remain:

```text
                    ON-CHAIN / MEMPOOL DATA
                              │
                              ▼
                      NORMALIZE EVERYTHING
                              │
                              ▼
                     DETECT CHEAPLY FIRST
                              │
                              ▼
                         CANDIDATE
                              │
                              ▼
                    SIMULATE EXACTLY WITH REVM
                              │
                              ▼
                    OPTIMIZE INPUT / ROUTE
                              │
                              ▼
                      FULL COST ACCOUNTING
                              │
                              ▼
                      VERIFIED NET PROFIT
```

And the three states must remain distinct:

```text
Candidate
    !=
Opportunity
    !=
Realized MEV
```

That separation is the foundation for a reliable MEV research/backtesting system and prevents an explorer-style detector from being mistaken for a profitable live strategy engine.
