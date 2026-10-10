//! Shared synthetic builders for explorer unit/scenario tests.
//!
//! Address constants and BlockInput/TxInput helpers used by `classify` tests
//! and the strategy catalogue in `scenarios`. Only compiled under `cfg(test)`.

#![allow(dead_code)]

use std::collections::{HashMap, HashSet};

use alloy::primitives::{address, Address, B256, U256};

use crate::explorer::classify::{BlockInput, TxInput};
use crate::explorer::profit::ProfitTokenPolicy;
use crate::explorer::types::{
    Amm, FlashLoanFact, JitFact, LegSource, LiquidationFact, MevEvent, MevKind, SwapFact,
    TransferFact,
};

pub const ATK: Address = address!("1000000000000000000000000000000000000001");
pub const VICTIM: Address = address!("2000000000000000000000000000000000000002");
pub const POOL_A: Address = address!("3000000000000000000000000000000000000003");
pub const POOL_B: Address = address!("3000000000000000000000000000000000000004");
pub const USDC: Address = address!("4000000000000000000000000000000000000005");
pub const TOKA: Address = address!("5000000000000000000000000000000000000006");
pub const WNATIVE: Address = address!("6000000000000000000000000000000000000007");
pub const MARKET: Address = address!("8000000000000000000000000000000000000008");
/// USDT (fee-on-transfer registry, `data/fot_tokens.json`).
pub const FOT: Address = address!("dac17f958d2ee523a2206206994597c13d831ec7");
/// AMPL (rebase registry, `data/fot_tokens.json`).
pub const REBASE: Address = address!("d46ba6d942050d489dbd938a2c909a5d5039a161");
/// Benqi sAVAX.
pub const SAVAX: Address = address!("2b2c81e08f1af8835a78bb2a90ae924ace0ea4be");

pub fn flash_loan(provider: Address) -> FlashLoanFact {
    FlashLoanFact {
        tx_index: 0,
        log_index: 0,
        protocol: "aave_v3",
        initiator: ATK,
        token: USDC,
        amount: U256::from(1000),
        fee: Some(U256::from(5)),
        recipient: ATK,
        provider,
    }
}

pub fn liquidation(
    protocol: &'static str,
    liquidator: Address,
    collateral: Address,
    debt: Address,
    collateral_amount: u64,
    debt_to_cover: u64,
) -> LiquidationFact {
    LiquidationFact {
        tx_index: 0,
        log_index: 0,
        protocol,
        emitter: Address::ZERO,
        user: VICTIM,
        liquidator,
        collateral_asset: collateral,
        debt_asset: debt,
        collateral_amount: U256::from(collateral_amount),
        debt_to_cover: U256::from(debt_to_cover),
        bad_debt_assets: U256::ZERO,
    }
}

pub fn jit(tx_index: u64, is_mint: bool) -> JitFact {
    JitFact {
        tx_index,
        log_index: 0,
        pool: POOL_A,
        owner: ATK,
        tick_lower: -100,
        tick_upper: 100,
        is_mint,
        liquidity: 1000,
        amount0: U256::from(5),
        amount1: U256::from(5),
        bin_amm: false,
    }
}

pub fn swap(pool: Address, tin: Address, tout: Address, ain: u64, aout: u64) -> SwapFact {
    SwapFact {
        tx_index: 0,
        log_index: 0,
        pool,
        amm: Amm::V2,
        token_in: tin,
        token_out: tout,
        token_source: LegSource::Registry,
        amount_in: U256::from(ain),
        amount_out: U256::from(aout),
        tick: None,
        owner: None,
    }
}

pub fn swap_owned(
    owner: Address,
    pool: Address,
    tin: Address,
    tout: Address,
    ain: u64,
    aout: u64,
) -> SwapFact {
    SwapFact {
        owner: Some(owner),
        ..swap(pool, tin, tout, ain, aout)
    }
}

pub fn swap_at_tick(
    pool: Address,
    tin: Address,
    tout: Address,
    ain: u64,
    aout: u64,
    tick: i32,
) -> SwapFact {
    let mut s = swap(pool, tin, tout, ain, aout);
    s.amm = Amm::V3;
    s.tick = Some(tick);
    s
}

pub fn transfer(log_idx: u64, token: Address, from: Address, to: Address, amt: u64) -> TransferFact {
    TransferFact {
        tx_index: 0,
        log_index: log_idx,
        token,
        from,
        to,
        amount: U256::from(amt),
    }
}

pub fn tx(
    idx: u64,
    from: Address,
    success: bool,
    swaps: Vec<SwapFact>,
    transfers: Vec<TransferFact>,
) -> TxInput {
    TxInput {
        tx_index: idx,
        tx_hash: B256::repeat_byte(idx as u8),
        from,
        to: None,
        success,
        gas_used: 100_000,
        effective_gas_price_gwei: 30.0,
        value: U256::ZERO,
        transfers,
        swaps,
        liquidations: vec![],
        flashloans: vec![],
        buy_collaterals: vec![],
        jit: vec![],
        v2_pair_ops: vec![],
        oracle_updates: vec![],
        reserve_updates: vec![],
        rate_cache_updates: vec![],
        keepers: vec![],
        epoch_rewards: vec![],
        gmx_events: vec![],
        user_ops: vec![],
    }
}

pub fn block(txs: Vec<TxInput>) -> BlockInput {
    BlockInput {
        block: 12345,
        ts: 1_700_000_000,
        wrapped_native: WNATIVE,
        profit_policy: ProfitTokenPolicy {
            priority: vec![USDC],
            wrapped_native: WNATIVE,
            weth: WNATIVE,
        },
        arb_likely_parity: true,
        open_positions: vec![],
        v2_like_pools: HashSet::new(),
        chainlink_feeds: HashMap::new(),
        savax: None,
        epoch_venue_pools: HashSet::new(),
        gmx_event_emitters: HashSet::new(),
        interest_lookback: crate::explorer::interest_attr::InterestLookback::default(),
        prior_oracle_answers: HashMap::new(),
        savax_exchange_rate_wad: None,
        txs,
    }
}

pub fn block_with_v2(txs: Vec<TxInput>, v2_like: &[Address]) -> BlockInput {
    let mut input = block(txs);
    input.v2_like_pools = v2_like.iter().copied().collect();
    input
}

pub fn event_of(events: &[MevEvent], kind: MevKind) -> &MevEvent {
    events.iter().find(|e| e.kind == kind).unwrap_or_else(|| {
        panic!(
            "expected a {:?} event; got {:?}",
            kind,
            events.iter().map(|e| e.kind).collect::<Vec<_>>()
        )
    })
}

pub fn kinds(events: &[MevEvent]) -> Vec<MevKind> {
    let mut v: Vec<MevKind> = events.iter().map(|e| e.kind).collect();
    v.sort_by_key(|k| k.as_str());
    v
}

pub fn classify_kind(input: &BlockInput, kind: MevKind) -> MevEvent {
    event_of(&crate::explorer::classify::classify_block(input), kind).clone()
}

pub fn has_tag(events: &[MevEvent], tag: &str) -> bool {
    events.iter().any(|e| {
        e.details
            .get("tags")
            .and_then(|t| t.as_array())
            .is_some_and(|a| a.iter().any(|x| x.as_str() == Some(tag)))
    })
}

pub fn pnl_basis(ev: &MevEvent) -> Option<&str> {
    ev.details.get("pnl_basis").and_then(|v| v.as_str())
}
