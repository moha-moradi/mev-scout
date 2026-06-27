# MEV Strategies: Complete Reference

> **32 strategies across 8 categories** — mechanics, edge, capital requirements, competition level, and implementation notes for each.  
> Profitability and competition scored 1–10. Capital: None / Low / Medium / High.

---

## Table of Contents

1. [V2 Pool Mechanics](#1-v2-pool-mechanics)
   - [skim() capture](#11-skim-capture)
   - [sync() race](#12-sync-race)
   - [Flash swap arbitrage](#13-flash-swap-arbitrage)
   - [Init price snipe](#14-init-price-snipe)
2. [Order Flow](#2-order-flow)
   - [Backrunning](#21-backrunning)
   - [Long-tail token arbitrage](#22-long-tail-token-arbitrage)
   - [CEX–DEX arbitrage](#23-cexdex-arbitrage)
   - [Statistical arbitrage / pairs](#24-statistical-arbitrage--pairs)
3. [Bundle / Positional](#3-bundle--positional)
   - [Sandwich attack](#31-sandwich-attack)
   - [JIT liquidity](#32-jit-liquidity)
   - [JIT + arb combo](#33-jit--arb-combo)
   - [V3 range order snipe](#34-v3-range-order-snipe)
4. [Liquidations](#4-liquidations)
   - [Cascading liquidation engineering](#41-cascading-liquidation-engineering)
   - [MakerDAO OSM preview + kick()](#42-makerdao-osm-preview--kick)
   - [Oracle-latency liquidation](#43-oracle-latency-liquidation)
   - [Flash loan atomic liquidation](#44-flash-loan-atomic-liquidation)
   - [LST depeg collateral liquidation](#45-lst-depeg-collateral-liquidation)
   - [AAVE partial liquidation optimizer](#46-aave-partial-liquidation-optimizer)
   - [Synthetix flag + delayed liquidation](#47-synthetix-flag--delayed-liquidation)
   - [Liquity recovery mode cascade](#48-liquity-recovery-mode-cascade)
   - [Liquity stability pool front-run](#49-liquity-stability-pool-front-run)
   - [MakerDAO Clip Dutch auction take()](#410-makerdao-clip-dutch-auction-take)
   - [GMX v1 keeper race](#411-gmx-v1-keeper-race)
   - [Perp protocol keeper (dYdX / Kwenta)](#412-perp-protocol-keeper-dydx--kwenta)
   - [Interest accrual liquidation](#413-interest-accrual-liquidation)
   - [NFT collateral liquidation](#414-nft-collateral-liquidation)
   - [Bad debt prevention optimizer](#415-bad-debt-prevention-optimizer)
5. [Oracle / Rebase / Peg](#5-oracle--rebase--peg)
   - [Stablecoin depeg arbitrage](#51-stablecoin-depeg-arbitrage)
   - [TWAP oracle manipulation](#52-twap-oracle-manipulation)
   - [Rebase token arbitrage](#53-rebase-token-arbitrage)
   - [Fee-on-transfer token arbitrage](#54-fee-on-transfer-token-arbitrage)
6. [Cross-Domain](#6-cross-domain)
   - [Bridge MEV](#61-bridge-mev)
   - [L2 sequencer MEV](#62-l2-sequencer-mev)
   - [PBS / MEV-Boost](#63-pbs--mev-boost)
   - [Cross-chain arbitrage](#64-cross-chain-arbitrage)
7. [Protocol Niches](#7-protocol-niches)
   - [Curve pool imbalance](#71-curve-pool-imbalance)
   - [Governance MEV](#72-governance-mev)
   - [Airdrop MEV](#73-airdrop-mev)
   - [ERC-4337 AA bundler MEV](#74-erc-4337-aa-bundler-mev)
8. [Emerging](#8-emerging)
   - [Solver / intent MEV](#81-solver--intent-mev)
   - [Batch auction MEV](#82-batch-auction-mev)
   - [NFT floor arbitrage](#83-nft-floor-arbitrage)
   - [Token launch snipe](#84-token-launch-snipe)
9. [Master Rankings Table](#9-master-rankings-table)
10. [Cross-Cutting Infrastructure](#10-cross-cutting-infrastructure)

---

## 1. V2 Pool Mechanics

Uniswap V2's design stores accounting state (`reserve0`, `reserve1`) separately from actual token balances. This gap between stored reserves and real balances is the source of an entire class of low-competition, latency-gated MEV opportunities.

---

### 1.1 skim() capture

| Attribute | Value |
|-----------|-------|
| Profitability | 6/10 |
| Competition | 4/10 |
| Capital required | None |
| Complexity | 3/10 |
| Chains | All EVM with Uniswap V2 forks |
| Frequency | Continuous (event-triggered) |

**Mechanism**

Uniswap V2 pair contracts store `reserve0` and `reserve1` via internal accounting, updated only during swaps, mints, and burns. The actual token balance held by the contract (`IERC20(token).balanceOf(address(pair))`) can exceed these reserves when:

- A rebase token (e.g. AMPL, stETH) accrues balance to all holders including the pair contract
- A fee-accruing token increases holder balances passively
- Someone accidentally `transfer()`s tokens directly to the pair address (no router call)
- A buggy contract sends tokens without initiating a swap

When `balanceOf(pair, token) > reserve`, any caller can invoke `skim(to)`, which sends `actual_balance - reserve` directly to `to`. The function signature:

```solidity
function skim(address to) external lock {
    address _token0 = token0;
    address _token1 = token1;
    _safeTransfer(_token0, to, IERC20(_token0).balanceOf(address(this)).sub(reserve0));
    _safeTransfer(_token1, to, IERC20(_token1).balanceOf(address(this)).sub(reserve1));
}
```

**Detection in Rust (alloy-rs)**

```rust
// For each pair, compare balance vs reserve
let (reserve0, reserve1, _) = pair.getReserves().call().await?;
let balance0 = token0.balanceOf(pair_address).call().await?;
let balance1 = token1.balanceOf(pair_address).call().await?;

if balance0 > reserve0 || balance1 > reserve1 {
    pair.skim(executor_address).send().await?;
}
```

**Edge and moat**

- Winner-takes-all on each pair — first `skim()` caller captures the entire excess
- Monitoring all pairs requires a factory-indexed pair registry with fresh balance reads
- Rebase events (e.g. AMPL rebase happens daily) are predictable in timing — pre-stage transactions

---

### 1.2 sync() race

| Attribute | Value |
|-----------|-------|
| Profitability | 3/10 |
| Competition | 3/10 |
| Capital required | None |
| Complexity | 2/10 |
| Chains | All EVM with Uniswap V2 forks |
| Frequency | Continuous |

**Mechanism**

`sync()` updates `reserve0` and `reserve1` to match the actual token balances — no tokens are transferred out. It corrects the reserves without extracting value:

```solidity
function sync() external lock {
    _update(IERC20(token0).balanceOf(address(this)),
            IERC20(token1).balanceOf(address(this)),
            reserve0, reserve1);
}
```

**When a bot calls sync()**

1. **Defensive burn**: A competitor is about to `skim()` excess tokens. Calling `sync()` first destroys the opportunity for everyone, including the attacker. Used to protect a pool you have an economic interest in (e.g. you are an LP).
2. **Post-rebase-down correction**: If a rebase token decreases balances (rebase down), `balance < reserve`. This makes the pool's implied price stale — the next trade executes at the wrong price. Calling `sync()` corrects this and may open an arbitrage opportunity in the subsequent block.
3. **Precondition for arb**: Some arbitrage paths require reserves to be in sync before the arb swap is profitable.

---

### 1.3 Flash swap arbitrage

| Attribute | Value |
|-----------|-------|
| Profitability | 7/10 |
| Competition | 7/10 |
| Capital required | None |
| Complexity | 5/10 |
| Chains | All EVM with Uniswap V2 forks |
| Frequency | Continuous |

**Mechanism**

Uniswap V2 `swap()` supports an optional callback via the `data` parameter. When `data.length > 0`, the pair calls `uniswapV2Call(sender, amount0Out, amount1Out, data)` on the recipient before verifying that the repayment invariant holds. This enables zero-capital atomic arbitrage:

```
1. Call pair.swap(amount0Out, 0, executor_contract, calldata)
2. Pair sends amount0Out tokens to executor_contract
3. Pair calls executor_contract.uniswapV2Call(...)
4. Inside callback: swap borrowed tokens on another DEX for profit
5. Repay pair: send back amount0In such that reserve0 * reserve1 >= k (with 0.3% fee)
6. If profit > fee + gas: transaction succeeds; else: reverts atomically
```

**Key formula for repayment**

```
amountIn = (reserveIn * amountOut * 1000) / ((reserveOut - amountOut) * 997) + 1
```

**Edge**

- Genuinely capital-free: the borrow and repayment happen within a single transaction
- Reversion is atomic — failed arbs cost only gas
- Most competitive on high-volume pairs (ETH/USDC etc.); less contested on long-tail pairs

---

### 1.4 Init price snipe

| Attribute | Value |
|-----------|-------|
| Profitability | 7/10 |
| Competition | 5/10 |
| Capital required | Low |
| Complexity | 4/10 |
| Chains | All EVM with Uniswap V3 |
| Frequency | On every new pool deployment |

**Mechanism**

Uniswap V3 pools require an explicit `initialize(uint160 sqrtPriceX96)` call before any liquidity can be added. If the deployer sets a price that does not match the current market price of the pair, an immediate risk-free arbitrage exists.

**Attack surface**

- Deployer sets stale price (e.g. copied from a test environment)
- Deployer miscalculates `sqrtPriceX96` encoding
- Deployer uses a token with a non-standard decimal configuration

**Detection**

Monitor `PoolCreated` events from the V3 Factory. On each new pool, read `slot0.sqrtPriceX96` immediately post-initialization, compute implied price, compare against Chainlink oracle or CEX feed. If deviation exceeds gas cost + swap fee, fire the arb.

```rust
// Listen to PoolCreated factory events
factory.event::<PoolCreated>()
    .subscribe()
    .await?
    .for_each(|event| {
        tokio::spawn(check_init_price(event.pool));
    });
```

---

## 2. Order Flow

Strategies that operate at the transaction-ordering layer — capturing value from the sequence in which transactions land in a block.

---

### 2.1 Backrunning

| Attribute | Value |
|-----------|-------|
| Profitability | 7/10 |
| Competition | 6/10 |
| Capital required | Low |
| Complexity | 4/10 |
| Chains | ETH, BSC, Polygon, Base, Arbitrum |
| Frequency | Continuous |

**Mechanism**

A backrun places a transaction *immediately after* a target transaction to capture a favorable state change the target creates. Unlike sandwich attacks, backrunning does not front-run the victim — it only adds a transaction after. This makes it ethically neutral and compatible with MEV-Share, where victims receive a rebate.

**Common backrun patterns**

- A large buy pushes price up on pool A → backrun with a sell on pool A, buy on pool B (closing the arb)
- A large liquidity removal changes the pool's price → backrun with a corrective arb
- A `sync()` call changes effective reserves → immediately arb the corrected price

**MEV-Share integration**

Flashbots MEV-Share allows searchers to backrun transactions with a configurable portion of profits returned to the original sender. This aligns incentives: users share order flow in exchange for rebates; searchers get exclusive backrun rights.

**Edge**

- Low ethical risk (no victim harm)
- Compatible with private order flow markets
- Moat: speed to detect the state change + accuracy of revm simulation to predict profit before committing gas

---

### 2.2 Long-tail token arbitrage

| Attribute | Value |
|-----------|-------|
| Profitability | 6/10 |
| Competition | 3/10 |
| Capital required | Low |
| Complexity | 5/10 |
| Chains | BSC, ETH, Polygon, Base, Avalanche |
| Frequency | Continuous |

**Mechanism**

Mainstream arbitrage (ETH/USDC, major pairs) is fully saturated by co-located bots with sub-millisecond latency advantages. Long-tail arbitrage targets newly deployed or obscure token pairs where:

- Fewer bots are monitoring
- Price discrepancies persist for seconds to minutes rather than milliseconds
- Routing through multiple intermediate hops is required

**Graph traversal approach**

Maintain a directed weighted graph of all known pools. Edges represent swap paths with weight = implied exchange rate. Run SPFA (Shortest Path Faster Algorithm) or Bellman-Ford to detect negative cycles (profitable arbitrage loops).

```rust
// Pseudocode for negative cycle detection
// Pool state in redb: pool_address -> (reserve0, reserve1, fee)
// Nodes: tokens; Edges: pools with weight = log(price)
// Negative cycle in log-weight graph = arbitrage opportunity
fn find_arb_cycle(graph: &PoolGraph) -> Option<ArbPath> {
    bellman_ford_negative_cycle(graph)
}
```

**Edge**

- Discovery speed: indexing new pool deployments faster than competitors
- Long-tail pools have thin liquidity → arb margin is larger but size is limited
- Moat: comprehensive pool registry + SPFA across 10,000+ pools in real time

---

### 2.3 CEX–DEX arbitrage

| Attribute | Value |
|-----------|-------|
| Profitability | 9/10 |
| Competition | 9/10 |
| Capital required | High |
| Complexity | 8/10 |
| Chains | ETH L1 primarily; Arbitrum |
| Frequency | Continuous |

**Mechanism**

CEX prices lead DEX prices. When a large market order hits Binance or Coinbase, the on-chain AMM price has not yet updated. The gap between CEX mid-price and DEX spot price represents an arbitrage profit for whoever closes it first.

**Architecture requirements**

- Co-location or ultra-low latency connection to CEX WebSocket orderbook feeds
- Private RPC or mempool feed (bloXroute, Fiber, 48Club) to land transactions early in the block
- Capital pre-positioned in both CEX and DEX environments simultaneously

**Niche angle**

The top-tier CEX-DEX arb (ETH/USDC on Binance vs Uniswap V3) is dominated by Jump, Wintermute, and similar. The niche opportunity is in secondary CEXs (Bybit, OKX) and secondary pairs where these firms have less presence. DEX response to CEX price updates on these pairs may be 200–500ms slower.

**Moat**

Infrastructure: latency to CEX feed + latency to block builder. Not strategy alpha — this is a pure speed competition at the top tier.

---

### 2.4 Statistical arbitrage / pairs

| Attribute | Value |
|-----------|-------|
| Profitability | 6/10 |
| Competition | 4/10 |
| Capital required | Medium |
| Complexity | 7/10 |
| Chains | ETH, Arbitrum, Base |
| Frequency | Intraday |

**Mechanism**

Statistical arb exploits mean-reversion in the price ratio of correlated assets. In DeFi, natural pairs include:

- `stETH / wstETH` — should maintain a fixed conversion ratio
- `WBTC / cbBTC / renBTC` — all pegged to BTC, diverge on liquidity events
- `DAI / USDC / USDT` — stablecoin trio with cross-AMM price divergences
- `ETH / rETH` — LST/ETH ratio drift

**Strategy**

Monitor the spread between two correlated assets across multiple DEXs. When the spread exceeds a threshold (accounting for fees and gas), trade the spread expecting reversion. Unlike pure arb, stat arb carries directional risk if the spread widens before reverting.

**Edge**

- Lower competition than pure arb: requires maintaining running statistics on price ratios
- Works on slower timeframes (seconds to minutes) — latency matters less
- Moat: accurate correlation modeling + multi-DEX aggregation

---

## 3. Bundle / Positional

Strategies that require constructing and submitting multi-transaction bundles or rely on precise transaction positioning within a block.

---

### 3.1 Sandwich attack

| Attribute | Value |
|-----------|-------|
| Profitability | 7/10 |
| Competition | 8/10 |
| Capital required | Medium |
| Complexity | 5/10 |
| Chains | ETH, BSC, Polygon (not Arbitrum FCFS) |
| Frequency | Continuous |

**Mechanism**

A sandwich attack wraps a victim's large swap with two transactions:

```
Block N:
  [frontrun tx]  — buy token before victim's price impact
  [victim tx]    — victim executes at worse price
  [backrun tx]   — sell token at elevated price created by victim
```

**Profitability condition**

```
profit = (price_after_victim - price_before_frontrun) × frontrun_amount
         - gas_cost_frontrun - gas_cost_backrun - swap_fees
```

**Bundle submission**

Submitted to Flashbots (ETH), BloXroute BSC relay (BSC), or sequencer-specific private channels. The frontrun must land in the same block as the victim.

**Chain constraints**

- Arbitrum uses FCFS ordering — no mempool reordering possible, sandwich structurally impossible
- Base sequencer ordering allows sandwiching
- BSC validators can reorder freely; 48Club provides privileged ordering access

**Ethical note**

Sandwich attacks directly harm users by increasing their slippage. This is the most contested ethical territory in MEV. MEV-Share and SUAVE are designed specifically to reduce sandwich attack profitability.

---

### 3.2 JIT liquidity

| Attribute | Value |
|-----------|-------|
| Profitability | 8/10 |
| Competition | 5/10 |
| Capital required | High |
| Complexity | 7/10 |
| Chains | ETH, Arbitrum, Base (Uniswap V3) |
| Frequency | On large swaps |

**Mechanism**

Just-in-time liquidity adds concentrated liquidity to a V3 pool in the same block as a large swap, captures the majority of the swap fees, then removes the liquidity immediately after. The atomic bundle:

```
Bundle:
  tx1: mint concentrated position (tight range around current tick)
  tx2: victim's large swap (fees accrue to our position)
  tx3: burn our position (collect fees + principal)
```

**Profitability condition**

```
fee_captured = swap_amount × fee_tier × (our_liquidity / total_liquidity_in_range)
profit = fee_captured - gas_cost_3_txs - impermanent_loss_during_swap
```

The swap itself moves the price, creating IL on the JIT position. Profitability requires the fee tier to be high enough relative to the price impact.

**Tick range selection**

Tight ranges (current tick ± 1) maximize fee concentration but also maximize IL. Optimal range is a function of predicted swap size and fee tier:

```
optimal_range = argmax(fee_capture_fraction × tick_width - IL(price_impact, tick_width))
```

**Moat**

Capital requirements are high. The edge is in tick range optimization — bots using naive ±1 tick ranges are easily outcompeted on profitability.

---

### 3.3 JIT + arb combo

| Attribute | Value |
|-----------|-------|
| Profitability | 9/10 |
| Competition | 3/10 |
| Capital required | High |
| Complexity | 9/10 |
| Chains | ETH, Arbitrum |
| Frequency | On large swaps with cross-pool price divergence |

**Mechanism**

Extends JIT by also capturing the post-swap price dislocation via an immediate arbitrage. The large swap in tx2 moves the V3 pool's price away from the market. After collecting fees in tx3, tx4 closes the price gap against another pool.

```
Bundle:
  tx1: mint JIT position
  tx2: victim's swap (moves pool price + generates fees)
  tx3: burn JIT position (capture fees)
  tx4: arb tx — swap in the opposite direction on the now-mispriced pool
```

The arb in tx4 is guaranteed to be profitable because tx2 created a known price dislocation.

**Edge**

Requires modeling both the fee capture and the arb profit simultaneously to determine if the 4-tx bundle cost is justified. Most JIT bots stop at tx3 — adding tx4 is the additional complexity that creates the moat.

---

### 3.4 V3 range order snipe

| Attribute | Value |
|-----------|-------|
| Profitability | 6/10 |
| Competition | 4/10 |
| Capital required | Low |
| Complexity | 5/10 |
| Chains | ETH, Arbitrum, Base |
| Frequency | Continuous (price-crossing triggered) |

**Mechanism**

Uniswap V3 range orders are single-sided liquidity positions that convert entirely to the other token when price crosses their tick range. This functions like a limit order. When a large price movement is anticipated:

- Detect large pending swaps that will cross a tick boundary
- Front-run by placing a range order just inside the crossed range
- After the price crosses, the position is now entirely in the other token at a favorable average price
- Exit the position on a different pool (or wait for price reversion)

**Execution**

Tick crossings consume extra gas. Monitoring the tick bitmap for densely-packed positions near the current price allows bots to predict when the next crossing will occur and pre-position accordingly.

---

## 4. Liquidations

The liquidation category has the most strategic depth. The space spans from fully commoditized (basic AAVE health factor monitoring) to highly specialized (cascading cross-protocol liquidation engineering). Strategies are ordered by profitability.

---

### 4.1 Cascading liquidation engineering

| Attribute | Value |
|-----------|-------|
| Profitability | 10/10 |
| Competition | 2/10 |
| Capital required | High (flash loan viable) |
| Complexity | 10/10 |
| Chains | ETH L1 |
| Frequency | Rare / event-driven |
| Latency type | Simulation-gated (revm) |

**Mechanism**

A large liquidation's price impact on the seized collateral asset can trigger secondary liquidations across other protocols. The entire cascade can be constructed as an atomic bundle, capturing the sum of all liquidation bonuses across all affected protocols.

**Example cascade (simplified)**

```
1. Identify: $50M ETH-collateral WBTC-debt position on AAVE
   → Liquidating seizes $27.5M ETH, sold to WBTC
   → ETH price impact on AMM: ~0.4%

2. Model: which other positions become underwater after 0.4% ETH drop?
   → 3 Compound positions cross their liquidation threshold
   → 1 Euler position crosses threshold
   → Combined bonus: additional $800K

3. Construct atomic bundle:
   tx1: AAVE flash borrow WBTC
   tx2: AAVE liquidate (seize ETH)
   tx3: Swap ETH→WBTC (price impact occurs)
   tx4: Compound liquidate position A (newly eligible)
   tx5: Compound liquidate position B
   tx6: Euler liquidate position C
   tx7: Repay AAVE flash loan
   tx8: Profit
```

**Implementation requirements**

- Cross-protocol liquidation dependency graph (pre-computed for all open positions)
- revm simulation of price impact at each step
- Flash loan capital routing through Balancer, AAVE, or Uniswap V3
- Bundle submission via Flashbots (ETH) or equivalent

**Moat**

The simulation infrastructure is the moat. Profitability requires knowing, before submitting, that the cascade generates sufficient combined bonus to cover flash loan fees, gas for all transactions, and swap slippage at each step.

---

### 4.2 MakerDAO OSM preview + kick()

| Attribute | Value |
|-----------|-------|
| Profitability | 9/10 |
| Competition | 3/10 |
| Capital required | None |
| Complexity | 8/10 |
| Chains | ETH L1 |
| Frequency | Hourly (OSM update cadence) |
| Latency type | Storage-read + bundle timing |

**Mechanism**

MakerDAO's Oracle Security Module (OSM) delays price feed updates by exactly 1 hour. The next price is stored in contract storage at `_next` (slot 4 of the OSM contract) — readable by anyone — **one hour before it takes effect**.

**Storage slot reading**

```rust
// OSM contract stores:
// slot 3: current price (packed with has/src)
// slot 4: next price (uint128 next | uint16 has_next)
let next_slot = provider.get_storage_at(osm_address, U256::from(4), None).await?;
let next_price = u128::from_be_bytes(next_slot.as_bytes()[16..32].try_into()?);
```

**Attack pipeline**

```
T-60min: Read OSM._next for all collateral types
         → Compute: which vaults become unsafe at next_price?
         → Pre-sign kick() transactions for each eligible vault

T-0:     OSM poke() transaction appears in mempool
         → Bundle: [poke(), kick(vault_A), kick(vault_B), ...]
         → Submit to Flashbots, ensuring bundle lands in same block as poke()
```

**Kicker reward**

The `tip` + `chip × tab` reward for calling `kick()` goes entirely to `msg.sender`. No capital required — only gas.

**Moat**

Full vault health database pre-computed against the upcoming price. Most liquidation bots are reactive (health factor < 1 now) — OSM preview bots are proactive (health factor < 1 in exactly 1 hour).

---

### 4.3 Oracle-latency liquidation

| Attribute | Value |
|-----------|-------|
| Profitability | 9/10 |
| Competition | 5/10 |
| Capital required | Medium (flash loan viable) |
| Complexity | 7/10 |
| Chains | ETH, Arbitrum, Base, Polygon |
| Frequency | Intraday (deviation-trigger driven) |
| Latency type | CEX WebSocket + oracle mempool monitoring |

**Mechanism**

Chainlink oracles update on two triggers:
- **Heartbeat**: time-based (1 hour on ETH mainnet for most pairs)
- **Deviation**: price moves beyond threshold (typically 0.5%)

Between updates, the oracle is stale. During fast price movements, the true market price diverges from the oracle price. Positions that are underwater at the market price but not at the oracle price are in a "pre-liquidatable" state.

**Bot architecture**

```
1. Subscribe to Chainlink off-chain WebSocket feed (real-time CEX aggregate price)
2. Subscribe to chain mempool for oracle keeper transactions
3. For each block, for each monitored position:
   compute: health_factor(real_price) vs health_factor(oracle_price)
   if health_factor(real_price) < 1 and health_factor(oracle_price) >= 1:
       → position is pre-liquidatable
       → queue liquidation bundle: [oracle_update, liquidate(position)]

4. When oracle keeper tx appears in mempool:
   → co-bundle your liquidation immediately after
   → submit to Flashbots / private builder
```

**Multi-oracle cascade**

During fast market crashes, multiple oracle pairs update in sequence. Bots that track update ordering can liquidate positions whose oracle hasn't updated yet, using other assets as leading indicators.

---

### 4.4 Flash loan atomic liquidation

| Attribute | Value |
|-----------|-------|
| Profitability | 8/10 |
| Competition | 7/10 |
| Capital required | None |
| Complexity | 6/10 |
| Chains | ETH, Polygon, Arbitrum |
| Frequency | Daily |
| Latency type | Health factor monitoring |

**Mechanism**

Capital-free liquidation using flash loans to fund the repayment:

```
atomic transaction:
  1. Flash borrow repay_token from AAVE (fee: 0.05%) / Balancer (fee: 0%) / Uniswap V3
  2. Call liquidate(borrower, repay_amount, collateral_token, ...)
     → receive collateral_token at (1 + liquidation_bonus) discount
  3. Swap collateral_token → repay_token via best route
  4. Repay flash loan + fee
  5. Retain: swap_proceeds - repay_amount - flash_fee - gas
```

**Profitability threshold**

```
profitable if:
  liquidation_bonus_value > flash_loan_fee + swap_slippage + gas_cost
  i.e.: collateral_USD × liq_bonus% > repay_USD × flash_fee% + swap_impact + gas
```

**Flash loan source selection**

| Source | Fee | Note |
|--------|-----|------|
| Balancer | 0% | Best for large amounts |
| AAVE v3 | 0.05% | Most flexible collateral |
| Uniswap V3 | ~0.05–1% | Token-dependent |

**Edge**

Removes capital as a constraint. The competition is in swap routing — minimizing slippage on the collateral-to-repay-token swap is where profit leaks.

---

### 4.5 LST depeg collateral liquidation

| Attribute | Value |
|-----------|-------|
| Profitability | 9/10 |
| Competition | 4/10 |
| Capital required | Medium (flash loan viable) |
| Complexity | 7/10 |
| Chains | ETH L1 |
| Frequency | Rare / event-driven |
| Latency type | Curve pool real-time price + oracle mempool |

**Mechanism**

stETH, rETH, cbETH, and similar LSTs are widely used as collateral on AAVE v3, Morpho, and Spark. Positions using stETH as collateral to borrow ETH or USDC have their health factor computed via the stETH/USD Chainlink oracle.

During mass LST exit events (e.g. March 2023 USDC depeg causing Curve pool imbalance), the stETH/ETH Curve pool price diverges from the Chainlink oracle. When the divergence triggers an oracle deviation update, positions that were previously healthy immediately become liquidatable.

**Monitoring stack**

```rust
// Monitor stETH/ETH price on multiple DEXs
let curve_price = curve_pool.get_dy(1, 0, one_eth).call().await?;
let chainlink_price = chainlink_feed.latestAnswer().call().await?;

let divergence = (chainlink_price - curve_price).abs() / chainlink_price;
if divergence > ORACLE_DEVIATION_THRESHOLD {
    // Oracle update imminent — pre-queue liquidations
    trigger_liquidation_bundle().await?;
}
```

**Pre-positioned position database**

The entire edge is in having all stETH-collateralized positions indexed before the event. Scanning on-the-fly during an active depeg event is too slow — events move in one to three blocks.

---

### 4.6 AAVE partial liquidation optimizer

| Attribute | Value |
|-----------|-------|
| Profitability | 7/10 |
| Competition | 7/10 |
| Capital required | Medium |
| Complexity | 6/10 |
| Chains | ETH, Polygon, Arbitrum, Base |
| Frequency | Daily |

**Mechanism**

AAVE v2 enforces a 50% close factor per liquidation call. AAVE v3 uses a dynamic close factor: positions with health factor < 0.95 can be fully liquidated in one call.

**Collateral selection optimization**

For a position with multiple collateral assets, the liquidator chooses which collateral to seize:

```rust
fn select_optimal_collateral(
    collateral_assets: &[CollateralAsset],
    repay_amount_usd: f64,
) -> CollateralAsset {
    collateral_assets.iter()
        .filter(|c| c.value_usd >= repay_amount_usd * (1.0 + c.liquidation_bonus))
        .max_by(|a, b| {
            // Maximize: liquidation_bonus - expected_swap_slippage
            let score_a = a.liquidation_bonus - estimate_slippage(a, repay_amount_usd);
            let score_b = b.liquidation_bonus - estimate_slippage(b, repay_amount_usd);
            score_a.partial_cmp(&score_b).unwrap()
        })
        .cloned()
        .unwrap()
}
```

**V2 multi-call batching**

For positions requiring multiple 50% liquidation calls (V2), an atomic batching contract loops until health factor ≥ 1:

```solidity
function liquidateLoop(address borrower, address collateral, address debt) external {
    while (getHealthFactor(borrower) < 1e18) {
        uint maxRepay = getMaxRepay(borrower, debt); // 50% of debt
        AAVE.liquidationCall(collateral, debt, borrower, maxRepay, false);
    }
}
```

---

### 4.7 Synthetix flag + delayed liquidation

| Attribute | Value |
|-----------|-------|
| Profitability | 6/10 |
| Competition | 4/10 |
| Capital required | Medium (sUSD) |
| Complexity | 5/10 |
| Chains | Optimism, ETH |
| Frequency | Weekly |

**Mechanism**

Synthetix stakers must maintain a minimum collateralization ratio (C-ratio). When a staker falls below, the liquidation process is two-step:

**Step 1 — Flag race**
```solidity
// Any caller can flag an undercollateralized staker
// Earns flagReward in SNX
synthetix.flagAccountForLiquidation(staker_address);
```

**Step 2 — Delayed liquidation race**
After `liquidationDelay` (currently 12 hours on Optimism), the position becomes liquidatable:
```solidity
// Callable by anyone after the delay expires
synthetix.liquidateDelinquentAccount(staker, susd_amount, exchange);
```

**Block-precise scheduling**

```rust
let flag_time = synthetix.getLastFlaggedTime(staker).call().await?;
let delay = synthetix.liquidationDelay().call().await?;
let liquidation_block = estimate_block_at_timestamp(flag_time + delay);

// Schedule transaction to fire at liquidation_block
scheduler.add(liquidation_block, build_liquidation_tx(staker));
```

**Moat**: the delay is deterministic — no latency advantage needed, only correct scheduling.

---

### 4.8 Liquity recovery mode cascade

| Attribute | Value |
|-----------|-------|
| Profitability | 8/10 |
| Competition | 3/10 |
| Capital required | Low (LUSD) |
| Complexity | 7/10 |
| Chains | ETH L1 |
| Frequency | Rare / mode-triggered |

**Mechanism**

Liquity has two liquidation regimes:

| Mode | Condition | Liquidation threshold |
|------|-----------|----------------------|
| Normal | TCR ≥ 150% | ICR < 110% |
| Recovery | TCR < 150% | ICR < 150% |

When system TCR approaches 150%, the set of liquidatable troves expands dramatically — every trove under 150% ICR becomes eligible (vs only under 110% in normal mode). This creates a sudden availability of hundreds of simultaneously liquidatable positions.

**Bot strategy**

```rust
// Monitor TCR in real time
loop {
    let tcr = trove_manager.getTCR(eth_price).call().await?;
    if tcr < U256::from(155 * 1e16 as u64) { // Within 5% of threshold
        // Pre-compute all troves with 110% < ICR < 150%
        let eligible_troves = get_troves_in_range(1.10, 1.50, eth_price).await?;
        // Stage cascade liquidation bundle
        stage_bundle(eligible_troves).await?;
    }
}
```

**Moat**: most bots monitor only TCR < 110% (normal mode). TCR < 150% monitoring is less common, making this niche significantly less contested.

---

### 4.9 Liquity stability pool front-run

| Attribute | Value |
|-----------|-------|
| Profitability | 6/10 |
| Competition | 5/10 |
| Capital required | Medium (LUSD) |
| Complexity | 5/10 |
| Chains | ETH L1 |
| Frequency | Weekly |

**Mechanism**

The Liquity stability pool absorbs liquidations: it burns LUSD to cover liquidated debt and receives ETH collateral at a ~10% discount. Depositors share this ETH proportionally to their LUSD share.

**Front-run deposit**
Detecting an imminent profitable liquidation, deposit LUSD into the stability pool immediately before to capture a larger share of the discounted ETH.

**Exit race (defensive)**
Before a "bad" liquidation (ETH falling rapidly, collateral value less than debt absorbed), other depositors race to exit the stability pool to avoid absorbing underwater debt.

Both sides of this market create MEV simultaneously during volatile ETH price moves.

---

### 4.10 MakerDAO Clip Dutch auction take()

| Attribute | Value |
|-----------|-------|
| Profitability | 7/10 |
| Competition | 6/10 |
| Capital required | None (flash via join adapter) |
| Complexity | 6/10 |
| Chains | ETH L1 |
| Frequency | Daily |

**Mechanism**

MakerDAO's Clip liquidator uses a Dutch auction. The auction price starts high and decays geometrically over time according to the `calc` contract (typically `LinearDecrease` or `StairstepExponentialDecrease`).

**Optimal take() block calculation**

```rust
fn optimal_take_block(
    auction: &ClipAuction,
    your_collateral_cost: f64,
) -> u64 {
    // Simulate price decay over future blocks
    for block in current_block..auction.end_block {
        let price = simulate_price_at_block(auction, block);
        let profit = (auction.collateral_amount * price) - your_collateral_cost;
        let gas_cost = estimate_gas(block);
        if profit > gas_cost + MIN_PROFIT_THRESHOLD {
            return block;
        }
    }
    panic!("No profitable block found");
}
```

**Flash loan via join adapter**

Maker's `take()` function accepts a `who` address and `data` payload, enabling a callback. By routing through a custom contract that calls `daiJoin.join()` inside the callback, the entire operation requires zero upfront DAI capital.

---

### 4.11 GMX v1 keeper race

| Attribute | Value |
|-----------|-------|
| Profitability | 5/10 |
| Competition | 8/10 |
| Capital required | None |
| Complexity | 4/10 |
| Chains | Arbitrum, Avalanche |
| Frequency | Daily |

**Mechanism**

GMX v1 positions are liquidatable when the remaining collateral (after losses + fees) falls below a maintenance margin. The `liquidatePosition()` function is publicly callable and the liquidation fee goes entirely to `msg.sender`.

**Position monitoring**

```rust
// Listen to IncreasePosition and DecreasePosition events
// Build position table: account → (collateralToken, indexToken, isLong, size, collateral, entryPrice)
// For each position, compute: is_liquidatable(current_price, funding_rate)
```

**GMX's own keeper** runs the same logic. The race is pure RPC speed — private mempool access (bloXroute, Alchemy private endpoints) and superior RPC latency are the only moats.

---

### 4.12 Perp protocol keeper (dYdX / Kwenta)

| Attribute | Value |
|-----------|-------|
| Profitability | 5/10 |
| Competition | 7/10 |
| Capital required | None |
| Complexity | 5/10 |
| Chains | Arbitrum, Optimism, Polygon |
| Frequency | Daily |

**Mechanism**

Each perpetual protocol has its own liquidation trigger and keeper reward structure:

| Protocol | Chain | Trigger | Reward |
|----------|-------|---------|--------|
| dYdX v4 | Cosmos app-chain | Maintenance margin | % of collateral |
| Kwenta / Synthetix Perps | Optimism | C-ratio | liquidationPremium |
| Gains Network (gTrade) | Polygon, Arbitrum | Collateral threshold | % of remaining collateral |
| Perpetual Protocol | Optimism | Maintenance margin | % of position |

**Moat**: off-chain position simulation using real-time CEX prices (not the lagging on-chain oracle) to detect liquidatable positions before the protocol's own keeper does.

---

### 4.13 Interest accrual liquidation

| Attribute | Value |
|-----------|-------|
| Profitability | 5/10 |
| Competition | 2/10 |
| Capital required | Low |
| Complexity | 5/10 |
| Chains | ETH, Polygon, Base, Arbitrum |
| Frequency | Continuous (scheduled) |
| Latency type | Deterministic block scheduling |

**Mechanism**

Lending protocol positions accumulate interest every block. A position that is marginally healthy today will eventually become undercollateralized purely from interest accrual — with no price movement required.

**Time-to-liquidation calculation**

```rust
fn time_to_liquidation(position: &Position, borrow_rate: f64) -> Option<u64> {
    // Health factor decays as debt grows via compounding interest
    // HF(t) = collateral_value / (debt_at_t0 * (1 + rate)^t * liquidation_threshold)
    // Solve for t where HF(t) = 1.0
    let t_blocks = solve_hf_crossing(position, borrow_rate, 1.0)?;
    Some(current_block() + t_blocks)
}
```

**Scheduling infrastructure**

```rust
// Add to priority queue: (liquidation_block, position_address)
scheduler.insert(PriorityEntry {
    block: time_to_liquidation(&pos, rate)?,
    action: Action::Liquidate(pos.address),
});

// On each new block, pop and execute due entries
scheduler.process_due(current_block).await?;
```

**Why this niche is underexploited**

Virtually all liquidation bots are reactive: they check health factor against *current* prices. Interest accrual liquidations require a proactive model: project health factor forward in time. The engineering overhead is low, the competition is near-zero, and the opportunity is continuous — it never dries up as long as lending markets exist.

---

### 4.14 NFT collateral liquidation

| Attribute | Value |
|-----------|-------|
| Profitability | 6/10 |
| Competition | 3/10 |
| Capital required | High |
| Complexity | 7/10 |
| Chains | ETH L1 |
| Frequency | Weekly |

**Mechanism**

Three distinct sub-strategies across NFT lending protocols:

**BendDAO (peer-to-pool)**
When an NFT-backed loan's health factor falls below 1 (based on collection floor price), a 48-hour Dutch auction begins. The first bidder at floor price acquires the NFT at a discount if no one outbids. Risk: NFT true value < stated floor.

**Blur Blend (perpetual lending)**
Blend loans have no fixed duration but lenders can trigger a 30-day Dutch auction for repayment. If the borrower doesn't refinance, the lender seizes the NFT. Bot strategy: monitor active Blend loans where `refinancing_APR > implied_floor_yield`, indicating the borrower will likely not refinance.

**NFTfi secondary market**
Undervalued loans (borrowed amount > current floor value) trade at a discount on NFTfi's secondary market. Buy the loan at discount, foreclose, receive the NFT at the original loan value.

**Floor price oracle reliability**

NFT floor prices are manipulated more easily than fungible token prices. Always cross-reference floor price across multiple sources:
- NFTX pool price
- Sudoswap AMM price
- Blur/OpenSea marketplace floor (wash-trading adjusted)

---

### 4.15 Bad debt prevention optimizer

| Attribute | Value |
|-----------|-------|
| Profitability | 5/10 |
| Competition | 4/10 |
| Capital required | Medium |
| Complexity | 6/10 |
| Chains | ETH, Arbitrum, Base |
| Frequency | Daily |

**Mechanism**

Bad debt occurs when a position's collateral is insufficient to cover the debt + liquidation bonus. Liquidating such a position results in the protocol absorbing the shortfall. Bots that detect near-bad-debt positions and optimize their liquidation to prevent this outcome are rewarded with the maximum extractable bonus while protecting protocol solvency.

**Optimization variables**

- `close_factor`: how much debt to repay (0–100% depending on HF)
- `collateral_choice`: which collateral asset to seize
- `liquidation_size`: constrained by the maximum swap size before slippage erodes the bonus

**Slippage modeling**

```rust
fn expected_profit(
    collateral: &Asset,
    repay_amount_usd: f64,
    liq_bonus: f64,
    pool_reserves: &PoolReserves,
) -> f64 {
    let gross_profit = repay_amount_usd * liq_bonus;
    let swap_slippage = estimate_price_impact(collateral.amount, pool_reserves);
    let gas = estimate_gas_cost();
    gross_profit - swap_slippage - gas
}
```

---

## 5. Oracle / Rebase / Peg

Strategies that exploit discrepancies between on-chain oracle prices and real market prices, or between token supply mechanics and AMM reserve accounting.

---

### 5.1 Stablecoin depeg arbitrage

| Attribute | Value |
|-----------|-------|
| Profitability | 8/10 |
| Competition | 6/10 |
| Capital required | High |
| Complexity | 5/10 |
| Chains | ETH, Polygon, Arbitrum |
| Frequency | Rare / event-driven |

**Mechanism**

Stablecoins briefly trade away from their $1 peg during:
- Black swan events (USDC depeg March 2023: ~$0.87 on some DEXs)
- Liquidity crises (UST collapse)
- Bridge or custody issues

The arb: buy the depegged stablecoin at discount, swap back to a stable peg.

**March 2023 USDC example**

USDC/DAI on Uniswap V3: USDC traded at $0.87 while DAI held $1.00 on Maker. The arb: buy USDC with DAI at $0.87 on-chain, redeem USDC at Circle for $1.00 (or wait for repeg). Bots that had pre-positioned capital on-chain and pre-modeled this scenario extracted tens of millions in hours.

**Pre-positioning requirement**

The depeg event lasts one to twelve hours. Capital cannot be deployed fast enough from a cold start. Pre-positioned capital in the pool + a monitoring system that fires automatically on price deviation threshold is required.

---

### 5.2 TWAP oracle manipulation

| Attribute | Value |
|-----------|-------|
| Profitability | 7/10 |
| Competition | 4/10 |
| Capital required | High |
| Complexity | 8/10 |
| Chains | ETH, Polygon |
| Frequency | Targeted / protocol-specific |

**Mechanism**

Uniswap V2/V3 TWAP oracles are used by some protocols (older Compound deployments, some synthetics protocols) as their price source. A TWAP is the time-weighted average of `log(price)` over a window.

An attacker can manipulate the TWAP by:
1. Executing large swaps in a V2/V3 pool to move the spot price significantly
2. Holding the price at that level for multiple blocks (costly but possible with large capital)
3. The TWAP eventually drifts toward the manipulated price
4. Using the manipulated TWAP to extract value from a protocol that relies on it

**Defensive MEV (counter-strategy)**

Bots can monitor TWAP pools for manipulation attempts and:
- Arb the manipulated spot price back to fair value (profits from the attacker's cost)
- Alert liquidation systems that are relying on the TWAP

**Note**: direct TWAP manipulation is adversarial toward protocols and users. The defensive counter-arb is the constructive angle.

---

### 5.3 Rebase token arbitrage

| Attribute | Value |
|-----------|-------|
| Profitability | 5/10 |
| Competition | 3/10 |
| Capital required | Low |
| Complexity | 4/10 |
| Chains | ETH, Avalanche |
| Frequency | Daily (rebase cadence) |

**Mechanism**

Elastic supply tokens (AMPL, OHM elastic tranches, BASED) adjust all holder balances periodically. When a positive rebase occurs:

1. Every holder's balance increases proportionally
2. The Uniswap V2 pool's actual token balance increases (via the rebase)
3. But `reserve0` / `reserve1` do not update until a swap, `sync()`, or `skim()` occurs
4. The pool's implied price is now stale — it undervalues the rebased token

**Arb window**

Between the rebase event and the next `sync()`/`skim()` call, the pool can be traded against. Buy the undervalued rebased token through the pool before reserves are updated.

**AMPL rebase schedule**

AMPL rebases daily at approximately 2 AM UTC (can be computed deterministically). Pre-stage transactions to fire in the first block after the rebase if the rebase is positive (supply expands → token undervalued in pool).

---

### 5.4 Fee-on-transfer token arbitrage

| Attribute | Value |
|-----------|-------|
| Profitability | 4/10 |
| Competition | 3/10 |
| Capital required | Low |
| Complexity | 4/10 |
| Chains | BSC primarily; ETH |
| Frequency | Continuous |

**Mechanism**

Fee-on-transfer tokens deduct a percentage on every transfer. When the router calls `transferFrom(user, pair, amountIn)`, the pair receives `amountIn × (1 - fee)`. The router calculates output using the full `amountIn`, but the pair uses only the received amount.

This creates a systematic reserve drift: over many swaps, the pair's reserves diverge from the implied price. The drift accumulates and creates intermittent arb opportunities.

Additionally, FoT tokens can make `balance < reserve` (if the fee is burned rather than redistributed), in which case a `sync()` call corrects the reserve, changing the implied price and opening an arb.

**V2 pair detection**

Most Uniswap V2 routers have a dedicated `swapExactTokensForTokensSupportingFeeOnTransferTokens` function. Monitoring calls to this function reveals which tokens are FoT.

---

## 6. Cross-Domain

Strategies that operate across multiple chains, layers, or block-production systems.

---

### 6.1 Bridge MEV

| Attribute | Value |
|-----------|-------|
| Profitability | 7/10 |
| Competition | 4/10 |
| Capital required | High |
| Complexity | 7/10 |
| Chains | ETH ↔ L2s, ETH ↔ BSC |
| Frequency | Continuous |

**Mechanism**

Cross-chain bridges have a finality gap between locking on the source chain and minting on the destination. The price of the asset may differ between chains during this window.

**Exploitable bridges and their windows**

| Bridge | Mechanism | Finality gap |
|--------|-----------|-------------|
| Wormhole | Guardian attestation | ~30 seconds |
| LayerZero | Oracle + Relayer | 1–5 minutes |
| Across | Optimistic + relayer | Near-instant (relayer fronts) |
| Canonical (Optimism/Arbitrum) | L1 fraud proof window | 7 days (withdrawal) |

**Strategy**

The canonical bridge MEV is not in bridging assets yourself (too slow) but in:
1. Monitoring bridge deposit/lock events on source chain
2. Arbing the resulting price impact on destination chain before the bridge mint finalizes
3. Providing liquidity on fast bridge protocols (Across, Hop) to earn relayer fees

---

### 6.2 L2 sequencer MEV

| Attribute | Value |
|-----------|-------|
| Profitability | 6/10 |
| Competition | 5/10 |
| Capital required | Low |
| Complexity | 6/10 |
| Chains | Base, Optimism, Arbitrum, BSC |
| Frequency | Continuous |

**Mechanism**

Each L2 has a different ordering model that determines MEV availability:

| Chain | Ordering model | MEV available |
|-------|---------------|---------------|
| Arbitrum | FCFS (strict first-come-first-serve) | No sandwich; backrun only |
| Optimism / Base | Sequencer-controlled | Sandwich possible; sequencer captures some MEV |
| BSC | Validator-controlled (21 validators) | Full MEV; 48Club for private ordering |
| Polygon | Bor block producer | Full MEV; limited private channels |

**Arbitrum FCFS note**

Sandwich attacks are structurally impossible on Arbitrum because FCFS ordering prevents a bot from inserting a transaction before a known victim. However, backrunning is possible by submitting with minimal latency after detecting a target transaction.

**48Club (BSC)**

A private MEV relay on BSC where validators agree to include bundles from registered searchers with priority. Access requires application and stake. Provides the equivalent of Flashbots on BSC.

---

### 6.3 PBS / MEV-Boost

| Attribute | Value |
|-----------|-------|
| Profitability | 9/10 (block builder) |
| Competition | 9/10 |
| Capital required | High |
| Complexity | 9/10 |
| Chains | ETH L1 |
| Frequency | Every block |

**Mechanism**

Proposer-Builder Separation (PBS) separates the role of block production (validators) from block construction (builders). MEV-Boost is the dominant implementation on Ethereum post-Merge.

**Roles**

| Role | Function | MEV position |
|------|----------|-------------|
| Searcher | Finds arb/liquidation opportunities | Submits bundles to builders |
| Builder | Constructs optimal blocks from bundles | Pays validator for block slot |
| Relay | Connects builders and validators | Ensures fair exchange |
| Validator | Proposes blocks | Receives MEV-Boost payments |

**Searcher strategy**

A searcher's goal is to construct bundles with positive value and submit them to block builders (e.g. Flashbots, beaverbuild, rsync-builder). Bundles are atomic: either the entire bundle lands in the block or none of it does.

**Bundle format (Flashbots)**

```json
{
  "txs": ["0x...", "0x..."],
  "blockNumber": "0x1234567",
  "minTimestamp": 0,
  "maxTimestamp": 0,
  "revertingTxHashes": []
}
```

---

### 6.4 Cross-chain arbitrage

| Attribute | Value |
|-----------|-------|
| Profitability | 7/10 |
| Competition | 5/10 |
| Capital required | High |
| Complexity | 7/10 |
| Chains | ETH ↔ Arbitrum ↔ Base ↔ Polygon ↔ BSC |
| Frequency | Continuous |

**Mechanism**

The same token (e.g. USDC, WETH) can trade at different prices across chains due to:
- Asymmetric liquidity distribution
- Bridge bottlenecks creating temporary supply imbalances
- Chain-specific demand events (launchpads, token unlock events)

**Infrastructure requirement**

Capital must be pre-deployed across all monitored chains simultaneously. Rebalancing via bridges is too slow for intraday arb — the position must already exist on both chains.

**On-chain execution**

Cross-chain arb is typically executed as two independent transactions on two chains rather than one atomic cross-chain transaction. This introduces execution risk: the sell leg may fail after the buy leg succeeds. Risk management: use slippage-protected swaps and accept that not every arb closes cleanly.

---

## 7. Protocol Niches

Strategies targeting the specific mechanics of individual DeFi protocols.

---

### 7.1 Curve pool imbalance

| Attribute | Value |
|-----------|-------|
| Profitability | 7/10 |
| Competition | 5/10 |
| Capital required | Medium |
| Complexity | 6/10 |
| Chains | ETH, Polygon, Arbitrum |
| Frequency | Daily |

**Mechanism**

Curve's StableSwap invariant (`A × Σxᵢ + D = A × n^n × D + D^(n+1) / (n^n × Πxᵢ)`) provides near-zero slippage near equilibrium but creates progressively more slippage as the pool imbalances. When the pool's token ratios deviate significantly from the target weights (1:1 for most stablecoin pools), there is arbitrage profit available by swapping the underrepresented token in.

**Detection**

```rust
// For a 3pool (DAI/USDC/USDT), check balances
let balances = curve_pool.get_balances().call().await?;
let target = total_tvl / 3;
let max_deviation = balances.iter()
    .map(|b| (b - target).abs() / target)
    .fold(0.0, f64::max);
if max_deviation > THRESHOLD {
    // Pool is imbalanced — arb opportunity
}
```

**Additional Curve mechanics**

- `add_liquidity` with imbalanced amounts → receive bonus LP tokens → immediately remove → arbitrage the imbalance
- Admin fee accrual creates minor reserve drift (similar to Uniswap V2 FoT effect)

---

### 7.2 Governance MEV

| Attribute | Value |
|-----------|-------|
| Profitability | 6/10 |
| Competition | 3/10 |
| Capital required | Medium |
| Complexity | 7/10 |
| Chains | ETH, Arbitrum |
| Frequency | Per governance cycle |

**Mechanism**

Approved governance proposals create predictable future state changes. Bots that read proposal queues and predict protocol parameter changes can pre-position before the change takes effect.

**Common opportunities**

- **Interest rate model changes**: If a governance vote will lower the AAVE USDC supply APY, pre-exit your USDC supply position before the change is applied
- **Collateral factor changes**: If a governance vote will lower the LTV of an asset, positions using that asset as collateral will have their health factor reduced — enabling pre-staged liquidations at the exact block the proposal executes
- **Fee switch activations**: Uniswap governance activating the fee switch concentrates more fees to LPs — pre-providing liquidity benefits from the new regime

**Timelock-based prediction**

Most governance systems use a timelock (24–48 hours on mainnet). The execution block is computable in advance, allowing precise pre-staging.

---

### 7.3 Airdrop MEV

| Attribute | Value |
|-----------|-------|
| Profitability | 5/10 |
| Competition | 6/10 |
| Capital required | None |
| Complexity | 4/10 |
| Chains | ETH, Arbitrum, Base |
| Frequency | Episodic |

**Mechanism**

Airdrop claim contracts typically use a Merkle tree where each eligible address has a proof. The first transaction that calls `claim(address, amount, proof)` for a given address receives the tokens.

**Front-run mechanics**

A bot monitors the mempool for `claim()` transactions, extracts the `(address, amount, proof)` parameters, and submits an identical transaction with higher gas to land first. The original claimer's transaction reverts (already claimed).

**Unlock event front-running**

For team/investor token unlocks with cliff dates: monitor the vesting contract's unlock timestamp, submit a `transfer()` or `sell()` transaction for the exact unlocking block. Racing the token holder to sell ahead of anticipated price impact.

**Airdrop contract patterns to monitor**

```solidity
// Standard Merkle airdrop (Uniswap/Arbitrum style)
function claim(uint256 index, address account, uint256 amount, bytes32[] calldata merkleProof)

// Direct claim (no Merkle — just eligible addresses)
function claim()

// Vesting claim
function release(address beneficiary)
```

---

### 7.4 ERC-4337 AA bundler MEV

| Attribute | Value |
|-----------|-------|
| Profitability | 5/10 |
| Competition | 4/10 |
| Capital required | Low |
| Complexity | 6/10 |
| Chains | ETH, Polygon, Base, Optimism |
| Frequency | Continuous |

**Mechanism**

ERC-4337 Account Abstraction introduces `UserOperation` objects that are collected in an `alt mempool` and submitted to the `EntryPoint` contract by a `bundler`. The bundler chooses which UserOps to include and in what order within the bundle.

**MEV surface**

- **UserOp ordering**: Within a single `handleOps()` call, the bundler orders UserOps arbitrarily. A UserOp that triggers a large DEX swap can be front-run by another UserOp within the same bundle.
- **Bundler fee extraction**: Bundlers can set `maxFeePerGas` lower than the priority fee and pocket the difference as MEV.
- **Paymaster MEV**: Paymasters that subsidize gas fees for token payments create opportunities around the token-to-gas conversion rate.

**ERC-4337 vs traditional MEV**

Traditional MEV operates at the transaction level. AA MEV operates at the UserOperation level — a new, less-competitive MEV market that is currently in early stages of exploitation.

---

## 8. Emerging

Structural shifts in DeFi market design that create new MEV categories distinct from classical order-flow MEV.

---

### 8.1 Solver / intent MEV

| Attribute | Value |
|-----------|-------|
| Profitability | 8/10 |
| Competition | 5/10 |
| Capital required | Medium |
| Complexity | 7/10 |
| Chains | ETH, Arbitrum (UniswapX); ETH (CoW) |
| Frequency | Continuous |

**Mechanism**

Intent-based protocols (UniswapX, CoW Protocol, 1inch Fusion) replace on-chain AMM swaps with off-chain auctions where solvers compete to fill user orders. The "MEV" shifts from block ordering to solver optimization.

**Solver profit model**

```
user_intent: sell 1 ETH, receive at least 2,950 USDC
solver fills at: 1 ETH → 2,975 USDC (market price)
solver pays user: 2,955 USDC
solver profit: 2,975 - 2,955 = 20 USDC (spread capture)
```

**Solver competitive advantage**

- Access to more liquidity sources → better fill price → wider spread
- Faster settlement → fewer failed auctions → more solver reputation
- Capital efficiency: solvers who internalize orders (fill from inventory without on-chain swaps) have lower costs

**Architecture**

```rust
// Solver loop
loop {
    let intents = fetch_pending_intents().await?;
    for intent in intents {
        let best_fill = find_optimal_fill(&intent, &all_liquidity_sources).await?;
        let profit = best_fill.price - intent.min_output;
        if profit > MIN_SOLVER_PROFIT {
            submit_solution(&intent, &best_fill).await?;
        }
    }
}
```

---

### 8.2 Batch auction MEV

| Attribute | Value |
|-----------|-------|
| Profitability | 7/10 |
| Competition | 4/10 |
| Capital required | Medium |
| Complexity | 8/10 |
| Chains | ETH (CoW Protocol, 1inch Fusion) |
| Frequency | Continuous (batch cadence ~30s) |

**Mechanism**

CoW Protocol collects all pending orders into a batch, then solvers compete to find the optimal settlement path that satisfies all orders simultaneously. The best settlement (maximizing trader surplus) wins the batch.

**This is structurally different from classical MEV**:
- No block ordering — all orders in the batch are treated equally
- No front-running — solvers see all orders before settling
- MEV = routing optimization quality, not latency

**Settlement path optimization**

For a batch containing {buy ETH with USDC, buy USDC with DAI, sell ETH for DAI}, the optimal solver finds a Coincidence of Wants (CoW) — an internal match — and routes remaining imbalance through on-chain liquidity. The internal match avoids DEX fees entirely.

**CoW Protocol solver submission**

```
1. Receive batch from CoW backend API
2. Solve: find optimal combination of internal matches + on-chain routes
3. Submit settlement transaction within deadline
4. If your settlement wins: receive solver reward (ETH + COW tokens)
```

---

### 8.3 NFT floor arbitrage

| Attribute | Value |
|-----------|-------|
| Profitability | 6/10 |
| Competition | 4/10 |
| Capital required | High |
| Complexity | 6/10 |
| Chains | ETH L1 |
| Frequency | Daily |

**Mechanism**

NFTs trade across multiple venues with different pricing mechanisms:

| Venue | Pricing | Liquidity |
|-------|---------|-----------|
| Blur marketplace | Orderbook (bids/asks) | High |
| OpenSea | Fixed price + auctions | High |
| NFTX | AMM (pool per collection) | Medium |
| Sudoswap | AMM (bonding curve) | Medium |

Price discrepancies between venues create arb:

- NFT listed on Blur below the NFTX pool price → buy on Blur, sell into NFTX pool
- NFTX pool overpriced vs Blur floor → buy cheapest NFT on Blur, deposit into NFTX for pool tokens, sell pool tokens
- Cross-collection arb (less common): correlated collections with divergent floor ratios

**Floor price manipulation risk**

NFT floors can be wash-traded. Always verify floor against at least two independent sources and use the lower bound.

---

### 8.4 Token launch snipe

| Attribute | Value |
|-----------|-------|
| Profitability | 9/10 (high variance) |
| Competition | 8/10 |
| Capital required | Low |
| Complexity | 7/10 |
| Chains | BSC, ETH, Base |
| Frequency | Continuous |

**Mechanism**

New token pools (especially meme coins) are deployed with concentrated initial liquidity. Early buyers in the first few blocks receive the lowest price. Sniping bots monitor for new pool `PairCreated` or `PoolCreated` events and submit buy transactions in the same block or the next block.

**Signal quality is the edge — not speed**

Being first into a rug is worse than being third into a genuine token. The alpha is not raw speed but signal quality:

- **Honeypot detection**: simulate `approve()` + `sell()` via revm — if the sell reverts, it's a honeypot
- **Ownership check**: is the deployer contract renounced or does it retain mint/pause capabilities?
- **Liquidity lock check**: is the LP locked in a time-lock contract?
- **Tax check**: simulate a buy and sell to measure the effective tax (FoT%)
- **Social signal integration**: token name + deployer address pattern matching against known rug patterns

**revm simulation before snipe**

```rust
async fn safe_to_snipe(pool: &NewPool) -> bool {
    let mut evm = Evm::builder()
        .with_db(fork_db.clone())
        .build();

    // Simulate buy
    let buy_result = simulate_buy(&mut evm, pool, BUY_AMOUNT).await;
    if buy_result.is_err() { return false; }

    // Simulate immediate sell
    let sell_result = simulate_sell(&mut evm, pool, buy_result.unwrap().tokens_received).await;
    if sell_result.is_err() { return false; }

    // Check: did we receive reasonable value back?
    let roundtrip_ratio = sell_result.unwrap().eth_received / BUY_AMOUNT;
    roundtrip_ratio > 0.85 // Allow up to 15% tax
}
```

---

## 9. Master Rankings Table

Ranked by profitability (descending). Competition score: lower = less contested = better opportunity.

| # | Strategy | Profitability | Competition | Capital | Complexity | Frequency |
|---|----------|:---:|:---:|---------|:---:|-----------|
| 1 | Cascading liquidation engineering | 10/10 | 2/10 | High (flash OK) | 10/10 | Rare |
| 2 | Token launch snipe | 9/10 | 8/10 | Low | 7/10 | Continuous |
| 3 | MakerDAO OSM preview + kick() | 9/10 | 3/10 | None | 8/10 | Hourly |
| 4 | Oracle-latency liquidation | 9/10 | 5/10 | Medium | 7/10 | Intraday |
| 5 | LST depeg collateral liquidation | 9/10 | 4/10 | Medium | 7/10 | Rare |
| 6 | CEX–DEX arbitrage | 9/10 | 9/10 | High | 8/10 | Continuous |
| 7 | PBS / MEV-Boost (block building) | 9/10 | 9/10 | High | 9/10 | Every block |
| 8 | Solver / intent MEV | 8/10 | 5/10 | Medium | 7/10 | Continuous |
| 9 | JIT + arb combo | 9/10 | 3/10 | High | 9/10 | On large swaps |
| 10 | Flash loan atomic liquidation | 8/10 | 7/10 | None | 6/10 | Daily |
| 11 | Liquity recovery mode cascade | 8/10 | 3/10 | Low | 7/10 | Rare |
| 12 | Stablecoin depeg arbitrage | 8/10 | 6/10 | High | 5/10 | Rare |
| 13 | JIT liquidity | 8/10 | 5/10 | High | 7/10 | On large swaps |
| 14 | Flash swap arbitrage | 7/10 | 7/10 | None | 5/10 | Continuous |
| 15 | Sandwich attack | 7/10 | 8/10 | Medium | 5/10 | Continuous |
| 16 | MakerDAO Clip Dutch auction take() | 7/10 | 6/10 | None | 6/10 | Daily |
| 17 | AAVE partial liquidation optimizer | 7/10 | 7/10 | Medium | 6/10 | Daily |
| 18 | Cross-chain arbitrage | 7/10 | 5/10 | High | 7/10 | Continuous |
| 19 | Init price snipe | 7/10 | 5/10 | Low | 4/10 | New pools |
| 20 | Curve pool imbalance | 7/10 | 5/10 | Medium | 6/10 | Daily |
| 21 | Backrunning | 7/10 | 6/10 | Low | 4/10 | Continuous |
| 22 | TWAP oracle manipulation | 7/10 | 4/10 | High | 8/10 | Targeted |
| 23 | Batch auction MEV | 7/10 | 4/10 | Medium | 8/10 | Continuous |
| 24 | Governance MEV | 6/10 | 3/10 | Medium | 7/10 | Per cycle |
| 25 | Synthetix flag + delayed liquidation | 6/10 | 4/10 | Medium | 5/10 | Weekly |
| 26 | NFT floor arbitrage | 6/10 | 4/10 | High | 6/10 | Daily |
| 27 | NFT collateral liquidation | 6/10 | 3/10 | High | 7/10 | Weekly |
| 28 | Liquity stability pool front-run | 6/10 | 5/10 | Medium | 5/10 | Weekly |
| 29 | skim() capture | 6/10 | 4/10 | None | 3/10 | Continuous |
| 30 | Long-tail token arbitrage | 6/10 | 3/10 | Low | 5/10 | Continuous |
| 31 | Statistical arbitrage / pairs | 6/10 | 4/10 | Medium | 7/10 | Intraday |
| 32 | L2 sequencer MEV | 6/10 | 5/10 | Low | 6/10 | Continuous |
| 33 | V3 range order snipe | 6/10 | 4/10 | Low | 5/10 | Continuous |
| 34 | Rebase token arbitrage | 5/10 | 3/10 | Low | 4/10 | Daily |
| 35 | GMX v1 keeper race | 5/10 | 8/10 | None | 4/10 | Daily |
| 36 | Perp protocol keeper | 5/10 | 7/10 | None | 5/10 | Daily |
| 37 | Interest accrual liquidation | 5/10 | 2/10 | Low | 5/10 | Continuous |
| 38 | Bad debt prevention optimizer | 5/10 | 4/10 | Medium | 6/10 | Daily |
| 39 | ERC-4337 AA bundler MEV | 5/10 | 4/10 | Low | 6/10 | Continuous |
| 40 | Airdrop MEV | 5/10 | 6/10 | None | 4/10 | Episodic |
| 41 | Bridge MEV | 7/10 | 4/10 | High | 7/10 | Continuous |
| 42 | sync() race | 3/10 | 3/10 | None | 2/10 | Continuous |
| 43 | Fee-on-transfer token arb | 4/10 | 3/10 | Low | 4/10 | Continuous |

**Opportunity score** = `(Profitability × 2) - Competition` — a rough measure of risk-adjusted opportunity for a new entrant:

| Rank | Strategy | Opportunity Score |
|------|----------|:-----------------:|
| 1 | Cascading liquidation engineering | 18 |
| 2 | MakerDAO OSM preview | 15 |
| 3 | JIT + arb combo | 15 |
| 4 | Liquity recovery mode cascade | 13 |
| 5 | Interest accrual liquidation | 8 → but competition is 2, so best **low-barrier** pick |

---

## 10. Cross-Cutting Infrastructure

Regardless of strategy, the following infrastructure components determine competitiveness across all categories.

### Order flow access

| Feed | Coverage | Cost model |
|------|----------|------------|
| Flashbots MEV-Share | ETH | Free (share profits with users) |
| bloXroute | ETH, BSC, Polygon | Subscription |
| Fiber (chainbound) | ETH | Subscription |
| 48Club | BSC | Application + stake |
| Alchemy private | ETH | Pay-per-use |

### Simulation accuracy (revm)

All profitable strategies require profit simulation before gas commitment. `revm` provides in-process EVM execution at native Rust speeds. Critical for:
- Liquidation profitability (slippage modeling)
- Token launch safety (honeypot detection)
- Flash loan path validation
- JIT tick range optimization

### Storage (redb)

Position databases, pool registries, and historical state must persist across bot restarts. `redb` (embedded key-value store) with a prefixed key scheme:

```
pool:{address}     → (reserve0, reserve1, fee, last_updated)
position:{address} → (health_factor, collateral, debt, protocol)
token:{address}    → (is_fot, rebase_schedule, decimals)
```

### Gas optimization

- Ternary search on priority fee tip for sandwich/JIT bundles
- Yul-optimized executor contracts (reduce calldata, inline assembly for hot paths)
- EIP-7702 (account abstraction for EOAs) — monitor for new gas optimization surfaces

### Bundle submission channels

| Chain | Channel | Type |
|-------|---------|------|
| ETH | Flashbots `eth_sendBundle` | Private relay |
| ETH | MEV-Share `mev_sendBundle` | Private + rebate |
| BSC | 48Club | Private relay |
| Polygon | bloXroute BDN | Private relay |
| Arbitrum | Standard mempool (FCFS) | Public only |
| Base | Standard mempool | Public only |

---

*Document generated from MEV strategy research session covering V2 pool mechanics, order flow, bundle/positional strategies, liquidations, oracle/rebase/peg strategies, cross-domain MEV, protocol niches, and emerging strategies.*
