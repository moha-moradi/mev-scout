//! ABI encoder for `FlashArbExecutor.execute(Plan)` (ABI v3).
//!
//! The generated `sol!` types keep the dynamic `bytes` leg payload encoded
//! correctly instead of hand-rolling offsets.
use alloy::primitives::{Address, Bytes, U256};
use alloy::sol_types::SolCall;

use super::plan::ExecPlan;

alloy::sol! {
    struct AbiLeg {
        address target;
        address tokenIn;
        address tokenOut;
        address spender;
        uint8 kind;
        uint24 fee;
        uint256 minOut;
        bytes data;
    }

    struct AbiPlan {
        address vault;
        address borrowToken;
        uint256 amount;
        uint256 minProfit;
        uint256 deadline;
        AbiLeg[] legs;
    }

    function execute(AbiPlan plan) external;
}

pub const EXECUTE_SIG: &str = executeCall::SIGNATURE;

#[allow(dead_code)]
pub fn execute_selector() -> [u8; 4] {
    executeCall::SELECTOR
}

pub fn encode_execute(plan: &ExecPlan) -> Bytes {
    let abi_plan = AbiPlan {
        vault: plan.vault,
        borrowToken: plan.borrow_token,
        amount: plan.amount,
        minProfit: plan.min_profit,
        deadline: U256::from(plan.deadline),
        legs: plan
            .legs
            .iter()
            .map(|leg| AbiLeg {
                target: leg.target,
                tokenIn: leg.token_in,
                tokenOut: leg.token_out,
                spender: leg.spender,
                kind: leg.kind,
                fee: alloy::primitives::aliases::U24::from(leg.fee),
                minOut: leg.min_out,
                data: leg.data.clone(),
            })
            .collect(),
    };
    Bytes::from(executeCall { plan: abi_plan }.abi_encode())
}

#[allow(dead_code)]
fn _address_word(addr: Address) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[12..].copy_from_slice(addr.as_slice());
    word
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::plan::{ExecLeg, ExecPlan};
    use crate::types::FlashLoanProvider;
    use alloy::primitives::{address, Bytes};

    fn sample() -> ExecPlan {
        ExecPlan {
            vault: address!("0x0000000000000000000000000000000000000001"),
            borrow_token: address!("0x0000000000000000000000000000000000000002"),
            amount: U256::from(1u64),
            min_profit: U256::from(1u64),
            deadline: 12,
            legs: vec![ExecLeg {
                target: address!("0x0000000000000000000000000000000000000003"),
                token_in: address!("0x0000000000000000000000000000000000000002"),
                token_out: address!("0x0000000000000000000000000000000000000004"),
                spender: Address::ZERO,
                kind: 0,
                fee: 30,
                min_out: U256::ZERO,
                data: Bytes::new(),
            }],
            provider: FlashLoanProvider::Balancer,
        }
    }

    #[test]
    fn selector_matches_abi_v3() {
        let data = encode_execute(&sample());
        assert_eq!(&data[..4], execute_selector().as_slice());
        assert_eq!(
            EXECUTE_SIG,
            "execute((address,address,uint256,uint256,uint256,(address,address,address,address,uint8,uint24,uint256,bytes)[]))"
        );
    }

    #[test]
    fn dynamic_leg_data_round_trips() {
        let mut plan = sample();
        plan.legs[0].kind = 2;
        plan.legs[0].data = Bytes::from(vec![0xde, 0xad, 0xbe, 0xef]);
        let data = encode_execute(&plan);
        // ABI v3 legs carry a bytes payload, so encoding must be larger than
        // the fixed 8-word tuple head.
        assert!(data.len() > 4 + 32 * 10);
        let decoded = executeCall::abi_decode(&data).expect("decode");
        assert_eq!(decoded.plan.legs.len(), 1);
        assert_eq!(decoded.plan.legs[0].kind, 2);
        assert_eq!(
            decoded.plan.legs[0].data.as_ref(),
            &[0xde, 0xad, 0xbe, 0xef]
        );
    }
}
