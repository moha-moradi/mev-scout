//! Configuration validation — parses and normalizes runtime parameters, returning a `ValidationResult`.

use crate::config::defaults::ChainConfig;
use crate::config::settings::Config;
use crate::error::ConfigError;
use crate::types::{ChainName, FlashLoanProvider, GasModel, OutputFormat, RangeMode, Strategy};

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

pub fn resolve_chain(
    config: &Config,
) -> std::result::Result<(ChainName, ChainConfig), ConfigError> {
    let chain_name: ChainName = config
        .chain
        .parse()
        .map_err(|e| ConfigError::Validation(format!("{e}")))?;

    let chain_config = config
        .chains
        .get(chain_name.to_string().as_str())
        .cloned()
        .ok_or_else(|| {
            ConfigError::Validation(format!(
                "no [chains.{}] section found in config.",
                chain_name
            ))
        })?;

    Ok((chain_name, chain_config))
}

pub fn validate_rpc_url(url: &str) -> std::result::Result<(), ConfigError> {
    if url.trim().is_empty() {
        return Err(ConfigError::Validation(
            "RPC URL cannot be empty.".to_string(),
        ));
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(ConfigError::Validation(format!(
            "RPC URL '{}' must start with http:// or https://.",
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
    if days.is_some() {
        flags.push("--days");
    }
    if blocks.is_some() {
        flags.push("--blocks");
    }
    if block.is_some() {
        flags.push("--block");
    }
    if from_block.is_some() || to_block.is_some() {
        flags.push("--from-block/--to-block");
    }

    if flags.len() > 1 {
        return Err(ConfigError::Validation(format!(
            "{} cannot be used together.\n\
             Use exactly one of: --days, --blocks, --block, or --from-block/--to-block.",
            flags.join(" and ")
        )));
    }

    if (from_block.is_some() && to_block.is_none()) || (from_block.is_none() && to_block.is_some())
    {
        return Err(ConfigError::Validation(
            "--from-block and --to-block must be used together.".to_string(),
        ));
    }

    if let (Some(f), Some(t)) = (from_block, to_block) {
        if t <= f {
            return Err(ConfigError::Validation(format!(
                "--to-block ({t}) must be greater than --from-block ({f})."
            )));
        }
        return Ok(RangeMode::Range(f, t));
    }

    if let Some(d) = days {
        if !(1..=365).contains(&d) {
            return Err(ConfigError::Validation(
                "--days must be between 1 and 365.".to_string(),
            ));
        }
        return Ok(RangeMode::Days(d));
    }

    if let Some(b) = blocks {
        if b < 1 {
            return Err(ConfigError::Validation(
                "--blocks must be >= 1.".to_string(),
            ));
        }
        return Ok(RangeMode::Blocks(b));
    }

    if let Some(b) = block {
        if b == 0 {
            return Err(ConfigError::Validation("--block must be > 0.".to_string()));
        }
        return Ok(RangeMode::Single(b));
    }

    Err(ConfigError::Validation(
        "no block range specified.\n\
         Use one of: --days, --blocks, --block, or --from-block + --to-block."
            .to_string(),
    ))
}

fn check_range_conflicts(cfg: &Config) -> std::result::Result<RangeMode, ConfigError> {
    resolve_block_range(
        cfg.days,
        cfg.blocks,
        cfg.block,
        cfg.from_block,
        cfg.to_block,
    )
}

/// Validates config for the replay subcommand.
/// Only allows --block (single block), rejects all other range flags.
pub fn validate_replay(
    config: &Config,
) -> std::result::Result<(ChainName, ChainConfig), ConfigError> {
    let (chain_name, chain_config) = resolve_chain(config)?;

    match resolve_block_range(
        config.days,
        config.blocks,
        config.block,
        config.from_block,
        config.to_block,
    ) {
        Ok(RangeMode::Single(b)) if b > 0 => {}
        Ok(RangeMode::Single(_)) => {
            return Err(ConfigError::Validation("--block must be > 0.".to_string()));
        }
        Ok(RangeMode::Days(_)) => {
            return Err(ConfigError::Validation(
                "--days is not supported by the replay subcommand. Use --block instead."
                    .to_string(),
            ));
        }
        Ok(RangeMode::Blocks(_)) => {
            return Err(ConfigError::Validation(
                "--blocks is not supported by the replay subcommand. Use --block instead."
                    .to_string(),
            ));
        }
        Ok(RangeMode::Range(_, _)) => {
            return Err(ConfigError::Validation(
                "--from-block/--to-block is not supported by the replay subcommand. Use --block instead.".to_string(),
            ));
        }
        Err(e) => {
            // If resolve_block_range returned an error (e.g. no flags set), map it to the
            // replay-specific missing --block error.
            let _ = e;
            return Err(ConfigError::Validation(
                "--block is required for the replay subcommand and must be > 0.".to_string(),
            ));
        }
    }

    if let Some(url) = &config.rpc.rpc_url {
        validate_rpc_url(url)?;
    }

    Ok((chain_name, chain_config))
}

pub fn validate_and_resolve(config: &Config) -> std::result::Result<ValidationResult, ConfigError> {
    validate_and_resolve_for(config, true)
}

pub fn validate_and_resolve_for(
    config: &Config,
    check_strategies: bool,
) -> std::result::Result<ValidationResult, ConfigError> {
    let (chain_name, chain_config) = resolve_chain(config)?;

    let provider: FlashLoanProvider = config
        .backtest
        .flash_loan_provider
        .parse()
        .map_err(|e| ConfigError::Validation(format!("{e}")))?;

    // Forced providers need a chain-specific contract address; Auto picks
    // per-opportunity and needs none. The match is exhaustive, so adding a
    // provider variant is a compile error here rather than a runtime gap.
    let forced_contract: Option<(&str, bool)> = match provider {
        FlashLoanProvider::Balancer => {
            Some(("balancer_vault", chain_config.balancer_vault.is_some()))
        }
        FlashLoanProvider::Aave => Some(("aave_v3_pool", chain_config.aave_v3_pool.is_some())),
        FlashLoanProvider::Uniswap => Some((
            "uniswap_v3_factories",
            chain_config
                .uniswap_v3_factories
                .as_ref()
                .is_some_and(|f| !f.is_empty()),
        )),
        FlashLoanProvider::Auto => None,
    };
    if let Some((contract_field, has_contract)) = forced_contract {
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
        Strategy::from_comma_list(&config.backtest.strategies)
            .map_err(|e| ConfigError::Validation(e.to_string()))?
    } else {
        Vec::new()
    };

    let range_mode = check_range_conflicts(config)?;

    if let Some(url) = &config.rpc.rpc_url {
        validate_rpc_url(url)?;
    }
    if !config.rpc.rpc_urls.is_empty() {
        validate_rpc_urls(&config.rpc.rpc_urls)?;
    }

    let gas_model: GasModel = config
        .gas
        .gas_model
        .parse()
        .map_err(|e: String| ConfigError::Validation(e.to_string()))?;

    let _: OutputFormat = config
        .output
        .output
        .parse()
        .map_err(|e| ConfigError::Validation(format!("{e}")))?;

    if !(21_000..=30_000_000).contains(&config.gas.gas_limit) {
        return Err(ConfigError::InvalidValue {
            field: "gas_limit".into(),
            message: format!(
                "must be between 21,000 and 30,000,000, got {}",
                config.gas.gas_limit
            ),
        });
    }

    if config.rpc.rps_limit > 10_000.0 {
        return Err(ConfigError::InvalidValue {
            field: "rps_limit".into(),
            message: format!("must be between 0 and 10,000, got {}", config.rpc.rps_limit),
        });
    }

    if config.backtest.proximity_window > 100 {
        return Err(ConfigError::InvalidValue {
            field: "proximity_window".into(),
            message: format!(
                "must be between 0 and 100, got {}",
                config.backtest.proximity_window
            ),
        });
    }

    Ok(ValidationResult {
        chain_name,
        chain_config,
        range_mode,
        strategies,
        flash_loan_provider: provider,
        gas_model,
    })
}

/// Validate config for the `live` subcommand.
///
/// Like [`validate_and_resolve`] but skips the block-range check —
/// live mode auto-detects the chain tip at runtime.
pub fn validate_live(config: &Config) -> std::result::Result<ValidationResult, ConfigError> {
    let (chain_name, chain_config) = resolve_chain(config)?;

    let provider: FlashLoanProvider = config
        .backtest
        .flash_loan_provider
        .parse()
        .map_err(|e| ConfigError::Validation(format!("{e}")))?;

    let strategies: Vec<Strategy> = Strategy::from_comma_list(&config.backtest.strategies)
        .map_err(|e| ConfigError::Validation(e.to_string()))?;

    if let Some(url) = &config.rpc.rpc_url {
        validate_rpc_url(url)?;
    }
    if !config.rpc.rpc_urls.is_empty() {
        validate_rpc_urls(&config.rpc.rpc_urls)?;
    }

    let gas_model: GasModel = config
        .gas
        .gas_model
        .parse()
        .map_err(|e: String| ConfigError::Validation(e.to_string()))?;

    Ok(ValidationResult {
        chain_name,
        chain_config,
        range_mode: RangeMode::Single(0),
        strategies,
        flash_loan_provider: provider,
        gas_model,
    })
}

/// Validate that every address-bearing field in a `ChainConfig` parses as
/// an [`Address`]. A single typo'd address previously silently dropped a
/// DEX venue (factory lists were parsed with `.ok()`), turning the whole
/// DEX family's discovery off without any signal.
///
/// Returns `Err(ConfigError::InvalidValue)` naming the offending field.
pub fn validate_chain_config_addresses(
    chain_config: &ChainConfig,
) -> std::result::Result<(), ConfigError> {
    use alloy::primitives::Address;

    let mut checked: Vec<(&str, &String)> = Vec::new();
    for (field, v) in [
        ("balancer_vault", &chain_config.balancer_vault),
        ("aave_v3_pool", &chain_config.aave_v3_pool),
        ("curve_registry", &chain_config.curve_registry),
        ("wrapped_native_token", &chain_config.wrapped_native_token),
        ("v4_pool_manager", &chain_config.v4_pool_manager),
        (
            "infinity_cl_pool_manager",
            &chain_config.infinity_cl_pool_manager,
        ),
        ("pendle_factory", &chain_config.pendle_factory),
        ("metric_factory", &chain_config.metric_factory),
        ("fluid_factory", &chain_config.fluid_factory),
    ] {
        if let Some(v) = v {
            checked.push((field, v));
        }
    }
    for (field, list) in [
        ("uniswap_v3_factories", &chain_config.uniswap_v3_factories),
        ("uniswap_v2_factories", &chain_config.uniswap_v2_factories),
        ("solidly_factories", &chain_config.solidly_factories),
        ("camelot_factories", &chain_config.camelot_factories),
        ("curve_factories", &chain_config.curve_factories),
        ("trader_joe_factories", &chain_config.trader_joe_factories),
    ] {
        if let Some(list) = list {
            for addr in list {
                checked.push((field, addr));
            }
        }
    }

    for (field, value) in checked {
        if value.parse::<Address>().is_err() {
            return Err(ConfigError::InvalidValue {
                field: field.to_string(),
                message: format!("'{value}' is not a valid EVM address"),
            });
        }
    }
    Ok(())
}
