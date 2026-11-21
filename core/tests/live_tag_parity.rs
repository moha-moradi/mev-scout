//! Historical live-detector benchmark; opt in with MEV_SCOUT_E2E=1 and RPC_URL.
//! Only RPC_URL is used. The explorer reference is read-only; replay data and
//! reports live in an isolated MEV_SCOUT_PARITY_OUTPUT directory.
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use alloy::primitives::{Address, B256};
use anyhow::{ensure, Context};
use mev_scout_core::cache::SqliteStore;
use mev_scout_core::chain::savax::{fetch_savax_exchange_rate, SavaxRateCache};
use mev_scout_core::config::Config;
use mev_scout_core::dex_type::DexType;
use mev_scout_core::explorer::store::{ExplorerStore, MevOpRow};
use mev_scout_core::explorer::MevKind;
use mev_scout_core::fetch::Fetcher;
use mev_scout_core::jobs::{init_rpc, load_pool_registry};
use mev_scout_core::mev::detectors::ArbTagPolicy;
use mev_scout_core::paper::{LedgerPolicy, SimMode, SimPolicy, SimSeeds};
use mev_scout_core::pipeline::BacktestRunner;
use mev_scout_core::pool::state::{PoolInfo, PoolManager};
use mev_scout_core::replay::BlockReplayer;
use mev_scout_core::resolver::ResolvedRange;
use mev_scout_core::types::{GasConfig, MevOpportunity, RangeMode};
use serde_json::{json, Value};

const FROM: u64 = 99_833_048;
const TO: u64 = 99_836_394;
const VENDOR_CORPUS: &str =
    include_str!("../../docs/benchmarks/mevlive_avax_arb_recall_2026-11-09.md");
const NA_TAGS: &[&str] = &["forwarded_profit", "fot_arb", "rebase_arb"];

fn repo_path(raw: impl AsRef<Path>) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(raw)
}

fn env_path(name: &str, default: &str) -> PathBuf {
    let path = PathBuf::from(std::env::var(name).unwrap_or_else(|_| default.into()));
    if path.is_absolute() {
        path
    } else {
        repo_path(path)
    }
}

fn env_number(name: &str, default: u64) -> anyhow::Result<u64> {
    match std::env::var(name) {
        Ok(value) => value.parse().with_context(|| format!("invalid {name}")),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(e) => Err(e.into()),
    }
}

fn vendor_hashes() -> anyhow::Result<BTreeMap<B256, u64>> {
    let section = VENDOR_CORPUS
        .split("## Matched (5)")
        .nth(1)
        .context("vendor tables missing")?
        .split("## Our ops in the same block window")
        .next()
        .context("vendor tables missing")?;
    let mut hashes = BTreeMap::new();
    for line in section.lines() {
        let cells: Vec<_> = line
            .split('|')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        let Some(block) = cells.first().and_then(|s| s.parse::<u64>().ok()) else {
            continue;
        };
        if let Some(hash) = cells
            .last()
            .and_then(|s| s.trim_matches('`').parse::<B256>().ok())
        {
            hashes.insert(hash, block);
        }
    }
    ensure!(
        hashes.len() == 47,
        "frozen vendor corpus must have 47 unique hashes"
    );
    Ok(hashes)
}

fn tags(row: &MevOpRow) -> anyhow::Result<BTreeSet<String>> {
    let details: Value = serde_json::from_str(
        row.details_json
            .as_deref()
            .context("missing reference details")?,
    )?;
    let Some(values) = details.get("tags") else {
        return Ok(BTreeSet::new());
    };
    values
        .as_array()
        .context("tags is not an array")?
        .iter()
        .map(|v| Ok(v.as_str().context("non-string tag")?.to_owned()))
        .collect()
}

fn route_pools(row: &MevOpRow) -> anyhow::Result<HashSet<Address>> {
    let route: Value = serde_json::from_str(
        row.route_json
            .as_deref()
            .context("missing reference route")?,
    )?;
    route
        .as_array()
        .context("route is not an array")?
        .iter()
        .map(|leg| {
            Ok(leg
                .get("pool")
                .and_then(Value::as_str)
                .context("route pool missing")?
                .parse()?)
        })
        .collect()
}

fn candidate_pools(opp: &MevOpportunity) -> HashSet<Address> {
    mev_scout_core::paper::pools_of_opp(opp)
        .into_iter()
        .collect()
}

fn strict_match(hash: B256, pools: &HashSet<Address>, opp: &MevOpportunity) -> bool {
    opp.tx_hash == Some(hash) && !pools.is_disjoint(&candidate_pools(opp))
}

fn snapshot_cache(src: &Path, dst: &Path) -> anyhow::Result<()> {
    ensure!(
        !dst.exists(),
        "refusing to overwrite benchmark cache {}",
        dst.display()
    );
    let conn =
        rusqlite::Connection::open_with_flags(src, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let integrity: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    ensure!(integrity == "ok", "invalid source cache: {integrity}");
    // VACUUM INTO includes committed WAL data without copying live sidecars.
    conn.execute(
        "VACUUM INTO ?1",
        [dst.to_str().context("UTF-8 cache path")?],
    )?;
    Ok(())
}

fn seed_registry(
    cache: &SqliteStore,
    explorer: &ExplorerStore,
    from: u64,
    to: u64,
) -> anyhow::Result<usize> {
    let existing: HashSet<_> = cache
        .list_discovered_pools()?
        .iter()
        .map(|p| p.address)
        .collect();
    let mut stmt = explorer.connection().prepare(
        "SELECT DISTINCT pool, amm FROM swaps WHERE block_number BETWEEN ?1 AND ?2 ORDER BY pool, amm")?;
    let rows = stmt.query_map([from, to], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut count = 0;
    let mut seen = HashSet::new();
    for row in rows {
        let (pool, amm) = row?;
        let address: Address = pool.parse()?;
        if existing.contains(&address) || !seen.insert(address) {
            continue;
        }
        let dex_type = match amm.as_str() {
            "v2" => DexType::UniswapV2,
            "v3" => DexType::UniswapV3,
            "solidly" => DexType::Solidly,
            "curve" => DexType::Curve,
            "balancer" => DexType::Balancer,
            "lb" => DexType::TraderJoeLB,
            "pendle" => DexType::Pendle,
            "metric" => DexType::Metric,
            "fluid" => DexType::Fluid,
            _ => continue,
        };
        // Let historical on-chain metadata establish token ordering, especially PT/SY.
        cache.put_discovered_pool(&PoolInfo {
            address,
            dex_type,
            ..Default::default()
        })?;
        count += 1;
    }
    Ok(count)
}

#[derive(Default, serde::Serialize)]
struct TagCounts {
    reference: usize,
    matched: usize,
    missing: usize,
}

fn compare(
    reference: &[MevOpRow],
    candidates: &[MevOpportunity],
    vendor: &BTreeMap<B256, u64>,
) -> anyhow::Result<Value> {
    ensure!(
        !reference.is_empty(),
        "empty reference is not a passing benchmark"
    );
    let mut by_block: HashMap<u64, Vec<&MevOpportunity>> = HashMap::new();
    for opp in candidates {
        by_block.entry(opp.block_number).or_default().push(opp);
    }
    let mut by_tag: BTreeMap<String, TagCounts> = BTreeMap::new();
    let mut matches = 0;
    let mut matched_vendor = BTreeSet::new();
    let mut misses = Vec::new();
    let mut tag_misses = Vec::new();
    let mut nearby_only = 0;
    let mut unsupported_reference_tags = BTreeMap::<String, usize>::new();
    for row in reference {
        let hash: B256 = row.tx_hash.parse()?;
        let pools = route_pools(row)?;
        let reference_tags = tags(row)?;
        let block_candidates = by_block.get(&row.block_number).cloned().unwrap_or_default();
        let matched: Vec<_> = block_candidates
            .iter()
            .filter(|opp| strict_match(hash, &pools, opp))
            .collect();
        if matched.is_empty() {
            let nearby = block_candidates
                .iter()
                .filter(|opp| !pools.is_disjoint(&candidate_pools(opp)))
                .count();
            nearby_only += usize::from(nearby > 0);
            misses.push(json!({"block":row.block_number,"tx_hash":hash,"tags":reference_tags,"same_block_pool_overlap_only":nearby}));
        } else {
            matches += 1;
            if vendor.contains_key(&hash) {
                matched_vendor.insert(hash);
            }
        }
        for tag in reference_tags {
            if NA_TAGS.contains(&tag.as_str()) {
                *unsupported_reference_tags.entry(tag).or_default() += 1;
                continue;
            }
            let count = by_tag.entry(tag.clone()).or_default();
            count.reference += 1;
            if matched.iter().any(|opp| opp.has_tag(&tag)) {
                count.matched += 1;
            } else {
                count.missing += 1;
                tag_misses.push(json!({"block":row.block_number,"tx_hash":hash,"tag":tag}));
            }
        }
    }
    let reference_hashes: BTreeSet<B256> = reference
        .iter()
        .map(|r| r.tx_hash.parse())
        .collect::<Result<_, _>>()?;
    let vendor_missing_reference: Vec<_> = vendor
        .keys()
        .filter(|h| !reference_hashes.contains(*h))
        .collect();
    let vendor_missed_live: Vec<_> = vendor
        .keys()
        .filter(|h| !matched_vendor.contains(*h))
        .collect();
    Ok(json!({
        "strict_join":"same block AND tx_hash AND nonempty route-pool intersection",
        "reference_ops":reference.len(),"matched_ops":matches,"unmatched_ops":misses.len(),
        "same_block_pool_overlap_only":nearby_only,"by_tag":by_tag,
        "na_reference_tags":unsupported_reference_tags,
        "vendor_total":vendor.len(),"vendor_found_in_reference":vendor.len()-vendor_missing_reference.len(),
        "vendor_strict_live_matches":matched_vendor.len(),"vendor_missing_reference":vendor_missing_reference,
        "vendor_missed_live":vendor_missed_live,"missed_ops":misses,"missing_tags":tag_misses,
        "parity_pass":matches == reference.len() && tag_misses.is_empty() && vendor_missed_live.is_empty()
    }))
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "historical RPC benchmark; requires MEV_SCOUT_E2E=1 and RPC_URL"]
async fn live_tag_parity_frozen_avax_window() -> anyhow::Result<()> {
    ensure!(
        std::env::var("MEV_SCOUT_E2E").as_deref() == Ok("1"),
        "set MEV_SCOUT_E2E=1"
    );
    let _ = tracing_subscriber::fmt()
        .with_env_filter("mev_scout_core=warn")
        .with_ansi(false)
        .try_init();
    let rpc_url = std::env::var("RPC_URL").context("RPC_URL required; no fallback")?;
    ensure!(!rpc_url.trim().is_empty(), "RPC_URL is empty");
    ensure!(
        std::env::var("MEV_SCOUT_PARITY_VENUES").is_err(),
        "venue filtering is not full parity"
    );
    let from = env_number("MEV_SCOUT_PARITY_FROM", FROM)?;
    let to = env_number("MEV_SCOUT_PARITY_TO", TO)?;
    ensure!(
        FROM <= from && from <= to && to <= TO,
        "window outside frozen corpus"
    );
    let vendor: BTreeMap<_, _> = vendor_hashes()?
        .into_iter()
        .filter(|(_, b)| from <= *b && *b <= to)
        .collect();
    let explorer_path = env_path(
        "MEV_SCOUT_PARITY_EXPLORER_DB",
        "cache/explorer-avax-rerun-2026-11-13.sqlite",
    );
    let explorer = ExplorerStore::open_read_only(&explorer_path)?;
    let reference = explorer.ops_in_range(from, to, &[MevKind::ArbAtomic], None)?;
    let comparison_blocks: HashSet<_> = reference.iter().map(|row| row.block_number).collect();
    ensure!(
        !reference.is_empty(),
        "reference has no arb_atomic rows in {from}..={to}"
    );
    let reference_blocks: u64 = explorer.connection().query_row(
        "SELECT COUNT(*) FROM blocks_classified WHERE block BETWEEN ?1 AND ?2",
        [from, to],
        |r| r.get(0),
    )?;
    ensure!(
        reference_blocks == to - from + 1,
        "incomplete explorer reference: {reference_blocks} blocks"
    );

    let output = env_path(
        "MEV_SCOUT_PARITY_OUTPUT",
        &format!("results/live-benchmark-{from}-{to}-{}", std::process::id()),
    );
    std::fs::create_dir_all(&output)?;
    let mut rows_file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output.join("blocks.jsonl"))?;
    let mut live_file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output.join("opportunities.jsonl"))?;
    let cache_path = output.join("replay.sqlite");
    if std::env::var("MEV_SCOUT_PARITY_CACHE_DB").is_ok() {
        snapshot_cache(&env_path("MEV_SCOUT_PARITY_CACHE_DB", ""), &cache_path)?;
    }
    let cache = SqliteStore::open(&cache_path)?;
    let seeded = seed_registry(&cache, &explorer, from, to)?;
    eprintln!(
        "reference={} vendor={} newly_seeded={} output={}",
        reference.len(),
        vendor.len(),
        seeded,
        output.display()
    );

    let config_path = env_path("MEV_SCOUT_PARITY_CONFIG", "mev-scout.toml");
    let mut config = Config::load(config_path.to_str().context("UTF-8 config path")?)?;
    config.chain = mev_scout_core::types::ChainName::Avalanche;
    for chain in config.chains.values_mut() {
        chain.rpc = None;
    }
    config.rpc.rpc_url = Some(rpc_url);
    config.rpc.rpc_urls.clear();
    config.rpc.rpc_rps = vec![5.0];
    config.rpc.rps_limit = 5.0;
    config.rpc.block_concurrency = Some(2);
    config.output.db_path = cache_path.to_string_lossy().into_owned();
    config.backtest.capture_pending = false;
    let validation = mev_scout_core::config::validation::validate_live(&config)?;
    let setup = init_rpc(&config, config.chain, true).await?;
    ensure!(
        setup.provider_configs.len() == 1,
        "benchmark requires exactly one RPC"
    );
    let started = Instant::now();
    let range = ResolvedRange {
        start_block: from,
        end_block: to,
        block_count: to - from + 1,
        mode: RangeMode::Range(from, to),
    };
    let fetcher = Fetcher::new(setup.rpc.clone(), cache.clone()).with_block_concurrency(2);
    let fetched_progress = std::sync::atomic::AtomicU64::new(0);
    let fetch_tick = || {
        let done = fetched_progress.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        if done.is_multiple_of(100) {
            eprintln!(
                "fetch progress {done} at {:.1}s",
                started.elapsed().as_secs_f64()
            );
        }
        true
    };
    let summary = fetcher.fetch_range(&range, Some(&fetch_tick)).await?;
    ensure!(
        summary.missing_after_fetch.is_empty(),
        "fetch incomplete: {:?}",
        summary.missing_after_fetch
    );
    eprintln!(
        "fetch done: {} blocks in {:.1}s",
        range.block_count,
        started.elapsed().as_secs_f64()
    );
    let mut pm = PoolManager::new();
    pm.set_max_pairs_per_token(config.backtest.max_pairs_per_token);
    pm.set_tax_quoting(config.live.fot_quote);
    pm.set_concurrency_limit(2);
    if let Some(vault) = validation.chain_config.balancer_vault {
        pm = pm.with_balancer_vault(vault);
    }
    if let Some(native) = validation.chain_config.wrapped_native_token {
        pm = pm.with_wrapped_native(native);
    }
    BacktestRunner::init_pools(&mut pm, &setup.rpc, from - 1, Some(&cache)).await;
    ensure!(
        !pm.pool_addresses().is_empty(),
        "no historical pool states hydrated"
    );
    let hydrated: HashSet<_> = pm.pool_addresses().into_iter().collect();
    let mut missing_reference_pools = BTreeSet::new();
    for op in &reference {
        for pool in route_pools(op)? {
            if !hydrated.contains(&pool) {
                missing_reference_pools.insert(pool);
            }
        }
    }
    let registry = load_pool_registry(&config, &config.chain);
    let chain = &validation.chain_config;
    let wrapped_native = chain.wrapped_native_token.unwrap_or(Address::ZERO);
    let mut blue_chips = chain.profit_stable_tokens.clone().unwrap_or_default();
    if !wrapped_native.is_zero() {
        blue_chips.push(wrapped_native);
    }
    let policy = ArbTagPolicy {
        blue_chips,
        wrapped_native,
        savax: chain.savax,
        epoch_venue_pools: registry.epoch_venue.into_iter().collect(),
        gmx_market_tokens: chain.gmx_market_tokens.clone().unwrap_or_default(),
        gmx_event_emitters: chain.gmx_event_emitters.clone().unwrap_or_default(),
        flash_financed: config.paper.sim_flash_loan,
        funding_source: Some(validation.flash_loan_provider.funding_source().to_string()),
        funding_vault: chain.balancer_vault,
        savax_rate_wad: None,
    };
    let gas = GasConfig {
        gas_limit: config.gas.gas_limit,
        gas_model: validation.gas_model,
        priority_fee_gwei: config.gas.priority_fee_gwei,
        flash_loan_provider: validation.flash_loan_provider,
        winning_bid_premium: config.gas.winning_bid_premium,
        percentile_gas_price: None,
        calibration: Default::default(),
    };
    let handle = tokio::runtime::Handle::current();
    let replayer = BlockReplayer::new(
        handle.clone(),
        cache.clone(),
        setup.rpc.clone(),
        chain.chain_id,
    );
    let mut runner = BacktestRunner::new(replayer, pm, gas)
        .with_tag_policy(policy)
        .with_sim_seeds(true, config.paper.sim_max_candidates_per_block)
        .with_min_profit_wei(config.backtest.min_profit_wei)
        .with_max_candidates_per_tx(config.backtest.max_candidates_per_tx);
    let sim_policy = SimPolicy {
        mode: SimMode::StateSim,
        flash_loan: config.paper.sim_flash_loan,
        fee_bps: validation.flash_loan_provider.fee_rate_bps(),
        max_candidates_per_block: config.paper.sim_max_candidates_per_block,
    };
    let mut ledger_policy = LedgerPolicy::default();
    let mut comparison_candidates = Vec::new();
    let mut total_candidate_count = 0usize;
    let mut simulation_candidates = Vec::new();
    let mut skip_counts = BTreeMap::<String, usize>::new();
    let mut fills_by_tag = BTreeMap::<String, usize>::new();
    let mut fill_count = 0usize;
    let mut seed_mismatch_count = 0usize;
    let mut missing_rate_blocks = 0usize;
    let mut seeds_capped = 0usize;
    let mut rate_cache = SavaxRateCache::default();
    let mut rate_hour = None;
    let replay_started = Instant::now();
    eprintln!(
        "hydrated={} missing_reference_pools={} max_pairs={} replay starting",
        hydrated.len(),
        missing_reference_pools.len(),
        config.backtest.max_pairs_per_token
    );
    for block in from..=to {
        let header = cache.get_block(block)?.context("cached header missing")?;
        let reference_hash: String = explorer.connection().query_row(
            "SELECT block_hash FROM blocks WHERE block_number=?1",
            [block],
            |r| r.get(0),
        )?;
        ensure!(
            header.hash == reference_hash.parse::<B256>()?,
            "reference/cache hash mismatch at {block}"
        );
        let hour = header.timestamp / 3600;
        if rate_hour != Some(hour) {
            let rate = fetch_savax_exchange_rate(
                &setup.rpc,
                chain.savax,
                block,
                header.timestamp,
                &mut rate_cache,
            )
            .await;
            runner.set_savax_rate(rate);
            rate_hour = Some(hour);
        }
        if chain.savax.is_some() && rate_cache.get(hour).is_none() {
            missing_rate_blocks += 1;
        }
        let (mut opps, _, _) = runner
            .sync_block_from_logs(block)
            .with_context(|| format!("replay block {block}"))?;
        runner.finish_log_block();
        let seeds = runner.take_sim_seeds();
        seeds_capped += runner.take_seeds_capped();
        let mut local_seeds = SimSeeds::new();
        // Never reuse a snapshot from another transaction/block for a repeated route ID.
        for (index, opp) in opps.iter_mut().enumerate() {
            let old = opp.canonical_id.clone();
            let id = format!(
                "{block}|{}|{index}|{}",
                opp.tx_index,
                old.as_deref().unwrap_or("missing")
            );
            if let Some(seed) = old.as_ref().and_then(|id| seeds.get(id)) {
                if seed.block_number == block && seed.tx_index == opp.tx_index {
                    local_seeds.insert(id.clone(), seed.clone());
                } else {
                    seed_mismatch_count += 1;
                }
            }
            opp.canonical_id = Some(id);
        }
        let ledger = ledger_policy.apply_simulated(
            &opps,
            &local_seeds,
            &sim_policy,
            Some(runner.pool_manager()),
        );
        ledger_policy.starting_gas_wei = ledger.ending_gas_wei;
        for skip in &ledger.skips {
            *skip_counts.entry(format!("{:?}", skip.reason)).or_default() += 1;
        }
        for fill in &ledger.fills {
            for tag in &fill.tags {
                *fills_by_tag.entry(tag.clone()).or_default() += 1;
            }
        }
        fill_count += ledger.fills.len();
        total_candidate_count += opps.len();
        #[derive(serde::Serialize)]
        struct BlockOutput<'a> {
            block: u64,
            candidates: usize,
            captured_seeds: usize,
            valid_seeds: usize,
            fills: &'a [mev_scout_core::paper::PaperFill],
            skips: &'a [mev_scout_core::paper::PaperSkip],
        }
        serde_json::to_writer(
            &mut rows_file,
            &BlockOutput {
                block,
                candidates: opps.len(),
                captured_seeds: seeds.len(),
                valid_seeds: local_seeds.len(),
                fills: &ledger.fills,
                skips: &ledger.skips,
            },
        )?;
        writeln!(&mut rows_file)?;
        if comparison_blocks.contains(&block) {
            let accepted: HashSet<_> = ledger
                .fills
                .iter()
                .filter_map(|fill| fill.canonical_id.as_ref())
                .collect();
            for opp in &opps {
                serde_json::to_writer(&mut live_file, opp)?;
                writeln!(&mut live_file)?;
                if opp
                    .canonical_id
                    .as_ref()
                    .is_some_and(|id| accepted.contains(id))
                {
                    simulation_candidates.push(opp.clone());
                }
            }
            comparison_candidates.extend(opps);
        }
        let done = block - from + 1;
        if done == 1 || done.is_multiple_of(100) || block == to {
            rows_file.flush()?;
            live_file.flush()?;
            let elapsed = replay_started.elapsed().as_secs_f64();
            eprintln!(
                "replay {done}/{} {:.2} blocks/s ETA {:.0}s candidates={} fills={fill_count}",
                range.block_count,
                done as f64 / elapsed,
                (range.block_count - done) as f64 * elapsed / done as f64,
                total_candidate_count
            );
        }
    }
    let comparison = compare(&reference, &comparison_candidates, &vendor)?;
    let simulated_comparison = compare(&reference, &simulation_candidates, &vendor)?;
    let report = json!({
        "from":from,"to":to,"processed_blocks":range.block_count,
        "reference_db":explorer_path,"reference_schema":explorer.user_version()?,
        "mode":"historical receipt-driven live detectors + state quotes, NOT EVM execution",
        "rpc_providers":1,"cached_blocks":summary.cached,"fetched_blocks":summary.fetched,
        "registry":"retrospective swap-derived pool universe; not a discovery recall test",
        "opportunities_file_scope":"all candidates on blocks containing reference arbs; per-block counts cover the whole range",
        "hydrated_pools":hydrated.len(),"missing_reference_pools":missing_reference_pools,
        "max_pairs_per_token":config.backtest.max_pairs_per_token,"candidate_count":total_candidate_count,
        "fills":fill_count,"fills_by_tag":fills_by_tag,"skips":skip_counts,
        "rejected_snapshot_mismatches":seed_mismatch_count,"seeds_capped":seeds_capped,
        "missing_savax_rate_blocks":missing_rate_blocks,"elapsed_secs":started.elapsed().as_secs_f64(),
        "simulation_limitations":["state quotes are not EVM-confirmed profit", "tax map is the block-final learned map", "receipt replay uses existing venue state approximations"],
        "comparison":comparison,"simulated_comparison":simulated_comparison
    });
    std::fs::write(
        output.join("summary.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    eprintln!(
        "BENCHMARK reference={} matched={} vendor={}/{} candidates={} fills={} report={}",
        reference.len(),
        comparison["matched_ops"],
        comparison["vendor_strict_live_matches"],
        vendor.len(),
        total_candidate_count,
        fill_count,
        output.join("summary.json").display()
    );
    ensure!(
        comparison["parity_pass"] == true,
        "parity NOT achieved; complete metrics in {}",
        output.join("summary.json").display()
    );
    Ok(())
}

#[test]
fn frozen_vendor_corpus_has_47_unique_hashes() {
    assert_eq!(vendor_hashes().unwrap().len(), 47);
}

#[test]
fn join_requires_hash_and_pool() {
    let hash = B256::repeat_byte(1);
    let pool = Address::repeat_byte(2);
    let mut opp = MevOpportunity::new(FROM, 0, mev_scout_core::types::Strategy::TwoHopArb, pool, 0);
    let pools = HashSet::from([pool]);
    assert!(!strict_match(hash, &pools, &opp));
    opp.tx_hash = Some(hash);
    assert!(strict_match(hash, &pools, &opp));
    assert!(!strict_match(
        hash,
        &HashSet::from([Address::repeat_byte(3)]),
        &opp
    ));
}

#[test]
fn unmatched_rows_fail_parity_and_count_missing_tags() {
    let row = MevOpRow {
        id: 1,
        block_number: FROM,
        tx_index: Some(0),
        tx_hash: B256::repeat_byte(1).to_string(),
        ts: 0,
        kind: "arb_atomic".into(),
        eoa: String::new(),
        contract: None,
        confidence: "inferred".into(),
        canonical_id: None,
        profit_token: None,
        profit_amount: None,
        profit_usd: None,
        volume_usd: None,
        gas_cost_usd: None,
        flashloan_fee_usd: None,
        net_profit_usd: None,
        route_json: Some(json!([{"pool":Address::repeat_byte(2)}]).to_string()),
        victim_hashes: None,
        details_json: Some(json!({"tags":["long_tail"]}).to_string()),
        detector: "test".into(),
        created_at: 0,
        subkind: None,
    };
    let report = compare(&[row], &[], &BTreeMap::new()).unwrap();
    assert_eq!(report["parity_pass"], false);
    assert_eq!(report["unmatched_ops"], 1);
    assert_eq!(report["by_tag"]["long_tail"]["missing"], 1);
    assert!(compare(&[], &[], &BTreeMap::new()).is_err());
}
