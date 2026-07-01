//! Dune SQL query templates for MEV Scout.
//!
//! # Usage
//! 1. Go to dune.com/queries and create a **New Query**
//! 2. Copy-paste a template below
//! 3. Set the numeric query ID in `mev-scout.toml` under `[dune]`
//!
//! # Parameter Placeholders
//! - `{chain}` — Dune chain name: `ethereum`, `polygon`, `bsc`, `arbitrum`, `base`, `optimism`, `avalanche_c`
//! - `{from_block}` / `{to_block}` — block range (inclusive)
//! - `{from_time}` / `{to_time}` — ISO-8601 timestamps, e.g. `2024-01-01 00:00:00`
//! - `{pool_address}` / `{token_address}` / `{tx_hash}` — hex addresses with `0x` prefix
//!
//! # Column Order
//! The column index (0-based) in SELECT defines how Rust code reads the result.
//! Do NOT change column order without updating the corresponding fetch function.

// ══════════════════════════════════════════════════════════════════════════
// Section 1: Pool Discovery
// ══════════════════════════════════════════════════════════════════════════

/// V2-style pools via PairCreated event (Uniswap V2, PancakeSwap V2, QuickSwap, SushiSwap, etc.).
///
/// Columns: `pool_address`(0), `token0`(1), `token1`(2), `creation_block`(3), `factory`(4)
pub const QUERY_V2_POOLS_BY_FACTORY: &str = r#"
SELECT
  p.pair AS pool_address,
  p.token0,
  p.token1,
  p.evt_block_number AS creation_block,
  p.contract_address AS factory
FROM uniswap_v2_{chain}.Factory_evt_PairCreated p
WHERE p.evt_block_number >= {from_block}
  AND p.evt_block_number <= {to_block}
ORDER BY p.evt_block_number ASC
"#;

/// V3 pools via PoolCreated event (Uniswap V3, PancakeSwap V3, QuickSwap V3, etc.).
///
/// Columns: `pool_address`(0), `token0`(1), `token1`(2), `fee`(3), `tick_spacing`(4),
///          `creation_block`(5), `factory`(6)
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

/// Curve pools via `PoolAdded` events from Curve's Registry and PoolRegistry contracts.
///
/// Columns: `pool_address`(0), `coins`(1) [JSON array of token addresses], `n_coins`(2),
///          `creation_block`(3), `pool_type`(4), `registry`(5)
pub const QUERY_CURVE_POOLS: &str = r#"
WITH curve_registries AS (
  SELECT contract_address AS registry FROM ethereum.contracts WHERE namespace = 'curve' AND name = 'Registry'
  UNION
  SELECT contract_address FROM ethereum.contracts WHERE namespace = 'curve' AND name = 'PoolRegistry'
  UNION
  SELECT contract_address FROM ethereum.contracts WHERE namespace = 'curve' AND name = 'MetaPoolFactory'
)
SELECT
  p.pool AS pool_address,
  p.coins AS coins_json,
  ARRAY_LENGTH(p.coins) AS n_coins,
  p.evt_block_number AS creation_block,
  'curve_' || CAST(ARRAY_LENGTH(p.coins) AS VARCHAR) AS pool_type,
  p.contract_address AS registry
FROM curve_{chain}.Registry_evt_PoolAdded p
WHERE p.evt_block_number >= {from_block}
  AND p.evt_block_number <= {to_block}
UNION ALL
SELECT
  p.pool,
  p.coins,
  ARRAY_LENGTH(p.coins),
  p.evt_block_number,
  'curve_' || CAST(ARRAY_LENGTH(p.coins) AS VARCHAR),
  p.contract_address
FROM curve_{chain}.PoolRegistry_evt_PoolAdded p
WHERE p.evt_block_number >= {from_block}
  AND p.evt_block_number <= {to_block}
ORDER BY creation_block ASC
"#;

/// Balancer V2 pools via `PoolRegistered` event.
///
/// Columns: `pool_address`(0), `pool_id`(1) [bytes32], `pool_type`(2),
///          `creation_block`(3), `vault_address`(4)
pub const QUERY_BALANCER_POOLS: &str = r#"
SELECT
  p.pool AS pool_address,
  p.poolId AS pool_id,
  p.poolType AS pool_type,
  p.evt_block_number AS creation_block,
  p.contract_address AS vault_address
FROM balancer_v2_{chain}.Vault_evt_PoolRegistered p
WHERE p.evt_block_number >= {from_block}
  AND p.evt_block_number <= {to_block}
ORDER BY p.evt_block_number ASC
"#;

/// Discover all DEX pools from `dex.trades` — extracts unique pool addresses with metadata.
/// This is the most reliable catch-all query (works even without decoded event tables).
///
/// Columns: `pool_address`(0), `token_bought_address`(1), `token_sold_address`(2),
///          `project`(3), `project_type`(4), `last_active_block`(5), `min_fee`(6)
pub const QUERY_ALL_ACTIVE_POOLS: &str = r#"
WITH pool_stats AS (
  SELECT
    t.pool_address,
    t.token_bought_address AS token0,
    t.token_sold_address AS token1,
    t.project,
    t.project_type,
    MAX(t.block_number) AS last_active_block,
    MIN(t.fee) AS min_fee
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_number >= {from_block}
    AND t.block_number <= {to_block}
  GROUP BY 1,2,3,4,5
)
SELECT
  ps.pool_address,
  ps.token0,
  ps.token1,
  ps.project,
  ps.project_type,
  ps.last_active_block,
  COALESCE(ps.min_fee, 0) AS fee
FROM pool_stats ps
ORDER BY ps.last_active_block DESC
"#;

/// Get pools with token symbols and decimals (richest pool discovery query).
/// Uses distinct pools from dex.trades and joins with tokens.erc20.
///
/// Columns: `pool_address`(0), `token0_address`(1), `token1_address`(2),
///          `token0_symbol`(3), `token1_symbol`(4), `token0_decimals`(5), `token1_decimals`(6),
///          `fee`(7), `project`(8), `last_active_block`(9)
pub const QUERY_POOLS_WITH_METADATA: &str = r#"
WITH active_pools AS (
  SELECT
    t.pool_address,
    MIN(t.token_bought_address) AS token0,
    MIN(t.token_sold_address) AS token1,
    MIN(t.fee) AS fee,
    t.project,
    MAX(t.block_number) AS last_active_block
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_number >= {from_block}
    AND t.block_number <= {to_block}
  GROUP BY 1,4
)
SELECT
  ap.pool_address,
  ap.token0,
  ap.token1,
  COALESCE(t0.symbol, 'UNKNOWN') AS token0_symbol,
  COALESCE(t1.symbol, 'UNKNOWN') AS token1_symbol,
  COALESCE(t0.decimals, 18) AS token0_decimals,
  COALESCE(t1.decimals, 18) AS token1_decimals,
  ap.fee,
  ap.project,
  ap.last_active_block
FROM active_pools ap
LEFT JOIN tokens.erc20 t0
  ON t0.blockchain = '{chain}' AND t0.contract_address = ap.token0
LEFT JOIN tokens.erc20 t1
  ON t1.blockchain = '{chain}' AND t1.contract_address = ap.token1
ORDER BY ap.last_active_block DESC
"#;

/// Discover pools of a specific DEX fork by factory address.
/// Use this for custom/fork DEXes not in the standard datasets.
///
/// Parameters: `{factory_address}`
/// Columns: `pool_address`(0), `token0`(1), `token1`(2), `creation_block`(3)
pub const QUERY_POOLS_BY_FACTORY_ADDRESS: &str = r#"
SELECT
  p.pair AS pool_address,
  p.token0,
  p.token1,
  p.evt_block_number AS creation_block
FROM uniswap_v2_{chain}.Factory_evt_PairCreated p
WHERE p.contract_address = '{factory_address}'::bytea
  AND p.evt_block_number >= {from_block}
  AND p.evt_block_number <= {to_block}
ORDER BY p.evt_block_number ASC
"#;

// ══════════════════════════════════════════════════════════════════════════
// Section 2: Trade & Swap Analysis
// ══════════════════════════════════════════════════════════════════════════

/// All DEX trades in a specific block (full detail).
///
/// Columns: `block_number`(0), `tx_hash`(1), `tx_index`(2), `token_bought_address`(3),
///          `token_sold_address`(4), `amount_bought`(5), `amount_sold`(6),
///          `amount_usd`(7), `taker`(8), `pool_address`(9), `project`(10), `block_time`(11)
pub const QUERY_TRADES_IN_BLOCK: &str = r#"
SELECT
  t.block_number,
  t.tx_hash,
  t.tx_index,
  t.token_bought_address,
  t.token_sold_address,
  t.amount_bought,
  t.amount_sold,
  t.amount_usd,
  t.taker,
  t.pool_address,
  t.project,
  t.block_time
FROM dex.trades t
WHERE t.blockchain = '{chain}'
  AND t.block_number = {block_number}
ORDER BY t.tx_index ASC
"#;

/// All DEX trades in a block range (for batch analysis).
///
/// Same columns as above. For large ranges, use sparingly or split into chunks.
pub const QUERY_TRADES_IN_RANGE: &str = r#"
SELECT
  t.block_number,
  t.tx_hash,
  t.tx_index,
  t.token_bought_address,
  t.token_sold_address,
  t.amount_bought,
  t.amount_sold,
  t.amount_usd,
  t.taker,
  t.pool_address,
  t.project,
  t.block_time
FROM dex.trades t
WHERE t.blockchain = '{chain}'
  AND t.block_number >= {from_block}
  AND t.block_number <= {to_block}
ORDER BY t.block_number, t.tx_index
"#;

/// All trades involving a specific pool (useful for analyzing a single pool).
///
/// Columns: `block_number`(0), `tx_hash`(1), `amount_usd`(2), `token_in`(3),
///          `token_out`(4), `taker`(5), `block_time`(6)
pub const QUERY_TRADES_BY_POOL: &str = r#"
SELECT
  t.block_number,
  t.tx_hash,
  t.amount_usd,
  t.token_bought_address,
  t.token_sold_address,
  t.taker,
  t.block_time
FROM dex.trades t
WHERE t.blockchain = '{chain}'
  AND t.pool_address = '{pool_address}'::bytea
  AND t.block_number >= {from_block}
  AND t.block_number <= {to_block}
ORDER BY t.block_number, t.tx_index
"#;

/// All trades involving a specific token pair (token0 → token1 swaps).
///
/// Columns: `block_number`(0), `tx_hash`(1), `pool_address`(2), `amount_usd`(3),
///          `amount_in`(4), `amount_out`(5), `taker`(6), `project`(7), `block_time`(8)
pub const QUERY_TRADES_BY_TOKEN_PAIR: &str = r#"
SELECT
  t.block_number,
  t.tx_hash,
  t.pool_address,
  t.amount_usd,
  t.amount_bought,
  t.amount_sold,
  t.taker,
  t.project,
  t.block_time
FROM dex.trades t
WHERE t.blockchain = '{chain}'
  AND t.token_bought_address = '{token_out}'::bytea
  AND t.token_sold_address = '{token_in}'::bytea
  AND t.block_number >= {from_block}
  AND t.block_number <= {to_block}
ORDER BY t.amount_usd DESC NULLS LAST
"#;

/// Large swaps (whale detection) over a block range — swaps with USD value above threshold.
///
/// Columns: `block_number`(0), `tx_hash`(1), `pool_address`(2), `token_in_symbol`(3),
///          `token_out_symbol`(4), `amount_usd`(5), `amount`(6), `taker`(7), `block_time`(8)
pub const QUERY_LARGE_SWAPS: &str = r#"
SELECT
  t.block_number,
  t.tx_hash,
  t.pool_address,
  t.token_bought_symbol AS token_in_symbol,
  t.token_sold_symbol AS token_out_symbol,
  t.amount_usd,
  CASE WHEN t.amount_usd > 0
    THEN CAST(t.amount_bought AS VARCHAR)
    ELSE CAST(t.amount_sold AS VARCHAR)
  END AS amount,
  t.taker,
  t.block_time
FROM dex.trades t
WHERE t.blockchain = '{chain}'
  AND t.block_number >= {from_block}
  AND t.block_number <= {to_block}
  AND t.amount_usd >= {min_usd}
ORDER BY t.amount_usd DESC
"#;

/// Verify a specific trade by tx_hash.
///
/// Columns: `block_number`(0), `tx_hash`(1), `token_bought_address`(2),
///          `token_sold_address`(3), `amount_bought`(4), `amount_sold`(5),
///          `amount_usd`(6), `taker`(7), `pool_address`(8), `project`(9)
pub const QUERY_VERIFY_TRADE_BY_TX: &str = r#"
SELECT
  t.block_number,
  t.tx_hash,
  t.token_bought_address,
  t.token_sold_address,
  t.amount_bought,
  t.amount_sold,
  t.amount_usd,
  t.taker,
  t.pool_address,
  t.project
FROM dex.trades t
WHERE t.blockchain = '{chain}'
  AND t.tx_hash = '{tx_hash}'::bytea
  AND t.block_number = {block_number}
LIMIT 1
"#;

// ══════════════════════════════════════════════════════════════════════════
// Section 3: MEV Detection
// ══════════════════════════════════════════════════════════════════════════

/// All known sandwich attacks in a block range from Dune's curated dataset.
///
/// Columns: `block_number`(0), `victim_tx_hash`(1), `front_tx_hash`(2),
///          `back_tx_hash`(3), `sandwich_type`(4), `pool_address`(5), `mev_profit_eth`(6)
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

/// Sandwich attacks in a specific block.
///
/// Columns: same as above.
pub const QUERY_SANDWICHES_BY_BLOCK: &str = r#"
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
  AND s.block_number = {block_number}
ORDER BY s.victim_tx_hash
"#;

/// Sandwich attacks in a time range.
///
/// Parameters: `{from_time}` and `{to_time}` in ISO-8601 format.
pub const QUERY_SANDWICHES_BY_TIME: &str = r#"
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
  AND s.block_time >= TIMESTAMP '{from_time}'
  AND s.block_time < TIMESTAMP '{to_time}'
ORDER BY s.block_time
"#;

/// Detect arbitrage transactions: one tx that swaps through >= 2 different pools.
/// Uses a CTE to find multi-pool transactions and extracts start/end pools and tokens.
///
/// Columns: `block_number`(0), `tx_hash`(1), `pool_a`(2), `pool_b`(3),
///          `token_in`(4), `token_out`(5), `amount_usd`(6)
pub const QUERY_ARBITRAGES_BY_RANGE: &str = r#"
WITH tx_pools AS (
  SELECT
    t.blockchain,
    t.block_number,
    t.tx_hash,
    t.pool_address,
    t.token_bought_address AS token_out,
    t.token_sold_address AS token_in,
    t.amount_usd,
    t.tx_index,
    COUNT(*) OVER (PARTITION BY t.blockchain, t.block_number, t.tx_hash) AS pool_count,
    ROW_NUMBER() OVER (PARTITION BY t.blockchain, t.block_number, t.tx_hash ORDER BY t.tx_index) AS rn_asc,
    ROW_NUMBER() OVER (PARTITION BY t.blockchain, t.block_number, t.tx_hash ORDER BY t.tx_index DESC) AS rn_desc
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_number >= {from_block}
    AND t.block_number <= {to_block}
)
SELECT DISTINCT
  tp.block_number,
  tp.tx_hash,
  MAX(CASE WHEN tp.rn_asc = 1 THEN tp.pool_address END) OVER (PARTITION BY tp.tx_hash) AS pool_a,
  MAX(CASE WHEN tp.rn_desc = 1 THEN tp.pool_address END) OVER (PARTITION BY tp.tx_hash) AS pool_b,
  MAX(CASE WHEN tp.rn_asc = 1 THEN tp.token_in END) OVER (PARTITION BY tp.tx_hash) AS token_in,
  MAX(CASE WHEN tp.rn_desc = 1 THEN tp.token_out END) OVER (PARTITION BY tp.tx_hash) AS token_out,
  MAX(tp.amount_usd) OVER (PARTITION BY tp.tx_hash) AS amount_usd
FROM tx_pools tp
WHERE tp.pool_count >= 2
ORDER BY tp.block_number, tp.tx_hash
"#;

/// Arbitrage transactions in a specific block.
pub const QUERY_ARBITRAGES_BY_BLOCK: &str = r#"
WITH tx_pools AS (
  SELECT
    t.tx_hash,
    t.pool_address,
    t.token_bought_address AS token_out,
    t.token_sold_address AS token_in,
    t.amount_usd,
    t.tx_index,
    COUNT(*) OVER (PARTITION BY t.tx_hash) AS pool_count,
    ROW_NUMBER() OVER (PARTITION BY t.tx_hash ORDER BY t.tx_index) AS rn_asc,
    ROW_NUMBER() OVER (PARTITION BY t.tx_hash ORDER BY t.tx_index DESC) AS rn_desc
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_number = {block_number}
)
SELECT DISTINCT
  {block_number} AS block_number,
  tp.tx_hash,
  MAX(CASE WHEN tp.rn_asc = 1 THEN tp.pool_address END) OVER (PARTITION BY tp.tx_hash) AS pool_a,
  MAX(CASE WHEN tp.rn_desc = 1 THEN tp.pool_address END) OVER (PARTITION BY tp.tx_hash) AS pool_b,
  MAX(CASE WHEN tp.rn_asc = 1 THEN tp.token_in END) OVER (PARTITION BY tp.tx_hash) AS token_in,
  MAX(CASE WHEN tp.rn_desc = 1 THEN tp.token_out END) OVER (PARTITION BY tp.tx_hash) AS token_out,
  MAX(tp.amount_usd) OVER (PARTITION BY tp.tx_hash) AS amount_usd
FROM tx_pools tp
WHERE tp.pool_count >= 2
ORDER BY tp.tx_hash
"#;

/// Arbitrage transactions in a time range.
pub const QUERY_ARBITRAGES_BY_TIME: &str = r#"
WITH tx_pools AS (
  SELECT
    t.tx_hash,
    t.block_number,
    t.pool_address,
    t.token_bought_address AS token_out,
    t.token_sold_address AS token_in,
    t.amount_usd,
    t.tx_index,
    COUNT(*) OVER (PARTITION BY t.tx_hash) AS pool_count,
    ROW_NUMBER() OVER (PARTITION BY t.tx_hash ORDER BY t.tx_index) AS rn_asc,
    ROW_NUMBER() OVER (PARTITION BY t.tx_hash ORDER BY t.tx_index DESC) AS rn_desc
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_time >= TIMESTAMP '{from_time}'
    AND t.block_time < TIMESTAMP '{to_time}'
)
SELECT DISTINCT
  tp.block_number,
  tp.tx_hash,
  MAX(CASE WHEN tp.rn_asc = 1 THEN tp.pool_address END) OVER (PARTITION BY tp.tx_hash) AS pool_a,
  MAX(CASE WHEN tp.rn_desc = 1 THEN tp.pool_address END) OVER (PARTITION BY tp.tx_hash) AS pool_b,
  MAX(CASE WHEN tp.rn_asc = 1 THEN tp.token_in END) OVER (PARTITION BY tp.tx_hash) AS token_in,
  MAX(CASE WHEN tp.rn_desc = 1 THEN tp.token_out END) OVER (PARTITION BY tp.tx_hash) AS token_out,
  MAX(tp.amount_usd) OVER (PARTITION BY tp.tx_hash) AS amount_usd
FROM tx_pools tp
WHERE tp.pool_count >= 2
ORDER BY tp.block_number, tp.tx_hash
"#;

/// All flash loan events from Dune's consolidated `lending.flashloans` dataset.
///
/// Columns: `block_number`(0), `tx_hash`(1), `protocol`(2), `token_address`(3),
///          `amount_usd`(4), `amount`(5), `fee`(6)
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

/// Flash loans in a specific block.
pub const QUERY_FLASH_LOANS_BY_BLOCK: &str = r#"
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
  AND f.block_number = {block_number}
ORDER BY f.tx_hash
"#;

/// Aave V3 liquidation events — most liquid MEV opportunity on lending protocols.
///
/// Columns: `block_number`(0), `tx_hash`(1), `user`(2), `liquidator`(3),
///          `collateral_asset`(4), `debt_asset`(5), `collateral_amount`(6),
///          `debt_to_cover`(7), `block_time`(8)
pub const QUERY_AAVE_V3_LIQUIDATIONS: &str = r#"
SELECT
  l.evt_block_number AS block_number,
  l.evt_tx_hash AS tx_hash,
  l.user,
  l.liquidator,
  l.collateralAsset AS collateral_asset,
  l.debtAsset AS debt_asset,
  l.liquidatedCollateralAmount AS collateral_amount,
  l.debtToCover AS debt_to_cover,
  l.evt_block_time AS block_time
FROM aave_v3_{chain}.Pool_evt_LiquidationCall l
WHERE l.evt_block_number >= {from_block}
  AND l.evt_block_number <= {to_block}
ORDER BY l.evt_block_number, l.evt_tx_hash
"#;

/// Aave V3 liquidations in a specific block.
pub const QUERY_AAVE_V3_LIQUIDATIONS_BY_BLOCK: &str = r#"
SELECT
  l.evt_block_number AS block_number,
  l.evt_tx_hash AS tx_hash,
  l.user,
  l.liquidator,
  l.collateralAsset AS collateral_asset,
  l.debtAsset AS debt_asset,
  l.liquidatedCollateralAmount AS collateral_amount,
  l.debtToCover AS debt_to_cover,
  l.evt_block_time AS block_time
FROM aave_v3_{chain}.Pool_evt_LiquidationCall l
WHERE l.evt_block_number = {block_number}
ORDER BY l.evt_tx_hash
"#;

/// Compound V3 liquidation events.
///
/// Columns: `block_number`(0), `tx_hash`(1), `user`(2), `liquidator`(3),
///          `collateral_asset`(4), `debt_asset`(5), `collateral_amount`(6),
///          `debt_amount`(7), `block_time`(8)
pub const QUERY_COMPOUND_V3_LIQUIDATIONS: &str = r#"
SELECT
  a.evt_block_number AS block_number,
  a.evt_tx_hash AS tx_hash,
  a.actor AS user,
  a.liquidator,
  a.collateralAsset AS collateral_asset,
  a.baseAsset AS debt_asset,
  a.collateralAmount AS collateral_amount,
  a.baseAmount AS debt_amount,
  a.evt_block_time AS block_time
FROM compound_v3_{chain}.Comet_evt_Absorb a
WHERE a.evt_block_number >= {from_block}
  AND a.evt_block_number <= {to_block}
ORDER BY a.evt_block_number, a.evt_tx_hash
"#;

/// Combined liquidation events from all lending protocols.
///
/// Columns: `block_number`(0), `tx_hash`(1), `protocol`(2), `user`(3), `liquidator`(4),
///          `collateral_token`(5), `debt_token`(6), `collateral_amount`(7),
///          `debt_amount`(8), `amount_usd`(9), `block_time`(10)
pub const QUERY_LIQUIDATIONS_ALL: &str = r#"
SELECT
  l.block_number,
  l.tx_hash,
  l.protocol,
  l.user,
  l.liquidator,
  l.collateral_token,
  l.debt_token,
  l.collateral_amount,
  l.debt_amount,
  l.amount_usd,
  l.block_time
FROM lending.liquidations l
WHERE l.blockchain = '{chain}'
  AND l.block_number >= {from_block}
  AND l.block_number <= {to_block}
ORDER BY l.block_number, l.tx_hash
"#;

/// Combined liquidations in a specific block.
pub const QUERY_LIQUIDATIONS_BY_BLOCK: &str = r#"
SELECT
  l.block_number,
  l.tx_hash,
  l.protocol,
  l.user,
  l.liquidator,
  l.collateral_token,
  l.debt_token,
  l.collateral_amount,
  l.debt_amount,
  l.amount_usd,
  l.block_time
FROM lending.liquidations l
WHERE l.blockchain = '{chain}'
  AND l.block_number = {block_number}
ORDER BY l.tx_hash
"#;

/// Verify if a specific tx_hash is part of a sandwich attack.
///
/// Columns: `block_number`(0), `victim_tx_hash`(1), `front_tx_hash`(2),
///          `back_tx_hash`(3), `sandwich_type`(4), `pool_address`(5)
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
  AND (s.victim_tx_hash = '{tx_hash}'::bytea
       OR s.front_tx_hash = '{tx_hash}'::bytea
       OR s.back_tx_hash = '{tx_hash}'::bytea)
LIMIT 10
"#;

/// Failed (reverted) transactions with value > threshold in a block range.
/// These are potential MEV signals: searchers bidding on failed bundles.
///
/// Columns: `block_number`(0), `tx_hash`(1), `from`(2), `to`(3),
///          `value_eth`(4), `gas_used`(5), `gas_price_gwei`(6), `error`(7)
pub const QUERY_FAILED_TXS: &str = r#"
SELECT
  tx.block_number,
  tx.hash AS tx_hash,
  tx.from_address,
  tx.to_address,
  CAST(tx.value AS DOUBLE) / 1e18 AS value_eth,
  receipt.gas_used,
  CAST(tx.gas_price AS DOUBLE) / 1e9 AS gas_price_gwei,
  receipt.error AS error_reason
FROM ethereum.transactions tx
JOIN ethereum.receipts receipt
  ON receipt.block_number = tx.block_number
  AND receipt.tx_hash = tx.hash
WHERE tx.block_number >= {from_block}
  AND tx.block_number <= {to_block}
  AND receipt.success = FALSE
  AND tx.value > 0
ORDER BY tx.value DESC
"#;

/// Failed transactions in a specific block.
pub const QUERY_FAILED_TXS_BY_BLOCK: &str = r#"
SELECT
  tx.block_number,
  tx.hash AS tx_hash,
  tx.from_address,
  tx.to_address,
  CAST(tx.value AS DOUBLE) / 1e18 AS value_eth,
  receipt.gas_used,
  CAST(tx.gas_price AS DOUBLE) / 1e9 AS gas_price_gwei,
  receipt.error AS error_reason
FROM ethereum.transactions tx
JOIN ethereum.receipts receipt
  ON receipt.block_number = tx.block_number
  AND receipt.tx_hash = tx.hash
WHERE tx.block_number = {block_number}
  AND receipt.success = FALSE
  AND tx.value > 0
ORDER BY tx.value DESC
"#;

// ══════════════════════════════════════════════════════════════════════════
// Section 4: Token & Price Data
// ══════════════════════════════════════════════════════════════════════════

/// Bulk ERC20 token metadata from Dune's curated `tokens.erc20` dataset.
/// Useful for enriching pool discovery results with token symbols.
///
/// Columns: `contract_address`(0), `symbol`(1), `decimals`(2), `name`(3)
pub const QUERY_TOKEN_METADATA: &str = r#"
SELECT
  t.contract_address,
  t.symbol,
  t.decimals,
  t.name
FROM tokens.erc20 t
WHERE t.blockchain = '{chain}'
  AND t.contract_address IN ({token_list})
"#;

/// All known tokens on a chain (useful for building a local token registry).
///
/// Columns: `contract_address`(0), `symbol`(1), `decimals`(2), `name`(3)
pub const QUERY_ALL_TOKENS: &str = r#"
SELECT
  t.contract_address,
  t.symbol,
  t.decimals,
  t.name
FROM tokens.erc20 t
WHERE t.blockchain = '{chain}'
ORDER BY t.symbol
"#;

/// Historical USD price for a token at a specific block time.
///
/// Columns: `minute`(0), `price`(1), `symbol`(2), `decimals`(3)
pub const QUERY_TOKEN_PRICE_AT_BLOCK: &str = r#"
SELECT
  p.minute,
  p.price,
  p.symbol,
  p.decimals
FROM prices.usd p
WHERE p.blockchain = '{chain}'
  AND p.contract_address = '{token_address}'::bytea
  AND p.minute <= TIMESTAMP '{block_timestamp}'
  AND p.minute >= TIMESTAMP '{block_timestamp}' - INTERVAL '1' hour
ORDER BY p.minute DESC
LIMIT 1
"#;

/// Price history for a token over a time window (for TWAP / price analysis).
///
/// Columns: `minute`(0), `price`(1), `symbol`(2)
pub const QUERY_TOKEN_PRICE_HISTORY: &str = r#"
SELECT
  p.minute,
  p.price,
  p.symbol
FROM prices.usd p
WHERE p.blockchain = '{chain}'
  AND p.contract_address = '{token_address}'::bytea
  AND p.minute >= TIMESTAMP '{from_time}'
  AND p.minute <= TIMESTAMP '{to_time}'
ORDER BY p.minute
"#;

// ══════════════════════════════════════════════════════════════════════════
// Section 5: Block & Gas Data
// ══════════════════════════════════════════════════════════════════════════

/// Block metadata: timestamp, gas used, base fee, tx count.
///
/// Columns: `block_number`(0), `block_time`(1), `timestamp_utc`(2),
///          `gas_used`(3), `gas_limit`(4), `base_fee_per_gas`(5), `tx_count`(6)
pub const QUERY_BLOCK_METADATA: &str = r#"
SELECT
  b.number AS block_number,
  b.time AS block_time,
  CAST(b.time AS VARCHAR) AS timestamp_utc,
  b.gas_used,
  b.gas_limit,
  CAST(b.base_fee_per_gas AS DOUBLE) / 1e9 AS base_fee_per_gas,
  b.tx_count
FROM ethereum.blocks b
WHERE b.number >= {from_block}
  AND b.number <= {to_block}
ORDER BY b.number
"#;

/// Block metadata for a single block.
pub const QUERY_SINGLE_BLOCK: &str = r#"
SELECT
  b.number AS block_number,
  b.time AS block_time,
  CAST(b.time AS VARCHAR) AS timestamp_utc,
  b.gas_used,
  b.gas_limit,
  CAST(b.base_fee_per_gas AS DOUBLE) / 1e9 AS base_fee_per_gas,
  b.tx_count
FROM ethereum.blocks b
WHERE b.number = {block_number}
"#;

/// Gas price distribution stats per block (for gas modeling).
/// Returns percentile gas prices to model MEV bidding competition.
///
/// Columns: `block_number`(0), `block_time`(1), `base_fee_gwei`(2),
///          `p25_gwei`(3), `p50_gwei`(4), `p75_gwei`(5), `p95_gwei`(6), `p99_gwei`(7)
pub const QUERY_GAS_PRICE_HISTORY: &str = r#"
WITH tx_gas AS (
  SELECT
    tx.block_number,
    CAST(tx.gas_price AS DOUBLE) / 1e9 AS gas_price_gwei
  FROM ethereum.transactions tx
  WHERE tx.block_number >= {from_block}
    AND tx.block_number <= {to_block}
    AND tx.gas_price > 0
)
SELECT
  tg.block_number,
  b.time AS block_time,
  CAST(b.base_fee_per_gas AS DOUBLE) / 1e9 AS base_fee_gwei,
  APPROX_PERCENTILE(tg.gas_price_gwei, 0.25) AS p25_gwei,
  APPROX_PERCENTILE(tg.gas_price_gwei, 0.50) AS p50_gwei,
  APPROX_PERCENTILE(tg.gas_price_gwei, 0.75) AS p75_gwei,
  APPROX_PERCENTILE(tg.gas_price_gwei, 0.95) AS p95_gwei,
  APPROX_PERCENTILE(tg.gas_price_gwei, 0.99) AS p99_gwei
FROM tx_gas tg
JOIN ethereum.blocks b ON b.number = tg.block_number
GROUP BY tg.block_number, b.time, b.base_fee_per_gas
ORDER BY tg.block_number
"#;

// ══════════════════════════════════════════════════════════════════════════
// Section 6: Pattern Analysis
// ══════════════════════════════════════════════════════════════════════════

/// Detects sandwiches within a block using Dune's pattern: if the same
/// address appears as front-runner and back-runner of a victim swap.
///
/// This is a simplified heuristic; for production, use `dex.sandwiches`.
/// Columns: `block_number`(0), `victim_tx_hash`(1), `front_tx_hash`(2),
///          `back_tx_hash`(3), `pool_address`(4), `profit_eth`(5)
pub const QUERY_SANDWICH_PATTERN: &str = r#"
WITH block_trades AS (
  SELECT
    t.block_number,
    t.tx_hash,
    t.tx_index,
    t.pool_address,
    t.tx_from,
    t.amount_usd,
    LAG(t.tx_from) OVER (PARTITION BY t.pool_address ORDER BY t.tx_index) AS prev_tx_from,
    LEAD(t.tx_from) OVER (PARTITION BY t.pool_address ORDER BY t.tx_index) AS next_tx_from,
    LAG(t.tx_hash) OVER (PARTITION BY t.pool_address ORDER BY t.tx_index) AS prev_tx_hash,
    LEAD(t.tx_hash) OVER (PARTITION BY t.pool_address ORDER BY t.tx_index) AS next_tx_hash
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_number = {block_number}
)
SELECT
  bt.block_number,
  bt.tx_hash AS victim_tx_hash,
  bt.prev_tx_hash AS front_tx_hash,
  bt.next_tx_hash AS back_tx_hash,
  bt.pool_address,
  NULL AS profit_eth
FROM block_trades bt
WHERE bt.prev_tx_from IS NOT NULL
  AND bt.next_tx_from IS NOT NULL
  AND bt.prev_tx_from = bt.next_tx_from
  AND bt.prev_tx_from != bt.tx_from
ORDER BY bt.tx_index
"#;

/// Detect potential JIT (Just-In-Time) liquidity: a tx that adds liquidity
/// right before a large swap, then removes it right after.
///
/// Columns: `block_number`(0), `large_swap_tx`(1), `mint_tx`(2), `burn_tx`(3),
///          `pool_address`(4), `swap_amount_usd`(5), `profit_est_usd`(6)
pub const QUERY_JIT_PATTERN: &str = r#"
WITH block_events AS (
  SELECT
    evt_block_number AS block_number,
    evt_tx_hash AS tx_hash,
    evt_index,
    contract_address AS pool_address,
    'mint' AS event_type,
    NULL AS amount_usd
  FROM uniswap_v3_{chain}.Pool_evt_Mint
  WHERE evt_block_number = {block_number}
  UNION ALL
  SELECT
    evt_block_number,
    evt_tx_hash,
    evt_index,
    contract_address,
    'burn',
    NULL
  FROM uniswap_v3_{chain}.Pool_evt_Burn
  WHERE evt_block_number = {block_number}
  UNION ALL
  SELECT
    t.block_number,
    t.tx_hash,
    t.tx_index,
    t.pool_address,
    'swap',
    t.amount_usd
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_number = {block_number}
)
SELECT * FROM block_events ORDER BY pool_address, evt_index
"#;

/// Detect time-bandit reorg opportunities: blocks where the profit
/// from reorging a previous block exceeds the cost.
/// Identifies blocks with high value that attackers might want to replace.
///
/// Columns: `block_number`(0), `total_mev_value_eth`(1), `total_tx_value_eth`(2),
///          `tx_count`(3), `base_fee_gwei`(4), `timestamp`(5)
pub const QUERY_HIGH_VALUE_BLOCKS: &str = r#"
WITH block_value AS (
  SELECT
    t.block_number,
    SUM(COALESCE(t.amount_usd, 0)) AS total_mev_value_usd,
    COUNT(DISTINCT t.tx_hash) AS tx_count
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_number >= {from_block}
    AND t.block_number <= {to_block}
  GROUP BY t.block_number
)
SELECT
  bv.block_number,
  (bv.total_mev_value_usd / NULLIF(p.price, 0)) / 1e18 AS total_mev_value_eth,
  NULL AS total_tx_value_eth,
  bv.tx_count,
  CAST(blk.base_fee_per_gas AS DOUBLE) / 1e9 AS base_fee_gwei,
  blk.time AS timestamp
FROM block_value bv
JOIN ethereum.blocks blk ON blk.number = bv.block_number
LEFT JOIN prices.usd p
  ON p.blockchain = '{chain}'
  AND p.contract_address = 0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2
  AND p.minute = DATE_TRUNC('minute', blk.time)
ORDER BY bv.total_mev_value_usd DESC
"#;

/// Pool liquidity snapshots — reserve and TVL info for DEX pools
/// at the latest block in a given range.
///
/// Columns: `pool_address`(0), `project`(1), `token0_address`(2), `token1_address`(3),
///          `token0_symbol`(4), `token1_symbol`(5), `reserve0`(6), `reserve1`(7),
///          `tvl_usd`(8)
pub const QUERY_POOL_LIQUIDITY: &str = r#"
WITH latest_trades AS (
  SELECT DISTINCT ON (t.pool_address)
    t.pool_address,
    t.project,
    t.token_bought_address AS token0_address,
    t.token_sold_address AS token1_address,
    t.token_bought_symbol AS token0_symbol,
    t.token_sold_symbol AS token1_symbol,
    SUM(t.amount_bought) OVER (PARTITION BY t.pool_address, t.token_bought_address) AS reserve0,
    SUM(t.amount_sold) OVER (PARTITION BY t.pool_address, t.token_sold_address) AS reserve1,
    t.tvl_usd,
    t.block_number
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_number <= {to_block}
    AND t.block_number >= {to_block} - 5000
    AND t.tvl_usd IS NOT NULL
)
SELECT
  lt.pool_address,
  lt.project,
  lt.token0_address,
  lt.token1_address,
  lt.token0_symbol,
  lt.token1_symbol,
  lt.reserve0,
  lt.reserve1,
  lt.tvl_usd
FROM latest_trades lt
WHERE lt.tvl_usd > 0
ORDER BY lt.tvl_usd DESC
"#;

/// Hourly average gas price for identifying historically cheap periods.
/// Useful for scheduling execution, gas optimization, and cost modeling.
///
/// Columns: `hour`(0) [ISO-8601], `avg_gas_price_gwei`(1), `min_gas_price_gwei`(2),
///          `max_gas_price_gwei`(3), `median_gas_price_gwei`(4), `tx_count`(5)
pub const QUERY_GAS_BY_HOUR: &str = r#"
SELECT
  DATE_TRUNC('hour', tx.block_time) AS hour,
  AVG(CAST(tx.gas_price AS DOUBLE) / 1e9) AS avg_gas_price_gwei,
  MIN(CAST(tx.gas_price AS DOUBLE) / 1e9) AS min_gas_price_gwei,
  MAX(CAST(tx.gas_price AS DOUBLE) / 1e9) AS max_gas_price_gwei,
  APPROX_PERCENTILE(CAST(tx.gas_price AS DOUBLE) / 1e9, 0.50) AS median_gas_price_gwei,
  COUNT(*) AS tx_count
FROM ethereum.transactions tx
WHERE tx.blockchain = '{chain}'
  AND tx.block_time >= TIMESTAMP '{from_time}'
  AND tx.block_time < TIMESTAMP '{to_time}'
  AND tx.gas_price > 0
GROUP BY 1
ORDER BY 1
"#;

/// Large token transfers (whale detection) across any wallet or contract.
/// Captures CEX deposits/withdrawals, OTC deals, and whale accumulation
/// before they appear as DEX trades — a leading indicator for volatility.
///
/// Columns: `block_number`(0), `tx_hash`(1), `symbol`(2), `amount`(3),
///          `amount_usd`(4), `from_address`(5), `to_address`(6), `block_time`(7)
pub const QUERY_WHALE_TRANSFERS: &str = r#"
SELECT
  tr.evt_block_number AS block_number,
  tr.evt_tx_hash AS tx_hash,
  tok.symbol,
  CAST(tr.value AS DOUBLE) / POWER(10, COALESCE(tok.decimals, 18)) AS amount,
  (CAST(tr.value AS DOUBLE) / POWER(10, COALESCE(tok.decimals, 18))) * COALESCE(p.price, 0) AS amount_usd,
  tr."from",
  tr."to",
  tr.evt_block_time AS block_time
FROM tokens.{chain}.transfers tr
JOIN tokens.erc20 tok
  ON tok.blockchain = '{chain}'
  AND tok.contract_address = tr.contract_address
LEFT JOIN prices.usd p
  ON p.blockchain = '{chain}'
  AND p.contract_address = tr.contract_address
  AND p.minute = DATE_TRUNC('minute', tr.evt_block_time)
WHERE tr.evt_block_number >= {from_block}
  AND tr.evt_block_number <= {to_block}
  AND CAST(tr.value AS DOUBLE) / POWER(10, COALESCE(tok.decimals, 18)) * COALESCE(p.price, 0) > {min_usd}
ORDER BY amount_usd DESC
"#;

/// Large transfers in a specific block.
pub const QUERY_WHALE_TRANSFERS_BY_BLOCK: &str = r#"
SELECT
  tr.evt_block_number,
  tr.evt_tx_hash,
  tok.symbol,
  CAST(tr.value AS DOUBLE) / POWER(10, COALESCE(tok.decimals, 18)) AS amount,
  (CAST(tr.value AS DOUBLE) / POWER(10, COALESCE(tok.decimals, 18))) * COALESCE(p.price, 0) AS amount_usd,
  tr."from",
  tr."to",
  tr.evt_block_time
FROM tokens.{chain}.transfers tr
JOIN tokens.erc20 tok
  ON tok.blockchain = '{chain}'
  AND tok.contract_address = tr.contract_address
LEFT JOIN prices.usd p
  ON p.blockchain = '{chain}'
  AND p.contract_address = tr.contract_address
  AND p.minute = DATE_TRUNC('minute', tr.evt_block_time)
WHERE tr.evt_block_number = {block_number}
  AND CAST(tr.value AS DOUBLE) / POWER(10, COALESCE(tok.decimals, 18)) * COALESCE(p.price, 0) > {min_usd}
ORDER BY amount_usd DESC
"#;

/// Cross-chain bridge transfer volumes by blockchain.
/// Helps identify capital flows that create arbitrage opportunities
/// between chains (temporary price dislocations).
///
/// Columns: `blockchain`(0), `total_bridged_usd`(1), `tx_count`(2),
///          `from_time`(3), `to_time`(4)
pub const QUERY_BRIDGE_FLOWS: &str = r#"
SELECT
  b.blockchain,
  SUM(b.amount_usd) AS total_bridged_usd,
  COUNT(DISTINCT b.tx_hash) AS tx_count,
  MIN(b.block_time) AS from_time,
  MAX(b.block_time) AS to_time
FROM bridges.transfers b
WHERE b.blockchain = '{chain}'
  AND b.block_time >= TIMESTAMP '{from_time}'
  AND b.block_time < TIMESTAMP '{to_time}'
GROUP BY b.blockchain
ORDER BY total_bridged_usd DESC
"#;

/// Cross-chain bridge flows aggregated, showing net flow per chain.
/// Positive = net inflow, Negative = net outflow.
///
/// Columns: `blockchain`(0), `total_inflow_usd`(1), `total_outflow_usd`(2),
///          `net_flow_usd`(3), `tx_count`(4)
pub const QUERY_BRIDGE_FLOWS_NET: &str = r#"
WITH direction AS (
  SELECT
    CASE
      WHEN b.blockchain = '{chain}' THEN b.blockchain
      ELSE b.blockchain
    END AS chain_name,
    CASE WHEN b.blockchain = '{chain}' THEN b.amount_usd ELSE 0 END AS inflow,
    CASE WHEN b.blockchain != '{chain}' THEN b.amount_usd ELSE 0 END AS outflow,
    b.tx_hash,
    b.block_time
  FROM bridges.transfers b
  WHERE (b.blockchain = '{chain}' OR b.destination_blockchain = '{chain}')
    AND b.block_time >= TIMESTAMP '{from_time}'
    AND b.block_time < TIMESTAMP '{to_time}'
)
SELECT
  chain_name,
  SUM(inflow) AS total_inflow_usd,
  SUM(outflow) AS total_outflow_usd,
  SUM(inflow) - SUM(outflow) AS net_flow_usd,
  COUNT(DISTINCT tx_hash) AS tx_count
FROM direction
GROUP BY chain_name
ORDER BY net_flow_usd DESC
"#;

// ══════════════════════════════════════════════════════════════════════════
// Section 7: Cross-Chain & Aggregation
// ══════════════════════════════════════════════════════════════════════════

/// Price of a token at a specific block number using nearby trades.
/// Fallback when `prices.usd` doesn't have the token.
///
/// Columns: `block_number`(0), `price_usd`(1), `source_pool`(2), `confidence`(3)
pub const QUERY_TOKEN_PRICE_VIA_TRADES: &str = r#"
WITH near_swaps AS (
  SELECT
    t.block_number,
    t.amount_usd / NULLIF(ABS(CAST(t.amount_bought AS DOUBLE)), 0) AS price_usd,
    t.pool_address,
    t.amount_usd,
    ABS(CAST(t.block_number AS BIGINT) - CAST({block_number} AS BIGINT)) AS block_dist
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND (t.token_bought_address = '{token_address}'::bytea
         OR t.token_sold_address = '{token_address}'::bytea)
    AND t.amount_usd > 1
    AND t.block_number BETWEEN {from_block} AND {to_block}
)
SELECT
  ns.block_number,
  AVG(ns.price_usd) AS price_usd,
  ns.pool_address,
  CASE
    WHEN COUNT(*) >= 10 THEN 'high'
    WHEN COUNT(*) >= 3 THEN 'medium'
    ELSE 'low'
  END AS confidence
FROM near_swaps ns
GROUP BY ns.block_number, ns.pool_address
ORDER BY ns.block_number DESC
LIMIT 1
"#;
