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
        let mut pending: Vec<RepriceUpdate> = Vec::new();
        for row in rows {
            let row = row?;
            if row.kind.as_str() == "jit" {
                if let Some(update) = self.reprice_jit(&row, &decimals_of)? {
                    pending.push(update);
                    continue;
                }
            }
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
            let excluded = 0i64;
            pending.push(RepriceUpdate {
                id: row.id,
                profit_usd: Some(profit_usd),
                net_usd: net,
                excluded,
                gas_usd_set: false,
                gas_usd: None,
                details_json: None,
                usd_approximate: None,
            });
        }
        drop(stmt);
        let mut updated = 0u64;
        let tx = self.conn.unchecked_transaction()?;
        for u in pending {
            let n = tx.execute(
                "UPDATE mev_ops
                    SET profit_usd = ?1,
                        net_profit_usd = ?2,
                        excluded_from_net = ?3,
                        gas_cost_usd = CASE WHEN ?4 THEN ?5 ELSE gas_cost_usd END,
                        details_json = COALESCE(?6, details_json),
                        usd_approximate = COALESCE(?7, usd_approximate)
                  WHERE id = ?8",
                rusqlite::params![
                    u.profit_usd,
                    u.net_usd,
                    u.excluded,
                    u.gas_usd_set,
                    u.gas_usd,
                    u.details_json,
                    u.usd_approximate,
                    u.id
                ],
            )?;
            updated += n as u64;
        }
        tx.commit()?;
        Ok(updated)
    }

    /// Re-evaluate a settled JIT row from its stored accounting journal and the
    /// current `prices` table, using the same [`evaluate`] as ingest. A row
    /// whose accounting is not verified, or whose quotes are now missing, has
    /// its stale profit cleared instead of keeping an old valid-looking figure.
    fn reprice_jit(
        &self,
        row: &RepriceRow,
        decimals_of: &impl Fn(Address) -> Option<u32>,
    ) -> anyhow::Result<Option<RepriceUpdate>> {
        let Some(details) = row.details_json.as_deref() else {
            return Ok(None);
        };
        let mut value: serde_json::Value = match serde_json::from_str(details) {
            Ok(v) => v,
            Err(_) => return Ok(None),
        };
        let Some(mut accounting) =
            crate::explorer::jit_accounting::JitAccounting::from_details(&value)
        else {
            return Ok(None);
        };
        accounting.quotes.clear();
        for point in &accounting.timeline {
            if point.kind != "mint" && point.kind != "collect" {
                continue;
            }
            let hour = point.ts / 3600;
            for token in [accounting.token0, accounting.token1] {
                let (Some(decimals), Some((usd, source, observed))) =
                    (decimals_of(token), self.price_quote_at(token, hour)?)
                else {
                    continue;
                };
                let provenance = if observed == hour {
                    source
                } else {
                    format!("{source}@{observed}")
                };
                accounting
                    .quotes
                    .push(crate::explorer::jit_accounting::Quote {
                        token,
                        hour,
                        usd,
                        decimals,
                        source: provenance,
                    });
            }
        }
        for gas in &accounting.gas {
            if let Some(native) = self.native_price_near(gas.ts)? {
                accounting
                    .quotes
                    .push(crate::explorer::jit_accounting::Quote {
                        token: Address::ZERO,
                        hour: gas.ts / 3600,
                        usd: native,
                        decimals: 18,
                        source: "native_cached".into(),
                    });
            }
        }
        accounting.quotes.sort_by_key(|a| (a.token, a.hour));
        accounting
            .quotes
            .dedup_by(|a, b| a.token == b.token && a.hour == b.hour);
        accounting.valuation = crate::explorer::jit_accounting::evaluate(&accounting);
        let complete = accounting.status == "verified" && accounting.valuation.pricing_complete;
        value["jit_accounting"] = serde_json::to_value(&accounting)
            .map_err(|e| anyhow::anyhow!("serialize repriced JIT accounting: {e}"))?;
        value["usd_approximate"] = serde_json::json!(!complete);
        Ok(Some(RepriceUpdate {
            id: row.id,
            profit_usd: accounting.valuation.gross_usd,
            net_usd: accounting.valuation.net_profit_usd,
            excluded: 0,
            gas_usd_set: true,
            gas_usd: accounting.valuation.gas_usd,
            details_json: Some(value.to_string()),
            usd_approximate: Some(i64::from(!complete)),
        }))
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

struct RepriceUpdate {
    id: i64,
    profit_usd: Option<f64>,
    net_usd: Option<f64>,
    excluded: i64,
    /// When true, `gas_usd` replaces the stored value even if it is `None`.
    gas_usd_set: bool,
    gas_usd: Option<f64>,
    details_json: Option<String>,
    usd_approximate: Option<i64>,
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
