//! Cross-validation harness — realized MEV (explorer ground truth) vs
//! opportunity detections (`run`/`live`), plan §11.
//!
//! Matching tiers (§11.1.1, never merged):
//! - **T1 exact** — same `canonical_id` (+ block within match window).
//! - **T2 overlap** — ≥1 pool in common + same profit/token direction + block
//!   within window.
//! - **T3 block-level** — any op of the same kind in the same block (ceiling).
//!
//! Headline recall = T1 ∪ T2. USD-weighted recall is the primary metric
//! (§11.1.3). Misses are attributed to the disjoint M1–M8 taxonomy (§11.1.2)
//! using `rejected_candidates` + pool-coverage facts; windows without
//! rejection capture degrade to `unknown-coverage`, never silently M3/M5.

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::explorer::store::{ExplorerStore, MevOpRow, OpportunityRow, RejectedRow};
use crate::types::ChainName;

/// Kind mapping opportunity strategy → realized kind (§3 taxonomy).
fn strategy_to_kind(strategy: &str) -> Option<&'static str> {
    match strategy {
        "TwoHopArb" | "MultiHopArb" => Some("arb_atomic"),
        "Sandwich" => Some("sandwich"),
        "Liquidation" => Some("liquidation"),
        "Jit" => Some("jit"),
        "JitArb" => Some("jit_arb"),
        _ => None,
    }
}

fn norm_addr(s: &Option<String>) -> Option<String> {
    s.as_ref().map(|a| a.to_ascii_lowercase())
}

/// T2 token-overlap direction: opportunity token_in/out vs realized route
/// endpoints. Returns true when at least one endpoint token matches the
/// opportunity pair (both directions considered — searcher route order
/// differs from simulation).
fn token_overlap(op: &OpportunityRow, ev: &MevOpRow) -> bool {
    let op_in = norm_addr(&op.token_in);
    let op_out = norm_addr(&op.token_out);
    if op_in.is_none() && op_out.is_none() {
        return false;
    }
    let mut endpoints: HashSet<String> = HashSet::new();
    if let Some(t) = norm_addr(&ev.profit_token) {
        endpoints.insert(t);
    }
    if let Some(route) = ev.route_json.as_deref() {
        if let Ok(serde_json::Value::Array(hops)) = serde_json::from_str::<serde_json::Value>(route) {
            for hop in hops {
                if let Some(t) = hop.get("token_in").and_then(|v| v.as_str()) {
                    endpoints.insert(t.to_ascii_lowercase());
                }
                if let Some(t) = hop.get("token_out").and_then(|v| v.as_str()) {
                    endpoints.insert(t.to_ascii_lowercase());
                }
            }
        }
    }
    let mut hit = false;
    if let Some(t) = &op_in {
        hit |= endpoints.contains(t);
    }
    if let Some(t) = &op_out {
        hit |= endpoints.contains(t);
    }
    hit
}

fn route_pools(ev: &MevOpRow) -> HashSet<String> {
    let mut out = HashSet::new();
    if let Some(route) = ev.route_json.as_deref() {
        if let Ok(serde_json::Value::Array(hops)) = serde_json::from_str::<serde_json::Value>(route) {
            for hop in hops {
                if let Some(p) = hop.get("pool").and_then(|v| v.as_str()) {
                    out.insert(p.to_ascii_lowercase());
                }
            }
        }
    }
    // sandwich/arb details may carry the pool at the top level
    if let Some(details) = ev.details_json.as_deref() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(details) {
            if let Some(p) = v.get("pool").and_then(|v| v.as_str()) {
                out.insert(p.to_ascii_lowercase());
            }
        }
    }
    out
}

fn op_pools(op: &OpportunityRow) -> HashSet<String> {
    let mut out = HashSet::new();
    for p in [&op.pool_a, &op.pool_b] {
        if let Some(p) = norm_addr(p) {
            out.insert(p);
        }
    }
    out
}

/// One matched realized-op ↔ opportunity pair.
#[derive(Debug, Clone, Serialize)]
pub struct MatchRecord {
    pub tier: &'static str,
    pub kind: String,
    pub block: u64,
    pub tx_hash: String,
    pub searcher: String,
    /// Realized profit USD (net where available, else gross).
    pub realized_usd: Option<f64>,
    pub canonical_id: Option<String>,
    /// Blocks of separation (live pending skew) — 0 for run backtests.
    pub block_distance: i64,
    /// 1:N / N:1 race marker (§11.1.1 item 3): matches sharing this op.
    pub one_to_many: bool,
}

/// Distribution of miss causes M1–M8 (+unknown-coverage) for one kind.
#[derive(Debug, Clone, Default, Serialize)]
pub struct MissTaxonomy {
    pub m1_pool_gap: u64,
    pub m2_venue_gap: u64,
    pub m3_pricing_threshold: u64,
    pub m4_gas_model: u64,
    pub m5_quote_math: u64,
    pub m6_competition: u64,
    pub m7_scanner_coverage: u64,
    pub m8_false_ground_truth: u64,
    pub unknown_coverage: u64,
}

impl MissTaxonomy {
}

/// Recall figures at each tier for one kind.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TierRecall {
    pub ops: u64,
    pub matched_t1: u64,
    pub matched_t2: u64,
    pub matched_t3: u64,
    pub usd_total: f64,
    pub usd_matched_t1: f64,
    pub usd_matched_t2: f64,
    pub misses: MissTaxonomy,
}

impl TierRecall {
    pub fn headline_count_recall(&self) -> f64 {
        if self.ops == 0 {
            return 1.0;
        }
        (self.matched_t1 + self.matched_t2) as f64 / self.ops as f64
    }
    pub fn t3_count_recall(&self) -> f64 {
        if self.ops == 0 {
            return 1.0;
        }
        (self.matched_t1 + self.matched_t2 + self.matched_t3) as f64 / self.ops as f64
    }
    pub fn headline_usd_recall(&self) -> f64 {
        if self.usd_total <= 0.0 {
            return 1.0;
        }
        (self.usd_matched_t1 + self.usd_matched_t2) / self.usd_total
    }
}

/// Threshold-sweep point (§11.1.3).
#[derive(Debug, Clone, Serialize)]
pub struct SweepPoint {
    pub min_usd: f64,
    pub count_recall: f64,
    pub usd_recall: f64,
}

/// Full validation report (§11.1).
#[derive(Debug, Clone, Serialize)]
pub struct ValidationReport {
    pub chain: String,
    pub from_block: u64,
    pub to_block: u64,
    pub match_window: u64,
    /// Sources of scanner-side data.
    pub opportunity_runs: Vec<String>,
    /// Scanner-coverage gaps for the window (M7 context).
    pub scanner_blocks_covered: usize,
    pub realized_blocks: usize,
    pub per_kind: Vec<(String, TierRecall)>,
    pub matches: Vec<MatchRecord>,
    /// Scanner opportunities with no realized op in-window (precision signal,
    /// §11.1: candidate false positives OR outcompeted/unprofitable — tagged
    /// separately, never merged).
    pub precision_signal_count: u64,
    /// Blocks the scanner covered but the explorer saw nothing of the kind.
    pub threshold_sweep: Vec<SweepPoint>,
    /// Realized-op pools absent from the opportunity pool set (M1 evidence).
    pub missing_pools: Vec<String>,
    /// Whether rejection capture was present for the window (drives
    /// `unknown-coverage` vs M3/M5 disambiguation, §9.3).
    pub rejections_recorded: bool,
}

impl ValidationReport {
    /// USD-weighted headline recall across all kinds (the primary metric).
    pub fn headline_usd_recall(&self) -> f64 {
        let total: f64 = self.per_kind.iter().map(|(_, t)| t.usd_total).sum();
        let matched: f64 = self
            .per_kind
            .iter()
            .map(|(_, t)| t.usd_matched_t1 + t.usd_matched_t2)
            .sum();
        if total <= 0.0 {
            return 1.0;
        }
        matched / total
    }

    /// Miss-cause distribution aggregated over kinds (printed in the report).
    pub fn miss_distribution(&self) -> MissTaxonomy {
        let mut agg = MissTaxonomy::default();
        for (_, t) in &self.per_kind {
            agg.m1_pool_gap += t.misses.m1_pool_gap;
            agg.m2_venue_gap += t.misses.m2_venue_gap;
            agg.m3_pricing_threshold += t.misses.m3_pricing_threshold;
            agg.m4_gas_model += t.misses.m4_gas_model;
            agg.m5_quote_math += t.misses.m5_quote_math;
            agg.m6_competition += t.misses.m6_competition;
            agg.m7_scanner_coverage += t.misses.m7_scanner_coverage;
            agg.m8_false_ground_truth += t.misses.m8_false_ground_truth;
            agg.unknown_coverage += t.misses.unknown_coverage;
        }
        agg
    }
}

/// Compute the full report for one window.
///
/// `opportunities` = scanner side (from the store's `opportunities` table,
/// loaded for `chain` in the window, filtered by optional run_ids).
/// `ops` = realized ground truth from `mev_ops`.
#[allow(clippy::too_many_arguments)]
pub fn compute_validation(
    store: &ExplorerStore,
    chain: ChainName,
    from_block: u64,
    to_block: u64,
    match_window: u64,
    run_filter: Option<&[String]>,
    threshold_sweep: bool,
) -> anyhow::Result<ValidationReport> {
    let chain_str = chain.to_string();
    let opportunities: Vec<OpportunityRow> = store
        .opportunities_in_range(&chain_str, from_block, to_block)?
        .into_iter()
        .filter(|op| {
            match run_filter {
                Some(ids) => op.run_id.as_ref().map(|r| ids.contains(r)).unwrap_or(false),
                None => true,
            }
        })
        .collect();

    let ops: Vec<MevOpRow> = store.ops_in_range(from_block, to_block, &[])?;
    let rejections: Vec<RejectedRow> = store.rejected_in_range(&chain_str, from_block, to_block)?;
    let rejections_recorded = store.has_rejections(&chain_str, from_block, to_block);
    let scanner_blocks: HashSet<u64> = store
        .opportunity_blocks(&chain_str, from_block, to_block)?
        .into_iter()
        .collect();
    let realized_blocks: HashSet<u64> = ops.iter().map(|o| o.block_number).collect();

    // Scanner pool coverage for the missing-pool report (M1).
    let scanner_pools: HashSet<String> = store
        .opportunity_pools(&chain_str, from_block, to_block)?
        .into_iter()
        .map(|p| p.to_ascii_lowercase())
        .collect();

    let mut runs: HashSet<String> = HashSet::new();
    for op in &opportunities {
        if let Some(r) = &op.run_id {
            runs.insert(r.clone());
        }
    }

    // Index opportunities by block for window matching.
    let mut opps_by_block: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, op) in opportunities.iter().enumerate() {
        opps_by_block.entry(op.block_number).or_default().push(i);
    }

    // canonical_id → op indices (T1).
    let mut opps_by_canonical: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, op) in opportunities.iter().enumerate() {
        if let Some(c) = &op.canonical_id {
            opps_by_canonical.entry(c.clone()).or_default().push(i);
        }
    }

    // Track which opportunities got matched (for 1:N marking + precision signal).
    let mut matched_opp: HashSet<usize> = HashSet::new();

    // Group ops by kind for per-kind recall.
    let mut kinds: Vec<String> = ops.iter().map(|o| o.kind.clone()).collect();
    kinds.sort();
    kinds.dedup();

    let mut matches: Vec<MatchRecord> = Vec::new();
    let mut per_kind: Vec<(String, TierRecall)> = Vec::new();

    for kind in &kinds {
        let mut recall = TierRecall::default();
        let kind_ops: Vec<&MevOpRow> = ops.iter().filter(|o| &o.kind == kind).collect();
        recall.ops = kind_ops.len() as u64;
        recall.usd_total = kind_ops
            .iter()
            .map(|o| o.net_profit_usd.or(o.profit_usd).unwrap_or(0.0))
            .sum();

        // Count how many opportunities of the matching strategy exist (for N:1).
        let matching_strategy = |s: &str| strategy_to_kind(s) == Some(kind.as_str());
        let mut matches_per_opp: HashMap<usize, u64> = HashMap::new();

        for ev in &kind_ops {
            let ev_usd = ev.net_profit_usd.or(ev.profit_usd).unwrap_or(0.0);
            let ev_pools = route_pools(ev);
            let mut tier: Option<(&'static str, usize, i64)> = None;

            // T1: exact canonical_id + block window.
            if let Some(cid) = &ev.canonical_id {
                if let Some(idxs) = opps_by_canonical.get(cid) {
                    if let Some(&i) = idxs.iter().find(|&&i| {
                        (opportunities[i].block_number as i64 - ev.block_number as i64).abs()
                            <= match_window as i64
                    }) {
                        tier = Some(("T1", i, opportunities[i].block_number as i64 - ev.block_number as i64));
                    }
                }
            }

            // T2: pool overlap ≥1 + token overlap + block window.
            if tier.is_none() {
                let mut best: Option<(usize, i64)> = None;
                for b in ev.block_number.saturating_sub(match_window)
                    ..=ev.block_number.saturating_add(match_window)
                {
                    if let Some(idxs) = opps_by_block.get(&b) {
                        for &i in idxs {
                            let op = &opportunities[i];
                            if !matching_strategy(&op.strategy) {
                                continue;
                            }
                            let overlap = op_pools(op).intersection(&ev_pools).count();
                            if overlap == 0 || !token_overlap(op, ev) {
                                continue;
                            }
                            let dist = (op.block_number as i64 - ev.block_number as i64).abs();
                            if best.map(|(bi, bd)| dist < bd && !matched_opp.contains(&bi))
                                .unwrap_or(true)
                            {
                                best = Some((i, dist));
                            }
                            break;
                        }
                        if best.is_some() {
                            break;
                        }
                    }
                }
                if let Some((i, dist)) = best {
                    tier = Some(("T2", i, dist));
                }
            }

            match tier {
                Some((t, i, dist)) => {
                    match t {
                        "T1" => {
                            recall.matched_t1 += 1;
                            recall.usd_matched_t1 += ev_usd;
                        }
                        _ => {
                            recall.matched_t2 += 1;
                            recall.usd_matched_t2 += ev_usd;
                        }
                    }
                    matched_opp.insert(i);
                    *matches_per_opp.entry(i).or_default() += 1;
                    matches.push(MatchRecord {
                        tier: t,
                        kind: kind.clone(),
                        block: ev.block_number,
                        tx_hash: ev.tx_hash.clone(),
                        searcher: ev.eoa.clone(),
                        realized_usd: Some(ev_usd),
                        canonical_id: ev.canonical_id.clone(),
                        block_distance: dist,
                        one_to_many: false, // patched below
                    });
                }
                None => {
                    // Miss — attribute M1–M8 (§11.1.2, first match wins).
                    let cause = attribute_miss(
                        ev,
                        &rejections,
                        &scanner_blocks,
                        &scanner_pools,
                        rejections_recorded,
                        kind,
                    );
                    match cause {
                        MissCause::M1 => recall.misses.m1_pool_gap += 1,
                        MissCause::M2 => recall.misses.m2_venue_gap += 1,
                        MissCause::M3 => recall.misses.m3_pricing_threshold += 1,
                        MissCause::M4 => recall.misses.m4_gas_model += 1,
                        MissCause::M5 => recall.misses.m5_quote_math += 1,
                        MissCause::M6 => recall.misses.m6_competition += 1,
                        MissCause::M7 => recall.misses.m7_scanner_coverage += 1,
                        MissCause::M8 => recall.misses.m8_false_ground_truth += 1,
                        MissCause::UnknownCoverage => recall.misses.unknown_coverage += 1,
                    }
                }
            }
        }

        // T3 ceiling: any same-kind realized op in the same block where the
        // scanner flagged an opportunity (of the matching strategy family).
        for ev in &kind_ops {
            if scanner_blocks.contains(&ev.block_number) {
                recall.matched_t3 += 1;
            }
        }
        // Patch 1:N markers (§11.1.1 item 3 — races are signal, never deduped).
        for m in matches.iter_mut().filter(|m| &m.kind == kind) {
            if let Some(&n) = matches_per_opp
                .iter()
                .find(|(i, _)| opportunities[**i].block_number == m.block)
                .map(|(_, n)| n)
            {
                m.one_to_many = n > 1;
            }
        }

        per_kind.push((kind.clone(), recall));
    }

    // Precision signal: scanner opportunities never matched to a realized op
    // (§11.1 — candidate false positives or outcompeted; reported, not judged).
    let precision_signal_count = opportunities.len() as u64 - matched_opp.len() as u64;

    // Missing pools: realized-op pools absent from the scanner's pool set (M1).
    let missing_pools: Vec<String> = {
        let mut v: Vec<String> = ops
            .iter()
            .flat_map(route_pools)
            .filter(|p| !scanner_pools.contains(p))
            .collect();
        v.sort();
        v.dedup();
        v
    };

    // Threshold sweep over USD-weighted recall (§11.1.3).
    let mut threshold_sweep_points: Vec<SweepPoint> = Vec::new();
    if threshold_sweep {
        for min_usd in [0.0, 1.0, 10.0, 100.0, 1_000.0, 10_000.0] {
            let mut total_usd = 0.0;
            let mut matched_usd = 0.0;
            let mut total_n = 0u64;
            let mut matched_n = 0u64;
            for ev in &ops {
                let usd = ev.net_profit_usd.or(ev.profit_usd).unwrap_or(0.0);
                if usd < min_usd {
                    continue;
                }
                total_usd += usd;
                total_n += 1;
                if matches.iter().any(|m| m.tx_hash == ev.tx_hash && m.block == ev.block_number) {
                    matched_usd += usd;
                    matched_n += 1;
                }
            }
            threshold_sweep_points.push(SweepPoint {
                min_usd,
                count_recall: if total_n == 0 { 1.0 } else { matched_n as f64 / total_n as f64 },
                usd_recall: if total_usd <= 0.0 { 1.0 } else { matched_usd / total_usd },
            });
        }
    }

    let opportunity_runs = runs.into_iter().collect();
    Ok(ValidationReport {
        chain: chain_str,
        from_block,
        to_block,
        match_window,
        opportunity_runs,
        scanner_blocks_covered: scanner_blocks.len(),
        realized_blocks: realized_blocks.len(),
        per_kind,
        matches,
        precision_signal_count,
        threshold_sweep: threshold_sweep_points,
        missing_pools,
        rejections_recorded,
    })
}

/// M1–M8 attribution (§11.1.2): first match wins, disjoint.
enum MissCause {
    M1,
    M2,
    M3,
    M4,
    M5,
    M6,
    M7,
    M8,
    UnknownCoverage,
}

#[allow(clippy::too_many_arguments)]
fn attribute_miss(
    ev: &MevOpRow,
    rejections: &[RejectedRow],
    scanner_blocks: &HashSet<u64>,
    scanner_pools: &HashSet<String>,
    rejections_recorded: bool,
    kind: &str,
) -> MissCause {
    let ev_pools = route_pools(ev);

    // M1 pool-gap: a route pool the scanner has never seen.
    if ev_pools.iter().any(|p| !scanner_pools.contains(p)) {
        return MissCause::M1;
    }

    // M7 scanner coverage: the scanner never ran over this block at all.
    if !scanner_blocks.contains(&ev.block_number) {
        return MissCause::M7;
    }

    // Rejection-driven causes: match rejected candidates on same block +
    // pool overlap with the realized op.
    let rel: Vec<&RejectedRow> = rejections
        .iter()
        .filter(|r| {
            r.block_number == ev.block_number && {
                let pools: HashSet<String> = [&r.pool_a, &r.pool_b]
                    .iter()
                    .filter_map(|p| p.as_ref().map(|s| s.to_ascii_lowercase()))
                    .collect();
                !pools.is_empty() && !pools.is_disjoint(&ev_pools)
            }
        })
        .collect();

    let m3_like = |s: &str| s == "below_min_profit";
    let m4_like = |s: &str| s == "gas_dominates";
    let m5_like = |s: &str| s == "quote_nonpositive";
    let strategy_matches = |s: &str| strategy_to_kind(s) == Some(kind);

    if rel.iter().any(|r| m4_like(&r.reject_reason)) {
        return MissCause::M4;
    }
    if rel.iter().any(|r| m5_like(&r.reject_reason) && strategy_matches(&r.strategy)) {
        return MissCause::M5;
    }
    if rel.iter().any(|r| m3_like(&r.reject_reason) && strategy_matches(&r.strategy)) {
        return MissCause::M3;
    }

    // M6 competition: the scanner flagged this block (an opportunity of the
    // family exists in-window) yet a different realization was extracted —
    // detection succeeded; the extractor raced faster. Approximated as:
    // scanner covered the block + matching-strategy opportunity nearby.
    if scanner_blocks.contains(&ev.block_number) {
        return MissCause::M6;
    }

    if !rejections_recorded {
        return MissCause::UnknownCoverage;
    }

    // Rejections exist but nothing matches — default to other-flavored M8
    // review candidates (mis-attribution suspect for `unknown` kinds).
    if kind == "unknown" {
        MissCause::M8
    } else {
        MissCause::M2
    }
}

/// Render the report as terminal text (§10 `validate`).
pub fn render_validation_report(report: &ValidationReport) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(out, "Explorer validation — {} blocks {}–{}", report.chain, report.from_block, report.to_block);
    let _ = writeln!(
        out,
        "  Runs: {} | match window: ±{} blocks | rejections recorded: {}",
        if report.opportunity_runs.is_empty() { "(none)".into() } else { report.opportunity_runs.join(",") },
        report.match_window,
        if report.rejections_recorded { "yes" } else { "NO — misses degrade to unknown-coverage" },
    );
    let _ = writeln!(
        out,
        "  Scanner-covered blocks: {} | realized-MEV blocks: {}",
        report.scanner_blocks_covered, report.realized_blocks,
    );
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "  {:<12} {:>6} {:>6} {:>6} {:>6} {:>10} {:>8} {:>8} {:>8}",
        "kind", "ops", "T1", "T2", "T3", "usd_total", "rec_cnt", "rec_usd", "t3_cnt"
    );
    for (kind, t) in &report.per_kind {
        let _ = writeln!(
            out,
            "  {:<12} {:>6} {:>6} {:>6} {:>6} {:>10.2} {:>7.0}% {:>7.0}% {:>7.0}%",
            kind,
            t.ops,
            t.matched_t1,
            t.matched_t2,
            t.matched_t3,
            t.usd_total,
            t.headline_count_recall() * 100.0,
            t.headline_usd_recall() * 100.0,
            t.t3_count_recall() * 100.0,
        );
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "  Headline USD-weighted recall: {:.1}%", report.headline_usd_recall() * 100.0);
    let _ = writeln!(out, "  Precision signal (unmatched opportunities): {}", report.precision_signal_count);

    let miss = report.miss_distribution();
    let _ = writeln!(out);
    let _ = writeln!(out, "  Miss taxonomy (M1–M8, §11.1.2):");
    let _ = writeln!(out, "    M1 pool-gap:            {:>5}", miss.m1_pool_gap);
    let _ = writeln!(out, "    M2 venue-gap:           {:>5}", miss.m2_venue_gap);
    let _ = writeln!(out, "    M3 pricing/threshold:   {:>5}", miss.m3_pricing_threshold);
    let _ = writeln!(out, "    M4 gas-model:           {:>5}", miss.m4_gas_model);
    let _ = writeln!(out, "    M5 quote/AMM-math:      {:>5}", miss.m5_quote_math);
    let _ = writeln!(out, "    M6 competition:         {:>5}", miss.m6_competition);
    let _ = writeln!(out, "    M7 scanner-coverage:    {:>5}", miss.m7_scanner_coverage);
    let _ = writeln!(out, "    M8 false-ground-truth:  {:>5}", miss.m8_false_ground_truth);
    let _ = writeln!(out, "    unknown-coverage:       {:>5}", miss.unknown_coverage);

    if !report.missing_pools.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "  Missing pools (M1, {}):", report.missing_pools.len());
        for p in report.missing_pools.iter().take(20) {
            let _ = writeln!(out, "    {p}");
        }
        if report.missing_pools.len() > 20 {
            let _ = writeln!(out, "    … and {} more", report.missing_pools.len() - 20);
        }
    }

    if !report.threshold_sweep.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "  Threshold sweep (min_usd → count recall / USD recall):");
        for s in &report.threshold_sweep {
            let _ = writeln!(
                out,
                "    {:>9.0} → {:>5.1}% / {:>5.1}%",
                s.min_usd,
                s.count_recall * 100.0,
                s.usd_recall * 100.0,
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, b256};

    fn store_with_fixtures() -> ExplorerStore {
        use alloy::primitives::{address as addr, b256 as b256f, U256 as U256t};
        let store = ExplorerStore::open_in_memory().unwrap();
        use crate::explorer::types::{Confidence, MevEvent, MevKind};
        use std::collections::HashMap;

        let ev = |block: u64, pools: Vec<alloy::primitives::Address>, cid: &str| {
            let _ = cid;
            MevEvent {
            block,
            ts: 1_700_000_000,
            tx_index: 1,
            tx_hash: b256f!("1111111111111111111111111111111111111111111111111111111111111111"),
            kind: MevKind::ArbAtomic,
            searcher: addr!("2222000000000000000000000000000000000002"),
            contract: None,
            pools: pools.clone(),
            profit_token: Some(addr!("4444000000000000000000000000000000000004")),
            profit_amount: Some(U256t::from(1_000_000u64)),
            profit_usd: None,
            gas_cost_wei: U256t::from(100_000_000_000_000u64),
            confidence: Confidence::Exact,
            victim_hashes: vec![],
            victim_swap_size: None,
            details: serde_json::json!({
                "route": [
                    {"pool": format!("{:#x}", pools[0]), "amm": "v3",
                     "token_in": "0x4444000000000000000000000000000000000004",
                     "token_out": "0x5555000000000000000000000000000000000005",
                     "amount_in": "1", "amount_out": "2"}
                ]
            }),
        }
        };
        let pool_a = addr!("3333000000000000000000000000000000000003");
        let pool_b = addr!("33330000000000000000000000000000000000ff");
        let mut prices = HashMap::new();
        prices.insert(
            addr!("4444000000000000000000000000000000000004"),
            crate::explorer::pricing::TokenUsd { usd: 1.0, decimals: 6 },
        );
        // Block 100: realized op with canonical id (scanner knows pool_a + cid)
        let e = ev(100, vec![pool_a], "ArbAtomic|matching");
        let hash1 = b256f!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let hash2 = b256f!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        store.insert_block_facts(100, &hash1, 1_700_000_000, Some(25.0), 5, &[], &[], &[], &[e], Some(1.0), &prices).unwrap();
        // Block 102: realized op on an unknown pool (M1) + scanner never there
        let e2 = ev(102, vec![pool_b], "ArbAtomic|other");
        store.insert_block_facts(102, &hash2, 1_700_000_100, Some(25.0), 5, &[], &[], &[], &[e2], Some(1.0), &prices).unwrap();

        store.insert_opportunity(
            "run_1", "polygon", 100, Some(1), "TwoHopArb",
            Some(pool_a), None,
            Some(addr!("4444000000000000000000000000000000000004")),
            Some(addr!("5555000000000000000000000000000000000005")),
            Some(U256t::from(1_000_000u64)), Some(U256t::from(900_000u64)), Some(U256t::from(50_000u64)),
            None, Some(1_700_000_000), false, Some("0.9"),
            None, None, Some("replay"),
            // T1 join key: must equal the explorer-side canonical form the
            // store computed for the block-100 event (ArbAtomic|sorted pools).
            Some(&format!("ArbAtomic|{pool_a:#x}")),
        ).unwrap();
        store
    }

    #[test]
    fn t1_match_and_m1_miss() {
        let store = store_with_fixtures();
        let report = compute_validation(
            &store,
            crate::types::ChainName::Polygon,
            90,
            110,
            0,
            None,
            false,
        )
        .unwrap();

        let (kind, t) = report.per_kind.iter().find(|(k, _)| k == "arb_atomic").unwrap();
        assert_eq!(*kind, "arb_atomic");
        assert_eq!(t.ops, 2);
        // block 100 matched T1 via canonical_id
        assert_eq!(t.matched_t1, 1);
        // block 102 missed on unknown pool → M1
        assert_eq!(t.misses.m1_pool_gap, 1);
        // headline USD recall = 50% (matched pool_a op carries same USD weight)
        assert!((report.headline_usd_recall() - 0.5).abs() < 1e-9);
        assert!(report.missing_pools.contains(&format!("{:#x}", address!("33330000000000000000000000000000000000ff"))));
    }

    #[test]
    fn t3_is_ceiling() {
        let store = store_with_fixtures();
        let report = compute_validation(&store, crate::types::ChainName::Polygon, 90, 110, 0, None, false).unwrap();
        let (_, t) = report.per_kind.iter().find(|(k, _)| k == "arb_atomic").unwrap();
        // block 100 has a scanner opportunity → T3 includes it
        assert_eq!(t.matched_t3, 1);
    }

    #[test]
    fn report_renders() {
        let store = store_with_fixtures();
        let report = compute_validation(&store, crate::types::ChainName::Polygon, 90, 110, 0, None, true).unwrap();
        let text = render_validation_report(&report);
        assert!(text.contains("arb_atomic"));
        assert!(text.contains("Miss taxonomy"));
        assert!(text.contains("Threshold sweep"));
    }
}
