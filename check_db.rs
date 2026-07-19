use rusqlite::Connection;
fn main() {
    let db = Connection::open_with_flags("D:\\gitlab.dte.repo\\mev-scout\\cache\\polygon-mev-scout.sqlite", rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let count: i64 = db.query_row("SELECT COUNT(*) FROM blocks", [], |r| r.get(0)).unwrap();
    let min_max: (i64, i64) = db.query_row("SELECT MIN(number), MAX(number) FROM blocks", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    println!("Total blocks: {count}, Range: {} - {}", min_max.0, min_max.1);
}
