# MEV Strategies: Implementation & Execution Analysis

> **53 strategies across 8 categories** — ranked, capital-mapped, income-estimated, and codebase-status-tracked.
> Generated from `mev_strategies_complete_v2.md` analysis.

---

## Table of Contents

1. [Master Rankings by Implementation Difficulty](#1-master-rankings-by-implementation-difficulty)
2. [Capital-Free Strategies Inventory](#2-capital-free-strategies-inventory)
3. [Best "First $" by Operator Profile](#3-best-first--by-operator-profile)
4. [Frequency vs Income Heatmap](#4-frequency-vs-income-heatmap)
5. [Capital-Efficiency Opportunity Score](#5-capital-efficiency-opportunity-score)
6. [Strategies to Deprioritize Permanently](#6-strategies-to-deprioritize-permanently)
7. [Codebase Implementation Status](#7-codebase-implementation-status)

---

## 1. Master Rankings by Implementation Difficulty

### Tier 1 — Trivial Build (Complexity 2–3/10)

Zero or near-zero infrastructure. Validates provider, executor contract, and gas model.

| # | Strategy | Cat. | Cpx | Capital | Capital-Free? | Opps/Mo | Est. Monthly Income | Competition | Profitability | Overview | Competitive Edge |
|---|----------|------|:---:|---------|:---:|:---:|---:|:---:|:---:|----------|------------------|
| 1 | sync() race | V2 | 2/10 | None | None needed | 30–200 | $0–$50 | 3/10 | 3/10 | Front-runs or back-runs `sync()` calls on V2 pools that rebase or accumulate rewards, capturing small token amounts during reserve updates. | Three use cases: (a) defensive burn — call `sync()` before a competitor's `skim()` to destroy the opportunity for both; (b) post-rebase-down correction — rebase decreases `balance < reserve`, `sync()` corrects stale price and opens a subsequent arb; (c) precondition — correct reserves before an arb path becomes profitable. Trivial to build; value is in the follow-on strategy, not `sync()` itself. |
| 2 | skim() capture | V2 | 3/10 | None | None needed | 100–500 | $50–$500 | 4/10 | 6/10 | Captures small token amounts when a pool's `balanceOf` drifts above its stored `reserve`, typically from fees, FoT tokens, or rounding. | Scan all V2 pools for `balanceOf > reserve` discrepancies; first to call `skim()` wins. Build a persistent balance-tracker keyed by pair address for sub-ms detection. Primary trigger: rebase tokens (AMPL, stETH) that increase `balanceOf` without updating `reserve` — see Rebase Token Arb (§5.3) for the complementary strategy. |

### Tier 2 — Simple Single-Action (Complexity 4/10)

One read + one write, event-triggered or scheduled.

| # | Strategy | Cat. | Cpx | Capital | Capital-Free? | Opps/Mo | Est. Monthly Income | Competition | Profitability | Overview | Competitive Edge |
|---|----------|------|:---:|---------|:---:|:---:|---:|:---:|:---:|----------|------------------|
| 3 | Init price snipe | V2 | 4/10 | Low | Yes (flash swap) | 30–200 | $100–$2K | 5/10 | 7/10 | Front-runs new liquidity pool creation to set an advantageous initial price by being the first swap. | Monitor factory `PairCreated` events in mempool; submit add-liquidity + first-swap bundle with high gas. Flash swap eliminates need for capital. |
| 4 | Backrunning | OrderFlow | 4/10 | Low | Partial (flash works, speed wins) | 1K–5K | $500–$5K | 6/10 | 7/10 | Places a transaction immediately after a large pending swap to capture the resulting price dislocation via arbitrage. | Parse mempool for large swaps; simulate post-swap state via `eth_call`; submit atomic backrun tx that profits from the temporary imbalance. Latency to block builder is key. |
| 5 | Rebase token arb | Oracle | 4/10 | Low | None needed (balance drift) | 30–90 | $50–$500 | 3/10 | 5/10 | Arbitrage between rebase-token pools and their oracle-reported or DEX-implied values after balance updates. | Track rebase mechanics per token; detect when pool price diverges from rebase-adjusted fair value. Balance drift is passive — no capital needed. |
| 6 | Fee-on-transfer token arb | Oracle | 4/10 | Low | Partial (flash viable) | 200–800 | $50–$300 | 3/10 | 4/10 | Arbitrage between pools of fee-on-transfer tokens where the fee creates a persistent price wedge. | Model exact fee deduction per token transfer; route through pools that minimize fee impact. Flash loans viable if fee structure allows profitable round-trip. |
| 7 | Airdrop MEV | Protocol | 4/10 | None | None needed | 5–30 | $100–$5K | 6/10 | 5/10 | Claims or front-runs airdrop distributions by submitting transactions that interact with claim or distribution contracts. | Monitor governance/airdrop contracts for claim functions; batch multiple claims in one tx. Early detection of claim windows is the moat. |
| 8 | GMX v1 keeper race | Liquidation | 4/10 | None | None needed | 200–500 | $200–$1K | 8/10 | 5/10 | Liquidates undercollateralized GMX v1 positions by competing in the keeper race for liquidation rewards. | Build a position health-factor indexer; monitor positions approaching liquidation threshold; submit liquidation tx with competitive gas. Competition is fierce — speed + gas optimization critical. |

### Tier 3 — Moderate Build (Complexity 5/10)

Multi-step logic, flash loans, or basic simulation.

| # | Strategy | Cat. | Cpx | Capital | Capital-Free? | Opps/Mo | Est. Monthly Income | Competition | Profitability | Overview | Competitive Edge |
|---|----------|------|:---:|---------|:---:|:---:|---:|:---:|:---:|----------|------------------|
| 9 | Flash swap arbitrage | V2 | 5/10 | None | Yes (the mechanism) | 500–3K | $500–$5K | 7/10 | 7/10 | Uses Uniswap V2 flash swaps to execute multi-pool arbitrage in a single atomic tx with zero upfront capital. | Optimize routing across 2–4 hop paths; model slippage and gas precisely. Balancer/TraderJoe flash swaps complement V2. Speed of path computation and gas optimization are the moat. |
| 10 | Long-tail token arb | OrderFlow | 5/10 | Low | Yes (flash loan) | 1K–10K | $300–$2K | 3/10 | 6/10 | Arbitrage opportunities in low-cap, low-liquidity token pools where price discovery is inefficient. | Use SPFA (Shortest Path Fast Algorithm) for efficient path discovery across obscure pool graphs. Pre-compute pool relationships; detect imbalances before others. Low competition due to complexity of pool graph traversal. |
| 11 | Sandwich attack | Bundle | 5/10 | Medium | No (needs inventory, multi-tx) | 500–5K | $1K–$10K | 8/10 | 7/10 | Front-runs and back-runs victim transactions to extract value from their slippage. | Optimize gas pricing per position in bundle; calculate optimal front-run amount to maximize extraction without pricing victim out. Multi-victim batching amortizes base gas. Declining on ETH L1 due to MEV-Share. |
| 12 | V3 range order snipe | Bundle | 5/10 | Low | No (needs pre-positioned LP) | 100–500 | $200–$1.5K | 4/10 | 6/10 | Captures range orders (limit orders via concentrated liquidity) when price reaches the target tick. | Monitor pending swaps that will push price through range-order ticks; pre-position LP at target tick range. Requires understanding of V3 tick math and fee accumulation mechanics. |
| 13 | Perp protocol keeper | Liquidation | 5/10 | None | None needed | 200–800 | $200–$1K | 7/10 | 5/10 | Liquidates or settles undercollateralized perpetual protocol positions for keeper rewards. | Build protocol-specific health-factor indexers; monitor positions approaching threshold. Different perps protocols (Perp, Kwenta, Synthetix) have different liquidation mechanics — specialization wins. |
| 14 | Synthetix flag + delayed liq | Liquidation | 5/10 | Medium | Partial (flash for liq step) | 30–120 | $100–$500 | 4/10 | 6/10 | Flags Synthetix positions for liquidation, then executes after the mandatory delay period. | Track flag events on-chain; calculate optimal liquidation timing post-delay. Flash loans viable for the actual liquidation step. Low competition due to multi-step complexity. |
| 15 | Liquity stability pool front-run | Liquidation | 5/10 | Medium | No (needs pre-deposited LUSD) | 20–80 | $100–$1K | 5/10 | 6/10 | Front-runs stability pool deposits or withdrawals during liquidation events to capture discounted ETH. | Monitor liquidation queue; predict stability pool impact on ETH/LUSD price. Pre-deposited LUSD is required — capital-intensive but predictable returns during market stress. |
| 16 | Interest accrual liquidation | Liquidation | 5/10 | Low | Yes (flash loan) | 100–500 | $200–$1K | 2/10 | 5/10 | Liquidates positions that become undercollateralized purely from interest accrual over time. | Proactive model: project health factor forward in time, schedule liquidation at predicted crossing block. Complementary to Oracle-Latency Liq (§4.3) — that strategy is reactive (real price vs oracle price), this one is time-based (debt growth vs collateral). Very low competition — most MEV bots ignore slow-moving opportunities. |
| 17 | Stablecoin depeg arbitrage | Oracle | 5/10 | High | No (needs pre-positioned capital) | 1–5 | $5K–$50K | 6/10 | 8/10 | Arbitrage between depegged stablecoins and their peg value across Curve, AAVE, and CEX. | Requires pre-positioned capital but massive per-event profit. Monitor depeg signals (Curve pool ratios, CEX spreads); execute rapid multi-DEX swaps. Shared pattern with LST Depeg Liq (§4.5): both exploit Curve pool price diverging from oracle. Black Swan events are the primary income driver. |
| 18 | Flash loan atomic liquidation | Liquidation | 6/10 | None | Yes (the whole point) | 100–500 | $500–$3K | 7/10 | 8/10 | Uses flash loans to execute liquidations atomically — borrow, liquidate, repay in one tx. | Optimize flash loan routing (Balancer 0% fee preferred); model liquidation penalty vs flash loan cost. Multi-protocol support (AAVE, Compound, Maker) increases opportunity set. Swap routing optimization within the flash loan is the edge. |

### Tier 4 — Solid Build (Complexity 6/10)

Requires protocol-specific knowledge, multi-step simulation, or specialized storage.

| # | Strategy | Cat. | Cpx | Capital | Capital-Free? | Opps/Mo | Est. Monthly Income | Competition | Profitability | Overview | Competitive Edge |
|---|----------|------|:---:|---------|:---:|:---:|---:|:---:|:---:|----------|------------------|
| 19 | MakerDAO Clip Dutch auction | Liquidation | 6/10 | None | Yes (flash via join adapter) | 200–500 | $500–$3K | 6/10 | 7/10 | Participates in MakerDAO's Clipper Dutch auction liquidations, buying collateral at a discount. | Optimal `take()` timing — bid when auction price reaches optimal discount. Flash via join adapter for zero-capital execution. Pre-compute vault health database to identify upcoming auctions before they start. |
| 20 | AAVE partial liquidation opt. | Liquidation | 6/10 | Medium | Partial (flash viable) | 300–1K | $500–$3K | 7/10 | 7/10 | Executes partial liquidations on AAVE to close positions at minimum penalty while maximizing recovery. | Calculate optimal liquidation percentage to minimize penalty (5% vs 10% tiers). Flash loans viable for some steps. Batch multiple positions per tx. Understanding AAVE V3's penalty tiers is the edge. |
| 21 | Bad debt prevention optimizer | Liquidation | 6/10 | Medium | Partial (flash viable) | 50–200 | $200–$1K | 4/10 | 5/10 | Predicts and prevents bad debt by liquidating positions before they become insolvent. | Model protocol solvency in real-time; identify positions that will create bad debt if not liquidated. Early liquidation captures full penalty before protocol health degrades. Low competition due to predictive modeling requirement. |
| 22 | Curve pool imbalance | Protocol | 6/10 | Medium | Partial (flash viable) | 500–2K | $500–$3K | 5/10 | 7/10 | Exploits imbalances in Curve stableswap pools where tokens deviate from their peg ratio. | Monitor Curve pool balances for deviation from 1:1 ratio; execute multi-step swaps through the pool. Flash loans viable. Understanding Curve's StableSwap invariant and amplification coefficient is key. |
| 23 | L2 sequencer MEV | CrossDomain | 6/10 | Low | Partial (depends on sub-strategy) | 1K–5K | $300–$2K | 5/10 | 6/10 | Extracts MEV on L2 networks (Arbitrum, Optimism) through sequencer interaction patterns. | L2 sequencer ordering differs from L1 — opportunities arise from delayed finality and sequencer economics. Adapt L1 strategies to L2 constraints. Lower gas costs enable higher-frequency strategies. |
| 24 | NFT floor arbitrage | Emerging | 6/10 | High | No (needs NFT capital) | 50–200 | $200–$2K | 4/10 | 6/10 | Arbitrage between NFT floor prices and AMM pool prices for tokenized NFT positions. | Requires NFT capital and accurate floor price oracle. Wash-trading distorts floor data — need independent floor estimation. Illiquid market limits scalability. |
| 25 | ERC-4337 AA bundler MEV | Protocol | 6/10 | Low | None needed (bundler reward) | 1K–5K | $200–$1K | 4/10 | 5/10 | Extracts MEV through Account Abstraction bundler by ordering UserOps for maximum value. | Access alternative mempools (alt-mempool) for UserOps not visible in public mempool. Optimize UserOp ordering within bundles. Bundler reward mechanics create predictable income stream. |
| 26 | Velodrome/Aerodrome epoch | Protocol | 6/10 | Medium | No (needs pre-positioned LP) | 4 | $500–$2K | 2/10 | 6/10 | Exploits epoch-based liquidity incentives on Velodrome/Aerodrome by timing LP positions with reward distributions. | Time LP deposits/withdrawals to align with epoch boundaries for maximum veNEX boost. Low frequency (4x/month) but predictable. Requires understanding of epoch mechanics and gauge weight voting. |
| 27 | Trader Joe V2 Liquidity Book | Protocol | 6/10 | Low | Partial (JIT needs capital) | 200–1K | $500–$3K | 2/10 | 7/10 | Arbitrage and JIT liquidity using Trader Joe's Liquidity Book's discrete bin mechanics. | Understand bin-based pricing — each bin is a discrete price range. JIT in Liquidity Book requires precise bin placement. Lower competition on Avalanche. Bin math differs from V3 tick math — specialization required. |

### Tier 5 — Complex Build (Complexity 7/10)

Deep protocol expertise, multi-component simulation, or cross-system coordination.

| # | Strategy | Cat. | Cpx | Capital | Capital-Free? | Opps/Mo | Est. Monthly Income | Competition | Profitability | Overview | Competitive Edge |
|---|----------|------|:---:|---------|:---:|:---:|---:|:---:|:---:|----------|------------------|
| 28 | JIT liquidity (V3) | Bundle | 7/10 | High | No (needs real LP capital) | 200–1K | $2K–$10K | 5/10 | 8/10 | Provides just-in-time liquidity for large swaps — mint concentrated LP, capture fees, burn immediately after. | Detect large pending swaps in mempool; calculate optimal tick range matching the swap's price impact. Mint→Swap→Burn in one block. Fee growth tracking and tick-range overlap analysis are critical. Requires real capital but high returns. |
| 29 | Statistical arb / pairs | OrderFlow | 7/10 | Medium | Partial (flash viable) | 500–2K | $500–$3K | 4/10 | 6/10 | Statistical arbitrage between correlated token pairs (stables, LSTs, wrapped assets) that temporarily diverge. | Maintain correlation models; detect deviations beyond normal variance. Flash loans viable for execution. Pairs like stETH/ETH, rETH/ETH, LUSD/USDC have predictable mean-reversion. Requires statistical modeling infrastructure. |
| 30 | Oracle-latency liquidation | Liquidation | 7/10 | Medium | Yes (flash loan) | 200–800 | $2K–$10K | 5/10 | 9/10 | Liquidates positions using the latency between on-chain price changes and oracle updates. | Reactive model: monitor real price vs oracle price for deviation; co-bundle liquidation after oracle keeper tx. Complementary to Interest Accrual Liq (§4.13) — that is time-based (debt growth), this is price-based (oracle lag). Highest profitability of any liquidation strategy. Requires direct node access for minimum latency. |
| 31 | LST depeg collateral liq | Liquidation | 7/10 | Medium | Yes (flash loan) | 5–30 | $2K–$20K | 4/10 | 9/10 | Liquidates positions collateralized by LSTs (stETH, rETH) during depeg events. | Predict LST depeg cascades — when stETH depegs, leveraged positions on AAVE/Compound become undercollateralized. Flash loan covers capital. Low frequency but massive per-event profit ($2K–$20K). Shared pattern with Stablecoin Depeg Arb (§5.1): both exploit Curve pool real-time price diverging from Chainlink oracle. |
| 32 | NFT collateral liquidation | Liquidation | 7/10 | High | No (needs NFT capital) | 30–100 | $500–$3K | 3/10 | 6/10 | Liquidates NFT-collateralized positions on protocols like BendDAO, NFTfi when floor prices drop. | Requires accurate NFT floor price oracle and NFT capital for liquidation. NFT market illiquidity limits speed but also limits competition. Monitor floor price feeds and position health factors. |
| 33 | Governance MEV | Protocol | 7/10 | Medium | Partial (depends on opportunity) | 5–30 | $200–$2K | 3/10 | 6/10 | Extracts value from governance proposals — voting, proposal creation, or front-running governance actions. | Monitor all governance forums and Snapshot votes; predict outcomes and position accordingly. Low frequency but high value per event. Requires deep protocol knowledge and forum monitoring infrastructure. |
| 34 | Pendle PT/YT yield spread | Protocol | 7/10 | Medium | Partial (flash viable) | 200–500 | $500–$3K | 2/10 | 6/10 | Arbitrage between Pendle's Principal Token (PT) and Yield Token (YT) when implied yield diverges from realized yield. | Calculate implied vs realized yield spreads; execute when divergence exceeds threshold. Flash loans viable for capital. Low competition due to Pendle-specific complexity. Understanding PT/YT mechanics and expiry timing is critical. |
| 35 | Balancer rate provider staleness | Protocol | 7/10 | Medium | Yes (flash swap) | 200–500 | $300–$2K | 3/10 | 6/10 | Exploits stale rate providers in Balancer pools where the reported rate lags actual market price. | Monitor rate provider update frequency; detect when rate becomes stale relative to market. Flash swaps enable zero-capital execution. Rate staleness window is predictable per provider. |
| 36 | GMX V2 ADL front-run | Protocol | 7/10 | Low | None needed (keeper reward) | 20–100 | $1K–$5K | 2/10 | 7/10 | Front-runs GMX V2's automatic deleveraging (ADL) events by predicting which positions will be closed. | Predict ADL triggers based on price movements and position leverage; execute before automatic closure captures the keeper reward. GMX V2's ADL mechanism creates predictable opportunities. Low competition due to protocol-specific knowledge required. |
| 37 | Lido oracle report front-run | Protocol | 7/10 | Medium | Partial (flash viable) | 30 | $1K–$5K | 3/10 | 7/10 | Front-runs Lido's oracle reports that update stETH's reported balance based on validator performance. | Monitor validator performance and predict oracle report outcomes before they're submitted. Flash loans viable for execution. Monthly frequency but significant per-event profit. Requires validator set monitoring. |
| 38 | Convex/Curve gauge vote epoch | Protocol | 7/10 | High | No (needs LP capital) | 2 | $1K–$5K | 3/10 | 6/10 | Exploits gauge voting incentives by timing LP positions with Convex/Curve reward distributions. | Time LP deposits to maximize CRV/CVX emissions. Requires veToken governance understanding and gauge weight analysis. Very low frequency (2x/month) but predictable income. Capital-intensive. |
| 39 | Liquity recovery mode cascade | Liquidation | 7/10 | Low | Partial (flash viable) | 1–5 | $2K–$20K | 3/10 | 8/10 | Profits from Liquity's recovery mode which triggers cascading liquidations when system collateral ratio drops. | Predict recovery mode triggers; execute cascading liquidations during system stress. Flash loans viable per step. Massive per-event profit during market crashes. Requires real-time Liquity system health monitoring. |
| 40 | Cross-chain arbitrage | CrossDomain | 7/10 | High | No (needs capital on both chains) | 500–2K | $2K–$10K | 5/10 | 7/10 | Arbitrage between same assets on different chains via bridges. | Hard constraint: capital must be pre-deployed on all monitored chains simultaneously — rebalancing via bridges is too slow for intraday arb. Bridge-specific mechanics (Lock-and-Mint vs Burn-and-Mint) and finality windows determine execution risk. Bridge selection and timing are the moat. |
| 41 | Bridge MEV | CrossDomain | 7/10 | High | No (needs pre-deployed capital) | 500–2K | $1K–$5K | 4/10 | 7/10 | Extracts MEV through bridge transactions by predicting liquidity changes on destination chains. | Monitor bridge state and pending transfers; predict destination chain liquidity impact. Pre-deployed capital on both sides. Bridge-specific mechanics (Lock-and-Mint vs Burn-and-Mint) affect strategy design. |
| 42 | Solver / intent MEV | Emerging | 7/10 | Medium | Partial (depends on fill) | 2K–10K | $2K–$10K | 5/10 | 8/10 | Solves user intents (CoW Swap, 1inch Fusion, UniswapX) for MEV extraction by providing optimal execution. | Optimize solution algorithms to maximize fill rates and profit per fill. Access intent-specific mempools. Solver reputation affects priority. Growing market as intent-based trading increases. Requires sophisticated optimization engine. |
| 43 | Token launch snipe | Emerging | 7/10 | Low | None needed (sniping needs only gas) | 500–3K | $1K–$10K | 8/10 | 9/10 | Front-runs new token launches by being the first swap on a newly created liquidity pool. | Monitor factory contracts for pair creation events; submit add-liquidity + first-swap bundle. Only gas needed. Extremely competitive — speed to block builder and gas optimization are everything. High frequency, high profit per successful snipe. |

### Tier 6 — Advanced Build (Complexity 8/10)

Multi-protocol dependency, storage-level insight, or novel market design.

| # | Strategy | Cat. | Cpx | Capital | Capital-Free? | Opps/Mo | Est. Monthly Income | Competition | Profitability | Overview | Competitive Edge |
|---|----------|------|:---:|---------|:---:|:---:|---:|:---:|:---:|----------|------------------|
| 44 | CEX–DEX arbitrage | OrderFlow | 8/10 | High | No (capital on both sides) | 5K–30K | $10K–$100K | 9/10 | 9/10 | Arbitrage between centralized and decentralized exchanges when prices diverge. | Requires low-latency CEX connections (co-location, direct API), significant capital on both sides, and optimized execution. Jump/Wintermute hold infrastructure moat on top pairs. Focus on secondary pairs or alt-L1 DEXs where competition is lower. Highest absolute income of any strategy. |
| 45 | MakerDAO OSM preview + kick() | Liquidation | 8/10 | None | None needed (keeper reward) | ~720 | $500–$5K | 3/10 | 9/10 | Uses MakerDAO's Oracle Security Module preview slot to read next price before official update, then kicks undercollateralized vaults. | Read OSM storage slot 4 (next price, `uint128 next | uint16 has_next`) before the official oracle update; calculate which vaults will become undercollateralized. Submit `kick()` tx in the same block as poke(). Pre-computed vault health DB is critical. ~720 opportunities/month (hourly OSM cadence). Keeper reward is the income. |
| 46 | TWAP oracle manipulation | Oracle | 8/10 | High | No (needs massive capital) | 5–50 | $5K–$50K | 4/10 | 7/10 | Manipulates TWAP oracles by controlling price over multiple blocks to trigger favorable liquidations or arbitrage. | Requires massive capital to move price and sustain manipulation over multiple blocks. Adversarial strategy with legal and community risk. Deprioritized due to hostile nature. |
| 47 | Batch auction MEV | Emerging | 8/10 | Medium | Partial (flash possible) | 2K–10K | $1K–$5K | 4/10 | 7/10 | Extracts MEV from batch auction protocols (CoW Swap, DFNS) by optimizing settlement orders. | Understand batch auction clearing mechanics; optimize order bundling and pricing. Flash loans viable for some strategies. Requires understanding of solver competition dynamics. |
| 48 | Morpho Blue market transition | Protocol | 8/10 | Medium | Yes (flash loan) | 200–1K | $500–$3K | 2/10 | 5/10 | Exploits Morpho Blue's market transition events when positions migrate between isolated lending markets. | Predict market migrations; execute during transition windows. Flash loans viable. Low competition due to Morpho-specific complexity. Requires understanding of Morpho's isolated market architecture. |
| 49 | Uniswap V4 hook MEV | Protocol | 8/10 | Low | Yes (flash accounting built in) | 500–3K | $1K–$5K | 2/10 | 7/10 | Extracts MEV through Uniswap V4's hook system by exploiting hook-specific behaviors. | Build hook-type registry to understand each pool's hook behavior. Flash accounting in PoolManager enables zero-capital execution. Low competition due to V4's novelty. Hook-specific logic creates unique MEV opportunities not possible in V2/V3. |

### Tier 7 — Elite / Infrastructure-Moat (Complexity 9–10/10)

Requires capital + team + co-location or validator relationships.

| # | Strategy | Cat. | Cpx | Capital | Capital-Free? | Opps/Mo | Est. Monthly Income | Competition | Profitability | Overview | Competitive Edge |
|---|----------|------|:---:|---------|:---:|:---:|---:|:---:|:---:|----------|------------------|
| 50 | PBS / MEV-Boost (block building) | CrossDomain | 9/10 | High | No (needs staking + builder infra) | ~4,500 | $50K–$500K+ | 9/10 | 9/10 | Builds blocks for MEV-Boost PBS system, ordering transactions to maximize extracted MEV from the entire mempool. | Requires validator relationships, builder infrastructure, and co-location. Captures MEV from ALL other strategies by controlling block construction. $50K–$500K+/month but requires significant infrastructure investment. The ultimate infrastructure moat. |
| 51 | JIT + arb combo | Bundle | 9/10 | High | No (needs real LP capital) | 100–500 | $5K–$30K | 3/10 | 9/10 | Combines JIT liquidity provision with immediate arbitrage — provide liquidity for a swap, then arb the resulting imbalance. | Execute both strategies simultaneously in one block. Requires real LP capital but captures fees + arb profit. Low competition due to complexity of coordinating both strategies. Capital efficiency is maximized. |
| 52 | Multi-block MEV | Emerging | 9/10 | None | None needed (strategic ordering) | 10–100 | $5K–$50K | 1/10 | 9/10 | Extracts MEV across multiple consecutive blocks by controlling transaction ordering over time. | Requires validator relationships or block proposer access. No capital needed — pure strategic ordering. Lowest competition (1/10) of any high-income strategy. The moat is validator access, not capital. |
| 53 | Cascading liquidation eng. | Liquidation | 10/10 | High | Yes (flash loan viable per doc) | 5–50 | $10K–$100K+ | 2/10 | 10/10 | Engineers cascading liquidations across multiple protocols by modeling cross-protocol dependencies. | Model the entire DeFi dependency graph — when one protocol liquidates, it affects prices on others, triggering more liquidations. Flash loans viable per step. Requires deep understanding of cross-protocol interactions. $10K–$100K+ per cascade event. The most complex and profitable liquidation strategy. |

---

## 2. Capital-Free Strategies Inventory

Strategies executable with **zero pre-positioned capital** through flash loans, flash swaps, or keeper mechanics.

| Strategy | Tier | Monthly Income | Capital-Free Mechanism | Competition | Moat |
|----------|:---:|---:|:---|:---:|------|
| skim() capture | 1 | $50–$500 | balanceOf > reserve drift | 4/10 | First-caller wins |
| sync() race | 1 | $0–$50 | Public function, no capital | 3/10 | Defensive burn |
| Flash swap arbitrage | 3 | $500–$5K | Uniswap V2 callback | 7/10 | Speed + routing |
| Long-tail token arb | 3 | $300–$2K | Flash loan from Balancer/AAVE | 3/10 | Pool graph + SPFA |
| Interest accrual liq | 3 | $200–$1K | Flash loan + block scheduler | 2/10 | Forward HF model |
| Flash loan atomic liq | 4 | $500–$3K | Flash loan (Balancer 0% fee) | 7/10 | Swap routing optimization |
| MakerDAO Clip Dutch auction | 4 | $500–$3K | Flash via join adapter callback | 6/10 | Optimal take() block calc |
| ERC-4337 AA bundler MEV | 4 | $200–$1K | Bundler reward for UserOp ordering | 4/10 | Alt mempool access |
| MakerDAO OSM preview + kick() | 6 | $500–$5K | Keeper reward + storage slot read | 3/10 | Vault health DB pre-computed |
| Uniswap V4 hook MEV | 6 | $1K–$5K | Flash accounting built into PoolManager | 2/10 | Hook-type registry |
| Multi-block MEV | 7 | $5K–$50K | Strategic ordering, no capital | 1/10 | Validator relationships |
| Cascading liq eng. | 7 | $10K–$100K+ | Flash loan through Balancer/AAVE | 2/10 | Cross-protocol dep graph |

**Pattern**: Capital-free and low-competition rarely coincide — but when they do (OSM preview, V4 hooks, interest accrual, multi-block), the ROI per engineering hour is highest.

**Flash loan source hierarchy** (all flash-based strategies):

| Source | Fee | Best for |
|--------|-----|----------|
| Balancer | 0% | Large amounts, preferred default |
| AAVE V3 | 0.05% | Flexible collateral, most protocols |
| Uniswap V3 | 0.05–1% | Token-dependent, fallback only |

Balancer's zero fee makes it the default flash source. Route through Balancer first, fall back to AAVE V3 for tokens not supported by Balancer pools.

---

## 3. Best "First $" by Operator Profile

### Zero Capital, Solo Dev, 1 Week Build
1. **skim() capture** — validates pipeline, $50–$500/mo, comp=4
2. **Interest accrual liquidation** — $200–$1K/mo, comp=2, no reactivity needed
3. **MakerDAO OSM kick()** — $500–$5K/mo, comp=3, ETH L1 only
4. **GMX v1 keeper** — $200–$1K/mo, requires position indexer

### Low Capital ($5K–$10K), 2–3 Week Build
1. **Long-tail token arb (SPFA engine)** — $300–$2K/mo, comp=3
2. **V3 range order snipe** — $200–$1.5K/mo, comp=4
3. **MakerDAO Clip auction** — $500–$3K/mo, comp=6
4. **Velodrome/Aerodrome epoch** — $500–$2K/mo, 4 opps/mo, comp=2

### Medium Capital ($10K–$50K)
1. **JIT liquidity (V3)** — $2K–$10K/mo, comp=5
2. **Pendle PT/YT spread** — $500–$3K/mo, comp=2
3. **Lido oracle front-run** — $1K–$5K/mo, 30 opps/mo
4. **Oracle-latency liquidation** — $2K–$10K/mo, comp=5

### High Capital ($200K+) or Validator Access
1. **Cascading liquidation eng.** — $10K–$100K+/mo, comp=2
2. **Multi-block MEV** — $5K–$50K+/mo, comp=1
3. **CEX–DEX arb (secondary pairs)** — $10K–$100K/mo, comp=9

---

## 4. Frequency vs Income Heatmap

| | Rare (1–10/mo) | Episodic (10–200/mo) | Daily (200–1K/mo) | Continuous (1K+/mo) |
|---|:---:|:---:|:---:|:---:|
| **$0–$500** | — | Rebase arb, FoT arb | — | sync(), GMX v1 keeper |
| **$500–$3K** | Stablecoin depeg, recovery cascade | Clip auction, Balancer staleness | Interest accrual liq, AAVE liq | Flash swap arb, Curve imbalance, Trader Joe LB, ERC-4337 |
| **$3K–$10K** | Cascading liq eng., LST depeg | Lido oracle, GMX V2 ADL | JIT liquidity, Oracle-latency liq | Backrunning, Sandwich, Solver intent |
| **$10K+** | Multi-block MEV | TWAP manipulation, Governance | — | CEX–DEX arb, PBS/block building |

---

## 5. Capital-Efficiency Opportunity Score

**Formula**: `(Profitability²) / Competition × (1.0 if capital-free, else 0.5 if low capital, else 0.25 if medium/high)`

| Strategy | Profit | Comp | Capital-Free? | Capital-Efficiency Score |
|----------|:---:|:---:|:---:|:---:|
| Multi-block MEV | 9 | 1 | Yes | **81.0** |
| Cascading liq eng. | 10 | 2 | Yes | **50.0** |
| MakerDAO OSM preview | 9 | 3 | Yes | **27.0** |
| GMX V2 ADL front-run | 7 | 2 | Yes | **24.5** |
| Uniswap V4 hook MEV | 7 | 2 | Yes | **24.5** |
| Interest accrual liq | 5 | 2 | Yes | **12.5** |
| Trader Joe V2 LB | 7 | 2 | Low | **12.25** |
| Velodrome epoch | 6 | 2 | Medium | **9.0** |
| Flash swap arbitrage | 7 | 7 | Yes | **7.0** |
| ERC-4337 bundler MEV | 5 | 4 | Yes | **6.25** |
| MakerDAO Clip auction | 7 | 6 | Yes | **8.17** |
| CEX–DEX arb | 9 | 9 | High | **4.5** |
| Sandwich attack | 7 | 8 | Medium | **3.06** |

**Takeaway**: Capital-free + low-competition strategies dominate capital-efficiency. Multi-block MEV is the single highest return-per-unit-effort strategy if validator relationships are established.

---

## 6. Strategies to Deprioritize Permanently

| Strategy | Reason |
|----------|--------|
| Top-tier CEX–DEX arb (ETH L1) | Jump/Wintermute hold infrastructure moat; not a strategy problem |
| Sandwich on ETH L1 | MEV-Share and SUAVE structurally compressing this market |
| TWAP oracle manipulation | Adversarial; community hostile; legal exposure |
| NFT floor arbitrage | Illiquid; wash-trading distorts floor data; operational complexity |
| Governance MEV | Requires monitoring all governance forums; low frequency |

---

## 7. Codebase Implementation Status

Status of each strategy in the MEV Scout codebase:
- **Coded & Backtested**: Full Rust implementation, tested, can run in backtest/live mode
- **Planned (Phase N)**: Referenced in `plans/implementation_plan.md` with file path assigned
- **Not in scope**: Not included in any plan

### 7.1 Coded & Backtested (7 strategies)

| # | Strategy | File | Lines | Tests | Backtested? | Live Mode? | Notes |
|---|----------|------|:---:|:---:|:---:|:---:|-------|
| 1 | **TwoHopArb** | `core/src/mev/detectors/two_hop.rs` | ~623 | `tests/arbitrage.rs` | Yes | Yes | V2↔V2, V3↔V3, V2↔V3, Curve, Balancer, TraderJoeLB, Pendle. Dedup per block. |
| 2 | **MultiHopArb** | `core/src/mev/detectors/multi_hop.rs` | ~346 | `tests/arbitrage.rs` | Yes | Yes | BFS depth ≤ 4. Optimal N-hop. Slippage modeling. |
| 3 | **JIT (V3)** | `core/src/mev/detectors/jit.rs` | ~333 | `tests/sandwich.rs` | Yes | Yes | Mint→Swap→Burn detection. Fee growth tracking. Tick-range overlap. |
| 4 | **JitArb** | `core/src/mev/detectors/jit_arb.rs` | ~420 | `tests/sandwich.rs` | Yes | Yes | JIT + arbitrage. Proximity-window matching. |
| 5 | **Sandwich** | `core/src/mev/detectors/sandwich.rs` | ~530 | `tests/sandwich.rs` | Yes | Yes | V2, V3, Curve, Balancer. Front→Victim→Back matching. Re-quote profit. |
| 6 | **Liquidation** | `core/src/mev/detectors/liquidation.rs` | ~575 | `tests/liquidation.rs` (empty) | Yes | Yes | AAVE V3 reactive + proactive. Reserve cache. Health factor scanning. |
| 7 | **CrossBlockArb** | `core/src/mev/detectors/cross_block.rs` | ~236 | None | Yes | Yes | Sliding window. Persistent arb detection. Time-bandit detection. |

**Supporting infrastructure** (all built):
- **Mempool** (`mempool.rs`): Calldata parsing, V2/V3 exact-in, pending tx effects, eth_call simulation
- **LiveRunner** (`execution/live.rs`): Full live mode with settled block processing + mempool scanning + virtual wallet + P&L tracking
- **BacktestRunner** (`pipeline/runner.rs`): Orchestrates block replay through revm + all detectors
- **Pool state**: V2, V3, V4, Curve, Balancer, TraderJoeLB, Pendle, DODO
- **Pool math**: Core CPMM, V3 sqrt-price, Curve StableSwap, Balancer weighted, Trader Joe LB, Pendle
- **Detection filters**: FoT tokens, rebase tokens, V4 hook flags

**Recent backtest run** (`results/run_1783523729.json`):
- Chain: Polygon | Blocks: 89,878,932–89,879,031 (100 blocks)
- Strategies: two_hop_arb, multi_hop_arb, jit, jit_arb, sandwich, liquidation, cross_block_arb
- Result: 0 opportunities detected (range may not have had live opportunities)

### 7.2 Planned — Phase 0 (Quick Wins)

Capital-free, trivial builds that validate the pipeline.

| # | Strategy | File (planned) | ~Lines | Capital | Est. Build Time |
|---|----------|----------------|:---:|---------|:---:|
| 8 | **skim() capture** | `core/src/mev/detectors/skim.rs` | ~150 | None | 1–2 days |
| 9 | **Interest accrual liq** | `core/src/mev/detectors/interest_liq.rs` | ~200 | Low | 3–5 days |

### 7.3 Planned — Phase 1 (Capital-Free Production)

Capital-free strategies with capital-efficiency score > 10 or high frequency. Leverages existing `liquidation.rs` and `mempool.rs`.

| # | Strategy | File (planned) | ~Lines | Capital | Score |
|---|----------|----------------|:---:|---------|:---:|
| 10 | **Flash loan liq** (extend existing) | `core/src/mev/detectors/liquidation.rs` | +200 | None | 7.0 |
| 11 | **Backrunning** | `core/src/mev/detectors/backrun.rs` | ~300 | Low | — |
| 12 | **MakerDAO OSM preview** | `core/src/mev/detectors/makerdao_osm.rs` | ~250 | None | **27.0** |
| 13 | **GMX V2 ADL front-run** | `core/src/mev/detectors/gmx_adl.rs` | ~200 | None | **24.5** |
| 14 | **sync() race** | `core/src/mev/detectors/sync_race.rs` | ~80 | None | — |

### 7.4 Planned — Phase 2 (Low Capital + Chain-Specific)

Low-capital strategies, capital-efficiency score > 9, or chain-specific extensions. V4 hooks moved here from Phase 4 (capital-free, score=24.5).

| # | Strategy | File (planned) | ~Lines | Capital | Score |
|---|----------|----------------|:---:|---------|:---:|
| 15 | **Uniswap V4 hook MEV** | `core/src/mev/detectors/v4_hook_mev.rs` | ~300 | Low | **24.5** |
| 16 | **Oracle-latency liq** | `core/src/mev/detectors/oracle_latency_liq.rs` | ~200 | Low | 11.25 |
| 17 | **Trader Joe V2 LB** | `core/src/mev/detectors/joe_v2_lb.rs` | ~300 | Low | 12.25 |
| 18 | **Long-tail SPFA arb** | `core/src/mev/detectors/long_tail.rs` | ~400 | Low | 12.0 |
| 19 | **Init price snipe** | `core/src/mev/detectors/init_price.rs` | ~150 | Low | — |
| 20 | **FoT token arb** | `core/src/mev/detectors/fot_arb.rs` | ~150 | Low | — |
| 21 | **Rebase token arb** | `core/src/mev/detectors/rebase_arb.rs` | ~150 | Low | — |

### 7.5 Planned — Phase 3 (Medium Capital)

Medium-capital strategies with capital-efficiency score > 6. Liquity recovery moved here (low capital actually, score=21.3). Bad debt prevention added (extends `liquidation.rs`).

| # | Strategy | File (planned) | ~Lines | Capital | Score |
|---|----------|----------------|:---:|---------|:---:|
| 22 | **Pendle PT/YT yield spread** | `core/src/mev/detectors/pendle_pt_yt.rs` | ~250 | Medium | 18.0 |
| 23 | **Liquity recovery mode** | `core/src/mev/detectors/liquity_recovery.rs` | ~250 | Low | 21.3 |
| 24 | **Lido oracle front-run** | `core/src/mev/detectors/lido_oracle.rs` | ~200 | Medium | 16.3 |
| 25 | **Curve pool imbalance** | `core/src/mev/detectors/curve_imbalance.rs` | ~200 | Medium | 9.8 |
| 26 | **AAVE partial liq opt** | `core/src/mev/detectors/aave_partial_liq.rs` | ~250 | Medium | 6.9 |
| 27 | **Statistical arb/pairs** | `core/src/mev/detectors/stat_arb.rs` | ~300 | Medium | 9.0 |
| 28 | **Balancer rate provider** | `core/src/mev/detectors/balancer_rate.rs` | ~200 | Medium | 12.0 |
| 29 | **Velodrome/Aerodrome epoch** | `core/src/mev/detectors/velodrome_epoch.rs` | ~200 | Medium | 9.0 |
| 30 | **Bad debt prevention** | `core/src/mev/detectors/bad_debt_prevent.rs` | ~200 | Medium | — |

### 7.6 Planned — Phase 4 (High Capital + Infrastructure)

High-capital or infrastructure-moat strategies. Ordered by capital-efficiency score within phase.

| # | Strategy | File (planned) | ~Lines | Capital | Score |
|---|----------|----------------|:---:|---------|:---:|
| 31 | **MakerDAO Clip auction** | `core/src/mev/detectors/makerdao_clip.rs` | ~300 | None | 8.17 |
| 32 | **JIT + arb combo** (extend) | `core/src/mev/detectors/jit_arb.rs` | +200 | High | 27.0 |
| 33 | **Cascading liq engineering** | `core/src/mev/detectors/cascading_liq.rs` | ~500 | High (flash OK) | 50.0 |
| 34 | **Multi-block MEV** | `core/src/mev/detectors/multi_block.rs` | ~300 | None | **81.0** |
| 35 | **CEX–DEX arb** | `core/src/mev/detectors/cex_dex.rs` | ~400 | High | 4.5 |
| 36 | **PBS/MEV-Boost** | `core/src/mev/detectors/pbs_mev_boost.rs` | ~500 | High | — |

### 7.7 Planned — Phase 5 (Long-Tail + Emerging)

Remaining strategies: chain-specific extensions, high-competition, niche protocol, or low-priority. Build as market opportunities arise.

| # | Strategy | File (planned) | ~Lines | Capital |
|---|----------|----------------|:---:|---------|
| 37 | **Liquity stability pool** | `core/src/mev/detectors/liquity_stability.rs` | ~200 | Medium |
| 38 | **Stablecoin depeg arb** | `core/src/mev/detectors/stablecoin_depeg.rs` | ~250 | High |
| 39 | **Synthetix flag+delayed** | `core/src/mev/detectors/synthetix_flag.rs` | ~200 | Medium |
| 40 | **GMX v1 keeper** | `core/src/mev/detectors/gmx_keeper.rs` | ~250 | None |
| 41 | **GMX v2 keeper** (extend) | `core/src/mev/detectors/gmx_keeper.rs` | +100 | None |
| 42 | **PancakeSwap token snipe** | `core/src/mev/detectors/token_launch.rs` | ~250 | Low |
| 43 | **Venus flash loan liq** | `core/src/mev/detectors/liquidation.rs` | +150 | None |
| 44 | **Sandwich via 48Club** | `core/src/mev/detectors/sandwich.rs` | +100 | Medium |
| 45 | **Pharaoh epoch** | `core/src/mev/detectors/pharaoh_epoch.rs` | ~200 | Medium |
| 46 | **ERC-4337 bundler MEV** | `core/src/mev/detectors/erc4337_bundler.rs` | ~300 | Low |
| 47 | **L2 sequencer MEV** | `core/src/mev/detectors/l2_sequencer.rs` | ~300 | Low |
| 48 | **Solver/intent MEV** | `core/src/mev/detectors/solver_intent.rs` | ~350 | Medium |
| 49 | **Batch auction/CoW** | `core/src/mev/detectors/batch_auction.rs` | ~300 | Medium |
| 50 | **Bridge MEV** | `core/src/mev/detectors/bridge_mev.rs` | ~300 | High |
| 51 | **Cross-chain arb** | `core/src/mev/detectors/cross_chain.rs` | ~400 | High |
| 52 | **Morpho Blue market state** | `core/src/mev/detectors/morpho_blue.rs` | ~200 | Medium |
| 53 | **Convex gauge vote epoch** | `core/src/mev/detectors/convex_gauge.rs` | ~250 | High |

### 7.7 Detection & Simulation Status Summary

| Capability | Status | Details |
|-----------|--------|---------|
| **Detection** | 7 strategies coded | TwoHopArb, MultiHopArb, JIT, JitArb, Sandwich, Liquidation, CrossBlockArb |
| **Simulation (revm)** | Built | `replay/` module with full revm block replay |
| **Backtesting** | Active | `scripts/test_backtest.ps1` runs Dune-guided backtests across historical blocks |
| **Live mode** | Active | `LiveRunner` in `execution/live.rs` processes real blocks + mempool |
| **Virtual trading** | Active | Wallet simulation with P&L tracking, bankruptcy detection, execution history |
| **Mempool detection** | Active | Calldata parsing for V2/V3 swaps, eth_call validation |
| **Pool discovery** | Active | Dune + on-chain discovery for V2, V3, Curve, Balancer, Trader Joe, Pendle |
| **Test coverage** | 4 test files | `arbitrage.rs` (610 lines), `sandwich.rs` (398 lines), `liquidation.rs` (empty), `e2e.rs` (539 lines) |
| **46 strategies** | Planned (Phases 0–5) | All mapped to specific files in `plans/implementation_plan.md` (~8K–10K Rust planned) |
| **3 strategies** | Excluded permanently | TWAP manipulation, NFT floor arb, governance MEV |

---

*Analysis generated from `mev_strategies_complete_v2.md` (53 strategies, 8 categories, 2,716 lines) cross-referenced with MEV Scout codebase status at `core/src/` (7 detectors coded, 46 planned).*

---

## 8. Dune On-Chain Validation Report

> **Methodology**: Corrected Dune SQL queries executed via `mev-scout dune-query` against Polygon mainnet (blocks 89,616,951–90,851,237, ~21 days, July 4–July 25, 2026).
> All frequency/income claims in this document were compared against real on-chain data.

### 8.1 Validation Results

| # | Strategy | Query | Result (21 days) | Projected /mo | Claim in Analysis | Verdict |
|---|----------|-------|:-----------------:|:-------------:|-------------------|---------|
| 11 | **Sandwich attack** | `QUERY_SANDWICHES_BY_RANGE` | **34 txs** | ~49 | 500–5K/mo, $1K–$10K | **10–100x inflated** |
| 4 | **Backrunning** | `VALIDATE_BACKRUN` | **9,488 opps, $5.0M est** | ~13.5K, $7.1M | 1K–5K/mo, $500–$5K | **2–10x higher than claimed** |
| 10 | **Long-tail token arb** | `VALIDATE_LONG_TAIL_ARB` | **140,603 opps, $50.6K est** | ~200K, $72K | 1K–10K/mo, $300–$2K | **14x+ inflated** |
| 28 | **JIT liquidity (V3)** | `VALIDATE_JIT_FEE_CAPTURE` | **280 txs, $280K est** | ~400, $400K | 200–1K/mo, $2K–$10K | **Within range** (freq accurate, income likely optimistic) |
| 3 | **Init price snipe** | `VALIDATE_INIT_PRICE_SNIPE` | **0** | 0 | 30–200/mo, $100–$2K | **Zero on Polygon** — no V3 PoolCreated events in range |
| 31 | **LST depeg collateral liq** | `VALIDATE_LST_DEPEG_LIQ` | **0** | 0 | 5–30/mo, $2K–$20K | **Zero on Polygon** — LSTs are Ethereum-native |
| 18 | **Flash loan atomic liq** | `VALIDATE_FLASH_LIQ_PROFIT` | **136 txs, $5.2K vol** | ~194 | 100–500/mo, $500–$3K | **Frequency accurate**, volume data partial |
| All | **Liquidations (AAVE V3)** | `QUERY_LIQUIDATIONS_ALL` | **519 txs** | ~741 | Various | Active but modest |
| — | **Flash loans** | `QUERY_FLASH_LOANS_BY_RANGE` | **30,884 rows** | ~44K | — | Active flash loan market confirmed |

### 8.2 Not Validated on Polygon (Protocol Not Deployed / No Decoded Tables)

| # | Strategy | Reason | Chain to Validate |
|---|----------|--------|-------------------|
| 1 | sync() race | `uniswap_v2_polygon` decoded tables don't exist on Dune | Ethereum, or find QuickSwap table name |
| 2 | skim() capture | Same as above | Ethereum, or find QuickSwap table name |
| 17 | Stablecoin depeg arb | `curve_polygon` schema doesn't exist on Dune | Ethereum (`curve_ethereum`) |
| 22 | Curve pool imbalance | Same as above | Ethereum |
| 19 | MakerDAO Clip auction | MakerDAO not deployed on Polygon | Ethereum |
| 45 | MakerDAO OSM preview + kick | Same as above | Ethereum |
| 8 | GMX v1 keeper | GMX not deployed on Polygon | Arbitrum |
| 36 | GMX V2 ADL front-run | Same as above | Arbitrum |
| 39 | Liquity recovery mode | Liquity not deployed on Polygon | Ethereum |
| 14 | Synthetix flag + delayed liq | Synthetix not on Polygon | Optimism, Ethereum |
| 13 | Perp protocol keeper | Gains Network on Polygon but table name wrong | Arbitrum (dYdX), Optimism (Kwenta) |

### 8.3 Key Corrections to Analysis

**Sandwich income (Strategy #11)**: The analysis claims 500–5K opportunities/month generating $1K–$10K on Polygon. Dune data shows **34 sandwich events in 21 days** (~49/month). This is **10–100x lower** than claimed. The sandwich market on Polygon is far more competitive and lower-value than estimated.

**Backrunning (Strategy #4)**: The analysis claims 1K–5K opportunities/month. Dune data shows **9,488 backrunning-proxied transactions in 21 days** (~13.5K/month). This is **2–10x higher** than claimed. The $5.0M estimated profit (at 0.3% fee rate) suggests this is a significant market.

**Long-tail token arb (Strategy #10)**: The analysis claims 1K–10K opportunities/month. Dune shows **140,603 multi-pool transactions involving low-volume tokens in 21 days** (~200K/month). This is **14x higher** than the upper bound claimed. However, not all multi-pool txs are arbitrage — many are normal trading activity. The 0.5% fee estimate ($50.6K total) is a proxy, not confirmed profit.

**JIT liquidity (Strategy #28)**: The analysis claims 200–1K opportunities/month. Dune shows **280 V3 Mint+Swap+Burn transactions in 21 days** (~400/month). This is **within the claimed range**. The $1K per-event estimate is likely optimistic for Polygon.

### 8.4 Recommendations

1. **Sandwich**: Deprioritize on Polygon — market is too small
2. **Backrunning**: Higher opportunity than claimed — validate with mempool data for real-time profitability
3. **Long-tail arb**: Volume is high but profit per opportunity is very low ($0.36 avg) — needs high-frequency execution
4. **JIT liquidity**: On track — proceed with implementation as planned
5. **Init price snipe**: Not applicable on Polygon — validate on Ethereum
6. **LST depeg liq**: Not applicable on Polygon — validate on Ethereum
7. **Skim/sync**: Find correct QuickSwap decoded table name on Dune, or validate on Ethereum

---

*Validation report generated July 25, 2026 via `mev-scout dune-query` against Dune Analytics API.*
