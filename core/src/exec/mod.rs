//! Virtual MEV execution: flash-arb calldata, revm, Anvil fork.
//!
//! Opt-in from `live` via `--exec`. The default stays `none`, which keeps the
//! paper ledger on detector claims (`--sim assumed`) or state-sim.

mod anvil;
mod artifacts;
mod calldata;
mod plan;
mod revm_backend;
mod session;

pub use anvil::{anvil_args, AnvilNode};
pub use artifacts::{creation_bytecode, deployed_bytecode, ABI_VERSION};
pub use calldata::{encode_execute, EXECUTE_SIG};
pub use plan::{plan_from_opportunity, ExecLeg, ExecPlan, PlanError};
pub use session::{execute_ledger, ExecContext};

use std::str::FromStr;

use alloy::primitives::{address, Address};
use alloy::signers::local::PrivateKeySigner;

/// Anvil dev account #0. Used only as the virtual searcher on revm / local fork.
pub const ANVIL_DEFAULT_KEY: &str =
    "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
pub const ANVIL_DEFAULT_ADDRESS: Address = address!("0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266");

/// How `live` turns a detected opportunity into a fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecMode {
    #[default]
    None,
    Revm,
    Anvil,
}

impl ExecMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Revm => "revm",
            Self::Anvil => "anvil",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "none" | "off" => Some(Self::None),
            "revm" => Some(Self::Revm),
            "anvil" => Some(Self::Anvil),
            _ => None,
        }
    }

    pub fn is_off(self) -> bool {
        matches!(self, Self::None)
    }
}

impl std::fmt::Display for ExecMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Searcher EOA. Empty key → Anvil account #0 (local only; never a funded mainnet key).
pub fn searcher_address(key: Option<&str>) -> anyhow::Result<Address> {
    let Some(raw) = key.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(ANVIL_DEFAULT_ADDRESS);
    };
    let signer = PrivateKeySigner::from_str(raw)
        .map_err(|e| anyhow::anyhow!("invalid paper.exec_key: {e}"))?;
    Ok(signer.address())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rejects_testnet() {
        assert!(ExecMode::parse("testnet").is_none());
        assert_eq!(ExecMode::parse("revm"), Some(ExecMode::Revm));
        assert_eq!(ExecMode::parse("anvil"), Some(ExecMode::Anvil));
        assert_eq!(ExecMode::parse("none"), Some(ExecMode::None));
    }
}
