use crate::cli::{Cli, Command};
use mev_scout_core::config::CliOverrides;

pub fn build_overrides(cli: &Cli) -> CliOverrides {
    let mut o = CliOverrides::default();
    match &cli.command {
        Command::Run(args) => {
            o.days = args.block_range.days;
            o.blocks = args.block_range.blocks;
            o.block = args.block_range.block;
            o.from_block = args.block_range.from_block;
            o.to_block = args.block_range.to_block;
            o.chain = Some(args.chain_args.chain.clone());
            o.rpc_url = args.chain_args.rpc_url.clone();
            o.rpc_urls = args.chain_args.rpc_urls.clone();
            o.rpc_rps = args.chain_args.rpc_rps.clone();
            o.block_concurrency = args.block_concurrency;
            o.rps_limit = Some(args.chain_args.rps_limit);
            o.flash_loan_provider = Some(args.flash_loan_provider.clone());
            o.strategies = Some(args.strategies.clone());
            o.gas_model = Some(args.gas_model.clone());
            o.gas_limit = Some(args.gas_limit);
            o.priority_fee_gwei = Some(args.priority_fee);
            o.output = Some(args.output.clone());
            o.export_path = Some(args.export_path.clone());
            o.db_path = args.db_path.clone();
            o.parquet_dir = args.parquet_dir.clone();
            o.price_oracle_mode = Some(args.price_oracle_mode.clone());
            o.token_prices = args.token_prices.clone();
            o.proximity_window = Some(args.proximity_window);
            o.capture_pending = Some(args.capture_pending);
            o.cross_block_window = Some(args.cross_block_window);
        }
        Command::Fetch(args) => {
            o.days = args.block_range.days;
            o.blocks = args.block_range.blocks;
            o.block = args.block_range.block;
            o.from_block = args.block_range.from_block;
            o.to_block = args.block_range.to_block;
            o.chain = Some(args.chain_args.chain.clone());
            o.rpc_url = args.chain_args.rpc_url.clone();
            o.rpc_urls = args.chain_args.rpc_urls.clone();
            o.rpc_rps = args.chain_args.rpc_rps.clone();
            o.block_concurrency = args.block_concurrency;
            o.rps_limit = Some(args.chain_args.rps_limit);
            o.db_path = args.db_path.clone();
            o.parquet_dir = args.parquet_dir.clone();
        }
        Command::Replay(args) => {
            o.block = Some(args.block);
            o.chain = Some(args.chain_args.chain.clone());
            o.rpc_url = args.chain_args.rpc_url.clone();
            o.rpc_urls = args.chain_args.rpc_urls.clone();
            o.rpc_rps = args.chain_args.rpc_rps.clone();
            o.rps_limit = Some(args.chain_args.rps_limit);
            o.db_path = args.db_path.clone();
            o.parquet_dir = args.parquet_dir.clone();
        }
        Command::Report(_) => {}
        Command::Config => {}
        Command::Discover(args) => {
            o.days = args.block_range.days;
            o.blocks = args.block_range.blocks;
            o.block = args.block_range.block;
            o.from_block = args.block_range.from_block;
            o.to_block = args.block_range.to_block;
            o.chain = Some(args.chain_args.chain.clone());
            o.rpc_url = args.chain_args.rpc_url.clone();
            o.rpc_urls = args.chain_args.rpc_urls.clone();
            o.rpc_rps = args.chain_args.rpc_rps.clone();
            o.rps_limit = Some(args.chain_args.rps_limit);
            o.db_path = args.db_path.clone();
        }
        Command::Live(args) => {
            o.chain = Some(args.chain_args.chain.clone());
            o.rpc_url = args.chain_args.rpc_url.clone();
            o.rpc_urls = args.chain_args.rpc_urls.clone();
            o.rpc_rps = args.chain_args.rpc_rps.clone();
            o.rps_limit = Some(args.chain_args.rps_limit);
            o.strategies = Some(args.strategies.clone());
            o.gas_model = Some(args.gas_model.clone());
            o.gas_limit = Some(args.gas_limit);
            o.priority_fee_gwei = Some(args.priority_fee);
            o.output = Some("json".to_string());
            o.export_path = Some(args.export_path.clone());
            o.db_path = args.db_path.clone();
            o.price_oracle_mode = Some(args.price_oracle_mode.clone());
            o.token_prices = args.token_prices.clone();
            o.initial_balance = Some(args.initial_balance);
            o.min_profit_threshold = Some(args.min_profit);
            o.poll_interval_ms = Some(args.poll_interval);
            o.max_executions = args.max_executions;
        }
        Command::Audit(args) => {
            o.chain = Some(args.chain_args.chain.clone());
            o.rpc_url = args.chain_args.rpc_url.clone();
            o.rpc_urls = args.chain_args.rpc_urls.clone();
            o.rpc_rps = args.chain_args.rpc_rps.clone();
            o.rps_limit = Some(args.chain_args.rps_limit);
            o.from_block = Some(args.from_block);
            o.to_block = Some(args.to_block);
        }
        Command::DuneCheck(args) => {
            o.chain = Some(args.chain.clone());
            o.dune_api_key = args.dune_api_key.clone();
        }
    }
    o
}
