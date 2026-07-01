use std::collections::{HashMap, HashSet};

use alloy::primitives::Address;
use serde::{Deserialize, Serialize};
use tracing;

use super::client::DuneClient;
use crate::dune::pool_discovery::dune_chain_label;
use crate::types::MevOpportunity;
use crate::types::Strategy;

/// A sandwich attack as recorded in Dune's `dex.sandwiches`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuneSandwichEvent {
    pub block_number: u64,
    pub victim_tx_hash: String,
    pub front_tx_hash: String,
    pub back_tx_hash: String,
    pub sandwich_type: Option<String>,
    pub pool_address: Option<Address>,
    pub mev_profit_eth: Option<f64>,
}

/// An arbitrage opportunity derived from Dune's `dex.trades`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuneArbitrageEvent {
    pub block_number: u64,
    pub tx_hash: String,
    pub pool_a: Option<Address>,
    pub pool_b: Option<Address>,
    pub token_in: Option<Address>,
    pub token_out: Option<Address>,
    pub amount_usd: Option<f64>,
}

/// A flash loan event from Dune's `lending.flashloans`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuneFlashLoanEvent {
    pub block_number: u64,
    pub tx_hash: String,
    pub protocol: Option<String>,
    pub token_address: Option<Address>,
    pub amount_usd: Option<f64>,
    pub amount: Option<String>,
    pub fee: Option<String>,
}

/// Comparison result between MEV Scout and Dune for a single opportunity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpportunityComparison {
    pub scout_id: Option<String>,
    pub block_number: u64,
    pub strategy: String,
    pub confirmed_by_dune: bool,
    pub dune_event: Option<DuneMatchDetail>,
    pub pool_addresses: Vec<Address>,
}

/// Details of a Dune match.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuneMatchDetail {
    pub source: String,
}

/// Full audit report comparing MEV Scout detection vs Dune curated data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuneAuditReport {
    pub chain: String,
    pub start_block: u64,
    pub end_block: u64,
    pub scout_total: usize,
    pub dune_sandwiches: usize,
    pub dune_arbitrages: usize,
    pub dune_flash_loans: usize,
    pub confirmed_by_both: usize,
    pub only_in_dune: usize,
    pub only_in_scout: usize,
    pub comparisons: Vec<OpportunityComparison>,
    pub unmatched_dune_events: Vec<DuneUnmatchedEvent>,
}

/// A Dune event that MEV Scout did not detect.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuneUnmatchedEvent {
    pub block_number: u64,
    pub event_type: String,
    pub pool_address: Option<Address>,
}

/// Fetch all sandwich events from Dune for a block range.
pub async fn fetch_sandwiches_from_dune(
    client: &DuneClient,
    query_id: u64,
    chain: &str,
    from_block: u64,
    to_block: u64,
) -> anyhow::Result<Vec<DuneSandwichEvent>> {
    let dune_chain = dune_chain_label(chain);
    let params: &[(&str, &str)] = &[
        ("chain", dune_chain.as_str()),
        ("from_block", &from_block.to_string()),
        ("to_block", &to_block.to_string()),
    ];

    let result = client.execute_query_by_id(query_id, params).await?;
    let rows = match result.result {
        Some(ref r) => &r.rows,
        None => return Ok(Vec::new()),
    };

    let mut events = Vec::with_capacity(rows.len());
    for row in rows {
        events.push(DuneSandwichEvent {
            block_number: DuneClient::col_as_u64(row, "block_number").unwrap_or(0),
            victim_tx_hash: DuneClient::col_as_string(row, "victim_tx_hash").unwrap_or_default(),
            front_tx_hash: DuneClient::col_as_string(row, "front_tx_hash").unwrap_or_default(),
            back_tx_hash: DuneClient::col_as_string(row, "back_tx_hash").unwrap_or_default(),
            sandwich_type: DuneClient::col_as_string(row, "sandwich_type"),
            pool_address: DuneClient::col_as_address(row, "pool_address"),
            mev_profit_eth: DuneClient::col_as_f64(row, "mev_profit_eth"),
        });
    }
    Ok(events)
}

/// Fetch arbitrage events from Dune for a block range.
pub async fn fetch_arbitrages_from_dune(
    client: &DuneClient,
    query_id: u64,
    chain: &str,
    from_block: u64,
    to_block: u64,
) -> anyhow::Result<Vec<DuneArbitrageEvent>> {
    let dune_chain = dune_chain_label(chain);
    let params: &[(&str, &str)] = &[
        ("chain", dune_chain.as_str()),
        ("from_block", &from_block.to_string()),
        ("to_block", &to_block.to_string()),
    ];

    let result = client.execute_query_by_id(query_id, params).await?;
    let rows = match result.result {
        Some(ref r) => &r.rows,
        None => return Ok(Vec::new()),
    };

    let mut events = Vec::with_capacity(rows.len());
    for row in rows {
        events.push(DuneArbitrageEvent {
            block_number: DuneClient::col_as_u64(row, "block_number").unwrap_or(0),
            tx_hash: DuneClient::col_as_string(row, "tx_hash").unwrap_or_default(),
            pool_a: DuneClient::col_as_address(row, "pool_a"),
            pool_b: DuneClient::col_as_address(row, "pool_b"),
            token_in: DuneClient::col_as_address(row, "token_in"),
            token_out: DuneClient::col_as_address(row, "token_out"),
            amount_usd: DuneClient::col_as_f64(row, "amount_usd"),
        });
    }
    Ok(events)
}

/// Fetch flash loan events from Dune for a block range.
pub async fn fetch_flash_loans_from_dune(
    client: &DuneClient,
    query_id: u64,
    chain: &str,
    from_block: u64,
    to_block: u64,
) -> anyhow::Result<Vec<DuneFlashLoanEvent>> {
    let dune_chain = dune_chain_label(chain);
    let params: &[(&str, &str)] = &[
        ("chain", dune_chain.as_str()),
        ("from_block", &from_block.to_string()),
        ("to_block", &to_block.to_string()),
    ];

    let result = client.execute_query_by_id(query_id, params).await?;
    let rows = match result.result {
        Some(ref r) => &r.rows,
        None => return Ok(Vec::new()),
    };

    let mut events = Vec::with_capacity(rows.len());
    for row in rows {
        events.push(DuneFlashLoanEvent {
            block_number: DuneClient::col_as_u64(row, "block_number").unwrap_or(0),
            tx_hash: DuneClient::col_as_string(row, "tx_hash").unwrap_or_default(),
            protocol: DuneClient::col_as_string(row, "protocol"),
            token_address: DuneClient::col_as_address(row, "token_address"),
            amount_usd: DuneClient::col_as_f64(row, "amount_usd"),
            amount: DuneClient::col_as_string(row, "amount"),
            fee: DuneClient::col_as_string(row, "fee"),
        });
    }
    Ok(events)
}

/// Run a complete Dune audit comparing MEV Scout opportunities against Dune data.
///
/// # Matching logic
/// - **Sandwiches**: matched if Dune has a sandwich event in the same block with
///   overlapping pool addresses
/// - **Arbitrages**: matched if Dune has an arbitrage trade in the same block
///   involving one of the same pools
pub async fn run_audit(
    client: &DuneClient,
    config: &DuneAuditConfig,
    scout_opportunities: &[MevOpportunity],
    chain: &str,
    from_block: u64,
    to_block: u64,
) -> anyhow::Result<DuneAuditReport> {
    let dune_chain = dune_chain_label(chain);
    let _from_str = from_block.to_string();
    let _to_str = to_block.to_string();

    // ── Fetch all Dune events ──────────────────────────────────────────
    let dune_sandwiches = if let Some(qid) = config.sandwich_query_id {
        let r = fetch_sandwiches_from_dune(client, qid, &dune_chain, from_block, to_block).await?;
        tracing::info!("Dune audit: fetched {} sandwiches", r.len());
        r
    } else {
        Vec::new()
    };
    let dune_arbitrages = if let Some(qid) = config.arbitrage_query_id {
        let r = fetch_arbitrages_from_dune(client, qid, &dune_chain, from_block, to_block).await?;
        tracing::info!("Dune audit: fetched {} arbitrages", r.len());
        r
    } else {
        Vec::new()
    };
    let dune_flash_loans = if let Some(qid) = config.flash_loan_query_id {
        let r = fetch_flash_loans_from_dune(client, qid, &dune_chain, from_block, to_block).await?;
        tracing::info!("Dune audit: fetched {} flash loans", r.len());
        r
    } else {
        Vec::new()
    };

    // ── Index Dune pools by block for matching ─────────────────────────
    let dune_sandwich_pools: HashMap<u64, HashSet<Address>> = {
        let mut map: HashMap<u64, HashSet<Address>> = HashMap::new();
        for s in &dune_sandwiches {
            if let Some(pool) = s.pool_address {
                map.entry(s.block_number).or_default().insert(pool);
            }
        }
        map
    };
    let dune_arb_pools: HashMap<u64, HashSet<Address>> = {
        let mut map: HashMap<u64, HashSet<Address>> = HashMap::new();
        for a in &dune_arbitrages {
            if let Some(pa) = a.pool_a {
                map.entry(a.block_number).or_default().insert(pa);
            }
            if let Some(pb) = a.pool_b {
                map.entry(a.block_number).or_default().insert(pb);
            }
        }
        map
    };

    // ── Compare each MEV Scout opportunity ─────────────────────────────
    let mut comparisons = Vec::with_capacity(scout_opportunities.len());
    let mut scout_blocks: HashSet<u64> = HashSet::new();
    let mut matched_dune_blocks: HashSet<u64> = HashSet::new();

    for opp in scout_opportunities {
        scout_blocks.insert(opp.block_number);
        let (confirmed, source) = match opp.strategy {
            Strategy::Sandwich => {
                let block_pools = dune_sandwich_pools.get(&opp.block_number);
                let matched = block_pools.map_or(false, |pools| {
                    pools.contains(&opp.pool_a) || pools.contains(&opp.pool_b)
                });
                if matched { matched_dune_blocks.insert(opp.block_number); }
                (matched, "dex.sandwiches")
            }
            Strategy::TwoHopArb | Strategy::MultiHopArb => {
                let block_pools = dune_arb_pools.get(&opp.block_number);
                let matched = block_pools.map_or(false, |pools| {
                    pools.contains(&opp.pool_a) || pools.contains(&opp.pool_b)
                });
                if matched { matched_dune_blocks.insert(opp.block_number); }
                (matched, "dex.trades")
            }
            _ => (false, ""),
        };

        comparisons.push(OpportunityComparison {
            scout_id: opp.canonical_id.clone(),
            block_number: opp.block_number,
            strategy: opp.strategy.to_string(),
            confirmed_by_dune: confirmed,
            dune_event: if confirmed {
                Some(DuneMatchDetail { source: source.into() })
            } else {
                None
            },
            pool_addresses: vec![opp.pool_a, opp.pool_b],
        });
    }

    // ── Find Dune events in blocks where MEV Scout found nothing ───────
    let mut unmatched_dune_events = Vec::new();
    for s in &dune_sandwiches {
        if !matched_dune_blocks.contains(&s.block_number) {
            unmatched_dune_events.push(DuneUnmatchedEvent {
                block_number: s.block_number,
                event_type: "sandwich".into(),
                pool_address: s.pool_address,
            });
        }
    }
    for a in &dune_arbitrages {
        if !matched_dune_blocks.contains(&a.block_number) {
            unmatched_dune_events.push(DuneUnmatchedEvent {
                block_number: a.block_number,
                event_type: "arbitrage".into(),
                pool_address: a.pool_a,
            });
        }
    }

    let scout_total = scout_opportunities.len();
    let confirmed_by_both = comparisons.iter().filter(|c| c.confirmed_by_dune).count();
    let only_in_scout = scout_total.saturating_sub(confirmed_by_both);
    let only_in_dune = unmatched_dune_events.len();

    tracing::info!(
        "Dune audit complete: {scout_total} scout opps, {confirmed_by_both} confirmed, \
         {only_in_dune} Dune-only, {only_in_scout} scout-only"
    );

    Ok(DuneAuditReport {
        chain: chain.to_string(),
        start_block: from_block,
        end_block: to_block,
        scout_total,
        dune_sandwiches: dune_sandwiches.len(),
        dune_arbitrages: dune_arbitrages.len(),
        dune_flash_loans: dune_flash_loans.len(),
        confirmed_by_both,
        only_in_dune,
        only_in_scout,
        comparisons,
        unmatched_dune_events,
    })
}

/// Configuration for a Dune audit run.
#[derive(Debug, Clone, Default)]
pub struct DuneAuditConfig {
    pub sandwich_query_id: Option<u64>,
    pub arbitrage_query_id: Option<u64>,
    pub flash_loan_query_id: Option<u64>,
}

impl From<&crate::config::Config> for DuneAuditConfig {
    fn from(cfg: &crate::config::Config) -> Self {
        Self {
            // Re-use existing query IDs (user creates saved queries from templates)
            sandwich_query_id: cfg.dune_verify_sandwich_query_id,
            arbitrage_query_id: cfg.dune_v2_pools_query_id, // temporary — user sets this to arbitrage query ID
            flash_loan_query_id: None,
        }
    }
}
