//! Recompute USD columns from the `prices` table. No RPC.
//!
//! Gas wei is not a `mev_ops` column, so `gas_cost_usd` is left as stored.
//! `net_profit_usd` is rebuilt from the new `profit_usd`, the stored gas USD,
//! and the stored flash-loan fee (skipped when `details_json` says the
//! classifier already netted that fee into the residual). Historical native
//! pricing for gas still needs a stored wei figure; this path only moves
//! token-denominated profit.
use alloy::primitives::{Address, U256};

use super::ExplorerStore;
use crate::explorer::pricing::{hour_bucket, token_amount_to_usd, wei_to_usd, TokenUsd};
use crate::explorer::profit::NATIVE_MARKER;
use crate::explorer::types::MevKind;

impl ExplorerStore {
    /// Reprice `profit_usd` / `net_profit_usd` / `excluded_from_net` for ops in
    /// `from_block..=to_block` using prices already in this database.
    ///
    /// `decimals_of` is the caller's token-metadata cache. A token with no
    /// decimals or no usable quote is left unchanged. Returns how many rows
    /// had `profit_usd` rewritten.
    pub fn reprice_range(
        &self,
        from_block: u64,
        to_block: u64,
        decimals_of: impl Fn(Address) -> Option<u32>,
    ) -> anyhow::Result<u64> {
        let mut stmt = self.conn.prepare(
            "SELECT m.id, m.profit_token, m.profit_amount, m.gas_cost_usd,
                    m.flashloan_fee_usd, m.kind, m.details_json, b.ts
               FROM mev_ops m
               JOIN blocks b ON b.block_number = m.block_number
              WHERE m.block_number BETWEEN ?1 AND ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![from_block as i64, to_block as i64], |r| {
            Ok(RepriceRow {
                id: r.get(0)?,
                profit_token: r.get(1)?,
                profit_amount: r.get(2)?,
                gas_cost_usd: r.get(3)?,
                flashloan_fee_usd: r.get(4)?,
                kind: r.get(5)?,
                details_json: r.get(6)?,
                ts: r.get::<_, i64>(7)? as u64,
            })
        })?;
        let mut pending: Vec<(i64, f64, Option<f64>, i64)> = Vec::new();
        for row in rows {
            let row = row?;
            let Some(profit_usd) = self.reprice_profit(&row, &decimals_of)? else {
                continue;
            };
            let fee_netted = details_flag(&row.details_json, "flashloan_fee_netted_in_ledger");
            let fee_for_net = if fee_netted {
                0.0
            } else {
                row.flashloan_fee_usd.unwrap_or(0.0)
            };
            let net = row.gas_cost_usd.map(|gas| profit_usd - gas - fee_for_net);
            let excluded = (row.kind == MevKind::Sandwich.as_str()
                && matches!(net, Some(n) if n <= 0.0)) as i64;
            pending.push((row.id, profit_usd, net, excluded));
        }
        drop(stmt);
        let mut updated = 0u64;
        let tx = self.conn.unchecked_transaction()?;
        for (id, profit_usd, net, excluded) in pending {
            let n = tx.execute(
                "UPDATE mev_ops
                    SET profit_usd = ?1, net_profit_usd = ?2, excluded_from_net = ?3
                  WHERE id = ?4",
                rusqlite::params![profit_usd, net, excluded, id],
            )?;
            updated += n as u64;
        }
        tx.commit()?;
        Ok(updated)
    }

    fn reprice_profit(
        &self,
        row: &RepriceRow,
        decimals_of: &impl Fn(Address) -> Option<u32>,
    ) -> anyhow::Result<Option<f64>> {
        let (Some(token_s), Some(amount_s)) = (&row.profit_token, &row.profit_amount) else {
            return Ok(None);
        };
        let Ok(token) = token_s.parse::<Address>() else {
            return Ok(None);
        };
        let Ok(amount) = amount_s.parse::<U256>() else {
            return Ok(None);
        };
        if amount.is_zero() {
            return Ok(Some(0.0));
        }
        if token == NATIVE_MARKER || token == Address::ZERO {
            let Some(native) = self.native_price_near(row.ts)? else {
                return Ok(None);
            };
            return Ok(Some(wei_to_usd(amount, native)));
        }
        let Some(decimals) = decimals_of(token) else {
            return Ok(None);
        };
        let hour = hour_bucket(row.ts);
        let Some((usd, _)) = self.price_at(token, hour)? else {
            return Ok(None);
        };
        let quoted = token_amount_to_usd(amount, &TokenUsd { usd, decimals });
        if quoted.is_finite() && quoted > 0.0 {
            Ok(Some(quoted))
        } else {
            Ok(None)
        }
    }
}

struct RepriceRow {
    id: i64,
    profit_token: Option<String>,
    profit_amount: Option<String>,
    gas_cost_usd: Option<f64>,
    flashloan_fee_usd: Option<f64>,
    kind: String,
    details_json: Option<String>,
    ts: u64,
}

fn details_flag(details_json: &Option<String>, key: &str) -> bool {
    let Some(raw) = details_json else {
        return false;
    };
    serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .and_then(|v| v.get(key).and_then(|x| x.as_bool()))
        .unwrap_or(false)
}
