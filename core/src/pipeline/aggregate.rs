// Cross-run opportunity aggregation: summary / per-strategy / per-dex metrics
// (MEV-VERIFICATION §C.3 re-expose). Two entry points share one rollup:
// `aggregate*` over detected `MevOpportunity` rows (CLI `report`) and
// `aggregate_fills` over persisted `PaperFill` rows (CLI `paper stats`).
// Covered by offline unit tests below.

use crate::paper::types::PaperFill;
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
            by_dex: dexes
                .iter()
                .map(|d| DexMetrics {
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
                })
                .collect(),
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
    let mut total = 0_usize;

    // Deduplicate by canonical_id when available, falling back to
    // (block, pool pair, token pair) for backward compatibility (L9).
    let mut dedup_seen = std::collections::HashSet::<String>::new();
    for opp in opportunities.iter().filter(|opp| {
        let key = if let Some(ref cid) = opp.canonical_id {
            cid.clone()
        } else {
            format!(
                "{:?}|{}|{:#x}|{:#x}|{:#x}|{:#x}",
                opp.strategy, opp.block_number, opp.pool_a, opp.pool_b, opp.token_in, opp.token_out,
            )
        };
        dedup_seen.insert(key)
    }) {
        total += 1;
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
        let token_price = token_prices
            .get(&opp.token_out)
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
            let token_price = token_prices
                .get(&opp.token_out)
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
        let avg = if count > 0 {
            strat_gross / count as f64
        } else {
            0.0
        };

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

            let avg_profit = if count > 0 {
                revenue / count as f64
            } else {
                0.0
            };
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
    dex_metrics.sort_by(|a, b| {
        b.revenue
            .partial_cmp(&a.revenue)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

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

/// Roll up persisted paper fills into the same `SummaryMetrics` /
/// `StrategyMetrics` shapes as [`aggregate`].
///
/// A paper session persists [`PaperFill`] rows (gross/gas/net in wei, strategy
/// as `Strategy::to_string()`), not `MevOpportunity`, so the opportunity-shaped
/// entry point cannot consume them directly. This adapter projects fills back
/// onto the shared rollup so `paper stats` can surface per-strategy net and ROI
/// instead of only a flat per-fill table (MEV-VERIFICATION §C.3).
///
/// Notes:
/// - Fills are deduplicated by `canonical_id` exactly as in [`aggregate`]. When
///   a fill carries no `canonical_id`, the fallback key is built from the full
///   fill identity — block, tx index, strategy, pools and the running
///   `wallet_after` (strictly monotone, so unique within a session) — because
///   `PaperFill` has no token pair and the generic fallback key would collapse
///   two distinct same-block fills of the same strategy onto one row.
/// - Fills whose `strategy` string does not parse into a [`Strategy`] are
///   excluded: they cannot be attributed to a strategy bucket. The raw per-fill
///   table in `paper stats` still lists them, so nothing is hidden.
/// - `usd_price` is the **native** token price. Paper is native-normalized by
///   construction (only `is_native_eligible` strategies are ever filled), so
///   there is no per-token lookup to do. Pass `0.0` when no price snapshot is
///   available; the USD fields then read zero rather than guessing.
pub fn aggregate_fills(fills: &[PaperFill], usd_price: f64) -> AggregationResult {
    let opps: Vec<MevOpportunity> = fills.iter().filter_map(fill_as_opportunity).collect();
    aggregate(&opps, &[], usd_price)
}

/// Project one paper fill onto the opportunity shape the rollup consumes.
/// `None` when the persisted strategy string is not a known [`Strategy`].
fn fill_as_opportunity(fill: &PaperFill) -> Option<MevOpportunity> {
    let strategy: Strategy = fill.strategy.parse().ok()?;
    let pool_a = fill.pools.first().copied().unwrap_or(Address::ZERO);
    let pool_b = fill.pools.get(1).copied().unwrap_or(Address::ZERO);
    let mut opp = MevOpportunity::new(
        fill.block_number,
        fill.tx_index.unwrap_or(0),
        strategy,
        pool_a,
        0,
    );
    opp.pool_b = pool_b;
    // Paper is native-normalized, so both token legs are the native unit; that
    // makes `Address::ZERO` the right key for the native price lookup.
    opp.token_in = Address::ZERO;
    opp.token_out = Address::ZERO;
    opp.expected_profit = alloy::primitives::U256::from(fill.gross_wei);
    opp.gas_cost_wei = fill.gas_wei;
    opp.mempool_only = fill.mempool_only;
    opp.canonical_id = Some(fill.canonical_id.clone().unwrap_or_else(|| {
        format!(
            "paper_fill|{}|{}|{}|{:?}|{}",
            fill.block_number,
            fill.tx_index
                .map_or_else(|| "mempool".to_string(), |i| i.to_string()),
            fill.strategy,
            fill.pools,
            fill.wallet_after,
        )
    }));
    Some(opp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, U256};

    const ETH: u64 = 1_000_000_000_000_000_000;

    #[allow(clippy::too_many_arguments)]
    fn opp(
        block: u64,
        tx: usize,
        strategy: Strategy,
        pool_a: Address,
        pool_b: Address,
        profit_wei: u64,
        gas_wei: u64,
        cid: Option<&str>,
    ) -> MevOpportunity {
        let mut o = MevOpportunity::new(block, tx, strategy, pool_a, 1_700_000_000);
        o.pool_b = pool_b;
        o.token_in = address!("1111000000000000000000000000000000000001");
        o.token_out = address!("2222000000000000000000000000000000000002");
        o.expected_profit = U256::from(profit_wei);
        o.gas_cost_wei = gas_wei as u128;
        o.canonical_id = cid.map(|s| s.to_string());
        o
    }

    #[test]
    fn aggregate_dedups_by_canonical_id_and_falls_back_to_key_fields() {
        let pool_a = address!("aaaa000000000000000000000000000000000001");
        let pool_b = address!("bbbb000000000000000000000000000000000002");
        // Two rows with the SAME canonical id (cross-run duplicates)…
        let dup1 = opp(
            10,
            0,
            Strategy::TwoHopArb,
            pool_a,
            pool_b,
            ETH,
            ETH / 4,
            Some("c1"),
        );
        let dup2 = opp(
            10,
            0,
            Strategy::TwoHopArb,
            pool_a,
            pool_b,
            ETH,
            ETH / 4,
            Some("c1"),
        );
        // …and one without a canonical id: its fallback key (strategy, block,
        // pools, tokens) is a DIFFERENT key than the shared cid, so it survives.
        let noref = opp(
            10,
            0,
            Strategy::TwoHopArb,
            pool_a,
            pool_b,
            ETH,
            ETH / 4,
            None,
        );

        let agg = aggregate(&[dup1, dup2, noref], &[], 1.0);
        assert_eq!(
            agg.summary.total, 2,
            "cid dedup collapses dup1+dup2; the cid-less row keeps its own key"
        );
        assert_eq!(agg.summary.profitable, 2);
        assert_eq!(agg.summary.gross_revenue_wei, 2 * ETH as u128);
        assert_eq!(
            agg.summary.net_profit_wei,
            2 * (ETH - ETH / 4) as i128,
            "net = gross − gas for the two surviving ops"
        );
        let arb = &agg.by_strategy["arb"];
        assert_eq!(arb.count, 2);
    }

    #[test]
    fn aggregate_roi_formula_and_net_gas() {
        let pool_a = address!("aaaa000000000000000000000000000000000001");
        let pool_b = address!("bbbb000000000000000000000000000000000002");
        let o = opp(
            10,
            0,
            Strategy::TwoHopArb,
            pool_a,
            pool_b,
            ETH + ETH / 2,
            ETH / 2,
            Some("c2"),
        );
        // gross 1.5 ETH, gas 0.5 ETH → net 1.0 ETH, ROI = (1.0/0.5)*100 = 200%.
        let agg = aggregate(&[o], &[], 0.0);
        let arb = &agg.by_strategy["arb"];
        assert_eq!(arb.gross_revenue, 1.5);
        assert_eq!(arb.gas_fees, 0.5);
        assert_eq!(arb.net_profit, 1.0);
        assert!(
            (arb.roi - 200.0).abs() < 1e-9,
            "roi = net/gas*100, got {}",
            arb.roi
        );
        // Zero gas must not divide-by-zero.
        let o0 = opp(
            11,
            0,
            Strategy::Sandwich,
            pool_a,
            pool_b,
            ETH,
            0,
            Some("c3"),
        );
        let agg0 = aggregate(&[o0], &[], 0.0);
        assert_eq!(agg0.by_strategy["sandwich"].roi, 0.0);
    }

    #[test]
    fn aggregate_rolls_up_per_strategy_counts_and_net() {
        let pool_a = address!("aaaa000000000000000000000000000000000001");
        let pool_b = address!("bbbb000000000000000000000000000000000002");
        let ops = vec![
            opp(
                10,
                0,
                Strategy::TwoHopArb,
                pool_a,
                pool_b,
                ETH,
                ETH / 2,
                Some("x1"),
            ),
            opp(
                10,
                1,
                Strategy::Sandwich,
                pool_b,
                pool_a,
                2 * ETH,
                ETH,
                Some("x2"),
            ),
            // second sandwich, dedup-free canonical: adds to sandwich count
            opp(
                11,
                0,
                Strategy::Sandwich,
                pool_b,
                pool_a,
                3 * ETH,
                ETH,
                Some("x3"),
            ),
        ];
        let agg = aggregate(&ops, &[], 1.0);
        assert_eq!(agg.summary.total, 3);
        // all three are net-positive: arb 1.0−0.25, sandwich 2.0−1.0, 3.0−1.0.
        assert_eq!(agg.summary.profitable, 3);
        assert_eq!(agg.by_strategy["arb"].count, 1);
        assert_eq!(agg.by_strategy["sandwich"].count, 2);
        assert_eq!(
            agg.by_strategy["sandwich"].net_profit,
            (2.0 - 1.0) + (3.0 - 1.0)
        );
        assert_eq!(
            agg.by_strategy["sandwich"].net_profit_usd,
            // token price 1.0 quoted from pool: (2-1)+(3-1) = 3.0
            3.0
        );
        assert_eq!(agg.summary.best_strategy.as_deref(), Some("sandwich"));
    }

    #[test]
    fn aggregate_attributes_opp_to_both_dexes_and_empty_case() {
        use alloy::primitives::address as a;
        let pool_a = a!("cccc000000000000000000000000000000000003");
        let pool_b = a!("dddd000000000000000000000000000000000004");
        let dex_a = DexMeta {
            name: "uniswap_v3".into(),
            fork: "v3".into(),
            tx_count: 1,
            pool_addresses: vec![pool_a],
        };
        let dex_b = DexMeta {
            name: "quickswap".into(),
            fork: "v3".into(),
            tx_count: 1,
            pool_addresses: vec![pool_b],
        };
        let o = opp(
            10,
            0,
            Strategy::TwoHopArb,
            pool_a,
            pool_b,
            ETH,
            ETH / 2,
            Some("y1"),
        );
        let agg = aggregate(&[o], &[dex_a, dex_b], 1.0);
        let dexes = &agg.by_dex;
        assert_eq!(dexes.len(), 2, "both pools' dexes get an entry");
        let u = dexes.iter().find(|d| d.dex == "uniswap_v3").unwrap();
        let q = dexes.iter().find(|d| d.dex == "quickswap").unwrap();
        assert_eq!(u.opportunities, 1);
        assert_eq!(q.opportunities, 1);
        assert_eq!(u.gross_revenue_wei, ETH as u128);
        assert_eq!(q.revenue, 1.0);

        let empty = aggregate(
            &[],
            &[DexMeta {
                name: "uniswap_v3".into(),
                fork: "v3".into(),
                tx_count: 1,
                pool_addresses: vec![pool_a],
            }],
            1.0,
        );
        assert_eq!(empty.summary.total, 0);
        assert_eq!(empty.by_strategy.len(), 0);
        assert_eq!(empty.by_dex[0].opportunities, 0);
    }

    fn pfill(
        block: u64,
        tx: Option<usize>,
        strategy: &str,
        gross: u128,
        gas: u128,
        wallet_after: u128,
        cid: Option<&str>,
    ) -> PaperFill {
        PaperFill {
            block_number: block,
            tx_index: tx,
            canonical_id: cid.map(|s| s.to_string()),
            strategy: strategy.to_string(),
            gross_wei: gross,
            gas_wei: gas,
            net_wei: gross as i128 - gas as i128,
            wallet_before: wallet_after,
            wallet_after,
            pools: vec![address!("aaaa000000000000000000000000000000000001")],
            mempool_only: tx.is_none(),
        }
    }

    #[test]
    fn aggregate_fills_rolls_up_by_strategy_with_roi() {
        // One arb fill (1.0 − 0.5 = 0.5 net) and two sandwich fills
        // ((2.0 − 1.0) + (3.0 − 1.0) = 3.0 net). Sandwich gas total is 2.0,
        // so its ROI = 3.0 / 2.0 × 100 = 150%.
        let fills = vec![
            pfill(
                10,
                Some(0),
                "two_hop_arb",
                ETH as u128,
                ETH as u128 / 2,
                0,
                Some("f1"),
            ),
            pfill(
                10,
                Some(1),
                "sandwich",
                2 * ETH as u128,
                ETH as u128,
                0,
                Some("f2"),
            ),
            pfill(
                11,
                Some(0),
                "sandwich",
                3 * ETH as u128,
                ETH as u128,
                0,
                Some("f3"),
            ),
        ];
        let agg = aggregate_fills(&fills, 1.0);

        assert_eq!(agg.summary.total, 3, "all three fills are attributable");
        assert_eq!(agg.summary.gross_revenue_wei, 6 * ETH as u128);
        assert_eq!(agg.summary.total_gas_cost_wei, 5 * ETH as u128 / 2);
        assert_eq!(
            agg.summary.net_profit_wei,
            (ETH as i128 - ETH as i128 / 2) + (ETH as i128) + (2 * ETH as i128),
            "net = Σgross − Σgas across every fill"
        );
        // USD is quoted off the native price (paper is native-normalized).
        assert_eq!(agg.summary.net_profit_usd, 3.5);

        assert_eq!(agg.by_strategy["arb"].count, 1);
        assert_eq!(agg.by_strategy["sandwich"].count, 2);
        assert!(
            (agg.by_strategy["sandwich"].roi - 150.0).abs() < 1e-9,
            "roi = net/gas*100, got {}",
            agg.by_strategy["sandwich"].roi
        );
        assert!(
            (agg.by_strategy["arb"].roi - 100.0).abs() < 1e-9,
            "roi = net/gas*100, got {}",
            agg.by_strategy["arb"].roi
        );
        assert_eq!(agg.summary.best_strategy.as_deref(), Some("sandwich"));
    }

    #[test]
    fn aggregate_fills_dedups_cid_and_keeps_distinct_cidless_fills() {
        // Same canonical id twice → one row, the generic dedup path.
        let dup1 = pfill(
            10,
            Some(0),
            "two_hop_arb",
            ETH as u128,
            ETH as u128 / 4,
            0,
            Some("c1"),
        );
        let dup2 = pfill(
            10,
            Some(0),
            "two_hop_arb",
            ETH as u128,
            ETH as u128 / 4,
            0,
            Some("c1"),
        );
        // No canonical id, same block + strategy + pool, different tx index and a
        // different running wallet: two genuinely distinct fills that the
        // generic (token-less) fallback key would have collapsed into one.
        let tx0 = pfill(
            10,
            Some(1),
            "sandwich",
            ETH as u128,
            ETH as u128 / 4,
            7,
            None,
        );
        let tx1 = pfill(
            10,
            Some(2),
            "sandwich",
            ETH as u128,
            ETH as u128 / 4,
            8,
            None,
        );
        // Mempool fill: tx_index None. Also cidless — must not collide with the
        // two tx-anchored fills above.
        let mempool = pfill(10, None, "sandwich", ETH as u128, ETH as u128 / 4, 9, None);

        let agg = aggregate_fills(&[dup1, dup2, tx0, tx1, mempool], 0.0);
        assert_eq!(
            agg.summary.total, 4,
            "cid dedup collapses dup1+dup2; the three cid-less fills stay distinct"
        );
        assert_eq!(agg.summary.gross_revenue_wei, 4 * ETH as u128);
        assert_eq!(agg.by_strategy["sandwich"].count, 3);
    }

    #[test]
    fn aggregate_fills_skips_unattributable_strategies() {
        let good = pfill(
            10,
            Some(0),
            "jit",
            ETH as u128,
            ETH as u128 / 4,
            0,
            Some("g1"),
        );
        let bogus = pfill(
            10,
            Some(1),
            "not_a_strategy",
            ETH as u128,
            ETH as u128 / 4,
            0,
            Some("b1"),
        );
        let agg = aggregate_fills(&[good, bogus], 0.0);
        assert_eq!(
            agg.summary.total, 1,
            "an unparseable strategy cannot be attributed to a bucket"
        );
        assert_eq!(agg.by_strategy["jit"].count, 1);
        assert_eq!(agg.by_dex.len(), 0, "fill rollup passes no dex metadata");
    }
}
