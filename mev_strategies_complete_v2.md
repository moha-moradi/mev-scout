# MEV Strategies: Complete Reference

> **53 strategies across 8 categories** — mechanics, edge, capital requirements, competition level, and implementation notes for each.  
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
   - [Velodrome/Aerodrome epoch transition](#75-velodromeaerodrome-epoch-transition)
   - [Pendle PT/YT implied yield spread](#76-pendle-ptyt-implied-yield-spread)
   - [Balancer rate provider staleness](#77-balancer-rate-provider-staleness)
   - [GMX V2 ADL front-run](#78-gmx-v2-adl-front-run)
   - [Lido oracle report front-run](#79-lido-oracle-report-front-run)
   - [Morpho Blue market state transition](#710-morpho-blue-market-state-transition)
   - [Uniswap V4 hook MEV](#711-uniswap-v4-hook-mev)
   - [Convex/Curve gauge vote epoch](#712-convexcurve-gauge-vote-epoch)
   - [Trader Joe V2 Liquidity Book](#713-trader-joe-v2-liquidity-book)
8. [Emerging](#8-emerging)
   - [Solver / intent MEV](#81-solver--intent-mev)
   - [Batch auction MEV](#82-batch-auction-mev)
   - [NFT floor arbitrage](#83-nft-floor-arbitrage)
   - [Token launch snipe](#84-token-launch-snipe)
   - [Multi-block MEV](#85-multi-block-mev)
9. [Master Rankings Table](#9-master-rankings-table)
10. [Cross-Cutting Infrastructure](#10-cross-cutting-infrastructure)
11. [Implementation Roadmap](#11-implementation-roadmap)

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

Strategies tied to the **specific mechanics of individual DeFi protocols**. These opportunities are structurally impossible to replicate on other protocols — they require deep knowledge of a single protocol's storage layout, tokenomics, epoch timing, or liquidity model. This is a strong moat class: most generalist MEV bots don't monitor these surfaces.

Entries 7.1–7.4 target common protocol archetypes. Entries 7.5–7.12 are single-protocol strategies with the highest opportunity-score-to-competition ratios in this document.

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

---

### 7.5 Velodrome/Aerodrome epoch transition

| Attribute | Value |
|-----------|-------|
| Profitability | 6/10 |
| Competition | 2/10 |
| Capital required | Medium |
| Complexity | 6/10 |
| Chains | Optimism (Velodrome), Base (Aerodrome) |
| Frequency | Weekly (per epoch boundary) |

**Mechanism**

Velodrome and Aerodrome are ve(3,3) DEXs where weekly epochs govern the entire emission and fee distribution system. At the epoch boundary (every Thursday ~00:00 UTC on Velodrome), three simultaneous events create MEV:

1. **Gauge weight reset**: CRV-equivalent emissions are redistributed according to the previous week's vote totals. Pools that accumulated more votes suddenly receive higher emission rates — LP APY changes instantaneously.

2. **Bribe and fee distribution**: All trading fees and bribes paid to voters become claimable in the first block of the new epoch.

3. **Vote snapshot finalization**: The on-chain `VotingEscrow` snapshot is finalized; the computed gauge weights are available from storage before the epoch-flip transaction lands.

**Epoch boundary MEV pipeline**

```
T-5min: Read VotingEscrow.totalSupply() and Voter.weights(gauge)
        → Compute exact gauge weight percentages for every pool
        → Identify pools whose emission rate will increase significantly

T-1 block: LP into the to-be-high-emission pools
           (TVL hasn't migrated yet → you earn outsized yield for 1-2 blocks)

T-0 (epoch flip block):
  → Gauge weights reset on-chain
  → All bribe/fee claims become available
  → Automated claim sweep across all voted gauges

T+1 to T+10: Large LPs migrate into newly high-emission pools
  → TVL increase compresses yield back to market rate
  → Exit or hold depending on sustainable emission rate
```

**Storage-read prediction (Rust)**

```rust
// Velodrome Voter contract: total votes per gauge readable before epoch flip
let gauge_weight = voter.weights(gauge_address).call().await?;
let total_weight = voter.totalWeight().call().await?;
let emission_fraction = gauge_weight.as_u128() as f64 / total_weight.as_u128() as f64;

// VELO emissions per week (from Minter contract)
let weekly_emission = minter.weekly().call().await?;
let my_gauge_emission = (weekly_emission.as_u128() as f64 * emission_fraction) as u128;
```

**Moat**

Protocol-specific knowledge of the epoch contract timing and storage layout. Most generalist bots don't monitor Velodrome/Aerodrome epoch events — this is a chain-specific niche with near-zero dedicated competition.

---

### 7.6 Pendle PT/YT implied yield spread

| Attribute | Value |
|-----------|-------|
| Profitability | 6/10 |
| Competition | 2/10 |
| Capital required | Medium |
| Complexity | 7/10 |
| Chains | ETH, Arbitrum |
| Frequency | Daily |

**Mechanism**

Pendle Finance splits yield-bearing tokens into two components:

- **PT** (Principal Token): redeemable 1:1 for the underlying asset at maturity — a zero-coupon bond trading at a discount
- **YT** (Yield Token): receives all yield generated by the underlying until maturity

Pendle's AMM continuously prices these tokens, producing an **implied yield** — the annualized yield the market expects from the underlying. When the implied yield diverges from the actual underlying protocol yield, a durable arbitrage exists.

**Divergence cases**

| Scenario | Implied yield | Actual yield | Action |
|----------|:---:|:---:|--------|
| Market overestimates future yield | 8% | 4% | Mint PT+YT, sell YT (receive upfront premium), hold PT to maturity |
| Market underestimates yield | 3% | 7% | Buy YT at cheap implied price, hold to earn actual yield above cost |
| PT near maturity at discount | 0.5% discount | 0 days to maturity | Buy PT, redeem at face value |

**Near-maturity arb (near risk-free)**

```rust
// PT trades at face_value - discount. At maturity, redeems 1:1.
// If time_to_maturity < 24h and PT discount > gas_cost:
let pt_price = pendle_amm.get_pt_price(market_address).call().await?;
let face_value = U256::from(1e18 as u64); // 1 underlying unit
let discount = face_value.saturating_sub(pt_price);
let gas_cost = estimate_gas_cost_usd();

if u256_to_f64(discount) > gas_cost * 2.0 {
    // Buy PT, wait for maturity or instant redeem if maturity has passed
    router.swapExactTokenForPt(market, pt_amount, min_out, deadline).send().await?;
}
```

**YT convexity edge**

YT has a non-linear payoff: if yield spikes (e.g. Aave USDC borrow demand surge), YT price responds faster than most bots model. Monitoring underlying protocol utilization rates as a leading indicator of Pendle implied yield moves creates a latency edge.

**Moat**

Understanding Pendle's AMM formula (`logistic UAMM`) is required to compute arb size correctly. Most searchers are generalist arbitrageurs — the Pendle-specific AMM is a meaningful technical moat.

---

### 7.7 Balancer rate provider staleness

| Attribute | Value |
|-----------|-------|
| Profitability | 6/10 |
| Competition | 3/10 |
| Capital required | Medium |
| Complexity | 7/10 |
| Chains | ETH, Arbitrum, Polygon |
| Frequency | Daily |

**Mechanism**

Balancer V2 ComposableStablePools (formerly Boosted Pools) use **rate providers** to translate between wrapped and unwrapped token values. For example, a wstETH/WETH pool uses a `WstETHRateProvider` that returns the wstETH/ETH exchange rate from Lido.

The rate provider is not queried on every swap — it is cached inside the pool and updated only when `updateTokenRates()` is called or certain interactions trigger a refresh. This creates a staleness window.

**Staleness → mispricing → arb**

```
1. Lido distributes validator rewards: wstETH/ETH rate increases (e.g. 0.003% per day)
2. Balancer pool's cached rate has not updated yet
3. The pool undervalues wstETH relative to the true rate
4. Swap wstETH → WETH through the pool: receive slightly more WETH than you should
5. Swap back WETH → wstETH on Curve (uses the real rate via Curve's oracle)
6. Profit = rate_delta × swap_amount - fees - gas
```

**Stale rate detection**

```rust
// Compare Balancer pool's cached rate vs the rate provider's live output
let pool_rate = balancer_pool.getTokenRate(wsteth_address).call().await?;
let live_rate = wsteth_rate_provider.getRate().call().await?;

let stale_delta = (u256_to_f64(live_rate) - u256_to_f64(pool_rate))
    / u256_to_f64(pool_rate);

// stale_delta > 0 means pool undervalues wstETH — arb direction: wstETH → WETH in pool
if stale_delta > FEE_THRESHOLD {
    execute_rate_arb(pool_rate, live_rate, optimal_size).await?;
}
```

**Rate update trigger**

After your swap, the next interaction with the pool will update the cached rate, normalizing prices. This is analogous to the `sync()` dynamic in Uniswap V2 but affects the pricing curve rather than reserves.

**Affected pools**

Any Balancer ComposableStablePool with a rate provider is eligible. Current targets include:
- wstETH/WETH
- rETH/WETH  
- cbETH/WETH
- sDAI/DAI (Spark)
- bb-a-USD pools (aToken wrappers)

---

### 7.8 GMX V2 ADL front-run

| Attribute | Value |
|-----------|-------|
| Profitability | 7/10 |
| Competition | 2/10 |
| Capital required | Low |
| Complexity | 7/10 |
| Chains | Arbitrum |
| Frequency | Rare / event-driven |

**Mechanism**

GMX V2 introduces **Auto-Deleveraging (ADL)** to protect pool solvency. When a market's reserved PnL for profitable traders exceeds available liquidity (the `maxPnlFactor` threshold is breached), the system enables keepers to forcibly close the most profitable long or short positions.

Three distinct MEV opportunities around ADL:

**1. ADL trigger prediction**

The ADL trigger is deterministic: monitor the ratio of `reservedUsd` (PnL owed to profitable traders) to `poolAmount` (pool liquidity).

```rust
// DataStore and Reader contracts expose these values
let market_info = reader.getMarketInfo(data_store, market_prices, market).call().await?;

let pnl_ratio = market_info.pnlToPoolFactor; // e18 scaled
let max_factor = data_store.getUint(max_pnl_factor_key).call().await?;

if pnl_ratio > max_factor {
    // ADL is now enabled — keepers can execute ADL on highest-profit positions
    trigger_adl_watch(market).await?;
}
```

When ADL becomes enabled, the positions being targeted (highest unrealized profit) are already in the winning directional trade. Pre-positioning alongside those positions — before ADL executes — captures additional directional profit during the wind-down.

**2. Keeper execution reward**

GMX V2 keepers that execute ADL transactions earn a percentage of the position's collateral as a `keeperExecutionFee`. Running a keeper monitors gas cost vs reward and executes when profitable:

```rust
async fn execute_adl(order_key: B256) -> eyre::Result<()> {
    let execution_fee = estimate_execution_fee().await?;
    let keeper_reward = get_keeper_reward(order_key).await?;
    if keeper_reward > execution_fee * 2 {
        exchange_router.executeAdl(order_key, prices).send().await?;
    }
    Ok(())
}
```

**3. Post-ADL price dislocation backrun**

ADL force-closes large positions, which impacts the GM pool's virtual price and may create a brief dislocation between GMX's internal price and external DEX prices. Backrun the ADL execution with a corrective arb.

---

### 7.9 Lido oracle report front-run

| Attribute | Value |
|-----------|-------|
| Profitability | 7/10 |
| Competition | 3/10 |
| Capital required | Medium |
| Complexity | 7/10 |
| Chains | ETH L1 |
| Frequency | Daily (each oracle report) |

**Mechanism**

Lido's `AccountingOracle` submits a daily report updating the total staked ETH across all validators, the validator reward accrual, and the new wstETH/stETH exchange rate. The oracle operates via a **committee quorum**: individual committee members submit their `reportData` separately, and the report finalizes when a quorum threshold is reached.

**The window**

Each committee member's `submitReportData()` transaction is observable in the mempool or shortly after landing. Once `n-1` of `N` required members have reported, the next member's submission will finalize the report — and the exchange rate update is computable in advance from the accumulated submissions.

**Impact of the report**

```
wstETH/ETH rate increases slightly (validator rewards accrued)
→ stETH/ETH Curve pool: stETH becomes slightly undervalued vs actual rate
→ AAVE: stETH collateral value ticks up → marginally improves health factors
→ wstETH on Uniswap V3 briefly mispriced vs the new rate
```

**Bot pipeline**

```rust
// Monitor Lido AccountingOracle for member submissions
let oracle_filter = accounting_oracle.event::<ReportSubmitted>()
    .subscribe().await?;

// Track quorum progress
let mut submissions: HashMap<Address, ReportData> = HashMap::new();
while let Some(event) = oracle_filter.next().await {
    submissions.insert(event.member, event.report);
    
    if submissions.len() >= QUORUM_THRESHOLD - 1 {
        // Next submission finalizes → compute the rate change
        let projected_rate = compute_new_rate(&submissions);
        let current_pool_rate = get_steth_curve_price().await?;
        
        if projected_rate > current_pool_rate + ARBIT_THRESHOLD {
            // Buy stETH on Curve (undervalued), hold through rate update
            execute_steth_arb(current_pool_rate, projected_rate).await?;
        }
    }
}
```

**Computable rate delta**

The new wstETH/stETH rate is derivable from the submitted `clBeaconValidators` and `clBalance` fields of each member's report. This allows precise profit modeling before committing capital.

---

### 7.10 Morpho Blue market state transition

| Attribute | Value |
|-----------|-------|
| Profitability | 5/10 |
| Competition | 2/10 |
| Capital required | Medium |
| Complexity | 8/10 |
| Chains | ETH, Base |
| Frequency | Continuous (utilization-triggered) |

**Mechanism**

Morpho Blue uses an `AdaptiveIrm` — an interest rate model that adjusts the borrow rate based on utilization relative to a `targetUtilization` (90%). The rate adjusts exponentially fast when utilization deviates from target, creating sharp rate cliffs.

**Three distinct MEV surfaces**

**1. IRM kink front-run**

When utilization crosses the target threshold, the adaptive rate begins adjusting rapidly. A large borrow that pushes utilization from 88% to 92% triggers the upward rate adjustment. Front-running that borrow with a supply deposit earns the higher post-crossing rate from the first block.

```rust
// Monitor each Morpho Blue market's utilization
let market_state = morpho.market(market_id).call().await?;
let utilization = market_state.totalBorrowAssets.as_u128() as f64
    / market_state.totalSupplyAssets.as_u128() as f64;
    
let target = adaptive_irm.TARGET_UTILIZATION().call().await?; // typically 0.9

if (utilization - target_f64).abs() < 0.02 {
    // Within 2% of kink — monitor for large incoming borrows/repayments
    watch_for_utilization_cross(market_id).await?;
}
```

**2. Bad debt detection and exit**

Morpho Blue socializes bad debt: when a liquidation leaves the borrower with negative equity (collateral < debt), the loss is spread across all suppliers proportionally. Unlike AAVE which has a reserve fund, Morpho has no buffer — bad debt directly reduces `totalSupplyAssets`.

A bot that detects a position approaching bad debt (oracle price tracking) and withdraws supply before the bad debt event avoids the haircut:

```rust
// Track positions at risk: health_factor(real_price) < 1 AND liquidation_incentive < bad_debt_threshold
let oracle_price = morpho_oracle.price().call().await?;
let health_factor = compute_hf(&position, oracle_price);
if health_factor < BAD_DEBT_THRESHOLD {
    // Withdraw supply before bad debt socializes
    morpho.withdraw(market_params, my_supply, 0, receiver, on_behalf).send().await?;
}
```

**3. Morpho-specific oracle latency liquidation**

Morpho Blue markets use single oracles with no circuit breakers or grace periods. When an oracle updates (e.g. Chainlink price deviation), the liquidation window opens immediately — faster than AAVE (which has liquidation guards) or Compound (which has a `liquidateCalculateSeizeTokens` floor). Specialized Morpho liquidation bots can be simpler and faster than generalist liquidators because the protocol has fewer special cases.

---

### 7.11 Uniswap V4 hook MEV

| Attribute | Value |
|-----------|-------|
| Profitability | 7/10 |
| Competition | 2/10 |
| Capital required | Low |
| Complexity | 8/10 |
| Chains | ETH, Arbitrum, Base |
| Frequency | Continuous |

**Mechanism**

Uniswap V4's singleton `PoolManager` allows each pool to attach a hook contract that executes arbitrary logic at eight lifecycle points: `beforeInitialize`, `afterInitialize`, `beforeAddLiquidity`, `afterAddLiquidity`, `beforeRemoveLiquidity`, `afterRemoveLiquidity`, `beforeSwap`, `afterSwap`. Hooks create entirely new MEV surfaces that did not exist in V3.

**Four concrete hook MEV patterns**

**1. TWAMM hook front-run**

Time-Weighted AMM hooks split large orders into per-block mini-swaps executed continuously over a time window. The order size and direction are stored in the hook's state — publicly readable.

```rust
// Read TWAMM hook state: how much accumulated directional flow remains
let twamm_state = twamm_hook.getOrderInfo(pool_id, order_key).call().await?;
let remaining_amount = twamm_state.sellRateContext; // tokens per second still to execute
let direction = twamm_state.zeroForOne; // which direction the pressure is

// If remaining_amount is large and persistent → pre-position in that direction
// The TWAMM will push price in this direction over the remaining blocks
```

**2. Dynamic fee hook arbitrage**

Hooks that adjust fees based on volatility (e.g. increasing fees during high-volatility periods) create a fee-timing arb: execute large swaps in the block immediately before the hook applies elevated fees.

```rust
// Read hook's volatility oracle or fee calculation method
let current_fee = dynamic_fee_hook.getCurrentFee(pool_id).call().await?;
let next_block_fee = dynamic_fee_hook.computeFeeForBlock(pool_id, block + 1).call().await?;

if next_block_fee > current_fee + FEE_JUMP_THRESHOLD {
    // Execute swap now before fee increase applies
    pool_manager.swap(pool_key, swap_params, hook_data).send().await?;
}
```

**3. Limit order hook front-run**

Limit order hooks convert LP positions into market orders when price crosses a specified tick. The activation tick is readable from hook storage, and the conversion from LP position to market order is predictable.

When a large limit order hook position is about to be triggered by incoming order flow:
- Front-run with a small swap to trigger the hook's conversion
- The hook's position clears at a known price
- Backrun with the corrective arb against the resulting price dislocation

**4. Singleton flash accounting MEV**

V4's flash accounting (via `PoolManager.unlock()`) allows chains of operations with net settlement at the end — a generalization of flash loans without the explicit borrow/repay structure. Bots can compose arbitrarily complex multi-pool paths within a single `unlock()` call, with settlement requiring only the net delta. This eliminates the capital overhead of V2/V3 flash swaps for multi-hop paths.

**Hook indexing infrastructure**

```rust
// Index V4 pools by hook address flags
// Hook flags are encoded in the last 3 bytes of the hook address
fn has_before_swap_hook(hook_address: Address) -> bool {
    let flags = hook_address.0[17]; // byte 17 of 20-byte address
    flags & 0x08 != 0 // BEFORE_SWAP flag
}

// Maintain a registry: hook_address → hook_type → MEV_profile
```

---

### 7.12 Convex/Curve gauge vote epoch

| Attribute | Value |
|-----------|-------|
| Profitability | 6/10 |
| Competition | 3/10 |
| Capital required | High |
| Complexity | 7/10 |
| Chains | ETH, Arbitrum |
| Frequency | Bi-weekly (Curve epoch boundary) |

**Mechanism**

Every two weeks, Curve's gauge weight voting system resets and distributes CRV emissions to pools based on accumulated veCRV votes. The emission rate to each gauge changes at the boundary block — effectively repricing all Curve LP positions simultaneously.

**Four MEV layers at the epoch boundary**

**1. Gauge weight prediction from on-chain storage**

Vote counts are finalized on-chain at `VotingEscrow`. The exact next-epoch gauge weights are computable 1–4 days before the boundary from accumulated votes:

```rust
// GaugeController stores vote slope data per address per gauge
let vote_data = gauge_controller
    .vote_user_slopes(voter_address, gauge_address)
    .call().await?;

// Aggregate all active votes (iterate UserVotedEvent historical log)
// → Compute total weight per gauge → predict emission fraction
let my_gauge_weight = aggregate_gauge_votes(all_voters, target_gauge).await?;
let total_weight = gauge_controller.get_total_weight().call().await?;
let predicted_emission_pct = my_gauge_weight as f64 / total_weight as u128 as f64;
```

**2. Pre-epoch LP migration front-run**

When a gauge's weight will increase significantly (e.g. from 5% to 15%), DAOs and large LPs migrate capital into that pool at the epoch boundary. The TVL increase temporarily compresses yield — first entrants earn the highest pre-equilibrium APY.

Strategy: LP into the predicted high-weight gauge 2–5 blocks before the epoch flip, collect the first block of high emissions, exit or hold based on sustainable yield vs capital cost.

**3. CRV emission cliff backrun**

At the epoch flip block, `GaugeController.checkpoint_gauge()` is called for each gauge. This transaction is public and triggers the emission rate update. Backrun with LP deposits into the now-confirmed high-emission gauges.

**4. Bribe protocol coordination**

Bribe protocols (Hidden Hand, Votemarket) distribute bribe rewards to voters in the first block of the new epoch. A bot claiming bribes for multiple wallets simultaneously:

```rust
// Hidden Hand: claimBribes() is callable for multiple token/gauge combinations
let claim_data: Vec<ClaimParam> = voted_gauges.iter()
    .map(|g| ClaimParam { identifier: g.bribe_id, account: my_address, amount, merkle_proof })
    .collect();
bribe_vault.claimBribes(claim_data).send().await?;
```

**Curve Wars context**

The most valuable gauge positions are stablecoin pools (3pool, FRAX/USDC, LUSD/3crv). Protocol-owned liquidity (Frax, Convex, Yearn) predictably concentrates voting here. Modeling their historical vote behavior gives high-confidence gauge weight predictions well before the epoch boundary.

---

---

### 7.13 Trader Joe V2 Liquidity Book

| Attribute | Value |
|-----------|-------|
| Profitability | 7/10 |
| Competition | 2/10 |
| Capital required | Low |
| Complexity | 6/10 |
| Chains | Avalanche C-Chain, Arbitrum |
| Frequency | Continuous |

**Mechanism**

Trader Joe V2's Liquidity Book (LB) AMM replaces Uniswap V3's continuous tick model with discrete, fixed-width **bins**. Each bin has a configurable `binStep` (e.g. 10, 25, or 100 basis points) and behaves internally as a constant-sum AMM. When a swap exhausts one bin, the active bin advances by exactly one step to the next. This creates MEV dynamics that differ fundamentally from V3.

**Bin vs tick: the MEV-relevant differences**

| Property | Uniswap V3 | Trader Joe LB |
|----------|-----------|--------------|
| Price granularity | Continuous (1 bp tick min) | Discrete (fixed `binStep`) |
| Fee distribution | Fractional, by in-range liquidity ratio | 100% of fees → LPs in active bin only |
| Price jump on crossing | Smooth | Step function: exactly `binStep` |
| JIT optimization target | Tick range fraction | Single active bin ID |

**MEV 1: Bin-level JIT liquidity**

In LB, all swap fees for a given swap go to LPs whose positions include the active bin. There is no fractional distribution within the bin — if you are the only LP in the active bin, you capture 100% of fees on the entire swap. This is simpler than V3 JIT: no tick range optimization, no IL calculus across a range. One bin, full fee.

```rust
// Read current active bin ID
let active_bin_id = lb_pair.getActiveId().call().await?;

// Add single-bin concentrated liquidity
let add_params = AddLiquidityParams {
    token_x: ..., token_y: ...,
    bin_step: 20,
    amount_x: ..., amount_y: ...,
    amount_x_min: ..., amount_y_min: ...,
    active_id_desired: active_bin_id,
    id_slippage: 0,                       // exact bin only, no slippage
    delta_ids: vec![0i64],                // only the active bin
    distribution_x: vec![1e18 as u64],   // 100% to active bin
    distribution_y: vec![1e18 as u64],
    to: executor_address,
    deadline: ...,
};
lb_router.addLiquidity(add_params).send().await?;
// After large swap executes: removeLiquidity → collect 100% of bin fees
```

**MEV 2: Bin crossing front-run**

A swap that exhausts the active bin and crosses to the next bin creates a price jump of exactly `binStep`. This jump is deterministic and visible in advance by comparing the incoming swap amount against the active bin's current reserves.

```rust
// Determine if a pending swap will exhaust the active bin
let (reserve_x, reserve_y) = lb_pair.getBin(active_bin_id).call().await?;
let swap_amount_in = decoded_pending_swap.amount_in;

if swap_amount_in > reserve_y {
    // Crossing will occur — deposit into the next bin before the swap
    let target_bin_id = active_bin_id + 1; // for a Y→X buy
    deposit_single_bin(target_bin_id, amount_x, amount_y).await?;
    // After swap crosses, the deposited position is now in the active bin
    // at the post-crossing price; withdraw for the step-function profit
}
```

**MEV 3: AvalancheGo IPC latency advantage**

AvalancheGo exposes a Unix domain socket (IPC) for each chain that publishes accepted transactions at lower latency than HTTP or WebSocket polling. For Avalanche C-Chain MEV, connecting directly to the IPC socket reduces detection latency by 10–50 ms compared to standard RPC — meaningful at the block times Avalanche uses (~2 seconds).

```rust
// Connect to AvalancheGo C-Chain IPC socket
let socket_path = "/tmp/avalanche/{chain_id}.sock";
let mut stream = tokio::net::UnixStream::connect(socket_path).await?;
let mut buf = vec![0u8; 65536];
loop {
    let n = stream.read(&mut buf).await?;
    if n == 0 { break; }
    if let Ok(tx) = decode_avax_tx(&buf[..n]) {
        check_mev_opportunity(tx).await?;
    }
}
```

**Avalanche ecosystem notes**

Pharaoh Exchange on Avalanche is a ve(3,3) fork (same tokenomics as Velodrome/Aerodrome). The same epoch transition strategy from 7.5 applies directly — with near-zero dedicated competition on Avalanche. Joe V2 and Pharaoh together cover the two highest-MEV surfaces on the C-Chain without requiring ETH L1 infrastructure.

**Moat**

Trader Joe LB is Avalanche-native with essentially no dedicated MEV tooling. The bin-level JIT is architecturally simpler than V3 JIT (no range optimization), and the IPC latency advantage compounds this further. The entire Avalanche MEV market is structurally underdeveloped relative to ETH/Arbitrum.

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

### 8.5 Multi-block MEV

| Attribute | Value |
|-----------|-------|
| Profitability | 9/10 |
| Competition | 1/10 |
| Capital required | None |
| Complexity | 9/10 |
| Chains | ETH L1, BSC, Avalanche |
| Frequency | Rare but predictable (slot-scheduled) |

**Mechanism**

When a validator or proposer controls two or more **consecutive block slots**, strategy classes become available that are atomically impossible within a single block. A standard MEV bot is constrained to act within one block's state transition. Multi-block control removes this constraint entirely.

On Ethereum, consecutive slot assignments are probabilistic: with ~500,000 active validators, any given validator rarely holds consecutive slots — but across the full validator set it happens constantly. Block builders with pre-arranged relationships across multiple validators can pool consecutive-slot opportunities.

On BSC, the 21-validator rotation is deterministic and publicly derivable from on-chain state. Adjacent validators in the rotation frequently produce back-to-back blocks. On Avalanche, the Snowman consensus proposer sequence is computable in advance. Both chains make multi-block planning more tractable than ETH.

**Strategy 1: Cross-block sandwich**

Standard single-block sandwiches are observable (MEV-Share tracking, Flashbots SUAVE detection). A cross-block sandwich is structurally invisible to these systems:

```
Block N (controlled):   frontrun tx lands — buy token X, move price
                        [no backrun in same block — looks like a normal buy]

Block N+1 (controlled): victim's large swap lands at natural gas priority
                        → executes at the price already shifted by block N
Block N+1 (controlled): backrun tx — sell token X at elevated price
```

The victim's transaction is not wrapped by two transactions in the same block. It appears to land naturally in N+1. This bypasses all intra-block sandwich detection and is compatible with MEV-protection tools that monitor same-block ordering only.

**Holding risk**: between block N close and block N+1 open (~12s on ETH), price can move adversely. This is a short volatility exposure that must be modeled per position size.

**Strategy 2: TWAP manipulation across blocks**

Single-block TWAP manipulation is expensive: the attacker must hold an extreme price through an entire block while absorbing all corrective arbitrage. Multi-block control removes this — the controller suppresses corrective arb by excluding it from their blocks.

```
Blocks N … N+k: hold extreme price; exclude corrective arb transactions
Block N+k:      exploit the protocol that uses the now-corrupted TWAP
Block N+k+1:    release; corrective arb floods in
```

Protocols vulnerable: any TWAP oracle with a window shorter than the number of controlled consecutive blocks, used as a price source for high-value collateral or liquidation logic.

**Strategy 3: Guaranteed cross-protocol leg execution**

Two-legged strategies across protocols that cannot be atomically composed in a single transaction (different rollups settling to L1, delayed finality):

```
Block N (controlled):   Execute leg A — open position on Protocol X
Block N+1 (guaranteed): Execute leg B — close matching position on Protocol Y
```

In single-block MEV, the risk is that leg A succeeds but leg B fails (adverse price movement or gas spike between submission and execution). Controlling both blocks eliminates this: if leg B would fail in N+1, abandon it and unwind leg A atomically in the same block you control.

**Strategy 4: Cross-block JIT**

```
After block N seals, N+1 mempool becomes visible:
  → Detect large swap pending for N+1

During block N construction (still open):
  → Add JIT liquidity into the active tick/bin targeting the pending swap

Block N+1 (controlled):
  → Large swap executes, accruing fees to our JIT position
  → Remove liquidity + collect fees
```

This pattern bypasses any same-block LP cooldown mechanisms. It also allows targeting swaps that were not visible in the mempool early enough to be included as same-block JIT.

**Slot prediction (ETH)**

```rust
// Query beacon chain proposer duties for the current epoch
let duties: Vec<ProposerDuty> = beacon_client
    .get_proposer_duties(current_epoch)
    .await?
    .data;

// Find consecutive slots where both proposers are known builder partners
let consecutive_windows: Vec<(Slot, Slot)> = duties
    .windows(2)
    .filter_map(|w| {
        let is_consecutive = w[1].slot == w[0].slot + 1;
        let both_partners = is_partner_validator(w[0].validator_index)
                         && is_partner_validator(w[1].validator_index);
        (is_consecutive && both_partners).then(|| (w[0].slot, w[1].slot))
    })
    .collect();

// Schedule multi-block strategies for each window
for (slot_n, slot_n1) in consecutive_windows {
    schedule_multiblock_opportunity(slot_n, slot_n1).await?;
}
```

**BSC execution note**

48Club validators rotate in a deterministic sequence. Two adjacent validators in the rotation produce consecutive blocks on a regular cycle. A searcher with relationships to two adjacent 48Club validators has a reliable, recurring source of multi-block windows — predictable days in advance from the validator rotation schedule.

**Why this is underexplored**

Most MEV research assumes single-block atomicity as a hard constraint. Multi-block MEV is discussed theoretically but has almost no dedicated tooling or public research. The beacon chain API makes slot prediction trivial. The main barrier is establishing validator relationships — which is a business development problem, not a technical one.

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
| 44 | Velodrome/Aerodrome epoch transition | 6/10 | 2/10 | Medium | 6/10 | Weekly |
| 45 | Pendle PT/YT implied yield spread | 6/10 | 2/10 | Medium | 7/10 | Daily |
| 46 | Balancer rate provider staleness | 6/10 | 3/10 | Medium | 7/10 | Daily |
| 47 | GMX V2 ADL front-run | 7/10 | 2/10 | Low | 7/10 | Rare |
| 48 | Lido oracle report front-run | 7/10 | 3/10 | Medium | 7/10 | Daily |
| 49 | Morpho Blue market state transition | 5/10 | 2/10 | Medium | 8/10 | Continuous |
| 50 | Uniswap V4 hook MEV | 7/10 | 2/10 | Low | 8/10 | Continuous |
| 51 | Convex/Curve gauge vote epoch | 6/10 | 3/10 | High | 7/10 | Bi-weekly |
| 52 | Trader Joe V2 Liquidity Book | 7/10 | 2/10 | Low | 6/10 | Continuous |
| 53 | Multi-block MEV | 9/10 | 1/10 | None | 9/10 | Rare |

**Opportunity score** = `(Profitability × 2) - Competition` — a rough measure of risk-adjusted opportunity for a new entrant:

| Rank | Strategy | Opportunity Score |
|------|----------|:-----------------:|
| 1 | Multi-block MEV | 17 |
| 2 | Cascading liquidation engineering | 18 |
| 3 | GMX V2 ADL front-run | 12 |
| 3 | Uniswap V4 hook MEV | 12 |
| 5 | MakerDAO OSM preview | 15 |
| 5 | JIT + arb combo | 15 |
| 7 | Lido oracle report front-run | 11 |
| 7 | Trader Joe V2 Liquidity Book | 12 |
| 9 | Velodrome/Aerodrome epoch transition | 10 |
| 9 | Pendle PT/YT implied yield spread | 10 |
| 11 | Liquity recovery mode cascade | 13 |
| 12 | Interest accrual liquidation | 8 → competition 2; best **low-barrier, zero-capital** entry |
| 12 | Morpho Blue state transition | 8 → competition 2; niche-specific, low barrier |

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
| Avalanche C-Chain | No native private relay; AvalancheGo IPC for local mempool latency | Node-level only |

---

## 11. Implementation Roadmap

The document answers *what* but not *what first*. This section is a practical decision tree for sequencing strategy implementation based on capital availability, target chain, and infrastructure maturity. The goal is to reach first production revenue as fast as possible while building incrementally toward higher-complexity strategies.

### Decision axes

**Capital tier**

| Tier | Capital range | Flash loan viable | Unlocks |
|------|-------------|:-----------------:|---------|
| Zero | None | Yes | Keeper rewards, skim(), flash-only liquidations |
| Low | < $10k | Yes | Most liquidations, long-tail arb, token snipe |
| Medium | $10k–$200k | Yes, supplements | JIT, stat arb, solver, Pendle, epoch strategies |
| High | > $200k | Supplements | CEX-DEX, JIT+arb, cascading liquidations |

**Infrastructure stage**

| Stage | What exists |
|-------|------------|
| 0 | Nothing — starting from scratch |
| 1 | alloy provider + revm simulation + basic event listener running |
| 2 | redb storage + executor contract + bundle submission + block scheduler |
| 3 | Multi-strategy production: monitoring dozens of pools/positions across multiple chains |

---

### Phase 0 — Foundation (any capital level, Stage 0 → 1)

Build this before any strategy. Everything else depends on it.

```
1. alloy WebSocket provider + HTTP fallback
2. revm fork-at-block simulation (test any strategy offline before risking capital)
3. redb storage (pool registry, position registry, token registry)
4. basic event subscription: PairCreated, PoolCreated, Swap, Transfer, Borrow
5. gas estimator + profitability threshold check
6. executor contract (minimal: flash loan callback + call-anything interface)
```

**First revenue target (zero capital, Stage 1)**

Start with exactly these two — nothing else. They validate the entire pipeline with near-zero risk.

| Strategy | Why first | Estimated build time |
|----------|-----------|---------------------|
| `skim()` capture | One read + one write. Validates provider, executor contract, and gas model. | 1–2 days |
| Interest accrual liquidation | No reactivity required — pure block scheduling. Competition ≈ 0. | 3–5 days |

Do not build anything from Phases 1–4 until these two are generating real (even tiny) revenue. The pipeline validation is more valuable than the revenue.

---

### Phase 1 — Capital-free production (Stage 1 → 2)

After Phase 0 is confirmed working with live revenue:

**Chain-agnostic (build in this order)**

| Strategy | Additional infra needed | Notes |
|----------|------------------------|-------|
| Flash loan liquidation | Executor contract + flash routing | Balancer (0% fee) first, then AAVE |
| Backrunning | Bundle submission to private relay | Start with MEV-Share on ETH for rebate access |
| MakerDAO OSM preview + `kick()` | Vault health DB + OSM storage slot reader | ETH L1 only; very low competition |
| Synthetix flag + delayed liquidation | Block scheduler | Optimism; moat = scheduling precision, not speed |
| GMX v1/v2 keeper | Position indexer + keeper contract | Arbitrum + Avalanche; immediate revenue |

**Do not build yet**: sandwich, CEX-DEX, JIT, cascading liquidations, multi-block MEV. These require either capital, co-location, or infrastructure maturity that only comes in Phase 2+.

---

### Phase 2 — Chain-specific expansion (Stage 2)

Once Phase 1 is stable, expand per target chain. Build one chain to production before starting the next.

**Avalanche C-Chain**
```
Priority: highest — lowest competition of any chain in this document.

1. AvalancheGo IPC connector (10–50ms latency advantage over RPC)
2. Trader Joe V2 Liquidity Book JIT (7.13) — bin-level JIT, no tick optimization needed
3. GMX v1 keeper race (4.11) — Avalanche deployment; less competition than Arbitrum
4. Pharaoh Exchange epoch transition — same mechanics as Aerodrome (7.5) on a fresh chain
5. Long-tail arb (SPFA across Joe V2 + Pharaoh pools)
```

**Base**
```
1. Aerodrome epoch transition (7.5) — zero dedicated competition as of writing
2. Token launch snipe with revm safety check (8.4)
3. Uniswap V4 hook MEV (7.11) — V4 is live on Base; hook ecosystem is forming now
4. Backrunning via priority fee (no private relay available)
```

**Arbitrum**
```
1. GMX V2 ADL front-run (7.8) — keeper reward + directional pre-positioning
2. Backrunning (FCFS — no sandwich possible; pure clean backrun market)
3. Pendle PT/YT spread (7.6) — Arbitrum is Pendle's primary deployment
4. Batch auction MEV / CoW solver (8.2)
```

**Polygon**
```
1. Long-tail arb (SPFA across QuickSwap V3 + SushiSwap + Uniswap V3)
2. Oracle-latency liquidation on AAVE V3 Polygon (4.3) — active market
3. bloXroute BDN integration for private bundle submission
```

**BSC**
```
1. 48Club application (required for private bundles — apply early, takes time)
2. Token launch snipe on PancakeSwap V3 PairCreated events (8.4)
3. Flash loan liquidation on Venus Protocol (BSC's dominant lending market)
4. Sandwich via 48Club (only after access confirmed)
```

---

### Phase 3 — Capital-intensive strategies (Stage 2, Medium+ capital)

Only add these after Phase 2 strategies are stable and generating consistent revenue:

| Strategy | Capital needed | Why it's Phase 3 |
|----------|:---:|-----------------|
| JIT liquidity (V3) | High | Requires tick range optimization + high gas; simulate extensively first |
| Pendle PT/YT (7.6) | Medium | Requires Pendle UAMM understanding; build dedicated simulation module |
| Lido oracle front-run (7.9) | Medium | Daily cadence — time to build correctly; no rush |
| Velodrome/Aerodrome epoch (7.5) | Medium | Weekly cadence; pre-position requires capital sitting idle |
| Stablecoin depeg arb (5.1) | High | Event-driven; pre-position capital months in advance; model the scenarios |

---

### Phase 4 — Full-spectrum (Stage 3, High capital + validator access)

Only worth pursuing once Phases 1–3 are operating with reliable revenue:

| Strategy | Why it's last | Path |
|----------|-------------|------|
| Cascading liquidation engineering | Highest ceiling; builds on all previous liquidation work | Extend Phase 1 flash liquidator with cross-protocol dependency graph |
| JIT + arb combo | Natural V3 JIT extension | Add arb leg to Phase 3 JIT work |
| Multi-block MEV (8.5) | Requires validator relationships | Build beacon chain slot watcher; establish BSC 48Club validator contacts first |
| PBS / block building | Not a solo-builder strategy; capital + team overhead | Consider only if targeting ETH L1 professionally at scale |

---

### Strategies to deprioritize permanently

| Strategy | Reason |
|----------|--------|
| Top-tier CEX-DEX arb (ETH L1) | Jump/Wintermute hold infrastructure moat; not a strategy problem |
| Sandwich on ETH L1 | MEV-Share and SUAVE are structurally compressing this market |
| TWAP oracle manipulation | Adversarial; community hostile; legal exposure |
| NFT floor arbitrage | Illiquid; wash-trading distorts floor data; operational complexity high |
| Governance MEV | Requires monitoring all governance forums across protocols; low frequency |

---

### Infrastructure build priority

If forced to sequence what to build in strict order:

```
1. revm simulation          — the multiplier: makes every strategy safer and offline-testable
2. redb pool + position DB  — the foundation: all strategies read from this
3. Flash loan executor       — unlocks all capital-free liquidation strategies
4. Bundle submission layer  — unlocks ordering strategies (backrun, sandwich, JIT)
5. Block scheduler          — unlocks time-gated strategies (interest accrual, Synthetix, OSM)
6. AvalancheGo IPC          — unlocks latency edge on Avalanche; highest ROI per hour of infra work
7. Beacon chain slot watcher — unlocks multi-block MEV; build last
```

---

*Document covers 53 strategies across 11 sections: V2 pool mechanics, order flow, bundle/positional strategies, liquidations, oracle/rebase/peg, cross-domain, protocol niches (expanded), emerging, master rankings, cross-cutting infrastructure, and implementation roadmap.*

