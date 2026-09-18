use comfy_table::Table;
use mev_scout_core::config::Config;
use mev_scout_core::jobs::{job_scan, ScanKind, ScanOpts, ScanOutcome};

use crate::cli::{ScanArgs, ScanKind as CliScanKind};
use crate::job_progress::JobProgress;

pub async fn cmd_scan(
    config: &Config,
    args: &ScanArgs,
    progress: &dyn JobProgress,
) -> anyhow::Result<()> {
    let kind = match args.kind {
        CliScanKind::Trades => ScanKind::Trades,
        CliScanKind::Transfers => ScanKind::Transfers,
        CliScanKind::Flashloans => ScanKind::Flashloans,
        CliScanKind::Liquidations => ScanKind::Liquidations,
        CliScanKind::Labels => ScanKind::Labels,
    };
    let addresses = args
        .addresses
        .as_ref()
        .map(|a| a.iter().filter_map(|s| s.parse().ok()).collect())
        .filter(|v: &Vec<alloy::primitives::Address>| !v.is_empty());
    let min_value = args.min_value.as_ref().and_then(|s| s.parse().ok());

    let opts = ScanOpts {
        kind,
        addresses,
        batch_size: args.batch_size,
        limit: args.limit,
        min_value,
    };

    let out = config.output.output.as_str();
    match job_scan(config, &opts, progress).await? {
        ScanOutcome::Trades(trades) => print_trades(&trades, args, out),
        ScanOutcome::Transfers(transfers) => print_transfers(&transfers, args, out),
        ScanOutcome::Flashloans(loans) => print_flash_loans(&loans, args, out),
        ScanOutcome::Liquidations(liqs) => print_liquidations(&liqs, args, out),
        ScanOutcome::Labels(_) => {}
    }
    Ok(())
}

struct Column<'a, T> {
    header: &'a str,
    csv_header: &'a str,
    cell: fn(&T) -> String,
}

fn col<'a, T>(header: &'a str, csv_header: &'a str, cell: fn(&T) -> String) -> Column<'a, T> {
    Column {
        header,
        csv_header,
        cell,
    }
}

fn print_events<T: serde::Serialize>(
    all: &[T],
    args: &ScanArgs,
    out: &str,
    label: &str,
    columns: &[Column<'_, T>],
) {
    let items: Vec<&T> = if args.limit > 0 {
        all.iter().take(args.limit).collect()
    } else {
        all.iter().collect()
    };

    match out {
        "json" => {
            println!("{}", serde_json::to_string_pretty(&items).unwrap());
        }
        "csv" => {
            println!(
                "{}",
                columns
                    .iter()
                    .map(|c| c.csv_header)
                    .collect::<Vec<_>>()
                    .join(",")
            );
            for t in &items {
                println!(
                    "{}",
                    columns
                        .iter()
                        .map(|c| (c.cell)(t))
                        .collect::<Vec<_>>()
                        .join(",")
                );
            }
        }
        _ => {
            let mut table = Table::new();
            table.set_header(columns.iter().map(|c| c.header));
            for t in &items {
                table.add_row(columns.iter().map(|c| (c.cell)(t)));
            }
            println!("{table}");
            println!();
            println!(
                "  {} {}(s) found (showing {})",
                all.len(),
                label,
                items.len()
            );
        }
    }
}

fn print_trades(trades: &[mev_scout_core::chain::events::TradeEvent], args: &ScanArgs, out: &str) {
    print_events(
        trades,
        args,
        out,
        "trade",
        &[
            col("Block", "block", |t: &_| t.block.to_string()),
            col("TX Hash", "tx_hash", |t| format!("{:?}", t.tx_hash)),
            col("Pool", "pool", |t| format!("{:?}", t.pool)),
            col("DEX", "dex_type", |t| t.dex_type.clone()),
            col("Amount In", "amount_in", |t| t.amount_in.to_string()),
            col("Amount Out", "amount_out", |t| t.amount_out.to_string()),
        ],
    );
}

fn print_transfers(
    transfers: &[mev_scout_core::chain::events::TransferEvent],
    args: &ScanArgs,
    out: &str,
) {
    print_events(
        transfers,
        args,
        out,
        "transfer",
        &[
            col("Block", "block", |t: &_| t.block.to_string()),
            col("TX Hash", "tx_hash", |t| format!("{:?}", t.tx_hash)),
            col("Token", "token", |t| format!("{:?}", t.token)),
            col("From", "from", |t| format!("{:?}", t.from)),
            col("To", "to", |t| format!("{:?}", t.to)),
            col("Value", "value", |t| t.value.to_string()),
        ],
    );
}

fn print_flash_loans(
    loans: &[mev_scout_core::chain::events::FlashLoanEvent],
    args: &ScanArgs,
    out: &str,
) {
    print_events(
        loans,
        args,
        out,
        "flash loan",
        &[
            col("Block", "block", |t: &_| t.block.to_string()),
            col("TX Hash", "tx_hash", |t| format!("{:?}", t.tx_hash)),
            col("Protocol", "protocol", |t| t.protocol.clone()),
            col("Token", "token", |t| format!("{:?}", t.token)),
            col("Amount", "amount", |t| t.amount.to_string()),
            col("Fee", "fee", |t| {
                t.fee.map(|f| f.to_string()).unwrap_or_else(|| "-".into())
            }),
        ],
    );
}

fn print_liquidations(
    liqs: &[mev_scout_core::chain::events::LiquidationEvent],
    args: &ScanArgs,
    out: &str,
) {
    print_events(
        liqs,
        args,
        out,
        "liquidation",
        &[
            col("Block", "block", |t: &_| t.block.to_string()),
            col("TX Hash", "tx_hash", |t| format!("{:?}", t.tx_hash)),
            col("Protocol", "protocol", |t| t.protocol.clone()),
            col("User", "user", |t| format!("{:?}", t.user)),
            col("Liquidator", "liquidator", |t| {
                format!("{:?}", t.liquidator)
            }),
            col("Collateral", "collateral", |t| {
                format!("{:?}", t.collateral_asset)
            }),
            col("Debt", "debt_amount", |t| t.debt_to_cover.to_string()),
            col("Collateral Amount", "collateral_amount", |t| {
                t.collateral_amount.to_string()
            }),
        ],
    );
}
