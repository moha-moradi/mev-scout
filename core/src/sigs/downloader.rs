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
    let base = std::env::var("MEV_SCOUT_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".mev-scout")
        });
    base
}

/// Ensure the signature database is available, downloading + decompressing if needed.
///
/// If the file already exists at the given path, returns it immediately.
/// Otherwise downloads the zstd-compressed archive from the CDN, decompresses it,
/// and caches the result.
pub async fn ensure_signature_db(db_path: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    let db_path = db_path.unwrap_or_else(default_sig_db_path);

    if db_path.exists() {
        return Ok(db_path);
    }

    tracing::info!("Downloading signature database from {SIG_DB_URL}...");
    let resp = reqwest::get(SIG_DB_URL).await?;
    let compressed_bytes = resp.bytes().await?;
    tracing::info!("Downloaded {} bytes (zstd compressed)", compressed_bytes.len());

    // Decompress zstd stream
    let mut decoder = ruzstd::StreamingDecoder::new(&compressed_bytes[..])?;
    let mut decompressed = Vec::new();
    decoder.read_to_end(&mut decompressed)?;
    tracing::info!("Decompressed to {} bytes", decompressed.len());

    // Write to disk
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&db_path, &decompressed)?;

    tracing::info!("Signature database cached at {}", db_path.display());
    Ok(db_path)
}
