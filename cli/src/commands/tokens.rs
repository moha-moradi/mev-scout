use comfy_table::Table;
use mev_scout_core::cache::{SqliteStore, TokenCache};
use mev_scout_core::config::validation;
use mev_scout_core::config::Config;
use mev_scout_core::dune::client::DuneClient;
use mev_scout_core::dune::token_discovery::{self, TokenFilter};

use crate::cli::TokensArgs;

pub async fn cmd_tokens(config: &Config, args: &TokensArgs) -> anyhow::Result<()> {
    let (chain_name, _chain_config) = validation::resolve_chain(config)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let chain_id = chain_name.chain_id();

    // Resolve Dune API key
    let api_key = args.dune_api_key.as_deref()
        .or(config.dune.dune_api_key.as_deref())
        .ok_or_else(|| anyhow::anyhow!(
            "dune API key required (set --dune-api-key or configure dune_api_key in config)"
        ))?;

    let dune = DuneClient::new(api_key);

    // Build filter from CLI args
    let filter = match args.filter.to_lowercase().as_str() {
        "all" => TokenFilter::All,
        "active" => TokenFilter::Active { days: args.days },
        "new" | "newly-launched" | "newlylaunched" => TokenFilter::NewlyLaunched { days: args.days },
        "tvl" => TokenFilter::Tvl { days: args.days, top: args.top },
        other => anyhow::bail!(
            "unknown filter '{other}' (use: all, active, new, tvl)"
        ),
    };

    if !args.cache_only {
        println!();
        println!("  Token Discovery");
        println!("  Chain:  {}", chain_name);
        println!("  Filter: {:?}", filter);
        println!();
    }

    // Execute Dune query
    let mut tokens = token_discovery::discover_tokens(&dune, &chain_name.to_string(), &filter, args.limit).await?;

    // Apply post-filters
    if let Some(ref pattern) = args.symbol {
        token_discovery::filter_by_symbol(&mut tokens, pattern);
    }
    if let Some(dec) = args.decimals {
        token_discovery::filter_by_decimals(&mut tokens, dec);
    }
    if let Some(min_vol) = args.min_volume {
        token_discovery::filter_by_min_volume(&mut tokens, min_vol);
    }

    // Sort results
    token_discovery::sort_by_field(&mut tokens, &args.sort);

    let total = tokens.len();

    // Persist to SQLite cache (unless --no-cache)
    if !args.no_cache {
        let cache_path = config.effective_db_path(&chain_name);
        let cache = SqliteStore::open(&cache_path)?;
        let mut token_cache = TokenCache::warm(chain_id);
        match TokenCache::load(&cache) {
            Ok(persisted) => token_cache.merge(persisted),
            Err(e) => tracing::warn!("Failed to load existing token cache: {e:#}"),
        }

        let entries: Vec<_> = tokens.iter().map(|t| {
            (t.contract_address, t.symbol.clone(), Some(t.decimals as i32))
        }).collect();
        match token_cache.save_batch(&cache, &entries) {
            Ok(saved) if saved > 0 => {
                if !args.cache_only {
                    println!("  Cached {} new token symbols to SQLite", saved);
                }
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("Failed to save tokens to cache: {e:#}"),
        }
    }

    if args.cache_only {
        println!("  Token cache updated: {} tokens (filter={:?})", total, filter);
        return Ok(());
    }

    // Display results
    let show_tvl = args.filter == "tvl";
    match args.output.as_str() {
        "json" => {
            println!("{}", serde_json::to_string_pretty(&tokens)?);
        }
        "csv" => {
            if show_tvl {
                println!("address,symbol,decimals,name,trade_count,tvl_usd,volume_usd");
                for t in &tokens {
                    let name = t.name.as_deref().unwrap_or("");
                    let tc = t.trade_count.map(|n| n.to_string()).unwrap_or_default();
                    let tvl = t.tvl_usd.map(|f| format!("{:.2}", f)).unwrap_or_default();
                    let vol = t.volume_usd.map(|f| format!("{:.2}", f)).unwrap_or_default();
                    println!("{},{},{},{},{},{},{}", t.contract_address, t.symbol, t.decimals, name, tc, tvl, vol);
                }
            } else {
                println!("address,symbol,decimals,name,trade_count,volume_usd");
                for t in &tokens {
                    let name = t.name.as_deref().unwrap_or("");
                    let tc = t.trade_count.map(|n| n.to_string()).unwrap_or_default();
                    let vol = t.volume_usd.map(|f| format!("{:.2}", f)).unwrap_or_default();
                    println!("{},{},{},{},{},{}", t.contract_address, t.symbol, t.decimals, name, tc, vol);
                }
            }
        }
        _ => {
            let mut table = Table::new();
            if show_tvl {
                table.set_header(vec!["#", "Address", "Symbol", "Decimals", "Name", "Trades", "TVL (USD)", "Volume (USD)"]);
                for (i, t) in tokens.iter().enumerate() {
                    let name = t.name.as_deref().unwrap_or("-").to_string();
                    let tc = t.trade_count.map(|n| n.to_string()).unwrap_or_else(|| "-".to_string());
                    let tvl = t.tvl_usd.map(|f| format_vol(f)).unwrap_or_else(|| "-".to_string());
                    let vol = t.volume_usd.map(|f| format_vol(f)).unwrap_or_else(|| "-".to_string());
                    table.add_row(vec![
                        (i + 1).to_string(),
                        format!("{}", t.contract_address),
                        t.symbol.clone(),
                        t.decimals.to_string(),
                        name,
                        tc,
                        tvl,
                        vol,
                    ]);
                }
            } else {
                table.set_header(vec!["#", "Address", "Symbol", "Decimals", "Name", "Trades", "Volume (USD)"]);
                for (i, t) in tokens.iter().enumerate() {
                    let name = t.name.as_deref().unwrap_or("-").to_string();
                    let tc = t.trade_count.map(|n| n.to_string()).unwrap_or_else(|| "-".to_string());
                    let vol = t.volume_usd.map(|f| format_vol(f)).unwrap_or_else(|| "-".to_string());
                    table.add_row(vec![
                        (i + 1).to_string(),
                        format!("{}", t.contract_address),
                        t.symbol.clone(),
                        t.decimals.to_string(),
                        name,
                        tc,
                        vol,
                    ]);
                }
            }

            println!("  {table}");
            println!();
            println!("  {} token(s) found", total);
        }
    }

    Ok(())
}

fn format_vol(v: f64) -> String {
    if v >= 1_000_000_000.0 {
        format!("{:.1}B", v / 1_000_000_000.0)
    } else if v >= 1_000_000.0 {
        format!("{:.1}M", v / 1_000_000.0)
    } else if v >= 1_000.0 {
        format!("{:.1}K", v / 1_000.0)
    } else {
        format!("{:.2}", v)
    }
}
