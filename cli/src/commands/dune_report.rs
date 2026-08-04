use std::io::Write;

use anyhow::Context;
use crate::cli::DuneReportArgs;
use mev_scout_core::config::Config;
use mev_scout_core::dune::client::DuneClient;
use mev_scout_core::dune::report::StrategyReport;
use mev_scout_core::dune::util::{dune_indexing_lag_blocks, estimate_latest_block};

/// Generate a per-strategy MEV revenue report for a chain and block range.
pub async fn cmd_dune_report(config: &Config, args: &DuneReportArgs) -> anyhow::Result<()> {
    let api_key = args
        .dune_api_key
        .clone()
        .or_else(|| config.dune.dune_api_key.clone())
        .ok_or_else(|| anyhow::anyhow!(
            "No Dune API key. Set in mev-scout.toml (dune_api_key = \"...\") or pass --dune-api-key"
        ))?;

    // Resolve block range: explicit pair wins, otherwise derive from --days
    // backed off from the Dune indexing lag so data is guaranteed indexed.
    let (from_block, to_block) = match (args.from_block, args.to_block) {
        (Some(from), Some(to)) if from <= to => (from, to),
        _ => {
            let lag = dune_indexing_lag_blocks(&args.chain);
            let latest = estimate_latest_block(&args.chain).saturating_sub(lag);
            let to = args.to_block.unwrap_or(latest);
            let from = match args.from_block {
                Some(from) => from,
                None => {
                    let p = mev_scout_core::dune::util::chain_timing(&args.chain);
                    to.saturating_sub(args.days * p.blocks_per_day)
                }
            };
            (from, to)
        }
    };

    if from_block > to_block {
        anyhow::bail!(
            "Invalid block range: from_block {} > to_block {}",
            from_block,
            to_block
        );
    }

    eprintln!(
        "Generating MEV strategy revenue report: chain={} blocks {}–{} ({} days of data)\n",
        args.chain,
        from_block,
        to_block,
        args.days
    );

    let client = DuneClient::new(api_key);
    let report = StrategyReport::run(&client, &args.chain, from_block, to_block, args.min_profit)
        .await
        .context("Dune report generation failed")?;

    let rendered = match args.output.as_str() {
        "json" => report.render_json(),
        "html" => report.render_html(),
        _ => report.render_markdown(),
    };

    match &args.output_file {
        Some(path) => {
            let mut f = std::fs::File::create(path)
                .with_context(|| format!("Cannot create output file {path}"))?;
            f.write_all(rendered.as_bytes())?;
            println!("Report written to {}", path);
        }
        None => print!("{}", rendered),
    }

    Ok(())
}
