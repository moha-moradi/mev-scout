use crate::data::TxData;
use crate::mev::multi_hop::MultiHopArbDetector;
use crate::mev::opportunity::MevOpportunity;
use crate::mev::two_hop::TwoHopArbDetector;
use crate::pool::state::PoolManager;
use crate::rpc::RpcClient;
use crate::types::GasConfig;

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
/// settled transactions for extended MEV detection (Phase 2+).
pub async fn capture_pending_block(rpc: &RpcClient) -> Option<PendingBlockCapture> {
    let (block_data, txs) = rpc.get_pending_block().await.ok()?;
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

/// Run two-hop and multi-hop arbitrage detection against the current `PoolManager`
/// state to find opportunities visible in the mempool (pending/unconfirmed txs).
///
/// Unlike settled blocks, pending txs have no execution logs, so detectors that
/// require log parsing (JIT, Sandwich, JitArb, Liquidation) are skipped. Only
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

    // Run two-hop detection on current pool state
    let two_hop_opps = two_hop.detect(
        pool_manager,
        0,
        timestamp,
        base_fee_per_gas,
        gas_config,
    );
    results.extend(two_hop_opps);

    // Run multi-hop detection on current pool state
    let multi_hop_opps = multi_hop.detect(
        pool_manager,
        0,
        timestamp,
        base_fee_per_gas,
        gas_config,
    );
    results.extend(multi_hop_opps);

    // Label all as mempool-only
    for opp in &mut results {
        opp.mempool_only = true;
    }

    results
}
