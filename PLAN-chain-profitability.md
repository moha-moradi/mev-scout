# Chain Profitability Ranking Plan ($60/month VPS Budget)

## Context

- **$60/month** = VPS cost only (runs our own RPC node for ONE chain + MEV bot)
- **Goal**: Rank all 7 chains (Polygon, Ethereum, BSC, Arbitrum, Avalanche, Base, Optimism) by profit margin
- **Other chains**: Use free public RPCs (publicnode.com) for lightweight screening

## 1. Which Chains Can Run a Full Node on $60 VPS?

| Chain | Archive Size | RAM Needed | Fits $60 VPS? | Notes |
|-------|-------------|------------|---------------|-------|
| **Ethereum** | ~2TB+ | 32GB+ | ❌ Too large | Archive alone exceeds $60 VPS storage |
| **Polygon** | ~500-800GB | 8-16GB | ✅ Yes | Good candidate |
| **BSC** | ~500-800GB | 8-16GB | ✅ Yes | Good candidate |
| **Arbitrum** | ~400-600GB | 8-16GB | ✅ Yes | Good candidate |
| **Optimism** | ~250-400GB | 8-16GB | ✅ Yes | Lightest L2 |
| **Base** | ~200-300GB | 8-16GB | ✅ Yes | Light |
| **Avalanche** | ~150-250GB | 8-16GB | ✅ Yes | Lightest overall |

**Recommended VPS specs** at ~$60/month: Hetzner AX42 (~$40/mo), Netcup RS 2000 (~$30/mo), or similar — 6+ cores, 32GB RAM, 1TB+ NVMe.

## 2. Phased Approach

### Phase 0 — RPC Feasibility Research (1-2 days)
Research what each chain requires to run a full node:
- Storage growth rate per month (affects sustainability)
- Pruning options (full vs archive)
- CPU/RAM requirements
- Sync time

### Phase 1 — Lightweight Screening Across All 7 Chains (1 week, $0 RPC cost)
**Use free publicnode.com RPCs** to run a lightweight comparison:
- **500 sampled blocks per chain**
- **TwoHopArb only** (most common MEV, ~70%+ of volume)
- Skip Curve/Balancer (known C1-C3 bugs, minor impact)
- Accept C4-C6 caveats (document them)

**What we learn**: Opportunity density (ops/block), average profit per opportunity, rough ranking.

**Cost**: $0 additional (free RPCs). The $60 VPS isn't even needed yet — could run on local or a $10 VPS.

### Phase 2 — Pick Top Chain & Run Full Node (Month 1)
Based on Phase 1 results, pick the best chain and:
1. Spin up a $60/month VPS with enough storage
2. Sync a full node for that chain (takes 1-3 days)
3. Run the MEV bot against the local node (no RPC costs)
4. Run a deep backtest: 10k-100k blocks, all relevant strategies
5. Fix **C4** (flash loan fees) and **C5** (profit normalization) for accurate estimates

**Cost**: $60/month VPS ✅

### Phase 3 — Rotate & Compare Monthly
Each month:
1. Keep the $60 VPS running the MEV bot on the current best chain
2. Use the VPS's spare capacity to re-scan other chains via free RPCs (Phase 1 light scan)
3. If another chain overtakes, re-sync the node to the new chain (1-3 day downtime)
4. Build a running comparison table

**Cost**: Still $60/month VPS ✅

## 3. What to Fix Before Accurate Comparison

### Must-fix (high impact, low effort):
| Bug | Effort | Impact | File |
|-----|--------|--------|------|
| **C4**: Flash loan fees not subtracted | ~30 min | Overestimates profit on Aave/Uni by 9-10bps | `two_hop.rs`, `multi_hop.rs` |
| **C5**: Profit in token_out vs gas in native wei | ~1-2 hours | Can mis-rank chains where profit token ≠ native | `run.rs:536` filter, `opportunity.rs` |
| `priority_fee_gwei = 0` default | ~5 min | Currently warns but doesn't prevent overestimation | `run.rs:60` |

### Nice-to-fix:
| Bug | Effort | Impact | Notes |
|-----|--------|--------|-------|
| **H10**: PGA calibration per chain | ~2-3 hours | Better competition modeling | Need searcher data per chain |
| Gas limits per strategy (H7) | ~1 hour | Accurate gas cost per opportunity | Currently hardcoded estimates |

### Accept as caveats (too much effort for comparison):
- **C1-C3**: Curve/Balancer math — skip for now, minor MEV share
- **C6**: Pre-tx vs post-tx detection — systematic bias, all chains same
- Full V3 tick data — only affects V3 pools on some chains

## 4. Cost-Benefit Per Chain (Initial Hypothesis)

| Chain | MEV Activity | Node Cost to Run | Gas Cost | Competition | Estimated ROI Rank |
|-------|-------------|------------------|----------|-------------|-------------------|
| BSC | Very High (PancakeSwap) | Medium (500GB) | Low | High | **#1** |
| Polygon | High (QuickSwap) | Medium (500GB) | Low | Medium-High | **#2** |
| Ethereum | Highest | Too expensive | Very High | Extreme | N/A (can't afford) |
| Arbitrum | Medium-High | Medium (400GB) | Very Low | Low-Medium | **#3** |
| Base | Medium | Low (250GB) | Very Low | Low | **#4** |
| Optimism | Medium | Low (250GB) | Very Low | Low | **#5** |
| Avalanche | Low-Medium | Lowest (150GB) | Low | Low | **#6** |

## 5. First Actions

1. **Research** RPC node requirements per chain (storage, RAM, sync time)
2. **Pick a $60 VPS** (Hetzner AX42 ~€37/mo, or similar with 1TB+ NVMe)
3. **Run Phase 1** (lightweight screening) immediately — can be done locally or on a cheap $5-10 VPS using publicnode.com
4. **Fix C4 + C5** (~1-2 hours work) before running the real comparison
5. **Based on Phase 1 results**, decide which chain to sync a full node for

## 6. Open Questions for You

1. Do you already have a preferred VPS provider? (Hetzner, Netcup, Contabo, others?)
2. Do you want to run Phase 1 (light screening) immediately using free RPCs while we figure out the VPS?
3. How soon do you want the first ranking results? (Phase 1 can be done in a day)
