//! validate-pools — quantified discovery accuracy vs off-chain references.
//!
//! Runs on-chain discovery over a recent window → set **A**, fetches reference
//! sets → set **B** per source (subgraphs / GeckoTerminal / DefiLlama), then reports:
//! - recall `|A∩B|/|B|` overall and per-DEX (vs explorer-top-N semantics)
//! - reference pools missing from A (the recall gap, top-TV examples)
//! - field mismatches (`fee`, token sides) on the overlap
//! - TVL parity (mean absolute delta where both sides know TVL)

use std::collections::HashMap;

use alloy::primitives::Address;
use anyhow::Context;
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Cell, Color, Table};

use crate::cli::{ValidatePoolsArgs, ValidationSource};
use crate::commands::discover::{fetch_remote, resolve_subgraphs};
use crate::rpc_setup::init_rpc;
use mev_scout_core::cache::SqliteStore;
use mev_scout_core::config::{validation, Config};
use mev_scout_core::pool::discovery::{remote, DiscoveryConfig, DiscoveredPool};
use mev_scout_core::resolver::RangeResolver;
use mev_scout_core::rpc::recommended_get_logs_batch;
use mev_scout_core::types::{ChainName, RangeMode};

#[derive(serde::Serialize)]
struct SourceReport {
    source: String,
    reference_pools: usize,
    matched: usize,
    recall_pct: f64,
    /// A∖B — discovered but absent from this reference (includes intentional
    /// dust retention; see plan gap #5).
    extra_in_discovery: usize,
    fee_mismatches: usize,
    token_side_mismatches: usize,
    tvl_compared: usize,
    tvl_mean_abs_delta_usd: f64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    missing_top_examples: Vec<String>,
}

#[derive(serde::Serialize)]
struct DexRecallRow {
    dex: String,
    reference_pools: usize,
    matched: usize,
    recall_pct: f64,
}

fn pick_factories(configured: Vec<Address>, defaults: &[&str]) -> Vec<Address> {
    if !configured.is_empty() {
        return configured;
    }
    defaults.iter().filter_map(|s| s.parse().ok()).collect()
}

pub async fn cmd_validate_pools(config: &Config, args: &ValidatePoolsArgs) -> anyhow::Result<()> {
    let (chain_name, chain_config) = validation::resolve_chain(config)
        .context("failed to resolve chain configuration")?;

    println!("  Pool Discovery Accuracy Validation");
    println!("  Chain:   {}", chain_name);
    println!("  Window:  last {} day(s)", args.days);
    println!("  Sources: {:?}", args.source);
    println!();

    // ── Set B: reference sets first (no RPC needed; fail fast if all dead) ──
    let mut references: Vec<(String, Vec<DiscoveredPool>)> = Vec::new();
    let subgraphs = resolve_subgraphs(&chain_name, &chain_config);

    let want = |s: ValidationSource| args.source == ValidationSource::All || args.source == s;

    if want(ValidationSource::Subgraph) {
        let pools = fetch_remote(&chain_name, &subgraphs, Some(1000), None).await;
        if pools.is_empty() {
            eprintln!("  Warning: subgraph reference set is empty (endpoints failed or no GRAPH_API_KEY)");
        } else {
            println!("  Reference [subgraph]: {} pools", pools.len());
            references.push(("subgraph".into(), pools));
        }
    }
    if want(ValidationSource::Gecko) {
        let slug = chain_name.to_string();
        let pools =
            remote::discover_via_geckoterminal(&slug, Some(1000), None).await;
        if !pools.is_empty() {
            println!("  Reference [gecko]:    {} pools", pools.len());
            references.push(("gecko".into(), pools));
        }
    }
    if want(ValidationSource::Llama) {
        let slug = chain_name.to_string();
        let pools =
            remote::discover_via_defillama(&slug, Some(1000), None).await;
        if !pools.is_empty() {
            println!("  Reference [llama]:    {} pools", pools.len());
            references.push(("llama".into(), pools));
        }
    }

    // ── Set A: on-chain discovery over the window + health check ──
    let setup = init_rpc(config, chain_name.clone(), true).await?;
    let rpc = setup.rpc;

    let resolver = RangeResolver::new(rpc.clone());
    let resolved = resolver.resolve(&RangeMode::Days(args.days)).await?;
    let (from, to) = (resolved.start_block, resolved.end_block);
    println!("  On-chain scan: blocks {from}–{to}");

    let cache_path = config.effective_db_path(&chain_name);
    let cache = SqliteStore::open(&cache_path)?;

    let vault = chain_config.balancer_vault.as_ref().and_then(|s| s.parse::<Address>().ok());
    let registry = chain_config.curve_registry.as_ref().and_then(|s| s.parse::<Address>().ok());
    let parse_all = |v: &Option<Vec<String>>| -> Vec<Address> {
        v.as_ref()
            .map(|fs| fs.iter().filter_map(|s| s.parse().ok()).collect())
            .unwrap_or_default()
    };
    let v2_factories = pick_factories(parse_all(&chain_config.uniswap_v2_factories), chain_name.default_uniswap_v2_factories());
    let v3_factories = pick_factories(parse_all(&chain_config.uniswap_v3_factories), chain_name.default_uniswap_v3_factories());
    let solidly_factories = pick_factories(parse_all(&chain_config.solidly_factories), &chain_name.default_solidly_factories());
    let camelot_factories = pick_factories(parse_all(&chain_config.camelot_factories), &chain_name.default_camelot_factories());

    let disc_config = DiscoveryConfig {
        batch_size: recommended_get_logs_batch(&config.rpc.rpc_urls, 500),
        v2_fee_override: chain_config.uniswap_v2_default_fee,
        balancer_vault: vault,
        v2_factories: if v2_factories.is_empty() { None } else { Some(v2_factories.as_slice()) },
        v3_factories: if v3_factories.is_empty() { None } else { Some(v3_factories.as_slice()) },
        curve_registry: registry,
        solidly_factories: if solidly_factories.is_empty() { None } else { Some(solidly_factories.as_slice()) },
        camelot_factories: if camelot_factories.is_empty() { None } else { Some(camelot_factories.as_slice()) },
        solidly_fee_bps: None,
        v4_pool_manager: chain_config.v4_pool_manager.as_ref().and_then(|s| s.parse().ok()),
        trader_joe_factory: chain_config.trader_joe_factory.as_ref().and_then(|s| s.parse().ok()),
        pendle_factory: chain_config.pendle_factory.as_ref().and_then(|s| s.parse().ok()),
        rpc_concurrency: 8,
        token_cache: None,
        pool_cache: Some(&cache),
    };

    let (set_a_raw, _active) = mev_scout_core::pool::discovery::discover_pools(
        &rpc, from, to, &disc_config, None,
    )
    .await?;
    let (set_a, _removed) = mev_scout_core::pool::discovery::health_check_pools(
        &rpc, set_a_raw, 8, vault,
    )
    .await;
    println!("  On-chain (healthy): {} pools", set_a.len());
    println!();

    let a_by_addr: HashMap<Address, &DiscoveredPool> =
        set_a.iter().map(|p| (p.address, p)).collect();

    // ── Compare per reference source ──
    let mut reports: Vec<SourceReport> = Vec::new();
    let mut per_dex: Vec<(String, Vec<DexRecallRow>)> = Vec::new();
    for (name, reference) in &references {
        reports.push(compare_source(name, reference, &a_by_addr, set_a.len()));
        per_dex.push((name.clone(), dex_recall(reference, &a_by_addr)));
    }

    render_reports(&reports);
    render_dex_recall(&per_dex);
    if args.json {
        println!("{}", serde_json::to_string_pretty(&reports)?);
    }
    if let Some(ref path) = args.markdown_out {
        write_markdown(path, &reports, &chain_name, args.days)?;
        println!("\n  Markdown report written to {}", path);
    }

    Ok(())
}

fn compare_source(
    name: &str,
    reference: &[DiscoveredPool],
    a_by_addr: &HashMap<Address, &DiscoveredPool>,
    a_len: usize,
) -> SourceReport {
    let mut matched = 0usize;
    let mut fee_mismatches = 0usize;
    let mut token_side_mismatches = 0usize;
    let mut tvl_deltas: Vec<f64> = Vec::new();
    let mut missing: Vec<(f64, String)> = Vec::new();

    for r in reference {
        match a_by_addr.get(&r.address) {
            Some(a) => {
                matched += 1;
                // Fee mismatch: only when both sides carry a non-zero fee.
                if r.fee != 0 && a.fee != 0 && r.fee != a.fee {
                    fee_mismatches += 1;
                }
                // Token-side mismatch: same pool, unordered token pair must still
                // contain both tokens.
                let pair_ok = (a.token0 == r.token0 && a.token1 == r.token1)
                    || (a.token0 == r.token1 && a.token1 == r.token0);
                if !pair_ok {
                    token_side_mismatches += 1;
                }
                if let (Some(at), Some(rt)) = (a.tvl_usd, r.tvl_usd) {
                    tvl_deltas.push((at - rt).abs());
                }
            }
            None => {
                let label = format!(
                    "{} {:?} {}/{} tvl=${:.0}",
                    r.address,
                    r.dex_name.as_deref().unwrap_or("?"),
                    r.token0_symbol.as_deref().unwrap_or("?"),
                    r.token1_symbol.as_deref().unwrap_or("?"),
                    r.tvl_usd.unwrap_or(0.0),
                );
                missing.push((r.tvl_usd.unwrap_or(0.0), label));
            }
        }
    }

    // Highest-TV misses are the most meaningful recall gaps.
    missing.sort_by(|x, y| y.0.partial_cmp(&x.0).unwrap_or(std::cmp::Ordering::Equal));
    let missing_top_examples: Vec<String> =
        missing.iter().take(10).map(|(_, l)| l.clone()).collect();

    let recall_pct = if reference.is_empty() {
        0.0
    } else {
        100.0 * matched as f64 / reference.len() as f64
    };
    let tvl_compared = tvl_deltas.len();
    let tvl_mean_abs_delta_usd = if tvl_deltas.is_empty() {
        0.0
    } else {
        tvl_deltas.iter().sum::<f64>() / tvl_deltas.len() as f64
    };

    SourceReport {
        source: name.to_string(),
        reference_pools: reference.len(),
        matched,
        recall_pct,
        extra_in_discovery: a_len.saturating_sub(matched),
        fee_mismatches,
        token_side_mismatches,
        tvl_compared,
        tvl_mean_abs_delta_usd,
        missing_top_examples,
    }
}

/// Per-DEX recall against the subgraph reference (explorer-top-N semantics).
fn dex_recall(reference: &[DiscoveredPool], a_by_addr: &HashMap<Address, &DiscoveredPool>) -> Vec<DexRecallRow> {
    let mut by_dex: HashMap<String, (usize, usize)> = HashMap::new(); // dex → (ref, matched)
    for r in reference {
        let key = r.dex_name.clone().unwrap_or_else(|| format!("{:?}", r.dex_type));
        let e = by_dex.entry(key).or_insert((0, 0));
        e.0 += 1;
        if a_by_addr.contains_key(&r.address) {
            e.1 += 1;
        }
    }
    let mut rows: Vec<DexRecallRow> = by_dex
        .into_iter()
        .map(|(dex, (ref_n, m))| DexRecallRow {
            recall_pct: if ref_n == 0 { 0.0 } else { 100.0 * m as f64 / ref_n as f64 },
            dex,
            reference_pools: ref_n,
            matched: m,
        })
        .collect();
    rows.sort_by(|a, b| b.reference_pools.cmp(&a.reference_pools));
    rows
}

fn render_reports(reports: &[SourceReport]) {
    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_header(vec![
        "Source", "Ref |B|", "Matched", "Recall %", "Extra in A", "Fee ≠", "TokSide ≠", "TVL n", "TVL Δ$ mean",
    ]);
    for r in reports {
        let recall_cell = if r.recall_pct >= 80.0 {
            Cell::new(format!("{:.1}", r.recall_pct)).fg(Color::Green)
        } else if r.recall_pct >= 50.0 {
            Cell::new(format!("{:.1}", r.recall_pct)).fg(Color::Yellow)
        } else {
            Cell::new(format!("{:.1}", r.recall_pct)).fg(Color::Red)
        };
        table.add_row(vec![
            Cell::new(&r.source),
            Cell::new(r.reference_pools),
            Cell::new(r.matched),
            recall_cell,
            Cell::new(r.extra_in_discovery),
            Cell::new(r.fee_mismatches),
            Cell::new(r.token_side_mismatches),
            Cell::new(r.tvl_compared),
            Cell::new(format!("{:.0}", r.tvl_mean_abs_delta_usd)),
        ]);
    }
    println!("{table}");
}

fn render_dex_recall(per_dex: &[(String, Vec<DexRecallRow>)]) {
    for (source, rows) in per_dex {
        if rows.is_empty() {
            continue;
        }
        println!("\n  Per-DEX recall vs [{source}] reference:");
        let mut table = Table::new();
        table.load_preset(UTF8_FULL);
        table.set_header(vec!["DEX", "Ref pools", "Matched", "Recall %"]);
        for r in rows {
            table.add_row(vec![
                Cell::new(&r.dex),
                Cell::new(r.reference_pools),
                Cell::new(r.matched),
                Cell::new(format!("{:.1}", r.recall_pct)),
            ]);
        }
        println!("{table}");
    }
}

fn write_markdown(
    path: &str,
    reports: &[SourceReport],
    chain: &ChainName,
    days: u64,
) -> anyhow::Result<()> {
    let mut out = String::new();
    out.push_str(&format!("# Pool discovery accuracy — {} (last {days}d)\n\n", chain));
    out.push_str("| Source | Ref \\|B\\| | Matched | Recall % | Extra in A | Fee mismatches | Token-side mismatches | TVL compared | TVL Δ mean USD |\n");
    out.push_str("|---|---|---|---|---|---|---|---|---|\n");
    for r in reports {
        out.push_str(&format!(
            "| {} | {} | {} | {:.1} | {} | {} | {} | {} | {:.0} |\n",
            r.source, r.reference_pools, r.matched, r.recall_pct, r.extra_in_discovery,
            r.fee_mismatches, r.token_side_mismatches, r.tvl_compared, r.tvl_mean_abs_delta_usd,
        ));
    }
    out.push_str("\n## Top missing pools (by TVL)\n\n");
    for r in reports {
        out.push_str(&format!("### {}\n\n", r.source));
        for ex in &r.missing_top_examples {
            out.push_str(&format!("- {ex}\n"));
        }
        out.push('\n');
    }
    out.push_str("> Note: `Extra in A` includes dust pools intentionally retained by\n");
    out.push_str("> on-chain discovery (no TVL floor — plan gap #5).\n");

    std::fs::write(path, out)?;
    Ok(())
}
