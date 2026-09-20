# MEV Detection & Opportunity Engine

## Engineering Specification for Rust + Alloy + REVM

**Target users:** Coding Agents (OpenCode / Cursor / Claude Code / Codex / similar)

**Document status:** This file merges and supersedes the former specs:

- `docs/mev_detection_engine_spec.md`
- `docs/mev_opportunity_detection_and_pnl_spec.md`

Overlapping material was deduplicated; where both covered the same topic, the fuller / more precise wording was retained.

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

**Out of scope for the opportunity/accounting layer alone** (covered elsewhere in the full system): node/RPC infrastructure details, mempool transport implementation, smart-contract executor implementation, UI/dashboard implementation.

---

## 1. Purpose / Executive Summary

The system should be designed as two related but distinct engines that share normalized primitives but must not be conflated:

1. **Historical MEV Classification Engine**
   - Answers: **"What actually happened on-chain?"**
   - Inputs: blocks, transactions, receipts, logs, traces, token transfers, pool/protocol state.
   - Detects realized arbitrage, liquidations, sandwiches, backruns, front-runs, JIT, and composite strategies.

2. **Opportunity Engine**
   - Answers: **"What could a professional MEV searcher have done from this state?"**
   - Inputs: historical state and optionally pending/mempool transactions.
   - Produces candidates, then validates them with deterministic REVM simulation.
   - Computes realistic gross and net P&L including gas, protocol fees, flash-loan fees, slippage, and optional builder/validator payment assumptions.

The opportunity/accounting layer specifically must:

1. Detect historical and hypothetical MEV opportunities.
2. Detect live/pending opportunities when pending transaction data is available.
3. Build candidate strategies.
4. Simulate candidates with deterministic EVM execution (REVM).
5. Optimize trade size / route / strategy parameters.
6. Calculate gross revenue, every material cost, and net profit.
7. Reject false or unprofitable opportunities.
8. Produce a normalized `OpportunityResult` that can be consumed by a backtester or future execution engine.

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

# 2. Core Design Distinctions

## 2.1 Observed / Hypothetical / Live

The system MUST distinguish between:

### A. Observed MEV (Realized)

Something that actually happened on-chain.

Example:

```text
Block N
  Tx A: USDC -> WETH on DEX A
  Tx B: WETH -> USDC on DEX B
```

The detector asks:

> Was this transaction an executed MEV strategy, and what was its realized P&L?

### B. Hypothetical Historical Opportunity

Something that was possible given historical state, whether or not a searcher actually captured it.

Example:

```text
Historical state S
DEX A: ETH = 2500
DEX B: ETH = 2520
```

The detector asks:

> Could a strategy have profitably executed from this state?

### C. Live/Pending Opportunity

A pending transaction or state transition creates a potential opportunity.

Example:

```text
Pending whale swap
        ↓
Simulate pending transaction
        ↓
State S'
        ↓
Search arbitrage/backrun/sandwich/JIT candidates
```

These three modes MUST NOT be conflated.

A historical explorer may observe a DEX cycle and report realized profit; an opportunity engine must instead detect a state discrepancy and determine whether any input size can exploit it *after* fees, price impact, gas, flash-loan fees, and other costs.

```text
Historical classifier != Opportunity scanner
```

The same distinction applies to liquidations, sandwiches, backruns, front-runs, and JIT.

## 2.2 Candidate != Opportunity != Realized MEV

Use explicit types:

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

Every result must clearly identify mode:

```rust
enum OpportunityMode {
    Realized,
    HistoricalHypothetical,
    LivePending,
}
```

This distinction is essential for backtesting and prevents treating theoretical opportunities as guaranteed realized revenue.

---

# 3. Supported MEV Strategy Types

The engine should support the following detector IDs:

```text
arbitrage
liquidation
sandwich
backrun
frontrun
jit
jit_arbitrage
```

Each detector MUST be independently testable.

Composite strategies MUST be represented explicitly rather than hidden inside another detector.

---

# 4. Reference Architecture and Detector Pipeline

## 4.1 System Architecture

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

## 4.2 Detector Pipeline

Every strategy should follow this pipeline:

```text
Raw/normalized market activity
        ↓
Cheap candidate detection
        ↓
Candidate generation
        ↓
Parameter optimization
        ↓
REVM simulation
        ↓
Exact asset flows
        ↓
Cost calculation
        ↓
Net P&L
        ↓
Profitability filter
        ↓
OpportunityResult
```

IMPORTANT: Do NOT run full REVM simulation on every transaction or every theoretical route. Use a cheap pre-filter first.

---

# 5. Ingestion Responsibilities

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

# 6. Normalized Data Model

The system should use chain-agnostic normalized primitives. All detectors should operate on these instead of raw protocol-specific objects.

## 6.1 Block

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

## 6.2 Transaction

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

## 6.3 Trace

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

## 6.4 Token Transfer

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

## 6.5 Normalized Swap

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

    // Optional but strongly preferred for exact analysis
    price_before: Option<U256>,
    price_after: Option<U256>,
    sqrt_price_before: Option<U256>,
    sqrt_price_after: Option<U256>,
    liquidity_before: Option<U256>,
    liquidity_after: Option<U256>,
    tick_before: Option<i32>,
    tick_after: Option<i32>,

    gas_used: Option<u64>,
}
```

The same logical swap MUST look identical whether it came from Uniswap V2/V3, SushiSwap, Trader Joe, Pangolin, QuickSwap, other V2/V3 forks, or supported aggregators.

For concentrated-liquidity pools, populate sqrt-price / liquidity / tick fields when state reconstruction allows it.

## 6.6 State Snapshot

The simulator must be able to access enough state to reproduce execution:

```rust
struct StateSnapshot {
    block_number: u64,
    block_timestamp: u64,

    // token balances / contract storage as required
    // pool reserves / ticks / liquidity
    // protocol account state
    // oracle state
    // fee configuration
}
```

## 6.7 Transaction Context (simulation / pending)

```rust
struct TxContext {
    tx_hash: Option<TxHash>,
    from: Address,
    to: Option<Address>,
    calldata: Bytes,
    value: U256,

    gas_limit: u64,
    gas_price: U256,
    max_fee_per_gas: Option<U256>,
    max_priority_fee_per_gas: Option<U256>,
}
```

---

# 7. Protocol Registry

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

# 8. Trace Classification Architecture

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

# 9. Opportunity Candidate Model

All detectors should produce a common candidate type.

```rust
struct OpportunityCandidate {
    strategy: StrategyType,
    chain_id: u64,
    block_number: Option<u64>,

    trigger_tx: Option<TxHash>,

    participants: Vec<Address>,
    pools: Vec<Address>,
    tokens: Vec<Address>,

    route: Vec<RouteStep>,

    // Strategy-specific parameters
    parameters: StrategyParameters,
}
```

The candidate is NOT yet considered profitable.

A candidate becomes an opportunity only after successful simulation and accounting.

Strategy-specific candidate structs (e.g. `ArbitrageCandidate`, `LiquidationCandidate`) may be used internally and then mapped into `OpportunityCandidate`.

---

# 10. Route Representation

```rust
struct RouteStep {
    dex: DexId,
    pool: Address,
    token_in: Address,
    token_out: Address,
}
```

Example:

```text
USDC
 ↓
Uniswap V3 / pool A
 ↓
WETH
 ↓
Trader Joe / pool B
 ↓
USDC
```

Route comparison must support:

- 2-hop arbitrage
- 3-hop arbitrage
- N-hop cycles
- split routes
- multiple pools for the same pair

---

# 11. Arbitrage Detection

## 11.1 Historical Executed Arbitrage

Detect closed token cycles where the net token amount returned to the starting asset exceeds the initial amount, and the flow is attributable to the same strategy actor and route.

Basic condition:

```text
final_amount_X > initial_amount_X
```

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
6. Require the cycle to return to a token controlled by the original trader/searcher (**flow-ownership**).
7. Produce an `ArbitrageCandidate`.
8. Re-simulate the route with REVM when exact historical state is available.
9. Calculate gross and net P&L.

Do **not** classify every profitable multi-swap transaction as arbitrage without checking flow ownership.

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

## 11.2 Historical Hypothetical Arbitrage

From a single state snapshot, build a graph of tradable pairs and find cycles / candidate routes.

For each route:

1. Estimate a cheap maximum-feasible trade size.
2. Simulate the route.
3. Optimize the input amount.
4. Calculate complete costs.
5. Keep only positive-net-profit candidates.

Do not assume that a visible price difference is profitable; account for pool fees, price impact, protocol fees, gas, flash-loan fee, transfer taxes where applicable, and execution constraints.

## 11.3 Live / Pending Arbitrage

```text
pool prices / post-pending state
    -> generate candidate routes
    -> optimize input amount
    -> simulate route with REVM
    -> calculate net profit
```

## 11.4 Arbitrage Search Algorithm

Use a graph representation:

```text
Node = token
Edge = pool swap
```

Start simple:

```text
For each token:
    discover 2-hop cycles
    discover 3-hop cycles
```

Then extend to N-hop cycles, split routes, and multi-path optimization.

Support:

- direct 2-pool arbitrage
- triangular routes
- longer cyclic paths

Do not search an unbounded graph. Do not brute-force every possible graph path.

Recommended controls:

```text
max_hops
max_pools_per_token
max_candidates_per_block
liquidity threshold
minimum expected edge
```

Use pruning based on:

- known viable token pairs / token compatibility
- pool liquidity
- pool fee
- estimated price edge / preliminary price edge
- gas budget
- token whitelist/blacklist
- maximum route length
- estimated price impact

## 11.5 Arbitrage Candidate Math

For a route:

```text
amount_0
  → swap_1
amount_1
  → swap_2
amount_2
  → ...
amount_final
```

Gross profit:

```text
GrossProfit = amount_final - amount_0
```

For exact profitability this is NOT sufficient. Use:

```text
NetProfit
    = FinalValue
    - InitialValue
    - GasCost
    - ProtocolFees
    - FlashLoanFee
    - BuilderPayment
    - OtherExecutionCosts
```

---

# 12. Trade Size Optimization

Arbitrage profit is generally non-linear because of price impact.

The detector MUST NOT assume that the largest possible trade produces the largest profit.

Define:

```text
P(x) = net profit when input size = x
```

Find:

```text
x* = argmax P(x)
```

For each candidate route:

```text
candidate amount range
    -> coarse search
    -> local refinement
    -> exact REVM validation
```

Possible implementation order:

### Phase 1 — Coarse grid search

```text
x
0.25x
0.5x
1x
2x
4x
...
```

Example dollar grid:

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

### Phase 2 — Refinement around the best candidate

### Phase 3 — Optional numerical optimizer

Every final candidate MUST be re-simulated at the selected optimal amount.

For concentrated liquidity pools, include tick-boundary effects.

---

# 13. Liquidation Detection

Liquidation detection has two modes: historical realized and opportunity generation.

## 13.1 Historical Realized Liquidation

Detect protocol-specific liquidation functions/events.

Protocol-specific examples may include:

```text
Aave liquidationCall
Compound liquidateBorrow
other lending liquidation entrypoints
```

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

The detector should not rely on function name alone. Validate using:

- target contract
- decoded selector/function
- resulting transfers
- debt repayment
- collateral received

The historical Flashbots implementation follows this parent/child trace structure and then delegates to a protocol-specific liquidation classifier. Treat that as a conceptual baseline.

## 13.2 Liquidation Candidate

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

## 13.3 Live / Historical Liquidation Opportunity

Monitor positions and protocol state.

Inputs / required calculations may include:

```text
collateral balance / collateral_value
debt balance / borrowed_value
oracle price
LTV
liquidation threshold
health factor or equivalent metric
liquidation bonus
protocol-specific close factor / max repay
max_close_factor
```

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

Estimate:

```text
GrossLiquidationRevenue
    = value_of_collateral_received
    - value_of_debt_repaid
```

Then simulate the actual protocol call and use exact token amounts.

Do not mark a position as profitable solely because it is liquidatable.

## 13.4 Liquidation P&L

```text
NetProfit
    = CollateralReceivedValue
    - DebtRepaidValue
    - GasCost
    - FlashLoanFee
    - Slippage
    - ProtocolSpecificFees
    - BuilderPayment
```

If the liquidation requires a flash loan:

```text
FlashLoanFee
    = BorrowedAmount × FlashLoanFeeRate
```

The system MUST support protocol-specific fee models.

---

# 14. Sandwich Detection

## 14.1 Historical Sandwich Structure

Canonical pattern:

```text
TX A: searcher front leg
TX B: victim swap
TX C: searcher back leg
```

Heuristics / required signals:

1. Sort swaps by `(transaction_position, trace_address)`.
2. Front transaction occurs before victim; consider the first swap as a front candidate.
3. Exclude known router contracts as the searcher identity when necessary.
4. Search later swaps in the same block; back transaction occurs after victim.
5. Require same pool/contract where appropriate; front and back interact with the same relevant pool(s).
6. Require same token direction between front and victim; victim trades the affected pair in the expected direction.
7. Require reverse direction for the searcher's back leg.
8. Require victim sender differs from the searcher.
9. Require the searcher identity is consistent (or strong attribution evidence exists).
10. Validate that the victim received a worse execution.
11. Validate that the complete sequence is profitable after costs.

The published Flashbots sandwich detector uses transaction position + trace ordering and searches for a same-direction victim swap followed by a reverse-direction swap by the same searcher on the same pool. Treat that as a conceptual baseline, not a complete production-grade detector.

## 14.2 Important Limitation and False Positive Protection

Ordering alone is not proof of malicious or intentional front-running.

Do NOT classify a sandwich solely because:

```text
same pool
+
transaction before victim
+
transaction after victim
```

Also verify price impact and economic causality.

Suggested checks:

```text
price_before_front
price_after_front
victim_execution_price
price_after_back
```

The victim should generally receive a worse execution because of the searcher's front leg.

Use a confidence score and explicit evidence fields:

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

## 14.3 Live Sandwich Opportunity

When pending transactions are available:

```text
Pending victim tx
        ↓
Decode
        ↓
Check whether it moves price materially
        ↓
Simulate victim on current state
        ↓
Construct candidate front leg
        ↓
Construct candidate back leg
        ↓
Simulate:
    Front → Victim → Back
        ↓
Calculate NetProfit
```

Inputs:

- pending victim transaction
- current pool state
- candidate front-run transaction
- candidate back-run transaction

Candidate should be rejected if:

```text
NetProfit <= min_profit_threshold
```

or if the victim transaction would revert under the simulated sequence.

Never treat a mempool transaction as a guaranteed inclusion or ordering.

---

# 15. Backrun Detection

## 15.1 Historical Backrun

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

If the profitability materially increases due to A (the opportunity exists primarily because of A), classify B as a probable backrun candidate.

Do not classify based solely on adjacent transaction indexes. The key requirement is economic causality, not merely adjacency.

## 15.2 Live Backrun

```text
Current State S
     ↓
simulate pending Tx A
     ↓
State S'
     ↓
run arbitrage / liquidation / other detectors on S'
     ↓
REVM
     ↓
Net P&L
```

This is one of the most important uses of deterministic REVM in the live architecture.

---

# 16. Front-run Detection

## 16.1 Historical Front-run

A transaction before a victim is NOT automatically a front-run.

Candidate pattern / required signals:

```text
BotTx before VictimTx
+
shared market/pool context
+
bot transaction changes relevant state
+
victim execution worsens because of bot tx
+
bot benefits economically
```

Minimum evidence should include ordering, same/correlated asset/pool, measurable state change, victim execution degradation, and economic benefit to the searcher.

## 16.2 Live Front-run Candidate

```text
pending victim
    -> simulate victim
    -> generate candidate state-changing transaction
    -> simulate candidate + victim
    -> measure victim degradation
    -> simulate complete strategy if back leg exists
    -> calculate searcher profit
```

The detector should produce both candidate confidence and economic profitability.

Do NOT use a heuristic confidence score as a replacement for REVM profitability.

Any final classification should be based on explicit evidence rather than labels alone.

---

# 17. Uniswap V3 JIT Liquidity

Focus initially on Uniswap V3-style concentrated liquidity.

Canonical historical pattern:

```text
Mint liquidity
      ↓
Large / targeted swap through the range
      ↓
Burn liquidity
```

Detect when / signals:

- the same LP/provider address performs mint and burn
- same pool
- the position exists for a very short time / mint and burn close in time / block sequence
- the relevant swap occurs while liquidity is active
- liquidity overlaps the active price range / mint/burn tick range overlaps the swap's executed range
- meaningful fees earned by the minted liquidity

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
= collected fees / LPFeesEarned
- mint/burn gas
- swap-related gas attribution
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

The detector must distinguish normal LP activity from JIT activity.

---

# 18. JIT + Arbitrage

Composite pattern:

```text
Mint liquidity
      ↓
Victim / large swap / price-moving order
      ↓
Arbitrage opportunity / fee capture
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

Compute both LP fee income and arbitrage profit, then combine all costs.

The system must attribute economics to the combined strategy without double-counting the same fee or trade flow.

Example:

```text
LP fee income = $500
arbitrage profit = $900
extra gas = $30
flashloan fee = $15

Net strategy P&L = $1,355
```

---

# 19. REVM Simulation Layer

REVM should be used as the deterministic execution validator and source of truth for exact execution results.

Detector heuristics create candidates. REVM determines whether the candidate actually works.

## 19.1 Simulation Responsibilities

- Reconstruct or load exact state.
- Apply transaction sequence.
- Execute candidate calldata.
- Track token balances.
- Track gas.
- Capture revert reasons.
- Capture logs and state changes.
- Return exact post-state.

## 19.2 Simulation Pipeline

```text
Candidate
   ↓
Build transaction/bundle sequence
   ↓
Load historical/current state
   ↓
Execute in REVM
   ↓
Collect:
    token balances
    native balance
    gas used
    logs
    revert status
    state changes
   ↓
Accounting
```

## 19.3 Simulation Inputs

```rust
struct SimulationRequest {
    chain_id: u64,
    block_context: BlockContext,
    state_snapshot: StateSnapshot,
    transactions: Vec<SimulatedTransaction>,
}
```

## 19.4 Simulation Result

```rust
struct SimulationResult {
    success: bool,
    gas_used: u64,

    asset_flows: Vec<AssetFlow>,

    revert_reason: Option<String>,

    // Native/token balances before and after
    balance_deltas: Vec<BalanceDelta>,
}
```

`SimulationResult` should also support reporting:

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

## 19.5 Candidate Simulation Examples

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

# 20. Exact P&L Accounting

This is a critical component.

Do NOT calculate profit merely from gross token output. All material costs must be represented explicitly.

Every result must expose a full breakdown. Use exact token accounting first. Convert to USD only after a reference pricing step.

Never calculate P&L by simply subtracting nominal USD values if token quantities have not been reconciled.

## 20.1 Revenue

Possible revenue sources:

```text
Arbitrage spread
Liquidation bonus
JIT LP fees
Backrun profit
Sandwich spread
Other captured price discrepancy
```

## 20.2 Costs

At minimum support:

```text
Gas cost
Flash loan fee
DEX fees
Protocol fees
Slippage / price impact
Builder payment / validator payment
Token transfer fees when relevant
Approval / setup cost when relevant
```

High-level formula:

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

## 20.3 Gas Cost

For historical transactions:

```text
GasCostNative
    = gas_used × effective_gas_price
```

For EIP-1559, `EffectiveGasPrice` must be taken from the actual transaction receipt/context where available.

For hypothetical simulations:

```text
estimated_gas_cost_native
    = simulated_gas_used * assumed_effective_gas_price
```

Support configurable assumptions for:

```text
gas_used
base_fee
priority_fee
```

Keep chain-native amount and USD valuation separately.

Do not hard-code one global USD gas price. Do not silently assume current gas prices for historical backtests.

## 20.4 Flash Loan Accounting

Initial backtest should support:

- Balancer flash loans
- Aave flash loans

Optional future support:

- Uniswap V2 flash swaps
- other chain-native lending/flash mechanisms

Generic model:

```text
FlashLoanFee
    = BorrowAmount × FeeRate
```

Flash-loan simulation must model:

```text
borrowed amount
+ fee
= exact repayment requirement
```

The engine MUST verify repayment in the simulation.

A candidate is invalid if:

```text
repayment_amount > available_final_balance
```

or the flash-loan callback transaction reverts.

A candidate is only profitable if repayment succeeds in the same atomic simulation.

## 20.5 DEX Fee Accounting

Every pool type must expose or infer its effective fee.

Examples:

```text
Uniswap V2: percentage fee
Uniswap V3: pool fee tier
Curve: pool-specific fee
Balancer: pool-specific swap fee
```

But do not simply subtract a theoretical fee from the result when REVM can provide the exact outcome.

Prefer exact simulation result over generic fee formula.

Generic fee formulas should mainly be used for candidate pre-filtering.

## 20.6 Slippage / Price Impact

Slippage must be measured against the correct reference.

Possible references:

```text
pre-trade pool state
oracle/reference price
counterfactual state without the strategy leg
```

For MEV analysis, a particularly useful measure is:

```text
strategy outcome
vs
counterfactual outcome without strategy
```

For sandwich analysis, compare victim execution:

```text
Victim output with front-run
vs
Victim output without front-run
```

This lets the system quantify victim price degradation.

## 20.7 Builder / Validator Payment

Support explicit attribution of:

```text
coinbase transfers
priority fee
bundle payment
validator payment
builder payment
```

For historical analysis, determine whether the searcher transferred value to the block beneficiary or paid through transaction gas.

The same payment must not be counted twice.

## 20.8 Profit in Native and USD Terms

Every result should preserve native/token-denominated P&L first.

Do NOT immediately convert everything to USD and discard the original units.

```rust
struct ProfitBreakdown {
    gross: Vec<AssetAmount>,
    costs: Vec<CostItem>,
    net: Vec<AssetAmount>,

    // Optional normalized values
    gross_usd: Option<Decimal>,
    total_cost_usd: Option<Decimal>,
    net_usd: Option<Decimal>,
}
```

A simpler signed form is also acceptable for reporting when asset vectors are collapsed after valuation:

```rust
struct ProfitBreakdownSimple {
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

USD conversion is an accounting layer, not the source of truth.

## 20.9 Cost Item Model

Use explicit cost items:

```rust
struct CostItem {
    kind: CostKind,
    token: Address,
    amount: U256,
    usd_value: Option<Decimal>,
}
```

Suggested enum:

```rust
enum CostKind {
    Gas,
    FlashLoanFee,
    DexFee,
    ProtocolFee,
    Slippage,
    BuilderPayment,
    ValidatorPayment,
    TokenTransferFee,
    Other,
}
```

Do not merge all costs into a single number internally.

## 20.10 Opportunity Result

Every profitable or rejected candidate should produce a structured result.

```rust
struct OpportunityResult {
    strategy: StrategyType,

    chain_id: u64,
    block_number: Option<u64>,
    trigger_tx: Option<TxHash>,

    success: bool,
    profitable: bool,

    route: Vec<RouteStep>,

    input: Vec<AssetAmount>,
    output: Vec<AssetAmount>,

    profit: ProfitBreakdown,

    gas_used: u64,

    simulation: SimulationSummary,

    confidence: Option<f64>,

    rejection_reason: Option<RejectionReason>,
}
```

## 20.11 Profitability Rule

Define configurable thresholds:

```text
min_net_profit_native
min_net_profit_usd
min_profit_after_risk
```

Basic rule:

```text
profitable = net_profit > threshold
```

Never classify an opportunity as profitable from gross revenue alone.

## 20.12 Rejection Reasons

Useful rejection reasons:

```text
NoRoute
NoLiquidity
SimulationReverted
InsufficientRepayment
NegativeGrossProfit
NegativeNetProfit
GasTooHigh
FlashLoanTooExpensive
PriceImpactTooHigh
UnknownTokenPrice
InvalidState
MissingProtocolData
UncertainAttribution
BelowProfitThreshold
```

This is important for strategy research because “not profitable” and “could not simulate” are different outcomes.

---

# 21. State Reconstruction

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

# 22. Storage

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

# 24. Counterfactual Simulation

For research-quality MEV analysis, support counterfactual simulations.

Examples:

### Sandwich

```text
Scenario A:
Victim only

Scenario B:
Front → Victim → Back
```

Compare:

```text
victim_output_A
victim_output_B
searcher_profit_B
```

### Backrun

```text
Scenario A:
State before trigger tx

Scenario B:
Trigger tx executed

Scenario C:
Trigger tx + backrun
```

### Arbitrage

```text
Scenario A:
No trade

Scenario B:
Arbitrage route
```

Counterfactual simulation is strongly preferred to heuristic attribution when practical.

---

# 25. Detection Workflows

## 25.1 Historical Opportunity Detection Workflow

```text
Historical Block
      ↓
Extract normalized swaps / calls / transfers
      ↓
Build market state
      ↓
Run cheap detectors
      ↓
Generate candidates
      ↓
Optimize parameters
      ↓
REVM simulation
      ↓
Exact asset-flow extraction
      ↓
Calculate all costs
      ↓
Calculate net profit
      ↓
Filter profitable candidates
      ↓
Return OpportunityResult
```

## 25.2 Live Opportunity Detection Workflow

When a pending transaction is available:

```text
Pending Tx
      ↓
Decode
      ↓
Classify market effect
      ↓
Simulate pending tx
      ↓
Create post-tx state
      ↓
Run opportunity detectors
      ↓
Generate strategy candidates
      ↓
Simulate candidate sequence
      ↓
Calculate all costs
      ↓
Return opportunity
```

For sandwich/front-run/backrun strategies, pending transaction ordering is essential.

---

# 26. Opportunity Ranking

Do not rank by gross profit.

Use:

```text
NetProfit
```

and optionally:

```text
NetProfit / Gas
NetProfit / Capital
NetProfit / Notional
```

For research output, return all values rather than a single composite score.

---

# 27. Multiple Candidate Routes

For an identical trigger/event, there may be many routes:

```text
Candidate A: DEX A → DEX B
Candidate B: DEX A → DEX C
Candidate C: DEX B → DEX C → DEX A
```

Each should be simulated independently.

Do not discard routes merely because a shorter route exists.

A longer route may have higher net profit because of deeper liquidity or lower fees.

---

# 28. Flashloan vs Own-Capital Mode

The simulation/accounting engine should support both:

```text
capital_mode = OwnCapital
capital_mode = FlashLoan
```

For the primary research use case, `FlashLoan` is preferred for capital-free feasibility studies.

But capital requirements should still be reported:

```text
required_capital
flash_loan_amount
```

---

# 29. Competition Modes

## 29.1 No-Competition Backtest Mode

Initial historical backtesting should intentionally model:

```text
No other searchers
No mempool competition
No auction competition
No probabilistic inclusion model
No latency model
No bundle auction
No failed inclusion model
```

The objective is:

> What theoretical net P&L was available from the historical state?

Or equivalently:

> If a hypothetical professional searcher had access to the historical state and could execute the detected strategy without competing with other searchers, what deterministic net profit was available?

This is NOT the same as executable live profit.

The result should be labeled:

```text
THEORETICAL_HISTORICAL_OPPORTUNITY
```

Later, add competition as a separate simulation layer rather than contaminating the baseline.

## 29.2 Competition-Aware Mode (Future)

Not required for the initial detector, but design interfaces so later this can model:

```text
other searchers
bundle competition
priority fees
builder payments
failed inclusion
ordering uncertainty
latency
```

Do not bake assumptions about competition into the base detector.

---

# 30. False Positive Control

False-positive prevention is a first-class requirement. Every detector must contain explicit false-positive tests.

## Arbitrage

Do not classify a round trip as profitable until exact token balances reconcile.

Reject:

- profitable-looking token transfers with no actual trading cycle
- unrelated transfers
- accounting artifacts

## Liquidation

Do not classify a generic transfer as liquidation without protocol-specific semantics.

Reject:

- unrelated collateral transfers
- failed/reverted liquidation calls
- nested protocol calls misclassified as independent liquidations

## Sandwich

Do not classify simply because two same-direction swaps surround a victim.

Reject:

- coincidental same-pool trades
- independent arbitrage before/after a victim
- same sender but unrelated transactions

## Backrun

Do not classify a transaction as backrun merely because it follows a whale transaction.

Reject:

- ordinary next-block/next-tx arbitrage not causally related to trigger

## Front-run

Do not classify merely based on transaction index.

Reject:

- transactions merely appearing before a victim

## JIT

Do not classify any short-lived LP position as JIT unless its active range overlaps relevant swap execution and fee generation.

Reject:

- ordinary LP mint/burn activity with long holding periods
- liquidity outside the relevant active range

---

# 31. Live vs Historical Capabilities Matrix

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

# 32. Initial MVP Scope

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

# 33. Suggested Rust Workspace

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

# 34. Interfaces

## 34.1 Detector Interface

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

Equivalent naming:

```rust
trait OpportunityDetector {
    fn detect(
        &self,
        context: &DetectionContext,
    ) -> Vec<OpportunityCandidate>;
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

## 34.2 Simulation Interface

```rust
pub trait OpportunitySimulator {
    fn simulate(
        &self,
        candidate: &MevCandidate,
        state: &StateSnapshot,
    ) -> anyhow::Result<SimulationResult>;
}
```

## 34.3 Accounting Interface

```rust
trait ProfitCalculator {
    fn calculate(
        &self,
        candidate: &OpportunityCandidate,
        simulation: &SimulationResult,
        context: &AccountingContext,
    ) -> OpportunityResult;
}
```

Detector, simulator, and accounting MUST remain separate.

## 34.4 Recommended Internal Pipeline API

Conceptually:

```rust
let candidates = detector.detect(&context);

for candidate in candidates {
    let optimized = optimizer.optimize(candidate, &context);

    let simulation = simulator.simulate(&optimized, &sim_context);

    let result = profit_calculator.calculate(
        &optimized,
        &simulation,
        &accounting_context,
    );

    if result.profitable {
        opportunities.push(result);
    }
}
```

This should be the central flow.

---

# 35. USD Pricing

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

# 36. Reporting

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

# 37. Examples

## 37.1 Example Opportunity Record (JSON)

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

## 37.2 Required Final Output Example (Human-Readable)

A profitable arbitrage result should be expressible as:

```text
Strategy: Arbitrage
Mode: Historical Hypothetical

Route:
USDC
 -> WETH on DEX_A
 -> USDC on DEX_B

Input:
100,000 USDC

Output:
101,420 USDC

Gross Profit:
1,420 USDC

Costs:
Gas:             $3.20
DEX Fees:        $48.10
Flashloan Fee:   $8.50
Builder Payment: $0.00
Slippage:        $21.30

Net Profit:
$1,338.90

Simulation:
SUCCESS

Profitable:
YES
```

A rejected opportunity should be equally explicit:

```text
Strategy: Backrun
Mode: Historical Hypothetical

Simulation:
SUCCESS

Gross Profit:
$42.00

Total Costs:
$47.80

Net Profit:
-$5.80

Profitable:
NO

Rejection:
NegativeNetProfit
```

---

# 38. Testing Strategy

Testing must be deterministic and fixture-driven.

## 38.1 Unit Tests

Every detector should have minimum tests for:

```text
positive case
negative case
reverted tx
zero profit
negative profit
high gas
high slippage
flashloan failure
multiple pools
multiple hops
same-block ordering
nested traces
```

Also cover:

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

Also include exact regression tests built from real historical transactions.

## 38.2 Integration Tests

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

## 38.3 Property Tests

Examples:

- token cycle conservation
- no negative balances unless protocol semantics permit it
- flash loan must be repaid
- gas cost cannot be negative
- `net_profit <= gross_profit + allowed valuation effects`

## 38.4 Regression / Golden Tests

Every false positive discovered in research becomes a fixture.

A detector regression test should look conceptually like:

```text
Input:
    block fixture
    state fixture
    protocol fixtures

Expected:
    strategy = arbitrage
    route = [poolA, poolB]
    input = 100000 USDC
    gross_profit = ...
    gas_cost = ...
    flashloan_fee = ...
    net_profit = ...
    profitable = true
```

For rejected candidates:

```text
profitable = false
rejection_reason = NegativeNetProfit
```

---

# 39. Precision Requirements

Use integer/fixed-point arithmetic wherever possible.

Do not use floating-point arithmetic for token accounting.

Recommended:

```text
U256 / U512
fixed-point decimal only at reporting boundaries
```

Profit calculations MUST be deterministic.

Preserve exact integer arithmetic. Avoid floating-point math in core accounting. Use decimal/fixed-point representations for valuation and reporting.

---

# 40. Determinism

Given identical:

```text
state
protocol metadata
transaction sequence
fee assumptions
price inputs
```

REVM simulation and accounting MUST produce identical outputs.

This is especially important for historical backtesting.

---

# 41. Performance Requirements

The system should be optimized for large historical ranges.

The detector layer should follow:

```text
Cheap filter
      ↓
Candidate reduction
      ↓
Targeted REVM simulation
      ↓
Exact accounting
```

Avoid:

- repeated ABI parsing
- repeated RPC calls
- repeated state loading
- REVM execution for obviously unprofitable candidates
- `all routes × all amounts × full simulation` without pre-filtering

Use:

- cached ABI decoders
- compact normalized records
- parallel block processing
- per-block or per-shard workers
- immutable shared protocol metadata
- hot caches for pools/tokens
- caching for repeated state/route calculations where safe

The pipeline should support:

```text
ingest in parallel
normalize in parallel
candidate detection in parallel
REVM simulation in bounded worker pool
```

Do not spawn unbounded REVM jobs.

---

# 42. Concurrency Model

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

# 43. Error Handling

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

# 44. Security / Correctness Rules

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

# 45. Relationship to MEV Zone / MEV Inspect

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

# 46. Recommended Development Order

Merge of system-build phases and detector-first phases. Historical classification supplies the primitives required for live opportunity detection.

## Phase 1 — Primitives and ingestion

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

## Phase 2 — Decoder and protocol registry

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

## Phase 3 — Historical detectors

Implement in order:

1. Historical arbitrage
2. Historical liquidation
3. Historical sandwich
4. Historical backrun
5. Historical JIT
6. Historical front-run attribution
7. Composite JIT+Arb

Acceptance:

```text
Known historical fixtures are classified correctly.
```

## Phase 4 — REVM

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

## Phase 5 — Opportunity engine and optimization

Implement:

- pool snapshot
- arbitrage route generation
- amount optimization
- liquidation candidate generation
- JIT candidate generation
- arbitrage route/amount optimization improvements

Acceptance:

```text
Historical opportunities can be reconstructed as hypothetical candidates.
```

## Phase 6 — Flash loans

Implement:

- Balancer flash loan model
- Aave flash loan model
- atomic repayment checks

## Phase 7 — Live mempool

Implement:

- pending tx ingestion
- victim classification
- post-state simulation
- live pending backrun
- live sandwich
- live front-run
- live liquidation
- live arbitrage

## Phase 8 — Competition model

Add only after deterministic opportunity research works:

- searcher competition
- builder payment
- priority ordering
- latency
- bundle inclusion assumptions

---

# 47. CLI Design

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

# 48. Configuration

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

# 49. Output Metrics

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

# 50. Searcher Identification

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

# 51. Historical Reconstruction of a Professional Bot

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

# 52. What Not to Build Yet

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

# 53. Quality Gates for Every Detector

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

# 54. Coding Agent Instructions

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
16. **Do not equate detection with profitability.**
17. **Do not equate realized MEV with historical opportunity.**
18. **Do not use gross token output as net profit.**
19. **Do not run expensive simulation before cheap candidate filtering.**
20. **Do not classify front-run/backrun/sandwich solely from transaction ordering.**
21. **Use counterfactual simulation whenever economic causality matters.**
22. **Keep all cost categories explicit.**
23. **Preserve native/token-denominated accounting before USD conversion.**
24. **Use deterministic integer/fixed-point arithmetic.**
25. **Treat REVM execution as the final truth for candidate feasibility.**
26. **Make all thresholds configurable.**
27. **Every detector must have positive and false-positive regression tests.**
28. **Never silently treat an unsimulated candidate as profitable.**
29. **Never silently treat a theoretical opportunity as guaranteed executable profit.**

---

# 55. Definition of Done

## 55.1 First Useful System Release

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

## 55.2 Opportunity Engine Research Completeness

The opportunity engine is considered complete for the initial research phase when it can:

```text
[✓] Detect historical arbitrage candidates
[✓] Detect historical liquidation candidates
[✓] Detect historical sandwich candidates
[✓] Detect historical backrun candidates
[✓] Detect historical JIT candidates
[✓] Generate hypothetical arbitrage opportunities from historical state
[✓] Optimize trade size
[✓] Simulate candidates deterministically with REVM
[✓] Verify success/revert
[✓] Calculate exact asset flows
[✓] Calculate gas cost
[✓] Calculate DEX/protocol fees
[✓] Calculate flashloan fee
[✓] Account for builder/validator payments where applicable
[✓] Calculate gross and net P&L
[✓] Support counterfactual simulation
[✓] Clearly separate realized vs hypothetical vs live opportunities
[✓] Return structured OpportunityResult
[✓] Explain why candidates were rejected
```

The engine should then be ready to plug into the broader backtesting system without changing the detector/accounting interfaces.

---

# 56. Reference Sources

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

# 57. Final Architectural Principle

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

Observed, hypothetical, and live modes must also remain distinct.

That separation is the foundation for a reliable MEV research/backtesting system and prevents an explorer-style detector from being mistaken for a profitable live strategy engine.
