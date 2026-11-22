//! Flash-loan event decoders plus the V2 flash-swap transfer inference.
use crate::utils::{abi_word_address, topic_address};
use alloy::primitives::{Address, U256};

use crate::chain::events::{
    decode_balancer_flash, AAVE_V2_FLASH_LOAN_TOPIC, AAVE_V3_FLASH_LOAN_TOPIC,
    BALANCER_FLASH_LOAN_TOPIC, LFJ_LB_FLASH_LOAN_TOPIC, LFJ_LB_FLASH_LOAN_V20_TOPIC,
    MORPHO_BLUE_FLASH_LOAN_TOPIC, UNI_V3_FLASH_TOPIC,
};
use crate::data::LogData;
use crate::explorer::types::{Amm, FlashLoanFact, SwapFact, TransferFact};
use crate::types::tags::funding_tag_for_source;

use super::is_unresolved_token;

/// Decode a flash-loan fact from a receipt log.
///
/// Covers Aave V2/V3, Balancer V2, Morpho Blue, Uniswap V3 `Flash`, and
/// LFJ/Trader Joe LB (2.0 plus 2.1/2.2). Uni V4 has no discrete Flash event
/// (unlock/callback flash accounting only), so the Avalanche Uni flash path
/// for plan P2.3 is V3.
pub fn decode_flash_loan(log: &LogData) -> Option<FlashLoanFact> {
    let topic0 = *log.topics.first()?;

    // Uniswap V3: Flash(address indexed sender, address indexed recipient,
    //   uint256 amount0, uint256 amount1, uint256 paid0, uint256 paid1).
    // Token identity needs the pool registry; store amount/fee on the
    // non-zero leg and leave `token = 0` for netting to fill via transfers.
    if topic0 == *UNI_V3_FLASH_TOPIC {
        if log.topics.len() < 3 || log.data.len() < 128 {
            return None;
        }
        let amount0 = U256::from_be_slice(&log.data[0..32]);
        let amount1 = U256::from_be_slice(&log.data[32..64]);
        let paid0 = U256::from_be_slice(&log.data[64..96]);
        let paid1 = U256::from_be_slice(&log.data[96..128]);
        let (amount, fee) = if !amount0.is_zero() || paid0 > paid1 {
            (amount0, paid0)
        } else {
            (amount1, paid1)
        };
        return Some(FlashLoanFact {
            tx_index: 0,
            log_index: 0,
            protocol: "uniswap_v3",
            initiator: topic_address(log.topics[1]),
            recipient: topic_address(log.topics[2]),
            token: Address::ZERO,
            amount,
            fee: Some(fee),
            provider: log.address,
        });
    }

    // Aave V3: FlashLoan(address indexed target, address initiator,
    //   address indexed asset, uint256 amount, uint8 mode, uint256 premium,
    //   uint16 referral) — topics [sig, target, asset], data 160 bytes.
    if topic0 == *AAVE_V3_FLASH_LOAN_TOPIC {
        if log.topics.len() < 3 || log.data.len() < 128 {
            return None;
        }
        return Some(FlashLoanFact {
            tx_index: 0,
            log_index: 0,
            protocol: "aave_v3",
            initiator: abi_word_address(&log.data, 0),
            recipient: topic_address(log.topics[1]),
            token: topic_address(log.topics[2]),
            amount: U256::from_be_slice(&log.data[32..64]),
            fee: Some(U256::from_be_slice(&log.data[96..128])),
            provider: log.address,
        });
    }

    // Aave V2: FlashLoan(address indexed target, address indexed initiator,
    //   address indexed asset, uint256 amount, uint256 premium, uint16 referral)
    //   — topics [sig, target, initiator, asset], data 96 bytes.
    if topic0 == *AAVE_V2_FLASH_LOAN_TOPIC {
        if log.topics.len() < 4 || log.data.len() < 64 {
            return None;
        }
        return Some(FlashLoanFact {
            tx_index: 0,
            log_index: 0,
            protocol: "aave_v2",
            initiator: topic_address(log.topics[2]),
            recipient: topic_address(log.topics[1]),
            token: topic_address(log.topics[3]),
            amount: U256::from_be_slice(&log.data[0..32]),
            fee: Some(U256::from_be_slice(&log.data[32..64])),
            provider: log.address,
        });
    }

    // Balancer V2: reuse the scanner decoder (topic + layout shared).
    if topic0 == *BALANCER_FLASH_LOAN_TOPIC {
        let rpc_log = logdata_to_rpc_log(log);
        let ev = decode_balancer_flash(&rpc_log)?;
        return Some(FlashLoanFact {
            tx_index: 0,
            log_index: 0,
            protocol: "balancer_v2",
            initiator: ev.initiator,
            token: ev.token,
            amount: ev.amount,
            fee: ev.fee,
            recipient: ev.target,
            provider: log.address,
        });
    }

    // Morpho Blue: FlashLoan(address indexed caller, address indexed token,
    //   uint256 assets) — 0% premium ( / P2.3 routing).
    if topic0 == *MORPHO_BLUE_FLASH_LOAN_TOPIC {
        if log.topics.len() < 3 || log.data.len() < 32 {
            return None;
        }
        let caller = topic_address(log.topics[1]);
        return Some(FlashLoanFact {
            tx_index: 0,
            log_index: 0,
            protocol: "morpho_blue",
            initiator: caller,
            recipient: caller,
            token: topic_address(log.topics[2]),
            amount: U256::from_be_slice(&log.data[0..32]),
            fee: Some(U256::ZERO),
            provider: log.address,
        });
    }

    // LFJ / Trader Joe LB 2.1/2.2: FlashLoan(address indexed sender,
    //   ILBFlashLoanCallback indexed receiver, uint24 activeId,
    //   bytes32 amounts, bytes32 totalFees, bytes32 protocolFees).
    // `amounts`/`totalFees` are PackedUint128 (X in the low 128 bits, Y in the
    // high 128 bits). The token is not in the log; as with Uni V3 it stays
    // zero so netting resolves it from the transfer stream. `protocol` is the
    // canonical funding source, so `funding_tag_for_source` yields
    // `funding:lfj_lb_flash` without an extra mapping.
    if topic0 == *LFJ_LB_FLASH_LOAN_TOPIC {
        if log.topics.len() < 3 || log.data.len() < 128 {
            return None;
        }
        let (amount_x, amount_y) = unpack_lb_xy(&log.data[32..64]);
        let (fee_x, fee_y) = unpack_lb_xy(&log.data[64..96]);
        // One `FlashLoanFact` holds a single leg. Prefer X on ties so a
        // two-token borrow is still attributed deterministically.
        let (amount, fee) = if amount_x >= amount_y {
            (amount_x, fee_x)
        } else {
            (amount_y, fee_y)
        };
        return Some(FlashLoanFact {
            tx_index: 0,
            log_index: 0,
            protocol: "lfj_lb_flash",
            initiator: topic_address(log.topics[1]),
            recipient: topic_address(log.topics[2]),
            token: Address::ZERO,
            amount,
            fee: Some(fee),
            provider: log.address,
        });
    }

    // Legacy LFJ / Trader Joe LB 2.0: FlashLoan(address indexed sender,
    //   ILBFlashLoanCallback indexed receiver, IERC20 token, uint256 amount,
    //   uint256 fee). The token is a normal data word here, not packed.
    if topic0 == *LFJ_LB_FLASH_LOAN_V20_TOPIC {
        if log.topics.len() < 3 || log.data.len() < 96 {
            return None;
        }
        return Some(FlashLoanFact {
            tx_index: 0,
            log_index: 0,
            protocol: "lfj_lb_flash",
            initiator: topic_address(log.topics[1]),
            recipient: topic_address(log.topics[2]),
            token: abi_word_address(&log.data, 0),
            amount: U256::from_be_slice(&log.data[32..64]),
            fee: Some(U256::from_be_slice(&log.data[64..96])),
            provider: log.address,
        });
    }

    None
}

/// V2-compatible AMMs have no discrete `FlashLoan` event: a flash swap pays
/// the borrowed token out, then the callback returns the input token before
/// the pool emits `Swap`. Infer that out→in same-address pattern from the
/// receipt's transfer stream so the funding axis (`funding:v2_flash_swap`),
/// `flash_arb` and `flash_loan_liq` see the loan like the explicit providers.
///
/// Principal and the in-kind pool fee are already in the swap's transfer legs,
/// so `token` stays zero: the arb netter deliberately skips zero-token facts
/// and would otherwise subtract the same principal twice.
pub fn infer_v2_flash_swap(
    swaps: &[SwapFact],
    transfers: &[TransferFact],
) -> Option<FlashLoanFact> {
    for s in swaps
        .iter()
        .filter(|s| matches!(s.amm, Amm::V2 | Amm::Solidly))
    {
        if is_unresolved_token(s.token_in)
            || is_unresolved_token(s.token_out)
            || s.token_in == s.token_out
            || s.amount_out.is_zero()
            || s.amount_in.is_zero()
        {
            continue;
        }

        // Pool pays the borrowed token to the callback contract before the
        // Swap log. Match the amount so unrelated pool outflows cannot trigger.
        let Some(out) = transfers.iter().find(|t| {
            t.log_index < s.log_index
                && t.from == s.pool
                && t.token == s.token_out
                && t.amount == s.amount_out
                && !t.to.is_zero()
        }) else {
            continue;
        };

        // The same callback sends the input token back to the pool, still
        // before the Swap log. A plain V2 swap is pre-funded (input transfer
        // before the outflow), so it does not match this ordering.
        let Some(back) = transfers.iter().find(|t| {
            t.log_index > out.log_index
                && t.log_index < s.log_index
                && t.from == out.to
                && t.to == s.pool
                && t.token == s.token_in
                && !t.amount.is_zero()
        }) else {
            continue;
        };
        let prefunded = transfers.iter().any(|t| {
            t.log_index < out.log_index
                && t.from == back.from
                && t.to == s.pool
                && t.token == s.token_in
        });
        if prefunded {
            continue;
        }

        return Some(FlashLoanFact {
            tx_index: 0,
            log_index: s.log_index,
            protocol: "v2_flash_swap",
            initiator: back.from,
            recipient: out.to,
            token: Address::ZERO,
            amount: s.amount_out,
            fee: None,
            provider: s.pool,
        });
    }
    None
}

/// Unpack an LFJ `PackedUint128` word: amountX in the low 16 bytes, amountY
/// in the high 16 bytes (`core::pool::decoders::decode_lb_amounts_xy` uses
/// the same layout for deposit/withdraw events).
fn unpack_lb_xy(word: &[u8]) -> (U256, U256) {
    (
        U256::from_be_slice(&word[16..32]),
        U256::from_be_slice(&word[0..16]),
    )
}

/// Funding-axis tag for a decoded flash loan (WP2 / A-01). The flash loan
/// funds an arbitrage route; it does not make the loan itself an arb family.
pub fn flash_loan_funding_tag(fact: &FlashLoanFact) -> &'static str {
    funding_tag_for_source(fact.protocol)
}

/// Minimal `LogData` → alloy `Log` bridge with placeholder block/tx metadata
/// (the explorer keys off its own tx/log indices; decoders only read the
/// event payload and topics).
fn logdata_to_rpc_log(log: &LogData) -> alloy::rpc::types::Log {
    let data = alloy::primitives::LogData::new_unchecked(log.topics.clone(), log.data.clone());
    alloy::rpc::types::Log {
        inner: alloy::primitives::Log {
            address: log.address,
            data,
        },
        block_number: Some(0),
        block_hash: Some(alloy::primitives::B256::ZERO),
        block_timestamp: None,
        transaction_hash: Some(alloy::primitives::B256::ZERO),
        transaction_index: Some(0),
        log_index: Some(0),
        removed: false,
    }
}
