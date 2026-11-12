//! Single reducer: KPIs, series, matrices, leaderboards, facets from OpRecords.
use std::collections::{BTreeMap, HashMap, HashSet};

use serde::Serialize;

use crate::explorer::reject::RejectReason;
use crate::explorer::types::MevKind;

use super::caps::StoreCaps;
use super::evidence::{parse_route, OpEvidence};
use super::{OpRecord, OperationEconomics, WindowSpec};

/// Aggregated KPI grid (P3) + quality (P4).
#[derive(Debug, Clone, Serialize, Default)]
pub struct KpiPanel {
    pub ops: i64,
    pub volume_usd: f64,
    pub volume_priced: i64,
    pub gross_usd: f64,
    pub gross_priced: i64,
    pub gas_usd: f64,
    pub gas_priced: i64,
    pub flash_fee_usd: f64,
    pub flash_fee_priced: i64,
    pub net_usd: f64,
    pub net_priced: i64,
    pub net_gating_available: bool,
    pub highest_single_usd: f64,
    pub searchers: i64,
    pub competitors: i64,
    pub quality: QualityPanel,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct QualityPanel {
    pub ops_priced: i64,
    pub ops_unpriced: i64,
    pub ops_exact: i64,
    pub ops_inferred: i64,
    pub net_usd_exact: f64,
    pub net_usd_inferred: f64,
    pub ops_approx: i64,
    pub ops_native_unpriced: i64,
    pub ops_multi_asset: i64,
    pub ops_pricing_clamped: i64,
    pub ops_flash_fee_unpriced: i64,
    pub ops_excluded_from_net: i64,
    pub pricing_coverage_pct: f64,
    pub pricing_flags_available: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SeriesPoint {
    pub bucket_start: i64,
    pub ops: i64,
    pub net_usd: f64,
    pub blocks_indexed: i64,
    pub gross_usd: f64,
    pub gas_usd: f64,
    pub priced_ops: i64,
    /// Unique competitors (contract||eoa) in the bucket.
    pub searchers: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct KindCell {
    pub kind: String,
    pub subkind: Option<String>,
    pub confidence: String,
    pub ops: i64,
    pub gross_usd: f64,
    pub net_usd: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActorRow {
    pub competitor: String,
    pub eoa: String,
    pub contract: Option<String>,
    pub label: Option<String>,
    pub ops: i64,
    pub gross_usd: f64,
    pub net_usd: f64,
    pub excluded_ops: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PoolRow {
    pub pool: String,
    pub ops: i64,
    pub gross_usd: f64,
    pub net_usd: f64,
    pub gas_usd: f64,
    pub priced_ops: i64,
    pub amm_mix: BTreeMap<String, i64>,
    pub unique_competitors: i64,
    pub top_kind: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TokenRow {
    pub token: String,
    pub ops: i64,
    pub gross_usd: f64,
    pub net_usd: f64,
    pub priced_ops: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FacetRow {
    pub value: String,
    pub ops: i64,
    pub gross_usd: f64,
    pub net_usd: f64,
    pub out_of_catalogue: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RejectRow {
    pub reason: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct EventRow {
    pub id: i64,
    pub block_number: u64,
    pub tx_index: Option<u64>,
    pub tx_hash: String,
    pub ts: u64,
    pub kind: String,
    pub subkind: Option<String>,
    pub confidence: String,
    pub eoa: String,
    pub contract: Option<String>,
    pub profit_usd: Option<f64>,
    pub net_profit_usd: Option<f64>,
    pub gas_cost_usd: Option<f64>,
    pub flashloan_fee_usd: Option<f64>,
    pub economics: OperationEconomics,
    pub volume_usd: Option<f64>,
    pub detector: String,
    pub excluded_from_net: bool,
    pub pnl_basis: Option<String>,
    /// `details.tags` — enables client-side tag facet filters without evidence join.
    pub tags: Vec<String>,
}

/// Per-bucket ops/net by kind/subkind/confidence slice (full-window; not truncated events).
#[derive(Debug, Clone, Serialize)]
pub struct StrategySeriesPoint {
    pub bucket_start: i64,
    pub slice_key: String,
    pub confidence: String,
    pub ops: i64,
    pub net_usd: f64,
    pub priced_ops: i64,
}

/// Matched-cohort execution economics across the full window, including known losses.
#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct StrategyEconomics {
    pub slice_key: String,
    pub ops_total: i64,
    pub ops_eligible: i64,
    pub ops_exact: i64,
    pub ops_inferred: i64,
    pub observed_losses: i64,
    pub included_excluded_from_net: i64,
    pub exclusion_reasons: BTreeMap<String, i64>,
    pub gross_usd: Option<f64>,
    pub cost_usd: Option<f64>,
    pub net_usd: Option<f64>,
    pub roi_pct: Option<f64>,
    pub gross_per_10: Option<f64>,
    pub net_per_10: Option<f64>,
}

/// Cap-friendly scatter row for Analytics (built from full ops, not truncated events).
#[derive(Debug, Clone, Serialize)]
pub struct ScatterPoint {
    pub id: i64,
    pub tx_hash: String,
    pub ts: u64,
    pub slice_key: String,
    pub confidence: String,
    pub tags: Vec<String>,
    pub net_profit_usd: Option<f64>,
    pub volume_usd: Option<f64>,
    pub volume_partial: bool,
    pub priced: bool,
    pub eoa: String,
    pub contract: Option<String>,
    pub block_number: u64,
}

/// Compact interned full-window index for search / ops list (not panel re-aggregation).
#[derive(Debug, Clone, Serialize, Default)]
pub struct OpIndex {
    pub slices: Vec<String>,
    pub actors: Vec<String>,
    pub eoas: Vec<String>,
    pub contracts: Vec<String>,
    pub tags: Vec<String>,
    pub pools: Vec<String>,
    pub amms: Vec<String>,
    pub tx: Vec<String>,
    /// `[ts, slice_id, conf, flags, actor_id, eoa_id, contract_id|null, block, tx_id,
    ///   net|null, gross|null, gas|null, vol|null, flash|null, tag_ids, legs, id]`
    /// Operation id is appended at row[16]; the original 16 positions are unchanged.
    /// `conf`: 0=exact, 1=inferred, 2=other.
    /// `legs`: `[[pool_id, amm_id], …]` deduped.
    pub rows: Vec<serde_json::Value>,
}

/// Full reducer output for the snapshot panels.
#[derive(Debug, Clone, Serialize)]
pub struct ReducedPanels {
    pub kpi: KpiPanel,
    pub series: Vec<SeriesPoint>,
    pub bucket_secs: i64,
    pub kind_matrix: Vec<KindCell>,
    pub unknown_rewritten: i64,
    pub actors: Vec<ActorRow>,
    pub pools: Vec<PoolRow>,
    pub tokens: Vec<TokenRow>,
    pub tag_facets: Vec<FacetRow>,
    pub reason_facets: Vec<FacetRow>,
    pub untagged_ops: i64,
    pub strategy_economics: Vec<StrategyEconomics>,
    pub events: Vec<EventRow>,
    pub evidence: Vec<OpEvidence>,
    pub strategy_series: Vec<StrategySeriesPoint>,
    pub scatter_points: Vec<ScatterPoint>,
    pub op_index: OpIndex,
    pub truncation: TruncationInfo,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct TruncationInfo {
    pub max_ops: usize,
    pub ops_total: usize,
    pub ops_emitted: usize,
    pub max_evidence: usize,
    pub evidence_emitted: usize,
    pub hit_budget: bool,
    pub index_total: usize,
    pub index_emitted: usize,
    pub index_complete: bool,
}

/// Op-index flag bits (must stay stable for the offline HTML).
pub const FLAG_EXCLUDED_FROM_NET: u32 = 1 << 0;
pub const FLAG_VOLUME_PARTIAL: u32 = 1 << 1;
pub const FLAG_USD_APPROXIMATE: u32 = 1 << 2;
pub const FLAG_NATIVE_UNPRICED: u32 = 1 << 3;
pub const FLAG_MULTI_ASSET_PRICING: u32 = 1 << 4;
pub const FLAG_PRICING_CLAMPED: u32 = 1 << 5;
pub const FLAG_FLASH_FEE_UNPRICED: u32 = 1 << 6;

/// Reduce ops (+ optional block ts for coverage band) into panels.
#[allow(clippy::too_many_arguments)]
pub fn reduce(
    ops: &[OpRecord],
    caps: &StoreCaps,
    window: &WindowSpec,
    block_timestamps: &[i64],
    labels: &HashMap<String, String>,
    catalogue_tags: &HashSet<String>,
    max_ops: usize,
    max_evidence: usize,
) -> ReducedPanels {
    let unknown_rewritten = ops.iter().filter(|o| o.kind_raw == "unknown").count() as i64;
    let kpi = reduce_kpi(ops, caps);
    let bucket_secs = auto_bucket_secs(window);
    let series = reduce_series(ops, block_timestamps, window, bucket_secs, caps);
    let kind_matrix = reduce_kind_matrix(ops, caps);
    let actors = reduce_actors(ops, labels, caps);
    let pools = reduce_pools(ops, caps);
    let tokens = reduce_tokens(ops, caps);
    let (tag_facets, reason_facets, untagged_ops) = reduce_facets(ops, catalogue_tags, caps);
    let strategy_economics = reduce_strategy_economics(ops);
    let strategy_series = reduce_strategy_series(ops, window, bucket_secs);
    let scatter_cap = max_ops.max(2_000);
    let scatter_points = reduce_scatter_points(ops, scatter_cap);
    let op_index = build_op_index(ops);

    let ops_total = ops.len();
    let emit_ops = ops.len().min(max_ops);
    let events: Vec<EventRow> = ops.iter().take(emit_ops).map(event_row).collect();
    let evidence_n = emit_ops.min(max_evidence);
    let evidence: Vec<OpEvidence> = ops
        .iter()
        .take(evidence_n)
        .map(super::evidence::build_evidence)
        .collect();

    let hit_budget = ops_total > max_ops || ops_total > max_evidence;
    let ops_emitted = events.len();
    let evidence_emitted = evidence.len();
    let index_total = op_index.rows.len();
    ReducedPanels {
        kpi,
        series,
        bucket_secs,
        kind_matrix,
        unknown_rewritten,
        actors,
        pools,
        tokens,
        tag_facets,
        reason_facets,
        untagged_ops,
        strategy_economics,
        events,
        evidence,
        strategy_series,
        scatter_points,
        op_index,
        truncation: TruncationInfo {
            max_ops,
            ops_total,
            ops_emitted,
            max_evidence,
            evidence_emitted,
            hit_budget,
            index_total,
            index_emitted: index_total,
            index_complete: true,
        },
    }
}

/// Stable kind/subkind key — never collapse utility subkinds.
pub fn slice_key(kind: &str, subkind: Option<&str>) -> String {
    match subkind.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => format!("{kind}/{s}"),
        None => kind.to_string(),
    }
}

fn reduce_strategy_economics(ops: &[OpRecord]) -> Vec<StrategyEconomics> {
    let mut groups: BTreeMap<String, (StrategyEconomics, [f64; 3])> = BTreeMap::new();
    for op in ops {
        let key = slice_key(&op.kind, op.subkind.as_deref());
        let (row, sums) = groups.entry(key.clone()).or_insert_with(|| {
            (
                StrategyEconomics {
                    slice_key: key,
                    ..Default::default()
                },
                [0.0; 3],
            )
        });
        row.ops_total += 1;
        let economics = OperationEconomics::from_record(op);
        if !economics.eligible {
            *row.exclusion_reasons
                .entry(
                    economics
                        .reason
                        .unwrap_or_else(|| "economics_unavailable".into()),
                )
                .or_default() += 1;
            continue;
        }
        if let (Some(gross), Some(cost), Some(net)) =
            (economics.gross_usd, economics.cost_usd, economics.net_usd)
        {
            row.ops_eligible += 1;
            row.ops_exact += i64::from(op.confidence == "exact");
            row.ops_inferred += i64::from(op.confidence == "inferred");
            row.observed_losses += i64::from(net < 0.0);
            row.included_excluded_from_net += i64::from(op.excluded_from_net == Some(true));
            sums[0] += gross;
            sums[1] += cost;
            sums[2] += net;
        }
    }
    groups
        .into_values()
        .map(|(mut row, [gross, cost, net])| {
            // Never expose partial sums or ratios if the cohort overflows f64.
            if row.ops_eligible > 0 && [gross, cost, net].iter().all(|v| v.is_finite()) {
                row.gross_usd = Some(gross);
                row.cost_usd = Some(cost);
                row.net_usd = Some(net);
                row.roi_pct = super::evidence::cost_ratio(net, cost, 100.0);
                row.gross_per_10 = super::evidence::cost_ratio(gross, cost, 10.0);
                row.net_per_10 = super::evidence::cost_ratio(net, cost, 10.0);
            }
            row
        })
        .collect()
}

fn reduce_strategy_series(
    ops: &[OpRecord],
    window: &WindowSpec,
    bucket_secs: i64,
) -> Vec<StrategySeriesPoint> {
    let since = (window.since_ts / bucket_secs) * bucket_secs;
    let mut map: BTreeMap<(i64, String, String), (i64, f64, i64)> = BTreeMap::new();
    for o in ops {
        let b = (o.ts as i64 / bucket_secs) * bucket_secs;
        if b < since || b > window.until_ts {
            continue;
        }
        let key = (
            b,
            slice_key(&o.kind, o.subkind.as_deref()),
            o.confidence.clone(),
        );
        let e = map.entry(key).or_insert((0, 0.0, 0));
        e.0 += 1;
        if o.profit_usd.is_some() {
            e.2 += 1;
        }
        let excluded = o.excluded_from_net.unwrap_or(false);
        if !excluded {
            if let Some(n) = o.net_profit_usd {
                e.1 += n;
            }
        }
    }
    map.into_iter()
        .map(
            |((bucket_start, slice_key, confidence), (ops, net_usd, priced_ops))| {
                StrategySeriesPoint {
                    bucket_start,
                    slice_key,
                    confidence,
                    ops,
                    net_usd,
                    priced_ops,
                }
            },
        )
        .collect()
}

fn reduce_scatter_points(ops: &[OpRecord], max_points: usize) -> Vec<ScatterPoint> {
    ops.iter()
        .take(max_points)
        .map(|o| {
            let volume_partial = details_has_token(o.details_json.as_deref(), "VOLUME_PARTIAL");
            ScatterPoint {
                id: o.id,
                tx_hash: o.tx_hash.clone(),
                ts: o.ts,
                slice_key: slice_key(&o.kind, o.subkind.as_deref()),
                confidence: o.confidence.clone(),
                tags: details_tags(o.details_json.as_deref()),
                net_profit_usd: if o.excluded_from_net.unwrap_or(false) {
                    None
                } else {
                    o.net_profit_usd
                },
                volume_usd: o.volume_usd,
                volume_partial,
                priced: o.profit_usd.is_some(),
                eoa: o.eoa.clone(),
                contract: o.contract.clone(),
                block_number: o.block_number,
            }
        })
        .collect()
}

fn conf_code(confidence: &str) -> u64 {
    match confidence {
        "exact" => 0,
        "inferred" => 1,
        _ => 2,
    }
}

fn op_flags(o: &OpRecord) -> u32 {
    let mut f = 0u32;
    if o.excluded_from_net.unwrap_or(false) {
        f |= FLAG_EXCLUDED_FROM_NET;
    }
    if details_has_token(o.details_json.as_deref(), "VOLUME_PARTIAL") {
        f |= FLAG_VOLUME_PARTIAL;
    }
    if o.usd_approximate.unwrap_or(false) {
        f |= FLAG_USD_APPROXIMATE;
    }
    if o.native_unpriced.unwrap_or(false) {
        f |= FLAG_NATIVE_UNPRICED;
    }
    if o.multi_asset_pricing.unwrap_or(false) {
        f |= FLAG_MULTI_ASSET_PRICING;
    }
    if o.pricing_clamped.unwrap_or(false) {
        f |= FLAG_PRICING_CLAMPED;
    }
    if o.flash_fee_unpriced.unwrap_or(false) {
        f |= FLAG_FLASH_FEE_UNPRICED;
    }
    f
}

struct Intern {
    map: HashMap<String, usize>,
    vec: Vec<String>,
}

impl Intern {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            vec: Vec::new(),
        }
    }

    fn get(&mut self, s: &str) -> usize {
        if let Some(&i) = self.map.get(s) {
            return i;
        }
        let i = self.vec.len();
        self.map.insert(s.to_string(), i);
        self.vec.push(s.to_string());
        i
    }
}

fn build_op_index(ops: &[OpRecord]) -> OpIndex {
    let mut slices = Intern::new();
    let mut actors = Intern::new();
    let mut eoas = Intern::new();
    let mut contracts = Intern::new();
    let mut tags = Intern::new();
    let mut pools = Intern::new();
    let mut amms = Intern::new();
    let mut tx = Intern::new();
    let mut rows = Vec::with_capacity(ops.len());

    for o in ops {
        let slice_id = slices.get(&slice_key(&o.kind, o.subkind.as_deref()));
        let actor_id = actors.get(&o.competitor());
        let eoa_id = eoas.get(&o.eoa);
        let contract_id = o
            .contract
            .as_deref()
            .filter(|c| !c.is_empty())
            .map(|c| contracts.get(c));
        let tx_id = tx.get(&o.tx_hash);
        let tag_ids: Vec<serde_json::Value> = details_tags(o.details_json.as_deref())
            .iter()
            .map(|t| serde_json::json!(tags.get(t)))
            .collect();

        let mut seen_legs = HashSet::new();
        let mut leg_vals = Vec::new();
        for leg in parse_route(o.route_json.as_deref()) {
            let pool = leg.pool.as_deref().unwrap_or("");
            let amm = leg.amm.as_deref().unwrap_or("");
            if pool.is_empty() && amm.is_empty() {
                continue;
            }
            let pid = pools.get(pool);
            let aid = amms.get(amm);
            if seen_legs.insert((pid, aid)) {
                leg_vals.push(serde_json::json!([pid, aid]));
            }
        }

        let net = if o.excluded_from_net.unwrap_or(false) {
            serde_json::Value::Null
        } else {
            opt_f64_json(o.net_profit_usd)
        };
        rows.push(serde_json::json!([
            o.ts,
            slice_id,
            conf_code(&o.confidence),
            op_flags(o),
            actor_id,
            eoa_id,
            contract_id
                .map(serde_json::Value::from)
                .unwrap_or(serde_json::Value::Null),
            o.block_number,
            tx_id,
            net,
            opt_f64_json(o.profit_usd),
            opt_f64_json(o.gas_cost_usd),
            opt_f64_json(o.volume_usd),
            opt_f64_json(o.flashloan_fee_usd),
            tag_ids,
            leg_vals,
            o.id,
        ]));
    }

    OpIndex {
        slices: slices.vec,
        actors: actors.vec,
        eoas: eoas.vec,
        contracts: contracts.vec,
        tags: tags.vec,
        pools: pools.vec,
        amms: amms.vec,
        tx: tx.vec,
        rows,
    }
}

fn opt_f64_json(v: Option<f64>) -> serde_json::Value {
    match v {
        Some(x) => serde_json::json!(x),
        None => serde_json::Value::Null,
    }
}

fn details_tags(details_json: Option<&str>) -> Vec<String> {
    let Some(raw) = details_json.filter(|s| !s.is_empty()) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    v.get("tags")
        .and_then(|t| t.as_array())
        .map(|arr| {
            let mut seen = HashSet::new();
            arr.iter()
                .filter_map(|x| x.as_str().filter(|s| !s.trim().is_empty()))
                .filter(|s| seen.insert(*s))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn details_has_token(details_json: Option<&str>, needle: &str) -> bool {
    let Some(raw) = details_json.filter(|s| !s.is_empty()) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
        return raw.contains(needle);
    };
    for key in ["tags", "reasons", "pricing_issues"] {
        if let Some(arr) = v.get(key).and_then(|x| x.as_array()) {
            if arr.iter().any(|x| x.as_str() == Some(needle)) {
                return true;
            }
        }
    }
    false
}

fn reduce_kpi(ops: &[OpRecord], caps: &StoreCaps) -> KpiPanel {
    let mut kpi = KpiPanel {
        ops: ops.len() as i64,
        net_gating_available: caps.has_excluded_from_net,
        quality: QualityPanel {
            pricing_flags_available: caps.has_pricing_flags,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut eoas = HashSet::new();
    let mut comps = HashSet::new();
    for o in ops {
        eoas.insert(o.eoa.clone());
        comps.insert(o.competitor());
        if let Some(v) = o.volume_usd {
            kpi.volume_usd += v;
            kpi.volume_priced += 1;
        }
        if let Some(v) = o.profit_usd {
            kpi.gross_usd += v;
            kpi.gross_priced += 1;
            if v > kpi.highest_single_usd {
                kpi.highest_single_usd = v;
            }
            kpi.quality.ops_priced += 1;
        } else {
            kpi.quality.ops_unpriced += 1;
        }
        if let Some(v) = o.gas_cost_usd {
            kpi.gas_usd += v;
            kpi.gas_priced += 1;
        }
        if let Some(v) = o.flashloan_fee_usd {
            kpi.flash_fee_usd += v;
            kpi.flash_fee_priced += 1;
        }
        let excluded = o.excluded_from_net.unwrap_or(false);
        if excluded {
            kpi.quality.ops_excluded_from_net += 1;
        } else if let Some(v) = o.net_profit_usd {
            kpi.net_usd += v;
            kpi.net_priced += 1;
            if o.confidence == "exact" {
                kpi.quality.net_usd_exact += v;
            } else if o.confidence == "inferred" {
                kpi.quality.net_usd_inferred += v;
            }
        }
        if o.confidence == "exact" {
            kpi.quality.ops_exact += 1;
        } else if o.confidence == "inferred" {
            kpi.quality.ops_inferred += 1;
        }
        if o.usd_approximate.unwrap_or(false) {
            kpi.quality.ops_approx += 1;
        }
        if o.native_unpriced.unwrap_or(false) {
            kpi.quality.ops_native_unpriced += 1;
        }
        if o.multi_asset_pricing.unwrap_or(false) {
            kpi.quality.ops_multi_asset += 1;
        }
        if o.pricing_clamped.unwrap_or(false) {
            kpi.quality.ops_pricing_clamped += 1;
        }
        if o.flash_fee_unpriced.unwrap_or(false) {
            kpi.quality.ops_flash_fee_unpriced += 1;
        }
    }
    kpi.searchers = eoas.len() as i64;
    kpi.competitors = comps.len() as i64;
    kpi.quality.pricing_coverage_pct = if kpi.ops == 0 {
        100.0
    } else {
        (kpi.quality.ops_priced as f64 / kpi.ops as f64) * 100.0
    };
    kpi
}

fn auto_bucket_secs(window: &WindowSpec) -> i64 {
    let span = (window.until_ts - window.since_ts).max(1);
    let mut secs = if span <= 3600 {
        60
    } else if span <= 6 * 3600 {
        300
    } else if span <= 24 * 3600 {
        900
    } else if span <= 7 * 86400 {
        3600
    } else {
        86400
    };
    // Hard cap so a mis-clamped window cannot explode the artifact.
    const MAX_BUCKETS: i64 = 720;
    let buckets = span / secs + 1;
    if buckets > MAX_BUCKETS {
        secs = (span / MAX_BUCKETS).max(secs);
    }
    secs
}

fn reduce_series(
    ops: &[OpRecord],
    block_timestamps: &[i64],
    window: &WindowSpec,
    bucket_secs: i64,
    _caps: &StoreCaps,
) -> Vec<SeriesPoint> {
    let mut map: BTreeMap<i64, SeriesPoint> = BTreeMap::new();
    let mut searcher_sets: BTreeMap<i64, HashSet<String>> = BTreeMap::new();
    let start = (window.since_ts / bucket_secs) * bucket_secs;
    let end = window.until_ts;
    let mut t = start;
    // Guard against pathological ranges even after auto_bucket_secs.
    let mut n = 0i64;
    while t <= end && n < 2_000 {
        map.insert(
            t,
            SeriesPoint {
                bucket_start: t,
                ops: 0,
                net_usd: 0.0,
                blocks_indexed: 0,
                gross_usd: 0.0,
                gas_usd: 0.0,
                priced_ops: 0,
                searchers: 0,
            },
        );
        searcher_sets.insert(t, HashSet::new());
        t = t.saturating_add(bucket_secs);
        n += 1;
    }
    for o in ops {
        let b = (o.ts as i64 / bucket_secs) * bucket_secs;
        if let Some(p) = map.get_mut(&b) {
            p.ops += 1;
            let excluded = o.excluded_from_net.unwrap_or(false);
            if !excluded {
                if let Some(v) = o.net_profit_usd {
                    p.net_usd += v;
                }
            }
            if let Some(v) = o.profit_usd {
                p.gross_usd += v;
                p.priced_ops += 1;
            }
            if let Some(v) = o.gas_cost_usd {
                p.gas_usd += v;
            }
            if let Some(set) = searcher_sets.get_mut(&b) {
                set.insert(o.competitor());
            }
        }
    }
    for &ts in block_timestamps {
        let b = (ts / bucket_secs) * bucket_secs;
        if let Some(p) = map.get_mut(&b) {
            p.blocks_indexed += 1;
        }
    }
    for (b, set) in searcher_sets {
        if let Some(p) = map.get_mut(&b) {
            p.searchers = set.len() as i64;
        }
    }
    map.into_values().collect()
}

type KindKey = (String, Option<String>, String);
type KindAgg = (i64, f64, f64);

fn reduce_kind_matrix(ops: &[OpRecord], _caps: &StoreCaps) -> Vec<KindCell> {
    let mut map: HashMap<KindKey, KindAgg> = HashMap::new();
    for kind in MevKind::ALL {
        map.insert(
            (kind.as_str().to_string(), None, "exact".to_string()),
            (0, 0.0, 0.0),
        );
        map.insert(
            (kind.as_str().to_string(), None, "inferred".to_string()),
            (0, 0.0, 0.0),
        );
    }
    for o in ops {
        let key = (o.kind.clone(), o.subkind.clone(), o.confidence.clone());
        let entry = map.entry(key).or_insert((0, 0.0, 0.0));
        entry.0 += 1;
        if let Some(g) = o.profit_usd {
            entry.1 += g;
        }
        // Net is loss-gated: skip excluded_from_net rows (false when column absent).
        let excluded = o.excluded_from_net.unwrap_or(false);
        if !excluded {
            if let Some(n) = o.net_profit_usd {
                entry.2 += n;
            }
        }
    }
    let mut cells: Vec<KindCell> = map
        .into_iter()
        .map(
            |((kind, subkind, confidence), (ops, gross, net))| KindCell {
                kind,
                subkind,
                confidence,
                ops,
                gross_usd: gross,
                net_usd: net,
            },
        )
        .collect();
    cells.sort_by(|a, b| {
        a.kind
            .cmp(&b.kind)
            .then(a.subkind.cmp(&b.subkind))
            .then(a.confidence.cmp(&b.confidence))
    });
    cells
}

fn reduce_actors(
    ops: &[OpRecord],
    labels: &HashMap<String, String>,
    caps: &StoreCaps,
) -> Vec<ActorRow> {
    struct Acc {
        eoa: String,
        contract: Option<String>,
        ops: i64,
        gross: f64,
        net: f64,
        excluded: i64,
    }
    let mut map: HashMap<String, Acc> = HashMap::new();
    for o in ops {
        let key = o.competitor();
        let acc = map.entry(key.clone()).or_insert_with(|| Acc {
            eoa: o.eoa.clone(),
            contract: o.contract.clone(),
            ops: 0,
            gross: 0.0,
            net: 0.0,
            excluded: 0,
        });
        acc.ops += 1;
        if let Some(g) = o.profit_usd {
            acc.gross += g;
        }
        let excluded = o.excluded_from_net.unwrap_or(false);
        if excluded {
            acc.excluded += 1;
        } else if let Some(n) = o.net_profit_usd {
            acc.net += n;
        } else if !caps.has_excluded_from_net {
            // no-op
        }
    }
    let mut rows: Vec<ActorRow> = map
        .into_iter()
        .map(|(competitor, a)| ActorRow {
            label: labels
                .get(&competitor)
                .cloned()
                .or_else(|| labels.get(&a.eoa).cloned()),
            competitor,
            eoa: a.eoa,
            contract: a.contract,
            ops: a.ops,
            gross_usd: a.gross,
            net_usd: a.net,
            excluded_ops: a.excluded,
        })
        .collect();
    rows.sort_by(|a, b| {
        b.net_usd
            .partial_cmp(&a.net_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                b.gross_usd
                    .partial_cmp(&a.gross_usd)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then(b.ops.cmp(&a.ops))
            .then(a.competitor.cmp(&b.competitor))
    });
    rows
}

fn reduce_pools(ops: &[OpRecord], _caps: &StoreCaps) -> Vec<PoolRow> {
    struct Acc {
        ops: HashSet<i64>,
        gross: f64,
        net: f64,
        gas: f64,
        priced: i64,
        amm_mix: BTreeMap<String, i64>,
        comps: HashSet<String>,
        kinds: HashMap<String, i64>,
    }
    let mut map: HashMap<String, Acc> = HashMap::new();
    for o in ops {
        let legs = parse_route(o.route_json.as_deref());
        let pools: HashSet<String> = legs.iter().filter_map(|l| l.pool.clone()).collect();
        if pools.is_empty() {
            continue;
        }
        let excluded = o.excluded_from_net.unwrap_or(false);
        for pool in pools {
            let acc = map.entry(pool).or_insert_with(|| Acc {
                ops: HashSet::new(),
                gross: 0.0,
                net: 0.0,
                gas: 0.0,
                priced: 0,
                amm_mix: BTreeMap::new(),
                comps: HashSet::new(),
                kinds: HashMap::new(),
            });
            if acc.ops.insert(o.id) {
                if let Some(g) = o.profit_usd {
                    acc.gross += g;
                    acc.priced += 1;
                }
                if !excluded {
                    if let Some(n) = o.net_profit_usd {
                        acc.net += n;
                    }
                }
                if let Some(g) = o.gas_cost_usd {
                    acc.gas += g;
                }
                acc.comps.insert(o.competitor());
                *acc.kinds.entry(o.kind.clone()).or_insert(0) += 1;
            }
            for leg in &legs {
                if let Some(amm) = &leg.amm {
                    *acc.amm_mix.entry(amm.clone()).or_insert(0) += 1;
                }
            }
        }
    }
    let mut rows: Vec<PoolRow> = map
        .into_iter()
        .map(|(pool, a)| {
            let top_kind = a.kinds.into_iter().max_by_key(|(_, c)| *c).map(|(k, _)| k);
            PoolRow {
                pool,
                ops: a.ops.len() as i64,
                gross_usd: a.gross,
                net_usd: a.net,
                gas_usd: a.gas,
                priced_ops: a.priced,
                amm_mix: a.amm_mix,
                unique_competitors: a.comps.len() as i64,
                top_kind,
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        b.net_usd
            .partial_cmp(&a.net_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                b.gross_usd
                    .partial_cmp(&a.gross_usd)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then(b.ops.cmp(&a.ops))
            .then(a.pool.cmp(&b.pool))
    });
    rows
}

fn reduce_tokens(ops: &[OpRecord], _caps: &StoreCaps) -> Vec<TokenRow> {
    struct Acc {
        ops: i64,
        gross: f64,
        net: f64,
        priced: i64,
    }
    let mut map: HashMap<String, Acc> = HashMap::new();
    for o in ops {
        let Some(tok) = o.profit_token.as_ref() else {
            continue;
        };
        let acc = map.entry(tok.clone()).or_insert(Acc {
            ops: 0,
            gross: 0.0,
            net: 0.0,
            priced: 0,
        });
        acc.ops += 1;
        if let Some(g) = o.profit_usd {
            acc.gross += g;
            acc.priced += 1;
        }
        let excluded = o.excluded_from_net.unwrap_or(false);
        if !excluded {
            if let Some(n) = o.net_profit_usd {
                acc.net += n;
            }
        }
    }
    let mut rows: Vec<TokenRow> = map
        .into_iter()
        .map(|(token, a)| TokenRow {
            token,
            ops: a.ops,
            gross_usd: a.gross,
            net_usd: a.net,
            priced_ops: a.priced,
        })
        .collect();
    rows.sort_by(|a, b| {
        b.net_usd
            .partial_cmp(&a.net_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.ops.cmp(&a.ops))
            .then(a.token.cmp(&b.token))
    });
    rows
}

fn reduce_facets(
    ops: &[OpRecord],
    catalogue: &HashSet<String>,
    caps: &StoreCaps,
) -> (Vec<FacetRow>, Vec<FacetRow>, i64) {
    fn bump(map: &mut HashMap<String, (i64, f64, f64)>, key: &str, o: &OpRecord, caps: &StoreCaps) {
        let e = map.entry(key.to_string()).or_insert((0, 0.0, 0.0));
        e.0 += 1;
        if let Some(g) = o.profit_usd {
            e.1 += g;
        }
        let excluded = o.excluded_from_net.unwrap_or(false);
        if !excluded {
            if let Some(n) = o.net_profit_usd {
                e.2 += n;
            }
        }
        let _ = caps;
    }
    let mut tags: HashMap<String, (i64, f64, f64)> = HashMap::new();
    let mut reasons: HashMap<String, (i64, f64, f64)> = HashMap::new();
    let mut untagged_ops = 0;
    for o in ops {
        let op_tags = details_tags(o.details_json.as_deref());
        if op_tags.is_empty() {
            untagged_ops += 1;
        }
        for t in &op_tags {
            bump(&mut tags, t, o, caps);
        }
        // Facets need only the details arrays, not routes or enriched evidence.
        if let Some(details) = o
            .details_json
            .as_deref()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        {
            if let Some(arr) = details.get("reasons").and_then(|v| v.as_array()) {
                let mut seen = HashSet::new();
                for r in arr
                    .iter()
                    .filter_map(|v| v.as_str())
                    .filter(|s| seen.insert(*s))
                {
                    bump(&mut reasons, r, o, caps);
                }
            }
        }
    }
    let to_rows = |map: HashMap<String, (i64, f64, f64)>, cat: &HashSet<String>| {
        let mut rows: Vec<FacetRow> = map
            .into_iter()
            .map(|(value, (ops, gross, net))| FacetRow {
                out_of_catalogue: !cat.is_empty() && !cat.contains(&value),
                value,
                ops,
                gross_usd: gross,
                net_usd: net,
            })
            .collect();
        rows.sort_by(|a, b| b.ops.cmp(&a.ops).then(a.value.cmp(&b.value)));
        rows
    };
    (
        to_rows(tags, catalogue),
        to_rows(reasons, &HashSet::new()),
        untagged_ops,
    )
}

fn event_row(o: &OpRecord) -> EventRow {
    let details = o
        .details_json
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok());
    let pnl_basis = details.as_ref().and_then(|v| {
        v.get("pnl_basis")
            .and_then(|b| b.as_str())
            .map(str::to_string)
    });
    let tags = details_tags(o.details_json.as_deref());
    EventRow {
        id: o.id,
        block_number: o.block_number,
        tx_index: o.tx_index,
        tx_hash: o.tx_hash.clone(),
        ts: o.ts,
        kind: o.kind.clone(),
        subkind: o.subkind.clone(),
        confidence: o.confidence.clone(),
        eoa: o.eoa.clone(),
        contract: o.contract.clone(),
        profit_usd: o.profit_usd,
        net_profit_usd: o.net_profit_usd,
        gas_cost_usd: o.gas_cost_usd,
        flashloan_fee_usd: o.flashloan_fee_usd,
        economics: OperationEconomics::from_record(o),
        volume_usd: o.volume_usd,
        detector: o.detector.clone(),
        excluded_from_net: o.excluded_from_net.unwrap_or(false),
        pnl_basis,
        tags,
    }
}

/// All reject reasons with explicit zeros (P12).
pub fn reject_ladder(counts: &HashMap<String, i64>) -> Vec<RejectRow> {
    const ALL: &[RejectReason] = &[
        RejectReason::NoPool,
        RejectReason::NoPath,
        RejectReason::QuoteNonpositive,
        RejectReason::BelowMinProfit,
        RejectReason::GasDominates,
        RejectReason::MaxCandidates,
        RejectReason::StateStale,
        RejectReason::UnresolvedCycle,
        RejectReason::MixedFlowOwner,
        RejectReason::NetZeroSearcher,
        RejectReason::MaterialDeficit,
        RejectReason::OpenInventory,
        RejectReason::BuilderTip,
        RejectReason::Other,
        RejectReason::CycleNotClosed,
        RejectReason::NoCounterpartyLeg,
        RejectReason::CausalWindowTooWide,
        RejectReason::CausalPriceMoveBelowFloor,
        RejectReason::SkimPoolNotV2Like,
        RejectReason::SkimLooksLikeSwap,
        RejectReason::JitPairingUnmatched,
        RejectReason::LiquidationNotReconciled,
    ];
    ALL.iter()
        .map(|r| {
            let reason = r.as_str().to_string();
            RejectRow {
                count: *counts.get(&reason).unwrap_or(&0),
                reason,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explorer::dashboard::caps::StoreCaps;
    use crate::explorer::dashboard::{OpRecord, WindowSpec};

    #[allow(clippy::too_many_arguments)]
    fn op(
        id: i64,
        ts: u64,
        kind: &str,
        subkind: Option<&str>,
        confidence: &str,
        net: Option<f64>,
        gross: Option<f64>,
        gas: Option<f64>,
        eoa: &str,
        contract: Option<&str>,
        details: Option<&str>,
        route: Option<&str>,
    ) -> OpRecord {
        OpRecord {
            id,
            block_number: 100 + id as u64,
            tx_index: Some(0),
            tx_hash: format!("0x{id:064x}"),
            ts,
            kind: kind.to_string(),
            kind_raw: kind.to_string(),
            eoa: eoa.to_string(),
            contract: contract.map(str::to_string),
            confidence: confidence.to_string(),
            canonical_id: None,
            profit_token: None,
            profit_amount: None,
            profit_usd: gross,
            gas_cost_usd: gas,
            flashloan_fee_usd: None,
            volume_usd: gross,
            net_profit_usd: net,
            route_json: route.map(str::to_string),
            victim_hashes: None,
            details_json: details.map(str::to_string),
            detector: "test".into(),
            created_at: ts,
            subkind: subkind.map(str::to_string),
            excluded_from_net: Some(false),
            usd_approximate: Some(false),
            native_unpriced: Some(false),
            multi_asset_pricing: Some(false),
            pricing_clamped: Some(false),
            flash_fee_unpriced: Some(false),
        }
    }

    #[test]
    fn strategy_series_includes_first_partial_bucket() {
        let bucket = 3600i64;
        // Window starts mid-bucket; flooring should still count the bucket.
        let window = WindowSpec {
            label: "test".into(),
            since_ts: 1_700_000_123,
            until_ts: 1_700_003_600,
        };
        let ops = vec![op(
            1,
            1_700_000_200,
            "arb_atomic",
            None,
            "exact",
            Some(1.0),
            Some(1.5),
            Some(0.1),
            "0xeoa1",
            None,
            None,
            None,
        )];
        let ss = reduce_strategy_series(&ops, &window, bucket);
        assert_eq!(ss.len(), 1);
        assert_eq!(ss[0].ops, 1);
        assert_eq!(ss[0].confidence, "exact");
        let floored = (1_700_000_200i64 / bucket) * bucket;
        assert_eq!(ss[0].bucket_start, floored);
    }

    #[test]
    fn strategy_series_splits_confidence() {
        let window = WindowSpec {
            label: "t".into(),
            since_ts: 0,
            until_ts: 10_000,
        };
        let ops = vec![
            op(
                1,
                100,
                "jit",
                None,
                "exact",
                Some(1.0),
                Some(1.0),
                None,
                "0xa",
                None,
                None,
                None,
            ),
            op(
                2,
                100,
                "jit",
                None,
                "inferred",
                Some(2.0),
                Some(2.0),
                None,
                "0xb",
                None,
                None,
                None,
            ),
        ];
        let ss = reduce_strategy_series(&ops, &window, 3600);
        assert_eq!(ss.len(), 2);
        let mut confs: Vec<_> = ss.iter().map(|p| p.confidence.as_str()).collect();
        confs.sort();
        assert_eq!(confs, vec!["exact", "inferred"]);
    }

    #[test]
    fn series_extended_fields_and_searchers() {
        let window = WindowSpec {
            label: "t".into(),
            since_ts: 0,
            until_ts: 3_600,
        };
        let caps = StoreCaps {
            user_version: 0,
            schema_head: 0,
            has_subkind: true,
            has_excluded_from_net: true,
            has_volume_usd: true,
            has_flashloan_fee_usd: true,
            has_pricing_flags: true,
            has_ops_ts_index: false,
            has_ops_natural_key: false,
            tables: vec![],
        };
        let ops = vec![
            op(
                1,
                10,
                "arb_atomic",
                None,
                "exact",
                Some(5.0),
                Some(6.0),
                Some(0.5),
                "0xeoa1",
                Some("0xc1"),
                None,
                None,
            ),
            op(
                2,
                20,
                "arb_atomic",
                None,
                "exact",
                Some(1.0),
                Some(2.0),
                Some(0.2),
                "0xeoa2",
                Some("0xc1"),
                None,
                None,
            ),
        ];
        let series = reduce_series(&ops, &[], &window, 3600, &caps);
        let p = series.iter().find(|p| p.ops > 0).expect("bucket");
        assert_eq!(p.ops, 2);
        assert!((p.gross_usd - 8.0).abs() < 1e-9);
        assert!((p.gas_usd - 0.7).abs() < 1e-9);
        assert_eq!(p.priced_ops, 2);
        assert_eq!(p.searchers, 1); // same contract competitor
    }

    #[test]
    fn scatter_includes_eoa_contract_block() {
        let ops = vec![op(
            7,
            50,
            "backrun",
            None,
            "exact",
            Some(1.0),
            Some(1.0),
            None,
            "0xeoa7",
            Some("0xcontract"),
            Some(r#"{"tags":["VOLUME_PARTIAL"]}"#),
            None,
        )];
        let pts = reduce_scatter_points(&ops, 100);
        assert_eq!(pts.len(), 1);
        assert_eq!(pts[0].eoa, "0xeoa7");
        assert_eq!(pts[0].contract.as_deref(), Some("0xcontract"));
        assert_eq!(pts[0].block_number, 107);
        assert!(pts[0].volume_partial);
    }

    #[test]
    fn op_index_row_count_matches_ops() {
        let ops = vec![
            op(
                1,
                1,
                "jit",
                None,
                "exact",
                Some(1.0),
                Some(1.0),
                None,
                "0xa",
                None,
                Some(r#"{"tags":["flash_arb"]}"#),
                Some(r#"[{"pool":"0xpool","amm":"v3"}]"#),
            ),
            op(
                2,
                2,
                "utility",
                Some("bundler"),
                "inferred",
                None,
                None,
                None,
                "0xb",
                Some("0xc"),
                None,
                None,
            ),
        ];
        let idx = build_op_index(&ops);
        assert_eq!(idx.rows.len(), 2);
        assert!(idx.slices.iter().any(|s| s == "jit"));
        assert!(idx.slices.iter().any(|s| s == "utility/bundler"));
        assert!(idx.amms.iter().any(|a| a == "v3"));
        assert!(idx.tags.iter().any(|t| t == "flash_arb"));
    }

    fn caps() -> StoreCaps {
        StoreCaps {
            user_version: 0,
            schema_head: 0,
            has_subkind: true,
            has_excluded_from_net: true,
            has_volume_usd: true,
            has_flashloan_fee_usd: true,
            has_pricing_flags: true,
            has_ops_ts_index: false,
            has_ops_natural_key: false,
            tables: vec![],
        }
    }

    /// Numerator and denominator come from the same eligible cohort; known
    /// losses (even excluded_from_net rows) stay in the comparison, while
    /// bundler gas-as-profit and unverified JIT do not.
    #[test]
    fn strategy_economics_matched_cohort_keeps_losses() {
        let mut loss = op(
            2,
            20,
            "arb_atomic",
            None,
            "exact",
            Some(-3.0),
            Some(-2.0),
            Some(1.0),
            "0xb",
            None,
            Some("{}"),
            None,
        );
        loss.excluded_from_net = Some(true);
        let ops = vec![
            op(
                1,
                10,
                "arb_atomic",
                None,
                "exact",
                Some(9.0),
                Some(10.0),
                Some(1.0),
                "0xa",
                None,
                Some("{}"),
                None,
            ),
            loss,
            op(
                3,
                30,
                "utility",
                Some("bundler"),
                "exact",
                Some(5.0),
                Some(5.0),
                Some(0.0),
                "0xc",
                None,
                Some(r#"{"pnl_basis":"gas_cost_as_profit"}"#),
                None,
            ),
            op(
                4,
                40,
                "jit",
                None,
                "exact",
                Some(1.0),
                Some(1.0),
                Some(0.1),
                "0xd",
                None,
                None,
                None,
            ),
        ];
        let rows = reduce_strategy_economics(&ops);
        let arb = rows.iter().find(|r| r.slice_key == "arb_atomic").unwrap();
        assert_eq!(arb.ops_total, 2);
        assert_eq!(arb.ops_eligible, 2);
        assert_eq!(arb.observed_losses, 1);
        assert_eq!(arb.included_excluded_from_net, 1);
        assert!((arb.gross_usd.unwrap() - 8.0).abs() < 1e-9);
        assert!((arb.cost_usd.unwrap() - 2.0).abs() < 1e-9);
        assert!((arb.net_usd.unwrap() - 6.0).abs() < 1e-9);
        assert!((arb.roi_pct.unwrap() - 300.0).abs() < 1e-9);
        assert!((arb.net_per_10.unwrap() - 30.0).abs() < 1e-9);
        let bundler = rows
            .iter()
            .find(|r| r.slice_key == "utility/bundler")
            .unwrap();
        assert_eq!(bundler.ops_eligible, 0);
        assert!(bundler.exclusion_reasons.contains_key("gas_cost_as_profit"));
        let jit = rows.iter().find(|r| r.slice_key == "jit").unwrap();
        assert_eq!(jit.ops_eligible, 0);
        assert!(jit.exclusion_reasons.contains_key("jit_unverified"));
    }

    /// Zero recorded cost is N/A, never Infinity or a fabricated zero ratio.
    #[test]
    fn strategy_economics_zero_cost_is_na() {
        let ops = vec![op(
            1,
            10,
            "arb_atomic",
            None,
            "exact",
            Some(5.0),
            Some(5.0),
            Some(0.0),
            "0xa",
            None,
            Some("{}"),
            None,
        )];
        let rows = reduce_strategy_economics(&ops);
        let row = &rows[0];
        assert_eq!(row.ops_eligible, 1);
        assert_eq!(row.roi_pct, None);
        assert_eq!(row.net_per_10, None);
        assert_eq!(row.gross_per_10, None);
    }

    /// Tag membership is counted once per operation, and truncation/budget
    /// caps never move the full-window economics or tag statistics.
    #[test]
    fn tags_and_economics_are_invariant_to_truncation() {
        let ops = vec![
            op(
                1,
                10,
                "arb_atomic",
                None,
                "exact",
                Some(9.0),
                Some(10.0),
                Some(1.0),
                "0xa",
                None,
                Some(r#"{"tags":["flash_arb","flash_arb","multi_hop"]}"#),
                None,
            ),
            op(
                2,
                20,
                "arb_atomic",
                None,
                "exact",
                Some(1.0),
                Some(2.0),
                Some(1.0),
                "0xb",
                None,
                Some(r#"{"tags":["flash_arb"]}"#),
                None,
            ),
        ];
        let window = WindowSpec {
            label: "t".into(),
            since_ts: 0,
            until_ts: 3_600,
        };
        let labels = HashMap::new();
        let catalogue = HashSet::new();
        let full = reduce(&ops, &caps(), &window, &[], &labels, &catalogue, 100, 100);
        let trimmed = reduce(&ops, &caps(), &window, &[], &labels, &catalogue, 1, 0);
        assert_eq!(full.strategy_economics, trimmed.strategy_economics);
        assert_eq!(full.tag_facets, trimmed.tag_facets);
        let flash = full
            .tag_facets
            .iter()
            .find(|t| t.value == "flash_arb")
            .unwrap();
        assert_eq!(flash.ops, 2, "duplicate tag in one op counts once");
        assert_eq!(trimmed.events.len(), 1);
        assert_eq!(trimmed.evidence.len(), 0);
    }

    /// Distinct tags on one operation are separate facets; a repeated tag and
    /// a repeated reason are each counted once, and tags never leak into the
    /// reason facet (or vice versa).
    #[test]
    fn overlapping_tags_and_reasons_count_once_per_operation() {
        let ops = vec![
            op(
                1,
                10,
                "arb_atomic",
                None,
                "exact",
                Some(1.0),
                Some(1.0),
                Some(0.1),
                "0xa",
                None,
                Some(
                    r#"{"tags":["flash_arb","multi_hop","flash_arb"],
                        "reasons":["price_move_low","price_move_low"]}"#,
                ),
                None,
            ),
            op(
                2,
                20,
                "arb_atomic",
                None,
                "exact",
                Some(2.0),
                Some(2.0),
                Some(0.1),
                "0xb",
                None,
                Some(r#"{"tags":["multi_hop"],"reasons":["price_move_low"]}"#),
                None,
            ),
        ];
        let window = WindowSpec {
            label: "t".into(),
            since_ts: 0,
            until_ts: 3_600,
        };
        let panels = reduce(
            &ops,
            &caps(),
            &window,
            &[],
            &HashMap::new(),
            &HashSet::new(),
            100,
            100,
        );
        let facet = |rows: &[FacetRow], value: &str| {
            rows.iter()
                .find(|r| r.value == value)
                .map(|r| r.ops)
                .unwrap_or(0)
        };
        assert_eq!(facet(&panels.tag_facets, "flash_arb"), 1);
        assert_eq!(facet(&panels.tag_facets, "multi_hop"), 2);
        assert_eq!(panels.tag_facets.len(), 2, "no reason leaked into tags");
        assert_eq!(facet(&panels.reason_facets, "price_move_low"), 2);
        assert_eq!(panels.reason_facets.len(), 1, "no tag leaked into reasons");
        assert_eq!(panels.untagged_ops, 0);
    }

    #[test]
    fn empty_ops_reduce_to_zeroed_panels() {
        let window = WindowSpec {
            label: "empty".into(),
            since_ts: 0,
            until_ts: 3_600,
        };
        let panels = reduce(
            &[],
            &caps(),
            &window,
            &[],
            &HashMap::new(),
            &HashSet::new(),
            100,
            100,
        );
        assert_eq!(panels.kpi.ops, 0);
        assert_eq!(panels.kpi.net_usd, 0.0);
        assert!(panels.tag_facets.is_empty());
        assert!(panels.reason_facets.is_empty());
        assert!(panels.strategy_economics.is_empty());
        assert!(panels.events.is_empty());
        assert!(panels.evidence.is_empty());
        assert_eq!(panels.untagged_ops, 0);
        assert!(panels
            .series
            .iter()
            .all(|p| p.ops == 0 && p.blocks_indexed == 0));
    }

    /// A flash fee already netted into the stored residual is added back to
    /// gross and counted once in cost; `net` is not charged twice.
    #[test]
    fn strategy_economics_reconstructs_netted_flash_fee() {
        let mut o = op(
            1,
            10,
            "arb_atomic",
            None,
            "exact",
            Some(8.0),
            // Profit is already net of the 1.0 flash fee but gross of gas.
            Some(9.0),
            Some(1.0),
            "0xa",
            None,
            Some(r#"{"flashloan_fee_netted_in_ledger":true,"flash_premium":1000000000000000000}"#),
            None,
        );
        o.flashloan_fee_usd = Some(1.0);
        let rows = reduce_strategy_economics(&[o]);
        let row = &rows[0];
        assert_eq!(row.ops_total, 1);
        assert_eq!(row.ops_eligible, 1);
        assert!((row.gross_usd.unwrap() - 10.0).abs() < 1e-9);
        assert!((row.cost_usd.unwrap() - 2.0).abs() < 1e-9);
        assert!((row.net_usd.unwrap() - 8.0).abs() < 1e-9);
        assert!((row.roi_pct.unwrap() - 400.0).abs() < 1e-9);
    }
}
