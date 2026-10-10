//! UniV2-style skim capture pass.
use std::collections::{HashMap, HashSet};

use alloy::primitives::{Address, U256};

use crate::explorer::types::{Confidence, MevEvent, MevKind, PnlBasis};

use super::BlockInput;

/// Realized UniV2 `skim` capture: registry V2-like pair outbound Transfers
/// that are not accompanied by a same-tx Swap, Sync, Mint, or Burn on that pair.
pub(super) fn classify_skims(input: &BlockInput) -> Vec<MevEvent> {
    if input.v2_like_pools.is_empty() {
        return Vec::new();
    }
    let mut events = Vec::new();
    for tx in &input.txs {
        if !tx.success {
            continue;
        }
        let mut excluded: HashSet<Address> = HashSet::new();
        for s in &tx.swaps {
            excluded.insert(s.pool);
        }
        for op in &tx.v2_pair_ops {
            excluded.insert(op.pool);
        }

        // pool → (token → amount, recipients)
        let mut by_pool: HashMap<Address, HashMap<Address, (U256, HashSet<Address>)>> =
            HashMap::new();
        for t in &tx.transfers {
            if t.amount.is_zero() || t.from.is_zero() {
                continue;
            }
            if !input.v2_like_pools.contains(&t.from) || excluded.contains(&t.from) {
                continue;
            }
            let entry = by_pool.entry(t.from).or_default();
            let slot = entry.entry(t.token).or_insert((U256::ZERO, HashSet::new()));
            slot.0 = slot.0.saturating_add(t.amount);
            if !t.to.is_zero() {
                slot.1.insert(t.to);
            }
        }

        for (pool, tokens) in by_pool {
            let mut profit_tokens: Vec<(Address, U256)> = tokens
                .iter()
                .filter(|(_, (amt, _))| !amt.is_zero())
                .map(|(tok, (amt, _))| (*tok, *amt))
                .collect();
            if profit_tokens.is_empty() {
                continue;
            }
            profit_tokens.sort_by_key(|a| a.0);
            // Primary display token: prefer profit-policy priority, else largest amount.
            let (profit_token, profit_amount) = profit_tokens
                .iter()
                .find(|(tok, _)| input.profit_policy.priority.contains(tok))
                .copied()
                .or_else(|| profit_tokens.iter().max_by(|a, b| a.1.cmp(&b.1)).copied())
                .unwrap_or((Address::ZERO, U256::ZERO));

            let recipients: Vec<String> = tokens
                .values()
                .flat_map(|(_, recips)| recips.iter())
                .copied()
                .collect::<HashSet<_>>()
                .into_iter()
                .map(|a| format!("{:#x}", a))
                .collect();
            let amounts: Vec<serde_json::Value> = profit_tokens
                .iter()
                .map(|(tok, amt)| {
                    serde_json::json!({
                        "token": format!("{:#x}", tok),
                        "amount": amt.to_string(),
                    })
                })
                .collect();

            events.push(MevEvent {
                block: input.block,
                ts: input.ts,
                tx_index: tx.tx_index,
                tx_hash: tx.tx_hash,
                kind: MevKind::Skim,
                searcher: tx.from,
                contract: tx.to,
                pools: vec![pool],
                profit_token: Some(profit_token),
                profit_amount: Some(profit_amount),
                profit_tokens: profit_tokens.clone(),
                profit_usd: None,
                gas_cost_wei: U256::from(tx.gas_used)
                    .saturating_mul(U256::from((tx.effective_gas_price_gwei * 1e9) as u128)),
                flashloan_fee_wei: None,
                flashloan_fee_token: None,
                confidence: Confidence::Exact,
                victim_hashes: vec![],
                victim_swap_size: None,
                details: serde_json::json!({
                    "pool": format!("{:#x}", pool),
                    "recipients": recipients,
                    "amounts": amounts,
                    "pnl_basis": PnlBasis::R.as_str(),
                    "pnl": {
                        "basis": PnlBasis::R.as_str(),
                        "profit_amount": profit_amount.to_string(),
                        "profit_token": format!("{:#x}", profit_token),
                    },
                }),
            });
        }
    }
    events
}
