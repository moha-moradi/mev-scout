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
//! - `{block_month_min}` — lower bound for `block_month` partition pruning, e.g. `'2024-01-01'`
//! - `{from_time}` / `{to_time}` — ISO-8601 timestamps, e.g. `2024-01-01 00:00:00`
//! - `{pool_address}` / `{token_address}` / `{tx_hash}` — hex addresses with `0x` prefix
//! - `{token_list}` — comma-separated token addresses for `IN` clause
//! - `{min_usd}` — minimum USD threshold for filtering
//! - `{factory_address}` — DEX factory contract address
//!
//! # Column Order
//! The column index (0-based) in SELECT defines how Rust code reads the result.
//! Do NOT change column order without updating the corresponding fetch function.

// ══════════════════════════════════════════════════════════════════════════
// Section 1: Pool Discovery
// ══════════════════════════════════════════════════════════════════════════

/// V2-style pools via dex.trades (Uniswap V2, PancakeSwap V2, QuickSwap, SushiSwap, etc.).
///
/// Uses DuneSQL V2 `dex.trades` table. Columns: `pool_address`(0), `token0`(1), `token1`(2),
/// `creation_block`(3), `factory`(4)
pub const QUERY_V2_POOLS_BY_FACTORY: &str = r#"
WITH v2_pools AS (
  SELECT
    t.project_contract_address AS pool_address,
    CASE WHEN t.token_bought_address < t.token_sold_address THEN t.token_bought_address ELSE t.token_sold_address END AS token0,
    CASE WHEN t.token_bought_address < t.token_sold_address THEN t.token_sold_address ELSE t.token_bought_address END AS token1,
    MIN(t.block_number) AS creation_block
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_month >= DATE '{block_month_min}'
    AND t.block_number >= {from_block}
    AND t.block_number <= {to_block}
    AND t.version = '2'
  GROUP BY 1, 2, 3
)
SELECT
  pool_address,
  token0,
  token1,
  creation_block,
  NULL AS factory
FROM v2_pools
ORDER BY creation_block ASC
"#;

/// V3 pools via dex.trades (Uniswap V3, PancakeSwap V3, QuickSwap V3, etc.).
///
/// Fee is approximated from dex.trades; tick_spacing is derived from fee in Rust code.
/// Factory is unavailable from dex.trades and defaults to None.
/// Columns: `pool_address`(0), `token0`(1), `token1`(2), `creation_block`(3), `fee`(4),
///          `tick_spacing`(5), `factory`(6)
pub const QUERY_V3_POOLS_BY_FACTORY: &str = r#"
WITH v3_pools AS (
  SELECT
    t.project_contract_address AS pool_address,
    CASE WHEN t.token_bought_address < t.token_sold_address THEN t.token_bought_address ELSE t.token_sold_address END AS token0,
    CASE WHEN t.token_bought_address < t.token_sold_address THEN t.token_sold_address ELSE t.token_bought_address END AS token1,
    MIN(t.block_number) AS creation_block
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_month >= DATE '{block_month_min}'
    AND t.block_number >= {from_block}
    AND t.block_number <= {to_block}
    AND t.version = '3'
  GROUP BY 1, 2, 3
)
SELECT
  vp.pool_address,
  vp.token0,
  vp.token1,
  vp.creation_block,
  3000 AS fee,
  NULL AS tick_spacing,
  NULL AS factory
FROM v3_pools vp
ORDER BY vp.creation_block ASC
"#;

/// Curve pools via `PoolAdded` events from Curve's Registry and PoolRegistry contracts.
///
/// Uses chain-specific decoded event tables.
/// Columns: `pool_address`(0), `coins`(1) [JSON array of token addresses], `n_coins`(2),
///          `creation_block`(3), `pool_type`(4), `registry`(5)
pub const QUERY_CURVE_POOLS: &str = r#"
WITH curve_contracts AS (
  SELECT contract_address FROM {chain}.contracts WHERE namespace = 'curve' AND name = 'Registry'
  UNION
  SELECT contract_address FROM {chain}.contracts WHERE namespace = 'curve' AND name = 'PoolRegistry'
  UNION
  SELECT contract_address FROM {chain}.contracts WHERE namespace = 'curve' AND name = 'MetaPoolFactory'
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

/// Discover all active DEX pools from `dex.trades` — extracts unique pool addresses with metadata.
///
/// Uses DuneSQL V2 `dex.trades` with `project_contract_address`. Fee defaults are applied
/// in Rust code (3000 for V3, 30 for V2).
/// Columns: `pool_address`(0), `token0`(1), `token1`(2), `project`(3), `version`(4),
///          `creation_block`(5), `last_active_block`(6)
pub const QUERY_ALL_ACTIVE_POOLS: &str = r#"
WITH pool_stats AS (
  SELECT
    t.project_contract_address AS pool_address,
    CASE WHEN t.token_bought_address < t.token_sold_address THEN t.token_bought_address ELSE t.token_sold_address END AS token0,
    CASE WHEN t.token_bought_address < t.token_sold_address THEN t.token_sold_address ELSE t.token_bought_address END AS token1,
    t.project,
    t.version,
    MIN(t.block_number) AS creation_block,
    MAX(t.block_number) AS last_active_block
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_month >= DATE '{block_month_min}'
    AND t.block_number >= {from_block}
    AND t.block_number <= {to_block}
  GROUP BY 1,2,3,4,5
)
SELECT
  ps.pool_address,
  ps.token0,
  ps.token1,
  ps.project,
  ps.version,
  ps.creation_block,
  ps.last_active_block
FROM pool_stats ps
ORDER BY ps.last_active_block DESC
"#;

/// Get pools with token symbols and decimals (richest pool discovery query).
/// Uses distinct pools from dex.trades and joins with tokens.erc20.
///
/// Note: `dex.trades` does not expose `fee`, so the fee column returns 0.
/// Columns: `pool_address`(0), `token0_address`(1), `token1_address`(2),
///          `token0_symbol`(3), `token1_symbol`(4), `token0_decimals`(5), `token1_decimals`(6),
///          `fee`(7), `project`(8), `last_active_block`(9)
pub const QUERY_POOLS_WITH_METADATA: &str = r#"
WITH active_pools AS (
  SELECT
    t.project_contract_address AS pool_address,
    MIN(t.token_bought_address) AS token0,
    MIN(t.token_sold_address) AS token1,
    t.project,
    MAX(t.block_number) AS last_active_block
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_month >= DATE '{block_month_min}'
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
  0 AS fee,
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
/// Columns: `block_number`(0), `tx_hash`(1),
///          `token_bought_address`(2), `token_sold_address`(3),
///          `token_bought_amount`(4), `token_sold_amount`(5),
///          `amount_usd`(6), `taker`(7), `pool_address`(8), `project`(9), `block_time`(10)
pub const QUERY_TRADES_IN_BLOCK: &str = r#"
SELECT
  t.block_number,
  t.tx_hash,
  t.token_bought_address,
  t.token_sold_address,
  t.token_bought_amount,
  t.token_sold_amount,
  t.amount_usd,
  t.taker,
  t.pool_address,
  t.project,
  t.block_time
FROM dex.trades t
WHERE t.blockchain = '{chain}'
  AND t.block_number = {block_number}
ORDER BY t.tx_hash
"#;

/// All DEX trades in a block range (for batch analysis).
///
/// Same columns as above. For large ranges, use sparingly or split into chunks.
pub const QUERY_TRADES_IN_RANGE: &str = r#"
SELECT
  t.block_number,
  t.tx_hash,
  t.token_bought_address,
  t.token_sold_address,
  t.token_bought_amount,
  t.token_sold_amount,
  t.amount_usd,
  t.taker,
  t.pool_address,
  t.project,
  t.block_time
FROM dex.trades t
WHERE t.blockchain = '{chain}'
  AND t.block_month >= DATE '{block_month_min}'
  AND t.block_number >= {from_block}
  AND t.block_number <= {to_block}
ORDER BY t.block_number, t.tx_hash
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
  AND t.block_month >= DATE '{block_month_min}'
  AND t.pool_address = '{pool_address}'::bytea
  AND t.block_number >= {from_block}
  AND t.block_number <= {to_block}
ORDER BY t.block_number, t.tx_hash
"#;

/// All trades involving a specific token pair (token_in → token_out swaps).
///
/// Columns: `block_number`(0), `tx_hash`(1), `pool_address`(2), `amount_usd`(3),
///          `token_bought_amount`(4), `token_sold_amount`(5), `taker`(6), `project`(7), `block_time`(8)
pub const QUERY_TRADES_BY_TOKEN_PAIR: &str = r#"
SELECT
  t.block_number,
  t.tx_hash,
  t.pool_address,
  t.amount_usd,
  t.token_bought_amount,
  t.token_sold_amount,
  t.taker,
  t.project,
  t.block_time
FROM dex.trades t
WHERE t.blockchain = '{chain}'
  AND t.block_month >= DATE '{block_month_min}'
  AND t.token_bought_address = '{token_out}'::bytea
  AND t.token_sold_address = '{token_in}'::bytea
  AND t.block_number >= {from_block}
  AND t.block_number <= {to_block}
ORDER BY t.amount_usd DESC NULLS LAST
"#;

/// Large swaps (whale detection) over a block range — swaps with USD value above threshold.
///
/// Columns: `block_number`(0), `tx_hash`(1), `pool_address`(2), `token_out_symbol`(3),
///          `token_in_symbol`(4), `amount_usd`(5), `amount`(6), `taker`(7), `block_time`(8)
pub const QUERY_LARGE_SWAPS: &str = r#"
SELECT
  t.block_number,
  t.tx_hash,
  t.pool_address,
  t.token_bought_symbol AS token_out_symbol,
  t.token_sold_symbol AS token_in_symbol,
  t.amount_usd,
  CASE WHEN t.amount_usd > 0
    THEN CAST(t.token_bought_amount AS VARCHAR)
    ELSE CAST(t.token_sold_amount AS VARCHAR)
  END AS amount,
  t.taker,
  t.block_time
FROM dex.trades t
WHERE t.blockchain = '{chain}'
  AND t.block_month >= DATE '{block_month_min}'
  AND t.block_number >= {from_block}
  AND t.block_number <= {to_block}
  AND t.amount_usd >= {min_usd}
ORDER BY t.amount_usd DESC
"#;

/// Verify a specific trade by tx_hash.
///
/// Columns: `block_number`(0), `tx_hash`(1), `token_bought_address`(2),
///          `token_sold_address`(3), `token_bought_amount`(4), `token_sold_amount`(5),
///          `amount_usd`(6), `taker`(7), `pool_address`(8), `project`(9)
pub const QUERY_VERIFY_TRADE_BY_TX: &str = r#"
SELECT
  t.block_number,
  t.tx_hash,
  t.token_bought_address,
  t.token_sold_address,
  t.token_bought_amount,
  t.token_sold_amount,
  t.amount_usd,
  t.taker,
  t.pool_address,
  t.project
FROM dex.trades t
WHERE t.blockchain = '{chain}'
  AND t.block_month >= DATE '{block_month_min}'
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
  AND s.block_month >= DATE '{block_month_min}'
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

/// Victim trades that were sandwiched in a block range (complements dex.sandwiches).
///
/// Columns: `block_number`(0), `tx_hash`(1), `victim`(2),
///          `token_bought_symbol`(3), `token_sold_symbol`(4),
///          `amount_usd`(5), `pool_address`(6)
pub const QUERY_SANDWICHED_VICTIMS_BY_RANGE: &str = r#"
SELECT
  v.block_number,
  v.tx_hash,
  v.tx_from AS victim,
  v.token_bought_symbol,
  v.token_sold_symbol,
  v.amount_usd,
  v.project_contract_address AS pool_address
FROM dex.sandwiched v
WHERE v.blockchain = '{chain}'
  AND v.block_month >= DATE '{block_month_min}'
  AND v.block_number >= {from_block}
  AND v.block_number <= {to_block}
ORDER BY v.block_number, v.tx_hash
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
    COUNT(*) OVER (PARTITION BY t.blockchain, t.block_number, t.tx_hash) AS pool_count,
    ROW_NUMBER() OVER (PARTITION BY t.blockchain, t.block_number, t.tx_hash ORDER BY t.amount_usd DESC) AS rn_asc,
    ROW_NUMBER() OVER (PARTITION BY t.blockchain, t.block_number, t.tx_hash ORDER BY t.amount_usd ASC) AS rn_desc
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_month >= DATE '{block_month_min}'
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
    COUNT(*) OVER (PARTITION BY t.tx_hash) AS pool_count,
    ROW_NUMBER() OVER (PARTITION BY t.tx_hash ORDER BY t.amount_usd DESC) AS rn_asc,
    ROW_NUMBER() OVER (PARTITION BY t.tx_hash ORDER BY t.amount_usd ASC) AS rn_desc
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
    COUNT(*) OVER (PARTITION BY t.tx_hash) AS pool_count,
    ROW_NUMBER() OVER (PARTITION BY t.tx_hash ORDER BY t.amount_usd DESC) AS rn_asc,
    ROW_NUMBER() OVER (PARTITION BY t.tx_hash ORDER BY t.amount_usd ASC) AS rn_desc
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
  AND f.block_month >= DATE '{block_month_min}'
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

/// Combined liquidation events from the consolidated `lending.borrow` dataset.
///
/// Dune does not have `lending.liquidations`; liquidations are recorded in
/// `lending.borrow` with `transaction_type = 'liquidation'`.
/// Columns: `block_number`(0), `tx_hash`(1), `protocol`(2), `user`(3), `liquidator`(4),
///          `collateral_token`(5), `debt_token`(6), `collateral_amount`(7),
///          `debt_amount`(8), `amount_usd`(9), `block_time`(10)
pub const QUERY_LIQUIDATIONS_ALL: &str = r#"
SELECT
  l.block_number,
  l.tx_hash,
  l.project AS protocol,
  l.borrower AS user,
  l.tx_from AS liquidator,
  l.token_address AS collateral_token,
  l.token_address AS debt_token,
  l.amount_raw AS collateral_amount,
  l.amount_raw AS debt_amount,
  l.amount_usd,
  l.block_time
FROM lending.borrow l
WHERE l.blockchain = '{chain}'
  AND l.transaction_type = 'liquidation'
  AND l.block_month >= DATE '{block_month_min}'
  AND l.block_number >= {from_block}
  AND l.block_number <= {to_block}
ORDER BY l.block_number, l.tx_hash
"#;

/// Combined liquidations in a specific block.
pub const QUERY_LIQUIDATIONS_BY_BLOCK: &str = r#"
SELECT
  l.block_number,
  l.tx_hash,
  l.project AS protocol,
  l.borrower AS user,
  l.tx_from AS liquidator,
  l.token_address AS collateral_token,
  l.token_address AS debt_token,
  l.amount_raw AS collateral_amount,
  l.amount_raw AS debt_amount,
  l.amount_usd,
  l.block_time
FROM lending.borrow l
WHERE l.blockchain = '{chain}'
  AND l.transaction_type = 'liquidation'
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
  AND s.block_month >= DATE '{block_month_min}'
  AND s.block_number = {block_number}
  AND (s.victim_tx_hash = '{tx_hash}'::bytea
       OR s.front_tx_hash = '{tx_hash}'::bytea
       OR s.back_tx_hash = '{tx_hash}'::bytea)
LIMIT 10
"#;

/// Failed (reverted) transactions with value > threshold in a block range.
/// These are potential MEV signals: searchers bidding on failed bundles.
///
/// Uses the curated `gas.fees` dataset for cross-chain gas and fee data.
/// Columns: `block_number`(0), `tx_hash`(1), `from`(2), `to`(3),
///          `value_eth`(4), `gas_used`(5), `gas_price_gwei`(6), `error`(7)
pub const QUERY_FAILED_TXS: &str = r#"
SELECT
  g.block_number,
  g.tx_hash,
  g.tx_from AS from_address,
  g.tx_to AS to_address,
  CAST(g.tx_value AS DOUBLE) / 1e18 AS value_eth,
  g.gas_used,
  g.effective_gas_price / 1e9 AS gas_price_gwei,
  g.error AS error_reason
FROM gas.fees g
WHERE g.blockchain = '{chain}'
  AND g.block_date >= DATE '{block_month_min}'
  AND g.block_number >= {from_block}
  AND g.block_number <= {to_block}
  AND g.success = FALSE
  AND g.tx_value > 0
ORDER BY g.tx_value DESC
"#;

/// Failed transactions in a specific block.
pub const QUERY_FAILED_TXS_BY_BLOCK: &str = r#"
SELECT
  g.block_number,
  g.tx_hash,
  g.tx_from AS from_address,
  g.tx_to AS to_address,
  CAST(g.tx_value AS DOUBLE) / 1e18 AS value_eth,
  g.gas_used,
  g.effective_gas_price / 1e9 AS gas_price_gwei,
  g.error AS error_reason
FROM gas.fees g
WHERE g.blockchain = '{chain}'
  AND g.block_number = {block_number}
  AND g.success = FALSE
  AND g.tx_value > 0
ORDER BY g.tx_value DESC
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
/// Uses the hybrid `prices.minute` table (Coinpaprika + DEX-derived, 900K+ tokens).
/// Columns: `minute`(0), `price`(1), `symbol`(2), `decimals`(3)
pub const QUERY_TOKEN_PRICE_AT_BLOCK: &str = r#"
SELECT
  p.minute,
  p.price,
  p.symbol,
  p.decimals
FROM prices.minute p
WHERE p.blockchain = '{chain}'
  AND p.contract_address = '{token_address}'::bytea
  AND p.minute <= TIMESTAMP '{block_timestamp}'
  AND p.minute >= TIMESTAMP '{block_timestamp}' - INTERVAL '1' hour
ORDER BY p.minute DESC
LIMIT 1
"#;

/// Price history for a token over a time window (for TWAP / price analysis).
///
/// Uses the hybrid `prices.minute` table.
/// Columns: `minute`(0), `price`(1), `symbol`(2)
pub const QUERY_TOKEN_PRICE_HISTORY: &str = r#"
SELECT
  p.minute,
  p.price,
  p.symbol
FROM prices.minute p
WHERE p.blockchain = '{chain}'
  AND p.contract_address = '{token_address}'::bytea
  AND p.minute >= TIMESTAMP '{from_time}'
  AND p.minute <= TIMESTAMP '{to_time}'
ORDER BY p.minute
"#;

/// Latest USD price for a token (uses the `prices.latest` hybrid table).
///
/// Columns: `price`(0), `symbol`(1), `decimals`(2), `source`(3)
pub const QUERY_TOKEN_PRICE_LATEST: &str = r#"
SELECT
  p.price,
  p.symbol,
  p.decimals,
  p.source
FROM prices.latest p
WHERE p.blockchain = '{chain}'
  AND p.contract_address = '{token_address}'::bytea
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
/// Uses the curated `gas.fees` table for cross-chain coverage.
/// Columns: `block_number`(0), `block_time`(1), `base_fee_gwei`(2),
///          `p25_gwei`(3), `p50_gwei`(4), `p75_gwei`(5), `p95_gwei`(6), `p99_gwei`(7)
pub const QUERY_GAS_PRICE_HISTORY: &str = r#"
WITH tx_gas AS (
  SELECT
    g.block_number,
    g.effective_gas_price / 1e9 AS gas_price_gwei
  FROM gas.fees g
  WHERE g.blockchain = '{chain}'
    AND g.block_date >= DATE '{block_month_min}'
    AND g.block_number >= {from_block}
    AND g.block_number <= {to_block}
    AND g.effective_gas_price > 0
)
SELECT
  tg.block_number,
  MIN(b.time) AS block_time,
  MIN(CAST(b.base_fee_per_gas AS DOUBLE) / 1e9) AS base_fee_gwei,
  APPROX_PERCENTILE(tg.gas_price_gwei, 0.25) AS p25_gwei,
  APPROX_PERCENTILE(tg.gas_price_gwei, 0.50) AS p50_gwei,
  APPROX_PERCENTILE(tg.gas_price_gwei, 0.75) AS p75_gwei,
  APPROX_PERCENTILE(tg.gas_price_gwei, 0.95) AS p95_gwei,
  APPROX_PERCENTILE(tg.gas_price_gwei, 0.99) AS p99_gwei
FROM tx_gas tg
JOIN ethereum.blocks b ON b.number = tg.block_number
GROUP BY tg.block_number
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
    t.pool_address,
    t.tx_from,
    t.amount_usd,
    LAG(t.tx_from) OVER (PARTITION BY t.pool_address ORDER BY t.block_number, t.tx_hash) AS prev_tx_from,
    LEAD(t.tx_from) OVER (PARTITION BY t.pool_address ORDER BY t.block_number, t.tx_hash) AS next_tx_from,
    LAG(t.tx_hash) OVER (PARTITION BY t.pool_address ORDER BY t.block_number, t.tx_hash) AS prev_tx_hash,
    LEAD(t.tx_hash) OVER (PARTITION BY t.pool_address ORDER BY t.block_number, t.tx_hash) AS next_tx_hash
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
ORDER BY bt.tx_hash
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
    contract_address AS pool_address,
    'mint' AS event_type,
    NULL AS amount_usd
  FROM uniswap_v3_{chain}.Pool_evt_Mint
  WHERE evt_block_number = {block_number}
  UNION ALL
  SELECT
    evt_block_number,
    evt_tx_hash,
    contract_address,
    'burn',
    NULL
  FROM uniswap_v3_{chain}.Pool_evt_Burn
  WHERE evt_block_number = {block_number}
  UNION ALL
  SELECT
    t.block_number,
    t.tx_hash,
    t.pool_address,
    'swap',
    t.amount_usd
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_number = {block_number}
)
SELECT * FROM block_events ORDER BY pool_address, tx_hash
"#;

/// Detect time-bandit reorg opportunities: blocks where the profit
/// from reorging a previous block exceeds the cost.
/// Identifies blocks with high value that attackers might want to replace.
///
/// Uses hybrid `prices.minute` for ETH price conversion.
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
    AND t.block_month >= DATE '{block_month_min}'
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
LEFT JOIN prices.minute p
  ON p.blockchain = '{chain}'
  AND p.contract_address = 0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2
  AND p.minute = DATE_TRUNC('minute', blk.time)
ORDER BY bv.total_mev_value_usd DESC
"#;

/// Pool liquidity snapshots — reserve and TVL info for DEX pools
/// at the latest block in a given range.
///
/// Uses `ROW_NUMBER()` (Trino-compatible) instead of PostgreSQL `DISTINCT ON`.
/// Columns: `pool_address`(0), `project`(1), `token0_address`(2), `token1_address`(3),
///          `token0_symbol`(4), `token1_symbol`(5), `reserve0`(6), `reserve1`(7),
///          `tvl_usd`(8)
pub const QUERY_POOL_LIQUIDITY: &str = r#"
WITH ranked_trades AS (
  SELECT
    t.pool_address,
    t.project,
    t.token_bought_address AS token0_address,
    t.token_sold_address AS token1_address,
    t.token_bought_symbol AS token0_symbol,
    t.token_sold_symbol AS token1_symbol,
    t.token_bought_amount AS reserve0,
    t.token_sold_amount AS reserve1,
    t.amount_usd AS tvl_usd,
    t.block_number,
    ROW_NUMBER() OVER (
      PARTITION BY t.pool_address
      ORDER BY t.block_number DESC
    ) AS rn
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_month >= DATE '{block_month_min}'
    AND t.block_number <= {to_block}
    AND t.block_number >= {to_block} - 10000
    AND t.amount_usd IS NOT NULL
)
SELECT
  rt.pool_address,
  rt.project,
  rt.token0_address,
  rt.token1_address,
  rt.token0_symbol,
  rt.token1_symbol,
  rt.reserve0,
  rt.reserve1,
  rt.tvl_usd
FROM ranked_trades rt
WHERE rt.rn = 1
  AND rt.tvl_usd > 0
ORDER BY rt.tvl_usd DESC
"#;

/// Hourly average gas price for identifying historically cheap periods.
/// Useful for scheduling execution, gas optimization, and cost modeling.
///
/// Uses the curated `gas.fees` table for cross-chain coverage.
/// Columns: `hour`(0) [ISO-8601], `avg_gas_price_gwei`(1), `min_gas_price_gwei`(2),
///          `max_gas_price_gwei`(3), `median_gas_price_gwei`(4), `tx_count`(5)
pub const QUERY_GAS_BY_HOUR: &str = r#"
SELECT
  DATE_TRUNC('hour', g.block_time) AS hour,
  AVG(g.effective_gas_price / 1e9) AS avg_gas_price_gwei,
  MIN(g.effective_gas_price / 1e9) AS min_gas_price_gwei,
  MAX(g.effective_gas_price / 1e9) AS max_gas_price_gwei,
  APPROX_PERCENTILE(g.effective_gas_price / 1e9, 0.50) AS median_gas_price_gwei,
  COUNT(*) AS tx_count
FROM gas.fees g
WHERE g.blockchain = '{chain}'
  AND g.block_time >= TIMESTAMP '{from_time}'
  AND g.block_time < TIMESTAMP '{to_time}'
  AND g.effective_gas_price > 0
GROUP BY 1
ORDER BY 1
"#;

/// Large token transfers (whale detection) across any wallet or contract.
/// Captures CEX deposits/withdrawals, OTC deals, and whale accumulation.
///
/// Uses the curated `tokens.transfers` table with pre-joined symbols and USD values.
/// Columns: `block_number`(0), `tx_hash`(1), `symbol`(2), `amount`(3),
///          `amount_usd`(4), `from_address`(5), `to_address`(6), `block_time`(7)
pub const QUERY_WHALE_TRANSFERS: &str = r#"
SELECT
  tr.block_number,
  tr.tx_hash,
  tr.symbol,
  tr.amount,
  tr.amount_usd,
  tr."from" AS from_address,
  tr."to" AS to_address,
  tr.block_time
FROM tokens.transfers tr
WHERE tr.blockchain = '{chain}'
  AND tr.block_date >= DATE '{block_month_min}'
  AND tr.block_number >= {from_block}
  AND tr.block_number <= {to_block}
  AND tr.amount_usd > {min_usd}
ORDER BY tr.amount_usd DESC
"#;

/// Large transfers in a specific block.
pub const QUERY_WHALE_TRANSFERS_BY_BLOCK: &str = r#"
SELECT
  tr.block_number,
  tr.tx_hash,
  tr.symbol,
  tr.amount,
  tr.amount_usd,
  tr."from" AS from_address,
  tr."to" AS to_address,
  tr.block_time
FROM tokens.transfers tr
WHERE tr.blockchain = '{chain}'
  AND tr.block_number = {block_number}
  AND tr.amount_usd > {min_usd}
ORDER BY tr.amount_usd DESC
"#;

/// Cross-chain bridge transfer volumes by blockchain.
/// Helps identify capital flows that create arbitrage opportunities
/// between chains (temporary price dislocations).
///
/// Uses the curated `bridges_evms.flows` table.
/// Columns: `blockchain`(0), `total_bridged_usd`(1), `tx_count`(2),
///          `from_time`(3), `to_time`(4)
pub const QUERY_BRIDGE_FLOWS: &str = r#"
SELECT
  b.destination_blockchain AS blockchain,
  SUM(b.amount_usd) AS total_bridged_usd,
  COUNT(DISTINCT b.tx_hash) AS tx_count,
  MIN(b.block_time) AS from_time,
  MAX(b.block_time) AS to_time
FROM bridges_evms.flows b
WHERE b.source_blockchain = '{chain}'
  AND b.block_time >= TIMESTAMP '{from_time}'
  AND b.block_time < TIMESTAMP '{to_time}'
GROUP BY b.destination_blockchain
ORDER BY total_bridged_usd DESC
"#;

/// Cross-chain bridge flows aggregated per chain (net flow).
/// Positive = net inflow, Negative = net outflow.
///
/// Uses the curated `bridges_evms.deposits` table.
/// Columns: `blockchain`(0), `total_inflow_usd`(1), `total_outflow_usd`(2),
///          `net_flow_usd`(3), `tx_count`(4)
pub const QUERY_BRIDGE_FLOWS_NET: &str = r#"
WITH inflows AS (
  SELECT
    d.withdrawal_chain AS chain_name,
    SUM(d.amount_usd) AS total_inflow,
    COUNT(DISTINCT d.tx_hash) AS tx_count_in
  FROM bridges_evms.deposits d
  WHERE d.withdrawal_chain = '{chain}'
    AND d.block_time >= TIMESTAMP '{from_time}'
    AND d.block_time < TIMESTAMP '{to_time}'
  GROUP BY d.withdrawal_chain
),
outflows AS (
  SELECT
    d.deposit_chain AS chain_name,
    SUM(d.amount_usd) AS total_outflow,
    COUNT(DISTINCT d.tx_hash) AS tx_count_out
  FROM bridges_evms.deposits d
  WHERE d.deposit_chain = '{chain}'
    AND d.block_time >= TIMESTAMP '{from_time}'
    AND d.block_time < TIMESTAMP '{to_time}'
  GROUP BY d.deposit_chain
)
SELECT
  COALESCE(i.chain_name, o.chain_name) AS blockchain,
  COALESCE(i.total_inflow, 0) AS total_inflow_usd,
  COALESCE(o.total_outflow, 0) AS total_outflow_usd,
  COALESCE(i.total_inflow, 0) - COALESCE(o.total_outflow, 0) AS net_flow_usd,
  COALESCE(i.tx_count_in, 0) + COALESCE(o.tx_count_out, 0) AS tx_count
FROM inflows i
FULL OUTER JOIN outflows o ON o.chain_name = i.chain_name
ORDER BY net_flow_usd DESC
"#;

// ══════════════════════════════════════════════════════════════════════════
// Section 7: Cross-Chain & Aggregation
// ══════════════════════════════════════════════════════════════════════════

/// Price of a token at a specific block number using nearby trades.
/// Fallback when `prices.minute` doesn't have the token.
///
/// Columns: `block_number`(0), `price_usd`(1), `source_pool`(2), `confidence`(3)
pub const QUERY_TOKEN_PRICE_VIA_TRADES: &str = r#"
WITH near_swaps AS (
  SELECT
    t.block_number,
    t.amount_usd / NULLIF(ABS(t.token_bought_amount), 0) AS price_usd,
    t.pool_address,
    t.amount_usd,
    ABS(CAST(t.block_number AS BIGINT) - CAST({block_number} AS BIGINT)) AS block_dist
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND (t.token_bought_address = '{token_address}'::bytea
         OR t.token_sold_address = '{token_address}'::bytea)
    AND t.amount_usd > 1
    AND t.block_month >= DATE '{block_month_min}'
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

/// Aggregator-routed trades (1inch, 0x, ParaSwap, etc.) in a block range.
/// Shows the user's intended single-hop trade rather than the multi-hop routing.
/// Useful for distinguishing organic trades from MEV activity.
///
/// Columns: `block_number`(0), `tx_hash`(1), `project`(2), `token_bought_address`(3),
///          `token_sold_address`(4), `token_bought_amount`(5), `token_sold_amount`(6),
///          `amount_usd`(7), `taker`(8), `block_time`(9)
pub const QUERY_AGGREGATOR_TRADES_IN_RANGE: &str = r#"
SELECT
  a.block_number,
  a.tx_hash,
  a.project,
  a.token_bought_address,
  a.token_sold_address,
  a.token_bought_amount,
  a.token_sold_amount,
  a.amount_usd,
  a.taker,
  a.block_time
FROM dex_aggregator.trades a
WHERE a.blockchain = '{chain}'
  AND a.block_month >= DATE '{block_month_min}'
  AND a.block_number >= {from_block}
  AND a.block_number <= {to_block}
ORDER BY a.block_number, a.tx_hash
"#;

/// Address labels from Dune's consolidated labels dataset.
/// Maps addresses to known entities (CEX, DEX, bridge, MEV bot, exploiter, etc.).
///
/// Columns: `address`(0), `name`(1), `category`(2), `blockchain`(3)
pub const QUERY_LABELS_BY_ADDRESSES: &str = r#"
SELECT
  l.address,
  l.name,
  l.category,
  l.blockchain
FROM labels.addresses l
WHERE l.blockchain = '{chain}'
  AND l.address IN ({address_list})
"#;

/// All address labels for a given category on a chain.
pub const QUERY_LABELS_BY_CATEGORY: &str = r#"
SELECT
  l.address,
  l.name,
  l.category,
  l.blockchain
FROM labels.addresses l
WHERE l.blockchain = '{chain}'
  AND l.category = '{category}'
"#;

/// Consolidated lending borrow events (including liquidations) from Dune's curated
/// `lending.borrow` dataset. Covers all lending protocols on all supported chains.
///
/// Columns: `block_number`(0), `tx_hash`(1), `protocol`(2), `transaction_type`(3),
///          `borrower`(4), `token_address`(5), `amount`(6), `amount_usd`(7), `block_time`(8)
pub const QUERY_LENDING_BORROW_BY_RANGE: &str = r#"
SELECT
  l.block_number,
  l.tx_hash,
  l.project AS protocol,
  l.transaction_type,
  l.borrower,
  l.token_address,
  l.amount,
  l.amount_usd,
  l.block_time
FROM lending.borrow l
WHERE l.blockchain = '{chain}'
  AND l.block_month >= DATE '{block_month_min}'
  AND l.block_number >= {from_block}
  AND l.block_number <= {to_block}
ORDER BY l.block_number, l.tx_hash
"#;

/// Consolidated lending supply events (deposits, withdrawals) from Dune's curated
/// `lending.supply` dataset.
///
/// Columns: `block_number`(0), `tx_hash`(1), `protocol`(2), `transaction_type`(3),
///          `supplier`(4), `token_address`(5), `amount`(6), `amount_usd`(7), `block_time`(8)
pub const QUERY_LENDING_SUPPLY_BY_RANGE: &str = r#"
SELECT
  l.block_number,
  l.tx_hash,
  l.project AS protocol,
  l.transaction_type,
  l.supplier,
  l.token_address,
  l.amount,
  l.amount_usd,
  l.block_time
FROM lending.supply l
WHERE l.blockchain = '{chain}'
  AND l.block_month >= DATE '{block_month_min}'
  AND l.block_number >= {from_block}
  AND l.block_number <= {to_block}
ORDER BY l.block_number, l.tx_hash
"#;

/// DEX-native flash loans (Balancer, Uniswap V3, dYdX) from `dex.flashloans`.
/// Complements the lending-protocol flash loans from `lending.flashloans`.
///
/// Columns: `block_number`(0), `tx_hash`(1), `project`(2), `token_address`(3),
///          `amount_usd`(4), `amount`(5), `fee`(6)
pub const QUERY_DEX_FLASH_LOANS_BY_RANGE: &str = r#"
SELECT
  f.block_number,
  f.tx_hash,
  f.project,
  f.token_address,
  f.amount_usd,
  f.amount,
  f.fee
FROM dex.flashloans f
WHERE f.blockchain = '{chain}'
  AND f.block_number >= {from_block}
  AND f.block_number <= {to_block}
ORDER BY f.block_number, f.tx_hash
"#;

/// Time-series scaffolding: continuous days from `utils.days`.
/// Useful for gap-free time-axis queries in dashboards and analytics.
pub const QUERY_UTILS_DAYS: &str = r#"
SELECT
  d.timestamp AS day
FROM utils.days d
WHERE d.timestamp >= TIMESTAMP '{from_time}'
  AND d.timestamp < TIMESTAMP '{to_time}'
ORDER BY d.timestamp
"#;

/// Time-series scaffolding: continuous hours from `utils.hours`.
pub const QUERY_UTILS_HOURS: &str = r#"
SELECT
  h.timestamp AS hour
FROM utils.hours h
WHERE h.timestamp >= TIMESTAMP '{from_time}'
  AND h.timestamp < TIMESTAMP '{to_time}'
ORDER BY h.timestamp
"#;

// ══════════════════════════════════════════════════════════════════════════
// Section 8: Strategy Validation
//
// Each query returns: opportunity_count, avg_profit_usd, total_profit_usd,
// period_start, period_end, period_days — for direct comparison against
// estimates in mev_strategies_analysis_summary.md.
// ══════════════════════════════════════════════════════════════════════════

/// Validate skim() capture opportunities: V2 pairs where balanceOf > reserve.
///
/// Dune does not expose `balanceOf` directly. Proxy: count V2 Sync events
/// where the token amounts suggest a balance-reserve discrepancy by comparing
/// consecutive Sync events on the same pair. When reserves drop without a
/// corresponding Swap, a skim() likely occurred.
///
/// Columns: `opportunity_count`(0), `avg_profit_usd`(1), `total_profit_usd`(2),
///          `period_start`(3), `period_end`(4), `period_days`(5)
pub const VALIDATE_SKIM_CAPTURE: &str = r#"
WITH v2_syncs AS (
  SELECT
    s.contract_address AS pool,
    s.evt_block_number AS block_number,
    s.evt_tx_hash AS tx_hash,
    s.evt_block_time AS block_time,
    s.reserve0,
    s.reserve1,
    LAG(s.reserve0) OVER (PARTITION BY s.contract_address ORDER BY s.evt_block_number, s.evt_tx_hash) AS prev_reserve0,
    LAG(s.reserve1) OVER (PARTITION BY s.contract_address ORDER BY s.evt_block_number, s.evt_tx_hash) AS prev_reserve1
  FROM uniswap_v2_{chain}.Pair_evt_Sync s
  WHERE s.evt_block_number >= {from_block}
    AND s.evt_block_number <= {to_block}
),
sync_without_swap AS (
  SELECT
    cs.pool,
    cs.block_number,
    cs.tx_hash,
    cs.block_time,
    cs.reserve0,
    cs.reserve1,
    cs.prev_reserve0,
    cs.prev_reserve1,
    ABS(cs.reserve0 - cs.prev_reserve0) AS reserve0_delta,
    ABS(cs.reserve1 - cs.prev_reserve1) AS reserve1_delta
  FROM v2_syncs cs
  LEFT JOIN uniswap_v2_{chain}.Pair_evt_Swap sw
    ON sw.contract_address = cs.pool
    AND sw.evt_tx_hash = cs.tx_hash
  WHERE sw.evt_tx_hash IS NULL
    AND cs.prev_reserve0 IS NOT NULL
    AND (ABS(cs.reserve0 - cs.prev_reserve0) > 0 OR ABS(cs.reserve1 - cs.prev_reserve1) > 0)
)
SELECT
  COUNT(*) AS opportunity_count,
  COALESCE(AVG(CASE WHEN reserve0_delta > 0 THEN reserve0_delta ELSE reserve1_delta END), 0) AS avg_profit_usd,
  COALESCE(SUM(CASE WHEN reserve0_delta > 0 THEN reserve0_delta ELSE reserve1_delta END), 0) AS total_profit_usd,
  MIN(block_time) AS period_start,
  MAX(block_time) AS period_end,
  DATE_DIFF('day', MIN(block_time), MAX(block_time)) AS period_days
FROM sync_without_swap
"#;

/// Validate sync() race opportunities: defensive sync calls (Sync without Swap).
///
/// Same signal as skim() capture but counts from the attacker's perspective:
/// how many times someone called sync() defensively to block a skim().
///
/// Columns: same as VALIDATE_SKIM_CAPTURE
pub const VALIDATE_SYNC_RACE: &str = r#"
WITH v2_syncs AS (
  SELECT
    s.contract_address AS pool,
    s.evt_block_number AS block_number,
    s.evt_tx_hash AS tx_hash,
    s.evt_block_time AS block_time
  FROM uniswap_v2_{chain}.Pair_evt_Sync s
  WHERE s.evt_block_number >= {from_block}
    AND s.evt_block_number <= {to_block}
)
SELECT
  COUNT(*) AS opportunity_count,
  0.0 AS avg_profit_usd,
  0.0 AS total_profit_usd,
  MIN(block_time) AS period_start,
  MAX(block_time) AS period_end,
  DATE_DIFF('day', MIN(block_time), MAX(block_time)) AS period_days
FROM v2_syncs cs
LEFT JOIN uniswap_v2_{chain}.Pair_evt_Swap sw
  ON sw.contract_address = cs.pool
  AND sw.evt_tx_hash = cs.tx_hash
WHERE sw.evt_tx_hash IS NULL
"#;

/// Validate init price snipe opportunities: V3 pools with mispriced initialization.
///
/// Finds V3 PoolCreated events, then checks the first swap price deviation
/// from the median price of the same token pair on other pools.
///
/// Columns: `opportunity_count`(0), `avg_profit_usd`(1), `total_profit_usd`(2),
///          `period_start`(3), `period_end`(4), `period_days`(5)
pub const VALIDATE_INIT_PRICE_SNIPE: &str = r#"
WITH new_pools AS (
  SELECT
    p.evt_block_number AS block_number,
    p.evt_tx_hash AS tx_hash,
    p.contract_address AS pool_address,
    p.token0,
    p.token1,
    p.evt_block_time AS block_time
  FROM uniswap_v3_{chain}.Factory_evt_PoolCreated p
  WHERE p.evt_block_number >= {from_block}
    AND p.evt_block_number <= {to_block}
),
first_swaps AS (
  SELECT
    t.pool_address,
    t.block_number,
    t.amount_usd,
    t.token_bought_amount,
    t.token_sold_amount,
    t.block_time,
    ROW_NUMBER() OVER (PARTITION BY t.pool_address ORDER BY t.block_number, t.tx_hash) AS rn
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_month >= DATE '{block_month_min}'
    AND t.project = 'uniswap_v3'
    AND t.pool_address IN (SELECT pool_address FROM new_pools)
)
SELECT
  COUNT(*) AS opportunity_count,
  COALESCE(AVG(fs.amount_usd), 0) AS avg_profit_usd,
  COALESCE(SUM(fs.amount_usd), 0) AS total_profit_usd,
  MIN(fs.block_time) AS period_start,
  MAX(fs.block_time) AS period_end,
  DATE_DIFF('day', MIN(fs.block_time), MAX(fs.block_time)) AS period_days
FROM first_swaps fs
JOIN new_pools np ON np.pool_address = fs.pool_address
WHERE fs.rn = 1
  AND fs.amount_usd > 0
"#;

/// Validate backrunning: multi-pool transactions following a large swap in the same block.
///
/// A backrun is proxied by: a tx that interacts with 2+ pools in a block where
/// a preceding tx moved > $10K on one of those pools.
///
/// Columns: same as VALIDATE_SKIM_CAPTURE
pub const VALIDATE_BACKRUN: &str = r#"
WITH large_swaps AS (
  SELECT
    t.block_number,
    t.pool_address,
    t.amount_usd,
    t.tx_hash
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_month >= DATE '{block_month_min}'
    AND t.block_number >= {from_block}
    AND t.block_number <= {to_block}
    AND t.amount_usd >= 10000
),
multi_pool_txs AS (
  SELECT
    t.block_number,
    t.tx_hash,
    COUNT(DISTINCT t.pool_address) AS pool_count,
    SUM(t.amount_usd) AS total_amount_usd,
    MIN(t.block_time) AS block_time
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_month >= DATE '{block_month_min}'
    AND t.block_number >= {from_block}
    AND t.block_number <= {to_block}
  GROUP BY t.block_number, t.tx_hash
  HAVING COUNT(DISTINCT t.pool_address) >= 2
)
SELECT
  COUNT(*) AS opportunity_count,
  COALESCE(AVG(mpt.total_amount_usd * 0.003), 0) AS avg_profit_usd,
  COALESCE(SUM(mpt.total_amount_usd * 0.003), 0) AS total_profit_usd,
  MIN(mpt.block_time) AS period_start,
  MAX(mpt.block_time) AS period_end,
  DATE_DIFF('day', MIN(mpt.block_time), MAX(mpt.block_time)) AS period_days
FROM multi_pool_txs mpt
WHERE EXISTS (
  SELECT 1 FROM large_swaps ls
  WHERE ls.block_number = mpt.block_number
    AND ls.pool_address IN (
      SELECT DISTINCT t2.pool_address
      FROM dex.trades t2
      WHERE t2.blockchain = '{chain}'
        AND t2.block_month >= DATE '{block_month_min}'
        AND t2.tx_hash = mpt.tx_hash
    )
)
"#;

/// Validate long-tail token arb: multi-pool arbs involving low-liquidity tokens.
///
/// Tokens with < $100K total volume in the period are considered long-tail.
///
/// Columns: same as VALIDATE_SKIM_CAPTURE
pub const VALIDATE_LONG_TAIL_ARB: &str = r#"
WITH token_volume AS (
  SELECT
    t.token_bought_address AS token,
    SUM(t.amount_usd) AS total_vol
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_month >= DATE '{block_month_min}'
    AND t.block_number >= {from_block}
    AND t.block_number <= {to_block}
  GROUP BY t.token_bought_address
),
long_tail_tokens AS (
  SELECT token FROM token_volume WHERE total_vol < 100000
),
multi_pool_txs AS (
  SELECT
    t.block_number,
    t.tx_hash,
    COUNT(DISTINCT t.pool_address) AS pool_count,
    SUM(t.amount_usd) AS total_amount_usd,
    MIN(t.block_time) AS block_time
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_month >= DATE '{block_month_min}'
    AND t.block_number >= {from_block}
    AND t.block_number <= {to_block}
  GROUP BY t.block_number, t.tx_hash
  HAVING COUNT(DISTINCT t.pool_address) >= 2
)
SELECT
  COUNT(*) AS opportunity_count,
  COALESCE(AVG(mpt.total_amount_usd * 0.005), 0) AS avg_profit_usd,
  COALESCE(SUM(mpt.total_amount_usd * 0.005), 0) AS total_profit_usd,
  MIN(mpt.block_time) AS period_start,
  MAX(mpt.block_time) AS period_end,
  DATE_DIFF('day', MIN(mpt.block_time), MAX(mpt.block_time)) AS period_days
FROM multi_pool_txs mpt
WHERE EXISTS (
  SELECT 1
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_month >= DATE '{block_month_min}'
    AND t.tx_hash = mpt.tx_hash
    AND (t.token_bought_address IN (SELECT token FROM long_tail_tokens)
         OR t.token_sold_address IN (SELECT token FROM long_tail_tokens))
)
"#;

/// Validate stablecoin depeg arbitrage: Curve pool price deviations > 1% from $1.
///
/// Monitors Curve USDC/DAI/USDT pools for price deviation events.
///
/// Columns: same as VALIDATE_SKIM_CAPTURE
pub const VALIDATE_STABLECOIN_DEPEG: &str = r#"
WITH curve_exchanges AS (
  SELECT
    e.contract_address AS pool,
    e.evt_block_number AS block_number,
    e.evt_block_time AS block_time,
    e.sold_id,
    e.bought_id,
    e.tokens_sold,
    e.tokens_bought,
    e.evt_tx_hash AS tx_hash
  FROM curve_{chain}.pool3_evt_TokenExchange e
  WHERE e.evt_block_number >= {from_block}
    AND e.evt_block_number <= {to_block}
  UNION ALL
  SELECT
    e.contract_address,
    e.evt_block_number,
    e.evt_block_time,
    e.sold_id,
    e.bought_id,
    e.tokens_sold,
    e.tokens_bought,
    e.evt_tx_hash
  FROM curve_{chain}.pool3_evt_TokenExchangeUnderlying e
  WHERE e.evt_block_number >= {from_block}
    AND e.evt_block_number <= {to_block}
),
priced AS (
  SELECT
    ce.pool,
    ce.block_number,
    ce.block_time,
    ce.tokens_sold,
    ce.tokens_bought,
    CASE
      WHEN ce.sold_id = 0 AND ce.bought_id = 1 THEN
        ABS(ce.tokens_bought / 1e6 - ce.tokens_sold / 1e18) / NULLIF(ce.tokens_sold / 1e18, 0)
      WHEN ce.sold_id = 1 AND ce.bought_id = 0 THEN
        ABS(ce.tokens_sold / 1e6 - ce.tokens_bought / 1e18) / NULLIF(ce.tokens_bought / 1e18, 0)
      ELSE NULL
    END AS price_deviation
  FROM curve_exchanges ce
)
SELECT
  COUNT(*) AS opportunity_count,
  COALESCE(AVG(price_deviation * 100), 0) AS avg_profit_usd,
  COALESCE(SUM(price_deviation * 100), 0) AS total_profit_usd,
  MIN(block_time) AS period_start,
  MAX(block_time) AS period_end,
  DATE_DIFF('day', MIN(block_time), MAX(block_time)) AS period_days
FROM priced
WHERE price_deviation > 0.01
"#;

/// Validate Curve pool imbalance: Curve pools with balances deviating from peg.
///
/// Columns: same as VALIDATE_SKIM_CAPTURE
pub const VALIDATE_CURVE_IMBALANCE: &str = r#"
WITH curve_exchanges AS (
  SELECT
    e.contract_address AS pool,
    e.evt_block_number AS block_number,
    e.evt_block_time AS block_time,
    e.sold_id,
    e.bought_id,
    e.tokens_sold,
    e.tokens_bought
  FROM curve_{chain}.pool3_evt_TokenExchange e
  WHERE e.evt_block_number >= {from_block}
    AND e.evt_block_number <= {to_block}
),
pool_imbalance AS (
  SELECT
    pool,
    block_number,
    block_time,
    CASE
      WHEN sold_id = 0 AND bought_id = 1 THEN tokens_bought / 1e6 / NULLIF(tokens_sold / 1e18, 0)
      WHEN sold_id = 1 AND bought_id = 0 THEN tokens_sold / 1e6 / NULLIF(tokens_bought / 1e18, 0)
      ELSE NULL
    END AS implied_price
  FROM curve_exchanges
)
SELECT
  COUNT(*) AS opportunity_count,
  COALESCE(AVG(ABS(implied_price - 1.0) * 100), 0) AS avg_profit_usd,
  COALESCE(SUM(ABS(implied_price - 1.0) * 100), 0) AS total_profit_usd,
  MIN(block_time) AS period_start,
  MAX(block_time) AS period_end,
  DATE_DIFF('day', MIN(block_time), MAX(block_time)) AS period_days
FROM pool_imbalance
WHERE implied_price IS NOT NULL
  AND ABS(implied_price - 1.0) > 0.005
"#;

/// Validate LST depeg collateral liquidation: AAVE liquidations where collateral is an LST.
///
/// LSTs: stETH, rETH, cbETH, frxETH, sETH2, wstETH.
///
/// Columns: same as VALIDATE_SKIM_CAPTURE
pub const VALIDATE_LST_DEPEG_LIQ: &str = r#"
WITH lst_tokens AS (
  SELECT token FROM (VALUES
    (0xae7ab96520de3a18e5e13f1536244d8eb00c4f53),  -- stETH
    (0xae78736cd615f374d3085123a210448e74fc6393),  -- rETH
    (0xbe9895146f7af43049ca1c1ae358b0541ea49704),  -- cbETH
    (0x5c84bf60de35539930200046e585fa8d3676e2e7),  -- frxETH
    (0xf34960d37339CCE23a98cDcaA5ACB0da7b9Ccb32),  -- sETH2
    (0x7f39c581f595b53c5cb19bd0b3f8da6c935e2ca0)   -- wstETH
  ) AS t(token)
)
SELECT
  COUNT(*) AS opportunity_count,
  COALESCE(AVG(l.collateral_amount / 1e18), 0) AS avg_profit_usd,
  COALESCE(SUM(l.collateral_amount / 1e18), 0) AS total_profit_usd,
  MIN(l.block_time) AS period_start,
  MAX(l.block_time) AS period_end,
  DATE_DIFF('day', MIN(l.block_time), MAX(l.block_time)) AS period_days
FROM aave_v3_{chain}.Pool_evt_LiquidationCall l
WHERE l.evt_block_number >= {from_block}
  AND l.evt_block_number <= {to_block}
  AND l.collateralAsset IN (SELECT token FROM lst_tokens)
"#;

/// Validate MakerDAO Clip Dutch auction take() events.
///
/// Columns: same as VALIDATE_SKIM_CAPTURE
pub const VALIDATE_MAKERDAO_CLIP: &str = r#"
SELECT
  COUNT(*) AS opportunity_count,
  COALESCE(AVG(t.art / 1e45), 0) AS avg_profit_usd,
  COALESCE(SUM(t.art / 1e45), 0) AS total_profit_usd,
  MIN(t.evt_block_time) AS period_start,
  MAX(t.evt_block_time) AS period_end,
  DATE_DIFF('day', MIN(t.evt_block_time), MAX(t.evt_block_time)) AS period_days
FROM clip_{chain}.clipper_evt_Take t
WHERE t.evt_block_number >= {from_block}
  AND t.evt_block_number <= {to_block}
"#;

/// Validate MakerDAO OSM kick() events (vault liquidation initiation).
///
/// Columns: same as VALIDATE_SKIM_CAPTURE
pub const VALIDATE_MAKERDAO_KICK: &str = r#"
SELECT
  COUNT(*) AS opportunity_count,
  COALESCE(AVG(k.art / 1e45), 0) AS avg_profit_usd,
  COALESCE(SUM(k.art / 1e45), 0) AS total_profit_usd,
  MIN(k.evt_block_time) AS period_start,
  MAX(k.evt_block_time) AS period_end,
  DATE_DIFF('day', MIN(k.evt_block_time), MAX(k.evt_block_time)) AS period_days
FROM vow_{chain}.vow_evt_Flip k
WHERE k.evt_block_number >= {from_block}
  AND k.evt_block_number <= {to_block}
"#;

/// Validate GMX v1 keeper race: liquidation events on GMX v1.
///
/// Columns: same as VALIDATE_SKIM_CAPTURE
pub const VALIDATE_GMX_V1_KEEPER: &str = r#"
SELECT
  COUNT(*) AS opportunity_count,
  COALESCE(AVG(l.feeAmount / 1e30), 0) AS avg_profit_usd,
  COALESCE(SUM(l.feeAmount / 1e30), 0) AS total_profit_usd,
  MIN(l.evt_block_time) AS period_start,
  MAX(l.evt_block_time) AS period_end,
  DATE_DIFF('day', MIN(l.evt_block_time), MAX(l.evt_block_time)) AS period_days
FROM gmx_{chain}.Vault_evt_Liquidation l
WHERE l.evt_block_number >= {from_block}
  AND l.evt_block_number <= {to_block}
"#;

/// Validate GMX V2 ADL front-run: automatic deleveraging events.
///
/// Columns: same as VALIDATE_SKIM_CAPTURE
pub const VALIDATE_GMX_V2_ADL: &str = r#"
SELECT
  COUNT(*) AS opportunity_count,
  0.0 AS avg_profit_usd,
  0.0 AS total_profit_usd,
  MIN(e.evt_block_time) AS period_start,
  MAX(e.evt_block_time) AS period_end,
  DATE_DIFF('day', MIN(e.evt_block_time), MAX(e.evt_block_time)) AS period_days
FROM gmx_router_{chain}.OrderHandler_evt_OrderExecuted e
WHERE e.evt_block_number >= {from_block}
  AND e.evt_block_number <= {to_block}
  AND e.sizeDelta > 0
  AND e.orderType = 7
"#;

/// Validate Liquity recovery mode cascade: trove liquidation events.
///
/// Columns: same as VALIDATE_SKIM_CAPTURE
pub const VALIDATE_LIQUITY_RECOVERY: &str = r#"
SELECT
  COUNT(*) AS opportunity_count,
  COALESCE(AVG(l.debtAmount / 1e18), 0) AS avg_profit_usd,
  COALESCE(SUM(l.debtAmount / 1e18), 0) AS total_profit_usd,
  MIN(l.evt_block_time) AS period_start,
  MAX(l.evt_block_time) AS period_end,
  DATE_DIFF('day', MIN(l.evt_block_time), MAX(l.evt_block_time)) AS period_days
FROM liquity_{chain}.TroveManager_evt_TroveLiquidated l
WHERE l.evt_block_number >= {from_block}
  AND l.evt_block_number <= {to_block}
"#;

/// Validate Synthetix flag + delayed liquidation events.
///
/// Columns: same as VALIDATE_SKIM_CAPTURE
pub const VALIDATE_SYNTHETIX_LIQ: &str = r#"
SELECT
  COUNT(*) AS opportunity_count,
  COALESCE(AVG(l.amount / 1e18), 0) AS avg_profit_usd,
  COALESCE(SUM(l.amount / 1e18), 0) AS total_profit_usd,
  MIN(l.evt_block_time) AS period_start,
  MAX(l.evt_block_time) AS period_end,
  DATE_DIFF('day', MIN(l.evt_block_time), MAX(l.evt_block_time)) AS period_days
FROM synthetix_{chain}.LiquidationRewards_evt_Liquidation l
WHERE l.evt_block_number >= {from_block}
  AND l.evt_block_number <= {to_block}
"#;

/// Validate perp protocol keeper: dYdX / Kwenta liquidation events.
///
/// Columns: same as VALIDATE_SKIM_CAPTURE
pub const VALIDATE_PERP_KEEPER: &str = r#"
WITH all_perp_liqs AS (
  -- Kwenta / Synthetix Perps on Optimism
  SELECT
    l.evt_block_time AS block_time,
    l.evt_block_number AS block_number,
    COALESCE(l.liquidationReward / 1e18, 0) AS reward_usd
  FROM gains_network_{chain}.GTokenLiquidation l
  WHERE l.evt_block_number >= {from_block}
    AND l.evt_block_number <= {to_block}
)
SELECT
  COUNT(*) AS opportunity_count,
  COALESCE(AVG(reward_usd), 0) AS avg_profit_usd,
  COALESCE(SUM(reward_usd), 0) AS total_profit_usd,
  MIN(block_time) AS period_start,
  MAX(block_time) AS period_end,
  DATE_DIFF('day', MIN(block_time), MAX(block_time)) AS period_days
FROM all_perp_liqs
"#;

/// Validate flash loan atomic liquidation: flash loans paired with liquidations in same tx.
///
/// Columns: same as VALIDATE_SKIM_CAPTURE
pub const VALIDATE_FLASH_LIQ_PROFIT: &str = r#"
WITH flash_liqs AS (
  SELECT
    f.block_number,
    f.tx_hash,
    f.amount_usd AS flash_amount_usd,
    f.fee AS flash_fee_usd,
    f.block_time
  FROM lending.flashloans f
  WHERE f.blockchain = '{chain}'
    AND f.block_month >= DATE '{block_month_min}'
    AND f.block_number >= {from_block}
    AND f.block_number <= {to_block}
    AND f.block_number IN (
      SELECT l.block_number
      FROM lending.borrow l
      WHERE l.blockchain = '{chain}'
        AND l.transaction_type = 'liquidation'
        AND l.block_number >= {from_block}
        AND l.block_number <= {to_block}
    )
)
SELECT
  COUNT(*) AS opportunity_count,
  COALESCE(AVG(flash_amount_usd), 0) AS avg_profit_usd,
  COALESCE(SUM(flash_amount_usd), 0) AS total_profit_usd,
  MIN(block_time) AS period_start,
  MAX(block_time) AS period_end,
  DATE_DIFF('day', MIN(block_time), MAX(block_time)) AS period_days
FROM flash_liqs
"#;

/// Validate JIT liquidity (V3): Mint→Swap→Burn patterns in same block, same pool.
///
/// Columns: same as VALIDATE_SKIM_CAPTURE
pub const VALIDATE_JIT_FEE_CAPTURE: &str = r#"
WITH v3_events AS (
  SELECT
    evt_block_number AS block_number,
    evt_tx_hash AS tx_hash,
    contract_address AS pool_address,
    'mint' AS event_type,
    evt_block_time AS block_time
  FROM uniswap_v3_{chain}.Pool_evt_Mint
  WHERE evt_block_number >= {from_block}
    AND evt_block_number <= {to_block}
  UNION ALL
  SELECT
    evt_block_number,
    evt_tx_hash,
    contract_address,
    'burn',
    evt_block_time
  FROM uniswap_v3_{chain}.Pool_evt_Burn
  WHERE evt_block_number >= {from_block}
    AND evt_block_number <= {to_block}
  UNION ALL
  SELECT
    t.block_number,
    t.tx_hash,
    t.pool_address,
    'swap',
    t.block_time
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_month >= DATE '{block_month_min}'
    AND t.block_number >= {from_block}
    AND t.block_number <= {to_block}
    AND t.project = 'uniswap_v3'
),
pool_block_events AS (
  SELECT
    pool_address,
    block_number,
    block_time,
    tx_hash,
    ARRAY_AGG(DISTINCT event_type) AS event_types,
    COUNT(DISTINCT event_type) AS event_count
  FROM v3_events
  GROUP BY pool_address, block_number, block_time, tx_hash
)
SELECT
  COUNT(*) AS opportunity_count,
  COALESCE(AVG(1000), 0) AS avg_profit_usd,
  COALESCE(SUM(1000), 0) AS total_profit_usd,
  MIN(block_time) AS period_start,
  MAX(block_time) AS period_end,
  DATE_DIFF('day', MIN(block_time), MAX(block_time)) AS period_days
FROM pool_block_events
WHERE event_count >= 2
  AND ARRAY_CONTAINS(event_types, 'mint')
  AND ARRAY_CONTAINS(event_types, 'burn')
  AND ARRAY_CONTAINS(event_types, 'swap')
  AND (
    -- Same tx contains mint+swap+burn (classic JIT)
    (event_count = 3)
    -- Or block has mint tx, swap tx, burn tx from same address
    OR EXISTS (
      SELECT 1 FROM v3_events m
      WHERE m.pool_address = pool_block_events.pool_address
        AND m.block_number = pool_block_events.block_number
        AND m.event_type = 'mint'
        AND EXISTS (
          SELECT 1 FROM v3_events b
          WHERE b.pool_address = m.pool_address
            AND b.block_number = m.block_number
            AND b.event_type = 'burn'
            AND b.tx_hash != m.tx_hash
        )
    )
  )
"#;
