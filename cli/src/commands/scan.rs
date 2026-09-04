use anyhow::Context;
use comfy_table::Table;
use mev_scout_core::config::validation;
use mev_scout_core::config::Config;

use crate::cli::{ScanArgs, ScanKind};
use crate::rpc_setup::init_rpc;

pub async fn cmd_scan(config: &Config, args: &ScanArgs) -> anyhow::Result<()> {
    let (chain_name, _chain_config) = validation::resolve_chain(config)
        .context("failed to resolve chain")?;

    let setup = init_rpc(config, chain_name, true).await?;
    let rpc = setup.rpc;

    let resolver = mev_scout_core::resolver::RangeResolver::new(rpc.clone());
    let resolved = resolver
        .resolve(&validation::resolve_block_range(
            config.days,
            config.blocks,
            config.block,
            config.from_block,
            config.to_block,
        )?)
        .await?;
    let from = resolved.start_block;
    let to = resolved.end_block;

    let addresses: Option<Vec<alloy::primitives::Address>> = args
        .addresses
        .as_ref()
        .map(|a| {
            a.iter()
                .filter_map(|s| s.parse().ok())
                .collect()
        })
        .filter(|v: &Vec<alloy::primitives::Address>| !v.is_empty());

    let addrs_ref = addresses.as_deref();

    match &args.kind {
        ScanKind::Trades => {
            let trades = mev_scout_core::chain::trades::scan_trades(
                &rpc,
                from,
                to,
                args.batch_size,
                addrs_ref,
            )
            .await
            .context("trade scan failed")?;
            print_trades(&trades, args, config.output.output.as_str());
        }
        ScanKind::Transfers => {
            let min_value = args
                .min_value
                .as_ref()
                .and_then(|s| s.parse().ok())
                .unwrap_or(alloy::primitives::U256::ZERO);
            let transfers = if min_value > alloy::primitives::U256::ZERO {
                mev_scout_core::chain::transfers::scan_whale_transfers(
                    &rpc,
                    from,
                    to,
                    args.batch_size,
                    min_value,
                    addrs_ref,
                )
                .await
                .context("whale transfer scan failed")?
            } else {
                mev_scout_core::chain::transfers::scan_transfers(
                    &rpc,
                    from,
                    to,
                    args.batch_size,
                    addrs_ref,
                )
                .await
                .context("transfer scan failed")?
            };
            print_transfers(&transfers, args, config.output.output.as_str());
        }
        ScanKind::Flashloans => {
            let loans = mev_scout_core::chain::flashloans::scan_flash_loans(
                &rpc,
                from,
                to,
                args.batch_size,
                addrs_ref,
            )
            .await
            .context("flash loan scan failed")?;
            print_flash_loans(&loans, args, config.output.output.as_str());
        }
        ScanKind::Liquidations => {
            let liqs = mev_scout_core::chain::liquidations::scan_liquidations(
                &rpc,
                from,
                to,
                args.batch_size,
                addrs_ref,
            )
            .await
            .context("liquidation scan failed")?;
            print_liquidations(&liqs, args, config.output.output.as_str());
        }
        ScanKind::Labels => {
            let db = mev_scout_core::chain::labels::LabelDb::load();
            if let Some(ref addrs) = args.addresses {
                for addr in addrs {
                    match db.get(addr) {
                        Some(label) => println!("{addr} => {label}"),
                        None => println!("{addr} => (unknown)"),
                    }
                }
            } else {
                println!("  Loaded {} address labels (use --address to look up specific addresses)", db.len());
            }
        }
    }

    Ok(())
}

fn print_trades(trades: &[mev_scout_core::chain::events::TradeEvent], args: &ScanArgs, out: &str) {
    let items: Vec<_> = if args.limit > 0 {
        trades.iter().take(args.limit).collect()
    } else {
        trades.iter().collect()
    };

    match out {
        "json" => {
            println!("{}", serde_json::to_string_pretty(&items).unwrap());
        }
        "csv" => {
            println!("block,tx_hash,pool,dex_type,amount_in,amount_out");
            for t in &items {
                println!(
                    "{},{},{:?},{},{},{}",
                    t.block, t.tx_hash, t.pool, t.dex_type, t.amount_in, t.amount_out,
                );
            }
        }
        _ => {
            let mut table = Table::new();
            table.set_header(vec!["Block", "TX Hash", "Pool", "DEX", "Amount In", "Amount Out"]);
            for t in &items {
                table.add_row(vec![
                    t.block.to_string(),
                    format!("{:?}", t.tx_hash),
                    format!("{:?}", t.pool),
                    t.dex_type.clone(),
                    t.amount_in.to_string(),
                    t.amount_out.to_string(),
                ]);
            }
            println!("{table}");
            println!();
            println!("  {} trade(s) found (showing {})", trades.len(), items.len());
        }
    }
}

fn print_transfers(transfers: &[mev_scout_core::chain::events::TransferEvent], args: &ScanArgs, out: &str) {
    let items: Vec<_> = if args.limit > 0 {
        transfers.iter().take(args.limit).collect()
    } else {
        transfers.iter().collect()
    };

    match out {
        "json" => {
            println!("{}", serde_json::to_string_pretty(&items).unwrap());
        }
        "csv" => {
            println!("block,tx_hash,token,from,to,value");
            for t in &items {
                println!(
                    "{},{},{:?},{:?},{:?},{}",
                    t.block, t.tx_hash, t.token, t.from, t.to, t.value,
                );
            }
        }
        _ => {
            let mut table = Table::new();
            table.set_header(vec!["Block", "TX Hash", "Token", "From", "To", "Value"]);
            for t in &items {
                table.add_row(vec![
                    t.block.to_string(),
                    format!("{:?}", t.tx_hash),
                    format!("{:?}", t.token),
                    format!("{:?}", t.from),
                    format!("{:?}", t.to),
                    t.value.to_string(),
                ]);
            }
            println!("{table}");
            println!();
            println!("  {} transfer(s) found (showing {})", transfers.len(), items.len());
        }
    }
}

fn print_flash_loans(loans: &[mev_scout_core::chain::events::FlashLoanEvent], args: &ScanArgs, out: &str) {
    let items: Vec<_> = if args.limit > 0 {
        loans.iter().take(args.limit).collect()
    } else {
        loans.iter().collect()
    };

    match out {
        "json" => {
            println!("{}", serde_json::to_string_pretty(&items).unwrap());
        }
        "csv" => {
            println!("block,tx_hash,protocol,token,amount,fee");
            for t in &items {
                println!(
                    "{},{},{},{:?},{},{}",
                    t.block,
                    t.tx_hash,
                    t.protocol,
                    t.token,
                    t.amount,
                    t.fee.map(|f| f.to_string()).unwrap_or_default(),
                );
            }
        }
        _ => {
            let mut table = Table::new();
            table.set_header(vec!["Block", "TX Hash", "Protocol", "Token", "Amount", "Fee"]);
            for t in &items {
                table.add_row(vec![
                    t.block.to_string(),
                    format!("{:?}", t.tx_hash),
                    t.protocol.clone(),
                    format!("{:?}", t.token),
                    t.amount.to_string(),
                    t.fee.map(|f| f.to_string()).unwrap_or_else(|| "-".into()),
                ]);
            }
            println!("{table}");
            println!();
            println!("  {} flash loan(s) found (showing {})", loans.len(), items.len());
        }
    }
}

fn print_liquidations(liqs: &[mev_scout_core::chain::events::LiquidationEvent], args: &ScanArgs, out: &str) {
    let items: Vec<_> = if args.limit > 0 {
        liqs.iter().take(args.limit).collect()
    } else {
        liqs.iter().collect()
    };

    match out {
        "json" => {
            println!("{}", serde_json::to_string_pretty(&items).unwrap());
        }
        "csv" => {
            println!("block,tx_hash,protocol,user,liquidator,collateral,debt_amount,collateral_amount");
            for t in &items {
                println!(
                    "{},{},{},{:?},{:?},{:?},{},{}",
                    t.block,
                    t.tx_hash,
                    t.protocol,
                    t.user,
                    t.liquidator,
                    t.collateral_asset,
                    t.debt_to_cover,
                    t.collateral_amount,
                );
            }
        }
        _ => {
            let mut table = Table::new();
            table.set_header(vec!["Block", "TX Hash", "Protocol", "User", "Liquidator", "Collateral", "Debt"]);
            for t in &items {
                table.add_row(vec![
                    t.block.to_string(),
                    format!("{:?}", t.tx_hash),
                    t.protocol.clone(),
                    format!("{:?}", t.user),
                    format!("{:?}", t.liquidator),
                    format!("{:?}", t.collateral_asset),
                    t.debt_to_cover.to_string(),
                ]);
            }
            println!("{table}");
            println!();
            println!("  {} liquidation(s) found (showing {})", liqs.len(), items.len());
        }
    }
}
