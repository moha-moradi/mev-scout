use anyhow::Context;
use comfy_table::Table;
use mev_scout_core::cache::{SqliteStore, TokenCache};
use mev_scout_core::config::validation;
use mev_scout_core::config::Config;

use crate::cli::TokensArgs;

pub async fn cmd_tokens(config: &Config, args: &TokensArgs) -> anyhow::Result<()> {
    let (chain_name, _chain_config) = validation::resolve_chain(config)
        .context("failed to resolve chain")?;
    let chain_id = chain_name.chain_id();

    if args.filter.to_lowercase().as_str() != "all" {
        anyhow::bail!(
            "filter '{}' is not supported in on-chain mode (use: all)",
            args.filter
        );
    }

    // ── Load token cache (SQLite + pre-populated known tokens) ──
    let cache_path = config.effective_db_path(&chain_name);
    let cache = SqliteStore::open(&cache_path)?;
    let mut token_cache = TokenCache::warm(chain_id);
    match TokenCache::load(&cache) {
        Ok(persisted) => token_cache.merge(persisted),
        Err(e) => tracing::warn!("Failed to load token cache from SQLite: {e:#}"),
    }

    // Apply post-filters
    let mut entries: Vec<(String, String, Option<i32>)> = token_cache
        .entries()
        .iter()
        .map(|(addr, (symbol, decimals))| (format!("{addr}"), symbol.clone(), *decimals))
        .collect();

    if let Some(ref pattern) = args.symbol {
        let pat = pattern.to_lowercase();
        entries.retain(|(_, sym, _)| sym.to_lowercase().contains(&pat));
    }
    if let Some(dec) = args.decimals {
        entries.retain(|(_, _, d)| *d == Some(dec as i32));
    }

    match args.sort.as_str() {
        "symbol" => entries.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase())),
        "name" | _ => entries.sort_by(|a, b| a.0.cmp(&b.0)),
    }

    entries.truncate(args.limit);

    if args.cache_only {
        println!("  Token cache: {} tokens (filter=all)", entries.len());
        return Ok(());
    }

    // ── Display results ──
    match args.output.as_str() {
        "json" => {
            let out: Vec<serde_json::Value> = entries.iter().map(|(addr, symbol, dec)| {
                serde_json::json!({
                    "address": addr,
                    "symbol": symbol,
                    "decimals": dec,
                })
            }).collect();
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        "csv" => {
            println!("address,symbol,decimals");
            for (addr, symbol, dec) in &entries {
                let d = dec.map(|n| n.to_string()).unwrap_or_default();
                println!("{addr},{symbol},{d}");
            }
        }
        _ => {
            let mut table = Table::new();
            table.set_header(vec!["#", "Address", "Symbol", "Decimals"]);
            for (i, (addr, symbol, dec)) in entries.iter().enumerate() {
                let d = dec.map(|n| n.to_string()).unwrap_or_else(|| "-".to_string());
                table.add_row(vec![
                    (i + 1).to_string(),
                    addr.clone(),
                    symbol.clone(),
                    d,
                ]);
            }
            println!("  {table}");
            println!();
            println!("  {} token(s) found", entries.len());
            println!("  (cached symbols; run 'discover' to add tokens from discovered pools)");
        }
    }

    Ok(())
}
