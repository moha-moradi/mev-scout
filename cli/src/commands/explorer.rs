//! `mev-scout explorer` subcommands:
//! doctor, index, live, stats, top, show, explain, validate, export.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use alloy::primitives::{B256, U256};
use anyhow::Context;
use comfy_table::Table;

use crate::cli::ValidateArgs;
use crate::rpc_setup::init_rpc;
use mev_scout_core::config::validation;
use mev_scout_core::config::Config;
use mev_scout_core::explorer::ingest::IngestConfig;
use mev_scout_core::explorer::store::{ExplorerStore, MevOpRow};
use mev_scout_core::explorer::validate;
use mev_scout_core::explorer::MevKind;
use mev_scout_core::types::ChainName;
use mev_scout_core::utils::epoch_secs;

// ── shared setup ────────────────────────────────────────────────────────

fn explorer_store(config: &Config, chain: ChainName) -> anyhow::Result<ExplorerStore> {
    ExplorerStore::open(config.effective_explorer_db_path(&chain))
}

#[allow(dead_code)]
fn ingest_config(config: &Config, chain: ChainName) -> anyhow::Result<IngestConfig> {
    let (_, chain_cfg) = validation::resolve_chain(config).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut cfg = IngestConfig::from_chain(chain, &chain_cfg);
    cfg.confirmations = config.explorer.confirmations;
    Ok(cfg)
}

fn parse_kinds(s: &str) -> anyhow::Result<Vec<MevKind>> {
    let mut out = Vec::new();
    for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        out.push(MevKind::parse(part).ok_or_else(|| anyhow::anyhow!("unknown kind '{part}'"))?);
    }
    Ok(out)
}

fn since_ts(since: Option<&str>) -> u64 {
    let now = epoch_secs();
    match since {
        Some("1d") => now - 86_400,
        Some("7d") => now - 7 * 86_400,
        Some("30d") => now - 30 * 86_400,
        Some("all") | None => 0,
        Some(other) => {
            tracing::warn!("unknown --since '{other}', using all");
            0
        }
    }
}

fn short_addr(s: &str) -> String {
    if s.len() == 42 && s.starts_with("0x") {
        format!("{}..{}", &s[..8], &s[s.len() - 6..])
    } else {
        s.to_string()
    }
}

fn short_err(e: &anyhow::Error) -> String {
    format!("{e:#}").chars().take(40).collect()
}

fn parse_duration(s: &str) -> anyhow::Result<std::time::Duration> {
    humantime::parse_duration(s).with_context(|| format!("invalid duration '{s}'"))
}

fn stop_flag_with_deadline(deadline: Option<std::time::Duration>) -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_ctrl = stop.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        stop_ctrl.store(true, Ordering::Relaxed);
    });
    if let Some(d) = deadline {
        let stop_d = stop.clone();
        tokio::spawn(async move {
            let t0 = std::time::Instant::now();
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                if t0.elapsed() >= d {
                    stop_d.store(true, Ordering::Relaxed);
                    break;
                }
            }
        });
    }
    stop
}

mod doctor;
mod export;
mod index;
mod show;
mod stats;
mod validator;

pub use doctor::cmd_doctor;
pub use export::cmd_export;
pub use index::{cmd_index, cmd_live_feed};
pub use show::{cmd_explain, cmd_show};
pub use stats::{cmd_stats, cmd_top};
pub use validator::cmd_validate;
