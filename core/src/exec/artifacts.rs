//! Executor bytecode produced by `forge build` in `contracts/`.
//!
//! The hex files are the creation and deployed objects from
//! `contracts/out/FlashArbExecutor.sol/FlashArbExecutor.json`. Refresh them
//! after an ABI or bytecode change (`ABI_VERSION` in the contract).
use alloy::primitives::Bytes;

/// Must match `FlashArbExecutor.ABI_VERSION`.
pub const ABI_VERSION: u8 = 1;

const DEPLOYED_HEX: &str = include_str!("artifacts/FlashArbExecutor.deployed.hex");
const CREATION_HEX: &str = include_str!("artifacts/FlashArbExecutor.creation.hex");

pub fn deployed_bytecode() -> anyhow::Result<Bytes> {
    decode_hex(DEPLOYED_HEX)
}

pub fn creation_bytecode() -> anyhow::Result<Bytes> {
    decode_hex(CREATION_HEX)
}

fn decode_hex(raw: &str) -> anyhow::Result<Bytes> {
    let mut compact = String::new();
    for ch in raw.chars() {
        if !ch.is_whitespace() {
            compact.push(ch);
        }
    }
    let compact = compact
        .strip_prefix("0x")
        .or_else(|| compact.strip_prefix("0X"))
        .unwrap_or(compact.as_str());
    if compact.len() % 2 != 0 {
        anyhow::bail!("executor artifact hex has an odd length");
    }
    let bytes = hex::decode(compact).map_err(|e| anyhow::anyhow!("executor artifact hex: {e}"))?;
    Ok(Bytes::from(bytes))
}
