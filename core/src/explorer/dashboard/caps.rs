//! Schema capability probe for read-only dashboard opens.
use std::collections::HashSet;

use rusqlite::Connection;
use serde::Serialize;

use crate::explorer::store::SCHEMA_HEAD;

/// Columns / indexes / tables present on the opened file.
///
/// Dashboard SQL is built from these flags so a pre-migration DB (e.g.
/// `user_version = 0` with 22-column `mev_ops`) never hits `no such column`.
#[derive(Debug, Clone, Serialize)]
pub struct StoreCaps {
    pub user_version: i64,
    pub schema_head: i64,
    pub has_subkind: bool,
    pub has_excluded_from_net: bool,
    pub has_volume_usd: bool,
    pub has_flashloan_fee_usd: bool,
    pub has_pricing_flags: bool,
    pub has_ops_ts_index: bool,
    pub has_ops_natural_key: bool,
    pub tables: Vec<String>,
}

impl StoreCaps {
    /// Probe `PRAGMA table_info` / `sqlite_master` on an open connection.
    pub fn probe(conn: &Connection) -> anyhow::Result<Self> {
        let user_version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let cols = column_names(conn, "mev_ops")?;
        let indexes = index_names(conn, "mev_ops")?;
        let tables = table_names(conn)?;
        Ok(Self {
            user_version,
            schema_head: SCHEMA_HEAD,
            has_subkind: cols.contains("subkind"),
            has_excluded_from_net: cols.contains("excluded_from_net"),
            has_volume_usd: cols.contains("volume_usd"),
            has_flashloan_fee_usd: cols.contains("flashloan_fee_usd"),
            has_pricing_flags: cols.contains("usd_approximate")
                && cols.contains("native_unpriced")
                && cols.contains("multi_asset_pricing")
                && cols.contains("pricing_clamped")
                && cols.contains("flash_fee_unpriced"),
            has_ops_ts_index: indexes.iter().any(|i| i == "mev_ops_ts"),
            has_ops_natural_key: indexes.iter().any(|i| i.contains("natural")),
            tables: {
                let mut t: Vec<String> = tables.into_iter().collect();
                t.sort();
                t
            },
        })
    }

    /// Human status for the P0 banner.
    pub fn status_label(&self) -> &'static str {
        if self.user_version >= self.schema_head {
            "at_head"
        } else if self.user_version == 0 {
            "legacy_unmigrated"
        } else {
            "behind_head"
        }
    }
}

fn column_names(conn: &Connection, table: &str) -> anyhow::Result<HashSet<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
    let mut out = HashSet::new();
    for row in rows {
        out.insert(row?);
    }
    Ok(out)
}

fn index_names(conn: &Connection, table: &str) -> anyhow::Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = ?1")?;
    let rows = stmt.query_map([table], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn table_names(conn: &Connection) -> anyhow::Result<HashSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut out = HashSet::new();
    for row in rows {
        out.insert(row?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explorer::store::ExplorerStore;

    #[test]
    fn head_store_reports_full_caps() {
        let store = ExplorerStore::open_in_memory().expect("mem");
        let caps = StoreCaps::probe(store.connection()).expect("probe");
        assert_eq!(caps.user_version, SCHEMA_HEAD);
        assert!(caps.has_subkind);
        assert!(caps.has_excluded_from_net);
        assert!(caps.has_pricing_flags);
        assert_eq!(caps.status_label(), "at_head");
    }
}
