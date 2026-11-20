//! Scratch read-only inspector for the scanner cache (pools + token symbols).
//! Usage: cargo run -p mev-scout-cli --example cacheq -- <db> [hex-token...]
#![allow(clippy::expect_used, clippy::unwrap_used)]
use rusqlite::Connection;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let db = args.next().expect("usage: cacheq <db> [hex-token...]");
    let db_arg = db.clone();
    let clear = std::env::args().any(|a| a == "--clear-pools");
    let conn = if clear {
        let c = Connection::open(&db_arg)?;
        let _ = c.execute("DELETE FROM pool_dex", []);
        c
    } else {
        Connection::open_with_flags(db_arg, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?
    };
    let pools: i64 = conn.query_row("SELECT COUNT(*) FROM pool_info", [], |r| r.get(0))?;
    let named: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pool_info WHERE dex_name IS NOT NULL AND dex_name <> ''",
        [],
        |r| r.get(0),
    )?;
    let tokens: i64 = conn.query_row("SELECT COUNT(*) FROM token_symbols", [], |r| r.get(0))?;
    let enriched: i64 = conn.query_row(
        "SELECT COUNT(*) FROM token_symbols WHERE name IS NOT NULL OR icon_url IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    println!("pools={pools} pools_with_dex_name={named} tokens={tokens} enriched={enriched}");
    let mut stmt = conn.prepare(
        "SELECT dex_name, COUNT(*) FROM pool_info WHERE dex_name IS NOT NULL GROUP BY dex_name ORDER BY 2 DESC LIMIT 20",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        println!("dex {:?} x {}", r.get::<_, String>(0)?, r.get::<_, i64>(1)?);
    }
    for tok in args {
        let raw = tok.trim_start_matches("0x").to_ascii_lowercase();
        if raw.len() != 40 {
            continue;
        }
        let bytes = (0..20)
            .map(|i| u8::from_str_radix(&raw[i * 2..i * 2 + 2], 16))
            .collect::<Result<Vec<u8>, _>>()?;
        let row = conn.query_row(
            "SELECT symbol, decimals, name, icon_url FROM token_symbols WHERE address = ?1",
            [&bytes],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<i32>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            },
        );
        println!("token 0x{raw}: {row:?}");
    }
    Ok(())
}
