use alloy::primitives::{keccak256, Address, I256, U256};

use crate::config::ChainConfig;
use crate::error;
use crate::types::{FlashLoanProvider, MevOpportunity};

/// Builds calldata for each executor contract type.
pub struct TxBuilder {
    pub executor_factory: Address,
    pub chain_defaults: ChainConfig,
}

impl TxBuilder {
    pub fn new(executor_factory: Address, chain_defaults: ChainConfig) -> Self {
        TxBuilder {
            executor_factory,
            chain_defaults,
        }
    }

    pub fn build_arbitrage_tx(
        &self,
        opp: &MevOpportunity,
        provider: FlashLoanProvider,
        min_profit: U256,
    ) -> Result<Vec<u8>, error::Error> {
        let provider_val: u8 = match provider {
            FlashLoanProvider::Auto | FlashLoanProvider::Balancer => 0,
            FlashLoanProvider::Aave => 1,
            FlashLoanProvider::Uniswap => 2,
        };
        let swap_path = encode_arbitrage_swap_path(opp);
        Ok(abi_encode_call(
            "executeArbitrage(uint8,address,address,uint256,uint256,bytes)",
            &[
                Arg::U8(provider_val),
                Arg::Address(opp.token_in),
                Arg::Address(opp.token_out),
                Arg::U256(opp.input_amount),
                Arg::U256(min_profit),
                Arg::Bytes(swap_path),
            ],
        ))
    }

    pub fn build_sandwich_txs(
        &self,
        opp: &MevOpportunity,
        pool: Address,
    ) -> Result<(Vec<u8>, Vec<u8>), error::Error> {
        let front = abi_encode_call(
            "executeSandwich(address,address,uint256,address,address,uint256)",
            &[
                Arg::Address(opp.token_in),
                Arg::Address(opp.token_out),
                Arg::U256(opp.input_amount),
                Arg::Address(pool),
                Arg::Address(Address::ZERO),
                Arg::U256(opp.expected_profit / U256::from(2)),
            ],
        );
        let back = abi_encode_call(
            "executeSandwich(address,address,uint256,address,address,uint256)",
            &[
                Arg::Address(opp.token_out),
                Arg::Address(opp.token_in),
                Arg::U256(opp.input_amount),
                Arg::Address(pool),
                Arg::Address(Address::ZERO),
                Arg::U256(opp.expected_profit),
            ],
        );
        Ok((front, back))
    }

    pub fn build_liquidation_tx(
        &self,
        opp: &MevOpportunity,
        user: Address,
        aave_pool: Address,
    ) -> Result<Vec<u8>, error::Error> {
        Ok(abi_encode_call(
            "executeLiquidation(address,address,address,uint256,address,uint8,uint256)",
            &[
                Arg::Address(user),
                Arg::Address(opp.token_in),
                Arg::Address(opp.token_out),
                Arg::U256(opp.input_amount),
                Arg::Address(aave_pool),
                Arg::U8(1),
                Arg::U256(opp.expected_profit),
            ],
        ))
    }

    pub fn build_jit_txs(
        &self,
        opp: &MevOpportunity,
        pool: Address,
        tick_lower: i32,
        tick_upper: i32,
    ) -> Result<(Vec<u8>, Vec<u8>), error::Error> {
        let call = abi_encode_call(
            "executeJit(address,int24,int24,uint256,uint256,address)",
            &[
                Arg::Address(pool),
                Arg::Int(tick_lower, 24),
                Arg::Int(tick_upper, 24),
                Arg::U256(U256::ZERO),
                Arg::U256(opp.input_amount),
                Arg::Address(Address::ZERO),
            ],
        );
        Ok((call.clone(), call))
    }
}

// ── Manual ABI encoder ──────────────────────────────────────────────────

enum Arg {
    Address(Address),
    U256(U256),
    U8(u8),
    Int(i32, usize),
    Bytes(Vec<u8>),
}

fn encode_head(arg: &Arg, dynamic_offset: &mut usize) -> (Vec<u8>, Option<Vec<u8>>) {
    match arg {
        Arg::Address(addr) => {
            let mut buf = [0u8; 32];
            buf[12..32].copy_from_slice(addr.as_ref());
            (buf.to_vec(), None)
        }
        Arg::U256(val) => {
            let be: [u8; 32] = val.to_be_bytes();
            (be.to_vec(), None)
        }
        Arg::U8(val) => {
            let mut buf = [0u8; 32];
            buf[31] = *val;
            (buf.to_vec(), None)
        }
        Arg::Int(val, _bits) => {
            let i256 = I256::try_from(*val as i128).unwrap_or(I256::ZERO);
            let be: [u8; 32] = i256.to_be_bytes();
            (be.to_vec(), None)
        }
        Arg::Bytes(data) => {
            let offset = *dynamic_offset;
            let len = data.len();
            let padded_len = ((len + 31) / 32) * 32;
            let mut tail = Vec::with_capacity(32 + padded_len);
            tail.extend_from_slice(&U256::from(len).to_be_bytes::<32>());
            tail.extend_from_slice(data);
            tail.resize(32 + padded_len, 0u8);
            *dynamic_offset += 32 + padded_len;
            (U256::from(offset as u128).to_be_bytes::<32>().to_vec(), Some(tail))
        }
    }
}

fn abi_encode_call(sig: &str, args: &[Arg]) -> Vec<u8> {
    let selector = &keccak256(sig.as_bytes())[..4];
    let mut dynamic_offset = 32 * args.len();
    let mut heads = Vec::with_capacity(args.len());
    let mut tails: Vec<Vec<u8>> = Vec::new();
    for arg in args {
        let (head, tail_opt) = encode_head(arg, &mut dynamic_offset);
        heads.push(head);
        if let Some(tail) = tail_opt {
            tails.push(tail);
        }
    }
    let mut calldata = selector.to_vec();
    for head in &heads {
        calldata.extend_from_slice(head);
    }
    for tail in &tails {
        calldata.extend_from_slice(tail);
    }
    calldata
}

fn encode_arbitrage_swap_path(opp: &MevOpportunity) -> Vec<u8> {
    // Encode as an array of SwapStep structs:
    // struct SwapStep { address tokenIn; address tokenOut; address pool; uint24 fee; uint256 amount; }
    // Array = offset + length + elements (each 5*32 = 160 bytes)
    // We encode two steps for a two-hop arbitrage
    let step_size = 5 * 32; // 160 bytes per step
    let num_steps = 2u64;
    let total_data = 32 + 32 + (num_steps as usize) * step_size; // array offset(32) + length(32) + elements

    let mut buf = Vec::with_capacity(total_data);

    // Tuple encoding: offset to array = 32 (since it's the only param)
    buf.extend_from_slice(&U256::from(32u64).to_be_bytes::<32>());

    // Array length
    buf.extend_from_slice(&U256::from(num_steps).to_be_bytes::<32>());

    // Step 1: token_in -> pool_a
    buf.extend_from_slice(&address_to_32(&opp.token_in));
    buf.extend_from_slice(&address_to_32(&opp.pool_a));
    buf.extend_from_slice(&address_to_32(&Address::ZERO));
    buf.extend_from_slice(&uint64_to_32(3000));
    buf.extend_from_slice(&U256::to_be_bytes::<32>(&opp.input_amount));

    // Step 2: pool_a -> token_out
    buf.extend_from_slice(&address_to_32(&opp.pool_a));
    buf.extend_from_slice(&address_to_32(&opp.token_out));
    buf.extend_from_slice(&address_to_32(&Address::ZERO));
    buf.extend_from_slice(&uint64_to_32(3000));
    buf.extend_from_slice(&U256::to_be_bytes::<32>(&(opp.expected_profit + opp.input_amount)));

    buf
}

fn address_to_32(addr: &Address) -> [u8; 32] {
    let mut buf = [0u8; 32];
    buf[12..32].copy_from_slice(addr.as_ref());
    buf
}

fn uint64_to_32(val: u64) -> [u8; 32] {
    let be = val.to_be_bytes();
    let mut buf = [0u8; 32];
    buf[24..32].copy_from_slice(&be);
    buf
}
