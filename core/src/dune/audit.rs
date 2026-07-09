use std::collections::{HashMap, HashSet};

use alloy::primitives::Address;
use serde::{Deserialize, Serialize};
use tracing;

use super::client::DuneClient;
use super::queries;
use crate::dune::pool_discovery::dune_chain_label;
use crate::types::MevOpportunity;
use crate::types::Strategy;

fn render_query(template: &str, chain: &str, from_block: u64, to_block: u64) -> String {
    let chain_label = dune_chain_label(chain);
    let block_month_min = approx_block_month_min(from_block, &chain_label);
    template
        .replace("{chain}", &chain_label)
        .replace("{block_month_min}", &block_month_min)
        .replace("{from_block}", &from_block.to_string())
        .replace("{to_block}", &to_block.to_string())
}

fn approx_block_month_min(block_number: u64, chain: &str) -> String {
    let (genesis_block, genesis_ts, secs_per_block) = match chain {
        "ethereum" => (0, 1438269988, 12.0),
        "polygon" => (0, 1591031691, 2.1),
        "bsc"      => (0, 1597734000, 3.0),
        "avalanche_c" => (0, 1624402800, 2.0),
        "arbitrum" => (0, 1630812600, 0.26),
        "base"     => (0, 1686787200, 2.0),
        "optimism" => (0, 1631808000, 2.0),
        _ => (0, 1609459200, 12.0),
    };
    let elapsed = (block_number.saturating_sub(genesis_block)) as f64 * secs_per_block;
    let approx_ts = genesis_ts as i64 + elapsed as i64;
    let naive = chrono::DateTime::from_timestamp(approx_ts, 0)
        .unwrap_or_default();
    naive.format("%Y-%m-%d").to_string()
}

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
/// Uses the built-in `QUERY_SANDWICHES_BY_RANGE` query.
pub async fn fetch_sandwiches_from_dune(
    client: &DuneClient,
    chain: &str,
    from_block: u64,
    to_block: u64,
) -> anyhow::Result<Vec<DuneSandwichEvent>> {
    let sql = render_query(queries::QUERY_SANDWICHES_BY_RANGE, chain, from_block, to_block);
    let result = client.execute_raw_sql(&sql).await?;
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
/// Uses the built-in `QUERY_ARBITRAGES_BY_RANGE` query.
pub async fn fetch_arbitrages_from_dune(
    client: &DuneClient,
    chain: &str,
    from_block: u64,
    to_block: u64,
) -> anyhow::Result<Vec<DuneArbitrageEvent>> {
    let sql = render_query(queries::QUERY_ARBITRAGES_BY_RANGE, chain, from_block, to_block);
    let result = client.execute_raw_sql(&sql).await?;
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
/// Uses the built-in `QUERY_FLASH_LOANS_BY_RANGE` query.
pub async fn fetch_flash_loans_from_dune(
    client: &DuneClient,
    chain: &str,
    from_block: u64,
    to_block: u64,
) -> anyhow::Result<Vec<DuneFlashLoanEvent>> {
    let sql = render_query(queries::QUERY_FLASH_LOANS_BY_RANGE, chain, from_block, to_block);
    let result = client.execute_raw_sql(&sql).await?;
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
    scout_opportunities: &[MevOpportunity],
    chain: &str,
    from_block: u64,
    to_block: u64,
) -> anyhow::Result<DuneAuditReport> {
    let dune_chain = dune_chain_label(chain);

    // ── Fetch all Dune events ──────────────────────────────────────────
    let dune_sandwiches = fetch_sandwiches_from_dune(client, &dune_chain, from_block, to_block).await?;
    tracing::info!("Dune audit: fetched {} sandwiches", dune_sandwiches.len());
    let dune_arbitrages = fetch_arbitrages_from_dune(client, &dune_chain, from_block, to_block).await?;
    tracing::info!("Dune audit: fetched {} arbitrages", dune_arbitrages.len());
    let dune_flash_loans = fetch_flash_loans_from_dune(client, &dune_chain, from_block, to_block).await?;
    tracing::info!("Dune audit: fetched {} flash loans", dune_flash_loans.len());

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


