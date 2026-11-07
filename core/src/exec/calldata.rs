//! ABI encoder for `FlashArbExecutor.execute(Plan)`.
//!
//! The signature is the canonical tuple form of `Plan` (ABI v2):
//! `execute((address,address,uint256,uint256,uint256,(address,address,address,uint8,uint24)[]))`
//!
//! `Plan` is a dynamic tuple, so the argument head is a single offset word.
use alloy::primitives::{keccak256, Address, Bytes, U256};

use super::plan::ExecPlan;

pub const EXECUTE_SIG: &str =
    "execute((address,address,uint256,uint256,uint256,(address,address,address,uint8,uint24)[]))";

pub fn execute_selector() -> [u8; 4] {
    let hash = keccak256(EXECUTE_SIG.as_bytes());
    let mut sel = [0u8; 4];
    sel.copy_from_slice(&hash[..4]);
    sel
}

pub fn encode_execute(plan: &ExecPlan) -> Bytes {
    let selector = execute_selector();
    let mut out = Vec::with_capacity(4 + 32 * (7 + 1 + plan.legs.len() * 5));
    out.extend_from_slice(&selector);
    // Head of the single dynamic argument: offset to the tuple, relative to the arg area.
    out.extend_from_slice(&word_u64(32));
    // Tuple head. `legs` is the only dynamic member; its offset is relative to the tuple start.
    out.extend_from_slice(&word_addr(plan.vault));
    out.extend_from_slice(&word_addr(plan.borrow_token));
    out.extend_from_slice(&word_u256(plan.amount));
    out.extend_from_slice(&word_u256(plan.min_profit));
    out.extend_from_slice(&word_u64(plan.deadline));
    out.extend_from_slice(&word_u64(32 * 6)); // legsOffset = 192
    out.extend_from_slice(&word_u64(plan.legs.len() as u64));
    for leg in &plan.legs {
        out.extend_from_slice(&word_addr(leg.pool));
        out.extend_from_slice(&word_addr(leg.token_in));
        out.extend_from_slice(&word_addr(leg.token_out));
        out.extend_from_slice(&word_u64(u64::from(leg.kind)));
        out.extend_from_slice(&word_u64(u64::from(leg.fee)));
    }
    Bytes::from(out)
}

fn word_addr(addr: Address) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[12..].copy_from_slice(addr.as_slice());
    word
}

fn word_u256(v: U256) -> [u8; 32] {
    v.to_be_bytes()
}

fn word_u64(v: u64) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[24..].copy_from_slice(&v.to_be_bytes());
    word
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::plan::{ExecLeg, ExecPlan};
    use crate::types::FlashLoanProvider;
    use alloy::primitives::{address, U256};

    fn sample() -> ExecPlan {
        ExecPlan {
            vault: address!("0x0000000000000000000000000000000000000001"),
            borrow_token: address!("0x0000000000000000000000000000000000000002"),
            amount: U256::from(1u64),
            min_profit: U256::from(1u64),
            deadline: 12,
            legs: vec![ExecLeg {
                pool: address!("0x0000000000000000000000000000000000000003"),
                token_in: address!("0x0000000000000000000000000000000000000002"),
                token_out: address!("0x0000000000000000000000000000000000000004"),
                kind: 0,
                fee: 30,
            }],
            provider: FlashLoanProvider::Balancer,
        }
    }

    #[test]
    fn selector_matches_abi_v2() {
        let data = encode_execute(&sample());
        assert_eq!(&data[..4], execute_selector().as_slice());
        // One dynamic argument → offset 32.
        assert_eq!(&data[4..36], word_u64(32).as_slice());
        // Tuple: vault is the first word.
        assert_eq!(&data[36..68], word_addr(sample().vault).as_slice());
        // legs offset inside the tuple is 6 words.
        assert_eq!(&data[36 + 32 * 5..36 + 32 * 6], word_u64(192).as_slice());
        // Array length 1 sits immediately after the 6-word tuple head.
        let legs_at = 36 + 32 * 6;
        assert_eq!(&data[legs_at..legs_at + 32], word_u64(1).as_slice());
        let fee_at = legs_at + 32 + 32 * 4;
        assert_eq!(&data[fee_at..fee_at + 32], word_u64(30).as_slice());
        assert_eq!(data.len(), 4 + 32 + 32 * 6 + 32 + 32 * 5);
    }
}
