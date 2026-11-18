//! Transaction-local evidence check for skim announcements.
//!
//! The classifier can prove a V2-like pair paid tokens out with no swap/log
//! counter-evidence, but that alone cannot distinguish real reserve excess
//! from a token whose `balanceOf` is synthetic. This pass reconciles the
//! observed outflow against historical pair/recipient balances:
//!
//! * `excess_before = balanceOf(pair, block-1) - reserve(block-1)`
//! * `gross_debit   = balanceOf(pair, block-1) - balanceOf(pair, block)`
//! * `recipient_gain` for the transfer recipients
//!
//! Only rows that pass every check are announced. Candidates whose state
//! cannot be read or is shared with another transaction stay in the store as
//! `candidate`; numeric contradictions are kept as `rejected`. Neither tier
//! enters headline metrics or profit.
use alloy::primitives::{Address, Bytes, U256};
use serde_json::{json, Value};

use crate::explorer::classify::TxInput;
use crate::explorer::types::{MevEvent, MevKind};
use crate::pool::selectors::{BALANCE_OF, GET_RESERVES};
use crate::rpc::RpcClient;

/// Maximum tolerated difference between the pair's gross debit and the
/// observed transfer amount (fee-on-transfer / reflection tokens).
const MAX_FEE_BPS: u32 = 3_000;
/// Maximum tolerated difference between the recipient's balance change and
/// the observed transfer amount.
const MAX_RECEIPT_DRIFT_BPS: u32 = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceTier {
    Verified,
    Candidate,
    Rejected,
}

impl EvidenceTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Candidate => "candidate",
            Self::Rejected => "rejected",
        }
    }
}

/// One token slot after the historical reads have been resolved.
#[derive(Debug, Clone, Copy)]
pub struct TokenObservation {
    pub amount: U256,
    pub pair_prev: U256,
    pub pair_now: U256,
    pub reserve_prev: U256,
    pub recipient_prev: U256,
    pub recipient_now: U256,
}

/// Pure tier decision for one token slot. `Rejected` wins over `Candidate`,
/// which wins over `Verified` when a skim carries several token slots.
pub fn evaluate(obs: &TokenObservation) -> (EvidenceTier, &'static str) {
    if obs.amount.is_zero() {
        return (EvidenceTier::Verified, "zero_amount");
    }
    if obs.pair_now >= obs.pair_prev {
        return (EvidenceTier::Rejected, "skim_pair_balance_not_reduced");
    }
    let gross = obs.pair_prev - obs.pair_now;
    if obs.reserve_prev >= obs.pair_prev {
        return (EvidenceTier::Rejected, "skim_excess_negative");
    }
    let excess = obs.pair_prev - obs.reserve_prev;
    if excess < gross {
        return (EvidenceTier::Rejected, "skim_excess_below_debit");
    }
    if gross < obs.amount {
        return (EvidenceTier::Rejected, "skim_gross_below_event");
    }
    let fee = gross - obs.amount;
    if fee.saturating_mul(U256::from(10_000)) > gross.saturating_mul(U256::from(MAX_FEE_BPS)) {
        return (EvidenceTier::Rejected, "skim_fee_exceeds_tolerance");
    }
    if obs.recipient_now < obs.recipient_prev {
        return (
            EvidenceTier::Rejected,
            "skim_recipient_balance_not_increased",
        );
    }
    let gain = obs.recipient_now - obs.recipient_prev;
    let diff = if gain >= obs.amount {
        gain - obs.amount
    } else {
        obs.amount - gain
    };
    if diff.saturating_mul(U256::from(10_000))
        > obs.amount.saturating_mul(U256::from(MAX_RECEIPT_DRIFT_BPS))
    {
        return (EvidenceTier::Rejected, "skim_recipient_delta_mismatch");
    }
    (EvidenceTier::Verified, "conserved")
}

fn balance_of_data(who: Address) -> Bytes {
    let mut out = Vec::with_capacity(36);
    out.extend_from_slice(&BALANCE_OF);
    out.extend_from_slice(&[0u8; 12]);
    out.extend_from_slice(who.as_slice());
    Bytes::from(out)
}

fn word(data: &Bytes, index: usize) -> Option<U256> {
    let start = index.checked_mul(32)?;
    let end = start.checked_add(32)?;
    if data.len() < end {
        return None;
    }
    Some(U256::from_be_slice(&data[start..end]))
}

async fn balance_of(
    rpc: &RpcClient,
    token: Address,
    who: Address,
    block: u64,
) -> anyhow::Result<U256> {
    let out = rpc.call(token, balance_of_data(who), block).await?;
    word(&out, 0).ok_or_else(|| anyhow::anyhow!("balanceOf returned short data"))
}

async fn reserves(rpc: &RpcClient, pool: Address, block: u64) -> anyhow::Result<(U256, U256)> {
    let out = rpc.call(pool, GET_RESERVES.clone(), block).await?;
    let r0 = word(&out, 0).ok_or_else(|| anyhow::anyhow!("getReserves returned short data"))?;
    let r1 = word(&out, 1).ok_or_else(|| anyhow::anyhow!("getReserves returned short data"))?;
    Ok((r0, r1))
}

fn parse_addr(v: &Value) -> Option<Address> {
    v.as_str()?.parse().ok()
}

fn parse_u256(v: &Value) -> Option<U256> {
    v.as_str()?.parse().ok()
}

fn mark(ev: &mut MevEvent, tier: EvidenceTier, reason: &str, detail: Value) {
    let obj = match ev.details.as_object_mut() {
        Some(o) => o,
        None => return,
    };
    let values = obj
        .entry("values")
        .or_insert_with(|| json!({}))
        .as_object_mut();
    let Some(values) = values else { return };
    values.insert("evidence_tier".into(), json!(tier.as_str()));
    values.insert(
        "evidence".into(),
        json!({ "reason": reason, "detail": detail }),
    );
}

/// True when another transfer/pair action in the block can move the same
/// token between the pre-block snapshot and the end-of-block read. Such rows
/// cannot be verified from block-boundary state; they stay `candidate`.
fn state_shared(
    txs: &[TxInput],
    ev: &MevEvent,
    pool: Address,
    token: Address,
    recipients: &[Address],
) -> bool {
    for tx in txs {
        let same_tx = tx.tx_index == ev.tx_index;
        if tx.swaps.iter().any(|s| s.pool == pool)
            || tx.v2_pair_ops.iter().any(|op| op.pool == pool)
        {
            return true;
        }
        for t in &tx.transfers {
            if t.token != token {
                continue;
            }
            let touches = t.from == pool
                || t.to == pool
                || recipients.contains(&t.from)
                || recipients.contains(&t.to);
            if !touches {
                continue;
            }
            let allowed = same_tx && t.from == pool && recipients.contains(&t.to);
            if !allowed {
                return true;
            }
        }
    }
    false
}

async fn verify_one(rpc: &RpcClient, ev: &mut MevEvent, block: u64, txs: &[TxInput]) {
    if block == 0 {
        mark(
            ev,
            EvidenceTier::Candidate,
            "skim_evidence_unavailable",
            json!({}),
        );
        return;
    }
    let Some(details) = ev.details.as_object() else {
        mark(
            ev,
            EvidenceTier::Candidate,
            "skim_evidence_missing",
            json!({}),
        );
        return;
    };
    let pool = details
        .get("pool")
        .and_then(parse_addr)
        .or_else(|| ev.pools.first().copied());
    let Some(pool) = pool else {
        mark(
            ev,
            EvidenceTier::Candidate,
            "skim_evidence_missing",
            json!({"field":"pool"}),
        );
        return;
    };
    let recipients: Vec<Address> = details
        .get("recipients")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(parse_addr).collect())
        .unwrap_or_default();
    if recipients.is_empty() {
        mark(
            ev,
            EvidenceTier::Candidate,
            "skim_evidence_missing",
            json!({"field":"recipients"}),
        );
        return;
    }
    let tokens = details.get("evidence").and_then(|e| e.get("pair_tokens"));
    let token0 = tokens.and_then(|t| t.get("token0")).and_then(parse_addr);
    let token1 = tokens.and_then(|t| t.get("token1")).and_then(parse_addr);
    let (Some(token0), Some(token1)) = (token0, token1) else {
        mark(
            ev,
            EvidenceTier::Candidate,
            "skim_evidence_missing",
            json!({"field":"pair_tokens"}),
        );
        return;
    };
    let Some(amounts) = details.get("amounts").and_then(Value::as_array) else {
        mark(
            ev,
            EvidenceTier::Candidate,
            "skim_evidence_missing",
            json!({"field":"amounts"}),
        );
        return;
    };

    let prev_block = block.saturating_sub(1);
    let (reserve0, reserve1) = match reserves(rpc, pool, prev_block).await {
        Ok(r) => r,
        Err(_) => {
            mark(
                ev,
                EvidenceTier::Candidate,
                "skim_evidence_unavailable",
                json!({"field":"reserves"}),
            );
            return;
        }
    };

    let mut tier = EvidenceTier::Verified;
    let mut reason = "conserved";
    let mut observations = Vec::new();
    for entry in amounts {
        let Some(token) = entry.get("token").and_then(parse_addr) else {
            mark(
                ev,
                EvidenceTier::Candidate,
                "skim_evidence_missing",
                json!({"field":"token"}),
            );
            return;
        };
        let Some(amount) = entry.get("amount").and_then(parse_u256) else {
            mark(
                ev,
                EvidenceTier::Candidate,
                "skim_evidence_missing",
                json!({"field":"amount"}),
            );
            return;
        };
        if amount.is_zero() {
            continue;
        }
        let (reserve_prev, token_slot) = if token == token0 {
            (reserve0, 0u8)
        } else if token == token1 {
            (reserve1, 1u8)
        } else {
            mark(
                ev,
                EvidenceTier::Rejected,
                "outflow_token_not_in_pair",
                json!({"token": format!("{token:#x}")}),
            );
            return;
        };
        if state_shared(txs, ev, pool, token, &recipients) {
            if tier != EvidenceTier::Rejected {
                tier = EvidenceTier::Candidate;
                reason = "skim_state_shared";
            }
            continue;
        }

        let pair_prev = match balance_of(rpc, token, pool, prev_block).await {
            Ok(v) => v,
            Err(_) => {
                mark(
                    ev,
                    EvidenceTier::Candidate,
                    "skim_evidence_unavailable",
                    json!({"field":"pair_prev"}),
                );
                return;
            }
        };
        let pair_now = match balance_of(rpc, token, pool, block).await {
            Ok(v) => v,
            Err(_) => {
                mark(
                    ev,
                    EvidenceTier::Candidate,
                    "skim_evidence_unavailable",
                    json!({"field":"pair_now"}),
                );
                return;
            }
        };
        let mut recipient_prev = U256::ZERO;
        let mut recipient_now = U256::ZERO;
        for recipient in &recipients {
            recipient_prev += match balance_of(rpc, token, *recipient, prev_block).await {
                Ok(v) => v,
                Err(_) => {
                    mark(
                        ev,
                        EvidenceTier::Candidate,
                        "skim_evidence_unavailable",
                        json!({"field":"recipient_prev"}),
                    );
                    return;
                }
            };
            recipient_now += match balance_of(rpc, token, *recipient, block).await {
                Ok(v) => v,
                Err(_) => {
                    mark(
                        ev,
                        EvidenceTier::Candidate,
                        "skim_evidence_unavailable",
                        json!({"field":"recipient_now"}),
                    );
                    return;
                }
            };
        }

        let obs = TokenObservation {
            amount,
            pair_prev,
            pair_now,
            reserve_prev,
            recipient_prev,
            recipient_now,
        };
        let (obs_tier, obs_reason) = evaluate(&obs);
        observations.push(json!({
            "token": format!("{token:#x}"),
            "token_slot": token_slot,
            "amount": amount.to_string(),
            "pair_prev": pair_prev.to_string(),
            "pair_now": pair_now.to_string(),
            "reserve_prev": reserve_prev.to_string(),
            "recipient_prev": recipient_prev.to_string(),
            "recipient_now": recipient_now.to_string(),
            "tier": obs_tier.as_str(),
            "reason": obs_reason,
        }));
        match obs_tier {
            EvidenceTier::Rejected => {
                tier = EvidenceTier::Rejected;
                reason = obs_reason;
            }
            EvidenceTier::Candidate => {
                if tier != EvidenceTier::Rejected {
                    tier = EvidenceTier::Candidate;
                    reason = obs_reason;
                }
            }
            EvidenceTier::Verified => {}
        }
    }

    if observations.is_empty() {
        mark(
            ev,
            EvidenceTier::Candidate,
            "skim_evidence_missing",
            json!({"field":"amounts"}),
        );
        return;
    }
    mark(ev, tier, reason, json!({ "observations": observations }));
}

/// Annotate every skim event with `values.evidence_tier`.
pub async fn verify_skim_events(
    rpc: &RpcClient,
    events: &mut [MevEvent],
    block: u64,
    txs: &[TxInput],
) {
    for ev in events.iter_mut().filter(|e| e.kind == MevKind::Skim) {
        verify_one(rpc, ev, block, txs).await;
    }
}

/// Headline visibility: a skim is announced only when its evidence tier is
/// `verified`. Non-skim kinds are unaffected.
pub fn is_announced(ev: &MevEvent) -> bool {
    if ev.kind != MevKind::Skim {
        return true;
    }
    ev.details
        .get("values")
        .and_then(|v| v.get("evidence_tier"))
        .and_then(Value::as_str)
        == Some(EvidenceTier::Verified.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    fn obs(
        amount: u128,
        prev: u128,
        now: u128,
        reserve: u128,
        rprev: u128,
        rnow: u128,
    ) -> TokenObservation {
        TokenObservation {
            amount: U256::from(amount),
            pair_prev: U256::from(prev),
            pair_now: U256::from(now),
            reserve_prev: U256::from(reserve),
            recipient_prev: U256::from(rprev),
            recipient_now: U256::from(rnow),
        }
    }

    #[test]
    fn conserved_fee_and_reflection_pass() {
        // SPORE raw amounts: excess 1,230,046.988e9, gross debit 1,230,046.734e9,
        // fee 73,802.819e9, recipient gain 1,156,244.169e9 + reflection dust.
        let (tier, reason) = evaluate(&obs(
            1_156_244_169_114_610,
            344_530_540_101_422_814_986,
            344_529_310_054_688_667_538,
            344_529_310_054_434_395_194,
            29_063_972_982_794_409,
            30_220_217_151_931_322,
        ));
        assert_eq!(tier, EvidenceTier::Verified, "{reason}");
    }

    #[test]
    fn block_random_balance_is_rejected() {
        // hihi-shaped: pair balance rises and excess is negative.
        let (tier, reason) = evaluate(&obs(259, 675, 968, 709, 429, 852));
        assert_eq!(tier, EvidenceTier::Rejected);
        assert_eq!(reason, "skim_pair_balance_not_reduced");
    }

    #[test]
    fn negative_excess_is_rejected() {
        let (tier, reason) = evaluate(&obs(10, 100, 90, 120, 0, 10));
        assert_eq!(tier, EvidenceTier::Rejected);
        assert_eq!(reason, "skim_excess_negative");
    }

    #[test]
    fn excessive_fee_is_rejected() {
        let (tier, reason) = evaluate(&obs(50, 100, 0, 0, 0, 50));
        assert_eq!(tier, EvidenceTier::Rejected);
        assert_eq!(reason, "skim_fee_exceeds_tolerance");
    }

    #[test]
    fn recipient_delta_mismatch_is_rejected() {
        let (tier, reason) = evaluate(&obs(100, 200, 100, 0, 0, 150));
        assert_eq!(tier, EvidenceTier::Rejected);
        assert_eq!(reason, "skim_recipient_delta_mismatch");
    }

    #[tokio::test]
    #[ignore = "requires MEV_SCOUT_LIVE_RPC (archive) and the Case 2 block"]
    async fn live_case2_assigns_spore_verified_and_hihi_rejected() {
        use crate::explorer::types::MevEvent;
        use alloy::primitives::B256;
        let url = std::env::var("MEV_SCOUT_LIVE_RPC").expect("MEV_SCOUT_LIVE_RPC");
        let rpc = RpcClient::new(&url, 43114).expect("rpc");
        let mut spore = MevEvent::for_tx(
            86_998_651,
            1_780_407_820,
            2,
            B256::ZERO,
            MevKind::Skim,
            address!("84f1e166d7f0c4884cbff06a35600442c80ecd76"),
            None,
        );
        spore.details = json!({
            "pool": "0x813902a6722ee55e141de2ea8dbae64896f7a44b",
            "recipients": ["0x84f1e166d7f0c4884cbff06a35600442c80ecd76"],
            "amounts": [{"token": "0x6e7f5c0b9f4432716bdd0a77a3601291b9d9e985", "amount": "1156244169114610"}],
            "evidence": {"pair_tokens": {
                "token0": "0x59414b3089ce2af0010e7523dea7e2b35d776ec7",
                "token1": "0x6e7f5c0b9f4432716bdd0a77a3601291b9d9e985"
            }}
        });
        let mut hihi = MevEvent::for_tx(
            86_998_651,
            1_780_407_820,
            2,
            B256::ZERO,
            MevKind::Skim,
            address!("84f1e166d7f0c4884cbff06a35600442c80ecd76"),
            None,
        );
        hihi.details = json!({
            "pool": "0x10eebdd5bc6ad1f35f33d63b679b693cf3c5c95d",
            "recipients": ["0x84f1e166d7f0c4884cbff06a35600442c80ecd76"],
            "amounts": [{"token": "0x84ac1aa455fe38a9b14c5d448c56489c2e97a681", "amount": "259000000000000000000"}],
            "evidence": {"pair_tokens": {
                "token0": "0x84ac1aa455fe38a9b14c5d448c56489c2e97a681",
                "token1": "0xb31f66aa3c1e785363f0875a1b74e27b85fd66c7"
            }}
        });
        let mut events = vec![spore, hihi];
        verify_skim_events(&rpc, &mut events, 86_998_651, &[]).await;
        let tier = |ev: &MevEvent| {
            ev.details["values"]["evidence_tier"]
                .as_str()
                .unwrap_or("missing")
                .to_string()
        };
        assert_eq!(tier(&events[0]), "verified");
        assert_eq!(tier(&events[1]), "rejected");
    }

    #[test]
    fn tier_strings_are_stable() {
        assert_eq!(EvidenceTier::Verified.as_str(), "verified");
        assert_eq!(EvidenceTier::Candidate.as_str(), "candidate");
        assert_eq!(EvidenceTier::Rejected.as_str(), "rejected");
        let _ = address!("1111111111111111111111111111111111111111");
    }
}
