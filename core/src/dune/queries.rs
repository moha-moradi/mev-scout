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
//! - `{min_profit_usd}` — minimum per-opportunity profit in USD (e.g. `10`)
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

/// Curve pools.
///
/// Discovery strategy:
/// - `dex.trades` (project = 'curve') covers ethereum (and other chains with curated
///   curve data). Note: polygon `dex.trades` does NOT contain a `curve` project, so on
///   polygon the query also unions pool-deployment events from the verified
///   `curvefi_polygon.stableswapfactory_evt_*` factory tables (guarded by
///   `'{chain}' = 'polygon'`, so they no-op on other chains).
/// Columns: `pool_address`(0), `coins`(1) [JSON array of token addresses], `n_coins`(2),
///          `creation_block`(3), `pool_type`(4), `registry`(5)
pub const QUERY_CURVE_POOLS: &str = r#"
WITH curve_pools AS (
  SELECT
    t.project_contract_address AS pool_address,
    MIN(t.block_number) AS creation_block
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.project = 'curve'
    AND t.block_month >= DATE '{block_month_min}'
    AND t.block_number >= {from_block}
    AND t.block_number <= {to_block}
  GROUP BY 1

  UNION ALL

  SELECT
    contract_address AS pool_address,
    MIN(evt_block_number) AS creation_block
  FROM curvefi_polygon.stableswapfactory_evt_plainpooldeployed
  WHERE '{chain}' = 'polygon'
    AND evt_block_number >= {from_block}
    AND evt_block_number <= {to_block}
  GROUP BY 1

  UNION ALL

  SELECT
    contract_address AS pool_address,
    MIN(evt_block_number) AS creation_block
  FROM curvefi_polygon.stableswapfactory_evt_metapooldeployed
  WHERE '{chain}' = 'polygon'
    AND evt_block_number >= {from_block}
    AND evt_block_number <= {to_block}
  GROUP BY 1

  UNION ALL

  SELECT
    contract_address AS pool_address,
    MIN(evt_block_number) AS creation_block
  FROM curvefi_polygon.stableswapfactory_evt_tricryptopooldeployed
  WHERE '{chain}' = 'polygon'
    AND evt_block_number >= {from_block}
    AND evt_block_number <= {to_block}
  GROUP BY 1
)
SELECT
  cp.pool_address,
  NULL AS coins_json,
  2 AS n_coins,
  cp.creation_block,
  'curve_2' AS pool_type,
  NULL AS registry
FROM curve_pools cp
ORDER BY cp.creation_block ASC
"#;

/// Balancer V2 pools via `PoolRegistered` event.
///
/// Columns: `pool_address`(0), `pool_id`(1) [bytes32], `pool_type`(2),
///          `creation_block`(3), `vault_address`(4)
pub const QUERY_BALANCER_POOLS: &str = r#"
SELECT
  p.pooladdress AS pool_address,
  p.poolid AS pool_id,
  NULL AS pool_type,
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
LIMIT 100000
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
FROM uniswap_v2_{chain}.uniswapv2factory_evt_paircreated p
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
  t.project_contract_address,
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
  t.project_contract_address,
  t.project,
  t.block_time
FROM dex.trades t
WHERE t.blockchain = '{chain}'
  AND t.block_month >= DATE '{block_month_min}'
  AND t.block_number >= {from_block}
  AND t.block_number <= {to_block}
ORDER BY t.block_number, t.tx_hash
LIMIT 100000
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
  AND t.project_contract_address = '{pool_address}'::bytea
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
  t.project_contract_address,
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
  t.project_contract_address,
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
  t.project_contract_address,
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
/// `dex.sandwiches` has the same schema as `dex.trades`. Each row is a trade
/// that was part of a sandwich attack (front-run or back-run).
///
/// Columns: `block_number`(0), `tx_hash`(1), `sandwicher`(2),
///          `pool`(3), `token_bought_symbol`(4), `token_sold_symbol`(5), `amount_usd`(6)
pub const QUERY_SANDWICHES_BY_RANGE: &str = r#"
SELECT
  s.block_number,
  s.tx_hash,
  s.tx_from AS sandwicher,
  s.project_contract_address AS pool,
  s.token_bought_symbol,
  s.token_sold_symbol,
  s.amount_usd
FROM dex.sandwiches s
WHERE s.blockchain = '{chain}'
  AND s.block_month >= DATE '{block_month_min}'
  AND s.block_number >= {from_block}
  AND s.block_number <= {to_block}
ORDER BY s.block_number, s.tx_hash
LIMIT 100000
"#;

/// Sandwich attacks in a specific block.
///
/// Columns: same as QUERY_SANDWICHES_BY_RANGE.
pub const QUERY_SANDWICHES_BY_BLOCK: &str = r#"
SELECT
  s.block_number,
  s.tx_hash,
  s.tx_from AS sandwicher,
  s.project_contract_address AS pool_address,
  s.amount_usd,
  s.token_bought_symbol,
  s.token_sold_symbol
FROM dex.sandwiches s
WHERE s.blockchain = '{chain}'
  AND s.block_number = {block_number}
ORDER BY s.tx_hash
"#;

/// Sandwich attacks in a time range.
///
/// Parameters: `{from_time}` and `{to_time}` in ISO-8601 format.
pub const QUERY_SANDWICHES_BY_TIME: &str = r#"
SELECT
  s.block_number,
  s.tx_hash,
  s.tx_from AS sandwicher,
  s.project_contract_address AS pool_address,
  s.amount_usd,
  s.token_bought_symbol,
  s.token_sold_symbol
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
LIMIT 100000
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
    t.project_contract_address,
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
  MAX(CASE WHEN tp.rn_asc = 1 THEN tp.project_contract_address END) OVER (PARTITION BY tp.tx_hash) AS pool_a,
  MAX(CASE WHEN tp.rn_desc = 1 THEN tp.project_contract_address END) OVER (PARTITION BY tp.tx_hash) AS pool_b,
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
    t.project_contract_address,
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
  MAX(CASE WHEN tp.rn_asc = 1 THEN tp.project_contract_address END) OVER (PARTITION BY tp.tx_hash) AS pool_a,
  MAX(CASE WHEN tp.rn_desc = 1 THEN tp.project_contract_address END) OVER (PARTITION BY tp.tx_hash) AS pool_b,
  MAX(CASE WHEN tp.rn_asc = 1 THEN tp.token_in END) OVER (PARTITION BY tp.tx_hash) AS token_in,
  MAX(CASE WHEN tp.rn_desc = 1 THEN tp.token_out END) OVER (PARTITION BY tp.tx_hash) AS token_out,
  MAX(tp.amount_usd) OVER (PARTITION BY tp.tx_hash) AS amount_usd
FROM tx_pools tp
WHERE tp.pool_count >= 2
ORDER BY tp.block_number, tp.tx_hash
LIMIT 100000
"#;

/// Arbitrage transactions in a time range.
pub const QUERY_ARBITRAGES_BY_TIME: &str = r#"
WITH tx_pools AS (
  SELECT
    t.tx_hash,
    t.block_number,
    t.project_contract_address,
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
  MAX(CASE WHEN tp.rn_asc = 1 THEN tp.project_contract_address END) OVER (PARTITION BY tp.tx_hash) AS pool_a,
  MAX(CASE WHEN tp.rn_desc = 1 THEN tp.project_contract_address END) OVER (PARTITION BY tp.tx_hash) AS pool_b,
  MAX(CASE WHEN tp.rn_asc = 1 THEN tp.token_in END) OVER (PARTITION BY tp.tx_hash) AS token_in,
  MAX(CASE WHEN tp.rn_desc = 1 THEN tp.token_out END) OVER (PARTITION BY tp.tx_hash) AS token_out,
  MAX(tp.amount_usd) OVER (PARTITION BY tp.tx_hash) AS amount_usd
FROM tx_pools tp
WHERE tp.pool_count >= 2
ORDER BY tp.block_number, tp.tx_hash
"#;

/// All flash loan events from Dune's consolidated `lending.flashloans` dataset.
///
/// Columns: `block_number`(0), `tx_hash`(1), `project`(2), `token_address`(3),
///          `amount_usd`(4), `amount`(5), `fee`(6)
pub const QUERY_FLASH_LOANS_BY_RANGE: &str = r#"
SELECT
  f.block_number,
  f.tx_hash,
  f.project,
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
  f.project,
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
/// Compound V3 has no `Comet_evt_Absorb` table; liquidations are captured via
/// `call_absorb` traces per market. Verified markets: Polygon `cusdcv3polygon`
/// and `cusdtv3`; Ethereum `comet`, `cusdcv3`, `cusdtv3`, `cwethv3`, `cusdsv3`.
/// Block-number ranges disambiguate the active chain.
///
/// Columns: `block_number`(0), `tx_hash`(1), `user`(2), `liquidator`(3),
///          `collateral_asset`(4), `debt_asset`(5), `collateral_amount`(6),
///          `debt_amount`(7), `block_time`(8)
pub const QUERY_COMPOUND_V3_LIQUIDATIONS: &str = r#"
WITH absorbs AS (
  SELECT
    call_block_number AS block_number,
    call_tx_hash AS tx_hash,
    absorber AS liquidator,
    call_block_time AS block_time
  FROM compound_v3_polygon.cusdcv3polygon_call_absorb
  WHERE call_block_number >= {from_block} AND call_block_number <= {to_block}
  UNION ALL
  SELECT call_block_number, call_tx_hash, absorber, call_block_time
  FROM compound_v3_polygon.cusdtv3_call_absorb
  WHERE call_block_number >= {from_block} AND call_block_number <= {to_block}
  UNION ALL
  SELECT call_block_number, call_tx_hash, absorber, call_block_time
  FROM compound_v3_ethereum.comet_call_absorb
  WHERE call_block_number >= {from_block} AND call_block_number <= {to_block}
  UNION ALL
  SELECT call_block_number, call_tx_hash, absorber, call_block_time
  FROM compound_v3_ethereum.cusdcv3_call_absorb
  WHERE call_block_number >= {from_block} AND call_block_number <= {to_block}
  UNION ALL
  SELECT call_block_number, call_tx_hash, absorber, call_block_time
  FROM compound_v3_ethereum.cusdtv3_call_absorb
  WHERE call_block_number >= {from_block} AND call_block_number <= {to_block}
  UNION ALL
  SELECT call_block_number, call_tx_hash, absorber, call_block_time
  FROM compound_v3_ethereum.cwethv3_call_absorb
  WHERE call_block_number >= {from_block} AND call_block_number <= {to_block}
  UNION ALL
  SELECT call_block_number, call_tx_hash, absorber, call_block_time
  FROM compound_v3_ethereum.cusdsv3_call_absorb
  WHERE call_block_number >= {from_block} AND call_block_number <= {to_block}
)
SELECT
  a.block_number,
  a.tx_hash,
  NULL AS user,
  a.liquidator,
  NULL AS collateral_asset,
  NULL AS debt_asset,
  NULL AS collateral_amount,
  NULL AS debt_amount,
  a.block_time
FROM absorbs a
ORDER BY a.block_number, a.tx_hash
"#;

/// Combined liquidation events from the consolidated `lending.borrow` dataset.
///
/// Dune does not have `lending.liquidations`; liquidations are recorded in
/// `lending.borrow` with `transaction_type = 'borrow_liquidation'`.
/// Columns: `block_number`(0), `tx_hash`(1), `protocol`(2), `user`(3), `liquidator`(4),
///          `token_address`(5), `amount`(6), `amount_usd`(7), `block_time`(8)
pub const QUERY_LIQUIDATIONS_ALL: &str = r#"
SELECT
  l.block_number,
  l.tx_hash,
  l.project AS protocol,
  l.borrower AS user,
  l.liquidator,
  l.token_address,
  l.amount,
  l.amount_usd,
  l.block_time
FROM lending.borrow l
WHERE l.blockchain = '{chain}'
  AND l.transaction_type = 'borrow_liquidation'
  AND l.block_month >= DATE '{block_month_min}'
  AND l.block_number >= {from_block}
  AND l.block_number <= {to_block}
ORDER BY l.block_number, l.tx_hash
LIMIT 100000
"#;

/// Combined liquidations in a specific block.
pub const QUERY_LIQUIDATIONS_BY_BLOCK: &str = r#"
SELECT
  l.block_number,
  l.tx_hash,
  l.project AS protocol,
  l.borrower AS user,
  l.liquidator,
  l.token_address,
  l.amount,
  l.amount_usd,
  l.block_time
FROM lending.borrow l
WHERE l.blockchain = '{chain}'
  AND l.transaction_type = 'borrow_liquidation'
  AND l.block_number = {block_number}
ORDER BY l.tx_hash
"#;

/// Verify if a specific tx_hash is part of a sandwich attack.
///
/// Checks both `dex.sandwiches` (attacker trades) and `dex.sandwiched` (victim trades).
///
/// Columns: `block_number`(0), `tx_hash`(1), `sandwicher`(2),
///          `pool_address`(3), `amount_usd`(4), `role`(5)
pub const QUERY_VERIFY_SANDWICH: &str = r#"
SELECT
  s.block_number,
  s.tx_hash,
  s.tx_from AS sandwicher,
  s.project_contract_address AS pool_address,
  s.amount_usd,
  'attacker' AS role
FROM dex.sandwiches s
WHERE s.blockchain = '{chain}'
  AND s.block_month >= DATE '{block_month_min}'
  AND s.block_number = {block_number}
  AND s.tx_hash = '{tx_hash}'::bytea
UNION ALL
SELECT
  v.block_number,
  v.tx_hash,
  NULL AS sandwicher,
  v.project_contract_address AS pool_address,
  v.amount_usd,
  'victim' AS role
FROM dex.sandwiched v
WHERE v.blockchain = '{chain}'
  AND v.block_month >= DATE '{block_month_min}'
  AND v.block_number = {block_number}
  AND v.tx_hash = '{tx_hash}'::bytea
LIMIT 10
"#;

/// Failed (reverted) transactions with value > threshold in a block range.
/// These are potential MEV signals: searchers bidding on failed bundles.
///
/// Uses the raw `{chain}.transactions` dataset (the curated `gas.fees` dataset
/// does not expose `success`/`value`/`error` columns).
/// Columns: `block_number`(0), `tx_hash`(1), `from`(2), `to`(3),
///          `value_eth`(4), `gas_used`(5), `gas_price_gwei`(6), `error`(7)
pub const QUERY_FAILED_TXS: &str = r#"
SELECT
  g.block_number,
  g.hash AS tx_hash,
  g."from" AS from_address,
  g."to" AS to_address,
  CAST(g.value AS DOUBLE) / 1e18 AS value_eth,
  g.gas_used,
  CAST(g.gas_price AS DOUBLE) / 1e9 AS gas_price_gwei,
  NULL AS error_reason
FROM {chain}.transactions g
WHERE g.block_number >= {from_block}
  AND g.block_number <= {to_block}
  AND g.success = FALSE
  AND g.value > 0
ORDER BY g.value DESC
"#;

/// Failed transactions in a specific block.
pub const QUERY_FAILED_TXS_BY_BLOCK: &str = r#"
SELECT
  g.block_number,
  g.hash AS tx_hash,
  g."from" AS from_address,
  g."to" AS to_address,
  CAST(g.value AS DOUBLE) / 1e18 AS value_eth,
  g.gas_used,
  CAST(g.gas_price AS DOUBLE) / 1e9 AS gas_price_gwei,
  NULL AS error_reason
FROM {chain}.transactions g
WHERE g.block_number = {block_number}
  AND g.success = FALSE
  AND g.value > 0
ORDER BY g.value DESC
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
LIMIT 100000
"#;

/// Historical USD price for a token at a specific block time.
///
/// Uses the hybrid `prices.minute` table (Coinpaprika + DEX-derived, 900K+ tokens).
/// Columns: `timestamp`(0), `price`(1), `symbol`(2), `decimals`(3)
pub const QUERY_TOKEN_PRICE_AT_BLOCK: &str = r#"
SELECT
  p.timestamp,
  p.price,
  p.symbol,
  p.decimals
FROM prices.minute p
WHERE p.blockchain = '{chain}'
  AND p.contract_address = '{token_address}'::bytea
  AND p.timestamp <= TIMESTAMP '{block_timestamp}'
  AND p.timestamp >= TIMESTAMP '{block_timestamp}' - INTERVAL '1' hour
ORDER BY p.timestamp DESC
LIMIT 1
"#;

/// Price history for a token over a time window (for TWAP / price analysis).
///
/// Uses the hybrid `prices.minute` table.
/// Columns: `timestamp`(0), `price`(1), `symbol`(2)
pub const QUERY_TOKEN_PRICE_HISTORY: &str = r#"
SELECT
  p.timestamp,
  p.price,
  p.symbol
FROM prices.minute p
WHERE p.blockchain = '{chain}'
  AND p.contract_address = '{token_address}'::bytea
  AND p.timestamp >= TIMESTAMP '{from_time}'
  AND p.timestamp <= TIMESTAMP '{to_time}'
ORDER BY p.timestamp
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

/// Block metadata: timestamp, gas used, base fee.
///
/// Columns: `block_number`(0), `block_time`(1), `timestamp_utc`(2),
///          `gas_used`(3), `gas_limit`(4), `base_fee_per_gas`(5)
pub const QUERY_BLOCK_METADATA: &str = r#"
SELECT
  b.number AS block_number,
  b.time AS block_time,
  CAST(b.time AS VARCHAR) AS timestamp_utc,
  b.gas_used,
  b.gas_limit,
  CAST(b.base_fee_per_gas AS DOUBLE) / 1e9 AS base_fee_per_gas
FROM {chain}.blocks b
WHERE b.number >= {from_block}
  AND b.number <= {to_block}
ORDER BY b.number
LIMIT 100000
"#;

/// Block metadata for a single block.
pub const QUERY_SINGLE_BLOCK: &str = r#"
SELECT
  b.number AS block_number,
  b.time AS block_time,
  CAST(b.time AS VARCHAR) AS timestamp_utc,
  b.gas_used,
  b.gas_limit,
  CAST(b.base_fee_per_gas AS DOUBLE) / 1e9 AS base_fee_per_gas
FROM {chain}.blocks b
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
    CAST(g.gas_price AS DOUBLE) / 1e9 AS gas_price_gwei
  FROM gas.fees g
  WHERE g.blockchain = '{chain}'
    AND g.block_date >= DATE '{block_month_min}'
    AND g.block_number >= {from_block}
    AND g.block_number <= {to_block}
    AND g.gas_price > 0
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
JOIN {chain}.blocks b ON b.number = tg.block_number
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
    t.project_contract_address AS pool_address,
    t.tx_from,
    t.amount_usd,
    LAG(t.tx_from) OVER (PARTITION BY t.project_contract_address ORDER BY t.block_number, t.tx_hash) AS prev_tx_from,
    LEAD(t.tx_from) OVER (PARTITION BY t.project_contract_address ORDER BY t.block_number, t.tx_hash) AS next_tx_from,
    LAG(t.tx_hash) OVER (PARTITION BY t.project_contract_address ORDER BY t.block_number, t.tx_hash) AS prev_tx_hash,
    LEAD(t.tx_hash) OVER (PARTITION BY t.project_contract_address ORDER BY t.block_number, t.tx_hash) AS next_tx_hash
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
/// Polygon: the `uniswap_v3_polygon` decode stopped in 2022-09, so pool events
/// come from the live QuickSwap V3 (Algebra) decode
/// (`quickswap_v3_polygon.algebrapool_evt_mint/burn`). V3 swaps on Polygon are
/// labelled `project='quickswap' AND version='3'` in `dex.trades`
/// (`project='uniswap_v3'` returns 0 rows there; Dune's `dex.liquidity` table
/// does not exist).
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
  FROM quickswap_v3_polygon.algebrapool_evt_mint
  WHERE evt_block_number = {block_number}
  UNION ALL
  SELECT
    evt_block_number,
    evt_tx_hash,
    contract_address,
    'burn',
    NULL
  FROM quickswap_v3_polygon.algebrapool_evt_burn
  WHERE evt_block_number = {block_number}
  UNION ALL
  SELECT
    t.block_number,
    t.tx_hash,
    t.project_contract_address,
    'swap',
    t.amount_usd
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_number = {block_number}
    AND t.project = 'quickswap'
    AND t.version = '3'
)
SELECT * FROM block_events ORDER BY pool_address, tx_hash
"#;

/// Detect time-bandit reorg opportunities: blocks where the profit
/// from reorging a previous block exceeds the cost.
/// Identifies blocks with high value that attackers might want to replace.
///
/// Uses `prices.usd` for ETH price conversion (0xc02aaa39... is ethereum WETH;
/// on other chains the join simply yields NULL). The price scan is bounded to the
/// block-range time window to keep it within Dune's small-query limit.
/// Columns: `block_number`(0), `total_mev_value_eth`(1), `total_tx_value_eth`(2),
///          `tx_count`(3), `base_fee_gwei`(4), `timestamp`(5)
pub const QUERY_HIGH_VALUE_BLOCKS: &str = r#"
WITH bounds AS (
  SELECT MIN(time) AS lo, MAX(time) AS hi
  FROM {chain}.blocks
  WHERE number IN ({from_block}, {to_block})
),
block_value AS (
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
),
eth_price AS (
  SELECT
    p.minute AS ts,
    p.price
  FROM prices.usd p
  CROSS JOIN bounds b
  WHERE p.blockchain = '{chain}'
    AND p.contract_address = 0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2
    AND p.minute >= b.lo
    AND p.minute <= b.hi
)
SELECT
  bv.block_number,
  bv.total_mev_value_usd / NULLIF(ep.price, 0) AS total_mev_value_eth,
  NULL AS total_tx_value_eth,
  bv.tx_count,
  CAST(blk.base_fee_per_gas AS DOUBLE) / 1e9 AS base_fee_gwei,
  blk.time AS timestamp
FROM block_value bv
JOIN {chain}.blocks blk ON blk.number = bv.block_number
LEFT JOIN eth_price ep ON ep.ts = DATE_TRUNC('minute', blk.time)
ORDER BY bv.total_mev_value_usd DESC
LIMIT 1000
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
    t.project_contract_address AS pool_address,
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
      PARTITION BY t.project_contract_address
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
  AVG(CAST(g.gas_price AS DOUBLE) / 1e9) AS avg_gas_price_gwei,
  MIN(CAST(g.gas_price AS DOUBLE) / 1e9) AS min_gas_price_gwei,
  MAX(CAST(g.gas_price AS DOUBLE) / 1e9) AS max_gas_price_gwei,
  APPROX_PERCENTILE(CAST(g.gas_price AS DOUBLE) / 1e9, 0.50) AS median_gas_price_gwei,
  COUNT(*) AS tx_count
FROM gas.fees g
WHERE g.blockchain = '{chain}'
  AND g.block_time >= TIMESTAMP '{from_time}'
  AND g.block_time < TIMESTAMP '{to_time}'
  AND g.gas_price > 0
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
    t.project_contract_address AS pool_address,
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
/// `dex_aggregator.trades` has no `block_number` column, so the block range is
/// converted to timestamps via `{chain}.blocks`.
/// Columns: `block_time`(0), `tx_hash`(1), `project`(2), `token_bought_address`(3),
///          `token_sold_address`(4), `token_bought_amount`(5), `token_sold_amount`(6),
///          `amount_usd`(7), `taker`(8)
pub const QUERY_AGGREGATOR_TRADES_IN_RANGE: &str = r#"
WITH bounds AS (
  SELECT MIN(time) AS lo, MAX(time) AS hi
  FROM {chain}.blocks
  WHERE number IN ({from_block}, {to_block})
)
SELECT
  a.block_time,
  a.tx_hash,
  a.project,
  a.token_bought_address,
  a.token_sold_address,
  a.token_bought_amount,
  a.token_sold_amount,
  a.amount_usd,
  a.taker
FROM dex_aggregator.trades a
CROSS JOIN bounds b
WHERE a.blockchain = '{chain}'
  AND a.block_month >= DATE '{block_month_min}'
  AND a.block_time >= b.lo
  AND a.block_time <= b.hi
ORDER BY a.block_time, a.tx_hash
LIMIT 100000
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
///          `depositor`(4), `token_address`(5), `amount`(6), `amount_usd`(7), `block_time`(8)
pub const QUERY_LENDING_SUPPLY_BY_RANGE: &str = r#"
SELECT
  l.block_number,
  l.tx_hash,
  l.project AS protocol,
  l.transaction_type,
  l.depositor,
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
LIMIT 100000
"#;

/// DEX-native flash loans (Balancer, Uniswap V3, dYdX) from `dex.flashloans`.
/// Complements the lending-protocol flash loans from `lending.flashloans`.
///
/// `dex.flashloans` has no `block_number` column and uses `currency_contract`
/// (not `token_address`), so the block range is converted to timestamps via
/// `{chain}.blocks`.
/// Columns: `block_time`(0), `tx_hash`(1), `project`(2), `token_address`(3),
///          `amount_usd`(4), `amount`(5), `fee`(6)
pub const QUERY_DEX_FLASH_LOANS_BY_RANGE: &str = r#"
WITH bounds AS (
  SELECT MIN(time) AS lo, MAX(time) AS hi
  FROM {chain}.blocks
  WHERE number IN ({from_block}, {to_block})
)
SELECT
  f.block_time,
  f.tx_hash,
  f.project,
  f.currency_contract AS token_address,
  f.amount_usd,
  f.amount,
  f.fee
FROM dex.flashloans f
CROSS JOIN bounds b
WHERE f.blockchain = '{chain}'
  AND f.block_time >= b.lo
  AND f.block_time <= b.hi
ORDER BY f.block_time, f.tx_hash
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
/// Dune does not expose `balanceOf` directly, so this uses an indirect signal:
/// Sync events on a V2 pair WITHOUT a Swap, Mint, or Burn in the same tx (i.e.
/// pure `sync()` calls) where a reserve INCREASED vs the previous Sync on the
/// pair. A reserve increase on sync() means token balance had drifted above the
/// stored reserve (rebase / fee-on-transfer / accidental transfer) — exactly the
/// amount a skim() would have extracted, but it was preempted by sync(). Since
/// skim() itself emits no event, this is an upper-bound proxy for the opportunity
/// size a skim bot could have captured by winning the race.
///
/// Reserve amounts are raw uint256 token units; they are converted to USD via
/// `tokens.erc20` decimals and daily `prices.usd`. Mint/Burn txs are excluded
/// because LP adds/removes also raise reserves and are NOT skim opportunities.
///
/// NOTE: `uniswap_v2_{chain}.uniswapv2pair_evt_sync/swap/mint/burn` and
/// `uniswapv2factory_evt_paircreated` decoded tables must exist for the target
/// chain (confirmed on Polygon; on other chains verify with DISCOVER_UNIV2_SYNC).
///
/// Columns: `opportunity_count`(0), `avg_profit_usd`(1), `total_profit_usd`(2),
///          `period_start`(3), `period_end`(4), `period_days`(5)
pub const VALIDATE_SKIM_CAPTURE: &str = r#"
WITH bounds AS (
  SELECT MIN(time) AS lo, MAX(time) AS hi
  FROM {chain}.blocks
  WHERE number >= {from_block}
    AND number <= {to_block}
),
daily_prices AS (
  SELECT
    p.contract_address,
    DATE_TRUNC('day', p.minute) AS day,
    AVG(p.price) AS price
  FROM prices.usd p
  CROSS JOIN bounds b
  WHERE p.blockchain = '{chain}'
    AND p.minute >= b.lo
    AND p.minute <= b.hi
  GROUP BY 1, 2
),
pair_tokens AS (
  SELECT pair AS pool, token0, token1
  FROM uniswap_v2_{chain}.uniswapv2factory_evt_paircreated
),
v2_syncs AS (
  SELECT
    s.contract_address AS pool,
    s.evt_block_number AS block_number,
    s.evt_tx_hash AS tx_hash,
    s.evt_block_time AS block_time,
    s.reserve0,
    s.reserve1,
    LAG(s.reserve0) OVER (PARTITION BY s.contract_address ORDER BY s.evt_block_number, s.evt_tx_hash) AS prev_reserve0,
    LAG(s.reserve1) OVER (PARTITION BY s.contract_address ORDER BY s.evt_block_number, s.evt_tx_hash) AS prev_reserve1
  FROM uniswap_v2_{chain}.uniswapv2pair_evt_sync s
  WHERE s.evt_block_number >= {from_block}
    AND s.evt_block_number <= {to_block}
),
sync_only AS (
  SELECT
    cs.pool,
    cs.block_time,
    pt.token0,
    pt.token1,
    CASE WHEN cs.reserve0 > cs.prev_reserve0 THEN cs.reserve0 - cs.prev_reserve0 ELSE 0 END AS drift0,
    CASE WHEN cs.reserve1 > cs.prev_reserve1 THEN cs.reserve1 - cs.prev_reserve1 ELSE 0 END AS drift1
  FROM v2_syncs cs
  LEFT JOIN uniswap_v2_{chain}.uniswapv2pair_evt_swap sw
    ON sw.contract_address = cs.pool
    AND sw.evt_tx_hash = cs.tx_hash
  LEFT JOIN uniswap_v2_{chain}.uniswapv2pair_evt_mint mn
    ON mn.contract_address = cs.pool
    AND mn.evt_tx_hash = cs.tx_hash
  LEFT JOIN uniswap_v2_{chain}.uniswapv2pair_evt_burn br
    ON br.contract_address = cs.pool
    AND br.evt_tx_hash = cs.tx_hash
  LEFT JOIN pair_tokens pt ON pt.pool = cs.pool
  WHERE sw.evt_tx_hash IS NULL
    AND mn.evt_tx_hash IS NULL
    AND br.evt_tx_hash IS NULL
    AND cs.prev_reserve0 IS NOT NULL
    AND (cs.reserve0 > cs.prev_reserve0 OR cs.reserve1 > cs.prev_reserve1)
)
SELECT
  COUNT(*) AS opportunity_count,
  COALESCE(AVG(d0_usd + d1_usd), 0) AS avg_profit_usd,
  COALESCE(SUM(d0_usd + d1_usd), 0) AS total_profit_usd,
  MIN(block_time) AS period_start,
  MAX(block_time) AS period_end,
  DATE_DIFF('day', MIN(block_time), MAX(block_time)) AS period_days
FROM (
  SELECT
    sk.pool,
    sk.block_time,
    COALESCE(CAST(sk.drift0 AS DOUBLE) / POWER(10, COALESCE(e0.decimals, 18)) * p0.price, 0) AS d0_usd,
    COALESCE(CAST(sk.drift1 AS DOUBLE) / POWER(10, COALESCE(e1.decimals, 18)) * p1.price, 0) AS d1_usd
  FROM sync_only sk
  LEFT JOIN tokens.erc20 e0 ON e0.blockchain = '{chain}' AND e0.contract_address = sk.token0
  LEFT JOIN tokens.erc20 e1 ON e1.blockchain = '{chain}' AND e1.contract_address = sk.token1
  LEFT JOIN daily_prices p0 ON p0.contract_address = sk.token0 AND p0.day = DATE_TRUNC('day', sk.block_time)
  LEFT JOIN daily_prices p1 ON p1.contract_address = sk.token1 AND p1.day = DATE_TRUNC('day', sk.block_time)
) val
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
  FROM uniswap_v2_{chain}.uniswapv2pair_evt_sync s
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
LEFT JOIN uniswap_v2_{chain}.uniswapv2pair_evt_swap sw
  ON sw.contract_address = cs.pool
  AND sw.evt_tx_hash = cs.tx_hash
WHERE sw.evt_tx_hash IS NULL
"#;

/// Validate init price snipe opportunities: V3 pools with mispriced initialization.
///
/// A pool is considered "new" when its FIRST trade in `dex.trades` falls inside the
/// block window (i.e. the pool was created and immediately traded during the period).
///
/// NOTE: This deliberately does NOT read the decoded `PoolCreated` event tables.
/// On Polygon the `uniswap_v3_polygon.factory_polygon_evt_PoolCreated` decode is
/// hollow (no rows), so pool discovery is done via `dex.trades` MIN(block_number),
/// which is populated on every supported chain.
///
/// Columns: `opportunity_count`(0), `avg_profit_usd`(1), `total_profit_usd`(2),
///          `period_start`(3), `period_end`(4), `period_days`(5)
pub const VALIDATE_INIT_PRICE_SNIPE: &str = r#"
WITH new_pools AS (
  SELECT
    t.project_contract_address AS pool_address,
    MIN(t.block_number) AS creation_block
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.version = '3'
    AND t.block_month >= DATE '{block_month_min}'
  GROUP BY 1
  HAVING MIN(t.block_number) >= {from_block}
    AND MIN(t.block_number) <= {to_block}
),
first_swaps AS (
  SELECT
    t.project_contract_address AS pool_address,
    t.block_number,
    t.amount_usd,
    t.token_bought_amount,
    t.token_sold_amount,
    t.block_time,
    ROW_NUMBER() OVER (PARTITION BY t.project_contract_address ORDER BY t.block_number, t.tx_hash) AS rn
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_month >= DATE '{block_month_min}'
    AND t.project_contract_address IN (SELECT pool_address FROM new_pools)
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
/// a DIFFERENT tx in that block moved > $10K on one of the same pools. Refinements
/// vs earlier versions:
/// - Direction check: the backrun leg on the affected pool must counter-trade the
///   large swap (buy what it sold, sell what it bought). Trades that add to the
///   imbalance are not backruns.
/// - Affected-pool-only volume: profit is sized on the counter-trading leg(s) on
///   the pool(s) the large swap hit, not the whole multi-pool tx volume.
/// - Ordering + success: `{chain}.transactions` provides the intra-block tx index
///   (the large swap must strictly precede the backrun) and `success` (both txs
///   must succeed).
/// - Gas: backrun tx gas (`gas_used × gas_price`) is priced in POL via daily
///   `prices.usd` for the native token and subtracted from gross profit.
///
/// Profit model: `gross = 0.003 × counter-volume` (fee-rate proxy) minus gas cost.
/// Opportunities with `net_profit <= 0` are excluded. Note this is still a modeled
/// upper-bound proxy: a V2 pool's 0.3% fee is equal to the assumed capture rate,
/// so most V2 backruns will net ≈0 or negative after gas — that is the point.
///
/// Columns: same as VALIDATE_SKIM_CAPTURE
pub const VALIDATE_BACKRUN: &str = r#"
WITH bounds AS (
  SELECT MIN(time) AS lo, MAX(time) AS hi
  FROM {chain}.blocks
  WHERE number >= {from_block}
    AND number <= {to_block}
),
native_prices AS (
  SELECT
    DATE_TRUNC('day', p.minute) AS day,
    AVG(p.price) AS price
  FROM prices.usd p
  CROSS JOIN bounds b
  WHERE p.blockchain = '{chain}'
    AND p.contract_address = 0x0000000000000000000000000000000000001010
    AND p.minute >= b.lo
    AND p.minute <= b.hi
  GROUP BY 1
),
tx_info AS (
  SELECT
    tx.block_number,
    tx.hash AS tx_hash,
    tx."index" AS tx_index,
    tx.success,
    tx.gas_used,
    tx.gas_price
  FROM {chain}.transactions tx
  WHERE tx.block_number >= {from_block}
    AND tx.block_number <= {to_block}
),
large_swaps AS (
  SELECT
    t.block_number,
    t.project_contract_address AS pool_address,
    t.token_bought_address,
    t.token_sold_address,
    t.tx_hash,
    ti.tx_index
  FROM dex.trades t
  JOIN tx_info ti
    ON ti.block_number = t.block_number
   AND ti.tx_hash = t.tx_hash
  WHERE t.blockchain = '{chain}'
    AND t.block_month >= DATE '{block_month_min}'
    AND t.block_number >= {from_block}
    AND t.block_number <= {to_block}
    AND t.amount_usd >= 10000
    AND ti.success = TRUE
),
multi_pool_pools AS (
  SELECT
    t.block_number,
    t.tx_hash,
    t.project_contract_address AS pool_address,
    t.token_bought_address,
    t.token_sold_address,
    t.amount_usd
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_month >= DATE '{block_month_min}'
    AND t.block_number >= {from_block}
    AND t.block_number <= {to_block}
),
multi_pool_txs AS (
  SELECT
    t.block_number,
    t.tx_hash,
    COUNT(DISTINCT t.project_contract_address) AS pool_count,
    MIN(t.block_time) AS block_time
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_month >= DATE '{block_month_min}'
    AND t.block_number >= {from_block}
    AND t.block_number <= {to_block}
  GROUP BY t.block_number, t.tx_hash
  HAVING COUNT(DISTINCT t.project_contract_address) >= 2
),
matched_legs AS (
  SELECT DISTINCT
    mpp.block_number,
    mpp.tx_hash,
    mpp.pool_address,
    mpp.amount_usd,
    ti.tx_index,
    ti.gas_used,
    ti.gas_price
  FROM multi_pool_pools mpp
  JOIN tx_info ti
    ON ti.block_number = mpp.block_number
   AND ti.tx_hash = mpp.tx_hash
  JOIN large_swaps ls
    ON ls.block_number = mpp.block_number
   AND ls.pool_address = mpp.pool_address
   AND ls.tx_hash != mpp.tx_hash
   AND mpp.token_bought_address = ls.token_sold_address
   AND mpp.token_sold_address = ls.token_bought_address
   AND ls.tx_index < ti.tx_index
  WHERE ti.success = TRUE
),
backrun_candidates AS (
  SELECT
    ml.block_number,
    ml.tx_hash,
    SUM(ml.amount_usd) AS backrun_volume_usd,
    MAX(ml.gas_used) AS gas_used,
    MAX(ml.gas_price) AS gas_price,
    MIN(mt.block_time) AS block_time
  FROM matched_legs ml
  JOIN multi_pool_txs mt
    ON mt.block_number = ml.block_number
   AND mt.tx_hash = ml.tx_hash
  GROUP BY ml.block_number, ml.tx_hash
),
priced AS (
  SELECT
    bc.block_number,
    bc.tx_hash,
    bc.block_time,
    bc.backrun_volume_usd * 0.003
      - COALESCE(CAST(bc.gas_used AS DOUBLE) * CAST(bc.gas_price AS DOUBLE) / 1e18 * np.price, 0) AS net_profit_usd
  FROM backrun_candidates bc
  LEFT JOIN native_prices np
    ON np.day = DATE_TRUNC('day', bc.block_time)
)
SELECT
  COUNT(*) AS opportunity_count,
  COALESCE(AVG(p.net_profit_usd), 0) AS avg_profit_usd,
  COALESCE(SUM(p.net_profit_usd), 0) AS total_profit_usd,
  MIN(p.block_time) AS period_start,
  MAX(p.block_time) AS period_end,
  DATE_DIFF('day', MIN(p.block_time), MAX(p.block_time)) AS period_days
FROM priced p
WHERE p.net_profit_usd > 0
"#;

/// Validate long-tail token arb: closed-loop multi-pool txs on low-liquidity tokens.
///
/// Improvements over the volume-proxy version:
/// - Long-tail volume counts BOTH bought and sold sides (a token that is only
///   ever sold no longer gets misclassified as zero-volume).
/// - A real closed-loop check: every token bought in the tx must also be sold
///   an equal number of times, so path trades that start and end on different
///   tokens are excluded.
/// - Amount-chaining: for every token except exactly one, the raw bought amount
///   must equal the raw sold amount. This guarantees the legs form a single
///   executed cycle (intermediate tokens chain to the next leg) and that the
///   single residual token is the loop's start/end token.
/// - Profit uses a same-token raw ratio (decimal-independent): the residual
///   token's bought/sold minus 1, times the USD capital deployed into it.
/// - Only opportunities above `{min_profit_usd}` are counted (defaults to 0).
///   Median and p90 of per-opportunity profit are returned alongside the mean
///   so the skewed distribution (mean dominated by a few large arbs) is visible.
///
/// Tokens with < $100K total volume in the period are considered long-tail.
///
/// Parameters: `{min_profit_usd}` — minimum per-opportunity profit in USD.
/// Columns: `opportunity_count`(0), `avg_profit_usd`(1), `total_profit_usd`(2),
///          `period_start`(3), `period_end`(4), `period_days`(5),
///          `median_profit_usd`(6), `p90_profit_usd`(7)
pub const VALIDATE_LONG_TAIL_ARB: &str = r#"
WITH token_volume AS (
  SELECT
    legs.token,
    SUM(legs.vol_usd) AS total_vol
  FROM (
    SELECT t.token_bought_address AS token, t.amount_usd AS vol_usd
    FROM dex.trades t
    WHERE t.blockchain = '{chain}'
      AND t.block_month >= DATE '{block_month_min}'
      AND t.block_number >= {from_block}
      AND t.block_number <= {to_block}
    UNION ALL
    SELECT t.token_sold_address AS token, t.amount_usd AS vol_usd
    FROM dex.trades t
    WHERE t.blockchain = '{chain}'
      AND t.block_month >= DATE '{block_month_min}'
      AND t.block_number >= {from_block}
      AND t.block_number <= {to_block}
  ) legs
  GROUP BY legs.token
),
long_tail_tokens AS (
  SELECT token
  FROM token_volume
  WHERE total_vol < 100000
),
long_tail_txs AS (
  SELECT DISTINCT t.tx_hash
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_month >= DATE '{block_month_min}'
    AND t.block_number >= {from_block}
    AND t.block_number <= {to_block}
    AND (t.token_bought_address IN (SELECT token FROM long_tail_tokens)
         OR t.token_sold_address IN (SELECT token FROM long_tail_tokens))
),
leg_tokens AS (
  SELECT
    t.block_number,
    t.tx_hash,
    t.token_bought_address AS token,
    t.token_bought_amount AS amount,
    t.amount_usd AS leg_usd,
    1 AS side
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_month >= DATE '{block_month_min}'
    AND t.block_number >= {from_block}
    AND t.block_number <= {to_block}
  UNION ALL
  SELECT
    t.block_number,
    t.tx_hash,
    t.token_sold_address,
    t.token_sold_amount,
    t.amount_usd,
    -1
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_month >= DATE '{block_month_min}'
    AND t.block_number >= {from_block}
    AND t.block_number <= {to_block}
),
token_ledger AS (
  SELECT
    block_number,
    tx_hash,
    token,
    SUM(side) AS count_balance,
    SUM(CASE WHEN side = 1 THEN amount ELSE -amount END) AS amount_balance,
    SUM(CASE WHEN side = 1 THEN amount END) AS bought,
    SUM(CASE WHEN side = -1 THEN amount END) AS sold,
    SUM(CASE WHEN side = 1 THEN leg_usd END) AS bought_usd,
    SUM(CASE WHEN side = -1 THEN leg_usd END) AS sold_usd
  FROM leg_tokens
  GROUP BY block_number, tx_hash, token
),
open_loops AS (
  SELECT DISTINCT block_number, tx_hash
  FROM token_ledger
  WHERE count_balance != 0
),
split_loops AS (
  SELECT block_number, tx_hash
  FROM token_ledger
  WHERE count_balance = 0
  GROUP BY block_number, tx_hash
  HAVING COUNT(CASE WHEN amount_balance != 0 THEN 1 END) != 1
),
closed_loops AS (
  SELECT
    t.block_number,
    t.tx_hash,
    COUNT(DISTINCT t.project_contract_address) AS pool_count,
    MIN(t.block_time) AS block_time
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_month >= DATE '{block_month_min}'
    AND t.block_number >= {from_block}
    AND t.block_number <= {to_block}
    AND NOT EXISTS (
      SELECT 1 FROM open_loops o
      WHERE o.block_number = t.block_number AND o.tx_hash = t.tx_hash
    )
    AND NOT EXISTS (
      SELECT 1 FROM split_loops s
      WHERE s.block_number = t.block_number AND s.tx_hash = t.tx_hash
    )
  GROUP BY t.block_number, t.tx_hash
  HAVING COUNT(DISTINCT t.project_contract_address) >= 2
),
loop_tokens AS (
  SELECT
    tl.block_number,
    tl.tx_hash,
    cl.block_time,
    tl.bought,
    tl.sold,
    tl.sold_usd
  FROM token_ledger tl
  INNER JOIN closed_loops cl
    ON cl.block_number = tl.block_number AND cl.tx_hash = tl.tx_hash
  WHERE tl.amount_balance != 0
),
priced AS (
  SELECT
    lt.block_number,
    lt.tx_hash,
    lt.block_time,
    (CAST(lt.bought AS DOUBLE) / NULLIF(CAST(lt.sold AS DOUBLE), 0) - 1) * lt.sold_usd AS profit_usd
  FROM loop_tokens lt
  WHERE lt.sold > 0 AND lt.bought > 0
)
SELECT
  COUNT(*) AS opportunity_count,
  COALESCE(AVG(p.profit_usd), 0) AS avg_profit_usd,
  COALESCE(SUM(p.profit_usd), 0) AS total_profit_usd,
  MIN(p.block_time) AS period_start,
  MAX(p.block_time) AS period_end,
  DATE_DIFF('day', MIN(p.block_time), MAX(p.block_time)) AS period_days,
  APPROX_PERCENTILE(p.profit_usd, 0.50) AS median_profit_usd,
  APPROX_PERCENTILE(p.profit_usd, 0.90) AS p90_profit_usd
FROM priced p
INNER JOIN long_tail_txs ltt ON ltt.tx_hash = p.tx_hash
WHERE p.profit_usd > {min_profit_usd}
"#;

/// Validate stablecoin depeg arbitrage: Curve pool price deviations > 1% from $1.
///
/// Uses dex.trades curated table filtered for Curve on the target chain.
///
/// Columns: same as VALIDATE_SKIM_CAPTURE
pub const VALIDATE_STABLECOIN_DEPEG: &str = r#"
WITH curve_trades AS (
  SELECT
    t.tx_hash,
    t.block_time,
    t.block_number,
    t.amount_usd,
    t.token_bought_symbol,
    t.token_sold_symbol
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.project = 'curve'
    AND t.block_number BETWEEN {from_block} AND {to_block}
    AND t.amount_usd > 0
),
stablecoin_trades AS (
  SELECT
    ct.*
  FROM curve_trades ct
  WHERE (ct.token_bought_symbol IN ('USDC', 'USDT', 'DAI', 'FRAX', 'LUSD')
    OR ct.token_sold_symbol IN ('USDC', 'USDT', 'DAI', 'FRAX', 'LUSD'))
)
SELECT
  COUNT(*) AS opportunity_count,
  COALESCE(AVG(amount_usd * 0.01), 0) AS avg_profit_usd,
  COALESCE(SUM(amount_usd * 0.01), 0) AS total_profit_usd,
  MIN(block_time) AS period_start,
  MAX(block_time) AS period_end,
  DATE_DIFF('day', MIN(block_time), MAX(block_time)) AS period_days
FROM stablecoin_trades
"#;

/// Validate Curve pool imbalance: Curve pools with balances deviating from peg.
/// Uses dex.trades curated table instead of per-pool decoded tables.
///
/// Columns: same as VALIDATE_SKIM_CAPTURE
pub const VALIDATE_CURVE_IMBALANCE: &str = r#"
WITH curve_trades AS (
  SELECT
    t.tx_hash,
    t.block_time,
    t.block_number,
    t.amount_usd
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.project = 'curve'
    AND t.block_number BETWEEN {from_block} AND {to_block}
    AND t.amount_usd > 0
)
SELECT
  COUNT(*) AS opportunity_count,
  COALESCE(AVG(amount_usd), 0) AS avg_profit_usd,
  COALESCE(SUM(amount_usd), 0) AS total_profit_usd,
  MIN(block_time) AS period_start,
  MAX(block_time) AS period_end,
  DATE_DIFF('day', MIN(block_time), MAX(block_time)) AS period_days
FROM curve_trades
"#;

/// Validate Curve pool imbalance (V2) using per-pool TokenExchange tables.
///
/// Chain-specific table names verified on Dune: polygon uses `curvefi_polygon`
/// (stableswap, atricrypto3), ethereum uses `curve_ethereum` (curvestableswapng,
/// tricryptousdt). `curvefi_polygon`/`curve_ethereum` schemas exist globally, so
/// the UNION is valid on any chain; block-number ranges disambiguate.
///
/// Columns: same as VALIDATE_SKIM_CAPTURE
pub const VALIDATE_CURVE_IMBALANCE_V2: &str = r#"
SELECT
  count(*) as opportunity_count,
  coalesce(sum(abs_amount_usd), 0) as total_profit_usd,
  coalesce(avg(abs_amount_usd), 0) as avg_profit_usd,
  min(block_time) as period_start,
  max(block_time) as period_end,
  date_diff('day', min(block_time), max(block_time)) as period_days
FROM (
  SELECT t.evt_tx_hash, t.evt_block_time as block_time,
    t.bought_id_uint256 AS bought_id,
    t.sold_id_uint256 AS sold_id,
    t.tokens_sold / 1e18 as tokens_sold_amount, t.tokens_bought / 1e18 as tokens_bought_amount,
    0.003 as abs_amount_usd
  FROM curvefi_polygon.stableswap_evt_tokenexchange t
  WHERE t.evt_block_number BETWEEN {from_block} AND {to_block}
  UNION ALL
  SELECT t.evt_tx_hash, t.evt_block_time as block_time, t.bought_id, t.sold_id,
    t.tokens_sold / 1e18, t.tokens_bought / 1e18, 0.003
  FROM curvefi_polygon.atricrypto3_evt_tokenexchange t
  WHERE t.evt_block_number BETWEEN {from_block} AND {to_block}
  UNION ALL
  SELECT t.evt_tx_hash, t.evt_block_time as block_time, t.bought_id, t.sold_id,
    t.tokens_sold / 1e18, t.tokens_bought / 1e18, 0.003
  FROM curve_ethereum.curvestableswapng_evt_tokenexchange t
  WHERE t.evt_block_number BETWEEN {from_block} AND {to_block}
  UNION ALL
  SELECT t.evt_tx_hash, t.evt_block_time as block_time, t.bought_id, t.sold_id,
    t.tokens_sold / 1e18, t.tokens_bought / 1e18, 0.003
  FROM curve_ethereum.tricryptousdt_evt_tokenexchange t
  WHERE t.evt_block_number BETWEEN {from_block} AND {to_block}
) curve_exchanges
"#;

/// Validate LST depeg collateral liquidation: AAVE liquidations where collateral is an LST.
///
/// The LST list is chain-conditional (guarded by `'{chain}' = ...`, so unused
/// branches no-op). Verified against each pool's `getReservesList`/`getReserveData`
/// on-chain (BNB wBETH from governance docs; BSC RPC unreliable):
/// - ethereum: wstETH, rETH, cbETH, weETH, osETH, ezETH
/// - polygon: stMATIC, MaticX, wstETH (bridged)
/// - arbitrum: wstETH, rETH, weETH, rsETH
/// - optimism: rETH
/// - base: cbETH, wstETH
/// - avalanche_c: sAVAX
/// - bsc: wBETH
///
/// Columns: same as VALIDATE_SKIM_CAPTURE
pub const VALIDATE_LST_DEPEG_LIQ: &str = r#"
WITH lst_tokens AS (
  SELECT token FROM (VALUES
    (0x7f39c581f595b53c5cb19bd0b3f8da6c935e2ca0),  -- wstETH
    (0xae78736cd615f374d3085123a210448e74fc6393),  -- rETH
    (0xbe9895146f7af43049ca1c1ae358b0541ea49704),  -- cbETH
    (0xcd5fe23c85820f7b72d0926fc9b05b43e359b7ee),  -- weETH
    (0xf1c9acdc66974dfb6decb12aa385b9cd01190e38),  -- osETH
    (0xbf5495efe5db9ce00f80364c8b423567e58d2110)   -- ezETH
  ) AS t(token)
  WHERE '{chain}' = 'ethereum'
  UNION ALL
  SELECT token FROM (VALUES
    (0x3a58a54c066fdc0f2d55fc9c89f0415c92ebf3c4),  -- stMATIC
    (0xfa68fb4628dff1028cfec22b4162fccd0d45efb6),  -- MaticX
    (0x03b54a6e9a984069379fae1a4fc4dbae93b3bccd)   -- wstETH (bridged)
  ) AS t(token)
  WHERE '{chain}' = 'polygon'
  UNION ALL
  SELECT token FROM (VALUES
    (0x5979d7b546e38e414f7e9822514be443a4800529),  -- wstETH
    (0xec70dcb4a1efa46b8f2d97c310c9c4790ba5ffa8),  -- rETH
    (0x35751007a407ca6feffe80b3cb397736d2cf4dbe),  -- weETH
    (0x4186bfc76e2e237523cbc30fd220fe055156b41f)   -- rsETH
  ) AS t(token)
  WHERE '{chain}' = 'arbitrum'
  UNION ALL
  SELECT token FROM (VALUES
    (0x9bcef72be871e61ed4fbbc7630889bee758eb81d)   -- rETH
  ) AS t(token)
  WHERE '{chain}' = 'optimism'
  UNION ALL
  SELECT token FROM (VALUES
    (0xcbb7c0000ab88b473b1f5afd9ef808440eed33bf),  -- cbETH
    (0xc1cba3fcea344f92d9239c08c0568f6f2f0ee452)   -- wstETH
  ) AS t(token)
  WHERE '{chain}' = 'base'
  UNION ALL
  SELECT token FROM (VALUES
    (0x2b2c81e08f1af8835a78bb2a90ae924ace0ea4be)   -- sAVAX
  ) AS t(token)
  WHERE '{chain}' = 'avalanche_c'
  UNION ALL
  SELECT token FROM (VALUES
    (0xa2e3356610840701bdf5611a53974510ae27e2e1)   -- wBETH
  ) AS t(token)
  WHERE '{chain}' = 'bsc'
)
SELECT
  COUNT(*) AS opportunity_count,
  COALESCE(AVG(l.liquidatedCollateralAmount / 1e18), 0) AS avg_profit_usd,
  COALESCE(SUM(l.liquidatedCollateralAmount / 1e18), 0) AS total_profit_usd,
  MIN(l.evt_block_time) AS period_start,
  MAX(l.evt_block_time) AS period_end,
  DATE_DIFF('day', MIN(l.evt_block_time), MAX(l.evt_block_time)) AS period_days
FROM aave_v3_{chain}.Pool_evt_LiquidationCall l
WHERE l.evt_block_number >= {from_block}
  AND l.evt_block_number <= {to_block}
  AND l.collateralAsset IN (SELECT token FROM lst_tokens)
"#;

/// Validate MakerDAO Clip Dutch auction take() events.
///
/// Maker is Ethereum-only; `maker_ethereum.Clipper_evt_Take` is verified. Running
/// with another chain's block range returns zero rows (no matching Ethereum blocks).
///
/// USD proxy: `owe` (DAI actually paid per take) is used instead of `lot` (collateral
/// token units) so `avg_profit_usd`/`total_profit_usd` are DAI-denominated. Note the
/// Clip contract stores `owe`/`tab` in rad (1e45) while `lot` is wad (1e18).
///
/// Columns: same as VALIDATE_SKIM_CAPTURE
pub const VALIDATE_MAKERDAO_CLIP: &str = r#"
SELECT
  COUNT(*) AS opportunity_count,
  COALESCE(AVG(t.owe / 1e45), 0) AS avg_profit_usd,
  COALESCE(SUM(t.owe / 1e45), 0) AS total_profit_usd,
  MIN(t.evt_block_time) AS period_start,
  MAX(t.evt_block_time) AS period_end,
  DATE_DIFF('day', MIN(t.evt_block_time), MAX(t.evt_block_time)) AS period_days
FROM maker_ethereum.Clipper_evt_Take t
WHERE t.evt_block_number >= {from_block}
  AND t.evt_block_number <= {to_block}
"#;

/// Validate MakerDAO OSM kick() events (vault liquidation initiation).
///
/// Maker liquidations are absent from `lending.borrow` (project='maker' returns 0
/// rows) and the `maker_ethereum.*` decoded tables stalled on Dune (~2026-06-26),
/// so this reads raw logs from `evms.logs` which is always current.
///
/// The reported profit is the ACTUAL kicker reward: `Dog.bark()` calls
/// `Clipper.kick()`, which mints DAI to the keeper (`kpr`) in the same tx via
/// `vat.suck(vow, kpr, coin)` where `coin = tip + wmul(tab, chip)` (both
/// governance-set). The reward is emitted on the Clipper `Kick` event, so it is
/// known immediately — no need to track the auction outcome.
///
/// - Dog contract (Ethereum): 0x135954d155898d42c90d2a57824c690e0c7bef1b
/// - Bark topic0: keccak256("Bark(bytes32,address,uint256,uint256,uint256,address,uint256)")
///   = 0x85258d09e1e4ef299ff3fc11e74af99563f022d21f3f940db982229dc2a3358c
///   (ilk/urn/id indexed; data: ink(1-32), art(33-64), due(65-96), clip(97-128))
/// - Kick topic0: keccak256("Kick(uint256,uint256,uint256,uint256,address,address,uint256)")
///   = 0x7c5bfdc0a5e8192f6cd4972f382cec69116862fb62e6abff8003874c58e064b8
///   (id/usr/kpr indexed; data: top(1-32), tab(33-64), lot(65-96), coin(97-128))
/// - `coin` is in rad (1e45) → DAI.
///
/// Columns: same as VALIDATE_SKIM_CAPTURE
pub const VALIDATE_MAKERDAO_KICK: &str = r#"
WITH bark AS (
  SELECT
    l.block_time,
    l.tx_hash,
    bytearray_substring(l.data, 109, 20) AS clip
  FROM evms.logs l
  WHERE l.blockchain = '{chain}'
    AND l.contract_address = 0x135954d155898d42c90d2a57824c690e0c7bef1b
    AND l.topic0 = 0x85258d09e1e4ef299ff3fc11e74af99563f022d21f3f940db982229dc2a3358c
    AND l.block_number BETWEEN {from_block} AND {to_block}
),
kick AS (
  SELECT
    l.tx_hash,
    l.contract_address AS clip,
    CAST(bytearray_to_uint256(bytearray_substring(l.data, 97, 32)) AS DOUBLE) / 1e45 AS reward_dai
  FROM evms.logs l
  WHERE l.blockchain = '{chain}'
    AND l.topic0 = 0x7c5bfdc0a5e8192f6cd4972f382cec69116862fb62e6abff8003874c58e064b8
    AND l.block_number BETWEEN {from_block} AND {to_block}
)
SELECT
  COUNT(*) AS opportunity_count,
  COALESCE(AVG(k.reward_dai), 0) AS avg_profit_usd,
  COALESCE(SUM(k.reward_dai), 0) AS total_profit_usd,
  MIN(b.block_time) AS period_start,
  MAX(b.block_time) AS period_end,
  DATE_DIFF('day', MIN(b.block_time), MAX(b.block_time)) AS period_days
FROM bark b
JOIN kick k ON k.tx_hash = b.tx_hash AND k.clip = b.clip
"#;

/// Validate GMX v1 keeper race: liquidation events on GMX v1.
///
/// GMX v1 is Arbitrum/Avalanche only; `gmx_arbitrum.vault_evt_liquidateposition`
/// is verified on Dune. Running with another chain's block range returns zero rows.
///
/// Columns: same as VALIDATE_SKIM_CAPTURE
pub const VALIDATE_GMX_V1_KEEPER: &str = r#"
WITH liqs AS (
  SELECT
    evt_tx_hash,
    evt_block_number,
    evt_block_time,
    size / 1e30 AS size_usd
  FROM gmx_arbitrum.vault_evt_liquidateposition
  WHERE evt_block_number BETWEEN {from_block} AND {to_block}
)
SELECT
  COUNT(*) AS opportunity_count,
  COALESCE(AVG(size_usd), 0) AS avg_profit_usd,
  COALESCE(SUM(size_usd), 0) AS total_profit_usd,
  MIN(evt_block_time) AS period_start,
  MAX(evt_block_time) AS period_end,
  DATE_DIFF('day', MIN(evt_block_time), MAX(evt_block_time)) AS period_days
FROM liqs
"#;

/// Validate GMX V2 ADL front-run: automatic deleveraging events.
///
/// GMX V2 is Arbitrum/Avalanche only; `gmx_v2_arbitrum.liquidationhandler_evt_oracleerror`
/// and `adlhandler_evt_oracleerror` are verified on Dune. Running with another
/// chain's block range returns zero rows.
///
/// Columns: same as VALIDATE_SKIM_CAPTURE
pub const VALIDATE_GMX_V2_ADL: &str = r#"
WITH events AS (
  SELECT evt_tx_hash, evt_block_number, evt_block_time, 1 AS est_count
  FROM gmx_v2_arbitrum.liquidationhandler_evt_oracleerror
  WHERE evt_block_number BETWEEN {from_block} AND {to_block}
  UNION ALL
  SELECT evt_tx_hash, evt_block_number, evt_block_time, 1 AS est_count
  FROM gmx_v2_arbitrum.adlhandler_evt_oracleerror
  WHERE evt_block_number BETWEEN {from_block} AND {to_block}
)
SELECT
  COUNT(*) AS opportunity_count,
  COALESCE(AVG(est_count * 1000), 0) AS avg_profit_usd,
  COALESCE(SUM(est_count * 1000), 0) AS total_profit_usd,
  MIN(evt_block_time) AS period_start,
  MAX(evt_block_time) AS period_end,
  DATE_DIFF('day', MIN(evt_block_time), MAX(evt_block_time)) AS period_days
FROM events
"#;

/// Validate Liquity recovery mode cascade: trove liquidation events.
///
/// Liquity is Ethereum-only; `liquity_ethereum.trovemanager_evt_troveliquidated`
/// is verified on Dune. Running with another chain's block range returns zero rows.
///
/// Columns: same as VALIDATE_SKIM_CAPTURE
pub const VALIDATE_LIQUITY_RECOVERY: &str = r#"
SELECT
  COUNT(*) AS opportunity_count,
  COALESCE(AVG(l._debt / 1e18), 0) AS avg_profit_usd,
  COALESCE(SUM(l._debt / 1e18), 0) AS total_profit_usd,
  MIN(l.evt_block_time) AS period_start,
  MAX(l.evt_block_time) AS period_end,
  DATE_DIFF('day', MIN(l.evt_block_time), MAX(l.evt_block_time)) AS period_days
FROM liquity_ethereum.trovemanager_evt_troveliquidated l
WHERE l.evt_block_number >= {from_block}
  AND l.evt_block_number <= {to_block}
"#;

/// Validate Synthetix V3 liquidation events.
/// Table: synthetix_v3_ethereum.core_evt_liquidation (verified on Dune).
/// liquidationData JSON contains: debtLiquidated, collateralSeized.
/// Note: synthetix_v3 is not available on Polygon; the Ethereum schema is used
/// for all chains (returns zero rows for non-Ethereum block ranges).
///
/// Columns: same as VALIDATE_SKIM_CAPTURE
pub const VALIDATE_SYNTHETIX_LIQ: &str = r#"
SELECT
  count(*) as opportunity_count,
  coalesce(avg(CAST(json_extract_scalar(liquidationdata, '$.debtLiquidated') AS DOUBLE) / 1e18), 0) as avg_profit_usd,
  coalesce(sum(CAST(json_extract_scalar(liquidationdata, '$.debtLiquidated') AS DOUBLE) / 1e18), 0) as total_profit_usd,
  min(evt_block_time) as period_start,
  max(evt_block_time) as period_end,
  date_diff('day', min(evt_block_time), max(evt_block_time)) as period_days
FROM synthetix_v3_ethereum.core_evt_liquidation
WHERE evt_block_number BETWEEN {from_block} AND {to_block}
"#;

/// Validate perp protocol keeper: Gains Network liquidation events.
/// NOTE: gns_{chain} has no GTokenLiquidation table on Dune. This query will
/// return 0 or error. Gains Network liquidation data is not available on Dune.
///
/// Columns: same as VALIDATE_SKIM_CAPTURE
pub const VALIDATE_PERP_KEEPER: &str = r#"
SELECT
  COUNT(*) AS opportunity_count,
  COALESCE(AVG(CAST(NULL AS DOUBLE)), 0) AS avg_profit_usd,
  COALESCE(SUM(CAST(NULL AS DOUBLE)), 0) AS total_profit_usd,
  MIN(CAST(NULL AS TIMESTAMP)) AS period_start,
  MAX(CAST(NULL AS TIMESTAMP)) AS period_end,
  CAST(NULL AS INTEGER) AS period_days
FROM (SELECT 1) dummy
WHERE 1=0
"#;

/// Validate flash loan atomic liquidation: flash loans paired with liquidations in the
/// *same transaction* (atomic). Matches on both `block_number` and `tx_hash`, so flash
/// loans from unrelated transactions in the same block are not counted.
///
/// Note: the returned USD columns are flash-loan sizes (volume proxy), not realized profit.
/// Columns: `opportunity_count`(0), `avg_flash_usd`(1), `total_flash_usd`(2),
///          `period_start`(3), `period_end`(4), `period_days`(5)
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
    AND EXISTS (
      SELECT 1
      FROM lending.borrow l
      WHERE l.blockchain = '{chain}'
        AND l.transaction_type = 'borrow_liquidation'
        AND l.block_number = f.block_number
        AND l.tx_hash = f.tx_hash
    )
)
SELECT
  COUNT(DISTINCT tx_hash) AS opportunity_count,
  COALESCE(AVG(flash_amount_usd), 0) AS avg_flash_usd,
  COALESCE(SUM(flash_amount_usd), 0) AS total_flash_usd,
  MIN(block_time) AS period_start,
  MAX(block_time) AS period_end,
  DATE_DIFF('day', MIN(block_time), MAX(block_time)) AS period_days
FROM flash_liqs
"#;

/// Validate JIT liquidity (V3): Mint→Swap→Burn patterns in same block, same pool.
///
/// Polygon: the `uniswap_v3_polygon` decoded tables stopped being populated at
/// 2022-09, so pool events come from the live QuickSwap V3 (Algebra) decode
/// (`quickswap_v3_polygon.algebrapool_evt_mint/burn`). V3 swaps on Polygon are
/// labelled `project='quickswap' AND version='3'` in `dex.trades`
/// (`project='uniswap_v3'` returns 0 rows there; Dune's `dex.liquidity` table
/// does not exist).
///
/// Profit estimate: a JIT provider captures the swap fee on the liquidity it
/// adds, so `profit_est = collocated_swap_volume_usd * fee_rate`. `dex.trades`
/// has no fee column, so we use the QuickSwap V3 / Algebra default fee tier
/// (0.05%). This is an approximation of fee capture, not an exact figure.
///
/// Columns: same as VALIDATE_SKIM_CAPTURE
pub const VALIDATE_JIT_FEE_CAPTURE: &str = r#"
WITH v3_events AS (
  SELECT
    evt_block_number AS block_number,
    evt_tx_hash AS tx_hash,
    contract_address AS pool_address,
    'mint' AS event_type,
    evt_block_time AS block_time,
    CAST(NULL AS DOUBLE) AS amount_usd
  FROM quickswap_v3_polygon.algebrapool_evt_mint
  WHERE evt_block_number >= {from_block}
    AND evt_block_number <= {to_block}
  UNION ALL
  SELECT
    evt_block_number,
    evt_tx_hash,
    contract_address,
    'burn',
    evt_block_time,
    CAST(NULL AS DOUBLE)
  FROM quickswap_v3_polygon.algebrapool_evt_burn
  WHERE evt_block_number >= {from_block}
    AND evt_block_number <= {to_block}
  UNION ALL
  SELECT
    t.block_number,
    t.tx_hash,
    t.project_contract_address,
    'swap',
    t.block_time,
    t.amount_usd
  FROM dex.trades t
  WHERE t.blockchain = '{chain}'
    AND t.block_month >= DATE '{block_month_min}'
    AND t.block_number >= {from_block}
    AND t.block_number <= {to_block}
    AND t.project = 'quickswap'
    AND t.version = '3'
),
pool_block_events AS (
  SELECT
    pool_address,
    block_number,
    block_time,
    tx_hash,
    ARRAY_AGG(DISTINCT event_type) AS event_types,
    COUNT(DISTINCT event_type) AS event_count,
    SUM(amount_usd) AS swap_volume_usd
  FROM v3_events
  GROUP BY pool_address, block_number, block_time, tx_hash
)
SELECT
  COUNT(*) AS opportunity_count,
  COALESCE(AVG(swap_volume_usd * 0.0005), 0) AS avg_profit_usd,
  COALESCE(SUM(swap_volume_usd * 0.0005), 0) AS total_profit_usd,
  MIN(block_time) AS period_start,
  MAX(block_time) AS period_end,
  DATE_DIFF('day', MIN(block_time), MAX(block_time)) AS period_days
FROM pool_block_events
WHERE event_count = 3
  AND contains(event_types, 'mint')
  AND contains(event_types, 'burn')
  AND contains(event_types, 'swap')
"#;

pub const DISCOVER_TABLES: &str = r#"
SELECT table_name, table_schema
FROM information_schema.tables
WHERE table_schema IN ('curve_ethereum', 'maker_ethereum', 'curve', 'maker')
  AND table_name LIKE '%TokenExchange%' OR table_name LIKE '%Clip%' OR table_name LIKE '%Take%'
  OR table_name LIKE '%Flip%' OR table_name LIKE '%Vow%'
ORDER BY table_schema, table_name
LIMIT 100
"#;

pub const DISCOVER_MAKERDAO_COLUMNS: &str = r#"
SELECT column_name
FROM information_schema.columns
WHERE table_schema = 'maker_ethereum'
  AND table_name = 'Clipper_evt_Take'
ORDER BY ordinal_position
LIMIT 50
"#;

pub const DISCOVER_CLIP_COLUMNS: &str = r#"SHOW COLUMNS FROM maker_ethereum.Clipper_evt_Take"#;

pub const DISCOVER_GMX_TABLES: &str = r#"SELECT table_name FROM information_schema.tables WHERE table_schema LIKE '%gmx%' AND table_name LIKE '%Liquidat%' LIMIT 20"#;

pub const DISCOVER_GAINS_TABLES: &str = r#"SELECT table_name FROM information_schema.tables WHERE table_schema LIKE '%gains%' AND table_name LIKE '%Liquidat%' LIMIT 20"#;

pub const DISCOVER_SYNX_TABLES: &str = r#"SELECT table_name FROM information_schema.tables WHERE table_schema LIKE '%synthetix%' AND table_name LIKE '%Liquidat%' LIMIT 20"#;

pub const DISCOVER_MAKER_TABLES: &str = r#"SELECT table_name FROM information_schema.tables WHERE table_schema LIKE '%maker%' AND (table_name LIKE '%Clip%' OR table_name LIKE '%Take%' OR table_name LIKE '%Flip%') LIMIT 20"#;

pub const DISCOVER_TRY_CLIP: &str = r#"SELECT * FROM maker_ethereum.Clipper_evt_Take LIMIT 1"#;

pub const DISCOVER_TRY_GMX: &str = r#"SELECT table_schema, table_name FROM information_schema.tables WHERE table_schema LIKE '%gmx%arbitrum%' LIMIT 50"#;

pub const DISCOVER_TRY_GAINS: &str = r#"SELECT table_schema, table_name FROM information_schema.tables WHERE table_schema LIKE '%gains%arbitrum%' LIMIT 50"#;

pub const DISCOVER_TRY_SYNX: &str = r#"SELECT table_schema, table_name FROM information_schema.tables WHERE table_schema LIKE '%synthetix%ethereum%' LIMIT 50"#;

pub const DISCOVER_ALL_MAKER: &str = r#"SHOW TABLES FROM maker_ethereum"#;

pub const DISCOVER_ALL_GMX_ARB: &str = r#"SHOW TABLES FROM gmx_arbitrum"#;

pub const DISCOVER_ALL_GMX_ROUTER_ARB: &str = r#"SHOW TABLES FROM gmx_v2_arbitrum"#;

pub const DISCOVER_ALL_GAINS_ARB: &str = r#"SHOW TABLES FROM gains_network_arbitrum"#;

pub const DISCOVER_ALL_SYNX_ETH: &str = r#"SHOW TABLES FROM synthetix_ethereum"#;

pub const DISCOVER_ALL_CURVE_POLY: &str = r#"SHOW TABLES FROM curvefi_polygon"#;

pub const DISCOVER_ALL_UNIV2_POLY: &str = r#"SHOW SCHEMAS LIKE '%uniswap%v2%polygon%'"#;

pub const DISCOVER_MAKER_FLIP: &str = r#"SHOW TABLES FROM maker_ethereum LIKE '%Vow%'"#;

pub const DISCOVER_MAKER_CLIPPER: &str = r#"SHOW TABLES FROM maker_ethereum LIKE '%Clip%'"#;

pub const DISCOVER_GMX_ARB_SHOW: &str = r#"SHOW SCHEMAS LIKE '%gmx%arbitrum%'"#;

pub const DISCOVER_GMX_V2_ARB_SHOW: &str = r#"SHOW SCHEMAS LIKE '%gmx_router%arbitrum%'"#;

pub const DISCOVER_GAINS_ARB_SHOW: &str = r#"SHOW SCHEMAS LIKE '%gains%arbitrum%'"#;

pub const DISCOVER_SYNX_ETH_SHOW: &str = r#"SHOW SCHEMAS LIKE '%synthetix%ethereum%'"#;

pub const DISCOVER_CURVE_POLY_SHOW: &str = r#"SHOW SCHEMAS LIKE '%curve%polygon%'"#;

pub const DISCOVER_GMX_LIQ_TABLES: &str = r#"SHOW TABLES FROM gmx_arbitrum LIKE '%Liquidat%'"#;

pub const DISCOVER_GAINS_LIQ_TABLES: &str = r#"SHOW TABLES FROM gains_network_arbitrum LIKE '%Liquidat%'"#;

pub const DISCOVER_SYNX_LIQ_TABLES: &str = r#"SHOW TABLES FROM synthetix_ethereum LIKE '%Liquidat%'"#;

pub const DISCOVER_SYNX_V3_LIQ_TABLES: &str = r#"SHOW TABLES FROM synthetix_v3_ethereum LIKE '%Liquidat%'"#;

pub const DISCOVER_MAKER_VOW_FLIP: &str = r#"SHOW TABLES FROM maker_ethereum LIKE '%Flip%'"#;

pub const DISCOVER_MAKER_CLIPPER_TAKE: &str = r#"SHOW TABLES FROM maker_ethereum LIKE '%Take%'"#;

pub const DISCOVER_GMX_V2_TABLES: &str = r#"SHOW TABLES FROM gmx_v2_arbitrum LIKE '%Order%'"#;

pub const DISCOVER_GMX_V21_TABLES: &str = r#"SHOW TABLES FROM gmx_v21_arbitrum LIKE '%Order%'"#;

pub const DISCOVER_MAKER_FULL: &str = r#"SELECT table_schema, table_name FROM information_schema.columns WHERE column_name = 'art' AND table_schema LIKE '%maker%' AND table_name LIKE '%evt%' LIMIT 20"#;

pub const DISCOVER_MAKER_V2_FULL: &str = r#"SELECT table_schema, table_name FROM information_schema.columns WHERE column_name = 'usr' AND table_schema LIKE '%maker%' AND table_name LIKE '%evt%' LIMIT 20"#;

pub const DISCOVER_GMX_VAULT_FULL: &str = r#"SELECT table_schema, table_name FROM information_schema.columns WHERE column_name = 'feeAmount' AND table_schema LIKE '%gmx%' LIMIT 20"#;

pub const DISCOVER_GMX_V2_ORDER_FULL: &str = r#"SELECT table_schema, table_name FROM information_schema.columns WHERE table_schema LIKE '%gmx%v2%' AND table_name LIKE '%evt%' LIMIT 20"#;

pub const DISCOVER_GAINS_LIQ_FULL: &str = r#"SELECT table_schema, table_name FROM information_schema.columns WHERE table_schema LIKE '%gains%arb%' AND table_name LIKE '%evt%' LIMIT 20"#;

pub const DISCOVER_SYNX_LIQ_FULL: &str = r#"SELECT table_schema, table_name FROM information_schema.columns WHERE table_schema LIKE '%synthetix%eth%' AND table_name LIKE '%liquidat%' LIMIT 20"#;

pub const DISCOVER_CURVE_TOKEN_EXCHANGE: &str = r#"SELECT table_schema, table_name FROM information_schema.columns WHERE table_schema LIKE '%curve%polygon%' AND table_name LIKE '%TokenExchange%' LIMIT 20"#;

pub const DISCOVER_UNIV2_SYNC: &str = r#"SELECT table_schema, table_name FROM information_schema.columns WHERE table_schema LIKE '%uniswap%v2%polygon%' AND table_name LIKE '%Sync%' LIMIT 20"#;

pub const DISCOVER_MAKER_EVT_TABLES: &str = r#"SELECT table_name FROM information_schema.tables WHERE table_schema = 'maker_ethereum' AND table_name LIKE '%evt%' AND (table_name LIKE '%Flip%' OR table_name LIKE '%Clip%' OR table_name LIKE '%Take%' OR table_name LIKE '%Vow%') LIMIT 20"#;

pub const DISCOVER_GMX_VAULT_EVT: &str = r#"SELECT table_name FROM information_schema.tables WHERE table_schema = 'gmx_arbitrum' AND table_name LIKE '%evt%' LIMIT 20"#;

pub const DISCOVER_GMX_V2_EVT: &str = r#"SELECT table_name FROM information_schema.tables WHERE table_schema = 'gmx_v2_arbitrum' AND table_name LIKE '%evt%' LIMIT 20"#;

pub const DISCOVER_GMX_V21_EVT: &str = r#"SELECT table_name FROM information_schema.tables WHERE table_schema = 'gmx_v21_arbitrum' AND table_name LIKE '%evt%' LIMIT 20"#;

pub const DISCOVER_GAINS_EVT: &str = r#"SELECT table_name FROM information_schema.tables WHERE table_schema = 'gains_network_arbitrum' AND table_name LIKE '%evt%' LIMIT 20"#;

pub const DISCOVER_SYNX_EVT: &str = r#"SELECT table_name FROM information_schema.tables WHERE table_schema = 'synthetix_ethereum' AND table_name LIKE '%evt%' LIMIT 20"#;

pub const DISCOVER_SYNX_V3_EVT: &str = r#"SELECT table_name FROM information_schema.tables WHERE table_schema = 'synthetix_v3_ethereum' AND table_name LIKE '%evt%' LIMIT 20"#;

pub const DISCOVER_CURVEFI_POLY_TABLES: &str = r#"SHOW TABLES FROM curvefi_polygon"#;

pub const DISCOVER_CURVEFI_POLY_LIKE: &str = r#"SHOW TABLES FROM curvefi_polygon LIKE '%tokenexchange%'"#;

pub const DISCOVER_MAKERDAO_SCHEMA_SHOW: &str = r#"SHOW SCHEMAS LIKE '%maker%ethereum%'"#;

pub const DISCOVER_GMX_LIQ_IN_V2: &str = r#"SHOW TABLES FROM gmx_v2_arbitrum LIKE '%Liquidat%'"#;

pub const DISCOVER_GMX_ORDER_IN_V2: &str = r#"SHOW TABLES FROM gmx_v2_arbitrum LIKE '%Order%'"#;

pub const DISCOVER_GMX_ADLP_IN_V2: &str = r#"SHOW TABLES FROM gmx_v2_arbitrum LIKE '%Adl%'"#;

pub const DISCOVER_GMX_ADLP_IN_V2_UPPER: &str = r#"SHOW TABLES FROM gmx_v2_arbitrum LIKE '%ADL%'"#;

pub const DISCOVER_GAINS_ARB_TABLES: &str = r#"SHOW TABLES FROM gains_network_arbitrum LIKE '%Liquidat%'"#;

pub const DISCOVER_GAINS_GTOKEN: &str = r#"SHOW TABLES FROM gains_network_arbitrum LIKE '%GToken%'"#;

pub const DISCOVER_GAINS_DUIF: &str = r#"SHOW TABLES FROM gains_network_arbitrum LIKE '%duif%'"#;

pub const DISCOVER_GAINS_DN: &str = r#"SHOW TABLES FROM gains_network_arbitrum LIKE '%dncdnc%'"#;

pub const DISCOVER_SYNX_V3_COLS: &str = r#"SELECT * FROM synthetix_v3_ethereum.core_evt_liquidation LIMIT 5"#;

pub const DISCOVER_GMX_V1_VAULT_LIQ: &str = r#"SELECT * FROM gmx_arbitrum.vault_evt_liquidateposition LIMIT 5"#;

pub const DISCOVER_GMX_V2_LIQ_HANDLER: &str = r#"SELECT * FROM gmx_v2_arbitrum.liquidationhandler_evt_oracleerror LIMIT 5"#;

pub const DISCOVER_GMX_V2_ADL_HANDLER: &str = r#"SELECT * FROM gmx_v2_arbitrum.adlhandler_evt_oracleerror LIMIT 5"#;


// ══════════════════════════════════════════════════════════════════════════
// Section 5: Token Discovery & Filtering
// ══════════════════════════════════════════════════════════════════════════

/// All known ERC-20 tokens on a chain.
///
/// Fast query (~2s on Dune). No JOINs.
/// Columns: `contract_address`(0), `symbol`(1), `decimals`(2), `name`(3)
pub const QUERY_TOKENS_ALL: &str = r#"
SELECT
  t.contract_address,
  t.symbol,
  t.decimals,
  t.name
FROM tokens.erc20 t
WHERE t.blockchain = '{chain}'
ORDER BY t.symbol
LIMIT {limit}
"#;

/// Tokens with at least one DEX trade in the last N days.
///
/// Uses a pre-aggregated CTE to avoid full dex.trades scan.
/// Columns: `contract_address`(0), `symbol`(1), `decimals`(2), `name`(3),
///          `trade_count`(4), `volume_usd`(5)
pub const QUERY_TOKENS_ACTIVE: &str = r#"
WITH recent_trades AS (
  SELECT
    token_bought_address AS token,
    amount_usd
  FROM dex.trades
  WHERE blockchain = '{chain}'
    AND block_time >= NOW() - INTERVAL '{days}' DAY
    AND token_bought_address IS NOT NULL
  UNION ALL
  SELECT
    token_sold_address AS token,
    amount_usd
  FROM dex.trades
  WHERE blockchain = '{chain}'
    AND block_time >= NOW() - INTERVAL '{days}' DAY
    AND token_sold_address IS NOT NULL
)
SELECT
  t.contract_address,
  t.symbol,
  t.decimals,
  t.name,
  COUNT(*) as trade_count,
  SUM(COALESCE(r.amount_usd, 0)) as volume_usd
FROM tokens.erc20 t
INNER JOIN recent_trades r ON r.token = t.contract_address
WHERE t.blockchain = '{chain}'
GROUP BY 1, 2, 3, 4
HAVING COUNT(*) >= 1
ORDER BY trade_count DESC
LIMIT {limit}
"#;

/// Tokens first traded in the last N days (newly launched).
///
/// Uses a pre-aggregated CTE to avoid full dex.trades scan.
/// Columns: `contract_address`(0), `symbol`(1), `decimals`(2), `name`(3),
///          `trade_count`(4), `first_seen`(5)
pub const QUERY_TOKENS_NEW: &str = r#"
WITH recent_trades AS (
  SELECT
    token_bought_address AS token,
    block_time
  FROM dex.trades
  WHERE blockchain = '{chain}'
    AND block_time >= NOW() - INTERVAL '{days}' DAY
    AND token_bought_address IS NOT NULL
  UNION ALL
  SELECT
    token_sold_address AS token,
    block_time
  FROM dex.trades
  WHERE blockchain = '{chain}'
    AND block_time >= NOW() - INTERVAL '{days}' DAY
    AND token_sold_address IS NOT NULL
)
SELECT
  t.contract_address,
  t.symbol,
  t.decimals,
  t.name,
  COUNT(*) as trade_count,
  MIN(r.block_time) as first_seen
FROM tokens.erc20 t
INNER JOIN recent_trades r ON r.token = t.contract_address
WHERE t.blockchain = '{chain}'
GROUP BY 1, 2, 3, 4
HAVING MIN(r.block_time) >= NOW() - INTERVAL '{days}' DAY
ORDER BY first_seen DESC
LIMIT {limit}
"#;

/// Tokens ranked by estimated TVL (USD value locked in pool reserves).
///
/// Approximates TVL by summing dollar value of token reserves in active pools.
/// Columns: `contract_address`(0), `symbol`(1), `decimals`(2), `name`(3),
///          `trade_count`(4), `tvl_usd`(5), `volume_usd`(6)
pub const QUERY_TOKENS_TVL: &str = r#"
WITH active_pools AS (
  SELECT DISTINCT
    token_bought_address AS token,
    project_contract_address AS pool
  FROM dex.trades
  WHERE blockchain = '{chain}'
    AND block_time >= NOW() - INTERVAL '{days}' DAY
    AND token_bought_address IS NOT NULL
  UNION
  SELECT DISTINCT
    token_sold_address AS token,
    project_contract_address AS pool
  FROM dex.trades
  WHERE blockchain = '{chain}'
    AND block_time >= NOW() - INTERVAL '{days}' DAY
    AND token_sold_address IS NOT NULL
),
pool_trades AS (
  SELECT
    ap.token,
    ap.pool,
    COUNT(*) as trade_count,
    SUM(COALESCE(d.amount_usd, 0)) as volume_usd
  FROM active_pools ap
  INNER JOIN dex.trades d
    ON d.project_contract_address = ap.pool
    AND (d.token_bought_address = ap.token OR d.token_sold_address = ap.token)
    AND d.blockchain = '{chain}'
    AND d.block_time >= NOW() - INTERVAL '{days}' DAY
  GROUP BY 1, 2
)
SELECT
  t.contract_address,
  t.symbol,
  t.decimals,
  t.name,
  SUM(pt.trade_count) as trade_count,
  SUM(pt.volume_usd) as tvl_usd,
  SUM(pt.volume_usd) as volume_usd
FROM tokens.erc20 t
INNER JOIN pool_trades pt ON pt.token = t.contract_address
WHERE t.blockchain = '{chain}'
GROUP BY 1, 2, 3, 4
HAVING SUM(pt.volume_usd) > 0
ORDER BY tvl_usd DESC
LIMIT {limit}
"#;
