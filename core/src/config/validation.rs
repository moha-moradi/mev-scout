//! Configuration validation — parses and normalizes runtime parameters, returning a `ValidationResult`.

use crate::config::defaults::ChainConfig;
use crate::config::settings::Config;
use crate::error::ConfigError;
use crate::types::{
    ChainName, FlashLoanProvider, GasModel, OutputFormat, RangeMode, Strategy,
};

/// Resolved configuration returned by successful validation.
///
/// Contains the parsed and normalized runtime parameters (chain, range, strategies,
/// flash loan provider, gas model) that the backtest engine consumes.
#[derive(Debug)]
pub struct ValidationResult {
    pub chain_name: ChainName,
    pub chain_config: ChainConfig,
    pub range_mode: RangeMode,
    pub strategies: Vec<Strategy>,
    pub flash_loan_provider: FlashLoanProvider,
    pub gas_model: GasModel,
}

pub fn resolve_chain(config: &Config) -> std::result::Result<(ChainName, ChainConfig), ConfigError> {
    let chain_name: ChainName = config
        .chain
        .parse()
        .map_err(|e: String| ConfigError::Validation(format!("Error: {e}")))?;

    let chain_config = config
        .chains
        .get(chain_name.to_string().as_str())
        .cloned()
        .ok_or_else(|| {
            ConfigError::Validation(format!(
                "Error: no [chains.{}] section found in config.",
                chain_name
            ))
        })?;

    Ok((chain_name, chain_config))
}

pub fn validate_rpc_url(url: &str) -> std::result::Result<(), ConfigError> {
    if url.trim().is_empty() {
        return Err(ConfigError::Validation(
            "Error: RPC URL cannot be empty.".to_string(),
        ));
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(ConfigError::Validation(format!(
            "Error: RPC URL '{}' must start with http:// or https://.",
            url
        )));
    }
    Ok(())
}

/// Validate all RPC URLs in a list. Returns error on the first invalid URL.
pub fn validate_rpc_urls(urls: &[String]) -> std::result::Result<(), ConfigError> {
    for url in urls {
        validate_rpc_url(url)?;
    }
    Ok(())
}

fn count_set_flags(cfg: &Config) -> Vec<&'static str> {
    let mut flags = Vec::new();
    if cfg.days.is_some() {
        flags.push("--days");
    }
    if cfg.blocks.is_some() {
        flags.push("--blocks");
    }
    if cfg.block.is_some() {
        flags.push("--block");
    }
    if cfg.from_block.is_some() || cfg.to_block.is_some() {
        flags.push("--from-block/--to-block");
    }
    flags
}

/// Resolve a `RangeMode` from individual block range CLI arguments.
/// Reusable across subcommands that accept `BlockRangeArgs`.
pub fn resolve_block_range(
    days: Option<u64>,
    blocks: Option<u64>,
    block: Option<u64>,
    from_block: Option<u64>,
    to_block: Option<u64>,
) -> std::result::Result<RangeMode, ConfigError> {
    let mut flags = Vec::new();
    if days.is_some() { flags.push("--days"); }
    if blocks.is_some() { flags.push("--blocks"); }
    if block.is_some() { flags.push("--block"); }
    if from_block.is_some() || to_block.is_some() { flags.push("--from-block/--to-block"); }

    if flags.len() > 1 {
        return Err(ConfigError::Validation(format!(
            "Error: {} cannot be used together.\n\
             Use exactly one of: --days, --blocks, --block, or --from-block/--to-block.",
            flags.join(" and ")
        )));
    }

    if (from_block.is_some() && to_block.is_none()) || (from_block.is_none() && to_block.is_some()) {
        return Err(ConfigError::Validation(
            "Error: --from-block and --to-block must be used together.".to_string(),
        ));
    }

    if let (Some(f), Some(t)) = (from_block, to_block) {
        if t <= f {
            return Err(ConfigError::Validation(format!(
                "Error: --to-block ({t}) must be greater than --from-block ({f})."
            )));
        }
        return Ok(RangeMode::Range(f, t));
    }

    if let Some(d) = days {
        if !(1..=365).contains(&d) {
            return Err(ConfigError::Validation(
                "Error: --days must be between 1 and 365.".to_string(),
            ));
        }
        return Ok(RangeMode::Days(d));
    }

    if let Some(b) = blocks {
        if b < 1 {
            return Err(ConfigError::Validation(
                "Error: --blocks must be >= 1.".to_string(),
            ));
        }
        return Ok(RangeMode::Blocks(b));
    }

    if let Some(b) = block {
        if b == 0 {
            return Err(ConfigError::Validation(
                "Error: --block must be > 0.".to_string(),
            ));
        }
        return Ok(RangeMode::Single(b));
    }

    Err(ConfigError::Validation(
        "Error: no block range specified.\n\
         Use one of: --days, --blocks, --block, or --from-block + --to-block."
            .to_string(),
    ))
}

fn check_range_conflicts(cfg: &Config) -> std::result::Result<RangeMode, ConfigError> {
    resolve_block_range(cfg.days, cfg.blocks, cfg.block, cfg.from_block, cfg.to_block)
}

/// Validates config for the replay subcommand.
/// Only allows --block (single block), rejects all other range flags.
pub fn validate_replay(config: &Config) -> std::result::Result<(ChainName, ChainConfig), ConfigError> {
    let (chain_name, chain_config) = resolve_chain(config)?;

    let active = count_set_flags(config);
    if active.len() > 1 {
        return Err(ConfigError::Validation(format!(
            "Error: {} cannot be used together.\n\
             Use exactly one of: --days, --blocks, --block, or --from-block/--to-block.",
            active.join(" and ")
        )));
    }

    let from = config.from_block;
    let to = config.to_block;
    if (from.is_some() && to.is_none()) || (from.is_none() && to.is_some()) {
        return Err(ConfigError::Validation(
            "Error: --from-block and --to-block must be used together.".to_string(),
        ));
    }

    if config.days.is_some() {
        return Err(ConfigError::Validation(
            "Error: --days is not supported by the replay subcommand. Use --block instead.".to_string(),
        ));
    }
    if config.blocks.is_some() {
        return Err(ConfigError::Validation(
            "Error: --blocks is not supported by the replay subcommand. Use --block instead.".to_string(),
        ));
    }
    if config.from_block.is_some() || config.to_block.is_some() {
        return Err(ConfigError::Validation(
            "Error: --from-block/--to-block is not supported by the replay subcommand. Use --block instead.".to_string(),
        ));
    }
    if config.block.is_none() || config.block == Some(0) {
        return Err(ConfigError::Validation(
            "Error: --block is required for the replay subcommand and must be > 0.".to_string(),
        ));
    }

    if let Some(url) = &config.rpc_url {
        validate_rpc_url(url)?;
    }

    Ok((chain_name, chain_config))
}

pub fn validate_and_resolve(config: &Config) -> std::result::Result<ValidationResult, ConfigError> {
    validate_and_resolve_for(config, true)
}

pub fn validate_and_resolve_for(config: &Config, check_strategies: bool) -> std::result::Result<ValidationResult, ConfigError> {
    let (chain_name, chain_config) = resolve_chain(config)?;

    let provider: FlashLoanProvider = config.flash_loan_provider.parse().map_err(|e: String| {
        ConfigError::Validation(format!("Error: {e}"))
    })?;

    if provider.is_forced() {
        let contract_field = match provider {
            FlashLoanProvider::Balancer => "balancer_vault",
            FlashLoanProvider::Aave => "aave_v3_pool",
            FlashLoanProvider::Uniswap => "uniswap_v3_factories",
            _ => unreachable!(),
        };
        let has_contract = match provider {
            FlashLoanProvider::Balancer => chain_config.balancer_vault.is_some(),
            FlashLoanProvider::Aave => chain_config.aave_v3_pool.is_some(),
            FlashLoanProvider::Uniswap => chain_config.uniswap_v3_factories.as_ref().is_some_and(|f| !f.is_empty()),
            _ => true,
        };
        if !has_contract {
            tracing::warn!(
                "{} contract address is missing for chain '{}'. \
                 Opportunities requiring this provider will be SKIPPED_NO_FLASHLOAN.",
                contract_field,
                chain_name
            );
        }
    }

    let strategies: Vec<Strategy> = if check_strategies {
        let s = Strategy::from_comma_list(&config.strategies)
            .map_err(|e| ConfigError::Validation(format!("Error: {e}")))?;
        s
    } else {
        Vec::new()
    };

    let range_mode = check_range_conflicts(config)?;

    if let Some(url) = &config.rpc_url {
        validate_rpc_url(url)?;
    }
    if !config.rpc_urls.is_empty() {
        validate_rpc_urls(&config.rpc_urls)?;
    }

    let gas_model: GasModel = config.gas_model.parse().map_err(|e: String| {
        ConfigError::Validation(format!("Error: {e}"))
    })?;

    let _output: OutputFormat = config.output.parse().map_err(|e: String| {
        ConfigError::Validation(format!("Error: {e}"))
    })?;

    Ok(ValidationResult {
        chain_name,
        chain_config,
        range_mode,
        strategies,
        flash_loan_provider: provider,
        gas_model,
    })
}


