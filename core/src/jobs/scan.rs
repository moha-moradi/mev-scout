use alloy::primitives::{Address, U256};
use anyhow::Context;

use crate::chain::events::TradeEvent;
use crate::config::validation;
use crate::config::Config;
use crate::progress::JobProgress;
use crate::resolver::RangeResolver;

use super::rpc::init_rpc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScanKind {
    #[default]
    Trades,
    Transfers,
    Flashloans,
    Liquidations,
    Labels,
}

#[derive(Debug, Clone, Default)]
pub struct ScanOpts {
    pub kind: ScanKind,
    pub addresses: Option<Vec<Address>>,
    pub batch_size: u64,
    pub limit: usize,
    pub min_value: Option<U256>,
}

pub enum ScanOutcome {
    Trades(Vec<TradeEvent>),
    Transfers(Vec<crate::chain::events::TransferEvent>),
    Flashloans(Vec<crate::chain::events::FlashLoanEvent>),
    Liquidations(Vec<crate::chain::events::LiquidationEvent>),
    Labels(Vec<(String, Option<String>)>),
}

pub async fn job_scan(
    config: &Config,
    opts: &ScanOpts,
    progress: &dyn JobProgress,
) -> anyhow::Result<ScanOutcome> {
    let (chain_name, _) = validation::resolve_chain(config).context("failed to resolve chain")?;
    let setup = init_rpc(config, chain_name, true).await?;
    let rpc = setup.rpc;

    let resolver = RangeResolver::new(rpc.clone());
    let resolved = resolver.resolve(&config.range_spec()?.resolve()).await?;
    let from = resolved.start_block;
    let to = resolved.end_block;

    let addrs_ref = opts.addresses.as_deref();

    let outcome = match opts.kind {
        ScanKind::Trades => {
            let trades =
                crate::chain::trades::scan_trades(&rpc, from, to, opts.batch_size, addrs_ref)
                    .await
                    .context("trade scan failed")?;
            progress.log(&format!(
                "trade scan: {} trade(s) in {from}-{to} (showing {})",
                trades.len(),
                opts.limit
            ));
            ScanOutcome::Trades(trades)
        }
        ScanKind::Transfers => {
            let min_value = opts.min_value.unwrap_or(U256::ZERO);
            let transfers = if min_value > U256::ZERO {
                crate::chain::transfers::scan_whale_transfers(
                    &rpc,
                    from,
                    to,
                    opts.batch_size,
                    min_value,
                    addrs_ref,
                )
                .await
                .context("whale transfer scan failed")?
            } else {
                crate::chain::transfers::scan_transfers(&rpc, from, to, opts.batch_size, addrs_ref)
                    .await
                    .context("transfer scan failed")?
            };
            progress.log(&format!(
                "transfer scan: {} transfer(s) in {from}-{to} (showing {})",
                transfers.len(),
                opts.limit
            ));
            ScanOutcome::Transfers(transfers)
        }
        ScanKind::Flashloans => {
            let loans = crate::chain::flashloans::scan_flash_loans(
                &rpc,
                from,
                to,
                opts.batch_size,
                addrs_ref,
            )
            .await
            .context("flash loan scan failed")?;
            progress.log(&format!(
                "flashloan scan: {} loan(s) in {from}-{to} (showing {})",
                loans.len(),
                opts.limit
            ));
            ScanOutcome::Flashloans(loans)
        }
        ScanKind::Liquidations => {
            let liqs = crate::chain::liquidations::scan_liquidations(
                &rpc,
                from,
                to,
                opts.batch_size,
                addrs_ref,
            )
            .await
            .context("liquidation scan failed")?;
            progress.log(&format!(
                "liquidation scan: {} liquidation(s) in {from}-{to} (showing {})",
                liqs.len(),
                opts.limit
            ));
            ScanOutcome::Liquidations(liqs)
        }
        ScanKind::Labels => {
            let db = crate::chain::labels::LabelDb::load();
            let mut rows = Vec::new();
            if let Some(addrs) = &opts.addresses {
                for addr in addrs {
                    let key = format!("{addr}");
                    let label = db.get(&key).map(String::from);
                    if let Some(ref l) = label {
                        progress.log(&format!("{key} => {l}"));
                    } else {
                        progress.log(&format!("{key} => (unknown)"));
                    }
                    rows.push((key, label));
                }
            } else {
                progress.log(&format!(
                    "Loaded {} address labels (use --address to look up specific addresses)",
                    db.len()
                ));
            }
            ScanOutcome::Labels(rows)
        }
    };

    Ok(outcome)
}
