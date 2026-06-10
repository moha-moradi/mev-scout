use crate::config::ChainConfig;
use crate::mev::opportunity::MevOpportunity;
use crate::types::Strategy;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SummaryMetrics {
    pub total: usize,
    pub profitable: usize,
    pub gross_revenue: f64,
    pub net_profit: f64,
    pub net_profit_usd: f64,
    pub total_cost: f64,
    pub best_strategy: Option<String>,
    pub best_single_opp: f64,
    pub gross_revenue_wei: u128,
    pub net_profit_wei: i128,
    pub total_gas_cost_wei: u128,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StrategyMetrics {
    pub strategy: String,
    pub count: usize,
    pub profitable: usize,
    pub gross_revenue: f64,
    pub gas_fees: f64,
    pub net_profit: f64,
    pub net_profit_usd: f64,
    pub roi: f64,
    pub avg_per_opp: f64,
    pub best_opp: f64,
    pub gross_revenue_wei: u128,
    pub net_profit_wei: i128,
    pub total_gas_cost_wei: u128,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DexMetrics {
    pub dex: String,
    pub fork: String,
    pub tx_count: usize,
    pub opportunities: usize,
    pub profitable: usize,
    pub revenue: f64,
    pub avg_profit: f64,
    pub gross_revenue_wei: u128,
    pub net_profit_wei: i128,
    pub total_gas_cost_wei: u128,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AggregationResult {
    pub summary: SummaryMetrics,
    pub by_strategy: std::collections::HashMap<String, StrategyMetrics>,
    pub by_dex: Vec<DexMetrics>,
}

pub struct DexMeta {
    pub name: String,
    pub fork: String,
    pub tx_count: usize,
}

const WEI_PER_ETH: f64 = 1_000_000_000_000_000_000.0;

fn wei_to_eth(wei: u128) -> f64 {
    wei as f64 / WEI_PER_ETH
}

fn ui_strategy_name(strategy: Strategy) -> &'static str {
    match strategy {
        Strategy::TwoHopArb | Strategy::MultiHopArb => "arb",
        Strategy::Jit => "jit",
        Strategy::JitArb => "jitarb",
        Strategy::Sandwich => "sandwich",
    }
}

pub fn aggregate(
    opportunities: &[MevOpportunity],
    _chain: &ChainConfig,
    dexes: &[DexMeta],
) -> AggregationResult {
    let mut by_strategy: std::collections::HashMap<String, Vec<&MevOpportunity>> =
        std::collections::HashMap::new();
    let mut by_dex: std::collections::HashMap<String, Vec<&MevOpportunity>> =
        std::collections::HashMap::new();

    for opp in opportunities {
        let sname = ui_strategy_name(opp.strategy).to_string();
        by_strategy.entry(sname).or_default().push(opp);

        for dex_meta in dexes {
            by_dex.entry(dex_meta.name.clone()).or_default().push(opp);
        }
    }

    let total = opportunities.len();
    let gross_revenue: f64 = opportunities
        .iter()
        .map(|o| wei_to_eth(o.expected_profit.to::<u128>()))
        .sum();
    let total_gas: f64 = opportunities
        .iter()
        .map(|o| wei_to_eth(o.gas_cost_wei))
        .sum();
    let net_profit = gross_revenue - total_gas;

    let profitable_count = opportunities
        .iter()
        .filter(|o| {
            let profit = wei_to_eth(o.expected_profit.to::<u128>()) - wei_to_eth(o.gas_cost_wei);
            profit > 0.0
        })
        .count();

    let best_single_opp = opportunities
        .iter()
        .map(|o| wei_to_eth(o.expected_profit.to::<u128>()))
        .fold(0.0_f64, f64::max);

    let mut best_strategy: Option<String> = None;
    let mut best_strat_net = 0.0_f64;
    let mut strategy_metrics = std::collections::HashMap::new();

    for (sname, opps) in &by_strategy {
        let count = opps.len();
        let strat_gross: f64 = opps.iter().map(|o| wei_to_eth(o.expected_profit.to::<u128>())).sum();
        let strat_gas: f64 = opps.iter().map(|o| wei_to_eth(o.gas_cost_wei)).sum();
        let strat_net = strat_gross - strat_gas;
        let strat_profitable = opps
            .iter()
            .filter(|o| {
                wei_to_eth(o.expected_profit.to::<u128>()) - wei_to_eth(o.gas_cost_wei) > 0.0
            })
            .count();
        let best_opp = opps
            .iter()
            .map(|o| wei_to_eth(o.expected_profit.to::<u128>()))
            .fold(0.0_f64, f64::max);
        let roi = if strat_gas > 0.0 {
            (strat_net / strat_gas) * 100.0
        } else {
            0.0
        };
        let avg = if count > 0 { strat_gross / count as f64 } else { 0.0 };

        let gross_wei: u128 = opps.iter().map(|o| o.expected_profit.to::<u128>()).sum();
        let gas_wei: u128 = opps.iter().map(|o| o.gas_cost_wei).sum();
        let net_wei = (gross_wei as i128) - (gas_wei as i128);

        if strat_net > best_strat_net {
            best_strat_net = strat_net;
            best_strategy = Some(sname.clone());
        }

        strategy_metrics.insert(
            sname.clone(),
            StrategyMetrics {
                strategy: sname.clone(),
                count,
                profitable: strat_profitable,
                gross_revenue: strat_gross,
                gas_fees: strat_gas,
                net_profit: strat_net,
                net_profit_usd: 0.0, // TODO: wire CoinGecko price
                roi,
                avg_per_opp: avg,
                best_opp,
                gross_revenue_wei: gross_wei,
                net_profit_wei: net_wei,
                total_gas_cost_wei: gas_wei,
            },
        );
    }

    let mut dex_metrics: Vec<DexMetrics> = dexes
        .iter()
        .map(|dex_meta| {
            let opps_for_dex = by_dex.get(&dex_meta.name).cloned().unwrap_or_default();
            let count = opps_for_dex.len();
            let revenue: f64 = opps_for_dex
                .iter()
                .map(|o| wei_to_eth(o.expected_profit.to::<u128>()))
                .sum();
            let profitable = opps_for_dex
                .iter()
                .filter(|o| {
                    wei_to_eth(o.expected_profit.to::<u128>()) - wei_to_eth(o.gas_cost_wei) > 0.0
                })
                .count();
            let avg_profit = if count > 0 { revenue / count as f64 } else { 0.0 };
            let gross_wei: u128 = opps_for_dex
                .iter()
                .map(|o| o.expected_profit.to::<u128>())
                .sum();
            let gas_wei: u128 = opps_for_dex
                .iter()
                .map(|o| o.gas_cost_wei)
                .sum();
            let net_wei = (gross_wei as i128) - (gas_wei as i128);
            DexMetrics {
                dex: dex_meta.name.clone(),
                fork: dex_meta.fork.clone(),
                tx_count: dex_meta.tx_count,
                opportunities: count,
                profitable,
                revenue,
                avg_profit,
                gross_revenue_wei: gross_wei,
                net_profit_wei: net_wei,
                total_gas_cost_wei: gas_wei,
            }
        })
        .collect();
    dex_metrics.sort_by(|a, b| b.revenue.partial_cmp(&a.revenue).unwrap_or(std::cmp::Ordering::Equal));

    let summary_gross_wei: u128 = opportunities
        .iter()
        .map(|o| o.expected_profit.to::<u128>())
        .sum();
    let summary_gas_wei: u128 = opportunities
        .iter()
        .map(|o| o.gas_cost_wei)
        .sum();
    let summary_net_wei = (summary_gross_wei as i128) - (summary_gas_wei as i128);

    AggregationResult {
        summary: SummaryMetrics {
            total,
            profitable: profitable_count,
            gross_revenue,
            net_profit,
            net_profit_usd: 0.0, // TODO: wire CoinGecko price
            total_cost: total_gas,
            best_strategy,
            best_single_opp,
            gross_revenue_wei: summary_gross_wei,
            net_profit_wei: summary_net_wei,
            total_gas_cost_wei: summary_gas_wei,
        },
        by_strategy: strategy_metrics,
        by_dex: dex_metrics,
    }
}
