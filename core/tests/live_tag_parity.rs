//! Live ↔ explorer per-tag parity on the frozen Avalanche window
//! `99833048..=99836394`.
//!
//! Gated like every other live-RPC test: `MEV_SCOUT_E2E=1` + `RPC_URL`,
//! otherwise it skips with no network I/O. The run is `#[ignore]`d because it
//! replays ~3.3k blocks.
//!
//! Requirements (external data, reported explicitly when missing):
//! * An **archive-capable** RPC: pool states are hydrated at `from − 1`.
//! * A discovery cache with `pool_info` rows. Defaults to
//!   `cache/avalanche-mev-scout.sqlite`; the test copies it to a temp file so
//!   the repo cache is never mutated. Override with `MEV_SCOUT_PARITY_CACHE_DB`.
//! * An explorer DB containing the window's `arb_atomic` rows. Defaults to
//!   `cache/explorer-avalanche.sqlite`; override with
//!   `MEV_SCOUT_PARITY_EXPLORER_DB`.
//!
//! Optional: `MEV_SCOUT_PARITY_CONFIG` (default `mev-scout.toml`),
//! `MEV_SCOUT_PARITY_FETCH_RPC` (default Avalanche publicnode; block/receipt
//! fetching only — the archive RPC is used for pool-state hydration),
//! `MEV_SCOUT_PARITY_FROM` / `MEV_SCOUT_PARITY_TO` to narrow the window.
//!
//! Comparison: live log-only replay (same detectors/tags as `live`) is joined
//! to explorer rows per block on `tx_hash`, falling back to route-pool
//! intersection. For every explorer `arb_atomic` tag in the supported set
//! (all ten except `forwarded_profit`, and the chain-unreachable static
//! `fot_arb`/`rebase_arb`), a matched live row must carry the tag.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use alloy::primitives::Address;
use mev_scout_core::cache::SqliteStore;
use mev_scout_core::chain::savax::{fetch_savax_exchange_rate, SavaxRateCache};
use mev_scout_core::config::Config;
use mev_scout_core::dex_type::DexType;
use mev_scout_core::explorer::store::ExplorerStore;
use mev_scout_core::explorer::MevKind;
use mev_scout_core::fetch::Fetcher;
use mev_scout_core::jobs::{init_rpc, load_pool_registry};
use mev_scout_core::mev::detectors::ArbTagPolicy;
use mev_scout_core::paper::{LedgerPolicy, SimMode, SimPolicy};
use mev_scout_core::pipeline::{aggregate_fills, BacktestRunner};
use mev_scout_core::pool::state::PoolManager;
use mev_scout_core::replay::BlockReplayer;
use mev_scout_core::resolver::ResolvedRange;
use mev_scout_core::types::{GasConfig, RangeMode};

mod common;
use common::*;

const DEFAULT_FROM: u64 = 99_833_048;
const DEFAULT_TO: u64 = 99_836_394;

/// Tags whose explorer side is a live target. `fot_arb` and `rebase_arb` come
/// only from the static registry (Ethereum-only; no AVAX entries) and
/// `forwarded_profit` is realized-only — all three are documented N/A.
const EXCLUDED_TAGS: &[&str] = &["fot_arb", "rebase_arb", "forwarded_profit"];

fn repo_path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join(rel)
}

fn resolve_path(raw: &str) -> PathBuf {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        path
    } else {
        repo_path(raw)
    }
}

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|v| v.parse().ok())
}

fn temp_copy(src: &Path, tag: &str) -> anyhow::Result<PathBuf> {
    let dst = std::env::temp_dir().join(format!(
        "mev-scout-parity-{tag}-{}.sqlite",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&dst);
    std::fs::copy(src, &dst)?;
    Ok(dst)
}

fn explorer_tags(details_json: Option<&str>) -> HashSet<String> {
    details_json
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .and_then(|v| v.get("tags").and_then(|t| t.as_array()).cloned())
        .map(|tags| {
            tags.iter()
                .filter_map(|t| t.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn explorer_pools(route_json: Option<&str>) -> HashSet<Address> {
    route_json
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .and_then(|v| v.as_array().cloned())
        .map(|legs| {
            legs.iter()
                .filter_map(|leg| {
                    leg.get("pool")
                        .and_then(|p| p.as_str())
                        .and_then(|s| s.parse::<Address>().ok())
                })
                .collect()
        })
        .unwrap_or_default()
}

fn live_pools(opp: &mev_scout_core::types::MevOpportunity) -> HashSet<Address> {
    let mut pools: HashSet<Address> = opp.path.clone().unwrap_or_default().into_iter().collect();
    if !opp.pool_a.is_zero() {
        pools.insert(opp.pool_a);
    }
    if !opp.pool_b.is_zero() {
        pools.insert(opp.pool_b);
    }
    pools
}

/// Seed the block-cache registry from the explorer's realized swaps in the
/// parity window when the discovery cache is empty. Venue mapping mirrors
/// `Amm::as_str`; on-chain init fills fees, full token sets, balances and bin
/// steps. When explorer `arb_atomic` rows exist, only their route pools are
/// seeded: those are the pools a parity comparison can actually need, and
/// hydrating every swap venue in a 3.3k-block window is needless RPC load.
fn seed_registry_from_explorer(
    cache: &SqliteStore,
    explorer: &ExplorerStore,
    from: u64,
    to: u64,
) -> usize {
    let mut wanted: HashSet<Address> = HashSet::new();
    if let Ok(mut stmt) = explorer.connection().prepare(
        "SELECT route_json FROM mev_ops
         WHERE block_number BETWEEN ?1 AND ?2 AND kind = 'arb_atomic'",
    ) {
        if let Ok(rows) = stmt.query_map(rusqlite::params![from as i64, to as i64], |r| {
            r.get::<_, Option<String>>(0)
        }) {
            for row in rows.flatten() {
                wanted.extend(explorer_pools(row.as_deref()));
            }
        }
    }

    let Ok(mut stmt) = explorer.connection().prepare(
        "SELECT DISTINCT pool, amm, token_in, token_out FROM swaps
         WHERE block_number BETWEEN ?1 AND ?2",
    ) else {
        return 0;
    };
    let Ok(rows) = stmt.query_map(rusqlite::params![from as i64, to as i64], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    }) else {
        return 0;
    };
    let mut seen: HashSet<Address> = HashSet::new();
    let mut count = 0usize;
    let venue_filter: Option<HashSet<String>> = std::env::var("MEV_SCOUT_PARITY_VENUES")
        .ok()
        .map(|raw| raw.split(',').map(|s| s.trim().to_string()).collect());
    let include = |venue: &str| {
        venue_filter
            .as_ref()
            .map(|set| set.contains(venue))
            .unwrap_or(true)
    };
    for row in rows.flatten() {
        let (pool, amm, token_in, token_out) = row;
        let (Ok(address), Ok(tin), Ok(tout)) = (
            pool.parse::<Address>(),
            token_in.parse::<Address>(),
            token_out.parse::<Address>(),
        ) else {
            continue;
        };
        if tin == tout || !seen.insert(address) {
            continue;
        }
        if !wanted.is_empty() && !wanted.contains(&address) {
            continue;
        }
        let amm_name = amm.as_deref().unwrap_or("");
        if !include(amm_name) {
            continue;
        }
        let dex_type = match amm_name {
            "v2" => DexType::UniswapV2,
            "v3" => DexType::UniswapV3,
            "solidly" => DexType::Solidly,
            "curve" => DexType::Curve,
            "balancer" => DexType::Balancer,
            "lb" => DexType::TraderJoeLB,
            "pendle" => DexType::Pendle,
            _ => continue,
        };
        let (token0, token1) = if tin < tout { (tin, tout) } else { (tout, tin) };
        let info = mev_scout_core::pool::state::PoolInfo {
            address,
            token0,
            token1,
            dex_type,
            ..Default::default()
        };
        if cache.put_discovered_pool(&info).is_ok() {
            count += 1;
        }
    }
    count
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live RPC parity replay; set MEV_SCOUT_E2E=1 and RPC_URL"]
async fn live_tag_parity_frozen_avax_window() -> anyhow::Result<()> {
    let Some(rpc_url) = rpc_url() else {
        eprintln!("skip: set MEV_SCOUT_E2E=1 and RPC_URL to run the parity replay");
        return Ok(());
    };
    let from = env_u64("MEV_SCOUT_PARITY_FROM").unwrap_or(DEFAULT_FROM);
    let to = env_u64("MEV_SCOUT_PARITY_TO").unwrap_or(DEFAULT_TO);
    assert!(from > 0 && to >= from, "invalid parity window {from}..={to}");

    let config_path = std::env::var("MEV_SCOUT_PARITY_CONFIG")
        .map(|p| resolve_path(&p).to_string_lossy().into_owned())
        .unwrap_or_else(|_| repo_path("mev-scout.toml").to_string_lossy().into_owned());
    let mut config = Config::load_or_default(&config_path)?;
    config.chain = mev_scout_core::types::ChainName::Avalanche;
    config.rpc.rpc_url = Some(rpc_url);
    config.rpc.rpc_urls.clear();
    if let Some(n) = env_u64("MEV_SCOUT_PARITY_MAX_PAIRS") {
        config.backtest.max_pairs_per_token = n as usize;
    }

    let explorer_db = std::env::var("MEV_SCOUT_PARITY_EXPLORER_DB")
        .map(|p| resolve_path(&p))
        .unwrap_or_else(|_| repo_path("cache/explorer-avalanche.sqlite"));
    if !explorer_db.exists() {
        eprintln!(
            "skip: explorer DB {} missing; run `explorer index` for the window first",
            explorer_db.display()
        );
        return Ok(());
    }

    let cache_src = std::env::var("MEV_SCOUT_PARITY_CACHE_DB")
        .map(|p| resolve_path(&p))
        .unwrap_or_else(|_| repo_path("cache/avalanche-mev-scout.sqlite"));
    if !cache_src.exists() {
        eprintln!(
            "skip: discovery cache {} missing; run `discover` for Avalanche first",
            cache_src.display()
        );
        return Ok(());
    }
    let cache_path = temp_copy(&cache_src, "cache")?;
    config.output.db_path = cache_path.to_string_lossy().into_owned();

    let started = std::time::Instant::now();
    let validation = mev_scout_core::config::validation::validate_live(&config)?;
    let chain = validation.chain_name;
    let setup = init_rpc(&config, chain, true).await?;
    eprintln!("rpc ready in {:?}", started.elapsed());
    let cache = SqliteStore::open(config.effective_db_path(&chain))?;
    let explorer = ExplorerStore::open_read_only(&explorer_db)?;

    if cache.list_discovered_pools().map(|p| p.is_empty()).unwrap_or(true) {
        let seeded = seed_registry_from_explorer(&cache, &explorer, from, to);
        eprintln!("seeded {seeded} pools from explorer swaps");
    }

    let registry = load_pool_registry(&config, &chain);
    if registry.tokens.is_empty() {
        eprintln!(
            "skip: pool registry is empty in {}; ingest pools before a parity run",
            cache_path.display()
        );
        return Ok(());
    }

    let mut pool_manager = PoolManager::new();
    pool_manager.set_max_pairs_per_token(config.backtest.max_pairs_per_token);
    pool_manager.set_tax_quoting(config.live.fot_quote);
    pool_manager.set_concurrency_limit(4);
    if let Some(vault) = validation.chain_config.balancer_vault {
        pool_manager = pool_manager.with_balancer_vault(vault);
    }
    if let Some(native) = validation.chain_config.wrapped_native_token {
        pool_manager = pool_manager.with_wrapped_native(native);
    }
    BacktestRunner::init_pools(
        &mut pool_manager,
        &setup.rpc,
        from.saturating_sub(1),
        Some(&cache),
    )
    .await;
    eprintln!("pool init done in {:?}", started.elapsed());
    let mut by_type: HashMap<String, usize> = HashMap::new();
    for addr in pool_manager.pool_addresses() {
        if let Some(state) = pool_manager.get(&addr) {
            *by_type.entry(format!("{:?}", state.info().dex_type)).or_default() += 1;
        }
    }
    eprintln!("pools by type: {by_type:?}");
    if let Ok(raw) = std::env::var("MEV_SCOUT_PARITY_VENUES") {
        let venues: HashSet<String> = raw.split(',').map(|s| s.trim().to_string()).collect();
        pool_manager.retain_pools(|state| {
            let name = match state.info().dex_type {
                DexType::UniswapV2 => "v2",
                DexType::UniswapV3 => "v3",
                DexType::Solidly => "solidly",
                DexType::Curve => "curve",
                DexType::Balancer => "balancer",
                DexType::TraderJoeLB => "lb",
                DexType::Pendle => "pendle",
                _ => "other",
            };
            venues.contains(name)
        });
        eprintln!(
            "venues restricted to {:?}: {} pools remain",
            venues,
            pool_manager.pool_addresses().len()
        );
    }
    if pool_manager.pool_addresses().is_empty() {
        eprintln!(
            "skip: no pool state hydrated at block {}; an archive RPC is required",
            from.saturating_sub(1)
        );
        return Ok(());
    }

    // sAVAX rate is resolved after the blocks are cached (see below), so the
    // archive RPC only sees the single rate call.

    let gas_config = GasConfig {
        gas_limit: config.gas.gas_limit,
        gas_model: validation.gas_model,
        priority_fee_gwei: config.gas.priority_fee_gwei,
        flash_loan_provider: validation.flash_loan_provider,
        winning_bid_premium: config.gas.winning_bid_premium,
        percentile_gas_price: None,
        calibration: Default::default(),
    };

    let range = ResolvedRange {
        start_block: from,
        end_block: to,
        block_count: to - from + 1,
        mode: RangeMode::Range(from, to),
    };
    // Receipt fetching goes to a full node (publicnode serves historical
    // getBlockReceipts); archive state stays on the configured RPC.
    let fetch_url = std::env::var("MEV_SCOUT_PARITY_FETCH_RPC").unwrap_or_else(|_| {
        "https://avalanche-c-chain-rpc.publicnode.com".to_string()
    });
    let fetch_rpc = mev_scout_core::rpc::RpcClient::from_urls(
        &[fetch_url.as_str()],
        validation.chain_config.chain_id,
    )?;
    eprintln!("fetching {} blocks from {from}", range.block_count);
    let mut fetcher = Fetcher::new(fetch_rpc.clone(), cache.clone());
    fetcher = fetcher.with_block_concurrency(8);
    let tick: Option<&fn() -> bool> = None;
    fetcher.fetch_range(&range, tick).await?;
    eprintln!("fetch done in {:?}", started.elapsed());

    let start_ts = cache
        .get_block(from)?
        .map(|b| b.timestamp)
        .unwrap_or_else(mev_scout_core::utils::epoch_secs);
    let mut savax_cache = SavaxRateCache::default();
    let savax_rate = fetch_savax_exchange_rate(
        &setup.rpc,
        validation.chain_config.savax,
        from,
        start_ts,
        &mut savax_cache,
    )
    .await;

    let wrapped_native = validation
        .chain_config
        .wrapped_native_token
        .unwrap_or(Address::ZERO);
    let mut blue_chips = validation
        .chain_config
        .profit_stable_tokens
        .clone()
        .unwrap_or_default();
    if !wrapped_native.is_zero() {
        blue_chips.push(wrapped_native);
    }
    blue_chips.retain(|a| !a.is_zero());
    let policy = ArbTagPolicy {
        blue_chips,
        wrapped_native,
        savax: validation.chain_config.savax,
        epoch_venue_pools: registry.epoch_venue.iter().copied().collect(),
        gmx_market_tokens: validation
            .chain_config
            .gmx_market_tokens
            .clone()
            .unwrap_or_default(),
        gmx_event_emitters: validation
            .chain_config
            .gmx_event_emitters
            .clone()
            .unwrap_or_default(),
        flash_financed: config.paper.sim_flash_loan,
        savax_rate_wad: savax_rate,
    };

    let replayer = BlockReplayer::new(
        tokio::runtime::Handle::current(),
        cache.clone(),
        fetch_rpc,
        validation.chain_config.chain_id,
    );
    let mut runner = BacktestRunner::new(replayer, pool_manager, gas_config)
        .with_tag_policy(policy)
        .with_sim_seeds(true, 32)
        .with_record_rejections(false);

    eprintln!("replaying (log-only) {from}..={to}");
    // horizon > end → every block uses the log-only path (no historical EVM
    // state needed per block; pool state is maintained from logs).
    // The multi-hop DFS is deep; libtest's 2 MB Windows stack overflows on a
    // 3.3k-block log-only replay. Run it on a dedicated stack, block by block,
    // so a failure names its block instead of only "stack overflow".
    let progress = |block: u64| {
        eprintln!("replay block {block} at {:?}", started.elapsed());
    };
    let stack_mb = env_u64("MEV_SCOUT_PARITY_STACK_MB").unwrap_or(64) as usize;
    let live_opps: Vec<mev_scout_core::types::MevOpportunity> = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("parity-replay".into())
            .stack_size(stack_mb * 1024 * 1024)
            .spawn_scoped(scope, || -> anyhow::Result<Vec<_>> {
                let mut all = Vec::new();
                for block in from..=to {
                    progress(block);
                    match runner.sync_block_from_logs(block) {
                        Ok((mut opps, _stats, _prices)) => {
                            runner.finish_log_block();
                            all.append(&mut opps);
                        }
                        Err(e) => {
                            runner.rollback_log_block();
                            anyhow::bail!("block {block}: {e}");
                        }
                    }
                }
                Ok(all)
            })
            .expect("spawn replay thread")
            .join()
            .expect("replay thread panicked")
    })?;
    eprintln!("replay done in {:?}", started.elapsed());
    let live_opps: Vec<_> = live_opps
        .into_iter()
        .filter(|o| o.strategy != mev_scout_core::types::Strategy::Jit)
        .collect();
    eprintln!("live rows: {}", live_opps.len());

    // State-sim the run's candidates against the captured pool snapshots,
    // exactly like `live --sim state` (no exec): fills vs counted skips.
    let seeds = runner.take_sim_seeds();
    let sim_policy = SimPolicy {
        mode: SimMode::StateSim,
        flash_loan: config.paper.sim_flash_loan,
        fee_bps: validation.flash_loan_provider.fee_rate_bps(),
        max_candidates_per_block: config.paper.sim_max_candidates_per_block,
    };
    let ledger = LedgerPolicy {
        starting_gas_wei: mev_scout_core::paper::default_starting_gas_wei(),
        reserve_wei: 0,
        max_fills_per_block: mev_scout_core::paper::HARD_MAX_FILLS_PER_BLOCK,
    }
    .apply_simulated(&live_opps, &seeds, &sim_policy, Some(runner.pool_manager()));
    let mut skip_counts: HashMap<String, usize> = HashMap::new();
    for skip in &ledger.skips {
        *skip_counts.entry(format!("{:?}", skip.reason)).or_default() += 1;
    }
    let by_tag = aggregate_fills(&ledger.fills, 0.0).by_tag;
    let tag_fills: HashMap<String, usize> =
        by_tag.iter().map(|(tag, m)| (tag.clone(), m.count)).collect();
    eprintln!(
        "state-sim: {} seeds, {} fills, {} skips {skip_counts:?}, fills by tag {tag_fills:?}",
        seeds.len(),
        ledger.fills.len(),
        ledger.skips.len(),
    );

    // Index live rows by block for the join.
    let mut live_by_block: HashMap<u64, Vec<&mev_scout_core::types::MevOpportunity>> = HashMap::new();
    for opp in &live_opps {
        live_by_block.entry(opp.block_number).or_default().push(opp);
    }

    let ops = explorer.ops_in_range(from, to, &[MevKind::ArbAtomic], None)?;

    let mut explorer_tag_counts: HashMap<String, usize> = HashMap::new();
    let mut live_tag_counts: HashMap<String, usize> = HashMap::new();
    for opp in &live_opps {
        for tag in &opp.tags {
            *live_tag_counts.entry(tag.clone()).or_default() += 1;
        }
    }

    let active_venues = std::env::var("MEV_SCOUT_PARITY_VENUES")
        .ok()
        .map(|raw| -> HashSet<String> { raw.split(',').map(|s| s.trim().to_string()).collect() });
    let venue_enabled = |venue: &str| {
        active_venues
            .as_ref()
            .map(|set| set.contains(venue))
            .unwrap_or(true)
    };
    let mut missing: Vec<(u64, String, String)> = Vec::new(); // (block, hash, tag)
    let mut unmatched_ops = 0usize;
    let mut compared = 0usize;
    let mut venue_skipped: HashMap<String, usize> = HashMap::new();
    for op in &ops {
        let tags = explorer_tags(op.details_json.as_deref());
        for tag in &tags {
            *explorer_tag_counts.entry(tag.clone()).or_default() += 1;
        }
        let Some(candidates) = live_by_block.get(&op.block_number) else {
            unmatched_ops += 1;
            continue;
        };
        let hash = op.tx_hash.to_lowercase();
        let pools = explorer_pools(op.route_json.as_deref());
        let matched: Vec<_> = candidates
            .iter()
            .filter(|live| {
                live.tx_hash
                    .is_some_and(|h| format!("{h}").eq_ignore_ascii_case(&hash))
                    || !pools.is_disjoint(&live_pools(live))
            })
            .collect();
        if matched.is_empty() {
            unmatched_ops += 1;
            continue;
        }
        compared += 1;
        let live_tags: HashSet<String> =
            matched.iter().flat_map(|o| o.tags.iter().cloned()).collect();
        for tag in &tags {
            if EXCLUDED_TAGS.contains(&tag.as_str()) {
                continue;
            }
            // Venue-scoped tags cannot be produced while their venue set is
            // filtered out of the replay; report them instead of failing.
            if tag == "curve_imbalance" && !venue_enabled("curve") {
                *venue_skipped.entry(tag.clone()).or_default() += 1;
                continue;
            }
            if tag == "rate_provider_staleness" && !venue_enabled("balancer") {
                *venue_skipped.entry(tag.clone()).or_default() += 1;
                continue;
            }
            if !live_tags.contains(tag) {
                missing.push((op.block_number, hash.clone(), tag.clone()));
            }
        }
    }

    eprintln!("explorer arb_atomic rows: {}", ops.len());
    eprintln!("joined rows: {compared} (unmatched: {unmatched_ops})");
    eprintln!("explorer tags: {explorer_tag_counts:?}");
    eprintln!("live tags: {live_tag_counts:?}");
    if !venue_skipped.is_empty() {
        eprintln!("venue-scoped tags not checked in this run: {venue_skipped:?}");
    }
    if !missing.is_empty() {
        for (block, hash, tag) in missing.iter().take(50) {
            eprintln!("MISSING tag {tag} block {block} tx {hash}");
        }
        anyhow::bail!(
            "live tag parity failed: {} explorer tag(s) missing from matched live rows",
            missing.len()
        );
    }
    eprintln!("live tag parity: OK (no missing supported tag)");
    let _ = std::fs::remove_file(&cache_path);
    Ok(())
}