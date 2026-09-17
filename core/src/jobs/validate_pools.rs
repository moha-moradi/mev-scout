//! Validate pool-discovery accuracy against off-chain references (GeckoTerminal).

use std::collections::HashMap;

use alloy::primitives::Address;
use anyhow::Context;
use serde::Serialize;

use crate::cache::SqliteStore;
use crate::config::validation;
use crate::config::Config;
use crate::pool::discovery::{
    remote, DiscoveredPool, DiscoveryRuntimeOpts, ResolvedFactories,
};
use crate::progress::JobProgress;
use crate::resolver::RangeResolver;
use crate::rpc::recommended_get_logs_batch;
use crate::types::{ChainName, RangeMode};

use super::rpc::init_rpc;

#[derive(Debug, Clone)]
pub struct ValidatePoolsOpts {
    pub days: u64,
    pub source: String,
    pub json: bool,
    pub markdown_out: Option<String>,
}

impl Default for ValidatePoolsOpts {
    fn default() -> Self {
        Self {
            days: 7,
            source: "all".into(),
            json: false,
            markdown_out: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceReport {
    pub source: String,
    pub reference_pools: usize,
    pub matched: usize,
    pub recall_pct: f64,
    pub extra_in_discovery: usize,
    pub fee_mismatches: usize,
    pub token_side_mismatches: usize,
    pub tvl_compared: usize,
    pub tvl_mean_abs_delta_usd: f64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub missing_top_examples: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DexRecallRow {
    pub dex: String,
    pub reference_pools: usize,
    pub matched: usize,
    pub recall_pct: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ValidatePoolsOutcome {
    pub chain: String,
    pub days: u64,
    pub onchain_healthy: usize,
    pub reports: Vec<SourceReport>,
    pub per_dex: Vec<(String, Vec<DexRecallRow>)>,
    pub markdown_path: Option<String>,
}

pub async fn job_validate_pools(
    config: &Config,
    opts: &ValidatePoolsOpts,
    progress: &dyn JobProgress,
) -> anyhow::Result<ValidatePoolsOutcome> {
    let (chain_name, chain_config) =
        validation::resolve_chain(config).context("failed to resolve chain configuration")?;
    validation::validate_chain_config_addresses(&chain_config)
        .context("chain configuration has invalid addresses")?;

    progress.log("  Pool Discovery Accuracy Validation");
    progress.log(&format!("  Chain:   {chain_name}"));
    progress.log(&format!("  Window:  last {} day(s)", opts.days));
    progress.log(&format!("  Sources: {}", opts.source));
    progress.log("");

    let mut references: Vec<(String, Vec<DiscoveredPool>)> = Vec::new();
    let source = opts.source.to_ascii_lowercase();
    let want_gecko = source == "all" || source == "gecko";

    if want_gecko {
        let slug = chain_name.to_string();
        let pools = remote::discover_via_geckoterminal(&slug, Some(1000), None).await;
        if !pools.is_empty() {
            progress.log(&format!("  Reference [gecko]:    {} pools", pools.len()));
            references.push(("gecko".into(), pools));
        }
    }

    let setup = init_rpc(config, chain_name, true).await?;
    let rpc = setup.rpc;

    let resolver = RangeResolver::new(rpc.clone());
    let resolved = resolver.resolve(&RangeMode::Days(opts.days)).await?;
    let (from, to) = (resolved.start_block, resolved.end_block);
    progress.log(&format!("  On-chain scan: blocks {from}–{to}"));

    let cache_path = config.effective_db_path(&chain_name);
    let cache = SqliteStore::open(&cache_path)?;

    let factories = ResolvedFactories::from_chain_config(&chain_config, chain_name);
    let disc_config = factories.discovery_config(DiscoveryRuntimeOpts {
        batch_size: recommended_get_logs_batch(&config.rpc.rpc_urls, 500),
        solidly_fee_bps: None,
        rpc_concurrency: 8,
        token_cache: None,
        pool_cache: Some(&cache),
    });

    let (set_a_raw, _active) =
        crate::pool::discovery::discover_pools(&rpc, from, to, &disc_config, None).await?;
    let (set_a, _removed) =
        crate::pool::discovery::health_check_pools(&rpc, set_a_raw, 8, factories.vault).await;
    progress.log(&format!("  On-chain (healthy): {} pools", set_a.len()));
    progress.log("");

    let a_by_addr: HashMap<Address, &DiscoveredPool> =
        set_a.iter().map(|p| (p.address, p)).collect();

    let mut reports: Vec<SourceReport> = Vec::new();
    let mut per_dex: Vec<(String, Vec<DexRecallRow>)> = Vec::new();
    for (name, reference) in &references {
        reports.push(compare_source(name, reference, &a_by_addr, set_a.len()));
        per_dex.push((name.clone(), dex_recall(reference, &a_by_addr)));
    }

    for r in &reports {
        progress.log(&format!(
            "  [{}] recall={:.1}% matched={}/{} extra={} fee≠{} tok≠{} tvlΔ=${:.0}",
            r.source,
            r.recall_pct,
            r.matched,
            r.reference_pools,
            r.extra_in_discovery,
            r.fee_mismatches,
            r.token_side_mismatches,
            r.tvl_mean_abs_delta_usd,
        ));
        for ex in r.missing_top_examples.iter().take(5) {
            progress.log(&format!("    missing: {ex}"));
        }
    }
    for (source, rows) in &per_dex {
        progress.log(&format!("  Per-DEX recall vs [{source}]:"));
        for r in rows.iter().take(20) {
            progress.log(&format!(
                "    {} ref={} matched={} recall={:.1}%",
                r.dex, r.reference_pools, r.matched, r.recall_pct
            ));
        }
    }

    if opts.json {
        progress.log(&serde_json::to_string_pretty(&reports)?);
    }

    let mut markdown_path = None;
    if let Some(ref path) = opts.markdown_out {
        write_markdown(path, &reports, &chain_name, opts.days)?;
        progress.log(&format!("\n  Markdown report written to {path}"));
        markdown_path = Some(path.clone());
    }

    Ok(ValidatePoolsOutcome {
        chain: chain_name.to_string(),
        days: opts.days,
        onchain_healthy: set_a.len(),
        reports,
        per_dex,
        markdown_path,
    })
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
                if r.fee != 0 && a.fee != 0 && r.fee != a.fee {
                    fee_mismatches += 1;
                }
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

fn dex_recall(
    reference: &[DiscoveredPool],
    a_by_addr: &HashMap<Address, &DiscoveredPool>,
) -> Vec<DexRecallRow> {
    let mut by_dex: HashMap<String, (usize, usize)> = HashMap::new();
    for r in reference {
        let key = r
            .dex_name
            .clone()
            .unwrap_or_else(|| format!("{:?}", r.dex_type));
        let e = by_dex.entry(key).or_insert((0, 0));
        e.0 += 1;
        if a_by_addr.contains_key(&r.address) {
            e.1 += 1;
        }
    }
    let mut rows: Vec<DexRecallRow> = by_dex
        .into_iter()
        .map(|(dex, (ref_n, m))| DexRecallRow {
            recall_pct: if ref_n == 0 {
                0.0
            } else {
                100.0 * m as f64 / ref_n as f64
            },
            dex,
            reference_pools: ref_n,
            matched: m,
        })
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.reference_pools));
    rows
}

fn write_markdown(
    path: &str,
    reports: &[SourceReport],
    chain: &ChainName,
    days: u64,
) -> anyhow::Result<()> {
    let mut out = String::new();
    out.push_str(&format!(
        "# Pool discovery accuracy — {chain} (last {days}d)\n\n"
    ));
    out.push_str("| Source | Ref \\|B\\| | Matched | Recall % | Extra in A | Fee mismatches | Token-side mismatches | TVL compared | TVL Δ mean USD |\n");
    out.push_str("|---|---|---|---|---|---|---|---|---|\n");
    for r in reports {
        out.push_str(&format!(
            "| {} | {} | {} | {:.1} | {} | {} | {} | {} | {:.0} |\n",
            r.source,
            r.reference_pools,
            r.matched,
            r.recall_pct,
            r.extra_in_discovery,
            r.fee_mismatches,
            r.token_side_mismatches,
            r.tvl_compared,
            r.tvl_mean_abs_delta_usd,
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
    std::fs::write(path, out)?;
    Ok(())
}
