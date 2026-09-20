//! Phase 6 golden-block end-to-end fixture.
//!
//! One synthetically-built block (one atomic-arb tx, one three-tx sandwich,
//! one JIT Mint→Burn) runs the full realized-MEV path — `classify_block` →
//! `ExplorerStore::insert_block_facts` — and asserts the exact event kinds,
//! profit token/amount, gas, and the persisted `mev_ops` rows with their
//! canonical IDs. No external RPC required.

use std::collections::HashMap;

use alloy::primitives::{address, Address, B256, U256};

use mev_scout_core::explorer::classify::{classify_block, BlockInput, TxInput};
use mev_scout_core::explorer::profit::ProfitTokenPolicy;
use mev_scout_core::explorer::store::{
    BlockFactsInput, ExplorerStore, SwapRow, TransferRow, TxRow,
};
use mev_scout_core::explorer::types::{Amm, Confidence, JitFact, MevKind, SwapFact, TransferFact};

const BLOCK: u64 = 10_000;
const USDC: Address = address!("4000000000000000000000000000000000000005");
const TOKA: Address = address!("5000000000000000000000000000000000000006");
const POOL_A: Address = address!("3000000000000000000000000000000000000003");
const POOL_B: Address = address!("3000000000000000000000000000000000000004");
const POOL_S: Address = address!("3000000000000000000000000000000000000033");
const POOL_J: Address = address!("3000000000000000000000000000000000000034");
const ARB: Address = address!("1000000000000000000000000000000000000001");
const SAND: Address = address!("1000000000000000000000000000000000000021");
const VICTIM: Address = address!("2000000000000000000000000000000000000002");
const JIT: Address = address!("1000000000000000000000000000000000000041");
const WNATIVE: Address = address!("6000000000000000000000000000000000000007");

const GAS_USED: u64 = 100_000;
const GAS_GWEI: f64 = 30.0;

fn swap(pool: Address, tin: Address, tout: Address, ain: u64, aout: u64) -> SwapFact {
    SwapFact {
        tx_index: 0,
        log_index: 0,
        pool,
        amm: Amm::V2,
        token_in: tin,
        token_out: tout,
        amount_in: U256::from(ain),
        amount_out: U256::from(aout),
        tick: None,
        owner: None,
    }
}

fn swap_v3_at_tick(pool: Address, tin: Address, tout: Address, ain: u64, aout: u64) -> SwapFact {
    SwapFact {
        tx_index: 0,
        log_index: 0,
        pool,
        amm: Amm::V3,
        token_in: tin,
        token_out: tout,
        amount_in: U256::from(ain),
        amount_out: U256::from(aout),
        tick: Some(0),
        owner: None,
    }
}

fn transfer(li: u64, token: Address, from: Address, to: Address, amt: u64) -> TransferFact {
    TransferFact {
        tx_index: 0,
        log_index: li,
        token,
        from,
        to,
        amount: U256::from(amt),
    }
}

fn tx_input(
    idx: u64,
    from: Address,
    swaps: Vec<SwapFact>,
    transfers: Vec<TransferFact>,
    jit: Vec<JitFact>,
) -> TxInput {
    TxInput {
        tx_index: idx,
        tx_hash: B256::repeat_byte(idx as u8),
        from,
        to: None,
        success: true,
        gas_used: GAS_USED,
        effective_gas_price_gwei: GAS_GWEI,
        value: U256::ZERO,
        transfers,
        swaps,
        liquidations: vec![],
        flashloans: vec![],
        jit,
    }
}

fn golden_input() -> BlockInput {
    // tx0 — atomic-arb cycle: USDC → TOKA on pool A, TOKA → USDC on pool B.
    let arb_tx = tx_input(
        0,
        ARB,
        vec![swap(POOL_A, USDC, TOKA, 100, 200), swap(POOL_B, TOKA, USDC, 200, 110)],
        vec![
            transfer(0, USDC, ARB, POOL_A, 100),
            transfer(1, TOKA, POOL_A, ARB, 200),
            transfer(2, TOKA, ARB, POOL_B, 200),
            transfer(3, USDC, POOL_B, ARB, 110),
        ],
        vec![],
    );

    // tx1–tx3 — classic three-EOA sandwich on POOL_S (USDC 6-dec raw units).
    let front = tx_input(
        1,
        SAND,
        vec![swap(POOL_S, USDC, TOKA, 100_000_000, 200_000_000)],
        vec![],
        vec![],
    );
    let victim = tx_input(
        2,
        VICTIM,
        vec![swap(POOL_S, USDC, TOKA, 200_000_000, 350_000_000)],
        vec![],
        vec![],
    );
    let back = tx_input(
        3,
        SAND,
        vec![swap(POOL_S, TOKA, USDC, 350_000_000, 195_000_000)],
        vec![],
        vec![],
    );

    // tx4–tx5 — JIT: Mint (tx4, with an in-range swap at tick 0) then an
    // exact-liquidity Burn (tx5) of the same position on POOL_J.
    let mint = JitFact {
        tx_index: 4,
        log_index: 0,
        pool: POOL_J,
        owner: JIT,
        tick_lower: -100,
        tick_upper: 100,
        is_mint: true,
        liquidity: 1000,
        amount0: U256::from(5),
        amount1: U256::from(5),
        bin_amm: false,
    };
    let burn = JitFact {
        tx_index: 5,
        log_index: 0,
        pool: POOL_J,
        owner: JIT,
        tick_lower: -100,
        tick_upper: 100,
        is_mint: false,
        liquidity: 1000,
        amount0: U256::from(5),
        amount1: U256::from(5),
        bin_amm: false,
    };
    let mint_tx = tx_input(
        4,
        JIT,
        vec![swap_v3_at_tick(POOL_J, USDC, TOKA, 100, 200)],
        vec![],
        vec![mint],
    );
    let burn_tx = tx_input(5, JIT, vec![], vec![], vec![burn]);

    BlockInput {
        block: BLOCK,
        ts: 1_700_000_000,
        wrapped_native: WNATIVE,
        profit_policy: ProfitTokenPolicy {
            priority: vec![USDC],
            wrapped_native: WNATIVE,
            weth: WNATIVE,
        },
        arb_likely_parity: true,
        open_positions: vec![],
        txs: vec![arb_tx, front, victim, back, mint_tx, burn_tx],
    }
}

use mev_scout_core::explorer::pricing::TokenUsd;

#[test]
fn golden_block_classifies_arb_sandwich_jit() {
    let events = classify_block(&golden_input());
    // Exactly the three golden events — the sandwich front/back/victim txs
    // carry no ledger transfers, so no parity-arb noise is emitted.
    let kinds: Vec<MevKind> = events.iter().map(|e| e.kind).collect();
    assert_eq!(events.len(), 3, "got {kinds:?}");

    let arb = events
        .iter()
        .find(|e| e.kind == MevKind::ArbAtomic)
        .expect("golden arb missing");
    assert_eq!(arb.searcher, ARB);
    assert_eq!(arb.profit_token, Some(USDC));
    assert_eq!(arb.profit_amount, Some(U256::from(10)));
    assert_eq!(arb.confidence, Confidence::Exact);
    assert_eq!(
        arb.gas_cost_wei,
        U256::from(GAS_USED * (GAS_GWEI as u64) * 1_000_000_000)
    );

    let sandwich = events
        .iter()
        .find(|e| e.kind == MevKind::Sandwich)
        .expect("golden sandwich missing");
    assert_eq!(sandwich.searcher, SAND);
    assert_eq!(sandwich.profit_token, Some(USDC));
    // back-run out (195_000_000) − front-run in (100_000_000).
    assert_eq!(sandwich.profit_amount, Some(U256::from(95_000_000)));
    assert_eq!(sandwich.confidence, Confidence::Exact);
    assert_eq!(sandwich.tx_index, 1, "sandwich anchors on the front-run tx");
    assert_eq!(sandwich.victim_hashes, vec![B256::repeat_byte(2)]);
    // front + back gas summed.
    assert_eq!(
        sandwich.gas_cost_wei,
        U256::from(GAS_USED * (GAS_GWEI as u64) * 1_000_000_000 * 2)
    );

    let jit_ev = events
        .iter()
        .find(|e| e.kind == MevKind::Jit)
        .expect("golden JIT missing");
    assert_eq!(jit_ev.searcher, JIT);
    assert_eq!(jit_ev.confidence, Confidence::Exact);
    assert_eq!(jit_ev.tx_index, 4, "JIT anchors on the mint tx");
}

#[test]
fn golden_block_persists_mev_ops_rows() {
    let input = golden_input();
    let txs: Vec<TxInput> = input.txs.clone();
    let events = classify_block(&input);

    let tx_rows: Vec<TxRow> = txs
        .iter()
        .map(|t| TxRow {
            hash: t.tx_hash,
            tx_index: t.tx_index,
            from: t.from,
            to: t.to,
            success: t.success,
            gas_used: t.gas_used,
            effective_gas_price_gwei: t.effective_gas_price_gwei,
            priority_fee_gwei: 0.0,
            value: t.value,
        })
        .collect();
    let swap_rows: Vec<SwapRow> = txs
        .iter()
        .flat_map(|t| {
            t.swaps.iter().map(|s| SwapRow {
                tx_index: t.tx_index,
                log_index: s.log_index,
                pool: s.pool,
                amm: s.amm,
                token_in: s.token_in,
                token_out: s.token_out,
                amount_in: s.amount_in,
                amount_out: s.amount_out,
            })
        })
        .collect();
    let transfer_rows: Vec<TransferRow> = txs
        .iter()
        .flat_map(|t| {
            t.transfers.iter().map(|tr| TransferRow {
                tx_index: t.tx_index,
                log_index: tr.log_index,
                token: tr.token,
                from: tr.from,
                to: tr.to,
                amount: tr.amount,
            })
        })
        .collect();

    let store = ExplorerStore::open_in_memory().unwrap();
    let block_hash = B256::repeat_byte(0xAB);

    let mut prices = HashMap::new();
    // USDC (6 decimals) @ $1.00.
    prices.insert(
        USDC,
        TokenUsd {
            usd: 1.0,
            decimals: 6,
        },
    );

    let n = store
        .insert_block_facts(BlockFactsInput {
            block_number: BLOCK,
            block_hash: &block_hash,
            ts: 1_700_000_000,
            base_fee_gwei: Some(25.0),
            tx_count: tx_rows.len(),
            txs: &tx_rows,
            swaps: &swap_rows,
            transfers: &transfer_rows,
            events: &events,
            native_price_usd: Some(0.75),
            token_prices: &prices,
        })
        .unwrap();
    assert_eq!(n, 3);

    let rows = store.ops_in_range(BLOCK, BLOCK, &[]).unwrap();
    assert_eq!(rows.len(), 3);

    let usdc_str = format!("{USDC:#x}");
    let row_by_kind = |kind: &str| {
        rows.iter()
            .find(|r| r.kind == kind)
            .unwrap_or_else(|| panic!("missing mev_ops row kind {kind}"))
    };

    let arb = row_by_kind("arb_atomic");
    assert_eq!(arb.confidence, "exact");
    assert_eq!(arb.tx_index, Some(0));
    assert_eq!(arb.profit_token.as_deref(), Some(usdc_str.as_str()));
    assert_eq!(arb.profit_amount.as_deref(), Some("10"));
    assert!(arb.canonical_id.as_deref().unwrap().starts_with("ArbAtomic|"));
    assert!(arb.profit_usd.unwrap() > 0.0);
    assert!(arb.gas_cost_usd.unwrap() > 0.0);

    let sandwich = row_by_kind("sandwich");
    assert_eq!(sandwich.confidence, "exact");
    assert_eq!(sandwich.tx_index, Some(1));
    assert_eq!(sandwich.profit_amount.as_deref(), Some("95000000"));
    assert!(sandwich.canonical_id.as_deref().unwrap().starts_with("Sandwich|"));
    assert!(
        sandwich.net_profit_usd.unwrap() > 0.0,
        "sandwich survives the profitability gate"
    );
    assert!(sandwich.victim_hashes.as_deref().unwrap().contains("0x02"));

    let jit = row_by_kind("jit");
    assert_eq!(jit.confidence, "exact");
    assert_eq!(jit.tx_index, Some(4));
    assert_eq!(jit.profit_token, None, "JIT is fee-capture; no profit token");
    assert!(jit.canonical_id.as_deref().unwrap().starts_with("Jit|"));
}