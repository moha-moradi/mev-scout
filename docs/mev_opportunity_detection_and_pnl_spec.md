# MEV Opportunity Detection & Profit/Cost Engine

## Purpose

This document is an implementation-focused specification for a Coding Agent (OpenCode, Cursor, Claude Code, Codex, etc.) to build the **MEV opportunity detection, simulation, optimization, and profit/cost accounting layer** of a professional MEV research/backtesting system.

This specification intentionally excludes:

- node/RPC infrastructure
- mempool transport implementation
- block ingestion pipelines
- databases/storage architecture
- deployment/execution infrastructure
- smart-contract executor implementation
- UI/dashboard implementation

The scope is strictly:

1. Detect historical and hypothetical MEV opportunities.
2. Detect live/pending opportunities when pending transaction data is available.
3. Build candidate strategies.
4. Simulate candidates with deterministic EVM execution (REVM).
5. Optimize trade size / route / strategy parameters.
6. Calculate gross revenue, every material cost, and net profit.
7. Reject false or unprofitable opportunities.
8. Produce a normalized `OpportunityResult` that can be consumed by a backtester or future execution engine.

---

# 1. Core Design Principle

The system MUST distinguish between:

### A. Observed MEV

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

---

# 2. Supported MEV Strategy Types

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

# 3. Normalized Inputs

All detectors should operate on normalized primitives instead of raw protocol-specific objects.

## 3.1 Swap

```rust
struct Swap {
    tx_hash: TxHash,
    block_number: u64,
    tx_position: u32,
    trace_address: Vec<u32>,

    trader: Address,
    pool: Address,
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

    gas_used: Option<u64>,
}
```

The same logical swap MUST look identical whether it came from:

- Uniswap V2
- Uniswap V3
- SushiSwap
- Trader Joe
- Pangolin
- QuickSwap
- other V2/V3 forks
- supported aggregators

---

## 3.2 Token Transfer

```rust
struct TokenTransfer {
    tx_hash: TxHash,
    trace_address: Vec<u32>,
    token: Address,
    from: Address,
    to: Address,
    amount: U256,
}
```

---

## 3.3 State Snapshot

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

The exact storage implementation is outside this specification.

---

## 3.4 Transaction Context

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

# 4. Detector Pipeline

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

IMPORTANT:

Do NOT run full REVM simulation on every transaction or every theoretical route.

Use a cheap pre-filter first.

---

# 5. Opportunity Candidate Model

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

---

# 6. Route Representation

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

# 7. Arbitrage Detection

## 7.1 Historical Executed Arbitrage

Detect routes where an actor starts with asset X and returns to X with a larger amount.

Basic condition:

```text
final_amount_X > initial_amount_X
```

But the detector MUST also verify that the flow is attributable to the same strategy actor and route.

Do not classify every profitable multi-swap transaction as arbitrage without checking flow ownership.

## 7.2 Historical Hypothetical Arbitrage

Build a graph of tradable pairs:

```text
Token A → Token B
Token B → Token C
Token C → Token A
```

Find cycles and candidate routes.

For each route:

1. Estimate a cheap maximum-feasible trade size.
2. Simulate the route.
3. Optimize the input amount.
4. Calculate complete costs.
5. Keep only positive-net-profit candidates.

## 7.3 Arbitrage Search Algorithm

Start simple:

```text
For each token:
    discover 2-hop cycles
    discover 3-hop cycles
```

Then extend to:

```text
N-hop cycles
split routes
multi-path optimization
```

Do not brute-force every possible graph path.

Use pruning based on:

- liquidity
- pool fee
- token compatibility
- preliminary price edge
- maximum route length
- estimated price impact

---

# 8. Arbitrage Candidate Math

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

For exact profitability this is NOT sufficient.

Use:

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

# 9. Trade Size Optimization

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

Possible implementation order:

### Phase 1
Coarse grid search:

```text
x
0.25x
0.5x
1x
2x
4x
...
```

### Phase 2
Refinement around the best candidate.

### Phase 3
Optional numerical optimizer.

Every final candidate MUST be re-simulated at the selected optimal amount.

---

# 10. Liquidation Detection

Liquidation detection has two modes.

## 10.1 Historical Realized Liquidation

Detect liquidation protocol calls.

Protocol-specific examples may include:

```text
Aave liquidationCall
Compound liquidateBorrow
other lending liquidation entrypoints
```

The detector should not rely on function name alone.

Validate using:

- target contract
- decoded selector/function
- resulting transfers
- debt repayment
- collateral received

Historical liquidation identification should follow the same conceptual model used by MEV Inspect: classify liquidation calls, inspect child traces, then attribute collateral/debt token transfers to the liquidation. fileciteturn1file0

## 10.2 Liquidation Opportunity

Monitor positions and determine whether liquidation is possible under the current state.

Required calculations may include:

```text
collateral_value
borrowed_value
LTV
health_factor
liquidation_threshold
max_close_factor
liquidation_bonus
```

Then estimate:

```text
GrossLiquidationRevenue
    = value_of_collateral_received
    - value_of_debt_repaid
```

Then simulate the actual protocol call and use exact token amounts.

---

# 11. Liquidation P&L

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

# 12. Sandwich Detection

## 12.1 Historical Sandwich

A historical sandwich should be detected using transaction ordering plus swap structure.

Canonical pattern:

```text
Tx A: Searcher front leg
Tx B: Victim
Tx C: Searcher back leg
```

Required signals should include:

1. Front transaction occurs before victim.
2. Back transaction occurs after victim.
3. Same searcher controls front/back legs or strong attribution evidence exists.
4. Front and back interact with the same relevant pool(s).
5. Victim trades the affected pair in the expected direction.
6. Back leg reverses the searcher's position.
7. The sequence is profitable after costs.

Flashbots' historical sandwich detector is a useful reference: it orders swaps by transaction position and trace address, then looks for same-pool same-direction victim swaps followed by the searcher's reverse swap. fileciteturn0file0

That implementation should be treated as a conceptual baseline, not as a complete production-grade detector.

## 12.2 Sandwich False Positive Protection

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

---

# 13. Live Sandwich Opportunity

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

Candidate should be rejected if:

```text
NetProfit <= min_profit_threshold
```

or if the victim transaction would revert under the simulated sequence.

---

# 14. Backrun Detection

## 14.1 Historical Backrun

A backrun candidate has:

```text
Tx A = market-moving transaction
Tx B = subsequent transaction
```

The key requirement is economic causality, not merely adjacency.

Compare:

```text
profit(B | state_after_A)
vs
profit(B | state_before_A)
```

If the opportunity exists primarily because of A, B is a strong backrun candidate.

## 14.2 Live Backrun

For a pending state-changing transaction:

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

This is one of the most important live opportunity pipelines.

---

# 15. Front-run Detection

## 15.1 Historical

A transaction before a victim is NOT automatically a front-run.

Require multiple signals:

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

## 15.2 Live

For a pending victim:

```text
simulate victim
simulate candidate front-run → victim
compare victim execution
simulate complete strategy if back leg exists
```

The detector should produce both:

```text
Candidate confidence
Economic profitability
```

Do NOT use a heuristic confidence score as a replacement for REVM profitability.

---

# 16. JIT Liquidity Detection

Focus initially on Uniswap V3-style concentrated liquidity.

Canonical historical pattern:

```text
Mint liquidity
      ↓
Large/targeted swap
      ↓
Burn liquidity
```

Signals:

- same LP/provider
- same pool
- mint and burn close in time / block sequence
- liquidity overlaps the active price range
- meaningful fees earned by the minted liquidity
- short liquidity lifetime

The system should calculate:

```text
LPFeesEarned
- MintGas
- Swap-related gas attribution
- BurnGas
= JITGrossOrNetContribution
```

The detector must distinguish normal LP activity from JIT activity.

---

# 17. JIT + Arbitrage

Composite pattern:

```text
Mint liquidity
      ↓
Swap / price-moving order
      ↓
Arbitrage or fee capture
      ↓
Burn liquidity
```

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

# 18. REVM Simulation Layer

REVM is the source of truth for exact execution results.

Detector heuristics create candidates.

REVM determines whether the candidate actually works.

Pipeline:

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

Every simulated strategy should produce a `SimulationResult`:

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

---

# 19. Exact P&L Accounting

This is a critical component.

Do NOT calculate profit merely from gross token output.

All material costs must be represented explicitly.

## 19.1 Revenue

Possible revenue sources:

```text
Arbitrage spread
Liquidation bonus
JIT LP fees
Backrun profit
Sandwich spread
Other captured price discrepancy
```

## 19.2 Costs

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

---

# 20. Gas Cost

For historical execution:

```text
GasCostNative
    = gas_used × effective_gas_price
```

Convert to accounting currency using the configured historical price source.

For EIP-1559:

```text
EffectiveGasPrice
```

must be taken from the actual transaction receipt/context where available.

For hypothetical execution:

support configurable assumptions for:

```text
gas_used
base_fee
priority_fee
```

Do not silently assume current gas prices for historical backtests.

---

# 21. Flash Loan Accounting

Support at least:

```text
Balancer
Aave
```

Generic model:

```text
FlashLoanFee
    = BorrowAmount × FeeRate
```

The engine MUST verify repayment in the simulation.

A candidate is invalid if:

```text
repayment_amount > available_final_balance
```

or the flash-loan callback transaction reverts.

---

# 22. DEX Fee Accounting

Every pool type must expose or infer its effective fee.

Examples:

```text
Uniswap V2: percentage fee
Uniswap V3: pool fee tier
Curve: pool-specific fee
Balancer: pool-specific swap fee
```

But do not simply subtract a theoretical fee from the result when REVM can provide the exact outcome.

Prefer:

```text
exact simulation result
```

over:

```text
generic fee formula
```

Generic fee formulas should mainly be used for candidate pre-filtering.

---

# 23. Slippage / Price Impact

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

---

# 24. Builder / Validator Payment

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

---

# 25. Profit in Native and USD Terms

Every result should preserve native/token-denominated P&L first.

Do NOT immediately convert everything to USD and discard the original units.

Example:

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

USD conversion is an accounting layer, not the source of truth.

---

# 26. Cost Item Model

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

---

# 27. Opportunity Result

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

---

# 28. Profitability Rule

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

---

# 29. Rejection Reasons

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

# 30. Counterfactual Simulation

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

# 31. Historical Opportunity Detection Workflow

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

---

# 32. Live Opportunity Detection Workflow

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

# 33. Opportunity Ranking

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

# 34. Multiple Candidate Routes

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

# 35. Flashloan vs Own-Capital Mode

The simulation/accounting engine should support both:

```text
capital_mode = OwnCapital
capital_mode = FlashLoan
```

For your primary research use case:

```text
FlashLoan
```

is preferred for capital-free feasibility studies.

But capital requirements should still be reported:

```text
required_capital
flash_loan_amount
```

---

# 36. No-Competition Backtest Mode

Initial historical backtesting should intentionally model:

```text
No other searchers
No mempool competition
No auction competition
No probabilistic inclusion model
```

The objective is:

> What theoretical net P&L was available from the historical state?

This is NOT the same as executable live profit.

The result should be labeled:

```text
THEORETICAL_HISTORICAL_OPPORTUNITY
```

Later, a competition model can be added.

---

# 37. Competition-Aware Mode (Future)

Not required for the initial detector, but design interfaces so later this can model:

```text
other searchers
bundle competition
priority fees
builder payments
failed inclusion
ordering uncertainty
```

Do not bake assumptions about competition into the base detector.

---

# 38. MEV Opportunity vs Realized MEV

Every result must clearly identify:

```text
mode:
    realized
    historical_hypothetical
    live_pending
```

Example:

```rust
enum OpportunityMode {
    Realized,
    HistoricalHypothetical,
    LivePending,
}
```

This prevents accidentally treating backtest opportunities as guaranteed realized revenue.

---

# 39. False Positive Requirements

Every detector must contain explicit false-positive tests.

## Arbitrage

Reject:

- profitable-looking token transfers with no actual trading cycle
- unrelated transfers
- accounting artifacts

## Liquidation

Reject:

- unrelated collateral transfers
- failed/reverted liquidation calls
- nested protocol calls misclassified as independent liquidations

## Sandwich

Reject:

- coincidental same-pool trades
- independent arbitrage before/after a victim
- same sender but unrelated transactions

## Backrun

Reject:

- ordinary next-block/next-tx arbitrage not causally related to trigger

## Front-run

Reject:

- transactions merely appearing before a victim

## JIT

Reject:

- ordinary LP mint/burn activity with long holding periods
- liquidity outside the relevant active range

---

# 40. Unit Test Strategy

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

Also include exact regression tests built from real historical transactions.

---

# 41. Golden Test Format

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

# 42. Precision Requirements

Use integer/fixed-point arithmetic wherever possible.

Do not use floating-point arithmetic for token accounting.

Recommended:

```text
U256 / U512
fixed-point decimal only at reporting boundaries
```

Profit calculations MUST be deterministic.

---

# 43. Determinism

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

# 44. Performance Principles

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

```text
all routes × all amounts × full simulation
```

without pre-filtering.

Use caching for repeated state/route calculations where safe.

---

# 45. Recommended Detector Interface

Use a common interface such as:

```rust
trait OpportunityDetector {
    fn detect(
        &self,
        context: &DetectionContext,
    ) -> Vec<OpportunityCandidate>;
}
```

Then a separate simulator:

```rust
trait OpportunitySimulator {
    fn simulate(
        &self,
        candidate: &OpportunityCandidate,
        context: &SimulationContext,
    ) -> SimulationResult;
}
```

And accounting:

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

---

# 46. Recommended Internal Pipeline API

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

# 47. Minimum Viable Detector Order

Implement in this order:

## Phase 1

1. Historical arbitrage
2. Historical liquidation
3. Historical sandwich
4. Historical backrun
5. Historical JIT

## Phase 2

6. Historical front-run attribution
7. JIT + arbitrage
8. Arbitrage route/amount optimization improvements

## Phase 3

9. Live pending backrun
10. Live sandwich
11. Live front-run
12. Live liquidation
13. Live arbitrage

This ordering maximizes reuse because historical classification supplies the primitives required for live opportunity detection.

---

# 48. Required Final Output Example

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

# 49. Important Engineering Rules for the Coding Agent

1. **Do not equate detection with profitability.**
2. **Do not equate realized MEV with historical opportunity.**
3. **Do not use gross token output as net profit.**
4. **Do not run expensive simulation before cheap candidate filtering.**
5. **Do not classify front-run/backrun/sandwich solely from transaction ordering.**
6. **Use counterfactual simulation whenever economic causality matters.**
7. **Keep all cost categories explicit.**
8. **Preserve native/token-denominated accounting before USD conversion.**
9. **Use deterministic integer/fixed-point arithmetic.**
10. **Treat REVM execution as the final truth for candidate feasibility.**
11. **Make all thresholds configurable.**
12. **Every detector must have positive and false-positive regression tests.**
13. **Never silently treat an unsimulated candidate as profitable.**
14. **Never silently treat a theoretical opportunity as guaranteed executable profit.**

---

# 50. Definition of Done

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
