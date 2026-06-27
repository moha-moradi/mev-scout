use crate::cli::ReportArgs;
use crate::display::render_results_table;
use mev_scout_core::config::Config;
use mev_scout_core::types::{OutputFormat, ResultsFile};

pub async fn cmd_report(_config: &Config, args: &ReportArgs) -> anyhow::Result<()> {
    let export_path = args.export_path.as_str();
    let dir = std::path::Path::new(export_path);

    let run_id = match &args.run_id {
        Some(id) => id.clone(),
        None => {
            if !dir.exists() {
                anyhow::bail!("Error: export directory '{}' does not exist.", export_path);
            }
            let entries = match std::fs::read_dir(dir) {
                Ok(entries) => entries,
                Err(e) => anyhow::bail!("Error reading export directory: {}", e),
            };
            let mut entries: Vec<_> = entries
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.path().extension().map(|ext| ext == "json").unwrap_or(false)
                })
                .collect();
            entries.sort_by_key(|e| e.path().metadata().ok().and_then(|m| m.created().ok()));
            match entries.last() {
                Some(entry) => {
                    let stem = entry.path().file_stem().expect("File path has no stem").to_string_lossy().to_string();
                    stem
                }
                None => anyhow::bail!("No results files found in '{}'", export_path),
            }
        }
    };

    let path = dir.join(format!("{}.json", run_id));
    if !path.exists() {
        anyhow::bail!("Error: results file not found: {}", path.display());
    }

    let json_str = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("Failed to read '{}': {}", path.display(), e))?;
    let results_file: ResultsFile = serde_json::from_str(&json_str)
        .map_err(|e| anyhow::anyhow!("Failed to parse '{}': {}", path.display(), e))?;

    let output_format: OutputFormat = args.output.parse().unwrap_or(OutputFormat::Table);

    match output_format {
        OutputFormat::Table => {
            println!();
            println!("  Run ID:        {}", results_file.run_id);
            println!("  Chain:         {}", results_file.chain);
            println!("  Block range:   {}–{}", results_file.start_block, results_file.end_block);
            println!("  Mode:          {}", results_file.range_mode);
            println!("  Strategies:    {}", results_file.strategies.join(", "));
            println!("  Flash loan:    {}", results_file.flash_loan_provider);
            println!("  Opportunities: {}", results_file.opportunities.len());
            println!();

            if results_file.opportunities.is_empty() {
                println!("No MEV opportunities in this run.");
            } else {
                render_results_table(&results_file.opportunities, None);
            }
        }
        OutputFormat::Csv => {
            println!("block_number,tx_index,strategy,input_amount,expected_profit,gas_cost_wei,confidence");
            for opp in &results_file.opportunities {
                println!(
                    "{},{},{},{},{},{},{}",
                    opp.block_number,
                    opp.tx_index,
                    opp.strategy,
                    opp.input_amount,
                    opp.expected_profit,
                    opp.gas_cost_wei,
                    opp.confidence.map_or("".to_string(), |c| format!("{:.2}", c)),
                );
            }
        }
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&results_file)?);
        }
    }

    Ok(())
}
