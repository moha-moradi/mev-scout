//! Executor bytecode produced by `forge build` in `contracts/`.
//!
//! ABI v2: deploy from `creation.hex` + constructor args
//! `[operator, withdrawer, weth]`. Never inject `deployed.hex` directly —
//! immutables are zeroed at compile time.
//!
//! Refresh after an ABI or bytecode change:
//! ```bash
//! node -e "const j=require('./contracts/out/FlashArbExecutor.sol/FlashArbExecutor.json'),f=require('fs');f.writeFileSync('core/src/exec/artifacts/FlashArbExecutor.creation.hex',j.bytecode.object.replace(/^0x/,'')+'\n');f.writeFileSync('core/src/exec/artifacts/FlashArbExecutor.deployed.hex',j.deployedBytecode.object.replace(/^0x/,'')+'\n')"
//! ```
use alloy::primitives::{Address, Bytes};

/// Must match `FlashArbExecutor.ABI_VERSION`.
pub const ABI_VERSION: u8 = 2;

const DEPLOYED_HEX: &str = include_str!("artifacts/FlashArbExecutor.deployed.hex");
const CREATION_HEX: &str = include_str!("artifacts/FlashArbExecutor.creation.hex");

pub fn deployed_bytecode() -> anyhow::Result<Bytes> {
    decode_hex(DEPLOYED_HEX)
}

pub fn creation_bytecode() -> anyhow::Result<Bytes> {
    decode_hex(CREATION_HEX)
}

/// Append ABI-encoded address constructor args to creation bytecode.
pub fn append_address_args(code: &Bytes, args: &[Address]) -> Bytes {
    let mut out = Vec::with_capacity(code.len() + args.len() * 32);
    out.extend_from_slice(code);
    for addr in args {
        let mut word = [0u8; 32];
        word[12..].copy_from_slice(addr.as_slice());
        out.extend_from_slice(&word);
    }
    Bytes::from(out)
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
    if !compact.len().is_multiple_of(2) {
        anyhow::bail!("executor artifact hex has an odd length");
    }
    let bytes = hex::decode(compact).map_err(|e| anyhow::anyhow!("executor artifact hex: {e}"))?;
    Ok(Bytes::from(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    #[test]
    fn embedded_bytecode_is_a_contract() {
        let deployed = match deployed_bytecode() {
            Ok(b) => b,
            Err(e) => panic!("deployed artifact: {e}"),
        };
        let creation = match creation_bytecode() {
            Ok(b) => b,
            Err(e) => panic!("creation artifact: {e}"),
        };
        assert!(deployed.len() > 64, "deployed bytecode missing");
        assert!(creation.len() > deployed.len(), "creation bytecode missing");
        assert_eq!(ABI_VERSION, 2);
    }

    #[test]
    fn append_address_args_pads_words() {
        let base = Bytes::from(vec![0x60, 0x80]);
        let a = address!("0x0000000000000000000000000000000000000001");
        let b = address!("0x0000000000000000000000000000000000000002");
        let out = append_address_args(&base, &[a, b]);
        assert_eq!(out.len(), 2 + 64);
        assert_eq!(&out[..2], &[0x60, 0x80]);
        assert_eq!(&out[2 + 12..2 + 32], a.as_slice());
        assert_eq!(&out[2 + 32 + 12..2 + 64], b.as_slice());
    }
}
