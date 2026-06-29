use comfy_table::Table;
use alloy::primitives::Address;

use mev_scout_core::config::validation;
use mev_scout_core::config::Config;
use mev_scout_core::mev::competition::CompetitionReport;
use mev_scout_core::mev::verify::BlockReplayStats;
use mev_scout_core::pool::state::{PoolManager, PoolState};
use mev_scout_core::types::ResultsFile;

pub fn print_startup_plan(result: &validation::ValidationResult, config: &Config) {
    let divider = "═".repeat(55);

    println!();
    println!("  ╔{divider}╗");
    println!("  ║        MEV Backtest Engine — Startup Plan        ║");
    println!("  ╚{divider}╝");
    println!();

    let plan = config.plan_summary(
        result.chain_name,
        &result.chain_config,
        &result.range_mode,
        &result.strategies,
        result.flash_loan_provider,
    );

    for line in plan.lines() {
        println!("  {line}");
    }

    println!("  [DRY RUN — no simulation yet]");
    println!();
}

pub fn save_results_json(
    export_path: &str,
    run_id: &str,
    results_file: &ResultsFile,
) -> anyhow::Result<()> {
    let dir = std::path::Path::new(export_path);
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}.json", run_id));
    let json = serde_json::to_string_pretty(results_file)?;
    std::fs::write(&path, json)?;
    println!("Results saved to {}", path.display());
    Ok(())
}

fn pool_name(pm: &PoolManager, addr: &Address) -> String {
    pm.get(addr)
        .map(|ps| match ps {
            PoolState::UniswapV2(s) => &s.info,
            PoolState::UniswapV3(s) => &s.info,
            PoolState::Curve(s) => &s.info,
            PoolState::Balancer(s) => &s.info,
        })
        .and_then(|info| info.name.clone())
        .unwrap_or_else(|| format!("{}", addr))
}

pub fn render_results_table(all_opportunities: &[mev_scout_core::types::MevOpportunity], pool_manager: Option<&PoolManager>) {
    let mut table = Table::new();
    let has_confidence = all_opportunities.iter().any(|opp| opp.confidence.is_some());

    if pool_manager.is_some() {
        let mut headers = vec![
            "Block", "Tx", "Strategy", "Pool A / Pool B",
            "Input", "Profit (token_out)", "Gas (wei)",
        ];
        if has_confidence {
            headers.push("Confidence");
        }
        table.set_header(headers);

        for opp in all_opportunities {
            let pm = pool_manager.expect("pool_manager is_some() checked above");
            let name_a = pool_name(pm, &opp.pool_a);
            let name_b = if opp.pool_b == Address::ZERO {
                String::new()
            } else {
                pool_name(pm, &opp.pool_b)
            };
            let mut row = vec![
                format!("{}", opp.block_number),
                format!("{}", opp.tx_index),
                format!("{}", opp.strategy),
                if name_b.is_empty() { name_a } else { format!("{} / {}", name_a, name_b) },
                format!("{}", opp.input_amount),
                format!("{}", opp.expected_profit),
                format!("{}", opp.gas_cost_wei),
            ];
            if has_confidence {
                row.push(opp.confidence.map_or("-".to_string(), |c| format!("{:.2}", c)));
            }
            table.add_row(row);
        }
    } else {
        let mut headers = vec![
            "Block", "Tx", "Strategy",
            "Input", "Profit (token_out)", "Gas (wei)",
        ];
        if has_confidence {
            headers.push("Confidence");
        }
        table.set_header(headers);

        for opp in all_opportunities {
            let mut row = vec![
                format!("{}", opp.block_number),
                format!("{}", opp.tx_index),
                format!("{}", opp.strategy),
                format!("{}", opp.input_amount),
                format!("{}", opp.expected_profit),
                format!("{}", opp.gas_cost_wei),
            ];
            if has_confidence {
                row.push(opp.confidence.map_or("-".to_string(), |c| format!("{:.2}", c)));
            }
            table.add_row(row);
        }
    }

    println!("{table}");
}

pub fn render_block_summary_table(summaries: &[BlockReplayStats]) {
    if summaries.len() <= 1 {
        return;
    }
    let mut table = Table::new();
    let has_pending = summaries.iter().any(|s| s.pending_tx_count > 0);
    if has_pending {
        table.set_header(vec!["Block", "Txs", "DEX txs", "Pending"]);
    } else {
        table.set_header(vec!["Block", "Txs", "DEX txs"]);
    }
    let mut total_tx = 0usize;
    let mut total_dex = 0usize;
    let mut total_pending = 0usize;
    for s in summaries {
        total_tx += s.total_tx_count;
        total_dex += s.dex_tx_count;
        total_pending += s.pending_tx_count;
        if has_pending {
            table.add_row(vec![
                format!("{}", s.block_number),
                format!("{}", s.total_tx_count),
                format!("{}", s.dex_tx_count),
                format!("{}", s.pending_tx_count),
            ]);
        } else {
            table.add_row(vec![
                format!("{}", s.block_number),
                format!("{}", s.total_tx_count),
                format!("{}", s.dex_tx_count),
            ]);
        }
    }
    if has_pending {
        table.add_row(vec![
            format!("{}", "Total"),
            format!("{}", total_tx),
            format!("{}", total_dex),
            format!("{}", total_pending),
        ]);
    } else {
        table.add_row(vec![
            format!("{}", "Total"),
            format!("{}", total_tx),
            format!("{}", total_dex),
        ]);
    }
    println!("\nBlock Summary");
    println!("{table}");
}

fn extraction_type_label(et: &mev_scout_core::mev::competition::ExtractionType) -> &'static str {
    use mev_scout_core::mev::competition::ExtractionType;
    match et {
        ExtractionType::TwoHopArb => "arb",
        ExtractionType::MultiHopArb => "marb",
        ExtractionType::Jit => "jit",
        ExtractionType::JitArb => "jitarb",
        ExtractionType::Sandwich => "sw",
        ExtractionType::Liquidation => "liq",
        ExtractionType::UnknownMev => "?",
    }
}

pub fn render_competition_table(report: &CompetitionReport) {
    if report.total_extractions == 0 {
        return;
    }

    println!("\nCompetitor Activity");
    println!("  Total searchers found: {}", report.total_searchers_found);
    println!("  Total extractions: {}", report.total_extractions);

    if !report.by_strategy.is_empty() {
        println!("  By strategy:");
        let mut strategies: Vec<_> = report.by_strategy.iter().collect();
        strategies.sort_by(|a, b| b.1.cmp(a.1));
        for (strategy, count) in strategies {
            println!("    {}: {}", strategy, count);
        }
    }

    // Per-block competitor activity table
    if !report.per_block.is_empty() {
        let mut block_table = Table::new();
        block_table.set_header(vec!["Block", "Txs", "Searchers", "Arbs", "S.Wich", "Liq"]);
        for bc in &report.per_block {
            let mut arb_count = 0usize;
            let mut sw_count = 0usize;
            let mut liq_count = 0usize;
            for ext in &bc.extractions {
                match ext.extraction_type {
                    mev_scout_core::mev::competition::ExtractionType::TwoHopArb
                    | mev_scout_core::mev::competition::ExtractionType::MultiHopArb => arb_count += 1,
                    mev_scout_core::mev::competition::ExtractionType::Sandwich => sw_count += 1,
                    mev_scout_core::mev::competition::ExtractionType::Liquidation => liq_count += 1,
                    _ => {}
                }
            }
            block_table.add_row(vec![
                format!("{}", bc.block_number),
                format!("{}", bc.total_tx_count),
                format!("{}", bc.unique_searchers),
                format!("{}", arb_count),
                format!("{}", sw_count),
                format!("{}", liq_count),
            ]);
        }
        println!("\n  Per-block activity:");
        println!("{block_table}");
    }

    if !report.top_searchers.is_empty() {
        let mut table = Table::new();
        table.set_header(vec![
            "Searcher",
            "Extractions",
            "Gas Paid (wei)",
            "Gross Profit (wei)",
            "Avg Priority Fee (wei)",
            "Strategies",
        ]);
        for profile in report.top_searchers.iter().take(10) {
            let strategies: Vec<&str> = profile.by_strategy
                .keys()
                .map(|et| extraction_type_label(et))
                .collect();
            table.add_row(vec![
                format!("{:#x}", profile.searcher),
                format!("{}", profile.total_extractions),
                format!("{}", profile.total_gas_spent_wei),
                format!("{}", profile.total_gross_profit_wei),
                format!("{}", profile.avg_priority_fee_wei),
                strategies.join(","),
            ]);
        }
        println!("{table}");
    }

    // PGA calibration summary
    let cal = &report.pga_calibration;
    if !cal.mean_competitors.is_empty() {
        println!("\nPGA Calibration (from observed data):");
        for (strategy, mean) in &cal.mean_competitors {
            let intensity = cal.bid_to_value_ratio.get(strategy)
                .copied()
                .unwrap_or(0.5);
            println!(
                "  {}: mean_competitors={:.2}, intensity={:.3} (blocks={}, extractions={})",
                strategy, mean, intensity, cal.blocks_analyzed, cal.total_extractions,
            );
        }
    }
}
