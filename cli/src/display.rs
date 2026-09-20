use alloy::primitives::Address;
use comfy_table::Table;

use mev_scout_core::config::validation;
use mev_scout_core::config::Config;
use mev_scout_core::pipeline::BlockReplayStats;
use mev_scout_core::pool::state::PoolManager;
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

/// Persist run/live results into the explorer store's `opportunities` table
/// (the results layer feeds offline analysis of rejected candidates). The
/// execution history lives only in SQLite; failures here warn only.
pub fn persist_opportunities_to_explorer(
    config: &Config,
    chain: mev_scout_core::types::ChainName,
    run_id: &str,
    results_file: &ResultsFile,
) {
    mev_scout_core::explorer::persist_opportunities_to_explorer(config, chain, run_id, results_file)
}

/// Persist drained runner rejections into the explorer store.
/// Only called when `--record-rejections` is enabled.
pub fn persist_rejections_to_explorer(
    config: &Config,
    chain: mev_scout_core::types::ChainName,
    run_id: &str,
    rejections: &[mev_scout_core::explorer::RejectedCandidate],
) {
    mev_scout_core::explorer::persist_rejections_to_explorer(config, chain, run_id, rejections)
}

fn pool_name(pm: &PoolManager, addr: &Address) -> String {
    pm.get(addr)
        .map(|ps| {
            let info = ps.info();
            if let Some(ref tokens) = info.underlying_tokens {
                if tokens.len() > 2 {
                    let syms: Vec<String> = tokens.iter().map(|t| t.to_string()).collect();
                    return format!("{} ({})", info.address, syms.join("/"));
                }
            }
            if let Some(ref name) = info.name {
                return name.to_string();
            }
            if let (Some(t0), Some(t1)) = (&info.token0_symbol, &info.token1_symbol) {
                let dex = info
                    .dex_name
                    .as_deref()
                    .map(String::from)
                    .unwrap_or(info.dex_type.to_string());
                return format!("{dex} {}/{}", t0, t1);
            }
            addr.to_string()
        })
        .unwrap_or_else(|| addr.to_string())
}

pub fn render_results_table(
    all_opportunities: &[mev_scout_core::types::MevOpportunity],
    pool_manager: Option<&PoolManager>,
) {
    let mut table = Table::new();
    let has_confidence = all_opportunities.iter().any(|opp| opp.confidence.is_some());

    if let Some(pm) = pool_manager {
        let mut headers = vec![
            "Block",
            "Tx",
            "Strategy",
            "Pool A / Pool B",
            "Input",
            "Profit (token_out)",
            "Gas (wei)",
        ];
        if has_confidence {
            headers.push("Confidence");
        }
        table.set_header(headers);

        for opp in all_opportunities {
            let name_a = pool_name(pm, &opp.pool_a);
            let name_b = if opp.pool_b == Address::ZERO {
                String::new()
            } else {
                pool_name(pm, &opp.pool_b)
            };
            let mut row = vec![
                opp.block_number.to_string(),
                opp.tx_index.to_string(),
                opp.strategy.to_string(),
                if name_b.is_empty() {
                    name_a
                } else {
                    format!("{} / {}", name_a, name_b)
                },
                opp.input_amount.to_string(),
                opp.expected_profit.to_string(),
                opp.gas_cost_wei.to_string(),
            ];
            if has_confidence {
                row.push(
                    opp.confidence
                        .map_or("-".to_string(), |c| format!("{:.2}", c)),
                );
            }
            table.add_row(row);
        }
    } else {
        let mut headers = vec![
            "Block",
            "Tx",
            "Strategy",
            "Input",
            "Profit (token_out)",
            "Gas (wei)",
        ];
        if has_confidence {
            headers.push("Confidence");
        }
        table.set_header(headers);

        for opp in all_opportunities {
            let mut row = vec![
                opp.block_number.to_string(),
                opp.tx_index.to_string(),
                opp.strategy.to_string(),
                opp.input_amount.to_string(),
                opp.expected_profit.to_string(),
                opp.gas_cost_wei.to_string(),
            ];
            if has_confidence {
                row.push(
                    opp.confidence
                        .map_or("-".to_string(), |c| format!("{:.2}", c)),
                );
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
                s.block_number.to_string(),
                s.total_tx_count.to_string(),
                s.dex_tx_count.to_string(),
                s.pending_tx_count.to_string(),
            ]);
        } else {
            table.add_row(vec![
                s.block_number.to_string(),
                s.total_tx_count.to_string(),
                s.dex_tx_count.to_string(),
            ]);
        }
    }
    if has_pending {
        table.add_row(vec![
            "Total".to_string(),
            total_tx.to_string(),
            total_dex.to_string(),
            total_pending.to_string(),
        ]);
    } else {
        table.add_row(vec![
            "Total".to_string(),
            total_tx.to_string(),
            total_dex.to_string(),
        ]);
    }
    println!("\nBlock Summary");
    println!("{table}");
}
