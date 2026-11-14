//! One-time purge of retired explorer strategies.
//!
//! `bundler` was removed from the classifier (utility fee business, not
//! searcher MEV) and `bad_debt_liq` is no longer a strategy tag. Historical
//! rows are deleted (bundler) or rewritten (bad-debt tag) only by an explicit
//! CLI command, never on open: the operation is destructive and auditable.
use rusqlite::params;
use serde::Serialize;

use super::ExplorerStore;

/// What a purge (or dry run) found and changed.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PurgeReport {
    pub bundler_rows: usize,
    pub bad_debt_rows: usize,
    pub dry_run: bool,
}

/// `bundler` rows are utility ops whose subkind is bundler, or legacy rows
/// carrying the tag without being a mixed keeper/claim family.
const BUNDLER_WHERE: &str = "kind='utility' AND (subkind='bundler' OR \
    (details_json LIKE '%\"bundler\"%' \
     AND details_json NOT LIKE '%\"claim_and_sell\"%' \
     AND details_json NOT LIKE '%\"keeper\"%'))";

const BAD_DEBT_TAG: &str = "\"bad_debt_liq\"";

impl ExplorerStore {
    /// Delete retired `bundler` rows and strip the retired `bad_debt_liq` tag
    /// from surviving liquidation rows. `dry_run` only counts.
    pub fn purge_retired_strategies(&self, dry_run: bool) -> anyhow::Result<PurgeReport> {
        let bundler_sql = format!("SELECT COUNT(*) FROM mev_ops WHERE {BUNDLER_WHERE}");
        let bad_debt_sql = "SELECT COUNT(*) FROM mev_ops WHERE details_json LIKE ?1";
        let bad_debt_like = format!("%{BAD_DEBT_TAG}%");
        if dry_run {
            // Count only; no transaction and no write, so a read-only open
            // never needs a migration or a write lock.
            return Ok(PurgeReport {
                bundler_rows: self
                    .conn
                    .query_row(&bundler_sql, [], |r| r.get::<_, i64>(0))?
                    as usize,
                bad_debt_rows: self
                    .conn
                    .query_row(bad_debt_sql, params![bad_debt_like], |r| r.get::<_, i64>(0))?
                    as usize,
                dry_run,
            });
        }

        let tx = self.conn.unchecked_transaction()?;
        let bundler_rows: i64 = tx.query_row(&bundler_sql, [], |r| r.get(0))?;
        let bad_debt_rows: i64 =
            tx.query_row(bad_debt_sql, params![bad_debt_like], |r| r.get(0))?;
        let report = PurgeReport {
            bundler_rows: bundler_rows as usize,
            bad_debt_rows: bad_debt_rows as usize,
            dry_run,
        };

        tx.execute(&format!("DELETE FROM mev_ops WHERE {BUNDLER_WHERE}"), [])?;
        let rows: Vec<(i64, String)> = {
            let mut stmt = tx.prepare(
                "SELECT id, details_json FROM mev_ops \
                 WHERE details_json LIKE ?1",
            )?;
            let rows = stmt
                .query_map(params![format!("%{BAD_DEBT_TAG}%")], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        for (id, raw) in rows {
            let Ok(mut details) = serde_json::from_str::<serde_json::Value>(&raw) else {
                continue;
            };
            let mut changed = false;
            if let Some(tags) = details.get_mut("tags").and_then(|t| t.as_array_mut()) {
                let before = tags.len();
                tags.retain(|t| t.as_str() != Some("bad_debt_liq"));
                changed |= tags.len() != before;
            }
            if let Some(obj) = details.as_object_mut() {
                for key in ["bad_debt", "post_liq_insolvent"] {
                    changed |= obj.remove(key).is_some();
                }
            }
            if changed {
                tx.execute(
                    "UPDATE mev_ops SET details_json = ?1 WHERE id = ?2",
                    params![details.to_string(), id],
                )?;
            }
        }
        tx.commit()?;
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert_op(
        store: &ExplorerStore,
        hash: &str,
        kind: &str,
        subkind: Option<&str>,
        details: &str,
    ) {
        store
            .connection()
            .execute(
                "INSERT INTO mev_ops(
                   block_number, tx_index, tx_hash, ts, kind, eoa, confidence,
                   canonical_id, details_json, detector, created_at, subkind)
                 VALUES (1, 0, ?1, 1, ?2, '0xeoa', 'exact', ?3, ?4, 'explorer', 1, ?5)",
                params![hash, kind, format!("{kind}|{hash}"), details, subkind],
            )
            .unwrap();
    }

    #[test]
    fn purge_deletes_bundler_and_strips_bad_debt_tag() {
        let store = ExplorerStore::open_in_memory().unwrap();
        insert_op(
            &store,
            "0x01",
            "utility",
            Some("bundler"),
            r#"{"tags":["bundler","erc4337"]}"#,
        );
        insert_op(
            &store,
            "0x02",
            "utility",
            None,
            r#"{"tags":["claim_and_sell","bundler"]}"#,
        );
        insert_op(
            &store,
            "0x03",
            "liquidation",
            None,
            r#"{"tags":["bad_debt_liq"],"bad_debt":true,"post_liq_insolvent":true,"bad_debt_assets":"10"}"#,
        );

        let report = store.purge_retired_strategies(false).unwrap();
        assert_eq!(report.bundler_rows, 1);
        assert_eq!(report.bad_debt_rows, 1);

        let ops = store.ops_in_range(0, u64::MAX, &[], None).unwrap();
        assert_eq!(ops.len(), 2, "mixed claim row and liquidation survive");
        let liq = ops.iter().find(|o| o.kind == "liquidation").unwrap();
        let details = liq.details_json.as_deref().unwrap();
        assert!(!details.contains("bad_debt_liq"));
        assert!(!details.contains("post_liq_insolvent"));
        assert!(details.contains("bad_debt_assets"), "raw fact stays");
        assert!(ops.iter().any(|o| o
            .details_json
            .as_deref()
            .is_some_and(|d| d.contains("claim_and_sell"))));
    }

    #[test]
    fn dry_run_reports_without_writing() {
        let store = ExplorerStore::open_in_memory().unwrap();
        insert_op(
            &store,
            "0x01",
            "utility",
            Some("bundler"),
            r#"{"tags":["bundler"]}"#,
        );
        let report = store.purge_retired_strategies(true).unwrap();
        assert_eq!(report.bundler_rows, 1);
        assert_eq!(store.ops_in_range(0, u64::MAX, &[], None).unwrap().len(), 1);
    }
}
