use comfy_table::Table;
use alloy::primitives::Address;

use mev_scout_core::config::validation;
use mev_scout_core::config::Config;
use mev_scout_core::pipeline::BlockReplayStats;
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
            PoolState::Dodo(s) => s,
            PoolState::Clipper(s) => s,
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
