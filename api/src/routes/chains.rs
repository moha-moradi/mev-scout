//! `GET /api/chains` — all 7 supported chains with per-chain DB presence.

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use mev_scout_core::types::ChainName;

use crate::error::ApiResult;
use crate::state::{derived_cache_db_path, derived_explorer_db_path, SharedState};

#[derive(Serialize)]
pub struct ChainDto {
    pub name: String,
    pub chain_id: u64,
    /// Wrapped-native token address (hex) from chains.toml, if configured.
    pub wrapped_native: Option<String>,
    pub has_cache_db: bool,
    pub has_explorer_db: bool,
}

pub fn router() -> Router<SharedState> {
    Router::new().route("/api/chains", get(chains))
}

async fn chains(State(state): State<SharedState>) -> ApiResult<Json<Vec<ChainDto>>> {
    let cfg = state.config.read().await;
    // Config may override DB dirs; derive candidate per-chain paths from the
    // effective base dirs so presence checks reflect real locations.
    let cache_base = std::path::PathBuf::from(cfg.effective_db_path(&cfg.chain));
    let explorer_base = std::path::PathBuf::from(cfg.effective_explorer_db_path(&cfg.chain));
    drop(cfg);

    let cache_dir = cache_base.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let explorer_dir = explorer_base
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();

    let all_chains = [
        ChainName::Polygon,
        ChainName::Avalanche,
        ChainName::Bsc,
        ChainName::Arbitrum,
        ChainName::Base,
        ChainName::Ethereum,
        ChainName::Optimism,
    ];

    let cfg_chains = mev_scout_core::config::defaults::default_chains();

    let out = all_chains
        .into_iter()
        .map(|c| {
            // Candidate path: same dir as the active config's DBs, chain-specific filename.
            let cache_candidate = if cache_dir.as_os_str().is_empty() {
                derived_cache_db_path(c)
            } else {
                cache_dir.join(format!("{}-mev-scout.sqlite", c))
            };
            let explorer_candidate = if explorer_dir.as_os_str().is_empty() {
                derived_explorer_db_path(c)
            } else {
                explorer_dir.join(format!("explorer-{}.sqlite", c))
            };
            let wrapped = cfg_chains
                .get(&c.to_string())
                .and_then(|cc| cc.wrapped_native_token.clone());
            ChainDto {
                name: c.to_string(),
                chain_id: c.chain_id(),
                wrapped_native: wrapped,
                has_cache_db: cache_candidate.exists(),
                has_explorer_db: explorer_candidate.exists(),
            }
        })
        .collect();
    Ok(Json(out))
}
