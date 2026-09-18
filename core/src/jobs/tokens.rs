use anyhow::Context;

use crate::cache::{
    enrich_from_coingecko, enrich_from_llama, CachedToken, SqliteStore, TokenCache,
};
use crate::config::validation;
use crate::config::Config;
use crate::progress::JobProgress;
use alloy::primitives::Address;

#[derive(Debug, Clone, Default)]
pub struct TokensOpts {
    pub symbol: Option<String>,
    pub decimals: Option<u64>,
    pub limit: usize,
    /// Skip detailed listing in progress logs (cache warm / count only).
    pub cache_only: bool,
    /// Fetch missing symbol/decimals (DefiLlama) and name/icon (CoinGecko).
    pub enrich: bool,
}

pub struct TokenEntry {
    pub address: String,
    pub symbol: String,
    pub decimals: Option<i32>,
    pub name: Option<String>,
    pub icon_url: Option<String>,
}

pub struct TokensOutcome {
    pub entries: Vec<TokenEntry>,
}

/// Cap CoinGecko contract lookups — free tier is slow / rate-limited.
const COINGECKO_ENRICH_CAP: usize = 50;

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

    if opts.enrich {
        seed_from_discovered_pools(&cache, &mut token_cache);
        run_remote_enrichment(chain_name, &mut token_cache, progress).await;
        if let Err(e) = token_cache.persist_all(&cache) {
            tracing::warn!("Failed to persist enriched token cache: {e:#}");
        } else {
            progress.log(&format!(
                "  Persisted {} token row(s) after enrich",
                token_cache.len()
            ));
        }
    }

    let mut entries: Vec<TokenEntry> = token_cache
        .entries()
        .iter()
        .filter(|(_, meta)| !meta.symbol.is_empty())
        .map(|(addr, meta)| TokenEntry {
            address: format!("{addr}"),
            symbol: meta.symbol.clone(),
            decimals: meta.decimals,
            name: meta.name.clone(),
            icon_url: meta.icon_url.clone(),
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
    let limit = if opts.limit == 0 {
        entries.len()
    } else {
        opts.limit
    };
    entries.truncate(limit);

    progress.log(&format!("  {} token(s) found", entries.len()));
    if !opts.cache_only && !entries.is_empty() {
        let max = entries.len().min(50);
        for e in entries.iter().take(max) {
            progress.log(&format!(
                "    {}  {}  {:?}  {}  {}",
                e.address,
                e.symbol,
                e.decimals.map(|n| n.to_string()).unwrap_or_default(),
                e.name.as_deref().unwrap_or("-"),
                e.icon_url.as_deref().unwrap_or("-"),
            ));
        }
        if entries.len() > max {
            progress.log(&format!("    … and {} more", entries.len() - max));
        }
    }

    Ok(TokensOutcome { entries })
}

fn seed_from_discovered_pools(store: &SqliteStore, token_cache: &mut TokenCache) {
    let Ok(pools) = store.list_discovered_pools() else {
        return;
    };
    let mut seeded = 0usize;
    for pool in pools {
        for addr in [pool.token0, pool.token1] {
            if addr.is_zero() || token_cache.contains(&addr) {
                continue;
            }
            token_cache.upsert_meta(addr, CachedToken::symbol_only(String::new(), None));
            seeded += 1;
        }
        if let Some(ref under) = pool.underlying_tokens {
            for &addr in under {
                if addr.is_zero() || token_cache.contains(&addr) {
                    continue;
                }
                token_cache.upsert_meta(addr, CachedToken::symbol_only(String::new(), None));
                seeded += 1;
            }
        }
    }
    if seeded > 0 {
        tracing::info!("Token cache: seeded {seeded} address(es) from discovered pools");
    }
}

async fn run_remote_enrichment(
    chain_name: crate::types::ChainName,
    token_cache: &mut TokenCache,
    progress: &dyn JobProgress,
) {
    let llama_targets: Vec<Address> = token_cache
        .entries()
        .iter()
        .filter(|(_, m)| m.needs_llama_enrichment())
        .map(|(a, _)| *a)
        .collect();
    if !llama_targets.is_empty() {
        progress.log(&format!(
            "  DefiLlama coins: enriching {} address(es)…",
            llama_targets.len()
        ));
        let llama = enrich_from_llama(chain_name, &llama_targets).await;
        progress.log(&format!("  DefiLlama coins: updated {}", llama.len()));
        for (addr, meta) in llama {
            token_cache.upsert_meta(addr, meta);
        }
    }

    let mut cg_targets: Vec<Address> = token_cache
        .entries()
        .iter()
        .filter(|(_, m)| m.needs_coingecko_enrichment())
        .map(|(a, _)| *a)
        .collect();
    cg_targets.sort();
    cg_targets.truncate(COINGECKO_ENRICH_CAP);
    if !cg_targets.is_empty() {
        progress.log(&format!(
            "  CoinGecko contract: enriching up to {} address(es)…",
            cg_targets.len()
        ));
        let cg = enrich_from_coingecko(chain_name, &cg_targets).await;
        progress.log(&format!("  CoinGecko contract: updated {}", cg.len()));
        for (addr, meta) in cg {
            token_cache.upsert_meta(addr, meta);
        }
    }
}
