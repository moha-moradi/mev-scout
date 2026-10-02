//! Configuration validation — parses and normalizes runtime parameters, returning a `ValidationResult`.

use crate::config::defaults::ChainConfig;
use crate::config::settings::Config;
use crate::error::ConfigError;
use crate::types::{ChainName, FlashLoanProvider, GasModel, RangeMode, Strategy};

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
    let chain_name = config.chain;

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

fn validate_explorer_tolerances(config: &Config) -> std::result::Result<(), ConfigError> {
    for (field, value) in [
        (
            "explorer.trace_tolerance_pct",
            config.explorer.trace_tolerance_pct,
        ),
        (
            "explorer.trace_error_usd_tol",
            config.explorer.trace_error_usd_tol,
        ),
        // MEV gate (detector expected vs realized net profit).
        (
            "explorer.mev_tolerance_pct",
            config.explorer.mev_tolerance_pct,
        ),
        (
            "explorer.mev_error_usd_tol",
            config.explorer.mev_error_usd_tol,
        ),
    ] {
        if !value.is_finite() || value < 0.0 {
            return Err(ConfigError::InvalidValue {
                field: field.into(),
                message: "must be finite and non-negative".into(),
            });
        }
    }
    Ok(())
}

/// Validate the gas/bidding knobs that feed `GasConfig`. `winning_bid_premium`
/// multiplies the priority fee, so a negative or NaN value would silently
/// deflate (or NaN-poison) every per-op gas estimate instead of erroring.
fn validate_gas_premium(config: &Config) -> std::result::Result<(), ConfigError> {
    let premium = config.gas.winning_bid_premium;
    if !premium.is_finite() || premium < 0.0 {
        return Err(ConfigError::InvalidValue {
            field: "gas.winning_bid_premium".into(),
            message: "must be finite and non-negative".into(),
        });
    }
    Ok(())
}

/// Validate every per-chain `[chains.<name>.rpc]` override present in the
/// config. Built-in defaults carry no override, so this only fires for URLs
/// the user configured — catching a typo'd per-chain endpoint at load/validate
/// time instead of at runtime.
pub fn validate_chain_rpc_overrides(config: &Config) -> std::result::Result<(), ConfigError> {
    for (name, chain_cfg) in &config.chains {
        if let Some(rpc) = &chain_cfg.rpc {
            if let Some(url) = &rpc.rpc_url {
                validate_rpc_url(url).map_err(|e| {
                    ConfigError::Validation(format!("[chains.{name}.rpc] rpc_url: {}", e))
                })?;
            }
            validate_rpc_urls(&rpc.rpc_urls)
                .map_err(|e| ConfigError::Validation(format!("[chains.{name}.rpc] {}", e)))?;
        }
    }
    Ok(())
}

/// A mutually-exclusive block-range selection.
///
/// `BlockRangeArgs` (and the config-file equivalent) originally exposed five
/// separate `Option` fields whose "exactly one required" invariant could not
/// be expressed at the type level — every consumer passed all five to
/// `resolve_block_range` and a forgot-one bug compiled fine. Constructing a
/// `RangeSpec` owns that invariant: exactly one variant is produced per call,
/// so downstream code has nothing left to get wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeSpec {
    Days(u64),
    Blocks(u64),
    Block(u64),
    FromTo(u64, u64),
}

impl RangeSpec {
    /// Build from the individual range flags, enforcing the "exactly one
    /// required" and coherence invariants.
    pub fn from_flags(
        days: Option<u64>,
        blocks: Option<u64>,
        block: Option<u64>,
        from_block: Option<u64>,
        to_block: Option<u64>,
    ) -> std::result::Result<Self, ConfigError> {
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

        if (from_block.is_some() && to_block.is_none())
            || (from_block.is_none() && to_block.is_some())
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
            return Ok(Self::FromTo(f, t));
        }

        if let Some(d) = days {
            if !(1..=365).contains(&d) {
                return Err(ConfigError::Validation(
                    "--days must be between 1 and 365.".to_string(),
                ));
            }
            return Ok(Self::Days(d));
        }

        if let Some(b) = blocks {
            if b < 1 {
                return Err(ConfigError::Validation(
                    "--blocks must be >= 1.".to_string(),
                ));
            }
            return Ok(Self::Blocks(b));
        }

        if let Some(b) = block {
            if b == 0 {
                return Err(ConfigError::Validation("--block must be > 0.".to_string()));
            }
            return Ok(Self::Block(b));
        }

        Err(ConfigError::Validation(
            "no block range specified.\n\
             Use one of: --days, --blocks, --block, or --from-block + --to-block."
                .to_string(),
        ))
    }

    /// Convert into the runtime range mode. Infallible once constructed.
    pub fn resolve(&self) -> RangeMode {
        match *self {
            Self::Days(d) => RangeMode::Days(d),
            Self::Blocks(b) => RangeMode::Blocks(b),
            Self::Block(b) => RangeMode::Single(b),
            Self::FromTo(f, t) => RangeMode::Range(f, t),
        }
    }
}

/// Resolve a `RangeMode` from individual block range CLI arguments.
/// Reusable across subcommands that accept `BlockRangeArgs`.
/// Prefer constructing a [`RangeSpec`] once and calling [`RangeSpec::resolve`].
pub fn resolve_block_range(
    days: Option<u64>,
    blocks: Option<u64>,
    block: Option<u64>,
    from_block: Option<u64>,
    to_block: Option<u64>,
) -> std::result::Result<RangeMode, ConfigError> {
    Ok(RangeSpec::from_flags(days, blocks, block, from_block, to_block)?.resolve())
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

    validate_chain_rpc_overrides(config)?;
    let rpc = config.effective_rpc(chain_name);
    if let Some(url) = &rpc.rpc_url {
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
    validate_common(config, check_strategies, true)
}

/// Validate config for the `live` subcommand.
///
/// Like [`validate_and_resolve`] but skips the block-range check —
/// live mode auto-detects the chain tip at runtime.
pub fn validate_live(config: &Config) -> std::result::Result<ValidationResult, ConfigError> {
    validate_common(config, true, false)
}

/// Shared body of every validating entry point.
///
/// `check_range_conflicts` is the *only* check that differs between
/// [`validate_and_resolve_for`] and [`validate_live`]: `run` resolves a block
/// range from flags, while live detects the tip at runtime and must therefore
/// ignore range flags. Every other check is a plain config invariant and
/// applies to both — silently skipping them in live meant an invalid config
/// passed validation there and failed only later, at replay time.
fn validate_common(
    config: &Config,
    check_strategies: bool,
    check_range: bool,
) -> std::result::Result<ValidationResult, ConfigError> {
    let (chain_name, chain_config) = resolve_chain(config)?;
    validate_explorer_tolerances(config)?;
    validate_gas_premium(config)?;

    let provider: FlashLoanProvider = config.backtest.flash_loan_provider;

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
        config.backtest.strategies.clone()
    } else {
        Vec::new()
    };

    let range_mode = if check_range {
        check_range_conflicts(config)?
    } else {
        RangeMode::Single(0)
    };

    validate_chain_rpc_overrides(config)?;
    let rpc = config.effective_rpc(chain_name);
    if let Some(url) = &rpc.rpc_url {
        validate_rpc_url(url)?;
    }
    if !rpc.rpc_urls.is_empty() {
        validate_rpc_urls(&rpc.rpc_urls)?;
    }

    let gas_model = config.gas.gas_model;

    if !(21_000..=30_000_000).contains(&config.gas.gas_limit) {
        return Err(ConfigError::InvalidValue {
            field: "gas_limit".into(),
            message: format!(
                "must be between 21,000 and 30,000,000, got {}",
                config.gas.gas_limit
            ),
        });
    }

    if rpc.rps_limit > 10_000.0 {
        return Err(ConfigError::InvalidValue {
            field: "rps_limit".into(),
            message: format!("must be between 0 and 10,000, got {}", config.rpc.rps_limit),
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

/// Address fields on [`ChainConfig`] are typed as [`alloy::primitives::Address`],
/// so invalid hex fails at TOML deserialize. Kept as a no-op hook so call sites
/// that historically validated here continue to compile without a silent drop.
pub fn validate_chain_config_addresses(
    _chain_config: &ChainConfig,
) -> std::result::Result<(), ConfigError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_finite_or_negative_tolerances_and_premium() {
        use crate::error::ConfigError;

        type Case = (&'static str, fn(&mut Config));
        let cases: &[Case] = &[
            ("explorer.trace_tolerance_pct", |c| {
                c.explorer.trace_tolerance_pct = -1.0
            }),
            ("explorer.trace_error_usd_tol", |c| {
                c.explorer.trace_error_usd_tol = f64::NAN
            }),
            ("explorer.mev_tolerance_pct", |c| {
                c.explorer.mev_tolerance_pct = -1.0
            }),
            ("explorer.mev_error_usd_tol", |c| {
                c.explorer.mev_error_usd_tol = f64::INFINITY
            }),
            ("gas.winning_bid_premium", |c| {
                c.gas.winning_bid_premium = -0.01
            }),
            ("gas.winning_bid_premium", |c| {
                c.gas.winning_bid_premium = f64::NAN
            }),
        ];
        for (field, set) in cases {
            let mut config = Config::default();
            set(&mut config);
            match validate_live(&config) {
                Err(ConfigError::InvalidValue {
                    field: got,
                    message,
                }) => {
                    assert_eq!(got, *field);
                    assert_eq!(message, "must be finite and non-negative");
                }
                other => panic!("expected InvalidValue for {field}, got {other:?}"),
            }
        }
    }

    #[test]
    fn accepts_winning_bid_premium_zero_and_positive() {
        let mut config = Config::default();
        assert_eq!(config.gas.winning_bid_premium, 0.0, "default is off");
        assert!(validate_live(&config).is_ok());
        config.gas.winning_bid_premium = 0.25;
        assert!(validate_live(&config).is_ok());
    }

    /// Live shares every config invariant with `run` except the block-range
    /// check, which it cannot honour. Regression guard: these three used to be
    /// silently skipped by `validate_live` and must not drift back.
    #[test]
    fn live_enforces_shared_config_invariants() {
        let cases = [
            (
                "gas_limit",
                (|c: &mut Config| c.gas.gas_limit = 30_000_001) as fn(&mut Config),
            ),
            ("rps_limit", |c: &mut Config| c.rpc.rps_limit = 10_001.0),
        ];
        for (field, set) in cases {
            let mut config = Config::default();
            set(&mut config);
            match validate_live(&config) {
                Err(ConfigError::InvalidValue { field: got, .. }) => {
                    assert_eq!(got, field);
                }
                other => panic!("expected InvalidValue for {field}, got {other:?}"),
            }
        }
    }

    #[test]
    fn live_ignores_block_range_flags() {
        // Conflicting range flags: rejected by `run`, irrelevant to live, which
        // detects the tip at runtime and must not care what the flags say.
        let config = Config {
            days: Some(3),
            blocks: Some(100),
            ..Config::default()
        };
        assert!(validate_live(&config).is_ok());
        assert!(validate_and_resolve(&config).is_err());
    }

    // `run` was removed from the CLI in favor of `live`, and `discover`
    // reaches `init_rpc` before it resolves a range — so the runtime range
    // branches are no longer reachable from an offline CLI test. They are
    // locked here instead, where they stay hermetic. `discover` still flattens
    // `BlockRangeArgs`, so these are exactly the combinations a user can type.

    fn spec(
        days: Option<u64>,
        blocks: Option<u64>,
        block: Option<u64>,
        from: Option<u64>,
        to: Option<u64>,
    ) -> std::result::Result<RangeSpec, ConfigError> {
        RangeSpec::from_flags(days, blocks, block, from, to)
    }

    fn msg(r: std::result::Result<RangeSpec, ConfigError>) -> String {
        match r {
            Ok(_) => panic!("expected a range validation error"),
            Err(e) => e.to_string(),
        }
    }

    #[test]
    fn no_range_flags_is_rejected() {
        assert!(msg(spec(None, None, None, None, None)).contains("no block range specified"));
    }

    #[test]
    fn from_block_requires_to_block() {
        assert!(msg(spec(None, None, None, Some(100), None)).contains("must be used together"));
        assert!(msg(spec(None, None, None, None, Some(100))).contains("must be used together"));
    }

    #[test]
    fn to_block_must_exceed_from_block() {
        assert!(msg(spec(None, None, None, Some(200), Some(200))).contains("must be greater than"));
        assert!(msg(spec(None, None, None, Some(200), Some(100))).contains("must be greater than"));
    }

    #[test]
    fn two_range_forms_conflict() {
        let e = msg(spec(Some(2), Some(5), None, None, None));
        assert!(e.contains("cannot be used together"), "msg was: {e}");
        // The message must name both offenders so the fix is obvious.
        assert!(
            e.contains("--days") && e.contains("--blocks"),
            "msg was: {e}"
        );

        let e = msg(spec(None, None, Some(7), Some(1), Some(9)));
        assert!(e.contains("cannot be used together"), "msg was: {e}");
        assert!(
            e.contains("--block") && e.contains("--from-block"),
            "msg was: {e}"
        );
    }

    #[test]
    fn block_zero_rejected_at_library_level() {
        // Unreachable from the CLI (clap's value_parser rejects 0 first), but
        // core is a public library, so the invariant is enforced here too.
        assert!(msg(spec(None, None, Some(0), None, None)).contains("--block must be > 0"));
    }

    #[test]
    fn each_single_range_form_is_accepted() {
        assert!(matches!(
            spec(Some(7), None, None, None, None).unwrap(),
            RangeSpec::Days(7)
        ));
        assert!(matches!(
            spec(None, Some(100), None, None, None).unwrap(),
            RangeSpec::Blocks(100)
        ));
        assert!(matches!(
            spec(None, None, Some(65_000_000), None, None).unwrap(),
            RangeSpec::Block(65_000_000)
        ));
        assert!(matches!(
            spec(None, None, None, Some(1), Some(9)).unwrap(),
            RangeSpec::FromTo(1, 9)
        ));
    }

    /// `Config.days/blocks/block/from_block/to_block` are `#[serde(skip)]`, so
    /// the TOML file cannot set them; only CLI flags or library calls can.
    #[test]
    fn config_file_range_fields_are_ignored() {
        // `Config.days/blocks/block/from_block/to_block` are `#[serde(skip)]`,
        // so TOML that sets them is accepted and then ignored — which is why
        // the old CLI test saw "no block range specified" rather than the
        // written values. Locked here now that `run` is gone from the CLI.
        let toml_src = r#"
output = "json"
days = 0
blocks = 0
block = 0
from_block = 500
to_block = 100
"#;
        let parsed: Config = toml::from_str(toml_src).unwrap();
        assert!(
            parsed.days.is_none(),
            "days leaked from TOML: {:?}",
            parsed.days
        );
        assert!(
            parsed.blocks.is_none(),
            "blocks leaked: {:?}",
            parsed.blocks
        );
        assert!(parsed.block.is_none(), "block leaked: {:?}", parsed.block);
        assert!(
            parsed.from_block.is_none(),
            "from_block leaked: {:?}",
            parsed.from_block
        );
        assert!(
            parsed.to_block.is_none(),
            "to_block leaked: {:?}",
            parsed.to_block
        );
        // And with no range visible, resolving one fails the same way the CLI
        // used to report.
        let e = msg(spec(
            parsed.days,
            parsed.blocks,
            parsed.block,
            parsed.from_block,
            parsed.to_block,
        ));
        assert!(e.contains("no block range specified"), "msg was: {e}");
    }
}
