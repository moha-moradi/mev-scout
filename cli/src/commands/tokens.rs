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
    };

    let outcome = job_tokens(
        config,
        &opts,
        &NoopProgress,
    )
    .await?;

    let entries: Vec<(String, String, Option<i32>)> = outcome
        .entries
        .into_iter()
        .map(|e| (e.address, e.symbol, e.decimals))
        .collect();

    if args.cache_only {
        println!("  Token cache: {} tokens", entries.len());
        return Ok(());
    }

    use mev_scout_core::types::OutputFormat;
    match config.output.output {
        OutputFormat::Json => {
            let out: Vec<serde_json::Value> = entries
                .iter()
                .map(|(addr, symbol, dec)| {
                    serde_json::json!({
                        "address": addr,
                        "symbol": symbol,
                        "decimals": dec,
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        OutputFormat::Csv => {
            println!("address,symbol,decimals");
            for (addr, symbol, dec) in &entries {
                let d = dec.map(|n| n.to_string()).unwrap_or_default();
                println!("{addr},{symbol},{d}");
            }
        }
        OutputFormat::Table => {
            let mut table = Table::new();
            table.set_header(vec!["#", "Address", "Symbol", "Decimals"]);
            for (i, (addr, symbol, dec)) in entries.iter().enumerate() {
                let d = dec
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "-".to_string());
                table.add_row(vec![(i + 1).to_string(), addr.clone(), symbol.clone(), d]);
            }
            println!("  {table}");
            println!();
            println!("  {} token(s) found", entries.len());
        }
    }

    Ok(())
}
