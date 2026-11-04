//! Run the paper ledger against revm, a local Anvil fork, or a testnet.
use std::collections::HashMap;

use alloy::primitives::Address;

use crate::paper::{ExecAttempt, FillSkipReason, LedgerPolicy, LedgerResult, SimMode};
use crate::pool::state::PoolManager;
use crate::replay::BlockReplayer;
use crate::types::{FlashLoanProvider, MevOpportunity};

use super::anvil::{deploy_executor, impersonate_and_fund, reset_fork, send_exec, AnvilNode};
use super::calldata::encode_execute;
use super::plan::{plan_from_opportunity, PlanError};
use super::revm_backend::{exec_plan_revm, open_revm_block, RevmBlock};
use super::testnet::TestnetSender;
use super::ExecMode;

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
    pub testnet: Option<TestnetSender>,
    pub testnet_executor: Option<Address>,
    /// Broadcasts already sent on testnet, keyed by [`submit_key`]. A later
    /// ledger pass replays these results instead of signing again.
    pub submitted: HashMap<String, ExecAttempt>,
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
            anyhow::bail!("execute_ledger is only for revm, anvil, or testnet")
        }
        ExecMode::Revm => Ok(run_revm(policy, opps, sim, replayer, pools, ctx)),
        ExecMode::Anvil => run_anvil(policy, opps, sim, pools, ctx),
        ExecMode::Testnet => run_testnet(policy, opps, sim, pools, ctx),
    }
}

fn run_revm(
    policy: &LedgerPolicy,
    opps: &[MevOpportunity],
    sim: SimMode,
    replayer: &BlockReplayer,
    pools: &PoolManager,
    ctx: &ExecContext,
) -> LedgerResult {
    let mut prepared: Option<RevmBlock> = None;
    policy.apply_executed(opps, sim, ctx.mode.as_str(), |opp| {
        if prepared.as_ref().map(|p| p.block_num) != Some(opp.block_number) {
            match open_revm_block(replayer, opp.block_number, ctx.searcher) {
                Ok(block) => prepared = Some(block),
                Err(e) => {
                    tracing::warn!("exec revm block {} state: {e}", opp.block_number);
                    return ExecAttempt::failed(FillSkipReason::ExecBuildFailed);
                }
            }
        }
        let plan = match plan_from_opportunity(
            opp,
            pools,
            ctx.vault,
            ctx.searcher,
            ctx.weth,
            ctx.provider,
        ) {
            Ok(plan) => plan,
            Err(PlanError::StrategyUnsupported) => {
                return ExecAttempt::failed(FillSkipReason::ExecStrategyUnsupported);
            }
            Err(e) => {
                tracing::debug!("exec build failed: {e}");
                return ExecAttempt::failed(FillSkipReason::ExecBuildFailed);
            }
        };
        let Some(block) = prepared.as_mut() else {
            return ExecAttempt::failed(FillSkipReason::ExecBuildFailed);
        };
        match exec_plan_revm(replayer, ctx.chain_id, block, ctx.searcher, &plan) {
            Ok(attempt) => attempt,
            Err(e) => {
                tracing::warn!("exec revm tx: {e}");
                ExecAttempt::failed(FillSkipReason::ExecBuildFailed)
            }
        }
    })
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
    let searcher = ctx.searcher;
    let vault = ctx.vault;
    let weth = ctx.weth;
    let provider = ctx.provider;
    let mode = ctx.mode.as_str();
    let mut executor = ctx.anvil_executor;
    let mut prepared_block = ctx.anvil_block;
    let ledger = policy.apply_executed(opps, sim, mode, |opp| {
        let plan = match plan_from_opportunity(opp, pools, vault, searcher, weth, provider) {
            Ok(plan) => plan,
            Err(PlanError::StrategyUnsupported) => {
                return ExecAttempt::failed(FillSkipReason::ExecStrategyUnsupported);
            }
            Err(e) => {
                tracing::debug!("exec build failed: {e}");
                return ExecAttempt::failed(FillSkipReason::ExecBuildFailed);
            }
        };
        if prepared_block != Some(opp.block_number) || executor.is_none() {
            let url = url.clone();
            let fork = fork.clone();
            let block = opp.block_number;
            let deployed = wait(async move {
                reset_fork(&url, &fork, block).await?;
                impersonate_and_fund(&url, searcher).await?;
                deploy_executor(&url, searcher).await
            });
            match deployed {
                Ok(addr) => {
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
        match wait(async move { send_exec(&url, searcher, to, data).await }) {
            Ok(attempt) => attempt,
            Err(e) => {
                tracing::warn!("anvil exec: {e}");
                ExecAttempt::failed(FillSkipReason::ExecBuildFailed)
            }
        }
    });
    ctx.anvil_executor = executor;
    ctx.anvil_block = prepared_block;
    Ok(ledger)
}

fn run_testnet(
    policy: &LedgerPolicy,
    opps: &[MevOpportunity],
    sim: SimMode,
    pools: &PoolManager,
    ctx: &mut ExecContext,
) -> anyhow::Result<LedgerResult> {
    let executor = ctx.testnet_executor.ok_or_else(|| {
        anyhow::anyhow!(
            "--exec testnet needs [paper].executor_address or a successful deploy this session"
        )
    })?;
    if ctx.testnet.is_none() {
        anyhow::bail!("--exec testnet has no signer");
    }
    let searcher = ctx.searcher;
    let vault = ctx.vault;
    let weth = ctx.weth;
    let provider = ctx.provider;
    let mode = ctx.mode.as_str();
    // Split the borrow: sender is taken out for the callback, then put back.
    let mut sender = ctx
        .testnet
        .take()
        .ok_or_else(|| anyhow::anyhow!("--exec testnet has no signer"))?;
    // Testnet cannot replay history. A later ledger pass (live recomputes the
    // whole session) must reuse the receipt instead of broadcasting again.
    let mut submitted = std::mem::take(&mut ctx.submitted);
    let ledger = policy.apply_executed(opps, sim, mode, |opp| {
        let key = submit_key(opp);
        if let Some(hit) = submitted.get(&key).cloned() {
            return hit;
        }
        let plan = match plan_from_opportunity(opp, pools, vault, searcher, weth, provider) {
            Ok(plan) => plan,
            Err(PlanError::StrategyUnsupported) => {
                return ExecAttempt::failed(FillSkipReason::ExecStrategyUnsupported);
            }
            Err(e) => {
                tracing::debug!("exec build failed: {e}");
                return ExecAttempt::failed(FillSkipReason::ExecBuildFailed);
            }
        };
        let data = encode_execute(&plan);
        let attempt = match wait(async { sender.send_exec(executor, data).await }) {
            Ok(attempt) => attempt,
            Err(e) => {
                tracing::warn!("testnet exec: {e}");
                ExecAttempt::failed(FillSkipReason::ExecBuildFailed)
            }
        };
        remember_broadcast(&mut submitted, key, &attempt);
        attempt
    });
    ctx.testnet = Some(sender);
    ctx.submitted = submitted;
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

/// Keep a result only after a transaction was handed to the node. A pre-send
/// build failure stays uncached so the next pass can retry it.
fn remember_broadcast(
    cache: &mut HashMap<String, ExecAttempt>,
    key: String,
    attempt: &ExecAttempt,
) {
    let broadcast = attempt.tx_hash.is_some() || attempt.fail == Some(FillSkipReason::ExecRevert);
    if broadcast {
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
    fn broadcast_is_cached_and_build_failure_is_not() {
        let mut cache = HashMap::new();
        let built = ExecAttempt::failed(FillSkipReason::ExecBuildFailed);
        remember_broadcast(&mut cache, "k".into(), &built);
        assert!(cache.is_empty());

        let sent = ExecAttempt::ran(true, 10, 21_000, 1, Some("0xabc".into()));
        remember_broadcast(&mut cache, "k".into(), &sent);
        assert_eq!(cache["k"].tx_hash.as_deref(), Some("0xabc"));

        let timed_out = ExecAttempt::failed(FillSkipReason::ExecRevert);
        remember_broadcast(&mut cache, "t".into(), &timed_out);
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
}
