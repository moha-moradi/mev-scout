use anyhow::Context;

use crate::cache::{SqliteStore, TokenCache};
use crate::config::validation;
use crate::config::Config;
use crate::progress::JobProgress;

#[derive(Debug, Clone, Default)]
pub struct TokensOpts {
    pub symbol: Option<String>,
    pub decimals: Option<u64>,
    pub limit: usize,
}

pub struct TokenEntry {
    pub address: String,
    pub symbol: String,
    pub decimals: Option<i32>,
}

pub struct TokensOutcome {
    pub entries: Vec<TokenEntry>,
}

pub async fn job_tokens(
    config: &Config,
    opts: &TokensOpts,
    progress: &dyn JobProgress,
) -> anyhow::Result<TokensOutcome> {
    let (chain_name, _) = validation::resolve_chain(config).context("failed to resolve chain")?;
    let chain_id = chain_name.chain_id();

    let cache = SqliteStore::open(config.effective_db_path(&chain_name))?;
    let mut token_cache = TokenCache::warm(chain_id);
    match TokenCache::load(&cache) {
        Ok(persisted) => token_cache.merge(persisted),
        Err(e) => tracing::warn!("Failed to load token cache from SQLite: {e:#}"),
    }

    let mut entries: Vec<TokenEntry> = token_cache
        .entries()
        .iter()
        .map(|(addr, (symbol, decimals))| TokenEntry {
            address: format!("{addr}"),
            symbol: symbol.clone(),
            decimals: *decimals,
        })
        .collect();

    if let Some(pattern) = &opts.symbol {
        let pat = pattern.to_lowercase();
        entries.retain(|e| e.symbol.to_lowercase().contains(&pat));
    }
    if let Some(dec) = opts.decimals {
        entries.retain(|e| e.decimals == Some(dec as i32));
    }

    entries.sort_by(|a, b| a.address.cmp(&b.address));
    let limit = if opts.limit == 0 { entries.len() } else { opts.limit };
    entries.truncate(limit);

    progress.log(&format!("  {} token(s) found", entries.len()));
    if !entries.is_empty() {
        let max = entries.len().min(50);
        for e in entries.iter().take(max) {
            progress.log(&format!(
                "    {}  {}  {:?}",
                e.address,
                e.symbol,
                e.decimals.map(|n| n.to_string()).unwrap_or_default()
            ));
        }
        if entries.len() > max {
            progress.log(&format!("    … and {} more", entries.len() - max));
        }
    }

    Ok(TokensOutcome { entries })
}
