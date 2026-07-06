use crate::types::MevOpportunity;
use crate::types::Strategy;
use alloy::primitives::Address;

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
    pub pool_addresses: Vec<Address>,
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
        Strategy::Liquidation => "liquidation",
        Strategy::CrossBlockArb => "crossblock",
    }
}

/// Backward-compatible aggregate with a single USD price for the native token.
/// Delegates to `aggregate_with_prices` using the native token as fallback.
pub fn aggregate(
    opportunities: &[MevOpportunity],
    dexes: &[DexMeta],
    usd_price: f64,
) -> AggregationResult {
    // Build a single-entry price map keyed by ZERO (native fallback)
    let mut prices = std::collections::HashMap::new();
    prices.insert(Address::ZERO, usd_price);
    aggregate_with_prices(opportunities, dexes, &prices)
}

/// Aggregate with per-token USD prices.
///
/// `token_prices` maps token addresses to their USD price.
/// If a token is not in the map, `Address::ZERO` (native token) is used as fallback.
/// When no native price is available, `net_profit_usd` is set to 0.0.
pub fn aggregate_with_prices(
    opportunities: &[MevOpportunity],
    dexes: &[DexMeta],
    token_prices: &std::collections::HashMap<Address, f64>,
) -> AggregationResult {
    if opportunities.is_empty() {
        return AggregationResult {
            summary: SummaryMetrics {
                total: 0,
                profitable: 0,
                gross_revenue: 0.0,
                net_profit: 0.0,
                net_profit_usd: 0.0,
                total_cost: 0.0,
                best_strategy: None,
                best_single_opp: 0.0,
                gross_revenue_wei: 0,
                net_profit_wei: 0,
                total_gas_cost_wei: 0,
            },
            by_strategy: std::collections::HashMap::new(),
            by_dex: dexes.iter().map(|d| DexMetrics {
                dex: d.name.clone(),
                fork: d.fork.clone(),
                tx_count: d.tx_count,
                opportunities: 0,
                profitable: 0,
                revenue: 0.0,
                avg_profit: 0.0,
                gross_revenue_wei: 0,
                net_profit_wei: 0,
                total_gas_cost_wei: 0,
            }).collect(),
        };
    }

    // Build reverse lookup: pool address → dex name
    let mut pool_to_dex: std::collections::HashMap<Address, &str> =
        std::collections::HashMap::new();
    for dex_meta in dexes {
        for addr in &dex_meta.pool_addresses {
            pool_to_dex.entry(*addr).or_insert(&dex_meta.name);
        }
    }

    let mut by_strategy: std::collections::HashMap<String, Vec<&MevOpportunity>> =
        std::collections::HashMap::new();
    let mut by_dex: std::collections::HashMap<String, Vec<&MevOpportunity>> =
        std::collections::HashMap::new();

    let mut gross_revenue = 0.0_f64;
    let mut total_gas = 0.0_f64;
    let mut profitable_count = 0_usize;
    let mut best_single_opp = 0.0_f64;
    let mut summary_gross_wei = 0_u128;
    let mut summary_gas_wei = 0_u128;
    let mut summary_usd = 0.0_f64;

    // Deduplicate by canonical_id when available, falling back to
    // (block, pool pair, token pair) for backward compatibility (L9).
    let mut dedup_seen = std::collections::HashSet::<String>::new();
    for opp in opportunities.iter().filter(|opp| {
        let key = if let Some(ref cid) = opp.canonical_id {
            cid.clone()
        } else {
            format!(
                "{:?}|{}|{:#x}|{:#x}|{:#x}|{:#x}",
                opp.strategy, opp.block_number, opp.pool_a, opp.pool_b,
                opp.token_in, opp.token_out,
            )
        };
        dedup_seen.insert(key)
    }) {
        let profit_wei = opp.expected_profit.to::<u128>();
        let gas_wei = opp.gas_cost_wei;
        let profit_eth = wei_to_eth(profit_wei);
        let gas_eth = wei_to_eth(gas_wei);

        gross_revenue += profit_eth;
        total_gas += gas_eth;
        summary_gross_wei += profit_wei;
        summary_gas_wei += gas_wei;
        if profit_eth - gas_eth > 0.0 {
            profitable_count += 1;
        }
        if profit_eth > best_single_opp {
            best_single_opp = profit_eth;
        }
        // Per-token USD: use token_out price if available, else native fallback (L3)
        let token_price = token_prices.get(&opp.token_out)
            .or_else(|| token_prices.get(&Address::ZERO))
            .copied()
            .unwrap_or(0.0);
        summary_usd += (profit_eth - gas_eth) * token_price;

        let sname = ui_strategy_name(opp.strategy).to_string();
        by_strategy.entry(sname).or_default().push(opp);

        let mut seen = std::collections::HashSet::new();
        if let Some(&dex_name) = pool_to_dex.get(&opp.pool_a) {
            if seen.insert(dex_name) {
                by_dex.entry(dex_name.to_string()).or_default().push(opp);
            }
        }
        if let Some(&dex_name) = pool_to_dex.get(&opp.pool_b) {
            if seen.insert(dex_name) {
                by_dex.entry(dex_name.to_string()).or_default().push(opp);
            }
        }
    }

    let total = opportunities.len();
    let net_profit = gross_revenue - total_gas;
    let summary_net_wei = (summary_gross_wei as i128) - (summary_gas_wei as i128);

    let mut best_strategy: Option<String> = None;
    let mut best_strat_net = 0.0_f64;
    let mut strategy_metrics = std::collections::HashMap::new();

    for (sname, opps) in &by_strategy {
        let count = opps.len();
        let mut strat_gross = 0.0_f64;
        let mut strat_gas = 0.0_f64;
        let mut strat_profitable = 0_usize;
        let mut best_opp = 0.0_f64;
        let mut gross_wei = 0_u128;
        let mut gas_wei = 0_u128;

        let mut strat_usd = 0.0_f64;
        for opp in opps {
            let pw = opp.expected_profit.to::<u128>();
            let gw = opp.gas_cost_wei;
            let pe = wei_to_eth(pw);
            let ge = wei_to_eth(gw);
            strat_gross += pe;
            strat_gas += ge;
            gross_wei += pw;
            gas_wei += gw;
            if pe - ge > 0.0 {
                strat_profitable += 1;
            }
            if pe > best_opp {
                best_opp = pe;
            }
            // Per-token USD: use token_out price if available, else native fallback (L3)
            let token_price = token_prices.get(&opp.token_out)
                .or_else(|| token_prices.get(&Address::ZERO))
                .copied()
                .unwrap_or(0.0);
            strat_usd += (pe - ge) * token_price;
        }

        let strat_net = strat_gross - strat_gas;
        let net_wei = (gross_wei as i128) - (gas_wei as i128);
        let roi = if strat_gas > 0.0 {
            (strat_net / strat_gas) * 100.0
        } else {
            0.0
        };
        let avg = if count > 0 { strat_gross / count as f64 } else { 0.0 };

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
                net_profit_usd: strat_usd,
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
            let mut revenue = 0.0_f64;
            let mut profitable = 0_usize;
            let mut gross_wei = 0_u128;
            let mut gas_wei = 0_u128;

            for opp in opps_for_dex {
                let pw = opp.expected_profit.to::<u128>();
                let gw = opp.gas_cost_wei;
                let pe = wei_to_eth(pw);
                let ge = wei_to_eth(gw);
                revenue += pe;
                gross_wei += pw;
                gas_wei += gw;
                if pe - ge > 0.0 {
                    profitable += 1;
                }
            }

            let avg_profit = if count > 0 { revenue / count as f64 } else { 0.0 };
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

    AggregationResult {
        summary: SummaryMetrics {
            total,
            profitable: profitable_count,
            gross_revenue,
            net_profit,
            net_profit_usd: summary_usd,
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

