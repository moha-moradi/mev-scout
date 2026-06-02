# Phase 5: Multi-DEX Arbitrage Engine (V2+V3+Curve+Balancer)

**Builds on**: Phase 4 (TwoHopArb V2 Detection) + our current work: V3 quoting, tick-traversal, mixed-DEX arb detection, V3 init_from_rpc.

**Goal**: Complete the full multi-DEX price graph pipeline so the backtester can detect two-hop arbitrage across any combination of V2, V3, Curve, and Balancer pools on Polygon.

---

## Status Legend

- ✅ Done
- ◐ Partially done (needs follow-up)
- ⬜ Not started

---

## Phase 5.1 — Pool Discovery & Verification (⬜)

### 5.1.1 `pool/thegraph.rs` — Subgraph client

Query TheGraph for pools sorted by volume/TVL per DEX type.

| Item | Status | Details |
|------|--------|---------|
| GraphQL client module | ⬜ | HTTP POST to subgraph URL, parse JSON response |
| V2 pools query | ⬜ | `pools(first: 1000, orderBy: volumeUSD, orderDirection: desc) { id token0 { id } token1 { id } feeTier }` |
| V3 pools query | ⬜ | Same shape; extract tickSpacing |
| Curve pools query | ⬜ | Curve registry subgraph |
| Balancer pools query | ⬜ | Balancer subgraph (poolId, tokens, weights) |
| Rate limiting | ⬜ | Polite delay between queries; retry on 429 |
| Output: `Vec<RawPoolEntry>` | ⬜ | Unified struct before factory filtering |

**Depends on**: nothing new.

### 5.1.2 `pool/factory_verify.rs` — On-chain verification

For each candidate pool from TheGraph, call the canonical factory's `getPool(token0, token1, feeTier)` (V3) or `getPair(token0, token1)` (V2) to confirm the pool exists and matches the expected address.

| Item | Status | Details |
|------|--------|---------|
| Uniswap V2 `getPair()` call | ⬜ | Selector `0xe6a43905` — returns pair address |
| Uniswap V3 `getPool()` call | ⬜ | Selector `0x1698ee82` — returns pool address |
| Curve pool validation | ⬜ | Check pool exists in Curve registry |
| Balancer pool validation | ⬜ | Check poolId exists in Balancer Vault |
| Factory address config | ⬜ | Hardcoded in `config.rs` (see `config.rs:162-271`) |
| Batch verification | ⬜ | Parallel eth_call with semaphore (reuse pattern from `init_from_rpc`) |
| Minimum count enforcement | ⬜ | Keep only top N pools that pass verification (V2≥100, V3≥100, Curve≥20, Balancer≥20) |

**Depends on**: 5.1.1 (candidates to verify).

### 5.1.3 Pool registry generation

Script or CLI subcommand to query TheGraph → verify on-chain → write `pools/polygon_{v2,v3,curve,balancer}.json`.

| Item | Status | Details |
|------|--------|---------|
| `fetch-pools` CLI subcommand | ⬜ | `mev-backtest fetch-pools --chain polygon` |
| V2 registry (≥100 pools) | ⬜ | `pools/polygon_v2.json` |
| V3 registry (≥100 pools) | ⬜ | `pools/polygon_v3.json` |
| Curve registry (≥20 pools) | ⬜ | `pools/polygon_curve.json` |
| Balancer registry (≥20 pools) | ⬜ | `pools/polygon_balancer.json` |
| Merge all into unified loader | ⬜ | `PoolRegistry::load_all()` reads all four files |
| QuickSwap factory (no code) workaround | ◐ | Factory `0x575737…9a819` has no code on Polygon — need alternate discovery (manual list, alternate factory, or PairCreated event scanning) |

**Depends on**: 5.1.1 + 5.1.2.

---

## Phase 5.2 — Quoting & Pricing (◐)

### 5.2.1 `quote_path()` — Chained multi-hop quoting

Utility to compute the output of a two-hop path where each hop can be a different DEX type.

| Item | Status | Details |
|------|--------|---------|
| `quote_path(pool_a, pool_b, amount_in, shared_token) -> Option<u128>` | ⬜ | Dispatches V2→V2, V2→V3, V3→V2, V3→V3, etc. |
| Reuse existing `check_direction` dispatch | ⬜ | The pattern in `two_hop.rs:80-113` already does this — extract to a standalone fn |
| Edge case: zero liquidity / full slippage | ⬜ | Return None when any hop reverts |

**Depends on**: nothing new — can be extracted from existing `two_hop.rs` code.

### 5.2.2 Curve quoting

| Item | Status | Details |
|------|--------|---------|
| Curve stable-swap invariant `(D, A, n)` | ⬜ | `compute_d()`, `get_y()` from whitepaper |
| `quote_curve_exact_in(pool, amount_in, i, j) -> Option<u128>` | ⬜ | Fee + admin fee |
| Curve crypto (v2) quoting | ⬜ | Different invariant (geometric mean + price oracle) |

**Depends on**: 5.2.1 (to integrate into quote_path).

### 5.2.3 Balancer quoting

| Item | Status | Details |
|------|--------|---------|
| Weighted pool invariant `Π(token_i^w_i) = const` | ⬜ | `compute_out_given_in()` |
| `quote_balancer_exact_in(pool, amount_in, token_in, token_out) -> Option<u128>` | ⬜ | Fee + swap fee |

**Depends on**: 5.2.1.

### 5.2.4 Historical USD prices from CSV

| Item | Status | Details |
|------|--------|---------|
| CSV format spec | ⬜ | `date,token_address,price_usd` (daily snapshots) |
| `historical_prices.rs` | ⬜ | `HistoricalPriceDB::load(path)`, `get_price(token, block_timestamp)` |
| Linear interpolation | ⬜ | Between daily snapshots for any block timestamp |
| Fallback to hardcoded prices | ⬜ | Use existing `pricing.rs` map when CSV missing |

**Depends on**: nothing new.

---

## Phase 5.3 — Accuracy Testing (⬜)

### 5.3.1 Reserve sync accuracy

| Item | Status | Details |
|------|--------|---------|
| Compare `update_from_logs()` reserves vs `eth_call` on 5+ consecutive blocks | ⬜ | Test that replay-driven reserves match RPC snapshots |
| V2 Sync event coverage | ⬜ | Ensure Sync events (which are authoritative) correctly override Swap-based deltas |
| V3 Swap event coverage | ⬜ | Ensure V3 Swap events correctly update sqrtPriceX96/tick/liquidity |

### 5.3.2 V3 quote accuracy

| Item | Status | Details |
|------|--------|---------|
| Compare `quote_v3_exact_in` vs real V3 Quoter contract on 3+ large swaps | ⬜ | Forge/geth fixture or recorded RPC data |
| Accuracy threshold | ⬜ | Error < 0.1% for swaps up to 10% of pool liquidity |

### 5.3.3 Full pipeline test

| Item | Status | Details |
|------|--------|---------|
| Known arb block test | ⬜ | Find a real two-hop arb tx on Polygon, verify the detector finds the same opportunity |
| Gas cost correctness | ⬜ | Compare simulated gas vs actual tx gas used |

**Depends on**: 5.1.3 (pool registry), 5.2.1+ (quoting), 5.2.4 (prices).

---

## Phase 5.4 — Flash Loan Simulation (⬜)

### 5.4.1 `FlashLoanWrapper`

| Item | Status | Details |
|------|--------|---------|
| Auto mode (Balancer→Aave→Uniswap fallback chain) | ⬜ | `FlashLoanWrapper::new_auto()` — tries providers in order |
| Forced provider (no fallback) | ⬜ | `FlashLoanWrapper::new_forced(provider)` |
| Borrow / repay interface | ⬜ | `borrow(token, amount) -> Result<()>`, `repay(token, amount) -> Result<()>` |
| Aave V3 flash loan provider | ⬜ | `0x…` on Polygon, premium calculation |
| Balancer V2 flash loan provider | ⬜ | Balancer Vault flash loan callback pattern |
| Uniswap V2/V3 flash swap provider | ⬜ | `swap()` with `amount0Out` / data pattern |

**Depends on**: pool registry (5.1.3) to find Aave/Balancer flash loan contracts.

### 5.4.2 `TwoHopArbSimulator`

| Item | Status | Details |
|------|--------|---------|
| Forked revm state | ⬜ | Clone state at block N, apply flash loan + arb swaps |
| Binary search for optimal input (≤10 iterations) | ⬜ | Similar to ternary search but with revm simulation for exact output |
| Validate no revert | ⬜ | Transaction succeeds end-to-end |
| Output: exact profit after gas | ⬜ | Including actual gas used by revm |

**Depends on**: 5.4.1.

---

## Phase 5.5 — JIT & Sandwich Readiness (⬜)

These strategies parse but detect nothing (per AGENTS.md). Stub out the detector scaffolding to avoid dead code warnings.

| Item | Status | Details |
|------|--------|---------|
| JIT detection stub | ⬜ | `JitDetector::detect()` — logs "not implemented", returns empty vec |
| JIT+Arb detection stub | ⬜ | Same |
| Sandwich detection stub | ⬜ | Same |
| Multi-hop arb (>2 hops) stub | ⬜ | Same |

**Depends on**: nothing.

---

## Implementation Order (Recommended)

```
Phase 5.1: Pool Discovery
  ├── 5.1.1 TheGraph client (foundation for pool lists)
  ├── 5.1.2 Factory verification (gate quality)
  └── 5.1.3 Registry generation (output)

Phase 5.2: Quoting
  ├── 5.2.1 quote_path() (extract from existing code — quick win)
  ├── 5.2.2 Curve quoting (new invariant math)
  ├── 5.2.3 Balancer quoting (new invariant math)
  └── 5.2.4 Historical prices CSV (independent)

Phase 5.3: Accuracy Tests
  ├── 5.3.1 Reserve sync (validate replay correctness)
  ├── 5.3.2 V3 quote accuracy (validate quoting engine)
  └── 5.3.3 Full pipeline test (end-to-end validation)

Phase 5.4: Flash Loan Simulation
  ├── 5.4.1 FlashLoanWrapper
  └── 5.4.2 TwoHopArbSimulator

Phase 5.5: Strategy Scaffolds
  └── JIT / Sandwich / Multi-hop stubs
```

---

## Quick Wins (can be done in parallel, no dependencies)

- 5.2.1 `quote_path()` — extract dispatch logic from `two_hop.rs` to a standalone public fn.
- 5.2.4 Historical prices CSV — independent file format + loader.
- 5.5 Strategy stubs — 4 small fns, structural only.

## Key Technical Details

- **TheGraph API key**: Use free tier (1000 req/s). Bundle default key or read from env `THEGRAPH_API_KEY`.
- **Factory abis**: GetPair (V2): `0xe6a43905`, GetPool (V3): `0x1698ee82`.
- **Flash loan premiums**: Aave V3 = 0.05%, Balancer = 0%, Uniswap V2/V3 = 0% (flash swap).
- **Binary search for sim**: Narrow to ±0.1% in ≤10 iterations. Use revm `transact()` return value for exact output.
- **CSV price format**: Date as unix timestamp or `YYYY-MM-DD`, token as lowercase hex with `0x` prefix.
