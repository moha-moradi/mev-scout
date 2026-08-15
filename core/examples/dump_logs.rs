use mev_scout_core::data::LogData;
use rusqlite::Connection;
use std::env;

fn main() -> anyhow::Result<()> {
    let block: u64 = env::args().nth(1).unwrap_or_else(|| "92045880".into()).parse()?;
    let conn = Connection::open(r"D:\gitlab.dte.repo\mev-scout\cache\polygon-mev-scout.sqlite")?;
    let mut stmt = conn.prepare(
        "SELECT r.tx_index, r.logs
         FROM receipts r INNER JOIN transactions t ON t.hash = r.tx_hash
         WHERE t.block_number = ?1 ORDER BY r.tx_index",
    )?;
    let rows = stmt.query_map([block as i64], |row| {
        Ok((row.get::<_, i64>(0)? as u64, row.get::<_, Vec<u8>>(1)?))
    })?;
    let mut out = Vec::new();
    for r in rows {
        let (idx, blob) = r?;
        let logs: Vec<LogData> = bincode::deserialize(&blob)?;
        let nlog = logs.len();
        let topics: u64 = logs.iter().map(|l| l.topics.len() as u64).sum();
        let datab: u64 = logs.iter().map(|l| l.data.len() as u64).sum();
        let loggas: u64 = logs
            .iter()
            .map(|l| 375 + 375 * l.topics.len() as u64 + 8 * l.data.len() as u64)
            .sum();
        let sys = logs
            .iter()
            .filter(|l| l.address == alloy::primitives::address!("0000000000000000000000000000000000001010")
                 || l.address == alloy::primitives::address!("0000000000000000000000000000000000001001"))
            .count();
        out.push((idx, nlog, topics, datab, loggas, sys));
    }
    for (idx, nlog, topics, datab, loggas, sys) in out {
        println!("tx {idx:>4} logs={nlog:>4} topics={topics:>6} datab={datab:>6} loggas={loggas:>8} sys1010/1001={sys}");
    }
    Ok(())
}
