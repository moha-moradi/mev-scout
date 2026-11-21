use alloy::primitives::U256;

use crate::data::TxData;
use crate::mev::detectors::MultiHopArbDetector;
use crate::mev::detectors::TwoHopArbDetector;
use crate::pool::state::{PoolManager, ScanScope};
use crate::rpc::RpcClient;
use crate::types::GasConfig;
use crate::types::MevOpportunity;

/// Result of capturing the pending block from the mempool.
#[derive(Debug, Clone)]
pub struct PendingBlockCapture {
    pub block_number: u64,
    pub txs: Vec<TxData>,
    pub tx_count: usize,
    pub base_fee_per_gas: u128,
    pub timestamp: u64,
}

/// Capture the current pending block from the node's mempool.
///
/// Calls `eth_getBlockByNumber("pending", true)` to retrieve all pending
/// (not-yet-mined) transactions. Returns `None` if the RPC call fails or
/// the pending block is unavailable (some nodes disable this).
///
/// The returned txs can be used for informational display or merged with
/// settled transactions for extended MEV detection.
pub async fn capture_pending_block(rpc: &RpcClient) -> Option<PendingBlockCapture> {
    let (block_data, txs) = match rpc.get_pending_block().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("Failed to fetch pending block (mempool pass skipped): {e:#}");
            return None;
        }
    };
    let tx_count = txs.len();
    tracing::info!(
        "Captured pending block: {} pending transactions (block #{})",
        tx_count,
        block_data.number,
    );
    Some(PendingBlockCapture {
        block_number: block_data.number,
        txs,
        tx_count,
        base_fee_per_gas: block_data.base_fee_per_gas.unwrap_or(0),
        timestamp: block_data.timestamp,
    })
}

/// Find the pending tx that should trigger a backrun re-quote (C-07).
///
/// Pending blocks have no logs, so the cheapest robust proxy for a "large
/// swap" is a native-value transfer into a contract (non-empty calldata) at or
/// above `min_value_wei`. Token-only swaps with zero native value are already
/// covered by the ordinary pending-state arb detector; this trigger exists to
/// re-quote the backrun around a materially large pending move. Returns the
/// transaction index and the tx itself, or `None` when no tx clears the floor.
pub fn find_large_pending_swap(txs: &[TxData], min_value_wei: U256) -> Option<usize> {
    let mut best: Option<(usize, U256)> = None;
    for (i, tx) in txs.iter().enumerate() {
        if tx.input.is_empty() || tx.value < min_value_wei {
            continue;
        }
        if best.map_or(true, |(_, best_value)| tx.value > best_value) {
            best = Some((i, tx.value));
        }
    }
    best.map(|(i, _)| i)
}

/// Run two-hop and multi-hop arbitrage detection against the current `PoolManager`
/// state to find opportunities visible in the mempool (pending/unconfirmed txs).
///
/// Unlike settled blocks, pending txs have no execution logs, so detectors that
/// require log parsing (JIT) are skipped. Only
/// pool-state-based arbitrage detection is performed.
///
/// Returns opportunities with `mempool_only = true`.
pub fn detect_pending_opportunities(
    pool_manager: &PoolManager,
    gas_config: GasConfig,
    base_fee_per_gas: u128,
    timestamp: u64,
    block_number: u64,
) -> Vec<MevOpportunity> {
    let mut two_hop = TwoHopArbDetector::new(block_number);
    let mut multi_hop = MultiHopArbDetector::new(block_number);

    let mut results = Vec::new();

    let scope = ScanScope::Full;
    let ctx = super::DetectCtx::new(
        pool_manager,
        0,
        timestamp,
        base_fee_per_gas,
        gas_config,
        &scope,
    );
    // Run two-hop / multi-hop detection on current pool state (full scan — single pass)
    results.extend(two_hop.detect(ctx));
    results.extend(multi_hop.detect(ctx));

    // Label all as mempool-only
    for opp in &mut results {
        opp.mempool_only = true;
        opp.detection_path = Some(crate::mev::detectors::DetectionPath::Pending);
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, b256, Bytes};

    fn pending_tx(index: u64, value: u128, calldata: bool) -> TxData {
        TxData {
            hash: b256!("0x0000000000000000000000000000000000000000000000000000000000000001"),
            index,
            tx_type: 2,
            from: address!("0x0000000000000000000000000000000000000001"),
            to: Some(address!("0x0000000000000000000000000000000000000002")),
            input: if calldata {
                Bytes::from(vec![0x38, 0xed, 0x17, 0x39])
            } else {
                Bytes::new()
            },
            value: U256::from(value),
            gas_limit: 100_000,
            max_fee_per_gas: 1,
            max_priority_fee_per_gas: Some(1),
            gas_price: None,
            nonce: 0,
            access_list: vec![],
            authorization_list: vec![],
        }
    }

    /// C-07 acceptance: a large pending swap selects the victim tx for the
    /// backrun re-quote; dust and plain value transfers are not triggers.
    #[test]
    fn large_pending_swap_triggers_requote() {
        let txs = vec![
            pending_tx(0, 10, true),
            pending_tx(1, 5 * 10u128.pow(18), true),
            pending_tx(2, 9 * 10u128.pow(18), false), // no calldata → not a swap
        ];
        let min = U256::from(10u128.pow(18));
        assert_eq!(find_large_pending_swap(&txs, min), Some(1));
        assert_eq!(
            find_large_pending_swap(&txs, U256::from(100u128.pow(18))),
            None
        );
    }
}
