//! Scratch query helper for offline benchmark comparisons: dumps the ops of a
//! block range from an explorer SQLite (read-only) as JSON.
//!
//! Usage: cargo run -p mev-scout-cli --example dbq -- <db> <from> <to>
use mev_scout_core::explorer::store::ExplorerStore;
use serde_json::json;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let usage = || anyhow::anyhow!("usage: dbq <db> <from> <to>");
    let db = args.next().ok_or_else(usage)?;
    let from: i64 = args.next().ok_or_else(usage)?.parse()?;
    let to: i64 = args.next().ok_or_else(usage)?.parse()?;

    let store = ExplorerStore::open_read_only(&db)?;
    let conn = store.connection();
    let classified: i64 = conn.query_row(
        "SELECT COUNT(*) FROM blocks_classified WHERE block BETWEEN ?1 AND ?2",
        [from, to],
        |r| r.get(0),
    )?;
    let mut stmt = conn.prepare(
        "SELECT tx_hash, block_number, kind, COALESCE(subkind, ''), confidence,
                COALESCE(net_profit_usd, 0.0), ts
         FROM mev_ops
         WHERE block_number BETWEEN ?1 AND ?2
         ORDER BY block_number, tx_index",
    )?;
    let ops = stmt
        .query_map([from, to], |r| {
            Ok(json!({
                "tx_hash": r.get::<_, String>(0)?,
                "block": r.get::<_, i64>(1)?,
                "kind": r.get::<_, String>(2)?,
                "subkind": r.get::<_, String>(3)?,
                "confidence": r.get::<_, String>(4)?,
                "net_usd": r.get::<_, f64>(5)?,
                "ts": r.get::<_, i64>(6)?,
            }))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    println!(
        "{}",
        serde_json::to_string(&json!({
            "classified_blocks": classified,
            "ops": ops,
        }))?
    );
    Ok(())
}
