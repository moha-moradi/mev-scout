//! First-class LB/DLMM action events (Phase 2).
//!
//! `WithdrawnFromBins` / `DepositedToBins` previously reached only the JIT
//! pass, and `ClaimRewards` had no decoder at all. A withdraw+claim tx
//! therefore produced either nothing or a false-positive `Skim`. This pass
//! turns those typed facts into `LiquidityRemove` / `LiquidityAdd` /
//! `RewardClaim` rows.
//!
//! Actions are **not P&L claims**: a bin withdrawal returns principal and an
//! explicit reward payout is an `F`-basis field. Every event here is
//! `Confidence::Inferred`, never `Exact`, and the classifier's
//! one-kind-per-tx arbitration does not apply (see `classify_block`).
use std::collections::HashSet;

use alloy::primitives::{Address, U256};

use crate::explorer::types::{
    Confidence, JitFact, MevEvent, MevKind, PnlBasis, RewardClaimFact, TransferFact,
};

use super::gas::tx_gas_cost_wei;
use super::{BlockInput, TxInput};

/// Emit first-class actions. `jit_txs` are indices already represented by a
/// paired JIT position; emitting a second liquidity row for them would
/// double-count the same principal.
pub(super) fn classify_actions(input: &BlockInput, jit_txs: &HashSet<u64>) -> Vec<MevEvent> {
    let mut events = Vec::new();
    for tx in &input.txs {
        if !tx.success {
            continue;
        }
        if !jit_txs.contains(&tx.tx_index) {
            for j in tx.jit.iter().filter(|j| j.bin_amm && !j.is_collect) {
                events.push(lb_liquidity_event(input, tx, j));
            }
        }
        for claim in &tx.reward_claims {
            events.push(reward_claim_event(input, tx, claim));
        }
    }
    events
}

/// Bind a decoded bin amount to the ERC-20 `Transfer` leg that carries it.
/// Returns `(token, log_index)` when a unique same-amount leg exists.
fn bind_amount(amount: U256, tx: &TxInput, outbound: bool) -> Option<(Address, u64)> {
    if amount.is_zero() {
        return None;
    }
    let matches: Vec<&TransferFact> = tx
        .transfers
        .iter()
        .filter(|t| {
            t.amount == amount
                && if outbound {
                    t.from != Address::ZERO
                } else {
                    t.to != Address::ZERO
                }
        })
        .collect();
    // Two identical amounts in one tx make the token binding ambiguous.
    if matches.len() != 1 {
        return None;
    }
    Some((matches[0].token, matches[0].log_index))
}

fn lb_liquidity_event(input: &BlockInput, tx: &TxInput, j: &JitFact) -> MevEvent {
    let (kind, event_name) = if j.is_mint {
        (MevKind::LiquidityAdd, "DepositedToBins")
    } else {
        (MevKind::LiquidityRemove, "WithdrawnFromBins")
    };
    // LB amounts are packed X/Y (token0/token1 of the pool). Bind each one to
    // its same-amount Transfer leg; an unmatched amount stays unknown rather
    // than being guessed from proximity.
    let outbound = !j.is_mint;
    let leg0 = bind_amount(j.amount0, tx, outbound);
    let leg1 = bind_amount(j.amount1, tx, outbound);
    let batches: Vec<u64> = tx
        .lb_transfer_batches
        .iter()
        .filter(|b| b.emitter == j.pool)
        .map(|b| b.log_index)
        .collect();
    let mut detail = serde_json::json!({
        "action": kind.as_str(),
        "event": event_name,
        "log_index": j.log_index,
        "owner": format!("{:#x}", j.owner),
        "sender": j.sender.map(|a| format!("{a:#x}")),
        "bin_lower": j.tick_lower,
        "bin_upper": j.tick_upper,
        "bin_count": j.liquidity,
        "amount0": j.amount0.to_string(),
        "amount1": j.amount1.to_string(),
        "token0": leg0.map(|(t, _)| format!("{t:#x}")),
        "token1": leg1.map(|(t, _)| format!("{t:#x}")),
        "transfer_log0": leg0.map(|(_, idx)| idx),
        "transfer_log1": leg1.map(|(_, idx)| idx),
        "transfer_batch_logs": batches,
        "detection": "lb_bin_liquidity",
        "pnl_basis": PnlBasis::F.as_str(),
        "reasons": ["LB_BIN_LIQUIDITY"],
        "tags": ["lb_bin_liquidity"],
    });
    if let Some(map) = detail.as_object_mut() {
        map.insert("confidence_reason".into(), serde_json::json!("heuristic_typed_event"));
    }
    MevEvent {
        pools: vec![j.pool],
        // Principal movement: deliberately no profit token/amount. Reporting
        // a withdrawn balance as P&L is the exact mistake this phase fixes.
        profit_token: None,
        profit_amount: None,
        profit_tokens: Vec::new(),
        gas_cost_wei: tx_gas_cost_wei(tx),
        confidence: Confidence::Inferred,
        details: detail,
        ..MevEvent::for_tx(
            input.block,
            input.ts,
            tx.tx_index,
            tx.tx_hash,
            kind,
            j.owner,
            tx.to,
        )
    }
}

fn reward_claim_event(input: &BlockInput, tx: &TxInput, c: &RewardClaimFact) -> MevEvent {
    let mut detail = serde_json::json!({
        "action": MevKind::RewardClaim.as_str(),
        "event": "ClaimRewards",
        "log_index": c.log_index,
        "claimer": format!("{:#x}", c.claimer),
        "rewarder": format!("{:#x}", c.emitter),
        "amount": c.amount.to_string(),
        "token": format!("{:#x}", c.token),
        "detection": "explicit_claim_rewards",
        "pnl_basis": PnlBasis::F.as_str(),
        "reasons": ["EXPLICIT_REWARD_EVENT"],
        "tags": ["reward_claim"],
    });
    if let Some(map) = detail.as_object_mut() {
        map.insert("confidence_reason".into(), serde_json::json!("heuristic_typed_event"));
    }
    MevEvent {
        pools: vec![c.emitter],
        profit_token: Some(c.token),
        profit_amount: Some(c.amount),
        profit_tokens: vec![(c.token, c.amount)],
        gas_cost_wei: tx_gas_cost_wei(tx),
        confidence: Confidence::Inferred,
        details: detail,
        ..MevEvent::for_tx(
            input.block,
            input.ts,
            tx.tx_index,
            tx.tx_hash,
            MevKind::RewardClaim,
            c.claimer,
            tx.to,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::LogData;
    use crate::explorer::classify::{classify_block, decode_tx_logs_with_aliases};
    use crate::explorer::test_fixtures::{block, tx};
    use alloy::primitives::{address, b256, hex, Address, Bytes, B256};
    use std::collections::HashMap;
    use std::sync::Arc;

    const TX: B256 =
        b256!("6ded44c4edeb5f6f2e9e077001965e8dfb28910052b948c5d376a6b1885ad4a9");
    const POOL: Address = address!("8ac5707f8d4bde1d771d34c7afd81c3922b73379");
    const REWARDER: Address = address!("138ce0008f7febdcffdf27c0ad156584eca7d5b6");
    const WAVAX: Address = address!("b31f66aa3c1e785363f0875a1b74e27b85fd66c7");
    const PHAR: Address = address!("13a466998ce03db73abc2d4df3bbd845ed1f28e7");
    const OWNER: Address = address!("a79f638bb584292c2105f73b91544f57700709a4");

    const T_BIN_REWARD: B256 =
        b256!("8eb73e2dcafd6475539c6f596078fb83191363912cdefa76ac116e179604da1d");
    const T_BATCH: B256 =
        b256!("4a39dc06d4c0dbc64b70af90fd698a233a518aa5d07e595d983b8c0526c8f7fb");
    const T_WITHDRAW: B256 =
        b256!("a32e146844d6144a22e94c586715a1317d58a8aa3581ec33d040113ddcb24350");
    const T_TRANSFER: B256 =
        b256!("ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef");
    const T_CLAIM: B256 =
        b256!("9aa05b3d70a9e3e2f004f039648839560576334fb45c81f91b6db03ad9e2efc9");
    const T_OWNER: B256 =
        b256!("000000000000000000000000a79f638bb584292c2105f73b91544f57700709a4");
    const T_REWARDER: B256 =
        b256!("000000000000000000000000138ce0008f7febdcffdf27c0ad156584eca7d5b6");
    const T_POOL: B256 =
        b256!("0000000000000000000000008ac5707f8d4bde1d771d34c7afd81c3922b73379");
    const T_PHAR: B256 =
        b256!("00000000000000000000000013a466998ce03db73abc2d4df3bbd845ed1f28e7");

    fn padded_addr(a: Address) -> B256 {
        let mut t = B256::ZERO;
        t[12..].copy_from_slice(a.as_slice());
        t
    }

    fn log(address: Address, topics: Vec<B256>, data_hex: &str) -> LogData {
        LogData {
            log_index: None,
            address,
            topics,
            data: Bytes::from(hex::decode(data_hex).expect("valid raw log data")),
        }
    }

    /// Exact receipt-log payloads from Avalanche tx `0x6ded44c4…ad4a9`
    /// (block 100538126), read-only from `tx_evidence` before the skim fix.
    fn audited_logs() -> Vec<LogData> {
        vec![
            log(REWARDER, vec![T_BIN_REWARD], concat!(
                "00000000000000000000000000000000000000000000000000000000007f377a",
                "0000000000000000000000000000000000000000000000000000000000000040",
                "0000000000000000000000000000000000000000000000000000000000000001",
                "000000000000000000000000000000000004d1bc101b0f1dc9e98743c8d7ed1e",
            )),
            log(POOL, vec![T_BATCH, T_OWNER, T_OWNER, B256::ZERO], concat!(
                "0000000000000000000000000000000000000000000000000000000000000040",
                "0000000000000000000000000000000000000000000000000000000000000080",
                "0000000000000000000000000000000000000000000000000000000000000001",
                "00000000000000000000000000000000000000000000000000000000007f377d",
                "0000000000000000000000000000000000000000000000000000000000000001",
                "0000000000000000000000000000000000000000013af9518acb2c88ac20e6a5",
            )),
            log(POOL, vec![T_WITHDRAW, T_OWNER, T_OWNER], concat!(
                "0000000000000000000000000000000000000000000000000000000000000040",
                "0000000000000000000000000000000000000000000000000000000000000080",
                "0000000000000000000000000000000000000000000000000000000000000001",
                "00000000000000000000000000000000000000000000000000000000007f377d",
                "0000000000000000000000000000000000000000000000000000000000000001",
                "000000000000000000000000000000000000000000000017669465789aac0000",
            )),
            log(WAVAX, vec![T_TRANSFER, T_POOL, T_OWNER],
                "000000000000000000000000000000000000000000000017669465789aac0000"),
            log(PHAR, vec![T_TRANSFER, T_REWARDER, T_OWNER],
                "000000000000000000000000000000000000000000000001003bc45ee69f3dab"),
            log(REWARDER, vec![T_CLAIM, T_OWNER, T_PHAR],
                "000000000000000000000000000000000000000000000001003bc45ee69f3dab"),
        ]
    }

    #[test]
    fn audited_withdraw_and_claim_replay_to_two_actions_and_zero_unknowns() {
        let logs = audited_logs();
        let facts = decode_tx_logs_with_aliases(0, &logs, &HashMap::new(), &HashMap::new());
        assert!(
            facts.unmapped_logs.is_empty(),
            "every one of the 6 receipt logs must be claimed: {:?}",
            facts.unmapped_logs
        );
        assert_eq!(facts.jit.len(), 1);
        assert_eq!(facts.reward_claims.len(), 1);
        assert_eq!(facts.lb_transfer_batches.len(), 1);
        assert_eq!(facts.lb_reward_accumulators.len(), 1);
        assert_eq!(facts.transfers.len(), 2);

        let mut input = block(vec![tx(0, OWNER, true, facts.swaps, facts.transfers)]);
        input.txs[0].tx_hash = TX;
        input.txs[0].jit = facts.jit;
        input.txs[0].reward_claims = facts.reward_claims;
        input.txs[0].lb_transfer_batches = facts.lb_transfer_batches;
        input.txs[0].lb_reward_accumulators = facts.lb_reward_accumulators;
        // Even if the registry still calls this pool V2-like, the decoded bin
        // withdrawal must win over a skim inference.
        input.ctx.v2_like_pools = Arc::new([POOL].into_iter().collect());
        let out = classify_block(&input);

        let kinds: Vec<MevKind> = out.events.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec![MevKind::LiquidityRemove, MevKind::RewardClaim],
            "expected withdraw + claim, got {out:?}"
        );
        assert!(
            out.events.iter().all(|e| e.confidence != Confidence::Exact),
            "typed actions are heuristic, never exact"
        );
        let remove = &out.events[0];
        assert_eq!(remove.pools, vec![POOL]);
        assert_eq!(remove.searcher, OWNER);
        assert_eq!(remove.profit_amount, None);
        assert_eq!(remove.details["amount0"], "431666758152406958080");
        assert_eq!(
            remove.details["token0"],
            serde_json::json!(format!("{WAVAX:#x}"))
        );
        let claim = &out.events[1];
        assert_eq!(
            claim.profit_amount,
            Some(U256::from(18_463_567_009_210_645_931u128))
        );
        assert_eq!(
            claim.profit_token,
            Some(PHAR)
        );
        assert_eq!(claim.details["token"], format!("{PHAR:#x}"));
        assert_eq!(claim.details["rewarder"], format!("{REWARDER:#x}"));
        // Padding helper must produce the exact indexed topics used above.
        assert_eq!(padded_addr(OWNER), T_OWNER);
        assert_eq!(padded_addr(REWARDER), T_REWARDER);
    }
}
