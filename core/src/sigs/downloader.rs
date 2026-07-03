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

/// Build a minimal signature DB with well-known common signatures.
/// Used as fallback when the CDN download fails.
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

    let methods: &[(&str, &str)] = &[
        ("a9059cbb", "transfer(address,uint256)"),
        ("23b872dd", "transferFrom(address,address,uint256)"),
        ("095ea7b3", "approve(address,uint256)"),
        ("70a08231", "balanceOf(address)"),
        ("18160ddd", "totalSupply()"),
        ("dd62ed3e", "allowance(address,address)"),
        ("313ce567", "decimals()"),
        ("06fdde03", "name()"),
        ("95d89b41", "symbol()"),
        ("d0e30db0", "deposit()"),
        ("2e1a7d4d", "withdraw(uint256)"),
        ("38ed1739", "swapExactTokensForTokens(uint256,uint256,address[],address,uint256)"),
        ("7ff36ab5", "swapExactETHForTokens(uint256,address[],address,uint256)"),
        ("4a25d94a", "swapTokensForExactTokens(uint256,uint256,address[],address,uint256)"),
        ("fb3bdb41", "swapETHForExactTokens(uint256,address[],address,uint256)"),
        ("18cbafe5", "swapExactTokensForETH(uint256,uint256,address[],address,uint256)"),
        ("5c11d795", "swapExactTokensForTokensSupportingFeeOnTransferTokens(uint256,uint256,address[],address,uint256)"),
        ("022c0d9f", "swap(uint256,uint256,address,bytes)"),
        ("128acb08", "swap(address,bool,int256,uint256,uint256,address,bytes)"),
        ("49404b7c", "exactInputSingle(tuple)"),
        ("414bf389", "exactInput(tuple)"),
        ("db3e2198", "exactOutputSingle(tuple)"),
        ("2eac270a", "exactOutput(tuple)"),
        ("0dc4bdae", "exactInputV2Swap((address,address,uint256,uint256,uint256,uint256,address[],address[],bytes,string),uint256)"),
        ("52bbbe29", "swap(tuple,address,bytes)"),
        ("7b3c1ef2", "batchSwap(tuple,address[],address,bytes)"),
        ("a6417ed6", "add_liquidity(uint256[2],uint256)"),
        ("4515cef3", "add_liquidity(uint256[3],uint256)"),
        ("3df02124", "exchange(int128,int128,uint256,uint256)"),
        ("617ba037", "supply(address,uint256,address,uint16)"),
        ("69328dec", "withdraw(address,uint256,address)"),
        ("573ade81", "borrow(address,uint256,uint256,uint16,address)"),
        ("ab9c4b5d", "repay(address,uint256,uint256,address)"),
        ("c4b5e15f", "flashLoan(address,address[],uint256[],uint256[],address,bytes,uint16)"),
        ("d505accf", "permit(address,address,uint256,uint256,uint8,bytes32,bytes32)"),
        ("e3ee160e", "transferWithAuthorization(address,address,uint256,uint256,uint256,bytes32,uint8,bytes32,bytes32)"),
        ("d286f3cf", "claimInterest(uint256,uint256)"),
        ("a694fc3a", "stake(uint256)"),
        ("1e83409a", "claim(address)"),
        ("9ebea88c", "unstake(uint256,bool)"),
        ("b88d4fde", "safeTransferFrom(address,address,uint256,bytes)"),
        ("01ffc9a7", "supportsInterface(bytes4)"),
        ("50d25bcd", "latestRoundData()"),
        ("feaf968c", "latestAnswer()"),
        ("8da5cb5b", "owner()"),
        ("715018a6", "renounceOwnership()"),
        ("f2fde38b", "transferOwnership(address)"),
        ("5c975abb", "paused()"),
        ("8456cb59", "pause()"),
        ("3f4ba83a", "unpause()"),
        ("40c10f19", "mint(address,uint256)"),
        ("42966c68", "burn(uint256)"),
        ("6352211e", "ownerOf(uint256)"),
        ("c87b56dd", "tokenURI(uint256)"),
        ("42842e0e", "safeTransferFrom(address,address,uint256)"),
        ("f242432a", "safeTransferFrom(address,address,uint256,uint256,bytes)"),
        ("6fadcf72", "forward(address,bytes)"),
        ("6a761202", "execTransaction(address,uint256,bytes,uint8,uint256,uint256,uint256,address,address,bytes)"),
        ("765e827f", "handleOps((address,uint256,bytes,bytes,bytes32,uint256,bytes32,bytes,bytes)[],address)"),
        ("405cec67", "relayCall(address,address,bytes,uint256,uint256,uint256,uint256,bytes,bytes)"),
        ("a1305b17", "relayCall(address,address,bytes,uint256,bytes,uint256,address,bytes)"),
        ("0a3c4405", "proxy((address,uint256,uint256,(address,uint256,bytes)[])[],bytes[])"),
        ("77835641", "deploy(address[],bytes32[])"),
        ("dbeccb23", "redeemPositions(bytes32,uint256[])"),
        ("5638f1f3", "redeemSilence(address,uint256)"),
        ("46a73fb1", "silence(address,uint256,uint256)"),
        ("7c025200", "swap(tuple)"),
        ("9b3b5f8f", "swap(uint16,address,uint256,uint256,address,bytes)"),
        ("3c2b4399", "matchOrders(bytes32,(uint256,address,address,uint256,uint256,uint256,uint8,uint8,uint256,bytes32,bytes32,bytes),(uint256,address,address,uint256,uint256,uint256,uint8,uint8,uint256,bytes32,bytes32,bytes)[],uint256,uint256[],uint256,uint256[])"),
        ("e2bbb158", "deposit(uint256)"),
    ];

    let events: &[(&str, &str)] = &[
        ("ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef", "Transfer(address,address,uint256)"),
        ("8c5be1e5ebec7d5bd14f71427d1e84f3dd0314c0f7b2291e5b200ac8c7c3b925", "Approval(address,address,uint256)"),
        ("e1fffcc4923d04b559f4d29a8bfc6cda04eb5b0d3c460751c2402c5c5cc9109c", "Deposit(address,uint256)"),
        ("7fcf532c15f0a6db0bd6d0e038bea71d30d808c7d98cb3bf7268a95bf5081b65", "Withdrawal(address,uint256)"),
        ("d78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822", "Swap(address,uint256,uint256,uint256,uint256,address)"),
        ("1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1", "Sync(uint112,uint112)"),
        ("4c209b5fc8ad50758f13e2e1088ba56a560dff690a1c6fef26394f4c03821c4f", "Mint(address,uint256,uint256)"),
        ("dccd412f0b1252819cb1fd330b93224ca42612892bb3f4f789976e6d81936496", "Burn(address,uint256,uint256,address)"),
        ("c42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67", "Swap(address,address,int256,int256,uint160,uint128,int24)"),
        ("17307eab39ab6107e8899845ad3d59bd9653f200f220920489ca2b5937696c31", "ApprovalForAll(address,address,bool)"),
        ("bc7cd75a20ee27fd9adebab32041f755214dbc6bffa90cc0225b39da2e5c2d3b", "Upgraded(address)"),
        ("7e644d79422f17c01e4894b5f4f588d331ebfa28653d42ae832dc59e38c9798f", "AdminChanged(address,address)"),
    ];

    let mut m_stmt = conn.prepare("INSERT OR IGNORE INTO methods (selector, signature) VALUES (?1, ?2)")?;
    for (hex, sig) in methods {
        let sel = hex::decode(hex)?;
        m_stmt.execute(rusqlite::params![sel, sig])?;
    }
    drop(m_stmt);

    let mut e_stmt = conn.prepare("INSERT OR IGNORE INTO events (topic, signature) VALUES (?1, ?2)")?;
    for (hex, sig) in events {
        let topic = hex::decode(hex)?;
        e_stmt.execute(rusqlite::params![topic, sig])?;
    }
    drop(e_stmt);

    tracing::info!("Built fallback sig DB: {} methods, {} events", methods.len(), events.len());
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
    tracing::info!("Downloaded {} bytes (zstd compressed)", compressed_bytes.len());

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
