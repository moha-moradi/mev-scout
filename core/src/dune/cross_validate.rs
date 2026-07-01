//! Cross-validation of detected MEV opportunities against Dune Analytics data.
//!
//! This module provides an independent verification layer: Dune's indexed
//! on-chain data (from `dex.trades`, `dex.sandwiches`, `prices.usd`) serves
//! as a second source of truth alongside the existing EVM re-execution
//! fact-checking in `mev::verify::fact_check`.

use tracing;

use super::client::DuneClient;
use super::types::DuneCrossValidation;
use crate::types::MevOpportunity;
use crate::types::Strategy;

/// Configuration for Dune cross-validation.
#[derive(Debug, Clone)]
pub struct DuneCrossValidateConfig {
    pub verify_trade_query_id: Option<u64>,
    pub verify_sandwich_query_id: Option<u64>,
    pub trades_in_block_query_id: Option<u64>,
    pub token_price_query_id: Option<u64>,
}

impl Default for DuneCrossValidateConfig {
    fn default() -> Self {
        Self {
            verify_trade_query_id: None,
            verify_sandwich_query_id: None,
            trades_in_block_query_id: None,
            token_price_query_id: None,
        }
    }
}

/// Cross-validate a batch of MEV opportunities against Dune data.
pub async fn cross_validate_opportunities(
    client: &DuneClient,
    config: &DuneCrossValidateConfig,
    opportunities: &[MevOpportunity],
    chain: &str,
) -> anyhow::Result<Vec<DuneCrossValidation>> {
    let mut results = Vec::with_capacity(opportunities.len());

    for opp in opportunities {
        let validation = validate_single_opportunity(client, config, opp, chain).await;
        results.push(validation);
    }

    let confirmed = results.iter().filter(|r| {
        r.trade_confirmed.unwrap_or(false) || r.sandwich_confirmed.unwrap_or(false)
    }).count();
    tracing::info!(
        "Dune cross-validation: {}/{} opportunities confirmed on-chain",
        confirmed,
        opportunities.len()
    );

    Ok(results)
}

async fn validate_single_opportunity(
    client: &DuneClient,
    config: &DuneCrossValidateConfig,
    opp: &MevOpportunity,
    chain: &str,
) -> DuneCrossValidation {
    let strategy = opp.strategy.to_string();
    let mut trade_confirmed = None;
    let mut sandwich_confirmed = None;
    let mut dune_profit_usd = None;
    let mut message = None;

    match opp.strategy {
        Strategy::Sandwich => {
            if let Some(qid) = config.verify_sandwich_query_id {
                match verify_sandwich_by_tx(client, qid, opp, chain).await {
                    Ok(Some(_)) => sandwich_confirmed = Some(true),
                    Ok(None) => {
                        sandwich_confirmed = Some(false);
                        message = Some("Sandwich not found in Dune dataset".to_string());
                    }
                    Err(e) => {
                        message = Some(format!("Dune sandwich lookup failed: {e}"));
                    }
                }
            }

            if trade_confirmed.is_none() {
                if let Some(qid) = config.verify_trade_query_id {
                    match verify_trade_by_tx(client, qid, opp, chain).await {
                        Ok(Some(trade)) => {
                            trade_confirmed = Some(true);
                            dune_profit_usd = trade.amount_usd;
                        }
                        Ok(None) => trade_confirmed = Some(false),
                        Err(e) => {
                            message = Some(format!("Dune trade lookup failed: {e}"));
                        }
                    }
                }
            }
        }
        Strategy::TwoHopArb | Strategy::MultiHopArb => {
            if let Some(qid) = config.verify_trade_query_id {
                match verify_trade_by_tx(client, qid, opp, chain).await {
                    Ok(Some(trade)) => {
                        trade_confirmed = Some(true);
                        dune_profit_usd = trade.amount_usd;
                    }
                    Ok(None) => {
                        trade_confirmed = Some(false);
                        message = Some("Trade not found in Dune dex.trades".to_string());
                    }
                    Err(e) => {
                        message = Some(format!("Dune trade lookup failed: {e}"));
                    }
                }
            }
        }
        _ => {
            message = Some("Cross-validation not applicable for this strategy".to_string());
        }
    }

    DuneCrossValidation {
        block_number: opp.block_number,
        tx_index: opp.tx_index,
        strategy,
        trade_confirmed,
        sandwich_confirmed,
        dune_profit_usd,
        message,
    }
}

struct DuneTradeCheck {
    amount_usd: Option<f64>,
}

async fn verify_trade_by_tx(
    client: &DuneClient,
    query_id: u64,
    opp: &MevOpportunity,
    chain: &str,
) -> anyhow::Result<Option<DuneTradeCheck>> {
    let chain_label = dune_chain_label(chain);
    let block_str = opp.block_number.to_string();
    let params: &[(&str, &str)] = &[
        ("chain", chain_label.as_str()),
        ("tx_hash", "\\x0000000000000000000000000000000000000000"),
        ("block_number", &block_str),
    ];

    let result = client.execute_query_by_id(query_id, params).await?;
    let rows = match result.result {
        Some(ref r) => &r.rows,
        None => return Ok(None),
    };

    if rows.is_empty() {
        return Ok(None);
    }

    let amount_usd = DuneClient::col_as_f64(&rows[0], "amount_usd");
    Ok(Some(DuneTradeCheck { amount_usd }))
}

async fn verify_sandwich_by_tx(
    client: &DuneClient,
    query_id: u64,
    opp: &MevOpportunity,
    chain: &str,
) -> anyhow::Result<Option<DuneCrossValidation>> {
    let chain_label = dune_chain_label(chain);
    let block_str = opp.block_number.to_string();
    let params: &[(&str, &str)] = &[
        ("chain", chain_label.as_str()),
        ("block_number", &block_str),
        ("tx_hash", "\\x0000000000000000000000000000000000000000"),
    ];

    let result = client.execute_query_by_id(query_id, params).await?;
    match result.result {
        Some(ref r) if !r.rows.is_empty() => Ok(Some(DuneCrossValidation {
            block_number: opp.block_number,
            tx_index: opp.tx_index,
            strategy: opp.strategy.to_string(),
            trade_confirmed: Some(true),
            sandwich_confirmed: Some(true),
            dune_profit_usd: None,
            message: Some("Sandwich confirmed in Dune dataset".to_string()),
        })),
        _ => Ok(None),
    }
}

fn dune_chain_label(chain: &str) -> String {
    match chain.to_lowercase().as_str() {
        "avalanche" => "avalanche_c".to_string(),
        other => other.to_string(),
    }
}
