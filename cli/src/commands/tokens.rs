use anyhow::Context;
use comfy_table::Table;
use mev_scout_core::config::validation;
use mev_scout_core::config::Config;
use mev_scout_core::jobs::{job_tokens, TokensOpts};

use crate::cli::TokensArgs;
use crate::job_progress::NoopProgress;

pub async fn cmd_tokens(config: &Config, args: &TokensArgs) -> anyhow::Result<()> {
    let _ = validation::resolve_chain(config).context("failed to resolve chain")?;

    let opts = TokensOpts {
        symbol: args.symbol.clone(),
        decimals: args.decimals.map(u64::from),
        limit: args.limit,
        cache_only: args.cache_only,
        enrich: args.enrich,
    };

    let outcome = job_tokens(config, &opts, &NoopProgress).await?;
    let entries = outcome.entries;

    if args.cache_only {
        println!("  Token cache: {} tokens", entries.len());
        return Ok(());
    }

    use mev_scout_core::types::OutputFormat;
    match config.output.output {
        OutputFormat::Json => {
            let out: Vec<serde_json::Value> = entries
                .iter()
                .map(|e| {
                    serde_json::json!({
                        "address": e.address,
                        "symbol": e.symbol,
                        "decimals": e.decimals,
                        "name": e.name,
                        "icon_url": e.icon_url,
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        OutputFormat::Csv => {
            println!("address,symbol,decimals,name,icon_url");
            for e in &entries {
                let d = e.decimals.map(|n| n.to_string()).unwrap_or_default();
                let name = e.name.as_deref().unwrap_or("");
                let icon = e.icon_url.as_deref().unwrap_or("");
                // Escape commas in name/url lightly for CSV.
                let name = name.replace(',', " ");
                let icon = icon.replace(',', " ");
                println!("{},{},{},{},{}", e.address, e.symbol, d, name, icon);
            }
        }
        OutputFormat::Table => {
            let mut table = Table::new();
            table.set_header(vec!["#", "Address", "Symbol", "Decimals", "Name", "Icon"]);
            for (i, e) in entries.iter().enumerate() {
                let d = e
                    .decimals
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "-".to_string());
                let name = e.name.as_deref().unwrap_or("-").to_string();
                let icon = e
                    .icon_url
                    .as_deref()
                    .map(|u| {
                        if u.len() > 40 {
                            format!("{}…", &u[..37])
                        } else {
                            u.to_string()
                        }
                    })
                    .unwrap_or_else(|| "-".to_string());
                table.add_row(vec![
                    (i + 1).to_string(),
                    e.address.clone(),
                    e.symbol.clone(),
                    d,
                    name,
                    icon,
                ]);
            }
            println!("  {table}");
            println!();
            println!("  {} token(s) found", entries.len());
        }
    }

    Ok(())
}
