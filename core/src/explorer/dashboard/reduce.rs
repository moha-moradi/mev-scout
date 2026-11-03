//! Single reducer: KPIs, series, matrices, leaderboards, facets from OpRecords.
use std::collections::{BTreeMap, HashMap, HashSet};

use serde::Serialize;

use crate::explorer::reject::RejectReason;
use crate::explorer::types::MevKind;

use super::caps::StoreCaps;
use super::evidence::{parse_route, OpEvidence};
use super::{OpRecord, WindowSpec};

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

#[derive(Debug, Clone, Serialize)]
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
    pub volume_usd: Option<f64>,
    pub detector: String,
    pub excluded_from_net: bool,
    pub pnl_basis: Option<String>,
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
    pub events: Vec<EventRow>,
    pub evidence: Vec<OpEvidence>,
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
}

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
    let (tag_facets, reason_facets) = reduce_facets(ops, catalogue_tags, caps);

    let ops_total = ops.len();
    let emit_ops = ops.len().min(max_ops);
    let events: Vec<EventRow> = ops
        .iter()
        .take(emit_ops)
        .map(event_row)
        .collect();
    let evidence_n = emit_ops.min(max_evidence);
    let evidence: Vec<OpEvidence> = ops
        .iter()
        .take(evidence_n)
        .map(|o| {
            super::evidence::build_evidence(
                &o.tx_hash,
                o.block_number,
                &o.kind,
                o.subkind.as_deref(),
                &o.confidence,
                &o.detector,
                &o.eoa,
                o.contract.as_deref(),
                o.profit_usd,
                o.net_profit_usd,
                o.gas_cost_usd,
                o.volume_usd,
                o.flashloan_fee_usd,
                o.profit_token.as_deref(),
                o.profit_amount.as_deref(),
                o.excluded_from_net.unwrap_or(false),
                o.route_json.as_deref(),
                o.victim_hashes.as_deref(),
                o.details_json.as_deref(),
            )
        })
        .collect();

    let hit_budget = ops_total > max_ops || ops_total > max_evidence;
    let ops_emitted = events.len();
    let evidence_emitted = evidence.len();
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
        events,
        evidence,
        truncation: TruncationInfo {
            max_ops,
            ops_total,
            ops_emitted,
            max_evidence,
            evidence_emitted,
            hit_budget,
        },
    }
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
    if span <= 3600 {
        60
    } else if span <= 6 * 3600 {
        300
    } else if span <= 24 * 3600 {
        900
    } else if span <= 7 * 86400 {
        3600
    } else {
        86400
    }
}

fn reduce_series(
    ops: &[OpRecord],
    block_timestamps: &[i64],
    window: &WindowSpec,
    bucket_secs: i64,
    _caps: &StoreCaps,
) -> Vec<SeriesPoint> {
    let mut map: BTreeMap<i64, SeriesPoint> = BTreeMap::new();
    let start = (window.since_ts / bucket_secs) * bucket_secs;
    let end = window.until_ts;
    let mut t = start;
    while t <= end {
        map.insert(
            t,
            SeriesPoint {
                bucket_start: t,
                ops: 0,
                net_usd: 0.0,
                blocks_indexed: 0,
            },
        );
        t += bucket_secs;
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
        }
    }
    for &ts in block_timestamps {
        let b = (ts / bucket_secs) * bucket_secs;
        if let Some(p) = map.get_mut(&b) {
            p.blocks_indexed += 1;
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
        .map(|((kind, subkind, confidence), (ops, gross, net))| KindCell {
            kind,
            subkind,
            confidence,
            ops,
            gross_usd: gross,
            net_usd: net,
        })
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
            label: labels.get(&competitor).cloned().or_else(|| {
                labels
                    .get(&a.eoa)
                    .cloned()
            }),
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
        let pools: HashSet<String> = legs
            .iter()
            .filter_map(|l| l.pool.clone())
            .collect();
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
            let top_kind = a
                .kinds
                .into_iter()
                .max_by_key(|(_, c)| *c)
                .map(|(k, _)| k);
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
) -> (Vec<FacetRow>, Vec<FacetRow>) {
    fn bump(
        map: &mut HashMap<String, (i64, f64, f64)>,
        key: &str,
        o: &OpRecord,
        caps: &StoreCaps,
    ) {
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
    for o in ops {
        let ev = super::evidence::build_evidence(
            &o.tx_hash,
            o.block_number,
            &o.kind,
            o.subkind.as_deref(),
            &o.confidence,
            &o.detector,
            &o.eoa,
            o.contract.as_deref(),
            o.profit_usd,
            o.net_profit_usd,
            o.gas_cost_usd,
            o.volume_usd,
            o.flashloan_fee_usd,
            o.profit_token.as_deref(),
            o.profit_amount.as_deref(),
            o.excluded_from_net.unwrap_or(false),
            o.route_json.as_deref(),
            o.victim_hashes.as_deref(),
            o.details_json.as_deref(),
        );
        for t in &ev.tags {
            bump(&mut tags, t, o, caps);
        }
        for r in &ev.reasons {
            bump(&mut reasons, r, o, caps);
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
    (to_rows(tags, catalogue), to_rows(reasons, &HashSet::new()))
}

fn event_row(o: &OpRecord) -> EventRow {
    let pnl_basis = o
        .details_json
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| {
            v.get("pnl_basis")
                .and_then(|b| b.as_str())
                .map(str::to_string)
        });
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
        volume_usd: o.volume_usd,
        detector: o.detector.clone(),
        excluded_from_net: o.excluded_from_net.unwrap_or(false),
        pnl_basis,
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
        RejectReason::VictimSetEmpty,
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
