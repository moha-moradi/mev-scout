//! Bare ``explorer`` — revenue report: cost, profit, and volume per time
//! window (1d/7d/30d default), broken out per MEV kind, with a daily trend,
//! competitors / top searchers/pools, and a top-op detail list. Pure SQL over
//! the store — requires history (see ``explorer index``).
use super::*;

use serde::Serialize;

use mev_scout_core::chain::timing::block_time_secs;
use mev_scout_core::explorer::store::{RejectCount, ReportOverview, ReportRow, StatsRow};
use mev_scout_core::explorer::Confidence;
use mev_scout_core::types::OutputFormat;
use mev_scout_core::utils::epoch_secs;

/// One window's worth of report data, ready for table / JSON / CSV rendering.
#[derive(Debug, Clone, Serialize)]
struct WindowReport {
    window: String,
    /// Fraction of expected blocks indexed in the window (0..=1); `None` for
    /// the `all` window where expected is unbounded.
    #[serde(skip_serializing_if = "Option::is_none")]
    coverage_pct: Option<f64>,
    blocks: i64,
    overview: ReportOverview,
    by_kind: Vec<ReportRow>,
    daily: Vec<ReportRow>,
    top_competitors: Vec<StatsRow>,
    top_senders: Vec<StatsRow>,
    top_pools: Vec<StatsRow>,
    top_ops: Vec<MevOpRow>,
    /// `explorer_classify` reject counts by reason (strategy record_rejections).
    classify_rejects: Vec<RejectCount>,
}

/// A report window: a rolling duration (`12h`, `1d`, `2w`) or all-time.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ReportWindow {
    Rolling { label: String, secs: u64 },
    AllTime,
}

impl ReportWindow {
    fn label(&self) -> &str {
        match self {
            Self::Rolling { label, .. } => label,
            Self::AllTime => "all",
        }
    }

    fn since_ts(&self, now: u64) -> u64 {
        match self {
            Self::Rolling { secs, .. } => now.saturating_sub(*secs),
            Self::AllTime => 0,
        }
    }

    /// Expected blocks in this window using the chain's authoritative block
    /// time; `None` for all-time, where the span is unbounded.
    fn expected_blocks(&self, chain: ChainName) -> Option<u64> {
        match self {
            Self::Rolling { secs, .. } => Some(secs / block_time_secs(chain) as u64),
            Self::AllTime => None,
        }
    }
}

fn parse_window(s: &str) -> anyhow::Result<ReportWindow> {
    if s == "all" {
        return Ok(ReportWindow::AllTime);
    }
    let secs = parse_duration_secs(s)
        .ok_or_else(|| anyhow::anyhow!("invalid --windows '{s}' (expected Nh, Nd, Nw, or all)"))?;
    Ok(ReportWindow::Rolling {
        label: s.to_string(),
        secs,
    })
}

/// Split comma-delimited windows and deduplicate in first-seen order.
fn parse_windows(windows: &[String]) -> anyhow::Result<Vec<ReportWindow>> {
    let mut out: Vec<ReportWindow> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for raw in windows {
        for part in raw.split(',') {
            let p = part.trim();
            if p.is_empty() || !seen.insert(p.to_string()) {
                continue;
            }
            out.push(parse_window(p)?);
        }
    }
    if out.is_empty() {
        anyhow::bail!("no --windows selected (expected Nh, Nd, Nw, or all)");
    }
    Ok(out)
}

/// `Nh` / `Nd` / `Nw`. Zero and unknown suffixes are rejected.
pub(super) fn parse_duration_secs(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.len().checked_sub(1)?);
    if num.is_empty() {
        return None;
    }
    let v: u64 = num.parse().ok()?;
    if v == 0 {
        return None;
    }
    let factor = match unit {
        "h" => 3_600,
        "d" => 86_400,
        "w" => 7 * 86_400,
        _ => return None,
    };
    Some(v.saturating_mul(factor))
}

fn usd(x: f64) -> String {
    format!("{x:.2}")
}

/// Leaderboards share one cap so `--top` means the same thing everywhere.
fn leaderboard_limit(top: usize) -> usize {
    top.min(100)
}

fn row_fields(r: &ReportRow) -> Vec<String> {
    vec![
        r.ops.to_string(),
        usd(r.volume_usd),
        usd(r.gross_usd),
        usd(r.gas_usd),
        usd(r.flash_fee_usd),
        usd(r.net_usd),
    ]
}

/// label + report row fields, joined for a single table row.
fn row_cells(label: &str, fields: &[String]) -> Vec<String> {
    let mut v = vec![label.to_string()];
    v.extend_from_slice(fields);
    v
}

fn report_table_text(rows: &[ReportRow], first_col: &str) -> String {
    let mut table = Table::new();
    table.set_header(vec![
        first_col,
        "ops",
        "volume USD",
        "gross USD",
        "gas USD",
        "flash USD",
        "net USD",
    ]);
    if rows.is_empty() {
        table.add_row(vec!["(no data)", "-", "-", "-", "-", "-", "-"]);
    }
    for r in rows {
        table.add_row(row_cells(&r.label, &row_fields(r)));
    }
    format!("{table}")
}

fn stats_table_text(rows: &[StatsRow]) -> String {
    let mut table = Table::new();
    table.set_header(vec!["label", "ops", "gross USD", "net USD"]);
    if rows.is_empty() {
        table.add_row(vec!["(no data)", "-", "-", "-"]);
    }
    for r in rows {
        table.add_row(vec![
            short_addr(&r.label),
            r.ops.to_string(),
            usd(r.gross_usd),
            usd(r.net_usd),
        ]);
    }
    format!("{table}")
}

fn top_ops_table_text(rows: &[MevOpRow]) -> String {
    let mut table = Table::new();
    table.set_header(vec![
        "tx",
        "block",
        "kind",
        "subkind",
        "basis",
        "volume USD",
        "gross USD",
        "gas USD",
        "net USD",
    ]);
    if rows.is_empty() {
        table.add_row(vec!["(no data)", "-", "-", "-", "-", "-", "-", "-", "-"]);
    }
    for r in rows {
        table.add_row(vec![
            short_addr(&r.tx_hash),
            r.block_number.to_string(),
            r.kind.clone(),
            r.subkind.clone().unwrap_or_else(|| "-".to_string()),
            pnl_basis_of(r),
            usd(r.volume_usd.unwrap_or(0.0)),
            usd(r.profit_usd.unwrap_or(0.0)),
            usd(r.gas_cost_usd.unwrap_or(0.0)),
            usd(r.net_profit_usd.unwrap_or(0.0)),
        ]);
    }
    format!("{table}")
}

/// A19: the persisted `details.pnl_basis` (R / O / F / gross_amount_diff /
/// gas_cost_as_profit), so a reader can see why a row is `exact` or not.
fn pnl_basis_of(r: &MevOpRow) -> String {
    r.details_json
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| {
            v.get("pnl_basis")
                .and_then(|b| b.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "-".to_string())
}

pub async fn cmd_explorer_report(
    config: &Config,
    windows: &[String],
    kind: Option<&str>,
    confidence: Option<&str>,
    top: usize,
) -> anyhow::Result<()> {
    let v = validation::validate_live(config).map_err(|e| anyhow::anyhow!("{e}"))?;
    let chain = v.chain_name;
    let store = explorer_store(config, chain)?;

    let kind_filter: Option<MevKind> = match kind {
        None => None,
        Some(k) => {
            let parsed = MevKind::parse(k).ok_or_else(|| {
                let kinds = MevKind::ALL
                    .iter()
                    .map(|kind| kind.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                anyhow::anyhow!("invalid --kind '{k}' (expected one of {kinds})")
            })?;
            Some(parsed)
        }
    };
    // The store takes typed filters; clap already restricts the CLI values, so
    // this parse only turns the flag string into the enum.
    let confidence_filter: Option<Confidence> = match confidence {
        None => None,
        Some(c) => match c.parse::<Confidence>() {
            Ok(parsed) => Some(parsed),
            Err(_) => {
                anyhow::bail!("invalid --confidence '{c}' (expected exact, inferred, or all)")
            }
        },
    };

    let window_list = parse_windows(windows)?;
    let top = leaderboard_limit(top);

    // Fill labels for ops ingested before competitor wiring (idempotent).
    let _ = store.backfill_competitor_labels()?;

    let now = epoch_secs();
    let mut reports: Vec<WindowReport> = Vec::with_capacity(window_list.len());
    for window in &window_list {
        let since = window.since_ts(now);
        let overview = store.report_window_overview(since, kind_filter, confidence_filter)?;
        let by_kind = store.report_by_kind(since, kind_filter, confidence_filter)?;
        let daily = store.report_daily(since, kind_filter, confidence_filter)?;
        let top_competitors =
            store.top_competitors_filtered(since, top, kind_filter, confidence_filter)?;
        let top_senders = store.top_senders_filtered(since, top, kind_filter, confidence_filter)?;
        let top_pools = store.top_pools_filtered(since, top, kind_filter, confidence_filter)?;
        let top_ops = store.top_ops(since, top, kind_filter, confidence_filter)?;
        let blocks = store.blocks_in_window(since)?;
        let classify_rejects = store.report_classify_rejects(since)?;
        let coverage_pct = window.expected_blocks(chain).map(|expected| {
            if expected == 0 {
                1.0
            } else {
                (blocks as f64 / expected as f64).min(1.0)
            }
        });
        reports.push(WindowReport {
            window: window.label().to_string(),
            coverage_pct,
            blocks,
            overview,
            by_kind,
            daily,
            top_competitors,
            top_senders,
            top_pools,
            top_ops,
            classify_rejects,
        });
    }

    let window_labels = window_list
        .iter()
        .map(ReportWindow::label)
        .collect::<Vec<_>>()
        .join(", ");
    println!("Explorer report — {chain} windows: {window_labels}");
    if let Some(k) = kind_filter {
        println!("  (kind filter: {k})");
    }
    if let Some(c) = confidence_filter {
        println!("  (confidence filter: {})", c.as_str());
    }

    match config.output.output {
        OutputFormat::Json => {
            let mut out = serde_json::Map::new();
            out.insert("chain".into(), serde_json::json!(chain.to_string()));
            out.insert("generated_at".into(), serde_json::json!(now));
            let mut wins = serde_json::Map::new();
            for r in &reports {
                wins.insert(r.window.clone(), serde_json::to_value(r)?);
            }
            out.insert("windows".into(), serde_json::Value::Object(wins));
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        OutputFormat::Csv => {
            for line in csv_lines(&reports) {
                println!("{line}");
            }
        }
        OutputFormat::Table => {
            for r in &reports {
                render_window(r);
            }
        }
    }

    Ok(())
}

fn row_overview_fields(o: &ReportOverview) -> Vec<String> {
    vec![
        o.ops.to_string(),
        usd(o.volume_usd),
        usd(o.gross_usd),
        usd(o.gas_usd),
        usd(o.flash_fee_usd),
        usd(o.net_usd),
    ]
}

/// Stats rows carry ops/gross/net; pad the volume and fee columns so every
/// CSV line has the header's width.
fn stats_fields(r: &StatsRow) -> Vec<String> {
    vec![
        r.ops.to_string(),
        String::new(),
        usd(r.gross_usd),
        String::new(),
        String::new(),
        usd(r.net_usd),
    ]
}

/// One op row: ops=1, then volume/gross/gas/net (flash fee is not stored per op).
fn op_fields(r: &MevOpRow) -> Vec<String> {
    vec![
        "1".to_string(),
        usd(r.volume_usd.unwrap_or(0.0)),
        usd(r.profit_usd.unwrap_or(0.0)),
        usd(r.gas_cost_usd.unwrap_or(0.0)),
        String::new(),
        usd(r.net_profit_usd.unwrap_or(0.0)),
    ]
}

fn csv_row(window: &str, section: &str, label: &str, cols: &[String]) -> String {
    let mut parts: Vec<String> = vec![window.to_string(), section.to_string(), label.to_string()];
    parts.extend_from_slice(cols);
    parts.join(",")
}

fn csv_lines(reports: &[WindowReport]) -> Vec<String> {
    let mut lines = vec![
        "window,section,label,ops,volume_usd,gross_usd,gas_usd,flash_fee_usd,net_usd".to_string(),
    ];
    for r in reports {
        lines.push(csv_row(
            &r.window,
            "overview",
            "total",
            &row_overview_fields(&r.overview),
        ));
        for k in &r.by_kind {
            lines.push(csv_row(&r.window, "by_kind", &k.label, &row_fields(k)));
        }
        for d in &r.daily {
            lines.push(csv_row(&r.window, "daily", &d.label, &row_fields(d)));
        }
        for s in &r.top_competitors {
            lines.push(csv_row(
                &r.window,
                "top_competitors",
                &s.label,
                &stats_fields(s),
            ));
        }
        for s in &r.top_senders {
            lines.push(csv_row(
                &r.window,
                "top_senders",
                &s.label,
                &stats_fields(s),
            ));
        }
        for s in &r.top_pools {
            lines.push(csv_row(&r.window, "top_pools", &s.label, &stats_fields(s)));
        }
        for op in &r.top_ops {
            lines.push(csv_row(&r.window, "top_ops", &op.tx_hash, &op_fields(op)));
        }
        for c in &r.classify_rejects {
            let padded = [
                c.count.to_string(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
            ];
            lines.push(csv_row(&r.window, "classify_rejects", &c.reason, &padded));
        }
    }
    lines
}

fn render_window(r: &WindowReport) {
    let (stdout, warnings) = render_window_text(r);
    print!("{stdout}");
    for warning in warnings {
        eprintln!("{warning}");
    }
}

/// Pure renderer: stdout text plus the warning lines the caller prints to
/// stderr. Kept separate so snapshot tests do not capture global streams.
fn render_window_text(r: &WindowReport) -> (String, Vec<String>) {
    let o = &r.overview;
    let mut out = String::new();
    let mut warnings: Vec<String> = Vec::new();
    out.push('\n');
    match r.coverage_pct {
        Some(c) => out.push_str(&format!(
            "── {} window ({} blocks indexed, {:.0}% coverage) ──────────\n",
            r.window,
            r.blocks,
            c * 100.0
        )),
        None => out.push_str(&format!(
            "── {} window (all-time, {} blocks indexed) ─────\n",
            r.window, r.blocks
        )),
    }
    if let Some(c) = r.coverage_pct {
        if c < 0.7 {
            warnings.push(format!(
                "  warning: {} window is under-filled ({:.0}% coverage). Run \
                 `mev-scout explorer index --days 30` first.",
                r.window,
                c * 100.0
            ));
        }
    }
    out.push_str(&format!(
        "  ops: {} | competitors: {} | searchers: {} | volume: ${} | gross: ${} | \
         gas: ${} | flash fees: ${} | net: ${} | highest single: ${}\n",
        o.ops,
        o.competitors,
        o.searchers,
        usd(o.volume_usd),
        usd(o.gross_usd),
        usd(o.gas_usd),
        usd(o.flash_fee_usd),
        usd(o.net_usd),
        usd(o.highest_single_usd),
    ));
    // Shown only when non-zero: a window without loss-gated sandwiches keeps
    // the historical output verbatim.
    if o.quality.ops_excluded_from_net > 0 {
        out.push_str(&format!(
            "  loss-gated ops: {}\n",
            o.quality.ops_excluded_from_net
        ));
    }
    let q = &o.quality;
    out.push_str(&format!(
        "  quality: priced {}/{} ({}% coverage) | gross (priced only) ${} | exact {} (net ${}) | inferred {} (net ${}) | approx {}\n",
        q.ops_priced,
        o.ops,
        q.pricing_coverage_pct,
        usd(q.gross_usd_priced_only),
        q.ops_exact,
        usd(q.net_usd_exact),
        q.ops_inferred,
        usd(q.net_usd_inferred),
        q.ops_approx,
    ));
    if q.ops_native_unpriced + q.ops_multi_asset + q.ops_pricing_clamped + q.ops_flash_fee_unpriced
        > 0
    {
        // Shown only when non-zero: a window without unpriced flash fees keeps
        // the historical warning text verbatim.
        let flash_fee = if q.ops_flash_fee_unpriced > 0 {
            format!(" | {} flash-fee-unpriced", q.ops_flash_fee_unpriced)
        } else {
            String::new()
        };
        warnings.push(format!(
            "  pricing reasons: {} native-unpriced | {} multi-asset | {} clamped{} \
             (USD figures approximate; net may be incomplete)",
            q.ops_native_unpriced, q.ops_multi_asset, q.ops_pricing_clamped, flash_fee,
        ));
    }
    if let Some(pct) = o.net_concentration_pct() {
        if pct > 40.0 {
            warnings.push(format!(
                "  warning: top op is {:.0}% of window net — totals may be dominated by a single hit.",
                pct
            ));
        }
    }
    if !r.classify_rejects.is_empty() {
        let joined = r
            .classify_rejects
            .iter()
            .map(|c| format!("{} {}", c.reason, c.count))
            .collect::<Vec<_>>()
            .join(" | ");
        out.push_str(&format!("  classify rejects: {joined}\n"));
    }

    out.push_str("\n  Per kind:\n");
    let mut t = Table::new();
    t.set_header(vec![
        "kind",
        "ops",
        "volume USD",
        "gross USD",
        "gas USD",
        "flash USD",
        "net USD",
    ]);
    if r.by_kind.is_empty() {
        t.add_row(vec!["(no data)", "-", "-", "-", "-", "-", "-"]);
    }
    for k in &r.by_kind {
        t.add_row(row_cells(&k.label, &row_fields(k)));
    }
    out.push_str(&format!("{t}\n"));

    out.push_str("\n  Daily trend:\n");
    out.push_str(&report_table_text(&r.daily, "date"));
    out.push('\n');

    out.push_str("\n  Competitors (bot = contract or eoa):\n");
    out.push_str(&stats_table_text(&r.top_competitors));
    out.push('\n');

    out.push_str("\n  Top searchers:\n");
    out.push_str(&stats_table_text(&r.top_senders));
    out.push('\n');

    out.push_str("\n  Top pools:\n");
    out.push_str(&stats_table_text(&r.top_pools));
    out.push('\n');

    out.push_str("\n  Top ops (by net profit):\n");
    out.push_str(&top_ops_table_text(&r.top_ops));
    out.push('\n');

    (out, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_suffixes() {
        assert_eq!(parse_duration_secs("12h"), Some(12 * 3_600));
        assert_eq!(parse_duration_secs("1d"), Some(86_400));
        assert_eq!(parse_duration_secs("2w"), Some(14 * 86_400));
        assert_eq!(parse_duration_secs("0d"), None);
        assert_eq!(parse_duration_secs("1x"), None);
        assert_eq!(parse_duration_secs("d"), None);
        assert_eq!(parse_duration_secs("all"), None);
    }

    #[test]
    fn windows_dedup_in_first_seen_order_and_reject_bad_input() {
        let windows = parse_windows(&["1d,7d,1d".to_string()]).unwrap();
        let labels: Vec<&str> = windows.iter().map(ReportWindow::label).collect();
        assert_eq!(labels, vec!["1d", "7d"]);
        assert!(parse_windows(&["1x".to_string()]).is_err());
        assert!(parse_windows(&[String::new()]).is_err());
    }

    #[test]
    fn leaderboard_cap_applies_to_every_list() {
        assert_eq!(leaderboard_limit(3), 3);
        assert_eq!(leaderboard_limit(150), 100);
    }

    fn fixture() -> WindowReport {
        let overview = ReportOverview {
            ops: 2,
            net_usd: 100.0,
            highest_single_usd: 50.0, // 50% of net → concentration warning
            quality: mev_scout_core::explorer::store::ReportQuality {
                ops_priced: 2,
                pricing_coverage_pct: 100.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let stats = || StatsRow {
            label: "0x0000000000000000000000000000000000000001".to_string(),
            ops: 1,
            gross_usd: 1.0,
            net_usd: 1.0,
        };
        WindowReport {
            window: "1d".to_string(),
            coverage_pct: Some(0.5), // under-filled warning
            blocks: 10,
            overview,
            by_kind: vec![ReportRow {
                label: "arb_atomic".to_string(),
                ops: 2,
                net_usd: 100.0,
                ..Default::default()
            }],
            daily: vec![ReportRow {
                label: "2026-10-31".to_string(),
                ops: 1,
                ..Default::default()
            }],
            top_competitors: vec![stats()],
            top_senders: vec![stats()],
            top_pools: vec![stats()],
            top_ops: vec![MevOpRow {
                tx_hash: "0xabc".to_string(),
                ..Default::default()
            }],
            classify_rejects: vec![RejectCount {
                reason: "cycle_not_closed".to_string(),
                count: 3,
            }],
        }
    }

    #[test]
    fn csv_carries_every_section() {
        let lines = csv_lines(&[fixture()]);
        assert_eq!(
            lines[0],
            "window,section,label,ops,volume_usd,gross_usd,gas_usd,flash_fee_usd,net_usd"
        );
        for section in [
            "overview",
            "by_kind",
            "daily",
            "top_competitors",
            "top_senders",
            "top_pools",
            "top_ops",
            "classify_rejects",
        ] {
            assert!(
                lines
                    .iter()
                    .any(|line| line.contains(&format!(",{section},"))),
                "missing CSV section {section}: {lines:?}"
            );
        }
        let reject = lines
            .iter()
            .find(|line| line.contains(",classify_rejects,"))
            .expect("reject row");
        assert!(reject.ends_with(",3,,,,,"), "reject row shape: {reject}");
        assert_eq!(reject.split(',').count(), 9);
    }

    #[test]
    fn render_text_keeps_warnings_and_empty_sections_visible() {
        let (stdout, warnings) = render_window_text(&fixture());
        let warnings = warnings.join("\n");
        assert!(warnings.contains("under-filled"), "got {warnings}");
        assert!(warnings.contains("top op is 50%"), "got {warnings}");
        assert!(stdout.contains("classify rejects: cycle_not_closed 3"));
        assert!(stdout.contains("Per kind:"));

        let mut empty = fixture();
        empty.daily.clear();
        empty.top_ops.clear();
        let (stdout, _) = render_window_text(&empty);
        assert!(stdout.contains("(no data)"), "empty sections must render");
    }
}
