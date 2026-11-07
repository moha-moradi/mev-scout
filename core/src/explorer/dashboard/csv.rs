//! CSV export aligned with bare `explorer` report columns.
//!
//! Header (lockstep with `cli/.../report.rs::csv_lines`):
//! `window,section,label,ops,volume_usd,gross_usd,gas_usd,flash_fee_usd,net_usd`
use std::collections::BTreeMap;

use super::DashboardSnapshot;

const HEADER: &str = "window,section,label,ops,volume_usd,gross_usd,gas_usd,flash_fee_usd,net_usd";

/// Emit report-compatible CSV for one dashboard snapshot.
pub fn render_csv(snap: &DashboardSnapshot) -> String {
    let w = &snap.window.label;
    let mut lines = vec![HEADER.to_string()];

    let k = &snap.panels.kpi;
    lines.push(csv_row(
        w,
        "overview",
        "total",
        &[
            k.ops.to_string(),
            usd(k.volume_usd),
            usd(k.gross_usd),
            usd(k.gas_usd),
            usd(k.flash_fee_usd),
            usd(k.net_usd),
        ],
    ));

    for (kind, agg) in by_kind_rollup(snap) {
        lines.push(csv_row(
            w,
            "by_kind",
            &kind,
            &[
                agg.ops.to_string(),
                usd(agg.volume),
                usd(agg.gross),
                usd(agg.gas),
                usd(agg.flash),
                usd(agg.net),
            ],
        ));
    }

    for (day, agg) in daily_rollup(snap) {
        lines.push(csv_row(
            w,
            "daily",
            &day,
            &[
                agg.ops.to_string(),
                usd(agg.volume),
                usd(agg.gross),
                usd(agg.gas),
                usd(agg.flash),
                usd(agg.net),
            ],
        ));
    }

    for a in snap.panels.actors.iter().take(100) {
        lines.push(csv_row(
            w,
            "top_competitors",
            &a.competitor,
            &[
                a.ops.to_string(),
                String::new(),
                usd(a.gross_usd),
                String::new(),
                String::new(),
                usd(a.net_usd),
            ],
        ));
    }

    // Senders: collapse actors by eoa (same shape as report top_senders).
    let mut senders: BTreeMap<String, (i64, f64, f64)> = BTreeMap::new();
    for a in &snap.panels.actors {
        let e = senders.entry(a.eoa.clone()).or_insert((0, 0.0, 0.0));
        e.0 += a.ops;
        e.1 += a.gross_usd;
        e.2 += a.net_usd;
    }
    let mut sender_rows: Vec<_> = senders.into_iter().collect();
    sender_rows.sort_by(|a, b| {
        b.1 .2
            .total_cmp(&a.1 .2)
            .then(b.1 .1.total_cmp(&a.1 .1))
            .then(a.0.cmp(&b.0))
    });
    for (eoa, (ops, gross, net)) in sender_rows.into_iter().take(100) {
        lines.push(csv_row(
            w,
            "top_senders",
            &eoa,
            &[
                ops.to_string(),
                String::new(),
                usd(gross),
                String::new(),
                String::new(),
                usd(net),
            ],
        ));
    }

    for p in snap.panels.pools.iter().take(100) {
        lines.push(csv_row(
            w,
            "top_pools",
            &p.pool,
            &[
                p.ops.to_string(),
                String::new(),
                usd(p.gross_usd),
                String::new(),
                String::new(),
                usd(p.net_usd),
            ],
        ));
    }

    let mut top_ops = snap.panels.events.clone();
    top_ops.sort_by(|a, b| {
        let an = a.net_profit_usd.unwrap_or(f64::NEG_INFINITY);
        let bn = b.net_profit_usd.unwrap_or(f64::NEG_INFINITY);
        bn.total_cmp(&an).then(a.tx_hash.cmp(&b.tx_hash))
    });
    for op in top_ops.into_iter().take(100) {
        // Match report CSV: NULL USD coerced to 0.0 for this section only.
        lines.push(csv_row(
            w,
            "top_ops",
            &op.tx_hash,
            &[
                "1".to_string(),
                usd(op.volume_usd.unwrap_or(0.0)),
                usd(op.profit_usd.unwrap_or(0.0)),
                usd(op.gas_cost_usd.unwrap_or(0.0)),
                String::new(),
                usd(op.net_profit_usd.unwrap_or(0.0)),
            ],
        ));
    }

    for r in &snap.rejects.ladder {
        if r.count == 0 {
            continue;
        }
        lines.push(csv_row(
            w,
            "classify_rejects",
            &r.reason,
            &[
                r.count.to_string(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
            ],
        ));
    }

    lines.join("\n") + "\n"
}

#[derive(Default, Clone, Copy)]
struct Agg {
    ops: i64,
    volume: f64,
    gross: f64,
    gas: f64,
    flash: f64,
    net: f64,
}

fn by_kind_rollup(snap: &DashboardSnapshot) -> Vec<(String, Agg)> {
    let mut map: BTreeMap<String, Agg> = BTreeMap::new();
    for cell in &snap.panels.kind_matrix {
        let e = map.entry(cell.kind.clone()).or_default();
        e.ops += cell.ops;
        e.gross += cell.gross_usd;
        e.net += cell.net_usd;
    }
    // Fill volume/gas/flash from events (kind_matrix lacks them).
    for ev in &snap.panels.events {
        let e = map.entry(ev.kind.clone()).or_default();
        if let Some(v) = ev.volume_usd {
            e.volume += v;
        }
        if let Some(v) = ev.gas_cost_usd {
            e.gas += v;
        }
    }
    let mut rows: Vec<_> = map.into_iter().filter(|(_, a)| a.ops > 0).collect();
    rows.sort_by(|a, b| b.1.gross.total_cmp(&a.1.gross).then(a.0.cmp(&b.0)));
    rows
}

fn daily_rollup(snap: &DashboardSnapshot) -> Vec<(String, Agg)> {
    let mut map: BTreeMap<String, Agg> = BTreeMap::new();
    for ev in &snap.panels.events {
        let day = unix_day_label(ev.ts);
        let e = map.entry(day).or_default();
        e.ops += 1;
        if let Some(v) = ev.volume_usd {
            e.volume += v;
        }
        if let Some(v) = ev.profit_usd {
            e.gross += v;
        }
        if let Some(v) = ev.gas_cost_usd {
            e.gas += v;
        }
        if !ev.excluded_from_net {
            if let Some(v) = ev.net_profit_usd {
                e.net += v;
            }
        }
    }
    let mut rows: Vec<_> = map.into_iter().collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    rows
}

fn unix_day_label(ts: u64) -> String {
    // UTC calendar day via chrono-free arithmetic (report uses sqlite date()).
    let days = (ts / 86_400) as i64;
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Howard Hinnant civil_from_days (UTC).
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

fn usd(x: f64) -> String {
    format!("{x:.2}")
}

fn csv_row(window: &str, section: &str, label: &str, cols: &[String]) -> String {
    let mut parts = vec![
        window.to_string(),
        section.to_string(),
        escape_csv_field(label),
    ];
    parts.extend(cols.iter().cloned());
    parts.join(",")
}

fn escape_csv_field(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explorer::dashboard::{build, BuildOptions, WindowSpec};
    use crate::explorer::store::ExplorerStore;

    #[test]
    fn csv_header_matches_report_contract() {
        let dir = std::env::temp_dir().join(format!("mev_dash_csv_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("explorer-avalanche.sqlite");
        {
            let store = ExplorerStore::open(&path).expect("create");
            store.set_sync_state(43114, 1, 1).expect("sync");
        }
        let snap = build(&BuildOptions {
            db_path: path,
            window: WindowSpec::all(),
            ..BuildOptions::default()
        })
        .expect("build");
        let csv = render_csv(&snap);
        let first = csv.lines().next().expect("header");
        assert_eq!(first, HEADER);
        assert!(csv.contains(",overview,total,"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn civil_day_known_epoch() {
        // 1700000000 = 2023-11-14 UTC
        assert_eq!(unix_day_label(1_700_000_000), "2023-11-14");
    }
}
