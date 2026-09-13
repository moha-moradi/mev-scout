use std::io::Read;
use std::path::PathBuf;

/// URL for the pre-built signature database (zstd-compressed SQLite).
const SIG_DB_URL: &str = "https://d39my35jed0oxi.cloudfront.net/mevlog-sigs-v5.db.zst";
/// Local filename for the decompressed signature database.
const SIG_DB_FILENAME: &str = "mevlog-sigs-v5.db";

/// Return the default path for the signature database.
pub fn default_sig_db_path() -> PathBuf {
    let mut path = dirs_data_dir();
    path.push(SIG_DB_FILENAME);
    path
}

/// Return the data directory for mev-scout.
fn dirs_data_dir() -> PathBuf {
    std::env::var("MEV_SCOUT_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".mev-scout")
        })
}

/// Build a comprehensive signature DB with all known DEX/MEV signatures.
/// Used as fallback when the CDN download fails.
/// Covers Uniswap V2/V3, Balancer V2, Curve, Aave V3, WETH, ERC20/721, MEV ops.
fn build_fallback_db(db_path: &PathBuf) -> anyhow::Result<()> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = rusqlite::Connection::open(db_path)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS methods (
            selector BLOB PRIMARY KEY,
            signature TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS events (
            topic BLOB PRIMARY KEY,
            signature TEXT NOT NULL
        );",
    )?;

    // Data tables live in `sigs/fallback_data.rs`; this function keeps
    // only the schema and the insert loop.
    let methods = super::fallback_data::FALLBACK_METHODS;
    let events = super::fallback_data::FALLBACK_EVENTS;

    let mut m_stmt =
        conn.prepare("INSERT OR IGNORE INTO methods (selector, signature) VALUES (?1, ?2)")?;
    for (hex, sig) in methods {
        let sel = hex::decode(hex)?;
        m_stmt.execute(rusqlite::params![sel, sig])?;
    }
    drop(m_stmt);

    let mut e_stmt =
        conn.prepare("INSERT OR IGNORE INTO events (topic, signature) VALUES (?1, ?2)")?;
    for (hex, sig) in events {
        let topic = hex::decode(hex)?;
        e_stmt.execute(rusqlite::params![topic, sig])?;
    }
    drop(e_stmt);

    tracing::info!(
        "Built fallback sig DB: {} methods, {} events",
        methods.len(),
        events.len()
    );
    Ok(())
}

/// Ensure the signature database is available, downloading + decompressing if needed.
///
/// If the file already exists at the given path, returns it immediately.
/// Otherwise tries to download the zstd-compressed archive from the CDN.
/// If the CDN download fails, builds a minimal fallback DB with well-known signatures.
pub async fn ensure_signature_db(db_path: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    let db_path = db_path.unwrap_or_else(default_sig_db_path);

    if db_path.exists() {
        return Ok(db_path);
    }

    // Try CDN download first
    match try_download_sig_db(&db_path).await {
        Ok(()) => {
            tracing::info!("Signature database cached at {}", db_path.display());
            return Ok(db_path);
        }
        Err(e) => {
            tracing::warn!("CDN download failed: {e} — building fallback sig DB");
        }
    }

    // Fallback: build minimal DB locally
    build_fallback_db(&db_path)?;
    Ok(db_path)
}

async fn try_download_sig_db(db_path: &PathBuf) -> anyhow::Result<()> {
    tracing::info!("Downloading signature database from {SIG_DB_URL}...");
    let resp = reqwest::get(SIG_DB_URL).await?;
    let compressed_bytes = resp.bytes().await?;
    tracing::info!(
        "Downloaded {} bytes (zstd compressed)",
        compressed_bytes.len()
    );

    let mut decoder = ruzstd::StreamingDecoder::new(&compressed_bytes[..])?;
    let mut decompressed = Vec::new();
    decoder.read_to_end(&mut decompressed)?;
    tracing::info!("Decompressed to {} bytes", decompressed.len());

    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(db_path, &decompressed)?;
    Ok(())
}
