//! Dune SQL query templates for pool discovery and cross-validation.
//!
//! These are designed to be created as saved queries on Dune (dune.com/queries)
//! and executed by their numeric ID via `DuneClient::execute_query_by_id`.
//!
//! Alternatively, if you have Dune Plus/Enterprise with raw SQL execution,
//! use `DuneClient::execute_raw_sql` with the `format!`-ed query string.
//!
//! # Usage
//!
//! 1. Go to dune.com/queries
//! 2. Create a new query with one of the templates below
//! 3. Note the numeric query ID
//! 4. Set `[dune] pool_discovery_query_id = <ID>` in your config
//!
//! # Template Variables
//! - `{chain}` — Dune chain name (e.g. `ethereum`, `polygon`, `bsc`, `arbitrum`, `base`, `optimism`, `avalanche_c`)
//! - `{from_block}` — start block
//! - `{to_block}` — end block

// ── Pool Discovery ────────────────────────────────────────────────────────

/// Discover V2-style pools (Uniswap V2 & forks like PancakeSwap, QuickSwap, etc.)
/// via their PairCreated events.
///
/// Columns returned: `pool_address`, `token0`, `token1`, `creation_block`, `factory`
pub const QUERY_V2_POOLS_BY_FACTORY: &str = r#"
SELECT
  p.contract_address AS pool_address,
  p.token0,
  p.token1,
  p.evt_block_number AS creation_block,
  p.contract_address AS factory
FROM uniswap_v2_{chain}.Factory_evt_PairCreated p
WHERE p.evt_block_number >= {from_block}
  AND p.evt_block_number <= {to_block}
ORDER BY p.evt_block_number ASC
"#;

/// Discover V3 pools via their PoolCreated events.
///
/// Columns returned: `pool_address`, `token0`, `token1`, `fee`, `tick_spacing`, `creation_block`, `factory`
pub const QUERY_V3_POOLS_BY_FACTORY: &str = r#"
SELECT
  p.pool AS pool_address,
  p.token0,
  p.token1,
  p.fee,
  p.tick_spacing,
  p.evt_block_number AS creation_block,
  p.contract_address AS factory
FROM uniswap_v3_{chain}.Factory_evt_PoolCreated p
WHERE p.evt_block_number >= {from_block}
  AND p.evt_block_number <= {to_block}
ORDER BY p.evt_block_number ASC
"#;

/// Discover all DEX pools at once using Dune's consolidated `dex.trades` table
/// (extracts unique pool addresses with their tokens and fee).
///
/// This is chain-agnostic — set {chain} to any supported EVM chain.
///
/// Columns returned: `pool_address`, `token0`, `token1`, `fee`, `project`, `blockchain`
pub const QUERY_ALL_ACTIVE_POOLS: &str = r#"
SELECT DISTINCT
  t.pool_address,
  t.token_bought_address AS token0,
  t.token_sold_address AS token1,
  0 AS fee,
  t.project,
  t.blockchain
FROM dex.trades t
WHERE t.blockchain = '{chain}'
  AND t.block_number >= {from_block}
  AND t.block_number <= {to_block}
"#;

/// Discover Uniswap V3 pools with their fee tier using Dune's consolidated dataset.
///
/// Columns returned: `pool_address`, `token0`, `token1`, `fee`, `tick_spacing`, `creation_block`, `project`
pub const QUERY_V3_POOLS_FROM_TRADES: &str = r#"
SELECT DISTINCT
  t.pool_address,
  t.token_bought_address AS token0,
  t.token_sold_address AS token1,
  t.fee,
  NULL AS tick_spacing,
  t.block_number AS creation_block,
  t.project
FROM dex.trades t
WHERE t.blockchain = '{chain}'
  AND t.block_number >= {from_block}
  AND t.block_number <= {to_block}
  AND t.project LIKE '%v3%'
"#;

/// PancakeSwap V2-specific pool discovery (BSC).
///
/// Columns returned: `pool_address`, `token0`, `token1`, `creation_block`
pub const QUERY_PANCAKESWAP_V2_POOLS: &str = r#"
SELECT
  p.contract_address AS pool_address,
  p.token0,
  p.token1,
  p.evt_block_number AS creation_block
FROM pancakeswap_v2_{chain}.Factory_evt_PairCreated p
WHERE p.evt_block_number >= {from_block}
  AND p.evt_block_number <= {to_block}
ORDER BY p.evt_block_number ASC
"#;

// ── Cross-Validation ──────────────────────────────────────────────────────

/// Check if a specific swap (by tx_hash) is recorded in Dune's `dex.trades`.
///
/// Columns returned: `block_number`, `tx_hash`, `token_bought_address`, `token_sold_address`,
/// `amount_bought`, `amount_sold`, `taker`, `pool_address`, `project`, `amount_usd`
pub const QUERY_VERIFY_TRADE_BY_TX: &str = r#"
SELECT
  t.block_number,
  t.tx_hash,
  t.token_bought_address,
  t.token_sold_address,
  t.amount_bought,
  t.amount_sold,
  t.taker,
  t.pool_address,
  t.project,
  t.amount_usd
FROM dex.trades t
WHERE t.blockchain = '{chain}'
  AND t.tx_hash = '{tx_hash}'
  AND t.block_number = {block_number}
LIMIT 1
"#;

/// Query all DEX trades in a block that match a given pool and token pair.
/// Useful for cross-validating arbitrage opportunity.
///
/// Columns returned: `tx_hash`, `token_bought_address`, `token_sold_address`,
/// `amount_bought`, `amount_sold`, `amount_usd`, `taker`
pub const QUERY_TRADES_IN_BLOCK: &str = r#"
SELECT
  t.tx_hash,
  t.token_bought_address,
  t.token_sold_address,
  t.amount_bought,
  t.amount_sold,
  t.amount_usd,
  t.taker
FROM dex.trades t
WHERE t.blockchain = '{chain}'
  AND t.block_number = {block_number}
  AND (t.pool_address = '{pool_a}' OR t.pool_address = '{pool_b}')
ORDER BY t.tx_index ASC
"#;

/// Check if a transaction was part of a sandwich attack.
/// Uses Dune's curated `dex.sandwiches` dataset.
///
/// Columns returned: `block_number`, `victim_tx_hash`, `front_tx_hash`,
/// `back_tx_hash`, `sandwich_type`, `pool_address`
pub const QUERY_VERIFY_SANDWICH: &str = r#"
SELECT
  s.block_number,
  s.victim_tx_hash,
  s.front_tx_hash,
  s.back_tx_hash,
  s.sandwich_type,
  s.pool_address
FROM dex.sandwiches s
WHERE s.blockchain = '{chain}'
  AND s.block_number = {block_number}
  AND (s.victim_tx_hash = '{tx_hash}'
       OR s.front_tx_hash = '{tx_hash}'
       OR s.back_tx_hash = '{tx_hash}')
LIMIT 10
"#;

/// Get historical USD prices for given token address at a specific block time.
///
/// Columns returned: `minute`, `price`, `symbol`
pub const QUERY_TOKEN_PRICE_AT_BLOCK: &str = r#"
SELECT
  p.minute,
  p.price,
  p.symbol
FROM prices.usd p
WHERE p.contract_address = '{token_address}'
  AND p.minute <= TIMESTAMP '{block_timestamp}'
  AND p.minute >= TIMESTAMP '{block_timestamp}' - INTERVAL '1' hour
ORDER BY p.minute DESC
LIMIT 1
"#;

// ── MEV Event Queries ─────────────────────────────────────────────────────

/// Fetch all known sandwich attacks in a block range from `dex.sandwiches`.
///
/// Dune's curated dataset captures frontrun+ victim+ backrun tx hashes per sandwich.
///
/// Columns: `block_number`, `victim_tx_hash`, `front_tx_hash`, `back_tx_hash`,
/// `sandwich_type`, `pool_address`, `mev_profit_eth`
pub const QUERY_SANDWICHES_BY_RANGE: &str = r#"
SELECT
  s.block_number,
  s.victim_tx_hash,
  s.front_tx_hash,
  s.back_tx_hash,
  s.sandwich_type,
  s.pool_address,
  s.mev_profit_eth
FROM dex.sandwiches s
WHERE s.blockchain = '{chain}'
  AND s.block_number >= {from_block}
  AND s.block_number <= {to_block}
ORDER BY s.block_number, s.victim_tx_hash
"#;

/// Fetch arbitrage transactions from Dune's curated dataset or detect them
/// from `dex.trades` using the pattern: one tx swaps through >= 2 pools.
///
/// Columns: `block_number`, `tx_hash`, `pool_a`, `pool_b`, `token_in`,
/// `token_out`, `profit_usd`, `profit_eth`
pub const QUERY_ARBITRAGES_BY_RANGE: &str = r#"
WITH tx_pools AS (
  SELECT
    t.blockchain,
    t.block_number,
    t.tx_hash,
    t.pool_address,
    t.token_bought_address,
    t.token_sold_address,
    t.amount_usd,
    t.tx_index,
    COUNT(*) OVER (PARTITION BY t.blockchain, t.block_number, t.tx_hash) AS pool_count
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_number >= {from_block}
    AND t.block_number <= {to_block}
)
SELECT DISTINCT
  tp.block_number,
  tp.tx_hash,
  FIRST_VALUE(tp.pool_address) OVER (PARTITION BY tp.block_number, tp.tx_hash ORDER BY tp.tx_index) AS pool_a,
  LAST_VALUE(tp.pool_address) OVER (PARTITION BY tp.block_number, tp.tx_hash ORDER BY tp.tx_index) AS pool_b,
  FIRST_VALUE(tp.token_sold_address) OVER (PARTITION BY tp.block_number, tp.tx_hash ORDER BY tp.tx_index) AS token_in,
  LAST_VALUE(tp.token_bought_address) OVER (PARTITION BY tp.block_number, tp.tx_hash ORDER BY tp.tx_index) AS token_out,
  tp.amount_usd
FROM tx_pools tp
WHERE tp.pool_count >= 2
ORDER BY tp.block_number, tp.tx_hash
"#;

/// Fetch all flash loan events from Dune.
///
/// Columns: `block_number`, `tx_hash`, `protocol`, `token_address`, `amount_usd`,
/// `amount`, `fee`
pub const QUERY_FLASH_LOANS_BY_RANGE: &str = r#"
SELECT
  f.block_number,
  f.tx_hash,
  f.protocol,
  f.token_address,
  f.amount_usd,
  f.amount,
  f.fee
FROM lending.flashloans f
WHERE f.blockchain = '{chain}'
  AND f.block_number >= {from_block}
  AND f.block_number <= {to_block}
ORDER BY f.block_number, f.tx_hash
"#;

/// Fetch pool liquidity snapshots from Dune — returns TVL and reserve info
/// for DEX pools at a given block range end.
///
/// Columns: `pool_address`, `project`, `token0_address`, `token1_address`,
/// `reserve0`, `reserve1`, `tvl_usd`
pub const QUERY_POOL_LIQUIDITY: &str = r#"
SELECT DISTINCT ON (t.pool_address)
  t.pool_address,
  t.project,
  t.token_bought_address AS token0_address,
  t.token_sold_address AS token1_address,
  SUM(t.amount_bought) OVER (PARTITION BY t.pool_address, t.token_bought_address) AS reserve0,
  SUM(t.amount_sold) OVER (PARTITION BY t.pool_address, t.token_sold_address) AS reserve1,
  t.tvl_usd
FROM dex.trades t
WHERE t.blockchain = '{chain}'
  AND t.block_number <= {to_block}
  AND t.block_number >= {to_block} - 1000
  AND t.tvl_usd IS NOT NULL
ORDER BY t.pool_address, t.block_number DESC
"#;
