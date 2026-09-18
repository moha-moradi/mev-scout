//! Explorer store — SQLite persistence for realized-MEV facts.
//!
//! Separate database file from the scanner cache (`explorer_{chain}.sqlite`)
//! so backfill writes never contend with replay-path reads. WAL mode,
//! single-writer, batched transactions. Schema is Postgres-portable.
//!
//! Layers:
//! - forensic facts: `blocks`, `txs`, `transfers`, `swaps`, `mev_ops`
//! - results layer: `opportunities` fed from `ResultsFile`
//! - rejection capture: `rejected_candidates`
//! - checkpointing: `sync_state` + `blocks_classified` (gap-safe resume)

use std::path::Path;

use alloy::primitives::{Address, B256, U256};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::explorer::types::{MevEvent, MevKind};
use crate::types::{MevOpportunity, Strategy};

/// Trace-based reconciliation record for one tx (`explorer show --trace`).
///
/// Measurement-only (Phase 0): compares the classifier's expected USD profit
/// with the trace-observed native balance delta. Never feeds `classify_block`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TraceVerification {
    /// Classifier-expected USD profit (sum over the tx's ops), if priced.
    pub expected_profit_usd: Option<f64>,
    /// Trace-observed native balance delta converted to USD (signed).
    pub trace_profit_usd: Option<f64>,
    /// `(expected − trace) / expected × 100`; > 0 = classifier over-estimate.
    pub profit_error_pct: Option<f64>,
    /// Raw trace native delta in wei, signed decimal string.
    pub native_delta_wei: String,
    pub note: String,
}

/// One persisted open concentrated-liquidity position (`jit_open_positions`).
///
/// Phase 5a-0/1.5: lets cross-block JIT detection survive live restarts. A row
/// is inserted on a Mint and deleted when the matching Burn arrives (or pruned
/// once `opened_block` falls outside the block-window cap).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenPosition {
    pub pool: Address,
    pub owner: Address,
    pub tick_lower: i32,
    pub tick_upper: i32,
    pub opened_block: u64,
    pub liquidity: u128,
}

/// One persisted realized-MEV operation row (`mev_ops`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MevOpRow {
    pub id: i64,
    pub block_number: u64,
    pub tx_index: Option<u64>,
    pub tx_hash: String,
    pub ts: u64,
    pub kind: String,
    pub eoa: String,
    pub contract: Option<String>,
    pub confidence: String,
    pub canonical_id: Option<String>,
    pub profit_token: Option<String>,
    pub profit_amount: Option<String>,
    pub profit_usd: Option<f64>,
    pub gas_cost_usd: Option<f64>,
    pub net_profit_usd: Option<f64>,
    pub route_json: Option<String>,
    pub victim_hashes: Option<String>,
    pub details_json: Option<String>,
    pub detector: String,
    pub created_at: u64,
}

/// Aggregate row for stats/leaderboard queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsRow {
    pub label: String,
    pub ops: i64,
    pub gross_usd: f64,
    pub net_usd: f64,
}

/// Live-feed row (tail of `mev_ops`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedRow {
    pub ts: u64,
    pub block_number: u64,
    pub kind: String,
    pub profit_token: Option<String>,
    pub profit_amount: Option<String>,
    pub profit_usd: Option<f64>,
    pub gas_cost_usd: Option<f64>,
    pub net_profit_usd: Option<f64>,
    pub eoa: String,
    pub tx_hash: String,
    pub route_json: Option<String>,
    /// Spot USD for the chain native / wrapped-native (mevlive Price column).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_price_usd: Option<f64>,
}

pub struct ExplorerStore {
    conn: Connection,
}

/// Merge a set of key/value pairs into a `details` JSON string, preserving any
/// existing fields (used for profit_tokens and FOT/approximate annotations).
fn merge_details_json<const N: usize>(
    details_json: &str,
    pairs: [(&str, serde_json::Value); N],
) -> String {
    let mut d: serde_json::Value =
        serde_json::from_str(details_json).unwrap_or(serde_json::json!({}));
    if let Some(map) = d.as_object_mut() {
        for (k, v) in pairs {
            map.insert(k.to_string(), v);
        }
    }
    d.to_string()
}

/// Liquidation P&L (Phase 1.3): gross seized collateral value minus repaid
/// debt value, both priced at persist time. Cross-asset liquidations are an
/// approximation (liquidation bonus / market-sale slippage) and are marked
/// `inferred` with `LIQ_BONUS_APPROX`; missing prices fall back to the
/// collateral display amount with `MULTI_ASSET_PRICING`.
fn liquidation_pnl(
    ev: &MevEvent,
    token_prices: &std::collections::HashMap<Address, crate::explorer::pricing::TokenUsd>,
) -> (Option<f64>, &'static str, String) {
    let parse_addr = |k: &str| {
        ev.details
            .get(k)
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<Address>().ok())
    };
    let parse_amt = |k: &str| {
        ev.details
            .get(k)
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<U256>().ok())
    };
    let collateral_asset = parse_addr("collateral_asset");
    let debt_asset = parse_addr("debt_asset");
    let price_usd = |asset: Option<Address>, amount: Option<U256>| -> Option<f64> {
        let (a, amt) = (asset?, amount?);
        let p = token_prices.get(&a)?;
        Some(crate::explorer::pricing::token_amount_to_usd(amt, p))
    };
    let collateral_usd = price_usd(collateral_asset, parse_amt("collateral_amount"));
    let debt_usd = price_usd(debt_asset, parse_amt("debt_to_cover"));
    let cross_asset = match (collateral_asset, debt_asset) {
        (Some(c), Some(d)) => c != d && !c.is_zero() && !d.is_zero(),
        _ => true,
    };

    let mut details = ev.details.clone();
    let mut reasons = reason_list(&details);
    details["profit_usd_method"] = serde_json::json!("collateral_usd_minus_debt_usd");
    match (collateral_usd, debt_usd) {
        (Some(c), Some(d)) => {
            if cross_asset {
                reasons.push("LIQ_BONUS_APPROX".to_string());
                details["reasons"] = serde_json::json!(reasons);
                (Some(c - d), "inferred", details.to_string())
            } else {
                details["reasons"] = serde_json::json!(reasons);
                (Some(c - d), "exact", details.to_string())
            }
        }
        _ => {
            reasons.push("MULTI_ASSET_PRICING".to_string());
            details["profit_usd_fallback"] = serde_json::json!("collateral_amount");
            details["reasons"] = serde_json::json!(reasons);
            (None, "inferred", details.to_string())
        }
    }
}

/// Existing `reasons` array from event details (empty when absent).
fn reason_list(details: &serde_json::Value) -> Vec<String> {
    details
        .get("reasons")
        .and_then(|r| r.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

impl ExplorerStore {
    /// Open (or create) the explorer database at `path`.
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        let store = ExplorerStore { conn };
        store.initialize()?;
        Ok(store)
    }

    /// Open an in-memory database (tests).
    pub fn open_in_memory() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = ExplorerStore { conn };
        store.initialize()?;
        Ok(store)
    }

    fn initialize(&self) -> anyhow::Result<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS blocks(
              block_number INTEGER PRIMARY KEY,
              block_hash TEXT NOT NULL,
              ts INTEGER NOT NULL,
              producer TEXT,
              base_fee_gwei REAL,
              tx_count INTEGER,
              indexed_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS txs(
              hash TEXT PRIMARY KEY,
              block_number INTEGER NOT NULL,
              tx_index INTEGER NOT NULL,
              \"from\" TEXT NOT NULL,
              \"to\" TEXT,
              success INTEGER NOT NULL,
              gas_used INTEGER,
              effective_gas_price_gwei REAL,
              priority_fee_gwei REAL,
              value_native TEXT
            );
            CREATE INDEX IF NOT EXISTS txs_block ON txs(block_number);

            CREATE TABLE IF NOT EXISTS transfers(
              block_number INTEGER NOT NULL,
              tx_index INTEGER NOT NULL,
              log_index INTEGER NOT NULL,
              token TEXT NOT NULL,
              \"from\" TEXT NOT NULL,
              \"to\" TEXT NOT NULL,
              amount TEXT NOT NULL,
              is_native INTEGER NOT NULL DEFAULT 0,
              PRIMARY KEY(block_number, tx_index, log_index)
            );
            CREATE INDEX IF NOT EXISTS transfers_token ON transfers(token, block_number);

            CREATE TABLE IF NOT EXISTS swaps(
              block_number INTEGER NOT NULL,
              tx_index INTEGER NOT NULL,
              log_index INTEGER NOT NULL,
              pool TEXT NOT NULL,
              dex TEXT,
              amm TEXT,
              token_in TEXT NOT NULL,
              token_out TEXT NOT NULL,
              amount_in TEXT NOT NULL,
              amount_out TEXT NOT NULL,
              sender TEXT,
              PRIMARY KEY(block_number, tx_index, log_index)
            );
            CREATE INDEX IF NOT EXISTS swaps_pool ON swaps(pool, block_number);

            CREATE TABLE IF NOT EXISTS mev_ops(
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              block_number INTEGER NOT NULL,
              tx_index INTEGER,
              tx_hash TEXT NOT NULL,
              ts INTEGER NOT NULL,
              kind TEXT NOT NULL CHECK(kind IN
                ('arb_atomic','sandwich','frontrun','backrun','liquidation','jit','jit_arb','unknown')),
              eoa TEXT NOT NULL,
              contract TEXT,
              confidence TEXT NOT NULL CHECK(confidence IN ('exact','inferred')),
              canonical_id TEXT,
              profit_token TEXT,
              profit_amount TEXT,
              profit_usd REAL,
              gas_cost_usd REAL,
              flashloan_fee_usd REAL,
              net_profit_usd REAL,
              route_json TEXT,
              victim_hashes TEXT,
              details_json TEXT,
              detector TEXT NOT NULL,
              created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS mev_ops_block ON mev_ops(block_number);
            CREATE INDEX IF NOT EXISTS mev_ops_sender ON mev_ops(eoa, block_number);
            CREATE INDEX IF NOT EXISTS mev_ops_kind_ts ON mev_ops(kind, ts);
            CREATE INDEX IF NOT EXISTS mev_ops_canonical ON mev_ops(canonical_id);

            CREATE TABLE IF NOT EXISTS labels(
              address TEXT PRIMARY KEY,
              kind TEXT,
              name TEXT,
              entity TEXT,
              evidence TEXT,
              first_seen_block INTEGER,
              source TEXT
            );

            CREATE TABLE IF NOT EXISTS prices(
              hour INTEGER NOT NULL,
              token TEXT NOT NULL,
              usd REAL NOT NULL,
              source TEXT,
              PRIMARY KEY(hour, token)
            );

            CREATE TABLE IF NOT EXISTS sync_state(
              chain_id INTEGER PRIMARY KEY,
              head INTEGER,
              indexed_to INTEGER,
              last_indexed_at INTEGER
            );

            CREATE TABLE IF NOT EXISTS blocks_classified(
              block INTEGER PRIMARY KEY,
              classified_at INTEGER NOT NULL,
              event_count INTEGER NOT NULL
            );

            -- Phase 5a-0: JIT open Mint positions, persisted so cross-block JIT
            -- detection survives restarts. Feeder table for Phase 1.5; pruned by
            -- opened_block block-window cap.
            CREATE TABLE IF NOT EXISTS jit_open_positions(
              pool TEXT NOT NULL,
              owner TEXT NOT NULL,
              tick_lower INTEGER NOT NULL,
              tick_upper INTEGER NOT NULL,
              opened_block INTEGER NOT NULL,
              liquidity TEXT NOT NULL,
              PRIMARY KEY(pool, owner, tick_lower, tick_upper)
            );
            CREATE INDEX IF NOT EXISTS jit_open_positions_prune
              ON jit_open_positions(opened_block);

            CREATE TABLE IF NOT EXISTS opportunities(
              run_id TEXT,
              chain TEXT,
              block_number INTEGER NOT NULL,
              tx_index INTEGER,
              strategy TEXT NOT NULL,
              pool_a TEXT,
              pool_b TEXT,
              token_in TEXT,
              token_out TEXT,
              input_amount TEXT,
              expected_profit TEXT,
              gas_cost_wei TEXT,
              path TEXT,
              timestamp INTEGER,
              mempool_only INTEGER,
              confidence TEXT,
              sender TEXT,
              tx_hash TEXT,
              detection_path TEXT,
              canonical_id TEXT
            );
            CREATE INDEX IF NOT EXISTS opportunities_block
              ON opportunities(chain, block_number);
            CREATE INDEX IF NOT EXISTS opportunities_canonical
              ON opportunities(canonical_id);

            CREATE TABLE IF NOT EXISTS rejected_candidates(
              run_id TEXT,
              chain TEXT,
              block_number INTEGER NOT NULL,
              tx_index INTEGER,
              strategy TEXT NOT NULL,
              pool_a TEXT,
              pool_b TEXT,
              path TEXT,
              token_in TEXT,
              token_out TEXT,
              input_amount TEXT,
              expected_profit TEXT,
              expected_profit_usd REAL,
              gas_cost_wei TEXT,
              reject_reason TEXT NOT NULL,
              detail TEXT,
              created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS rejected_block
              ON rejected_candidates(chain, block_number);
            CREATE INDEX IF NOT EXISTS rejected_reason
              ON rejected_candidates(reject_reason);
            ",
        )?;
        // Phase 2.2 runtime migration for databases created before the
        // flash-loan fee column existed.
        Self::ensure_column(&self.conn, "mev_ops", "flashloan_fee_usd", "REAL")?;
        Ok(())
    }

    /// Add `column` to `table` when missing (idempotent, for existing DBs).
    fn ensure_column(
        conn: &rusqlite::Connection,
        table: &str,
        column: &str,
        ty: &str,
    ) -> anyhow::Result<()> {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let mut rows = stmt.query([])?;
        let mut exists = false;
        while let Some(row) = rows.next()? {
            let name: String = row.get(1)?;
            if name == column {
                exists = true;
                break;
            }
        }
        drop(rows);
        drop(stmt);
        if !exists {
            conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {ty}"))?;
        }
        Ok(())
    }

    // ── sync_state / checkpoints ────────────────────────────────────────

    /// Read `indexed_to` for a chain (0 when absent).
    pub fn get_indexed_to(&self, chain_id: u64) -> anyhow::Result<u64> {
        let v: Option<u64> = self
            .conn
            .query_row(
                "SELECT indexed_to FROM sync_state WHERE chain_id = ?1",
                [chain_id as i64],
                |r| r.get(0),
            )
            .map(Some)
            .unwrap_or(None);
        Ok(v.unwrap_or(0))
    }

    /// Upsert sync checkpoint after indexing through `indexed_to`.
    pub fn set_sync_state(&self, chain_id: u64, head: u64, indexed_to: u64) -> anyhow::Result<()> {
        let now = crate::utils::epoch_secs() as i64;
        self.conn.execute(
            "INSERT INTO sync_state(chain_id, head, indexed_to, last_indexed_at)
             VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(chain_id) DO UPDATE SET
               head = excluded.head,
               indexed_to = excluded.indexed_to,
               last_indexed_at = excluded.last_indexed_at",
            rusqlite::params![chain_id as i64, head as i64, indexed_to as i64, now],
        )?;
        Ok(())
    }

    /// True when the block was already classified (idempotent skip).
    pub fn block_classified(&self, block: u64) -> anyhow::Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM blocks_classified WHERE block = ?1",
            [block as i64],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// Record a classified-block checkpoint.
    pub fn mark_block_classified(&self, block: u64, event_count: usize) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO blocks_classified(block, classified_at, event_count)
             VALUES(?1, ?2, ?3)",
            rusqlite::params![block, crate::utils::epoch_secs() as i64, event_count as i64],
        )?;
        Ok(())
    }

    /// Blocks in `[from, to]` not yet classified (gap resume).
    pub fn unclassified_blocks(&self, from: u64, to: u64) -> anyhow::Result<Vec<u64>> {
        let mut stmt = self.conn.prepare(
            "WITH RECURSIVE seq(b) AS (
               SELECT ?1 UNION ALL SELECT b+1 FROM seq WHERE b < ?2
             )
             SELECT b FROM seq
             WHERE b NOT IN (SELECT block FROM blocks_classified
                             WHERE block BETWEEN ?1 AND ?2)",
        )?;
        let rows = stmt.query_map(rusqlite::params![from as i64, to as i64], |r| {
            r.get::<_, i64>(0)
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row? as u64);
        }
        Ok(out)
    }

    /// Remove classified blocks >= fork block (reorg unwind).
    pub fn unwind_from(&self, fork_block: u64) -> anyhow::Result<()> {
        self.conn.execute(
            "DELETE FROM blocks_classified WHERE block >= ?1",
            [fork_block as i64],
        )?;
        self.conn.execute(
            "DELETE FROM mev_ops WHERE block_number >= ?1",
            [fork_block as i64],
        )?;
        self.conn.execute(
            "DELETE FROM transfers WHERE block_number >= ?1",
            [fork_block as i64],
        )?;
        self.conn.execute(
            "DELETE FROM swaps WHERE block_number >= ?1",
            [fork_block as i64],
        )?;
        self.conn.execute(
            "DELETE FROM txs WHERE block_number >= ?1",
            [fork_block as i64],
        )?;
        self.conn.execute(
            "DELETE FROM blocks WHERE block_number >= ?1",
            [fork_block as i64],
        )?;
        self.conn.execute(
            "DELETE FROM jit_open_positions WHERE opened_block >= ?1",
            [fork_block as i64],
        )?;
        Ok(())
    }

    /// Stored block hash for reorg detection.
    pub fn block_hash(&self, block: u64) -> anyhow::Result<Option<String>> {
        let v = self
            .conn
            .query_row(
                "SELECT block_hash FROM blocks WHERE block_number = ?1",
                [block as i64],
                |r| r.get::<_, String>(0),
            )
            .map(Some)
            .unwrap_or(None);
        Ok(v)
    }

    // ── facts inserts ───────────────────────────────────────────────────

    /// Insert block header + txs + swaps + transfers + mev events in one
    /// transaction. Idempotent via OR IGNORE on natural keys.
    pub fn insert_block_facts(&self, facts: BlockFactsInput<'_>) -> anyhow::Result<usize> {
        let BlockFactsInput {
            block_number,
            block_hash,
            ts,
            base_fee_gwei,
            tx_count,
            txs,
            swaps,
            transfers,
            events,
            native_price_usd,
            token_prices,
        } = facts;
        let now = crate::utils::epoch_secs() as i64;
        let conn = &self.conn;
        conn.execute("BEGIN IMMEDIATE", [])?;
        let result = (|| -> anyhow::Result<usize> {
            conn.execute(
                "INSERT OR IGNORE INTO blocks
                   (block_number, block_hash, ts, producer, base_fee_gwei, tx_count, indexed_at)
                 VALUES(?1, ?2, ?3, NULL, ?4, ?5, ?6)",
                rusqlite::params![
                    block_number as i64,
                    format!("{:#x}", block_hash),
                    ts as i64,
                    base_fee_gwei,
                    tx_count as i64,
                    now
                ],
            )?;

            for tx in txs {
                conn.execute(
                    "INSERT OR IGNORE INTO txs
                       (hash, block_number, tx_index, \"from\", \"to\", success,
                        gas_used, effective_gas_price_gwei, priority_fee_gwei, value_native)
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    rusqlite::params![
                        format!("{:#x}", tx.hash),
                        block_number as i64,
                        tx.tx_index as i64,
                        format!("{:#x}", tx.from),
                        tx.to.map(|a| format!("{:#x}", a)),
                        tx.success as i64,
                        tx.gas_used as i64,
                        tx.effective_gas_price_gwei,
                        tx.priority_fee_gwei,
                        tx.value.to_string(),
                    ],
                )?;
            }

            for s in swaps {
                conn.execute(
                    "INSERT OR IGNORE INTO swaps
                       (block_number, tx_index, log_index, pool, dex, amm,
                        token_in, token_out, amount_in, amount_out, sender)
                     VALUES(?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7, ?8, ?9, NULL)",
                    rusqlite::params![
                        block_number as i64,
                        s.tx_index as i64,
                        s.log_index as i64,
                        format!("{:#x}", s.pool),
                        s.amm.as_str(),
                        format!("{:#x}", s.token_in),
                        format!("{:#x}", s.token_out),
                        s.amount_in.to_string(),
                        s.amount_out.to_string(),
                    ],
                )?;
            }

            for t in transfers {
                conn.execute(
                    "INSERT OR IGNORE INTO transfers
                       (block_number, tx_index, log_index, token, \"from\", \"to\",
                        amount, is_native)
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)",
                    rusqlite::params![
                        block_number as i64,
                        t.tx_index as i64,
                        t.log_index as i64,
                        format!("{:#x}", t.token),
                        format!("{:#x}", t.from),
                        format!("{:#x}", t.to),
                        t.amount.to_string(),
                    ],
                )?;
            }

            let mut inserted = 0usize;
            for ev in events {
                // Liquidation P&L (Phase 1.3): profit ≈ collateral_usd −
                // debt_usd, valued at persist with hourly prices. Never
                // subtract raw amounts when tokens differ.
                let mut confidence = ev.confidence.as_str();
                let mut details_json = ev.details.to_string();
                let profit_usd = if ev.kind == MevKind::Liquidation {
                    let (usd, conf, details) = liquidation_pnl(ev, token_prices);
                    confidence = conf;
                    details_json = details;
                    usd
                } else if !ev.profit_tokens.is_empty() {
                    // Phase 2.3: USD-sum across every positive residual
                    // (flash-netting already cleaned the ledger in 2.2) with
                    // Phase 2.4 realized-rate fallback for unpriced tokens.
                    let total = ev.profit_tokens.iter().fold(0.0f64, |acc, (tok, amt)| {
                        acc + crate::explorer::pricing::amount_usd_realized(
                            *tok,
                            *amt,
                            token_prices,
                            &ev.details,
                        )
                        .unwrap_or(0.0)
                    });
                    if total > 0.0 {
                        // Surface the residual list for forensic display.
                        details_json = merge_details_json(
                            &details_json,
                            [(
                                "profit_tokens",
                                serde_json::json!(ev
                                    .profit_tokens
                                    .iter()
                                    .map(|(tok, amt)| serde_json::json!({
                                        "token": format!("{tok:#x}"),
                                        "amount": amt.to_string(),
                                    }))
                                    .collect::<Vec<_>>()),
                            )],
                        );
                        Some(total)
                    } else {
                        None
                    }
                } else {
                    match (ev.profit_token, ev.profit_amount) {
                        (Some(tok), Some(amt)) => crate::explorer::pricing::amount_usd_realized(
                            tok,
                            amt,
                            token_prices,
                            &ev.details,
                        ),
                        _ => None,
                    }
                };
                // Phase 2.4: FOT / rebase profit tokens are flagged approximate
                // (recorded amounts are distorted by the token mechanics).
                if let Some(reason) = ev
                    .profit_tokens
                    .iter()
                    .map(|(tok, _)| *tok)
                    .chain(ev.profit_token)
                    .find_map(|t| crate::explorer::pricing::approximate_token(&t))
                {
                    details_json = merge_details_json(
                        &details_json,
                        [
                            ("usd_approximate", serde_json::json!(true)),
                            ("approximate_reason", serde_json::json!(reason)),
                        ],
                    );
                }
                // Gas in USD via native price (gas wei → native → USD).
                let gas_usd = native_price_usd
                    .map(|np| crate::explorer::pricing::wei_to_usd(ev.gas_cost_wei, np));
                // Flash-loan fee in USD via the borrowed token's price (2.2).
                let flashloan_fee_usd = match (ev.flashloan_fee_token, ev.flashloan_fee_wei) {
                    (Some(tok), Some(amt)) => token_prices
                        .get(&tok)
                        .map(|p| crate::explorer::pricing::token_amount_to_usd(amt, p)),
                    _ => None,
                };
                let net = match (profit_usd, gas_usd) {
                    (Some(g), Some(gg)) => Some(g - gg - flashloan_fee_usd.unwrap_or(0.0)),
                    _ => None,
                };
                // Sandwich profitability gate (Phase 1.4): a sandwich whose
                // extractable profit does not cover combined front+back gas
                // (plus any flash-loan fee) is not a realized-MEV op. Drop it
                // rather than persist a provably lossy record. Unowned-net
                // (missing price) events are kept — the gate never guesses.
                if ev.kind == MevKind::Sandwich {
                    if let Some(n) = net {
                        if n <= 0.0 {
                            continue;
                        }
                    }
                }
                let canonical = crate::explorer::explorer_canonical_id(ev);
                conn.execute(
                    "INSERT INTO mev_ops
                       (block_number, tx_index, tx_hash, ts, kind, eoa, contract,
                        confidence, canonical_id, profit_token, profit_amount,
                        profit_usd, gas_cost_usd, flashloan_fee_usd, net_profit_usd,
                        route_json, victim_hashes, details_json, detector, created_at)
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                            ?14, ?15, ?16, ?17, ?18, 'explorer', ?19)",
                    rusqlite::params![
                        ev.block as i64,
                        ev.tx_index as i64,
                        format!("{:#x}", ev.tx_hash),
                        ev.ts as i64,
                        ev.kind.as_str(),
                        format!("{:#x}", ev.searcher),
                        ev.contract.map(|a| format!("{:#x}", a)),
                        confidence,
                        canonical,
                        ev.profit_token.map(|a| format!("{:#x}", a)),
                        ev.profit_amount.map(|a| a.to_string()),
                        profit_usd,
                        gas_usd,
                        flashloan_fee_usd,
                        net,
                        ev.details.get("route").map(|r| r.to_string()),
                        serde_json::to_string(
                            &ev.victim_hashes
                                .iter()
                                .map(|h| format!("{:#x}", h))
                                .collect::<Vec<_>>()
                        )
                        .ok(),
                        Some(details_json),
                        now,
                    ],
                )?;
                inserted += 1;
            }

            conn.execute(
                "INSERT OR REPLACE INTO blocks_classified(block, classified_at, event_count)
                 VALUES(?1, ?2, ?3)",
                rusqlite::params![block_number as i64, now, inserted as i64],
            )?;
            Ok(inserted)
        })();

        match result {
            Ok(n) => {
                conn.execute("COMMIT", [])?;
                Ok(n)
            }
            Err(e) => {
                conn.execute("ROLLBACK", [])?;
                Err(e)
            }
        }
    }

    /// Purge `transfers` older than `keep_blocks` behind head (retention).
    pub fn prune_transfers(&self, before_block: u64) -> anyhow::Result<usize> {
        let n = self.conn.execute(
            "DELETE FROM transfers WHERE block_number < ?1",
            [before_block as i64],
        )?;
        Ok(n)
    }

    // ── results layer ──────────────────────────────────────────────────

    /// Insert one opportunity row from a `ResultsFile` entry.
    pub fn insert_opportunity(&self, row: OpportunityInput<'_>) -> anyhow::Result<()> {
        let OpportunityInput {
            run_id,
            chain,
            block_number,
            tx_index,
            strategy,
            pool_a,
            pool_b,
            token_in,
            token_out,
            input_amount,
            expected_profit,
            gas_cost_wei,
            path,
            timestamp,
            mempool_only,
            confidence,
            sender,
            tx_hash,
            detection_path,
            canonical_id,
        } = row;
        self.conn.execute(
            "INSERT INTO opportunities
               (run_id, chain, block_number, tx_index, strategy, pool_a, pool_b,
                token_in, token_out, input_amount, expected_profit, gas_cost_wei,
                path, timestamp, mempool_only, confidence, sender, tx_hash,
                detection_path, canonical_id)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)",
            rusqlite::params![
                run_id,
                chain,
                block_number as i64,
                tx_index.map(|v| v as i64),
                strategy,
                pool_a.map(|a| format!("{:#x}", a)),
                pool_b.map(|a| format!("{:#x}", a)),
                token_in.map(|a| format!("{:#x}", a)),
                token_out.map(|a| format!("{:#x}", a)),
                input_amount.map(|v| v.to_string()),
                expected_profit.map(|v| v.to_string()),
                gas_cost_wei.map(|v| v.to_string()),
                path,
                timestamp.map(|v| v as i64),
                mempool_only as i64,
                confidence,
                sender.map(|a| format!("{:#x}", a)),
                tx_hash.map(|h| format!("{:#x}", h)),
                detection_path,
                canonical_id,
            ],
        )?;
        Ok(())
    }

    // ── queries ─────────────────────────────────────────────────────────

    /// Live feed: most recent ops (optionally filtered by kind).
    pub fn feed_tail(&self, limit: usize, kinds: &[MevKind]) -> anyhow::Result<Vec<FeedRow>> {
        let order = if kinds.is_empty() {
            String::new()
        } else {
            let list: Vec<String> = kinds.iter().map(|k| format!("'{}'", k.as_str())).collect();
            format!("WHERE kind IN ({})", list.join(","))
        };
        let sql = format!(
            "SELECT ts, block_number, kind, profit_token, profit_amount, profit_usd, gas_cost_usd,
                    net_profit_usd, eoa, tx_hash, route_json
             FROM mev_ops {order}
             ORDER BY block_number DESC, tx_index DESC, id DESC LIMIT {limit}"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_feed_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        let native = latest_native_price(&self.conn)?;
        for row in &mut out {
            row.native_price_usd = native;
        }
        Ok(out)
    }

    /// Period stats grouped by kind (`stats`).
    pub fn stats_by_kind(&self, since_ts: u64) -> anyhow::Result<Vec<StatsRow>> {
        self.stats_by_kind_filtered(since_ts, None)
    }

    /// `stats_by_kind` restricted to one `kind` value (`stats --kind`).
    pub fn stats_by_kind_filtered(
        &self,
        since_ts: u64,
        kind: Option<&str>,
    ) -> anyhow::Result<Vec<StatsRow>> {
        stats_grouped(&self.conn, "kind", since_ts, kind)
    }

    /// Sender leaderboard (`top --by sender`).
    pub fn top_senders(&self, since_ts: u64, limit: usize) -> anyhow::Result<Vec<StatsRow>> {
        self.top_senders_filtered(since_ts, limit, None)
    }

    /// `top_senders` restricted to one `kind` value (`stats --kind`).
    pub fn top_senders_filtered(
        &self,
        since_ts: u64,
        limit: usize,
        kind: Option<&str>,
    ) -> anyhow::Result<Vec<StatsRow>> {
        top_grouped(&self.conn, "eoa", since_ts, limit, kind)
    }

    /// Most profitable tokens (`top --by token`).
    pub fn top_tokens(&self, since_ts: u64, limit: usize) -> anyhow::Result<Vec<StatsRow>> {
        self.top_tokens_filtered(since_ts, limit, None)
    }

    /// `top_tokens` restricted to one `kind` value.
    pub fn top_tokens_filtered(
        &self,
        since_ts: u64,
        limit: usize,
        kind: Option<&str>,
    ) -> anyhow::Result<Vec<StatsRow>> {
        top_grouped(&self.conn, "profit_token", since_ts, limit, kind)
    }

    /// Busiest pools (`top --by pool`) — pool list comes from route_json.
    pub fn top_pools(&self, since_ts: u64, limit: usize) -> anyhow::Result<Vec<StatsRow>> {
        self.top_pools_filtered(since_ts, limit, None)
    }

    /// `top_pools` restricted to one `kind` value (`stats --kind`).
    pub fn top_pools_filtered(
        &self,
        since_ts: u64,
        limit: usize,
        kind: Option<&str>,
    ) -> anyhow::Result<Vec<StatsRow>> {
        // Pools live inside route_json; extract with json_each.
        let kind_clause = kind
            .map(|k| format!("AND m.kind = '{k}'"))
            .unwrap_or_default();
        let sql = format!(
            "SELECT j.value->>'$.pool' AS label,
                    COUNT(DISTINCT m.id) AS ops,
                    COALESCE(SUM(m.profit_usd), 0) AS gross_usd,
                    COALESCE(SUM(m.net_profit_usd), 0) AS net_usd
             FROM mev_ops m, json_each(COALESCE(m.route_json, '[]')) j
             WHERE m.ts >= {since_ts} {kind_clause} AND j.value->>'$.pool' IS NOT NULL
             GROUP BY label
             ORDER BY gross_usd DESC
             LIMIT {limit}"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_stats_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Daily breakdown (`stats --window day`).
    pub fn stats_daily(&self, since_ts: u64) -> anyhow::Result<Vec<StatsRow>> {
        self.stats_daily_filtered(since_ts, None)
    }

    /// `stats_daily` restricted to one `kind` value (`stats --kind`).
    pub fn stats_daily_filtered(
        &self,
        since_ts: u64,
        kind: Option<&str>,
    ) -> anyhow::Result<Vec<StatsRow>> {
        let kind_clause = kind
            .map(|k| format!("AND kind = '{k}'"))
            .unwrap_or_default();
        let sql = format!(
            "SELECT date(ts, 'unixepoch') AS label,
                    COUNT(*) AS ops,
                    COALESCE(SUM(profit_usd), 0) AS gross_usd,
                    COALESCE(SUM(net_profit_usd), 0) AS net_usd
             FROM mev_ops
             WHERE ts >= {since_ts} {kind_clause}
             GROUP BY label
             ORDER BY label DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_stats_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Full op detail for `show <TX>`.
    pub fn ops_for_tx(&self, tx_hash: &str) -> anyhow::Result<Vec<MevOpRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, block_number, tx_index, tx_hash, ts, kind, eoa, contract,
                    confidence, canonical_id, profit_token, profit_amount, profit_usd,
                    gas_cost_usd, net_profit_usd, route_json, victim_hashes,
                    details_json, detector, created_at
             FROM mev_ops WHERE tx_hash = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([tx_hash], map_mev_op_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Realized ops in a block window (`validate` ground truth).
    pub fn ops_in_range(
        &self,
        from_block: u64,
        to_block: u64,
        kinds: &[MevKind],
    ) -> anyhow::Result<Vec<MevOpRow>> {
        let kind_filter = if kinds.is_empty() {
            String::new()
        } else {
            let list: Vec<String> = kinds.iter().map(|k| format!("'{}'", k.as_str())).collect();
            format!("AND kind IN ({})", list.join(","))
        };
        let sql = format!(
            "SELECT id, block_number, tx_index, tx_hash, ts, kind, eoa, contract,
                    confidence, canonical_id, profit_token, profit_amount, profit_usd,
                    gas_cost_usd, net_profit_usd, route_json, victim_hashes,
                    details_json, detector, created_at
             FROM mev_ops
             WHERE block_number BETWEEN {from_block} AND {to_block} {kind_filter}
             ORDER BY block_number, tx_index"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_mev_op_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Opportunities in a block window (`validate` scanner side).
    pub fn opportunities_in_range(
        &self,
        chain: &str,
        from_block: u64,
        to_block: u64,
    ) -> anyhow::Result<Vec<OpportunityRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT run_id, block_number, tx_index, strategy, pool_a, pool_b,
                    token_in, token_out, expected_profit, mempool_only,
                    detection_path, canonical_id, tx_hash
             FROM opportunities
             WHERE chain = ?1 AND block_number BETWEEN ?2 AND ?3
             ORDER BY block_number",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![chain, from_block as i64, to_block as i64],
            |r| {
                Ok(OpportunityRow {
                    run_id: r.get(0)?,
                    block_number: r.get::<_, i64>(1)? as u64,
                    tx_index: r.get::<_, Option<i64>>(2)?.map(|v| v as u64),
                    strategy: r.get(3)?,
                    pool_a: r.get(4)?,
                    pool_b: r.get(5)?,
                    token_in: r.get(6)?,
                    token_out: r.get(7)?,
                    expected_profit: r.get(8)?,
                    mempool_only: r.get::<_, Option<i64>>(9)?.unwrap_or(0) != 0,
                    detection_path: r.get(10)?,
                    canonical_id: r.get(11)?,
                    tx_hash: r.get(12)?,
                })
            },
        )?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Reconstruct the `MevOpportunity` list recorded for a run id
    /// (SQLite-backed `report`). Rows whose strategy string cannot be parsed
    /// are skipped with a warning. Fields not persisted in the `opportunities`
    /// table (raw/slippage profit variants, JIT tick bounds, sandwich
    /// victim/backrun indices) come back as defaults.
    pub fn opportunities_by_run(&self, run_id: &str) -> anyhow::Result<Vec<MevOpportunity>> {
        let mut stmt = self.conn.prepare(
            "SELECT block_number, tx_index, strategy, pool_a, pool_b,
                    token_in, token_out, input_amount, expected_profit, gas_cost_wei,
                    path, timestamp, mempool_only, confidence, sender, tx_hash,
                    detection_path, canonical_id
             FROM opportunities
             WHERE run_id = ?1
             ORDER BY block_number, tx_index",
        )?;
        let rows = stmt.query_map(rusqlite::params![run_id], |r| {
            Ok((
                r.get::<_, i64>(0)? as u64,
                r.get::<_, Option<i64>>(1)?.map(|v| v as usize),
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, Option<String>>(10)?,
                r.get::<_, Option<i64>>(11)?.map(|v| v as u64),
                r.get::<_, Option<i64>>(12)?.unwrap_or(0) != 0,
                r.get::<_, Option<String>>(13)?,
                r.get::<_, Option<String>>(14)?,
                r.get::<_, Option<String>>(15)?,
                r.get::<_, Option<String>>(16)?,
                r.get::<_, Option<String>>(17)?,
            ))
        })?;
        let parse_addr = |s: &Option<String>| -> anyhow::Result<Address> {
            match s {
                Some(v) => v.parse::<Address>().map_err(|e| {
                    anyhow::anyhow!(
                        "opportunities table holds unparseable address '{v}' (schema drift?): {e}"
                    )
                }),
                None => Ok(Address::ZERO),
            }
        };
        let mut out = Vec::new();
        for row in rows {
            let (
                block_number,
                tx_index,
                strategy_str,
                pool_a,
                pool_b,
                token_in,
                token_out,
                input_amount,
                expected_profit,
                gas_cost_wei,
                path,
                timestamp,
                mempool_only,
                confidence,
                sender,
                tx_hash,
                detection_path,
                canonical_id,
            ) = row?;
            let strategy = match strategy_str.parse::<Strategy>() {
                Ok(s) => s,
                Err(_) => {
                    tracing::warn!(
                        "opportunities_by_run: skipping row with unknown strategy '{strategy_str}'"
                    );
                    continue;
                }
            };
            out.push(MevOpportunity {
                canonical_id,
                block_number,
                tx_index: tx_index.unwrap_or(0),
                strategy,
                pool_a: parse_addr(&pool_a)?,
                pool_b: parse_addr(&pool_b)?,
                token_in: parse_addr(&token_in)?,
                token_out: parse_addr(&token_out)?,
                input_amount: match &input_amount {
                    Some(v) => v.parse::<U256>().map_err(|e| {
                        anyhow::anyhow!(
                            "opportunities table holds unparseable input_amount '{v}' (schema drift?): {e}"
                        )
                    })?,
                    None => U256::ZERO,
                },
                expected_profit: match &expected_profit {
                    Some(v) => v.parse::<U256>().map_err(|e| {
                        anyhow::anyhow!(
                            "opportunities table holds unparseable expected_profit '{v}' (schema drift?): {e}"
                        )
                    })?,
                    None => U256::ZERO,
                },
                raw_profit: None,
                profit_slippage_p1: None,
                profit_slippage_m1: None,
                profit_slippage_p2: None,
                profit_slippage_m2: None,
                gas_cost_wei: match &gas_cost_wei {
                    Some(v) => v.parse::<u128>().map_err(|e| {
                        anyhow::anyhow!(
                            "opportunities table holds unparseable gas_cost_wei '{v}' (schema drift?): {e}"
                        )
                    })?,
                    None => 0,
                },
                timestamp: timestamp.unwrap_or(0),
                path: match &path {
                    Some(p) => {
                        if p.is_empty() {
                            None
                        } else {
                            let parsed: anyhow::Result<Vec<Address>> =
                                p.split(',').map(|a| {
                                    a.parse::<Address>().map_err(|e| {
                                        anyhow::anyhow!(
                                            "opportunities table holds unparseable path address '{a}' (schema drift?): {e}"
                                        )
                                    })
                                }).collect();
                            Some(parsed?).filter(|v| !v.is_empty())
                        }
                    }
                    None => None,
                },
                tick_lower: None,
                tick_upper: None,
                liquidity_amount: None,
                victim_tx_index: None,
                backrun_tx_index: None,
                mempool_only,
                confidence: confidence.as_deref().and_then(|v| v.parse().ok()),
                sender: match &sender {
                    Some(v) => Some(v.parse::<Address>().map_err(|e| {
                        anyhow::anyhow!(
                            "opportunities table holds unparseable sender '{v}' (schema drift?): {e}"
                        )
                    })?),
                    None => None,
                },
                tx_hash: match &tx_hash {
                    Some(v) => Some(v.parse().map_err(|e| {
                        anyhow::anyhow!(
                            "opportunities table holds unparseable tx_hash '{v}' (schema drift?): {e}"
                        )
                    })?),
                    None => None,
                },
                detection_path,
            });
        }
        Ok(out)
    }

    /// Blocks with realized ops of a kind (block-level recall, T3).
    pub fn blocks_with_kind(
        &self,
        from_block: u64,
        to_block: u64,
        kind: MevKind,
    ) -> anyhow::Result<std::collections::HashSet<u64>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT block_number FROM mev_ops
             WHERE kind = ?1 AND block_number BETWEEN ?2 AND ?3",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![kind.as_str(), from_block as i64, to_block as i64],
            |r| r.get::<_, i64>(0),
        )?;
        let mut out = std::collections::HashSet::new();
        for r in rows {
            out.insert(r? as u64);
        }
        Ok(out)
    }

    /// Distinct run_ids in the `opportunities` table with per-run
    /// aggregates (count, first/last timestamps), newest first. Includes
    /// `live_*` runs that have no manifest — powers the run-id dropdowns.
    pub fn opportunity_run_summaries(&self) -> anyhow::Result<Vec<OpportunityRunSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT run_id, COUNT(*), MIN(timestamp), MAX(timestamp)
             FROM opportunities
             WHERE run_id IS NOT NULL
             GROUP BY run_id
             ORDER BY MAX(timestamp) DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(OpportunityRunSummary {
                run_id: r.get(0)?,
                count: r.get::<_, i64>(1)? as u64,
                first_ts: r.get::<_, Option<i64>>(2)?.map(|v| v as u64),
                last_ts: r.get::<_, Option<i64>>(3)?.map(|v| v as u64),
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Blocks where the scanner recorded opportunities (M7 coverage check).
    pub fn opportunity_blocks(
        &self,
        chain: &str,
        from_block: u64,
        to_block: u64,
    ) -> anyhow::Result<std::collections::HashSet<u64>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT block_number FROM opportunities
             WHERE chain = ?1 AND block_number BETWEEN ?2 AND ?3",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![chain, from_block as i64, to_block as i64],
            |r| r.get::<_, i64>(0),
        )?;
        let mut out = std::collections::HashSet::new();
        for r in rows {
            out.insert(r? as u64);
        }
        Ok(out)
    }

    /// Price cache lookup: USD for a token at an hour bucket (exact hour, or
    /// the most recent cached hour within 24h).
    pub fn price_at(&self, token: Address, hour: u64) -> anyhow::Result<Option<(f64, String)>> {
        let tok = format!("{:#x}", token);
        let exact: Option<(f64, String)> = self
            .conn
            .query_row(
                "SELECT usd, source FROM prices WHERE token = ?1 AND hour = ?2",
                rusqlite::params![tok, hour as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map(Some)
            .unwrap_or(None);
        if exact.is_some() {
            return Ok(exact);
        }
        let v = self
            .conn
            .query_row(
                "SELECT usd, source FROM prices
                 WHERE token = ?1 AND hour <= ?2 AND hour > ?2 - 24
                 ORDER BY hour DESC LIMIT 1",
                rusqlite::params![tok, hour as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map(Some)
            .unwrap_or(None);
        Ok(v)
    }

    /// Cache a price observation.
    pub fn put_price(
        &self,
        token: Address,
        hour: u64,
        usd: f64,
        source: &str,
    ) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO prices(hour, token, usd, source) VALUES(?1, ?2, ?3, ?4)",
            rusqlite::params![hour as i64, format!("{:#x}", token), usd, source],
        )?;
        Ok(())
    }

    /// Persist (or refresh) open JIT Mint positions (Phase 1.5).
    pub fn record_jit_open(&self, positions: &[OpenPosition]) -> anyhow::Result<()> {
        for p in positions {
            self.conn.execute(
                "INSERT OR REPLACE INTO jit_open_positions
                   (pool, owner, tick_lower, tick_upper, opened_block, liquidity)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    format!("{:#x}", p.pool),
                    format!("{:#x}", p.owner),
                    p.tick_lower as i64,
                    p.tick_upper as i64,
                    p.opened_block as i64,
                    p.liquidity.to_string(),
                ],
            )?;
        }
        Ok(())
    }

    /// Delete an open position once its exact-liquidity Burn is observed.
    pub fn close_jit_position(
        &self,
        pool: Address,
        owner: Address,
        tick_lower: i32,
        tick_upper: i32,
    ) -> anyhow::Result<()> {
        self.conn.execute(
            "DELETE FROM jit_open_positions
              WHERE pool = ?1 AND owner = ?2 AND tick_lower = ?3 AND tick_upper = ?4",
            rusqlite::params![
                format!("{:#x}", pool),
                format!("{:#x}", owner),
                tick_lower as i64,
                tick_upper as i64,
            ],
        )?;
        Ok(())
    }

    /// Open positions still inside the block window (`opened_block >= min_block`).
    pub fn open_positions(&self, min_block: u64) -> anyhow::Result<Vec<OpenPosition>> {
        let mut stmt = self.conn.prepare(
            "SELECT pool, owner, tick_lower, tick_upper, opened_block, liquidity
               FROM jit_open_positions WHERE opened_block >= ?1",
        )?;
        let rows = stmt.query_map([min_block as i64], |r| {
            let pool: String = r.get(0)?;
            let owner: String = r.get(1)?;
            let liq: String = r.get(5)?;
            Ok((
                pool,
                owner,
                r.get::<_, i64>(2)? as i32,
                r.get::<_, i64>(3)? as i32,
                r.get::<_, i64>(4)? as u64,
                liq,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (pool, owner, tick_lower, tick_upper, opened_block, liq) = row?;
            let (Ok(pool), Ok(owner)) = (pool.parse::<Address>(), owner.parse::<Address>()) else {
                continue;
            };
            out.push(OpenPosition {
                pool,
                owner,
                tick_lower,
                tick_upper,
                opened_block,
                liquidity: liq.parse::<u128>().unwrap_or(0),
            });
        }
        Ok(out)
    }

    /// Prune positions older than the block window. Returns rows removed.
    pub fn prune_jit_positions(&self, before_block: u64) -> anyhow::Result<usize> {
        let n = self.conn.execute(
            "DELETE FROM jit_open_positions WHERE opened_block < ?1",
            [before_block as i64],
        )?;
        Ok(n)
    }

    /// Count of ops by kind since a timestamp (quick overview).
    pub fn op_count_since(&self, since_ts: u64) -> anyhow::Result<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM mev_ops WHERE ts >= ?1",
            [since_ts as i64],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// Insert an address label (classifier growth loop).
    pub fn upsert_label(
        &self,
        address: Address,
        name: &str,
        source: &str,
        first_seen_block: u64,
    ) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO labels(address, kind, name, entity, evidence, first_seen_block, source)
             VALUES(?1, 'searcher', ?2, NULL, NULL, ?3, ?4)
             ON CONFLICT(address) DO NOTHING",
            rusqlite::params![
                format!("{:#x}", address),
                name,
                first_seen_block as i64,
                source
            ],
        )?;
        Ok(())
    }

    /// Pools seen in realized ops (missing-pool report, `--emit-missing-pools`).
    pub fn distinct_route_pools(
        &self,
        from_block: u64,
        to_block: u64,
    ) -> anyhow::Result<Vec<String>> {
        let sql = format!(
            "SELECT DISTINCT j.value->>'$.pool' AS pool
             FROM mev_ops m, json_each(COALESCE(m.route_json, '[]')) j
             WHERE m.block_number BETWEEN {from_block} AND {to_block}
               AND j.value->>'$.pool' IS NOT NULL
             ORDER BY pool"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    // ── rejection capture ──────────────────────────────────────────────

    /// Insert one rejected-candidate row (run_id/chain stamped here).
    pub fn insert_rejected_candidate(
        &self,
        run_id: &str,
        chain: &str,
        r: &crate::explorer::RejectedCandidate,
    ) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO rejected_candidates
               (run_id, chain, block_number, tx_index, strategy, pool_a, pool_b, path,
                token_in, token_out, input_amount, expected_profit, expected_profit_usd,
                gas_cost_wei, reject_reason, detail, created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
            rusqlite::params![
                run_id,
                chain,
                r.block_number as i64,
                r.tx_index.map(|v| v as i64),
                r.strategy,
                r.pool_a,
                r.pool_b,
                r.path,
                r.token_in,
                r.token_out,
                r.input_amount,
                r.expected_profit,
                r.expected_profit_usd,
                r.gas_cost_wei,
                r.reject_reason,
                r.detail,
                r.created_at as i64,
            ],
        )?;
        Ok(())
    }

    /// Rejected candidates in a block window (`validate`/`explain`).
    pub fn rejected_in_range(
        &self,
        chain: &str,
        from_block: u64,
        to_block: u64,
    ) -> anyhow::Result<Vec<RejectedRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT block_number, tx_index, strategy, pool_a, pool_b, token_in, token_out,
                    expected_profit, gas_cost_wei, reject_reason, detail
             FROM rejected_candidates
             WHERE chain = ?1 AND block_number BETWEEN ?2 AND ?3
             ORDER BY block_number, tx_index",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![chain, from_block as i64, to_block as i64],
            |r| {
                Ok(RejectedRow {
                    block_number: r.get::<_, i64>(0)? as u64,
                    tx_index: r.get::<_, Option<i64>>(1)?.map(|v| v as u64),
                    strategy: r.get(2)?,
                    pool_a: r.get(3)?,
                    pool_b: r.get(4)?,
                    token_in: r.get(5)?,
                    token_out: r.get(6)?,
                    expected_profit: r.get(7)?,
                    gas_cost_wei: r.get(8)?,
                    reject_reason: r.get(9)?,
                    detail: r.get(10)?,
                })
            },
        )?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// True when the window has any rejection rows (drives the
    /// `unknown-coverage` degradation in `validate`).
    pub fn has_rejections(&self, chain: &str, from_block: u64, to_block: u64) -> bool {
        self.conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM rejected_candidates
                 WHERE chain = ?1 AND block_number BETWEEN ?2 AND ?3)",
                rusqlite::params![chain, from_block as i64, to_block as i64],
                |r| r.get::<_, i64>(0),
            )
            .map(|v| v != 0)
            .unwrap_or(false)
    }

    // ── reporting additions ────────────────────────────────────────────

    /// Whole-window overview stats (`stats` header block).
    pub fn stats_overview(&self, since_ts: u64) -> anyhow::Result<OverviewRow> {
        self.stats_overview_filtered(since_ts, None)
    }

    /// `stats_overview` restricted to one `kind` value (`stats --kind`).
    pub fn stats_overview_filtered(
        &self,
        since_ts: u64,
        kind: Option<&str>,
    ) -> anyhow::Result<OverviewRow> {
        let kind_clause = kind
            .map(|k| format!("AND kind = '{k}'"))
            .unwrap_or_default();
        let sql = format!(
            "SELECT COUNT(*),
                    COALESCE(SUM(profit_usd), 0),
                    COALESCE(SUM(net_profit_usd), 0),
                    COALESCE(SUM(gas_cost_usd), 0),
                    COALESCE(MAX(profit_usd), 0),
                    COUNT(DISTINCT eoa)
             FROM mev_ops WHERE ts >= {since_ts} {kind_clause}"
        );
        let row = self.conn.query_row(&sql, [], |r| {
            Ok(OverviewRow {
                ops: r.get::<_, i64>(0)?,
                gross_usd: r.get(1)?,
                net_usd: r.get(2)?,
                gas_usd: r.get(3)?,
                highest_single_usd: r.get(4)?,
                searchers: r.get::<_, i64>(5)?,
            })
        })?;
        Ok(row)
    }

    /// All ops since a timestamp (`export`).
    pub fn ops_since(&self, since_ts: u64) -> anyhow::Result<Vec<MevOpRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, block_number, tx_index, tx_hash, ts, kind, eoa, contract,
                    confidence, canonical_id, profit_token, profit_amount, profit_usd,
                    gas_cost_usd, net_profit_usd, route_json, victim_hashes,
                    details_json, detector, created_at
             FROM mev_ops WHERE ts >= ?1 ORDER BY block_number, tx_index",
        )?;
        let rows = stmt.query_map([since_ts as i64], map_mev_op_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Distinct pools referenced by scanner opportunities in a window
    /// (pool-coverage side of the M1 missing-pool report).
    pub fn opportunity_pools(
        &self,
        chain: &str,
        from_block: u64,
        to_block: u64,
    ) -> anyhow::Result<std::collections::HashSet<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT pool_a, pool_b FROM opportunities
             WHERE chain = ?1 AND block_number BETWEEN ?2 AND ?3",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![chain, from_block as i64, to_block as i64],
            |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                ))
            },
        )?;
        let mut out = std::collections::HashSet::new();
        for r in rows {
            let (a, b) = r?;
            if let Some(a) = a {
                out.insert(a);
            }
            if let Some(b) = b {
                out.insert(b);
            }
        }
        Ok(out)
    }

    /// Store `show --trace` verification results on the op's details.
    pub fn mark_trace_verified(
        &self,
        tx_hash: &str,
        verification: &TraceVerification,
    ) -> anyhow::Result<()> {
        for op in self.ops_for_tx(tx_hash)? {
            let mut details: serde_json::Value = op
                .details_json
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or(serde_json::json!({}));
            details["trace_verified"] = serde_json::json!(true);
            if let Some(usd) = verification.trace_profit_usd {
                details["trace_profit_usd"] = serde_json::json!(usd);
            }
            if let Some(usd) = verification.expected_profit_usd {
                details["expected_profit_usd"] = serde_json::json!(usd);
            }
            if let Some(pct) = verification.profit_error_pct {
                details["profit_error_pct"] = serde_json::json!(pct);
            }
            if !verification.native_delta_wei.is_empty() {
                details["trace_native_delta_wei"] =
                    serde_json::json!(verification.native_delta_wei);
            }
            details["trace_note"] = serde_json::json!(verification.note);
            self.conn.execute(
                "UPDATE mev_ops SET details_json = ?1 WHERE id = ?2",
                rusqlite::params![details.to_string(), op.id],
            )?;
        }
        Ok(())
    }

    /// Label a searcher automatically after a confirmed classifier hit
    /// (growth loop): address → label with `source = classifier`.
    pub fn label_searcher_auto(&self, address: Address, block: u64) -> anyhow::Result<()> {
        let addr = format!("{address:#x}");
        let exists: Option<i64> = self
            .conn
            .query_row("SELECT 1 FROM labels WHERE address = ?1", [&addr], |r| {
                r.get(0)
            })
            .map(Some)
            .unwrap_or(None);
        if exists.is_none() {
            self.upsert_label(address, "unclassified-searcher", "classifier", block)?;
        }
        Ok(())
    }
}

/// A transaction row to persist alongside block facts.
pub struct TxRow {
    pub hash: B256,
    pub tx_index: u64,
    pub from: Address,
    pub to: Option<Address>,
    pub success: bool,
    pub gas_used: u64,
    pub effective_gas_price_gwei: f64,
    pub priority_fee_gwei: f64,
    pub value: U256,
}

/// A swap row to persist.
pub struct SwapRow {
    pub tx_index: u64,
    pub log_index: u64,
    pub pool: Address,
    pub amm: crate::explorer::types::Amm,
    pub token_in: Address,
    pub token_out: Address,
    pub amount_in: U256,
    pub amount_out: U256,
}

/// A transfer row to persist.
pub struct TransferRow {
    pub tx_index: u64,
    pub log_index: u64,
    pub token: Address,
    pub from: Address,
    pub to: Address,
    pub amount: U256,
}

/// Everything a single indexed block contributes to the store.
pub struct BlockFactsInput<'a> {
    pub block_number: u64,
    pub block_hash: &'a B256,
    pub ts: u64,
    pub base_fee_gwei: Option<f64>,
    pub tx_count: usize,
    pub txs: &'a [TxRow],
    pub swaps: &'a [SwapRow],
    pub transfers: &'a [TransferRow],
    pub events: &'a [MevEvent],
    pub native_price_usd: Option<f64>,
    pub token_prices: &'a std::collections::HashMap<Address, crate::explorer::pricing::TokenUsd>,
}

/// One scanner opportunity row to insert (`opportunities` results table).
pub struct OpportunityInput<'a> {
    pub run_id: &'a str,
    pub chain: &'a str,
    pub block_number: u64,
    pub tx_index: Option<u64>,
    pub strategy: &'a str,
    pub pool_a: Option<Address>,
    pub pool_b: Option<Address>,
    pub token_in: Option<Address>,
    pub token_out: Option<Address>,
    pub input_amount: Option<U256>,
    pub expected_profit: Option<U256>,
    pub gas_cost_wei: Option<U256>,
    pub path: Option<&'a str>,
    pub timestamp: Option<u64>,
    pub mempool_only: bool,
    pub confidence: Option<&'a str>,
    pub sender: Option<Address>,
    pub tx_hash: Option<B256>,
    pub detection_path: Option<&'a str>,
    pub canonical_id: Option<&'a str>,
}

/// One scanner opportunity row (`opportunities` table).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpportunityRow {
    pub run_id: Option<String>,
    pub block_number: u64,
    pub tx_index: Option<u64>,
    pub strategy: String,
    pub pool_a: Option<String>,
    pub pool_b: Option<String>,
    pub token_in: Option<String>,
    pub token_out: Option<String>,
    pub expected_profit: Option<String>,
    pub mempool_only: bool,
    pub detection_path: Option<String>,
    pub canonical_id: Option<String>,
    pub tx_hash: Option<String>,
}

/// Per-run aggregate over the `opportunities` table (`/api/opportunities/runs`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpportunityRunSummary {
    pub run_id: String,
    pub count: u64,
    pub first_ts: Option<u64>,
    pub last_ts: Option<u64>,
}

/// One rejected-candidate row (`rejected_candidates` table).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RejectedRow {
    pub block_number: u64,
    pub tx_index: Option<u64>,
    pub strategy: String,
    pub pool_a: Option<String>,
    pub pool_b: Option<String>,
    pub token_in: Option<String>,
    pub token_out: Option<String>,
    pub expected_profit: Option<String>,
    pub gas_cost_wei: Option<String>,
    pub reject_reason: String,
    pub detail: Option<String>,
}

/// Window-wide overview stats (`explorer stats` header).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverviewRow {
    pub ops: i64,
    pub gross_usd: f64,
    pub net_usd: f64,
    pub gas_usd: f64,
    pub highest_single_usd: f64,
    pub searchers: i64,
}

fn map_feed_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<FeedRow> {
    Ok(FeedRow {
        ts: r.get::<_, i64>(0)? as u64,
        block_number: r.get::<_, i64>(1)? as u64,
        kind: r.get(2)?,
        profit_token: r.get(3)?,
        profit_amount: r.get(4)?,
        profit_usd: r.get(5)?,
        gas_cost_usd: r.get(6)?,
        net_profit_usd: r.get(7)?,
        eoa: r.get(8)?,
        tx_hash: r.get(9)?,
        route_json: r.get(10)?,
        native_price_usd: None,
    })
}

fn latest_native_price(conn: &rusqlite::Connection) -> anyhow::Result<Option<f64>> {
    // Native keyed at 0x000…000 in the prices table.
    let mut stmt = conn.prepare(
        "SELECT usd FROM prices
         WHERE lower(token) IN ('0x0000000000000000000000000000000000000000', '0x0')
         ORDER BY hour DESC LIMIT 1",
    )?;
    let mut rows = stmt.query([])?;
    if let Some(row) = rows.next()? {
        Ok(Some(row.get::<_, f64>(0)?))
    } else {
        Ok(None)
    }
}

fn map_stats_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<StatsRow> {
    Ok(StatsRow {
        label: r.get(0)?,
        ops: r.get(1)?,
        gross_usd: r.get(2)?,
        net_usd: r.get(3)?,
    })
}

fn map_mev_op_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<MevOpRow> {
    Ok(MevOpRow {
        id: r.get(0)?,
        block_number: r.get::<_, i64>(1)? as u64,
        tx_index: r.get::<_, Option<i64>>(2)?.map(|v| v as u64),
        tx_hash: r.get(3)?,
        ts: r.get::<_, i64>(4)? as u64,
        kind: r.get(5)?,
        eoa: r.get(6)?,
        contract: r.get(7)?,
        confidence: r.get(8)?,
        canonical_id: r.get(9)?,
        profit_token: r.get(10)?,
        profit_amount: r.get(11)?,
        profit_usd: r.get(12)?,
        gas_cost_usd: r.get(13)?,
        net_profit_usd: r.get(14)?,
        route_json: r.get(15)?,
        victim_hashes: r.get(16)?,
        details_json: r.get(17)?,
        detector: r.get(18)?,
        created_at: r.get::<_, i64>(19)? as u64,
    })
}

fn stats_grouped(
    conn: &Connection,
    group_col: &str,
    since_ts: u64,
    kind: Option<&str>,
) -> anyhow::Result<Vec<StatsRow>> {
    let kind_clause = kind
        .map(|k| format!("AND kind = '{k}'"))
        .unwrap_or_default();
    let sql = format!(
        "SELECT {group_col} AS label, COUNT(*) AS ops,
                COALESCE(SUM(profit_usd), 0) AS gross_usd,
                COALESCE(SUM(net_profit_usd), 0) AS net_usd
         FROM mev_ops
         WHERE ts >= {since_ts} {kind_clause}
         GROUP BY label
         ORDER BY gross_usd DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], map_stats_row)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn top_grouped(
    conn: &Connection,
    group_col: &str,
    since_ts: u64,
    limit: usize,
    kind: Option<&str>,
) -> anyhow::Result<Vec<StatsRow>> {
    let kind_clause = kind
        .map(|k| format!("AND kind = '{k}'"))
        .unwrap_or_default();
    let sql = format!(
        "SELECT {group_col} AS label, COUNT(*) AS ops,
                COALESCE(SUM(profit_usd), 0) AS gross_usd,
                COALESCE(SUM(net_profit_usd), 0) AS net_usd
         FROM mev_ops
         WHERE ts >= {since_ts} {kind_clause} AND {group_col} IS NOT NULL
         GROUP BY label
         ORDER BY gross_usd DESC
         LIMIT {limit}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], map_stats_row)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explorer::types::{Confidence, MevEvent};
    use alloy::primitives::{address, b256};

    fn sample_event(block: u64) -> MevEvent {
        MevEvent {
            block,
            ts: 1_700_000_000,
            tx_index: 3,
            tx_hash: b256!("1111111111111111111111111111111111111111111111111111111111111111"),
            kind: MevKind::ArbAtomic,
            searcher: address!("2222000000000000000000000000000000000002"),
            contract: None,
            pools: vec![address!("3333000000000000000000000000000000000003")],
            profit_token: Some(address!("4444000000000000000000000000000000000004")),
            profit_amount: Some(U256::from(1_000_000u64)),
            profit_tokens: vec![],
            profit_usd: None,
            gas_cost_wei: U256::from(100_000_000_000_000u64),
            flashloan_fee_wei: None,
            flashloan_fee_token: None,
            confidence: Confidence::Exact,
            victim_hashes: vec![],
            victim_swap_size: None,
            details: serde_json::json!({
                "route": [{"pool": "0x3333", "amm": "v3"}]
            }),
        }
    }

    #[test]
    fn insert_and_query_roundtrip() {
        let store = ExplorerStore::open_in_memory().unwrap();
        let mut prices = std::collections::HashMap::new();
        prices.insert(
            address!("4444000000000000000000000000000000000004"),
            crate::explorer::pricing::TokenUsd {
                usd: 0.5,
                decimals: 6,
            },
        );
        let n = store
            .insert_block_facts(BlockFactsInput {
                block_number: 100,
                block_hash: &b256!(
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                ),
                ts: 1_700_000_000,
                base_fee_gwei: Some(25.0),
                tx_count: 5,
                txs: &[],
                swaps: &[],
                transfers: &[],
                events: &[sample_event(100)],
                native_price_usd: Some(0.75),
                token_prices: &prices,
            })
            .unwrap();
        assert_eq!(n, 1);
        assert!(store.block_classified(100).unwrap());
        assert!(!store.block_classified(101).unwrap());

        let ops = store.ops_in_range(100, 100, &[]).unwrap();
        assert_eq!(ops.len(), 1);
        assert!(ops[0].profit_usd.unwrap() > 0.0);
        assert!(ops[0].net_profit_usd.unwrap() < ops[0].profit_usd.unwrap());
        assert!(ops[0]
            .canonical_id
            .as_deref()
            .unwrap()
            .starts_with("ArbAtomic|"));

        let blocks = store
            .blocks_with_kind(100, 100, MevKind::ArbAtomic)
            .unwrap();
        assert!(blocks.contains(&100));

        let gaps = store.unclassified_blocks(99, 102).unwrap();
        assert_eq!(gaps, vec![99, 101, 102]);

        store.unwind_from(100).unwrap();
        assert!(!store.block_classified(100).unwrap());
    }

    #[test]
    fn multi_residual_profit_usd_sums_all_priced_tokens() {
        let store = ExplorerStore::open_in_memory().unwrap();
        let tok_a = address!("4444000000000000000000000000000000000004"); // 6 decimals
        let tok_b = address!("5555000000000000000000000000000000000005"); // 18 decimals
        let mut ev = sample_event(200);
        ev.profit_token = Some(tok_a);
        ev.profit_amount = Some(U256::from(1_000_000u64)); // 1 token @0.5 = 0.5 USD
        ev.profit_tokens = vec![
            (tok_a, U256::from(1_000_000u64)),
            (tok_b, U256::from(1_000_000_000_000_000_000u64)), // 1 token @2.0 = 2 USD
        ];
        let mut prices = std::collections::HashMap::new();
        prices.insert(
            tok_a,
            crate::explorer::pricing::TokenUsd {
                usd: 0.5,
                decimals: 6,
            },
        );
        prices.insert(
            tok_b,
            crate::explorer::pricing::TokenUsd {
                usd: 2.0,
                decimals: 18,
            },
        );
        store
            .insert_block_facts(BlockFactsInput {
                block_number: 200,
                block_hash: &b256!(
                    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                ),
                ts: 1_700_000_000,
                base_fee_gwei: Some(25.0),
                tx_count: 1,
                txs: &[],
                swaps: &[],
                transfers: &[],
                events: &[ev],
                native_price_usd: Some(0.75),
                token_prices: &prices,
            })
            .unwrap();
        let ops = store.ops_in_range(200, 200, &[]).unwrap();
        assert_eq!(ops.len(), 1);
        let expected = 0.5 + 2.0;
        let profit_usd = ops[0].profit_usd.unwrap();
        assert!((profit_usd - expected).abs() < 1e-6, "got {profit_usd}");
        // Net subtracts gas (100_000_000_000_000 wei native @0.75 = 0.000075 USD).
        let gas_usd = 100_000_000_000_000f64 / 1e18 * 0.75;
        assert!(
            (ops[0].net_profit_usd.unwrap() - (expected - gas_usd)).abs() < 1e-9,
            "net got {}",
            ops[0].net_profit_usd.unwrap()
        );
        // Display-primary pair is persisted unchanged.
        assert_eq!(
            ops[0].profit_token.as_deref(),
            Some("0x4444000000000000000000000000000000000004")
        );
        assert_eq!(ops[0].profit_amount.as_deref(), Some("1000000"));
        // Residual list surfaces in details for forensic display.
        assert!(ops[0]
            .details_json
            .as_deref()
            .unwrap_or("")
            .contains("\"profit_tokens\""));
    }

    #[test]
    fn fot_profit_token_flagged_approximate_and_realized_rate_falls_back() {
        let usdt = address!("dac17f958d2ee523a2206206994597c13d831ec7"); // bundled FOT
        let longtail = address!("0a00000000000000000000000000000000000000");
        let usdc = address!("4444000000000000000000000000000000000004");

        let mut prices = std::collections::HashMap::new();
        prices.insert(
            usdt,
            crate::explorer::pricing::TokenUsd {
                usd: 0.99,
                decimals: 6,
            },
        );
        prices.insert(
            usdc,
            crate::explorer::pricing::TokenUsd {
                usd: 1.0,
                decimals: 6,
            },
        );

        let store = ExplorerStore::open_in_memory().unwrap();
        let mut fot = sample_event(301);
        fot.profit_token = Some(usdt);
        fot.profit_amount = Some(U256::from(1_000_000u64));
        // Unpriced profit token priced at the realized route rate (Phase 2.4):
        // 1 longtail token sold for 2 USDC ⇒ each is worth 2 USD.
        let mut realized = sample_event(302);
        realized.profit_token = Some(longtail);
        realized.profit_amount = Some(U256::from(500_000u64));
        realized.details = serde_json::json!({
            "route": [{
                "pool": "0x3333",
                "amm": "v2",
                "token_in": format!("{longtail:#x}"),
                "token_out": format!("{usdc:#x}"),
                "amount_in": "1000000",
                "amount_out": "2000000",
            }]
        });

        for (block, ev) in [(301u64, fot), (302, realized)] {
            store
                .insert_block_facts(BlockFactsInput {
                    block_number: block,
                    block_hash: &b256!(
                        "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
                    ),
                    ts: 1_700_000_000,
                    base_fee_gwei: Some(25.0),
                    tx_count: 1,
                    txs: &[],
                    swaps: &[],
                    transfers: &[],
                    events: &[ev],
                    native_price_usd: Some(0.75),
                    token_prices: &prices,
                })
                .unwrap();
        }

        let ops = store.ops_in_range(301, 302, &[]).unwrap();
        assert_eq!(ops.len(), 2);

        let fot_row = &ops[0];
        assert!(fot_row
            .details_json
            .as_deref()
            .unwrap_or("")
            .contains("\"usd_approximate\":true"));
        assert!(fot_row
            .details_json
            .as_deref()
            .unwrap_or("")
            .contains("\"approximate_reason\":\"FOT\""));
        // FOT still priced from the external feed.
        assert!((fot_row.profit_usd.unwrap() - 0.99).abs() < 1e-6);

        let realized_row = &ops[1];
        // 0.5 longtail token × realized 2.0 USD/token = 1.0, minus gas.
        let expected_profit = 1.0;
        assert!(
            (realized_row.profit_usd.unwrap() - expected_profit).abs() < 1e-6,
            "got {}",
            realized_row.profit_usd.unwrap()
        );
        let gas_usd = 100_000_000_000_000f64 / 1e18 * 0.75;
        assert_eq!(
            realized_row.net_profit_usd.unwrap(),
            expected_profit - gas_usd
        );
    }

    #[test]
    fn jit_open_positions_roundtrip() {
        let store = ExplorerStore::open_in_memory().unwrap();
        let pool = address!("3333000000000000000000000000000000000003");
        let owner = address!("2222000000000000000000000000000000000002");
        store
            .record_jit_open(&[OpenPosition {
                pool,
                owner,
                tick_lower: -100,
                tick_upper: 100,
                opened_block: 500,
                liquidity: 1000,
            }])
            .unwrap();
        store
            .record_jit_open(&[OpenPosition {
                pool,
                owner,
                tick_lower: -200,
                tick_upper: 200,
                opened_block: 10,
                liquidity: 7,
            }])
            .unwrap();

        let open = store.open_positions(100).unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].liquidity, 1000);
        assert_eq!(open[0].opened_block, 500);

        store.close_jit_position(pool, owner, -100, 100).unwrap();
        assert!(store
            .open_positions(0)
            .unwrap()
            .iter()
            .any(|p| p.tick_lower == -200));

        assert_eq!(store.prune_jit_positions(100).unwrap(), 1);
        assert!(store.open_positions(0).unwrap().is_empty());
    }

    fn liquidation_event(collateral: Address, debt: Address) -> MevEvent {
        let mut ev = sample_event(1);
        ev.kind = MevKind::Liquidation;
        ev.profit_token = Some(collateral);
        ev.profit_amount = Some(U256::from(500));
        ev.details = serde_json::json!({
            "collateral_asset": format!("{collateral:#x}"),
            "debt_asset": format!("{debt:#x}"),
            "collateral_amount": "500",
            "debt_to_cover": "300",
            "reconciled": true,
            "reasons": [],
        });
        ev
    }

    #[test]
    fn liquidation_pnl_cross_asset_is_inferred() {
        let collateral = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let debt = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let mut prices = std::collections::HashMap::new();
        prices.insert(
            collateral,
            crate::explorer::pricing::TokenUsd {
                usd: 2.0,
                decimals: 0,
            },
        );
        prices.insert(
            debt,
            crate::explorer::pricing::TokenUsd {
                usd: 1.0,
                decimals: 0,
            },
        );
        let ev = liquidation_event(collateral, debt);
        let (usd, conf, details) = liquidation_pnl(&ev, &prices);
        // 500 * $2 − 300 * $1 = 700
        assert!((usd.unwrap() - 700.0).abs() < 1e-9);
        assert_eq!(conf, "inferred");
        assert!(details.contains("LIQ_BONUS_APPROX"));
    }

    #[test]
    fn liquidation_pnl_missing_price_falls_back() {
        let collateral = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let debt = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let prices = std::collections::HashMap::new();
        let ev = liquidation_event(collateral, debt);
        let (usd, conf, details) = liquidation_pnl(&ev, &prices);
        assert!(usd.is_none());
        assert_eq!(conf, "inferred");
        assert!(details.contains("MULTI_ASSET_PRICING"));
        assert!(details.contains("collateral_amount"));
    }

    #[test]
    fn liquidation_pnl_same_asset_is_exact() {
        let asset = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let mut prices = std::collections::HashMap::new();
        prices.insert(
            asset,
            crate::explorer::pricing::TokenUsd {
                usd: 1.0,
                decimals: 0,
            },
        );
        let ev = liquidation_event(asset, asset);
        let (usd, conf, _) = liquidation_pnl(&ev, &prices);
        assert!((usd.unwrap() - 200.0).abs() < 1e-9);
        assert_eq!(conf, "exact");
    }

    #[test]
    fn sandwich_loss_gate_skips_unprofitable() {
        let store = ExplorerStore::open_in_memory().unwrap();
        let mut prices = std::collections::HashMap::new();
        prices.insert(
            address!("4444000000000000000000000000000000000004"),
            crate::explorer::pricing::TokenUsd {
                usd: 0.5,
                decimals: 6,
            },
        );
        // Zero extractable profit cannot cover front+back gas → not realized MEV.
        let mut loss = sample_event(130);
        loss.kind = MevKind::Sandwich;
        loss.profit_amount = Some(U256::ZERO);
        loss.details = serde_json::json!({ "reason": "test" });
        let n = store
            .insert_block_facts(BlockFactsInput {
                block_number: 130,
                block_hash: &b256!(
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaae"
                ),
                ts: 1_700_000_300,
                base_fee_gwei: Some(25.0),
                tx_count: 5,
                txs: &[],
                swaps: &[],
                transfers: &[],
                events: &[loss],
                native_price_usd: Some(0.75),
                token_prices: &prices,
            })
            .unwrap();
        assert_eq!(n, 0);
        assert!(store.ops_in_range(130, 130, &[]).unwrap().is_empty());
        // ...but the block is still marked classified (0 persisted ops).
        assert!(store.block_classified(130).unwrap());

        // A profitable sandwich (1.0 USD profit > ~0.00008 USD gas) is kept.
        let mut win = sample_event(131);
        win.kind = MevKind::Sandwich;
        let n = store
            .insert_block_facts(BlockFactsInput {
                block_number: 131,
                block_hash: &b256!(
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaf"
                ),
                ts: 1_700_000_400,
                base_fee_gwei: Some(25.0),
                tx_count: 5,
                txs: &[],
                swaps: &[],
                transfers: &[],
                events: &[win],
                native_price_usd: Some(0.75),
                token_prices: &prices,
            })
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(store.ops_in_range(131, 131, &[]).unwrap().len(), 1);
    }

    #[test]
    fn stats_kind_filter_matches_only_that_kind() {
        let store = ExplorerStore::open_in_memory().unwrap();
        let mut prices = std::collections::HashMap::new();
        prices.insert(
            address!("4444000000000000000000000000000000000004"),
            crate::explorer::pricing::TokenUsd {
                usd: 0.5,
                decimals: 6,
            },
        );
        let mut sand_evt = sample_event(120);
        sand_evt.ts = 1_700_000_101;
        sand_evt.kind = MevKind::Sandwich;
        let mut arb_evt = sample_event(121);
        arb_evt.ts = 1_700_000_200;
        arb_evt.kind = MevKind::ArbAtomic;
        let n = store
            .insert_block_facts(BlockFactsInput {
                block_number: 120,
                block_hash: &b256!(
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaab"
                ),
                ts: 1_700_000_100,
                base_fee_gwei: Some(25.0),
                tx_count: 5,
                txs: &[],
                swaps: &[],
                transfers: &[],
                events: &[sand_evt],
                native_price_usd: Some(0.75),
                token_prices: &prices,
            })
            .unwrap();
        assert_eq!(n, 1);
        let n = store
            .insert_block_facts(BlockFactsInput {
                block_number: 121,
                block_hash: &b256!(
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaac"
                ),
                ts: 1_700_000_200,
                base_fee_gwei: Some(25.0),
                tx_count: 5,
                txs: &[],
                swaps: &[],
                transfers: &[],
                events: &[arb_evt],
                native_price_usd: Some(0.75),
                token_prices: &prices,
            })
            .unwrap();
        assert_eq!(n, 1);

        let all = store.stats_by_kind_filtered(1_699_999_000, None).unwrap();
        assert_eq!(all.len(), 2);

        let arb = store
            .stats_by_kind_filtered(1_699_999_000, Some("arb_atomic"))
            .unwrap();
        assert_eq!(arb.len(), 1);
        assert_eq!(arb[0].label, "arb_atomic");
        assert_eq!(arb[0].ops, 1);

        let sand = store
            .stats_by_kind_filtered(1_699_999_000, Some("sandwich"))
            .unwrap();
        assert_eq!(sand.len(), 1);
        assert_eq!(sand[0].label, "sandwich");

        let none = store
            .stats_by_kind_filtered(1_699_999_000, Some("liquidation"))
            .unwrap();
        assert!(none.is_empty());

        let overview = store
            .stats_overview_filtered(1_699_999_000, Some("sandwich"))
            .unwrap();
        assert_eq!(overview.ops, 1);

        let senders = store
            .top_senders_filtered(1_699_999_000, 5, Some("arb_atomic"))
            .unwrap();
        assert!(!senders.is_empty());

        let senders_sand = store
            .top_senders_filtered(1_699_999_000, 5, Some("sandwich"))
            .unwrap();
        assert!(!senders_sand.is_empty());

        let misc = store
            .top_senders_filtered(1_699_999_000, 5, Some("liquidations"))
            .unwrap();
        assert!(misc.is_empty());
    }

    #[test]
    fn sync_state_roundtrip() {
        let store = ExplorerStore::open_in_memory().unwrap();
        assert_eq!(store.get_indexed_to(137).unwrap(), 0);
        store.set_sync_state(137, 1000, 990).unwrap();
        assert_eq!(store.get_indexed_to(137).unwrap(), 990);
    }

    #[test]
    fn price_cache_window() {
        let store = ExplorerStore::open_in_memory().unwrap();
        let tok = address!("4444000000000000000000000000000000000004");
        store.put_price(tok, 1000, 1.23, "test").unwrap();
        assert!(store.price_at(tok, 1000).unwrap().is_some());
        assert!(store.price_at(tok, 1010).unwrap().is_some());
        assert!(store.price_at(tok, 2000).unwrap().is_none());
    }
}
