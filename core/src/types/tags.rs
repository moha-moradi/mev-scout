//! ArbAtomic attribution tags shared by the explorer classifier and the live
//! detectors. String values match `details.tags` in the explorer store.
//!
//! Chain-gated N/A (explicit, never silent drops):
//! * `fot_arb` — the static registry (`data/fot_tokens.json`) is Ethereum-only;
//!   Avalanche has no entries, so on AVAX the tag only ever comes from
//!   dynamically measured taxes.
//! * `rebase_arb` — only the static registry can produce it; Avalanche has no
//!   rebase entries.
//! * `forwarded_profit` — realized-only explorer concept, intentionally absent
//!   from live detection (spec out-of-scope).

pub const BACKRUN: &str = "backrun";
pub const FLASH_ARB: &str = "flash_arb";
pub const LONG_TAIL: &str = "long_tail";
pub const CURVE_IMBALANCE: &str = "curve_imbalance";
pub const RATE_PROVIDER_STALENESS: &str = "rate_provider_staleness";
pub const FOT_ARB: &str = "fot_arb";
pub const REBASE_ARB: &str = "rebase_arb";
pub const SAVAX_RATE_ARB: &str = "savax_rate_arb";
pub const EPOCH_TRANSITION: &str = "epoch_transition";
pub const GMX_ADL_ARB: &str = "gmx_adl_arb";

/// Every tag live can attach to an arb opportunity.
pub const LIVE_ARB_TAGS: &[&str] = &[
    BACKRUN,
    FLASH_ARB,
    LONG_TAIL,
    CURVE_IMBALANCE,
    RATE_PROVIDER_STALENESS,
    FOT_ARB,
    REBASE_ARB,
    SAVAX_RATE_ARB,
    EPOCH_TRANSITION,
    GMX_ADL_ARB,
];
