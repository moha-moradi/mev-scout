//! Run the paper ledger against revm or a local Anvil fork.
use std::collections::HashMap;

use alloy::primitives::{Address, U256};

use crate::paper::{ExecAttempt, FillSkipReason, LedgerPolicy, LedgerResult, SimMode};
use crate::pool::state::PoolManager;
use crate::replay::BlockReplayer;
use crate::types::{FlashLoanProvider, MevOpportunity};

use super::anvil::{reset_fork, AnvilNode, AnvilSender};
use super::calldata::encode_execute;
use super::plan::{plan_from_opportunity, PlanError};
use super::revm_backend::{exec_plan_revm, open_revm_block, RevmBlock};
use super::{ExecMode, ANVIL_DEFAULT_KEY};

/// Backend handles owned by the live session. Revm needs none of the options.
pub struct ExecContext {
    pub mode: ExecMode,
    pub chain_id: u64,
    pub vault: Address,
    pub weth: Address,
    pub searcher: Address,
    pub provider: FlashLoanProvider,
    pub fork_url: String,
    pub exec_key: Option<String>,
    pub anvil_bin: String,
    pub anvil: Option<AnvilNode>,
    pub anvil_executor: Option<Address>,
    pub anvil_block: Option<u64>,
    /// Attempts already reached a backend this session, keyed by [`submit_key`].
    pub cache: HashMap<String, ExecAttempt>,
}

pub fn execute_ledger(
    policy: &LedgerPolicy,
    opps: &[MevOpportunity],
    sim: SimMode,
    replayer: &BlockReplayer,
    pools: &PoolManager,
    ctx: &mut ExecContext,
) -> anyhow::Result<LedgerResult> {
    match ctx.mode {
        ExecMode::None => {
            anyhow::bail!("execute_ledger is only for revm or anvil")
        }
        ExecMode::Revm => Ok(run_revm(policy, opps, sim, replayer, pools, ctx)),
        ExecMode::Anvil => run_anvil(policy, opps, sim, pools, ctx),
    }
}

fn plan_for(
    opp: &MevOpportunity,
    pools: &PoolManager,
    ctx: &ExecContext,
) -> Result<super::ExecPlan, PlanError> {
    plan_from_opportunity(
        opp,
        pools,
        ctx.vault,
        ctx.provider,
        U256::from(1u64),
        opp.timestamp.saturating_add(12),
    )
}

fn pending_kind(opp: &MevOpportunity) -> bool {
    opp.mempool_only
        || matches!(
            opp.detection_path,
            Some(crate::types::DetectionPath::Pending)
        )
}

/// Fork target for an opportunity. Pending opportunities carry the not-yet-mined
/// block number, so their parent is the forkable tip.
fn fork_block_for(opp: &MevOpportunity) -> u64 {
    if pending_kind(opp) {
        opp.block_number.saturating_sub(1)
    } else {
        opp.block_number
    }
}

fn run_revm(
    policy: &LedgerPolicy,
    opps: &[MevOpportunity],
    sim: SimMode,
    replayer: &BlockReplayer,
    pools: &PoolManager,
    ctx: &mut ExecContext,
) -> LedgerResult {
    let mut prepared: Option<(u64, bool, RevmBlock)> = None;
    let mut setup_gas_wei: u128 = 0;
    let mut cache = std::mem::take(&mut ctx.cache);
    let searcher = ctx.searcher;
    let chain_id = ctx.chain_id;
    let weth = ctx.weth;
    let mode = ctx.mode.as_str();
    let ledger = policy.apply_executed(opps, sim, mode, |opp, wallet| {
        let key = submit_key(opp);
        if let Some(hit) = cache.get(&key).cloned() {
            return hit;
        }
        let pending = pending_kind(opp);
        let needs_open = prepared
            .as_ref()
            .map(|(b, p, _)| *b != opp.block_number || *p != pending)
            .unwrap_or(true);
        if needs_open {
            match open_revm_block(replayer, chain_id, opp, searcher, searcher, weth, wallet) {
                Ok(block) => {
                    setup_gas_wei = setup_gas_wei.saturating_add(block.setup_gas_wei);
                    prepared = Some((opp.block_number, pending, block));
                }
                Err(e) => {
                    tracing::warn!("exec revm block {} state: {e}", opp.block_number);
                    return ExecAttempt::failed(FillSkipReason::ExecBuildFailed);
                }
            }
        }
        let plan = match plan_for(opp, pools, ctx) {
            Ok(plan) => plan,
            Err(PlanError::StrategyUnsupported) => {
                return ExecAttempt::failed(FillSkipReason::ExecStrategyUnsupported);
            }
            Err(e) => {
                tracing::debug!("exec build failed: {e}");
                return ExecAttempt::failed(FillSkipReason::ExecBuildFailed);
            }
        };
        let Some((_, _, block)) = prepared.as_mut() else {
            return ExecAttempt::failed(FillSkipReason::ExecBuildFailed);
        };
        let attempt = match exec_plan_revm(replayer, chain_id, block, searcher, &plan) {
            Ok(attempt) => attempt,
            Err(e) => {
                tracing::warn!("exec revm tx: {e}");
                ExecAttempt::failed(FillSkipReason::ExecBuildFailed)
            }
        };
        remember_attempt(&mut cache, key, &attempt);
        attempt
    });
    ctx.cache = cache;
    let mut ledger = ledger;
    ledger.setup_gas_wei = setup_gas_wei;
    ledger
}

fn run_anvil(
    policy: &LedgerPolicy,
    opps: &[MevOpportunity],
    sim: SimMode,
    pools: &PoolManager,
    ctx: &mut ExecContext,
) -> anyhow::Result<LedgerResult> {
    let url = ctx
        .anvil
        .as_ref()
        .map(|n| n.url.clone())
        .ok_or_else(|| anyhow::anyhow!("--exec anvil has no node"))?;
    let fork = ctx.fork_url.clone();
    if fork.is_empty() {
        anyhow::bail!("--exec anvil needs an RPC url to fork");
    }
    let key = ctx
        .exec_key
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| ANVIL_DEFAULT_KEY.to_string());
    let searcher = ctx.searcher;
    let weth = ctx.weth;
    let mode = ctx.mode.as_str();
    let mut executor = ctx.anvil_executor;
    let mut prepared_block = ctx.anvil_block;
    let mut setup_gas_wei: u128 = 0;
    let mut cache = std::mem::take(&mut ctx.cache);
    let ledger = policy.apply_executed(opps, sim, mode, |opp, wallet| {
        let cache_key = submit_key(opp);
        if let Some(hit) = cache.get(&cache_key).cloned() {
            return hit;
        }
        let plan = match plan_for(opp, pools, ctx) {
            Ok(plan) => plan,
            Err(PlanError::StrategyUnsupported) => {
                return ExecAttempt::failed(FillSkipReason::ExecStrategyUnsupported);
            }
            Err(e) => {
                tracing::debug!("exec build failed: {e}");
                return ExecAttempt::failed(FillSkipReason::ExecBuildFailed);
            }
        };
        let fork_block = fork_block_for(opp);
        if prepared_block != Some(fork_block) || executor.is_none() {
            let url = url.clone();
            let fork = fork.clone();
            let key = key.clone();
            let block = fork_block;
            let deployed = wait(async move {
                reset_fork(&url, &fork, block).await?;
                let mut sender = AnvilSender::connect(&url, &key).await?;
                sender.fund(wallet).await?;
                let (addr, setup) = sender.deploy_executor(&[searcher, searcher, weth]).await?;
                Ok::<_, anyhow::Error>((addr, setup, sender))
            });
            match deployed {
                Ok((addr, setup, _sender)) => {
                    setup_gas_wei = setup_gas_wei.saturating_add(setup);
                    executor = Some(addr);
                    prepared_block = Some(block);
                }
                Err(e) => {
                    tracing::warn!("anvil fork reset/deploy: {e}");
                    return ExecAttempt::failed(FillSkipReason::ExecBuildFailed);
                }
            }
        }
        let Some(to) = executor else {
            return ExecAttempt::failed(FillSkipReason::ExecBuildFailed);
        };
        let data = encode_execute(&plan);
        let url = url.clone();
        let key = key.clone();
        let attempt = match wait(async move {
            // Reconnect after deploy path so nonce tracks signed sends.
            let mut sender = AnvilSender::connect(&url, &key).await?;
            sender.send_exec(to, data).await
        }) {
            Ok(attempt) => attempt,
            Err(e) => {
                tracing::warn!("anvil exec: {e}");
                ExecAttempt::failed(FillSkipReason::ExecBuildFailed)
            }
        };
        remember_attempt(&mut cache, cache_key, &attempt);
        attempt
    });
    ctx.anvil_executor = executor;
    ctx.anvil_block = prepared_block;
    ctx.cache = cache;
    let mut ledger = ledger;
    ledger.setup_gas_wei = setup_gas_wei;
    Ok(ledger)
}

fn submit_key(opp: &MevOpportunity) -> String {
    if let Some(id) = opp.canonical_id.as_deref().filter(|s| !s.is_empty()) {
        return id.to_string();
    }
    format!(
        "{}:{}:{}:{:#x}:{:#x}",
        opp.block_number, opp.tx_index, opp.strategy, opp.pool_a, opp.pool_b
    )
}

/// Keep a result only after the attempt reached a backend.
fn remember_attempt(cache: &mut HashMap<String, ExecAttempt>, key: String, attempt: &ExecAttempt) {
    let reached = attempt.tx_hash.is_some()
        || attempt.status
        || attempt.fail == Some(FillSkipReason::ExecRevert);
    if reached {
        cache.insert(key, attempt.clone());
    }
}

fn wait<T>(fut: impl std::future::Future<Output = T>) -> T {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Strategy;

    #[test]
    fn attempt_is_cached_and_build_failure_is_not() {
        let mut cache = HashMap::new();
        let built = ExecAttempt::failed(FillSkipReason::ExecBuildFailed);
        remember_attempt(&mut cache, "k".into(), &built);
        assert!(cache.is_empty());

        let sent = ExecAttempt::ran(true, 10, 21_000, 1, Some("0xabc".into()));
        remember_attempt(&mut cache, "k".into(), &sent);
        assert_eq!(cache["k"].tx_hash.as_deref(), Some("0xabc"));

        let timed_out = ExecAttempt::failed(FillSkipReason::ExecRevert);
        remember_attempt(&mut cache, "t".into(), &timed_out);
        assert!(cache.contains_key("t"));
        assert_eq!(cache["k"].net_wei, 10);
    }

    #[test]
    fn submit_key_prefers_canonical_id() {
        let mut opp = MevOpportunity::new(1, 2, Strategy::TwoHopArb, Address::ZERO, 0);
        opp.canonical_id = Some("cid".into());
        assert_eq!(submit_key(&opp), "cid");
        opp.canonical_id = None;
        assert!(submit_key(&opp).starts_with("1:2:"));
    }

    #[test]
    fn pending_opps_fork_on_the_parent_block() {
        let mut opp = MevOpportunity::new(101, 0, Strategy::TwoHopArb, Address::ZERO, 0);
        assert_eq!(fork_block_for(&opp), 101);

        opp.mempool_only = true;
        assert_eq!(fork_block_for(&opp), 100);

        opp.mempool_only = false;
        opp.detection_path = Some(crate::types::DetectionPath::Pending);
        assert_eq!(fork_block_for(&opp), 100);

        // A node that reports the pending number as 0 must not underflow.
        let mut genesis = MevOpportunity::new(0, 0, Strategy::TwoHopArb, Address::ZERO, 0);
        genesis.mempool_only = true;
        assert_eq!(fork_block_for(&genesis), 0);
    }
}
